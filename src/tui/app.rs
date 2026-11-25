//! Minimal interactive orchestration with native scrollback and cancellable turns.

use super::{
    capabilities::{Capabilities, TerminalFeedback},
    commands,
    header::Header,
    input::{Action, Editor},
    intro::{self, IntroEnd},
    render::{Frame, Renderer},
    statusbar::Status,
    term::TerminalGuard,
    widgets,
};
use crate::{
    agent::{Agent, InputQueue},
    provider::canonical::{Content, Event as Progress, Message, Role, Usage},
    theme::{self, ColorMode, Theme},
};
use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use std::{
    io,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

fn save_selected_model(provider: &crate::config::Provider, model: &str, status: &mut Status) {
    let result = crate::platform::home_dir()
        .ok_or_else(|| io::Error::other("домашний каталог недоступен"))
        .and_then(|home| crate::model_preferences::set(&home, provider, model));
    if let Err(error) = result {
        status.toast = Some(format!("Модель не сохранена: {error}"));
    }
}

fn apply_model_effort(agent: &mut Agent, status: &mut Status) {
    let _ = agent.client.set_effort(Some("xhigh"));
    if let Some(home) = crate::platform::home_dir() {
        if let Err(error) = crate::effort_preferences::apply(&mut agent.client, &home, &agent.model)
        {
            status.toast = Some(format!("Не удалось загрузить effort: {error}"));
        }
    }
    status.effort = agent.client.effort().map(str::to_owned);
}

/// Store a freshly entered token in the private credential directory.
///
/// The profile keeps only `api_key_env`; the secret never reaches `config.toml`.
fn store_token(home: &std::path::Path, key_env: &str, token: &str) -> Result<(), String> {
    let path = home.join(".gitcode").join("credentials").join(key_env);
    crate::credentials::create(&path, &crate::credentials::Secret::new(token.to_owned()))
        .map_err(|_| "не удалось сохранить токен".to_owned())
}

/// Swap the live client for a saved profile, keeping the reasoning effort.
fn activate_provider(
    agent: &mut Agent,
    profile: &crate::config::Provider,
    secret: Option<crate::credentials::Secret>,
    status: &mut Status,
) -> Result<(), String> {
    let bearer = profile.protocol != crate::config::Protocol::Anthropic;
    let mut client = crate::provider::transport::Client::with_protocol(
        &profile.base_url,
        secret,
        bearer,
        Duration::from_secs(profile.timeout_seconds),
        profile.protocol,
    )?;
    client.set_prompt_cache(profile.prompt_cache);
    let effort = agent.client.effort().map(str::to_owned);
    let _ = client.set_effort(effort.as_deref());
    agent.client = client;
    status.provider = profile.name.clone();
    Ok(())
}

fn save_effort(agent: &Agent, status: &mut Status) {
    let result = crate::platform::home_dir()
        .ok_or_else(|| io::Error::other("домашний каталог недоступен"))
        .and_then(|home| {
            crate::effort_preferences::set(
                &home,
                &agent.model,
                agent.client.effort().unwrap_or("default"),
            )
        });
    if let Err(error) = result {
        status.toast = Some(format!("Не удалось сохранить effort: {error}"));
    }
}

// Keep deadlines independent of event arrival; dropped select futures must not
// restart the frame delay. Skip missed frames rather than burst-rendering them.
fn frame_clock() -> tokio::time::Interval {
    let mut clock = tokio::time::interval(Duration::from_nanos(1_000_000_000 / 60));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    clock
}

/// Whether a submitted prompt names a slash command rather than chat text.
///
/// Custom commands live on disk, so their names are checked separately; builtins
/// are resolved in memory and never touch the filesystem on a keystroke.
fn is_slash_command(prompt: &str, custom: &[crate::extensibility::CustomCommand]) -> bool {
    let Some(rest) = prompt.trim().strip_prefix('/') else {
        return false;
    };
    let name = rest
        .split_once(char::is_whitespace)
        .map_or(rest, |(name, _)| name);
    !name.is_empty()
        && (name == "new"
            || crate::extensibility::BUILTIN_COMMANDS
                .iter()
                .any(|command| command.name == name)
            || custom.iter().any(|command| command.name == name))
}

/// Derive a short session title from the first prompt, the way Codex names chats.
///
/// `/resume` is only usable when entries say what they are about, so the title
/// is written once, when the conversation still has no user messages.
fn session_title(prompt: &str) -> String {
    let single_line = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed: String = single_line.chars().take(60).collect();
    let trimmed = trimmed.trim_end_matches(['.', ',', ';', ':', '!', '?', ' ']);
    if trimmed.is_empty() {
        "Новый чат".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[derive(Default)]
struct LiveUsage {
    completed: Usage,
    current: Usage,
    characters: u64,
}

impl LiveUsage {
    fn snapshot(&mut self, usage: Usage) {
        self.current = usage;
    }

    // Done is the explicit validated request boundary, not the end of the turn.
    fn finish_request(&mut self) {
        self.completed = self.total();
        self.current = Usage::default();
        self.characters = 0;
    }

    fn add_text(&mut self, text: &str) {
        self.characters = self.characters.saturating_add(text.chars().count() as u64);
    }

    fn total(&self) -> Usage {
        Usage {
            input_tokens: self
                .completed
                .input_tokens
                .saturating_add(self.current.input_tokens),
            output_tokens: self
                .completed
                .output_tokens
                .saturating_add(self.current.output_tokens),
            cache_read_tokens: self
                .completed
                .cache_read_tokens
                .saturating_add(self.current.cache_read_tokens),
            cache_write_tokens: self
                .completed
                .cache_write_tokens
                .saturating_add(self.current.cache_write_tokens),
            reasoning_tokens: self
                .completed
                .reasoning_tokens
                .saturating_add(self.current.reasoning_tokens),
            estimated: self.completed.estimated || self.current.estimated,
        }
    }

    fn apply(&self, status: &mut Status, base: &Status, elapsed: Duration) {
        let total = self.total();
        let estimate = self.characters.div_ceil(4);
        let current = self.current.output_tokens.max(estimate);
        let turn_output = self.completed.output_tokens.saturating_add(current);
        status.input = base.input.saturating_add(total.input_tokens);
        status.turn_output = turn_output;
        status.output = base.output.saturating_add(turn_output);
        status.output_estimated = base.output_estimated
            || self.completed.estimated
            || self.current.estimated
            || estimate > self.current.output_tokens;
        status.cache_read = base.cache_read.saturating_add(total.cache_read_tokens);
        status.cache_write = base.cache_write.saturating_add(total.cache_write_tokens);
        status.tokens_per_second = Some(turn_output as f64 / elapsed.as_secs_f64().max(0.001));
    }
}

/// Estimate the current request plus streamed content until the next projection.
#[derive(Default)]
struct LiveContext {
    base_tokens: Option<u64>,
    window_tokens: u64,
    added_bytes: u64,
}

impl LiveContext {
    fn reset(&mut self, used_tokens: u64, window_tokens: u64, status: &mut Status) {
        self.base_tokens = Some(used_tokens);
        self.window_tokens = window_tokens.max(1);
        self.added_bytes = 0;
        self.apply(status);
    }

    fn add_bytes(&mut self, bytes: usize, status: &mut Status) {
        self.added_bytes = self.added_bytes.saturating_add(bytes as u64);
        self.apply(status);
    }

    fn apply(&self, status: &mut Status) {
        if let Some(base) = self.base_tokens {
            let used = base.saturating_add(self.added_bytes.div_ceil(3));
            status.context_percent =
                Some((used as f64 / self.window_tokens.max(1) as f64).clamp(0.0, 1.0) * 100.0);
        }
    }
}

fn completion_line(
    unix_seconds: u64,
    elapsed: Duration,
    usage: &Usage,
    width: usize,
    mode: ColorMode,
) -> String {
    let seconds = unix_seconds % 86_400;
    let time = format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    );
    let text = format!(
        "  {time} UTC • {} • {}{}↓",
        super::render::elapsed_duration(elapsed.as_secs()),
        if usage.estimated { "~" } else { "" },
        super::statusbar::compact(usage.output_tokens)
    );
    let clipped = super::clip(&text, width);
    let color = theme::foreground(Theme::default().placeholder, mode);
    let bullet = format!("{}•{}{}", theme::bold(mode), theme::reset(mode), color);
    format!(
        "{}{}{}",
        color,
        clipped.replace('•', &bullet),
        theme::reset(mode)
    )
}

#[cfg(test)]
#[path = "../../tests/tui_progress/regression.rs"]
#[allow(clippy::unwrap_used)]
mod regression;

/// Cached startup metadata; construction must not block on git or network work.
pub struct Options<'a> {
    /// Model display name.
    pub model: &'a str,
    /// Provider display name.
    pub provider: &'a str,
    /// Provider configuration used to persist the selected model.
    pub model_provider: &'a crate::config::Provider,
    /// Working directory display path.
    pub cwd: &'a str,
    /// Cached branch name, or an unknown marker.
    pub branch: &'a str,
    /// Current effective permission mode label.
    pub permission: &'a str,
    /// Disable the skippable startup animation.
    pub no_intro: bool,
    /// Conversation rows moved per mouse-wheel notch.
    pub mouse_scroll_speed: u32,
    /// Animate mouse wheel scrolling.
    pub scroll_animation: bool,
    /// Persistent project session and recovered conversation.
    pub session: Option<(
        crate::session::store::SessionStore,
        crate::session::store::Session,
    )>,
}

fn update_copy_notice(
    renderer: &mut Renderer<io::Stdout>,
    status: &mut Status,
    deadline: &mut Option<Instant>,
) -> io::Result<bool> {
    if let Some(characters) = renderer.copy_selection_if_due()? {
        status.copied_chars = Some(characters);
        *deadline = Some(Instant::now() + Duration::from_secs(2));
        return Ok(true);
    }
    if deadline.is_some_and(|until| Instant::now() >= until) {
        status.copied_chars = None;
        *deadline = None;
        return Ok(true);
    }
    Ok(false)
}

/// Offer an available release before the first frame is drawn.
///
/// The cached lookup is read once, so an unreachable network never delays
/// startup; a stale cache only triggers a background refresh for the next run.
async fn update_prompt(
    renderer: &mut Renderer<io::Stdout>,
    events: &mut EventStream,
    mode: ColorMode,
) -> io::Result<Option<(super::update_picker::Choice, String)>> {
    let Some(home) = crate::platform::home_dir() else {
        return Ok(None);
    };
    let installed = env!("CARGO_PKG_VERSION");
    let Some(latest) = crate::update_check::banner(&home, installed) else {
        return Ok(None);
    };
    let mut choice = super::update_picker::Choice::UpdateNow;
    loop {
        let (width, height) = renderer.size();
        renderer
            .paint_styled(&super::update_picker::frame(
                choice,
                installed,
                &latest,
                width.saturating_sub(1),
                super::picker::height(height),
                mode,
            ))
            .map_err(io::Error::other)?;
        match events.next().await {
            Some(Ok(Event::Key(key))) => {
                let event = Event::Key(key);
                if let Some(decision) = super::update_picker::answer(&event, &mut choice) {
                    return Ok(Some((decision, latest)));
                }
            }
            Some(Ok(Event::Resize(width, height))) => renderer.resize(width.into(), height.into()),
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error),
            None => return Ok(None),
        }
    }
}

async fn save_compaction_checkpoint(
    agent: &Agent,
    persistence: &Option<(
        crate::session::store::SessionStore,
        crate::session::store::Session,
    )>,
    messages: &[Message],
) -> io::Result<()> {
    let Some((store, session)) = persistence else {
        return Ok(());
    };
    let Some(data) = agent
        .compaction
        .snapshot(messages)
        .map_err(io::Error::other)?
    else {
        return Ok(());
    };
    let store = store.clone();
    let id = session.id.clone();
    tokio::task::spawn_blocking(move || store.save_compaction(&id, &data))
        .await
        .map_err(io::Error::other)?
}

fn remove_queued_prompt(status: &mut Status, prompt: &str) {
    if let Some(index) = status
        .queued_prompts
        .iter()
        .position(|entry| entry == prompt)
    {
        status.queued_prompts.remove(index);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint(
    renderer: &mut Renderer<io::Stdout>,
    editor: &Editor,
    status: &Status,
    stream: &mut super::markdown::Markdown,
    stream_started: &mut bool,
    tool_running: bool,
    approval: Option<&super::approval_picker::Pending>,
    mode: ColorMode,
) -> io::Result<()> {
    let (width, height) = renderer.size();
    renderer.set_activity_elapsed(status.elapsed_millis);
    let mut frame = Frame::input(editor, status, width.saturating_sub(1), height);
    let theme = Theme::default();
    let mut queued_rows = Vec::new();
    for prompt in &status.queued_prompts {
        for row in format_user_turn(prompt, width, &theme, ColorMode::None) {
            queued_rows.push(if mode == ColorMode::None || row.is_empty() {
                row
            } else {
                format!(
                    "{}{}{row}{}",
                    theme::background(theme.panel, mode),
                    theme::foreground(theme.placeholder, mode),
                    theme::reset(mode),
                )
            });
        }
    }
    // Keep the composer visible even for very long queued prompts.
    let queue_room = height.saturating_sub(frame.lines.len() + 2);
    if queued_rows.len() > queue_room {
        queued_rows.drain(..queued_rows.len() - queue_room);
    }
    let room = height
        .saturating_sub(frame.lines.len() + queued_rows.len())
        .saturating_sub(2)
        .max(1);
    let text_width = width.saturating_sub(3).max(1);
    let finalized = stream.drain_stable(text_width, &Theme::default(), mode, room);
    let finalized = if !finalized.is_empty() {
        let rows = decorate_assistant_rows(finalized, *stream_started, &Theme::default(), mode);
        if !rows.is_empty() {
            *stream_started = true;
        }
        rows
    } else {
        Vec::new()
    };
    if let Some(approval) = approval {
        if !finalized.is_empty() {
            renderer.finalize_styled(&finalized)?;
        }
        return renderer.paint_styled(&approval.frame(
            width.saturating_sub(1),
            height.saturating_sub(1).max(1),
            mode,
        ));
    }
    if status.bypass_confirmation {
        if !finalized.is_empty() {
            renderer.finalize_styled(&finalized)?;
        }
        return renderer.paint_styled(&super::bypass_picker::frame(
            status.bypass_selected_yes,
            width.saturating_sub(1),
            super::picker::height(height),
            mode,
        ));
    }
    let mut rendered = assistant_rows(
        stream,
        width.saturating_sub(1),
        &Theme::default(),
        mode,
        *stream_started,
    );
    if rendered.len() > room {
        rendered.drain(..rendered.len() - room);
    }
    let count = rendered.len();
    let gap = usize::from(status.busy && (count > 0 || *stream_started || tool_running));
    let mut prefix = rendered;
    if gap > 0 {
        prefix.push(String::new());
    }
    prefix.extend(queued_rows);
    let styled_rows = prefix.len();
    frame.lines.splice(0..0, prefix);
    if let Some((row, _)) = &mut frame.cursor {
        *row += styled_rows;
    }
    renderer.paint_stream_flow(&finalized, &frame, styled_rows)
}

fn draw_header(
    renderer: &mut Renderer<io::Stdout>,
    header: &Header<'_>,
    status: &Status,
    mode: ColorMode,
) -> io::Result<()> {
    let (width, height) = renderer.size();
    let width = width.saturating_sub(1);
    let rows = header.styled_lines_for_size(width, status, height, &Theme::default(), mode);
    renderer.finalize_styled(&rows)
}

fn action(event: Event, editor: &mut Editor, renderer: &mut Renderer<io::Stdout>) -> Action {
    if let Event::Mouse(mouse) = event {
        renderer.mouse(mouse);
        return Action::Refresh;
    }
    if let Event::Resize(w, h) = event {
        renderer.resize(w.into(), h.into());
    }
    if let Event::FocusGained = event {
        let (w, h) = renderer.size();
        renderer.resize(w, h);
        return Action::Refresh;
    }
    if matches!(event, Event::Key(key) if key.kind == crossterm::event::KeyEventKind::Press) {
        renderer.clear_selection();
    }
    let result = editor.handle(event);
    if let Action::Submit(text) = &result {
        if let Some(home) = crate::platform::home_dir() {
            let _ = crate::session::history::append(&home, text);
        }
    }
    result
}

fn format_user_turn(prompt: &str, width: usize, theme: &Theme, mode: ColorMode) -> Vec<String> {
    let width = width.saturating_sub(1).max(1);
    let mut rows = Vec::new();
    for (idx, line) in super::markdown::wrap_words(prompt, width.saturating_sub(4).max(1))
        .iter()
        .enumerate()
    {
        let prefix = if idx == 0 { "  › " } else { "    " };
        let clipped = widgets::fit(&format!("{prefix}{line}"), width);
        if mode == ColorMode::None {
            rows.push(clipped);
        } else {
            rows.push(format!(
                "{}{}{clipped}",
                theme::background(theme.panel, mode),
                theme::foreground(theme.rose, mode),
            ));
        }
    }
    rows.push(String::new());
    rows
}

#[cfg(test)]
#[test]
fn user_turn_wraps_long_and_multiline_messages() {
    let rows = format_user_turn(
        "abcdefghijklmnop\nвторая строка",
        12,
        &Theme::default(),
        ColorMode::None,
    );
    assert_eq!(
        rows.iter().map(|row| row.trim_end()).collect::<Vec<_>>(),
        [
            "  › abcdefg",
            "    hijklmn",
            "    op",
            "    вторая ",
            "    строка",
            ""
        ]
        .iter()
        .map(|row| row.trim_end())
        .collect::<Vec<_>>()
    );
}

#[cfg(test)]
#[test]
fn assistant_uses_terminal_width_beyond_one_hundred_columns() {
    let mut stream = super::markdown::Markdown::new();
    stream.push(&"я".repeat(120));
    let rows = format_assistant_turn(&mut stream, false, 150, &Theme::default(), ColorMode::None);
    assert_eq!(rows[0].chars().filter(|c| *c == 'я').count(), 120);
    assert_eq!(rows.len(), 2);
}

fn format_assistant_turn(
    stream: &mut super::markdown::Markdown,
    started: bool,
    width: usize,
    theme: &Theme,
    mode: ColorMode,
) -> Vec<String> {
    stream.finish();
    let mut rows = assistant_rows(stream, width.saturating_sub(1), theme, mode, started);
    // A message that carried only blank lines is a separator the model already
    // emits around tool calls; committing it again stretches the transcript.
    if rows.iter().all(|row| row.trim().is_empty()) {
        return Vec::new();
    }
    rows.push(String::new());
    rows
}

fn assistant_rows(
    markdown: &super::markdown::Markdown,
    width: usize,
    theme: &Theme,
    mode: ColorMode,
    started: bool,
) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let rows = markdown.render(width.saturating_sub(2).max(1), theme, mode);
    decorate_assistant_rows(rows, started, theme, mode)
}

fn decorate_assistant_rows(
    rows: Vec<String>,
    started: bool,
    theme: &Theme,
    mode: ColorMode,
) -> Vec<String> {
    let glyphs = widgets::Glyphs::detect();
    let mark = if glyphs.ascii { "*" } else { "✿" };
    rows.into_iter()
        .skip_while(|row| !started && widgets::unstyled(row).trim().is_empty())
        .enumerate()
        .map(|(index, row)| {
            if index == 0 && !started {
                if mode == ColorMode::None {
                    format!("{mark} {row}")
                } else {
                    format!(
                        "{}{mark}{} {row}",
                        theme::foreground(theme.rose, mode),
                        theme::reset(mode),
                    )
                }
            } else if row.is_empty() {
                row
            } else {
                format!("  {row}")
            }
        })
        .collect()
}

fn start_turn_status(status: &mut Status) {
    status.busy = true;
    status.compacting = false;
    status.subagents.clear();
    status.elapsed_secs = 0;
    status.elapsed_millis = 0;
    status.turn_output = 0;
    status.tokens_per_second = None;
    status.retry_notice = None;
}

fn finish_turn_status(status: &mut Status, succeeded: bool) {
    status.busy = false;
    status.compacting = false;
    status.retry_notice = None;
    if succeeded {
        status.subagents.clear();
    }
}

fn interruption_rows(theme: &Theme, mode: ColorMode) -> Vec<String> {
    let marker = if widgets::Glyphs::detect().ascii {
        "*"
    } else {
        "●"
    };
    let rows = vec![
        format!("{marker} Прервано. Уже выполненные операции не отменены."),
        String::new(),
    ];
    style_tool_rows(&rows, true, false, theme, mode)
}

fn style_tool_rows(
    rows: &[String],
    started: bool,
    error: bool,
    theme: &Theme,
    mode: ColorMode,
) -> Vec<String> {
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            if mode == ColorMode::None || row.is_empty() {
                return row.clone();
            }
            if started && index == 0 {
                let (marker, text) = row.split_once(' ').unwrap_or((row, ""));
                format!(
                    "{}{marker}{} {text}",
                    theme::foreground(theme.rose, mode),
                    theme::default_foreground(mode)
                )
            } else if started {
                format!(
                    "{}{row}{}",
                    theme::default_foreground(mode),
                    theme::reset(mode)
                )
            } else {
                let color = if error {
                    theme::foreground(theme.error, mode)
                } else {
                    theme::foreground(theme.placeholder, mode)
                };
                format!("{color}{row}{}", theme::reset(mode))
            }
        })
        .collect()
}

fn completed_tool_rows(
    name: &str,
    output: &str,
    is_error: bool,
    duration_ms: u64,
    width: usize,
    theme: &Theme,
    mode: ColorMode,
) -> Vec<String> {
    let mut rows = if name == "Agent" && !is_error {
        super::tool_card::completed_agent(output, width.saturating_sub(1).max(1), theme, mode)
    } else {
        let rows = super::tool_card::completed(
            name,
            output,
            is_error,
            duration_ms,
            width.saturating_sub(1).max(20),
        );
        style_tool_rows(
            &rows,
            false,
            is_error || output.contains("[exit code:") || output.contains("[command timed out]"),
            theme,
            mode,
        )
    };
    rows.push(String::new());
    rows
}

fn replay_rows(messages: &[Message], width: usize, theme: &Theme, mode: ColorMode) -> Vec<String> {
    let mut calls = std::collections::HashMap::new();
    let mut rows = Vec::new();
    for message in messages {
        if message.role == Role::System {
            continue;
        }
        for content in &message.content {
            match content {
                Content::Text { text } => match message.role {
                    Role::User => rows.extend(format_user_turn(text, width, theme, mode)),
                    Role::Assistant => {
                        let mut markdown = super::markdown::Markdown::new();
                        markdown.push(text);
                        rows.extend(format_assistant_turn(
                            &mut markdown,
                            false,
                            width,
                            theme,
                            mode,
                        ));
                    }
                    Role::System => {}
                },
                Content::ToolUse { id, name, input } => {
                    let display_input;
                    let (name, input) = if matches!(
                        name.as_str(),
                        "exec_command"
                            | "shell_command"
                            | "functions.exec_command"
                            | "functions.shell_command"
                            | "shell"
                            | "functions.shell"
                            | "local_shell_call"
                            | "local_shell"
                    ) {
                        let command = input
                            .get("cmd")
                            .or_else(|| input.get("command"))
                            .or_else(|| input.pointer("/action/command"));
                        let command = match command {
                            Some(serde_json::Value::String(command)) => command.clone(),
                            Some(serde_json::Value::Array(args)) => args
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .collect::<Vec<_>>()
                                .join(" "),
                            _ => String::new(),
                        };
                        display_input = serde_json::json!({"command": command});
                        ("Bash", &display_input)
                    } else {
                        (name.as_str(), input)
                    };
                    calls.insert(id.as_str(), (name, input.clone()));
                }
                Content::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let (name, input) = calls
                        .remove(tool_use_id.as_str())
                        .unwrap_or(("Tool", serde_json::Value::Null));
                    let started =
                        super::tool_card::started(name, &input, width.saturating_sub(1).max(20));
                    rows.extend(style_tool_rows(&started, true, false, theme, mode));
                    rows.extend(completed_tool_rows(
                        name, content, *is_error, 0, width, theme, mode,
                    ));
                }
                // Live thinking is accounting-only; opaque provider state is never transcript text.
                Content::Thinking { .. } | Content::Image { .. } | Content::Document { .. } => {}
            }
        }
    }
    for (_, (name, input)) in calls {
        let started = super::tool_card::started(name, &input, width.saturating_sub(1).max(20));
        rows.extend(style_tool_rows(&started, true, false, theme, mode));
    }
    rows
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod replay_tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str, name: &str, input: serde_json::Value) -> Content {
        Content::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
        }
    }

    fn result(id: &str, output: &str, is_error: bool) -> Content {
        Content::ToolResult {
            tool_use_id: id.into(),
            content: output.into(),
            is_error,
        }
    }

    #[test]
    fn replay_matches_live_tool_cards_without_protocol_metadata() {
        let theme = Theme::default();
        for mode in [ColorMode::None, ColorMode::TrueColor] {
            let input = json!({"command": "printf hello", "private_argument": "not displayed"});
            let messages = vec![
                Message {
                    role: Role::Assistant,
                    content: vec![
                        Content::Thinking {
                            thinking: "hidden reasoning".into(),
                            signature: Some("opaque signature".into()),
                            encrypted_content: Some("opaque state".into()),
                        },
                        call("private-call-id", "Bash", input.clone()),
                    ],
                },
                Message {
                    role: Role::User,
                    content: vec![result("private-call-id", "hello\n[exit code: 0]", false)],
                },
            ];
            let mut expected = style_tool_rows(
                &super::super::tool_card::started("Bash", &input, 79),
                true,
                false,
                &theme,
                mode,
            );
            expected.extend(completed_tool_rows(
                "Bash",
                "hello\n[exit code: 0]",
                false,
                0,
                80,
                &theme,
                mode,
            ));
            let rows = replay_rows(&messages, 80, &theme, mode);
            assert_eq!(rows, expected);
            let text = rows.join("\n");
            assert!(text.contains("Bash(printf hello)"));
            for hidden in [
                "hidden reasoning",
                "opaque",
                "private",
                "tool_use",
                "tool_result",
                "[exit code:",
            ] {
                assert!(!text.contains(hidden), "unexpected {hidden}: {text}");
            }
        }
    }

    #[test]
    fn replay_pairs_parallel_searches_with_their_results() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![
                    call("a", "Grep", json!({"pattern":"first"})),
                    call("b", "Grep", json!({"pattern":"second"})),
                ],
            },
            Message {
                role: Role::User,
                content: vec![
                    result("a", "first result", false),
                    result("b", "second result", false),
                ],
            },
        ];
        let rows = replay_rows(&messages, 80, &Theme::default(), ColorMode::None).join("\n");
        let first = rows.find("Grep(first)").unwrap();
        let first_result = rows.find("first result").unwrap();
        let second = rows.find("Grep(second)").unwrap();
        let second_result = rows.find("second result").unwrap();
        assert!(
            first < first_result && first_result < second && second < second_result,
            "{rows}"
        );
    }

    #[test]
    fn replay_normalizes_codex_command_calls_for_display_only() {
        for (name, input, command) in [
            (
                "exec_command",
                json!({"cmd": "printf hello"}),
                "printf hello",
            ),
            (
                "functions.exec_command",
                json!({"cmd": "printf hello"}),
                "printf hello",
            ),
            (
                "shell_command",
                json!({"command": "printf hello"}),
                "printf hello",
            ),
            (
                "shell",
                json!({"command": ["bash", "-lc", "pwd"]}),
                "bash -lc pwd",
            ),
            (
                "local_shell_call",
                json!({"action": {"command": ["bash", "-lc", "pwd"]}}),
                "bash -lc pwd",
            ),
            (
                "local_shell",
                json!({"command": ["bash", "-lc", "pwd"]}),
                "bash -lc pwd",
            ),
            (
                "shell_command",
                json!({"command": ["printf", "hello"]}),
                "printf hello",
            ),
        ] {
            let messages = vec![
                Message {
                    role: Role::Assistant,
                    content: vec![call("codex-call", name, input.clone())],
                },
                Message {
                    role: Role::User,
                    content: vec![result("codex-call", "hello\n[exit code: 0]", false)],
                },
            ];
            let original = serde_json::to_value(&messages).unwrap();
            for mode in [ColorMode::None, ColorMode::TrueColor] {
                let theme = Theme::default();
                let mut expected = style_tool_rows(
                    &super::super::tool_card::started("Bash", &json!({"command": command}), 79),
                    true,
                    false,
                    &theme,
                    mode,
                );
                expected.extend(completed_tool_rows(
                    "Bash",
                    "hello\n[exit code: 0]",
                    false,
                    0,
                    80,
                    &theme,
                    mode,
                ));
                let rows = replay_rows(&messages, 80, &theme, mode);
                assert_eq!(rows, expected, "{name}: {input}");
                assert!(rows.join("\n").contains(&format!("Bash({command})")));
                assert_eq!(serde_json::to_value(&messages).unwrap(), original);
            }
        }
    }

    #[test]
    fn replay_resolves_multiple_calls_by_id_in_result_order() {
        let messages = vec![
            Message::text(Role::System, "system instruction"),
            Message::text(Role::User, "user prompt"),
            Message {
                role: Role::Assistant,
                content: vec![
                    Content::Text {
                        text: "before tools".into(),
                    },
                    call("read", "Read", json!({"path": "file.rs"})),
                    call("bash", "Bash", json!({"command": "false"})),
                ],
            },
            Message {
                role: Role::User,
                content: vec![
                    result("bash", "denied", true),
                    result("read", "one\ntwo\nthree\nfour", false),
                ],
            },
            Message::text(Role::Assistant, "after tools"),
        ];
        let text = replay_rows(&messages, 80, &Theme::default(), ColorMode::None).join("\n");
        let positions: Vec<_> = [
            "user prompt",
            "before tools",
            "Bash(false)",
            "Error: denied",
            "Read(file.rs)",
            "Read 4 lines",
            "after tools",
        ]
        .iter()
        .map(|part| text.find(part).unwrap())
        .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(!text.contains("system instruction"));
    }

    #[test]
    fn replay_handles_orphan_results_and_unfinished_calls() {
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![result("missing", "orphan output\u{1b}[31m", false)],
            },
            Message {
                role: Role::Assistant,
                content: vec![call("pending", "Bash", json!({"command": "sleep 1"}))],
            },
        ];
        let text = replay_rows(&messages, 80, &Theme::default(), ColorMode::None).join("\n");
        assert!(text.contains("orphan output"));
        assert!(text.contains("Bash(sleep 1)"));
        assert!(!text.contains("missing"));
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn replay_agent_uses_markdown_result_rendering() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![call(
                    "agent",
                    "Agent",
                    json!({"description": "check", "subagent_type": "general-purpose"}),
                )],
            },
            Message {
                role: Role::User,
                content: vec![result("agent", "**Result:** `ok`", false)],
            },
        ];
        let text = replay_rows(&messages, 80, &Theme::default(), ColorMode::TrueColor).join("\n");
        assert!(text.contains("\u{1b}[1mResult:\u{1b}[22m"));
        assert!(!text.contains("**"));
        assert!(!text.contains('`'));
    }
}

async fn stage_clipboard_image(editor: &mut Editor, status: &mut Status) {
    use crate::i18n::{text, Key, Language};
    status.toast = Some(match super::clipboard::paste_image().await {
        Ok(Some(image)) => {
            editor.set_image(image);
            text(Language::default(), Key::ImageStaged).into()
        }
        Ok(None) => text(Language::default(), Key::ClipboardNoImage).into(),
        Err(error) => format!(
            "{}: {error}",
            text(Language::default(), Key::ImageNotAttached)
        ),
    });
}

async fn handle_aux_action(
    act: &Action,
    renderer: &mut Renderer<io::Stdout>,
    editor: &mut Editor,
    agent: &mut Agent,
    messages: &mut Vec<Message>,
    status: &mut Status,
) -> io::Result<()> {
    let width = renderer.size().0.saturating_sub(1).max(20);
    match act {
        Action::CyclePermission => {
            if commands::next_permission_mode(agent.tools.permissions.mode())
                == crate::perm::Mode::Bypass
            {
                status.bypass_confirmation = true;
                status.bypass_selected_yes = false;
            } else {
                commands::cycle_permission(agent, status);
            }
        }
        Action::Rewind => {
            if let Some(card) =
                commands::execute_command("/rewind", agent, messages, status, width).await?
            {
                renderer.finalize_card(&card)?;
            }
        }
        Action::Transcript => {
            let card = commands::transcript_card(messages, width);
            renderer.finalize_card(&card)?;
        }
        Action::Todos => {
            let card = commands::todos_card(messages, width);
            renderer.finalize_card(&card)?;
        }
        Action::SearchHistory => {
            let card = commands::history_card(editor.history(), editor.text(), width);
            renderer.finalize_card(&card)?;
        }
        Action::Complete if editor.text().starts_with('/') => {
            let choices = commands::candidates(editor.text());
            if let Some(first) = choices.first() {
                editor.set_text(&format!("/{} ", first.name));
            }
        }
        _ => {}
    }
    Ok(())
}

/// Run the event-driven terminal UI; dropping an active run cancels transport IO.
///
/// Completed and interrupted turns are persisted when a session is supplied.
pub async fn run(agent: &mut Agent, options: Options<'_>) -> io::Result<()> {
    let capabilities = Capabilities::detect(true);
    let _guard = TerminalGuard::enter_with_capabilities(capabilities)?;
    let (width, height) = crossterm::terminal::size()?;
    let render_term = if std::env::var_os("GITCODE_TMUX_MANAGED").is_some() {
        std::env::var("GITCODE_TMUX_SOURCE_TERM").ok()
    } else {
        std::env::var("TERM").ok()
    };
    let mode = theme::detect(
        true,
        std::env::var_os("NO_COLOR").is_some(),
        std::env::var("COLORTERM").ok().as_deref(),
        render_term.as_deref(),
    );
    #[cfg(windows)]
    let mode = match std::env::var("GITCODE_PSMUX_COLOR_MODE")
        .ok()
        .filter(|_| std::env::var_os("PSMUX_SESSION").is_some())
        .as_deref()
    {
        Some("truecolor") => ColorMode::TrueColor,
        Some("256") => ColorMode::Indexed256,
        Some("ansi16") => ColorMode::Ansi16,
        Some("none") => ColorMode::None,
        _ => mode,
    };
    let mut renderer = Renderer::with_capabilities(
        io::stdout(),
        width.into(),
        height.into(),
        mode,
        true,
        capabilities,
    );
    let feedback = TerminalFeedback::detect(true);
    let _ = feedback.set_title(&mut io::stdout());
    let mut events = EventStream::new();
    let mut editor = Editor::default();
    if let Some(home) = crate::platform::home_dir() {
        if let Ok(history) = crate::session::history::load(&home) {
            editor.set_history(history);
        }
    }
    let mut initial = None;
    if !options.no_intro {
        match intro::play(&mut events, |elapsed| {
            let (w, h) = renderer.size();
            renderer.paint_styled(&Frame {
                lines: intro::frame(elapsed, w.saturating_sub(1), h, mode),
                cursor: None,
            })
        })
        .await?
        {
            IntroEnd::Skipped(event) => initial = Some(event),
            IntroEnd::Closed => return Ok(()),
            IntroEnd::Finished => {}
        }
    }
    if let Some((choice, latest)) = update_prompt(&mut renderer, &mut events, mode).await? {
        match choice {
            super::update_picker::Choice::UpdateNow => {
                // Leave the alternate screen before the installer writes output.
                _guard.restore();
                let outcome = crate::update::run().await;
                match outcome {
                    Ok(()) => println!("Запустите gitcode заново, чтобы обновиться."),
                    Err(reason) => println!("Не удалось обновиться: {reason}"),
                }
                return Ok(());
            }
            super::update_picker::Choice::SkipVersion => {
                if let Some(home) = crate::platform::home_dir() {
                    crate::update_check::dismiss(&home, &latest);
                }
            }
            super::update_picker::Choice::Later => {}
        }
    }
    let header = Header {
        version: env!("CARGO_PKG_VERSION"),
        model: options.model,
        provider: options.provider,
        cwd: options.cwd,
        branch: options.branch,
    };
    let mut status = Status {
        model: options.model.into(),
        provider: options.provider.into(),
        effort: agent.client.effort().map(str::to_owned),
        permission: options.permission.into(),
        branch: options.branch.into(),
        ..Status::default()
    };
    renderer.enable_viewport()?;
    renderer.set_scroll_step(options.mouse_scroll_speed as usize);
    renderer.set_scroll_animation(options.scroll_animation);
    draw_header(&mut renderer, &header, &status, mode)?;
    let mut persistence = options.session;
    let mut messages = persistence
        .as_ref()
        .map(|(_, session)| session.messages.clone())
        .unwrap_or_default();
    renderer.finalize_styled(&replay_rows(
        &messages,
        renderer.size().0,
        &Theme::default(),
        mode,
    ))?;
    super::statusbar::update_context(&mut status, agent, &messages);
    let queue = InputQueue::default();
    let background_handles = agent.tools.background_handles();
    let completion_notify = agent.tools.completion_notify();
    let mut exit_armed = false;
    let mut copy_deadline = None;
    // Slash commands typed while a turn is streaming are commands, not chat:
    // they run as soon as the turn ends instead of being sent to the model.
    let mut deferred_commands: Vec<String> = Vec::new();
    loop {
        status.background_jobs = background_count(&background_handles);
        status.queued = queue.len();
        paint(
            &mut renderer,
            &editor,
            &status,
            &mut super::markdown::Markdown::new(),
            &mut false,
            false,
            None,
            mode,
        )?;
        let mut submitted_image = None;
        let pending = if !deferred_commands.is_empty() {
            // Slash commands typed during the previous turn run first, in order.
            let command = deferred_commands.remove(0);
            if deferred_commands.is_empty() {
                status.toast = None;
            }
            Action::Submit(command)
        } else if !status.bypass_confirmation && !queue.is_empty() {
            let Some((prompt, image)) = queue.pop_front_with_image() else {
                continue;
            };
            submitted_image = image;
            remove_queued_prompt(&mut status, &prompt);
            status.queued = queue.len();
            Action::Submit(prompt)
        } else {
            let event = match initial.take() {
                Some(event) => Some(event),
                None => loop {
                    tokio::select! {
                        event = events.next() => match event { Some(event) => break Some(event?), None => return Ok(()) },
                        _ = completion_notify.notified() => break None,
                        _ = tokio::time::sleep(Duration::from_millis(35)) => {
                            if renderer.scroll_pending() { break None; }
                            let count = background_count(&background_handles);
                            if count != status.background_jobs { status.background_jobs = count; break None; }
                            if update_copy_notice(&mut renderer, &mut status, &mut copy_deadline)? { break None; }
                        }
                    }
                },
            };
            if let Some(event) = event {
                if status.bypass_confirmation {
                    if let Some(confirmed) =
                        super::bypass_picker::answer(&event, &mut status.bypass_selected_yes)
                    {
                        status.bypass_confirmation = false;
                        if confirmed {
                            agent.tools.permissions.set_mode(crate::perm::Mode::Bypass);
                            status.permission =
                                commands::permission_label(crate::perm::Mode::Bypass).into();
                            status.toast = None;
                        }
                    }
                    if matches!(event, Event::Key(_)) {
                        continue;
                    }
                }
                let next = action(event, &mut editor, &mut renderer);
                if matches!(next, Action::Submit(_)) {
                    submitted_image = editor.take_image();
                }
                next
            } else {
                Action::Refresh
            }
        };
        // A background task may finish while the session is idle. Report it in the
        // transcript and hand it to the model so work continues without polling.
        for job in agent.tools.take_completed() {
            let status_text = if job.timed_out {
                "timed out".to_string()
            } else {
                job.exit_code
                    .map_or_else(|| "finished".to_string(), |code| format!("exit {code}"))
            };
            let rows = completed_tool_rows(
                "Bash",
                &format!("{} finished: {status_text}\n{}", job.label, job.output),
                job.is_error,
                job.duration_ms,
                renderer.size().0,
                &Theme::default(),
                mode,
            );
            renderer.finalize_styled(&rows)?;
            let note = format!(
                "[Background task {} finished: {status_text}]\n$ {}\n{}",
                job.label, job.command, job.output
            );
            queue.push_with_image(note, None);
            status.queued = queue.len();
        }
        match pending {
            Action::ScrollLines(rows) => {
                renderer.scroll_lines(rows);
            }
            Action::ScrollPage(pages) => {
                renderer.scroll_page(pages);
            }
            Action::ScrollBottom => {
                renderer.scroll_to_bottom();
            }
            Action::PasteImage => {
                stage_clipboard_image(&mut editor, &mut status).await;
                exit_armed = false;
            }
            Action::Exit => return Ok(()),
            Action::Interrupt => {
                if exit_armed {
                    return Ok(());
                }
                exit_armed = true;
                renderer.finalize("Для выхода нажмите Ctrl+C ещё раз; Ctrl+D — выход.")?;
            }
            Action::Submit(mut prompt) => {
                exit_armed = false;
                status.toast = None;
                if prompt.starts_with('/') && submitted_image.is_some() {
                    status.toast = Some(
                        crate::i18n::text(
                            crate::i18n::Language::default(),
                            crate::i18n::Key::CommandImageRejected,
                        )
                        .into(),
                    );
                    continue;
                }
                if let Some(command) = prompt.strip_prefix('/') {
                    let (name, args) = command
                        .split_once(char::is_whitespace)
                        .unwrap_or((command, ""));
                    let is_builtin = crate::extensibility::BUILTIN_COMMANDS
                        .iter()
                        .any(|c| c.name == name)
                        || name == "new";
                    if !is_builtin {
                        let home = crate::platform::home_dir()
                            .ok_or_else(|| io::Error::other("home directory is required"))?;
                        let root = agent.tools.permissions.root.clone();
                        let name = name.to_owned();
                        let args = args.to_owned();
                        let expanded = tokio::task::spawn_blocking(move || {
                            let commands =
                                crate::extensibility::load_custom_commands(&home, &root)?;
                            commands
                                .iter()
                                .find(|command| command.name == name)
                                .map(|command| {
                                    if !command.allowed_tools.is_empty() || command.model.is_some()
                                    {
                                        return Err(io::Error::other(
                                            "custom command tool/model overrides are not supported",
                                        ));
                                    }
                                    command.render(&args, &root)
                                })
                                .transpose()
                        })
                        .await
                        .map_err(io::Error::other)??;
                        if let Some(expanded) = expanded {
                            prompt = expanded;
                        }
                    }
                }
                if prompt.trim() == "/exit" {
                    renderer.clear_terminal()?;
                    return Ok(());
                }
                if matches!(prompt.trim(), "/new" | "/clear") {
                    agent.tools.clear_shell_approvals();
                    if let Some((store, _)) = &persistence {
                        let store = store.clone();
                        let create_store = store.clone();
                        let branch = status.branch.clone();
                        let session = tokio::task::spawn_blocking(move || {
                            create_store.create("New conversation", Some(branch))
                        })
                        .await
                        .map_err(io::Error::other)??;
                        persistence = Some((store, session));
                    }

                    let permission = status.permission.clone();
                    let branch = status.branch.clone();
                    status = Status {
                        model: agent.model.clone(),
                        effort: agent.client.effort().map(str::to_owned),
                        permission,
                        branch,
                        ..Status::default()
                    };
                    messages.clear();
                    agent.compaction.clear();
                    super::statusbar::update_context(&mut status, agent, &messages);
                    queue.clear();
                    status.queued_prompts.clear();
                    status.queued = 0;
                    renderer.clear_terminal()?;
                    draw_header(&mut renderer, &header, &status, mode)?;
                    continue;
                }
                if prompt.trim() == "/theme" {
                    if let Some(choice) =
                        super::theme_picker::select(&mut renderer, &mut events).await?
                    {
                        let home = crate::platform::home_dir()
                            .ok_or_else(|| io::Error::other("home directory is required"))?;
                        let saved = tokio::task::spawn_blocking(move || {
                            crate::config::save_theme(&home, choice)
                        })
                        .await
                        .map_err(io::Error::other)?;
                        match saved {
                            Ok(()) => {
                                theme::set_active(choice);
                                renderer.set_theme(Theme::default());
                                status.toast = Some(format!("Тема: {}", choice.preset.label()));
                            }
                            Err(error) => {
                                renderer.set_theme(Theme::default());
                                status.toast = Some(format!("Тема не сохранена: {error}"))
                            }
                        }
                    }
                    continue;
                }
                if prompt.trim() == "/provider" {
                    let home = crate::platform::home_dir()
                        .ok_or_else(|| io::Error::other("home directory is required"))?;
                    let saved = crate::config::load(
                        &home,
                        &agent.tools.permissions.root,
                        &std::collections::BTreeMap::new(),
                        &crate::config::Overrides::new(),
                    )
                    .map(|config| config.providers)
                    .unwrap_or_default();
                    // Show the profile the process is actually using, even when
                    // it came from a CLI flag and was never saved.
                    let mut profiles = saved;
                    if !profiles
                        .iter()
                        .any(|profile| profile.name == status.provider)
                        && !status.provider.is_empty()
                    {
                        profiles.insert(
                            0,
                            crate::config::Provider {
                                name: status.provider.clone(),
                                ..crate::config::Provider::default()
                            },
                        );
                    }
                    let choice = super::provider_picker::select(
                        &mut renderer,
                        &mut events,
                        &profiles,
                        &status.provider,
                    )
                    .await?;
                    let Some(choice) = choice else { continue };
                    let (profile, fresh_token) = match choice {
                        super::provider_picker::Choice::Existing(profile) => (profile, None),
                        super::provider_picker::Choice::Created(created) => {
                            if let Err(error) =
                                store_token(&home, &created.profile.api_key_env, &created.token)
                            {
                                status.toast = Some(error);
                                continue;
                            }
                            (created.profile, Some(created.token))
                        }
                    };
                    let secret = match fresh_token {
                        Some(token) => Some(crate::credentials::Secret::new(token)),
                        None => {
                            let key_env = profile.api_key_env.clone();
                            if key_env.is_empty() {
                                None
                            } else {
                                let env: std::collections::BTreeMap<String, String> =
                                    std::env::vars().collect();
                                let home = home.clone();
                                match tokio::task::spawn_blocking(move || {
                                    crate::credentials::resolve(&home, &key_env, &env)
                                })
                                .await
                                {
                                    Ok(Ok(secret)) => secret,
                                    _ => {
                                        status.toast = Some("Ключ провайдера не прочитан".into());
                                        continue;
                                    }
                                }
                            }
                        }
                    };
                    match activate_provider(agent, &profile, secret, &mut status) {
                        Ok(()) => {
                            let saved = tokio::task::spawn_blocking({
                                let home = home.clone();
                                let profile = profile.clone();
                                move || crate::config::save_provider(&home, profile)
                            })
                            .await
                            .map_err(io::Error::other)?;
                            status.toast = Some(match saved {
                                Ok(()) => format!("Провайдер: {}", profile.name),
                                Err(error) => format!("Провайдер активен, не сохранён: {error}"),
                            });
                        }
                        Err(error) => {
                            status.toast = Some(format!("Провайдер не применён: {error}"))
                        }
                    }
                    continue;
                }
                if prompt.trim() == "/model" {
                    if let Some(model) = super::model_picker::select(
                        &mut renderer,
                        &mut events,
                        &agent.model,
                        &agent.client,
                    )
                    .await?
                    {
                        agent.model = model.clone();
                        status.model = model;
                        apply_model_effort(agent, &mut status);
                        save_selected_model(options.model_provider, &agent.model, &mut status);
                    }
                    continue;
                }
                if prompt.trim() == "/effort" {
                    if let Some(level) = super::effort_picker::select(
                        &mut renderer,
                        &mut events,
                        &agent.model,
                        agent.client.effort(),
                    )
                    .await?
                    {
                        let value = if level == "default" {
                            None
                        } else {
                            Some(level.as_str())
                        };
                        if agent.client.set_effort(value).is_ok() {
                            status.effort = agent.client.effort().map(str::to_owned);
                            save_effort(agent, &mut status);
                        }
                    }
                    continue;
                }
                if let Some(args) = prompt
                    .trim()
                    .strip_prefix("/import")
                    .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
                {
                    let Some((store, _)) = &persistence else {
                        status.toast = Some("Хранилище сессий недоступно".into());
                        continue;
                    };
                    let store = store.clone();
                    let choice = if args.trim().is_empty() {
                        super::import_picker::select(&mut renderer, &mut events).await?
                    } else {
                        Some(super::import_picker::Choice::Path(
                            super::import_picker::expand_path(args.trim()),
                        ))
                    };
                    let path = match choice {
                        Some(super::import_picker::Choice::Path(path)) => Some(path),
                        Some(super::import_picker::Choice::Automatic(source)) => {
                            let home = crate::platform::home_dir()
                                .ok_or_else(|| io::Error::other("home directory is required"))?;
                            let batch_store = store.clone();
                            renderer.finalize_card(&super::widgets::card(
                                "/import",
                                &["Поиск и импорт сессий. Это может занять некоторое время…"
                                    .into()],
                                "",
                                renderer.size().0.saturating_sub(1),
                            ))?;
                            let result = tokio::task::spawn_blocking(move || {
                                crate::session::import::import_all(&home, &batch_store, source)
                            })
                            .await
                            .map_err(io::Error::other)?;
                            let mut body = Vec::new();
                            match result {
                                Ok(report) => {
                                    body.push(format!(
                                        "Импортировано: {} · уже было: {} · названий исправлено: {} · ошибок: {}",
                                        report.imported, report.skipped, report.renamed, report.failed
                                    ));
                                    for error in report.errors {
                                        body.push(format!("Не удалось: {}", super::plain(&error)));
                                    }
                                    body.push(
                                        "Откройте импортированные чаты через /resume.".into(),
                                    );
                                }
                                Err(error) => body.push(format!(
                                    "Ошибка импорта: {}",
                                    super::plain(&error.to_string())
                                )),
                            }
                            renderer.finalize_card(&super::widgets::card(
                                "/import",
                                &body,
                                "",
                                renderer.size().0.saturating_sub(1),
                            ))?;
                            None
                        }
                        None => None,
                    };
                    if let Some(path) = path {
                        let result = tokio::task::spawn_blocking(move || {
                            let conversation = crate::session::import::read_named(&path)?;
                            crate::session::import::save(&store, &conversation)
                        })
                        .await
                        .map_err(io::Error::other)?;
                        match result {
                            Ok(session) => {
                                let body = vec![
                                    format!(
                                        "Импортирована сессия · сообщений: {}",
                                        session.messages.len()
                                    ),
                                    format!("ID: {}", session.id),
                                    "Откройте чат через /resume. Исходный журнал сохранён рядом с сессией.".into(),
                                ];
                                renderer.finalize_card(&super::widgets::card(
                                    "/import",
                                    &body,
                                    "",
                                    renderer.size().0.saturating_sub(1),
                                ))?;
                            }
                            Err(error) => {
                                let body = vec![format!(
                                    "Не удалось импортировать: {}",
                                    super::plain(&error.to_string())
                                )];
                                renderer.finalize_card(&super::widgets::card(
                                    "/import",
                                    &body,
                                    "",
                                    renderer.size().0.saturating_sub(1),
                                ))?;
                            }
                        }
                    }
                    continue;
                }
                if let Some(args) = prompt
                    .trim()
                    .strip_prefix("/resume")
                    .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
                {
                    let Some((store, _)) = &persistence else {
                        status.toast = Some("Хранилище сессий недоступно".into());
                        continue;
                    };
                    let store = store.clone();
                    let id = if args.trim().is_empty() {
                        let list_store = store.clone();
                        match tokio::task::spawn_blocking(move || list_store.list_nonempty())
                            .await
                            .map_err(io::Error::other)?
                        {
                            Ok(list) if !list.is_empty() => {
                                super::session_picker::select(&mut renderer, &mut events, &list)
                                    .await?
                            }
                            Ok(_) => {
                                status.toast = Some("Сохранённых чатов пока нет".into());
                                None
                            }
                            Err(_) => {
                                status.toast = Some("Не удалось прочитать сохранённые чаты".into());
                                None
                            }
                        }
                    } else {
                        Some(args.trim().to_owned())
                    };
                    if let Some(id) = id {
                        let resume_store = store.clone();
                        match tokio::task::spawn_blocking(move || resume_store.resume(&id))
                            .await
                            .map_err(io::Error::other)?
                        {
                            Ok(session) => {
                                agent.tools.clear_shell_approvals();
                                let count = session.messages.len();
                                let id = session.id.clone();
                                let interrupted = session.interrupted_bytes > 0;
                                messages = session.messages.clone();
                                agent.compaction.clear();
                                let checkpoint_store = store.clone();
                                let checkpoint_id = session.id.clone();
                                if let Ok(Ok(Some(data))) = tokio::task::spawn_blocking(move || {
                                    checkpoint_store.load_compaction(&checkpoint_id)
                                })
                                .await
                                {
                                    agent.compaction.restore(&data, &messages);
                                }
                                persistence = Some((store, session));
                                let permission = status.permission.clone();
                                let branch = status.branch.clone();
                                let dirty = status.dirty;
                                status = Status {
                                    model: agent.model.clone(),
                                    effort: agent.client.effort().map(str::to_owned),
                                    permission,
                                    branch,
                                    dirty,
                                    queued: queue.len(),
                                    queued_prompts: status.queued_prompts.clone(),
                                    ..Status::default()
                                };
                                super::statusbar::update_context(&mut status, agent, &messages);
                                let mut body = vec![
                                    format!("Открыта сессия {id} · сообщений: {count}"),
                                    "Новые сообщения будут сохранены в выбранный чат.".into(),
                                ];
                                if interrupted {
                                    body.push("Незавершённый хвост журнала пропущен.".into());
                                }
                                // Start a clean transcript: the opened chat must look like the
                                // operator never left it instead of appending under another one.
                                renderer.clear_terminal()?;
                                draw_header(&mut renderer, &header, &status, mode)?;
                                renderer.finalize_card(&super::widgets::card(
                                    "/resume",
                                    &body,
                                    "",
                                    renderer.size().0.saturating_sub(1),
                                ))?;
                                renderer.finalize_styled(&replay_rows(
                                    &messages,
                                    renderer.size().0,
                                    &Theme::default(),
                                    mode,
                                ))?;
                            }
                            Err(_) => {
                                status.toast = Some(
                                    "Не удалось открыть чат; текущая сессия не изменена".into(),
                                );
                            }
                        }
                    }
                    continue;
                }
                if prompt.trim() == "/usage" {
                    let home = crate::platform::home_dir()
                        .ok_or_else(|| io::Error::other("home directory is required"))?;
                    let entries = tokio::task::spawn_blocking(move || {
                        crate::usage::UsageLedger::open(&home)?.entries(None)
                    })
                    .await
                    .map_err(io::Error::other)?;
                    let entries = match entries {
                        Ok(entries) => entries,
                        Err(_) => {
                            status.toast = Some("Не удалось прочитать журнал использования".into());
                            continue;
                        }
                    };
                    let session_id = persistence
                        .as_ref()
                        .map(|(_, s)| s.id.as_str())
                        .unwrap_or("");
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let mut period = 0;
                    let mut cost_chart = false;
                    let mut scroll = 0usize;
                    loop {
                        let rows = super::usage_panel::render(
                            &entries,
                            session_id,
                            now,
                            period,
                            cost_chart,
                            renderer.size().0.saturating_sub(1),
                        );
                        let lines = super::usage_panel::viewport(
                            &rows,
                            renderer.size().1.saturating_sub(1),
                            &mut scroll,
                        );
                        let theme = crate::theme::Theme::default();
                        let mode = renderer.color_mode();
                        let width = renderer.size().0.saturating_sub(1);
                        let last = lines.len().saturating_sub(1);
                        let lines = lines
                            .iter()
                            .enumerate()
                            .map(|(index, line)| {
                                if index == 0 {
                                    super::picker::heading(line, width, &theme, mode)
                                } else {
                                    let color = if index == last {
                                        theme.placeholder
                                    } else {
                                        theme.panel_text
                                    };
                                    super::picker::text(line, width, color, mode)
                                }
                            })
                            .collect();
                        renderer.paint_styled(&Frame {
                            lines,
                            cursor: None,
                        })?;
                        let Some(event) = events.next().await else {
                            return Ok(());
                        };
                        match event? {
                            Event::Key(key) => match key.code {
                                crossterm::event::KeyCode::Esc => break,
                                crossterm::event::KeyCode::Tab
                                | crossterm::event::KeyCode::Right => period = (period + 1) % 5,
                                crossterm::event::KeyCode::Left => period = (period + 4) % 5,
                                crossterm::event::KeyCode::Up => scroll = scroll.saturating_sub(1),
                                crossterm::event::KeyCode::Down => {
                                    scroll = scroll.saturating_add(1)
                                }
                                crossterm::event::KeyCode::PageUp => {
                                    scroll = scroll.saturating_sub(8)
                                }
                                crossterm::event::KeyCode::PageDown => {
                                    scroll = scroll.saturating_add(8)
                                }
                                crossterm::event::KeyCode::Char('t') => cost_chart = !cost_chart,
                                crossterm::event::KeyCode::Char('c')
                                    if key
                                        .modifiers
                                        .contains(crossterm::event::KeyModifiers::CONTROL) =>
                                {
                                    break
                                }
                                _ => {}
                            },
                            Event::Resize(w, h) => renderer.resize(w.into(), h.into()),
                            _ => {}
                        }
                    }
                    continue;
                }
                let card_width = renderer.size().0.saturating_sub(1);
                if prompt
                    .trim()
                    .strip_prefix("/compact")
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
                {
                    renderer.finalize("Сжимаю контекст… Esc — отмена.")?;
                    let card = {
                        let mut compact = Box::pin(commands::execute_command(
                            &prompt,
                            agent,
                            &mut messages,
                            &mut status,
                            card_width,
                        ));
                        loop {
                            tokio::select! {
                                result = &mut compact => break result?,
                                event = events.next() => match event {
                                    Some(Ok(Event::Key(key))) if matches!(key.code, crossterm::event::KeyCode::Esc)
                                        || (matches!(key.code, crossterm::event::KeyCode::Char('c')) && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)) => break None,
                                    Some(Ok(Event::Resize(w, h))) => renderer.resize(w.into(), h.into()),
                                    Some(Ok(_)) => {},
                                    Some(Err(error)) => return Err(error),
                                    None => return Ok(()),
                                }
                            }
                        }
                    };
                    if let Some(card) = card {
                        renderer.finalize_card(&card)?;
                    } else {
                        renderer.finalize("Сжатие отменено; полная история сохранена.")?;
                    }
                    let requests = agent.compaction.take_manual_usage();
                    if !requests.is_empty() {
                        if let Some((_, session)) = &persistence {
                            let id = session.id.clone();
                            let project = session.project.clone();
                            let provider = options.provider.to_owned();
                            let model = agent.model.clone();
                            let home = crate::platform::home_dir()
                                .ok_or_else(|| io::Error::other("home directory is required"))?;
                            if tokio::task::spawn_blocking(move || {
                                crate::usage::record_turn(
                                    &home, &id, &project, &provider, &model, &requests,
                                )
                            })
                            .await
                            .map_err(io::Error::other)?
                            .is_err()
                            {
                                status.toast =
                                    Some("Не удалось сохранить статистику сжатия".into());
                            }
                        }
                    }
                    if save_compaction_checkpoint(agent, &persistence, &messages)
                        .await
                        .is_err()
                    {
                        status.toast = Some(
                            "Не удалось сохранить сжатый контекст; полная история сохранена".into(),
                        );
                    }
                    super::statusbar::update_context(&mut status, agent, &messages);
                    continue;
                }
                let previous_model = agent.model.clone();
                if let Some(card) = commands::execute_command(
                    &prompt,
                    agent,
                    &mut messages,
                    &mut status,
                    card_width,
                )
                .await?
                {
                    if agent.model != previous_model {
                        apply_model_effort(agent, &mut status);
                        save_selected_model(options.model_provider, &agent.model, &mut status);
                    } else {
                        let parts: Vec<_> = prompt.split_whitespace().collect();
                        if matches!(
                            parts.as_slice(),
                            ["/model", _, ..] | ["/config", "model", _, ..]
                        ) {
                            save_selected_model(options.model_provider, &agent.model, &mut status);
                        }
                    }
                    let mut command_parts = prompt.split_whitespace();
                    if command_parts.next() == Some("/effort") {
                        if let Some(level) = command_parts.next() {
                            if matches!(
                                level.to_ascii_lowercase().as_str(),
                                "default" | "low" | "medium" | "high" | "xhigh" | "max"
                            ) {
                                save_effort(agent, &mut status);
                            }
                        }
                    }
                    renderer.finalize_card(&card)?;
                    super::statusbar::update_context(&mut status, agent, &messages);
                    continue;
                }
                renderer.finalize_styled(&format_user_turn(
                    &prompt,
                    renderer.size().0,
                    &Theme::default(),
                    mode,
                ))?;
                let mut request = messages.clone();
                // Capture the title before the prompt text is moved into the message.
                let first_prompt = messages
                    .iter()
                    .all(|message| message.role != Role::User)
                    .then(|| session_title(&prompt));
                let mut user_message = Message::text(Role::User, prompt);
                if let Some(image) = submitted_image {
                    user_message.content.push(image);
                }
                request.push(user_message);
                // Persist the operator prompt before the model runs: a reopened session must
                // show the request even if the process dies mid-turn.
                let mut persisted = messages.len();
                if let Some((store, session)) = &persistence {
                    let writer = store.clone();
                    let id = session.id.clone();
                    let pending = request[persisted..].to_vec();
                    if matches!(
                        tokio::task::spawn_blocking(move || writer.append_many(&id, &pending))
                            .await,
                        Ok(Ok(()))
                    ) {
                        persisted = request.len();
                    }
                    // Name the chat as soon as it has a topic, so /resume lists
                    // readable entries instead of "gitcode session".
                    if let Some(title) = first_prompt {
                        let store = store.clone();
                        let id = session.id.clone();
                        // An unknown branch is stored as absent, not as an empty label.
                        let branch =
                            (!status.branch.trim().is_empty()).then(|| status.branch.clone());
                        let _ = tokio::task::spawn_blocking(move || {
                            store.update_metadata(&id, title, branch)
                        })
                        .await;
                    }
                }
                super::statusbar::update_context(&mut status, agent, &request);
                let (sender, mut progress) = mpsc::channel(256);
                let (approval_sender, mut approvals) = mpsc::channel(1);
                let mut approval: Option<super::approval_picker::Pending> = None;
                let (question_sender, mut questions) = mpsc::channel(1);
                let mut question: Option<super::question_picker::Pending> = None;
                let started = Instant::now();
                let mut stream = super::markdown::Markdown::new();
                let mut stream_started = false;
                let mut running_tools = 0usize;
                let mut response_chunk = String::new();
                let mut committed_response_text = false;
                let mut dirty = true;
                let base = status.clone();
                start_turn_status(&mut status);
                let mut live_usage = LiveUsage::default();
                let mut live_context = LiveContext::default();
                let mut retry_deadline: Option<(Instant, u32, u32)> = None;
                // A live handle lets the operator change the mode while the agent owns Tools.
                let permission_handle = agent.tools.permissions.mode_handle();

                let ledger_model = agent.model.clone();
                let mut frames = frame_clock();
                let checkpoint = crate::agent::TurnCheckpoint::new(request.clone());
                let mut recovery = None;
                let mut run = Box::pin(agent.run_with_questions(
                    request,
                    Some(&sender),
                    Some(&checkpoint),
                    Some(&queue),
                    Some(&approval_sender),
                    Some(&question_sender),
                ));
                loop {
                    tokio::select! {
                        result = &mut run => {
                            finish_turn_status(&mut status, result.is_ok());
                            match result {
                                Ok(outcome) => {
                                    let finished_at = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default().as_secs();
                                    let elapsed = started.elapsed();
                                    status.input = base.input + outcome.usage.input_tokens;
                                    status.output = base.output + outcome.usage.output_tokens;
                                    status.turn_output = outcome.usage.output_tokens;
                                    status.output_estimated = base.output_estimated || outcome.usage.estimated;
                                    status.cache_read = base.cache_read + outcome.usage.cache_read_tokens;
                                    status.cache_write = base.cache_write + outcome.usage.cache_write_tokens;
                                    status.tokens_per_second = Some(outcome.usage.output_tokens as f64 / started.elapsed().as_secs_f64().max(0.001));
                                    if let Some((store, session)) = &persistence {
                                        let store = store.clone();
                                        let id = session.id.clone();
                                        let appended = outcome.messages[persisted.min(outcome.messages.len())..].to_vec();
                                        let requests = outcome.request_usage.clone();
                                        let home = crate::platform::home_dir().ok_or_else(|| io::Error::other("home directory is required"))?;
                                        let project = session.project.clone();
                                        let provider = options.provider.to_owned();
                                        let model = ledger_model.clone();
                                        let saved = tokio::task::spawn_blocking(move || {
                                            crate::usage::record_turn(&home, &id, &project, &provider, &model, &requests)?;
                                            store.append_many(&id, &appended)?;
                                            Ok::<_, io::Error>(())
                                        }).await;
                                        // Journal failures must never end the chat: the
                                        // conversation is still live in memory.
                                        match saved {
                                            Ok(Ok(())) => persisted = outcome.messages.len(),
                                            Ok(Err(error)) => {
                                                status.toast = Some(format!(
                                                    "Сессия не сохранена: {error}"
                                                ));
                                            }
                                            Err(error) => {
                                                status.toast = Some(format!(
                                                    "Сессия не сохранена: {error}"
                                                ));
                                            }
                                        }
                                    }
                                    messages = outcome.messages;
                                    let final_text = if response_chunk.is_empty()
                                        && !committed_response_text
                                    {
                                        &outcome.text
                                    } else {
                                        &response_chunk
                                    };
                                    if !final_text.is_empty() {
                                        if response_chunk.is_empty() && !committed_response_text && !stream_started {
                                            stream.push(final_text);
                                        }
                                        renderer.finalize_styled(&format_assistant_turn(
                                            &mut stream,
                                            stream_started,
                                            renderer.size().0,
                                            &Theme::default(),
                                            mode,
                                        ))?;
                                    }
                                    let line = completion_line(finished_at, elapsed, &outcome.usage, renderer.size().0.saturating_sub(1), mode);
                                    renderer.finalize_styled(&[line, String::new()])?;
                                    let _ = feedback.notify_completed_turn(&mut io::stdout());
                                    tokio::spawn(async move { let _ = feedback.notify_completed_turn_native().await; });
                                }
                                Err(error) => {
                                    recovery = Some(false);
                                    if !response_chunk.trim().is_empty() {
                                        renderer.finalize_styled(&format_assistant_turn(
                                            &mut stream,
                                            stream_started,
                                            renderer.size().0,
                                            &Theme::default(),
                                            mode,
                                        ))?;
                                    }
                                    let turn_limit_reached = error.starts_with("agent reached the configured turn limit");
                                    let advice = if turn_limit_reached {
                                        "Совет: /config max_turns 0 и затем «продолжай».".into()
                                    } else {
                                        "Совет: проверь соединение или выполни /doctor.".into()
                                    };
                                    let err_card = widgets::card(
                                        "Ошибка",
                                        &[
                                            format!("Причина: {error}"),
                                            advice,
                                        ],
                                        if turn_limit_reached { "/config настройки" } else { "/doctor диагностика" },
                                        renderer.size().0.saturating_sub(1),
                                    );
                                    renderer.finalize_card(&err_card)?;
                                }
                            }
                            break;
                        }
                        event = progress.recv() => {
                            status.elapsed_secs = started.elapsed().as_secs();
                            status.elapsed_millis = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                            match event {
                                Some(Progress::ContextUsage { used_tokens, window_tokens }) => {
                                    live_context.reset(used_tokens, window_tokens, &mut status);
                                }
                                Some(Progress::CompactionStarted) => status.compacting = true,
                                Some(Progress::CompactionFinished) => status.compacting = false,
                                Some(Progress::TextDelta { text }) => {
                                    retry_deadline = None;
                                    status.retry_notice = None;
                                    response_chunk.push_str(&text);
                                    stream.push(&text);
                                    live_context.add_bytes(text.len(), &mut status);
                                    live_usage.add_text(&text);
                                    live_usage.apply(&mut status, &base, started.elapsed());
                                }
                                Some(Progress::ThinkingDelta { text }) => {
                                    retry_deadline = None;
                                    status.retry_notice = None;
                                    live_context.add_bytes(text.len(), &mut status);
                                    live_usage.add_text(&text);
                                    live_usage.apply(&mut status, &base, started.elapsed());
                                }
                                Some(Progress::RetryNotice { attempt, max_attempts, delay_ms }) => {
                                    retry_deadline = Some((Instant::now() + Duration::from_millis(delay_ms), attempt, max_attempts));
                                }
                                Some(Progress::ToolStarted { name, input, .. }) => {
                                    let bytes = serde_json::to_vec(&input).map_or(0, |data| data.len()).saturating_add(name.len());
                                    live_context.add_bytes(bytes, &mut status);
                                    running_tools = running_tools.saturating_add(1);
                                    status.tools_used = status.tools_used.saturating_add(1);
                                    retry_deadline = None;
                                    status.retry_notice = None;
                                    status.toast = None;
                                    if !response_chunk.trim().is_empty() {
                                        renderer.finalize_styled(&format_assistant_turn(
                                            &mut stream,
                                            stream_started,
                                            renderer.size().0,
                                            &Theme::default(),
                                            mode,
                                        ))?;
                                        response_chunk.clear();
                                        stream = super::markdown::Markdown::new();
                                        stream_started = false;
                                        committed_response_text = true;
                                    }
                                    let rows = super::tool_card::started(
                                        &name,
                                        &input,
                                        renderer.size().0.saturating_sub(1).max(20),
                                    );
                                    renderer.finalize_styled(&style_tool_rows(&rows, true, false, &Theme::default(), mode))?;
                                }
                                Some(Progress::ToolFinished { name, output, is_error, duration_ms, .. }) => {
                                    live_context.add_bytes(output.len(), &mut status);
                                    running_tools = running_tools.saturating_sub(1);
                                    let rows = completed_tool_rows(&name, &output, is_error, duration_ms, renderer.size().0, &Theme::default(), mode);
                                    renderer.finalize_styled(&rows)?;
                                    status.toast = None;
                                    // Completed steps reach the journal mid-turn so a reopened
                                    // session already shows them; the streaming tail stays in memory.
                                    let snapshot = checkpoint.stable();
                                    if let Some((store, session)) = &persistence {
                                        if snapshot.len() > persisted {
                                            let store = store.clone();
                                            let id = session.id.clone();
                                            let pending = snapshot[persisted..].to_vec();
                                            if matches!(
                                                tokio::task::spawn_blocking(move || store.append_many(&id, &pending)).await,
                                                Ok(Ok(()))
                                            ) {
                                                persisted = snapshot.len();
                                            }
                                        }
                                    }
                                }
                                Some(Progress::SubagentStarted { id, name, task }) => {
                                    status.subagents.push(crate::agent::agency::SubagentRun {
                                        id, name, task, current_tool: None, tokens: 0, elapsed_ms: 0,
                                        state: crate::agent::agency::SubagentRunState::Running,
                                    });
                                }
                                Some(Progress::SubagentUpdated { id, tool, tokens }) => {
                                    if let Some(run) = status.subagents.iter_mut().find(|run| run.id == id) {
                                        run.current_tool = tool;
                                        run.tokens = tokens;
                                    }
                                }
                                Some(Progress::SubagentFinished { id, success, tokens }) => {
                                    if let Some(run) = status.subagents.iter_mut().find(|run| run.id == id) {
                                        run.state = if success { crate::agent::agency::SubagentRunState::Completed } else { crate::agent::agency::SubagentRunState::Failed };
                                        run.current_tool = None;
                                        run.tokens = tokens;
                                    }
                                }
                                Some(Progress::SteeringApplied { messages: entries }) => {
                                    if !response_chunk.trim().is_empty() {
                                        renderer.finalize_styled(&format_assistant_turn(
                                            &mut stream, stream_started, renderer.size().0, &Theme::default(), mode,
                                        ))?;
                                        response_chunk.clear();
                                        stream = super::markdown::Markdown::new();
                                        stream_started = false;
                                        committed_response_text = true;
                                    }
                                    for entry in &entries {
                                        remove_queued_prompt(&mut status, entry);
                                        renderer.finalize_styled(&format_user_turn(entry, renderer.size().0, &Theme::default(), mode))?;
                                    }
                                    status.queued = queue.len();
                                    queue.acknowledge();
                                }
                                Some(Progress::BackgroundJobFinished { label, output, is_error, duration_ms, .. }) => {
                                    if !response_chunk.trim().is_empty() {
                                        renderer.finalize_styled(&format_assistant_turn(
                                            &mut stream, stream_started, renderer.size().0, &Theme::default(), mode,
                                        ))?;
                                        response_chunk.clear();
                                        stream = super::markdown::Markdown::new();
                                        stream_started = false;
                                        committed_response_text = true;
                                    }
                                    let rows = completed_tool_rows(
                                        "Bash",
                                        &format!("{label} finished\n{output}"),
                                        is_error,
                                        duration_ms,
                                        renderer.size().0,
                                        &Theme::default(),
                                        mode,
                                    );
                                    renderer.finalize_styled(&rows)?;
                                }
                                Some(Progress::Usage { usage }) => {
                                    live_usage.snapshot(usage);
                                    live_usage.apply(&mut status, &base, started.elapsed());
                                }
                                Some(Progress::Done { .. }) => {
                                    live_usage.finish_request();
                                    live_usage.apply(&mut status, &base, started.elapsed());
                                }
                                _ => {},
                            }
                            dirty = true;
                        }
                        request = questions.recv() => {
                            if let Some(request) = request { question = Some(super::question_picker::Pending::new(request)); dirty = true; }
                        }
                        request = approvals.recv() => {
                            if let Some(request) = request {
                                approval = Some(super::approval_picker::Pending::new(request));
                                dirty = true;
                            }
                        }
                        event = events.next() => {
                            let Some(event) = event else { return Ok(()); };
                            let event = event?;
                            if let Some(pending) = &mut question {
                                if pending.in_chat() {
                                    match action(event, &mut editor, &mut renderer) {
                                        Action::Submit(text) => {
                                            renderer.finalize_styled(&format_user_turn(&text, renderer.size().0, &Theme::default(), mode))?;
                                            if let Some(pending) = question.take() { pending.finish_chat(text); }
                                            status.toast = None;
                                        }
                                        Action::Interrupt | Action::Exit => { if let Some(pending) = question.take() { pending.finish(false); } }
                                        _ => {}
                                    }
                                } else if let Event::Resize(w, h) = event { renderer.resize(w.into(), h.into()); }
                                else if let Some(submit) = pending.answer(&event) { if let Some(pending) = question.take() { pending.finish(submit); } }
                                dirty = true;
                                continue;
                            }
                            if let Some(pending) = &mut approval {
                                if let Event::Resize(w, h) = event {
                                    renderer.resize(w.into(), h.into());
                                    pending.resized();
                                } else {
                                    let (width, height) = renderer.size();
                                    if let Some(decision) = pending.answer(&event, width.saturating_sub(1), height.saturating_sub(1).max(1)) {
                                        if let Some(pending) = approval.take() { pending.finish(decision); }
                                    }
                                }
                                dirty = true;
                                continue;
                            }
                            if status.bypass_confirmation {
                                if let Some(confirmed) = super::bypass_picker::answer(&event, &mut status.bypass_selected_yes) {
                                    status.bypass_confirmation = false;
                                    if confirmed {
                                        // Apply at once: tools that have not run yet must
                                        // skip their prompts instead of waiting for turn end.
                                        permission_handle.set_mode(crate::perm::Mode::Bypass);
                                        status.permission = commands::permission_label(crate::perm::Mode::Bypass).into();
                                        status.toast = None;
                                    }
                                }
                                if matches!(event, Event::Key(_)) { dirty = true; continue; }
                            }
                            match action(event, &mut editor, &mut renderer) {
                                Action::Exit => return Ok(()),
                                Action::Interrupt => {
                                    status.busy = false;
                                    status.retry_notice = None;
                                    if !response_chunk.trim().is_empty() {
                                        renderer.finalize_styled(&format_assistant_turn(
                                            &mut stream,
                                            stream_started,
                                            renderer.size().0,
                                            &Theme::default(),
                                            mode,
                                        ))?;
                                    }
                                    if running_tools > 0 { renderer.finalize("")?; }
                                    while let Ok(event) = progress.try_recv() {
                                        if let Progress::SteeringApplied { messages: entries } = event {
                                            for entry in &entries {
                                                remove_queued_prompt(&mut status, entry);
                                                renderer.finalize_styled(&format_user_turn(entry, renderer.size().0, &Theme::default(), mode))?;
                                            }
                                            queue.acknowledge();
                                        }
                                    }
                                    for run in &mut status.subagents {
                                        if run.state == crate::agent::agency::SubagentRunState::Running {
                                            run.state = crate::agent::agency::SubagentRunState::Failed;
                                            run.current_tool = None;
                                        }
                                    }
                                    renderer.finalize_styled(&interruption_rows(&Theme::default(), mode))?;
                                    recovery = Some(true);
                                    break;
                                }
                                Action::Submit(prompt) => {
                                    if is_slash_command(&prompt, &[]) {
                                        // A command typed mid-turn must never reach the
                                        // model as steering text; it waits for the turn
                                        // to finish and runs on its own.
                                        deferred_commands.push(prompt);
                                        status.toast = Some(format!(
                                            "Команда выполнится после ответа ({})",
                                            deferred_commands.len()
                                        ));
                                    } else {
                                        status.queued_prompts.push(prompt.clone());
                                        queue.push_with_image(prompt, editor.take_image());
                                        status.queued = queue.len();
                                    }
                                }
                                Action::PasteImage => {
                                    stage_clipboard_image(&mut editor, &mut status).await;
                                }
                                Action::Transcript => {
                                    let w = renderer.size().0.saturating_sub(1).max(20);
                                    renderer.finalize_card(&commands::transcript_card(&messages, w))?;
                                }
                                Action::Todos => {
                                    let w = renderer.size().0.saturating_sub(1).max(20);
                                    renderer.finalize_card(&commands::todos_card(&messages, w))?;
                                }
                                Action::SearchHistory => {
                                    let w = renderer.size().0.saturating_sub(1).max(20);
                                    renderer.finalize_card(&commands::history_card(editor.history(), editor.text(), w))?;
                                }
                                Action::CyclePermission => {
                                    let next = commands::next_permission_mode(permission_handle.mode());
                                    if next == crate::perm::Mode::Bypass {
                                        status.bypass_confirmation = true;
                                        status.bypass_selected_yes = false;
                                    } else {
                                        permission_handle.set_mode(next);
                                        status.permission = commands::permission_label(next).into();
                                        status.toast = None;
                                    }
                                }
                                Action::Rewind | Action::Complete => {
                                    status.toast = Some("Заверши или отмени активный ход".into());
                                }
                                _ => {},
                            }
                            dirty = true;
                        }
                        _ = frames.tick(), if dirty || status.busy => {
                            update_copy_notice(&mut renderer, &mut status, &mut copy_deadline)?;
                            status.background_jobs = background_count(&background_handles);
                            // tmux does not always deliver a resize when a client
                            // reattaches, so a resumed session would keep painting
                            // into stale geometry and the live turn stayed invisible.
                            if let Ok((width, height)) = crossterm::terminal::size() {
                                let (width, height) = (usize::from(width.max(1)), usize::from(height.max(1)));
                                if (width, height) != renderer.size() {
                                    renderer.resize(width, height);
                                }
                            }
                            status.elapsed_secs = started.elapsed().as_secs();
                            status.elapsed_millis = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                            if let Some((deadline, attempt, max_attempts)) = retry_deadline {
                                // Round up so the countdown never reads "0с" while waiting.
                                let seconds = deadline
                                    .saturating_duration_since(Instant::now())
                                    .as_millis()
                                    .div_ceil(1000)
                                    .max(1);
                                status.retry_notice = Some(format!(
                                    "Переподключаюсь · попытка {attempt}/{max_attempts} · через {seconds}с"
                                ));
                            }
                            if let Some(pending) = question.as_ref().filter(|pending| !pending.in_chat()) {
                                let (width, height) = renderer.size();
                                renderer.paint_styled(&pending.frame(width.saturating_sub(1), height.saturating_sub(1).max(1), mode))?;
                            } else {
                                paint(&mut renderer, &editor, &status, &mut stream, &mut stream_started, running_tools > 0, approval.as_ref(), mode)?;
                            }
                            dirty = false;
                        }
                    }
                }
                drop(run);
                if let Some(interrupted) = recovery {
                    let recovered = if interrupted {
                        checkpoint.interrupted(&response_chunk)
                    } else {
                        checkpoint.failed(&response_chunk)
                    };
                    if let Some((store, session)) = &persistence {
                        let store = store.clone();
                        let id = session.id.clone();
                        let appended = recovered[persisted.min(recovered.len())..].to_vec();
                        if let Ok(Err(error)) =
                            tokio::task::spawn_blocking(move || store.append_many(&id, &appended))
                                .await
                        {
                            // An interrupted turn still ends the chat cleanly.
                            status.toast = Some(format!("Сессия не сохранена: {error}"));
                        }
                    }
                    messages = recovered;
                }
                super::statusbar::update_context(&mut status, agent, &messages);
                if save_compaction_checkpoint(agent, &persistence, &messages)
                    .await
                    .is_err()
                {
                    status.toast = Some(
                        "Не удалось сохранить сжатый контекст; полная история сохранена".into(),
                    );
                }
            }
            other @ (Action::Rewind
            | Action::Transcript
            | Action::Todos
            | Action::CyclePermission
            | Action::Complete
            | Action::SearchHistory) => {
                exit_armed = false;
                handle_aux_action(
                    &other,
                    &mut renderer,
                    &mut editor,
                    agent,
                    &mut messages,
                    &mut status,
                )
                .await?;
                super::statusbar::update_context(&mut status, agent, &messages);
            }
            Action::None => {}
            _ => exit_armed = false,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod completion_tests {
    use super::*;
    #[test]
    fn completion_receipt_has_utc_elapsed_and_per_turn_usage() {
        let usage = Usage {
            output_tokens: 12,
            ..Usage::default()
        };
        assert_eq!(
            completion_line(
                86_400 + 13 * 3600 + 5 * 60 + 9,
                Duration::from_secs(5),
                &usage,
                120,
                ColorMode::None
            ),
            "  13:05:09 UTC • 5с • 12↓"
        );
        let estimated = Usage {
            output_tokens: 3,
            estimated: true,
            ..Usage::default()
        };
        assert_eq!(
            completion_line(
                0,
                Duration::from_secs(3725),
                &estimated,
                120,
                ColorMode::None
            ),
            "  00:00:00 UTC • 1ч 2м 5с • ~3↓"
        );
        let large = Usage {
            output_tokens: 13_291,
            ..Usage::default()
        };
        assert!(
            completion_line(0, Duration::ZERO, &large, 120, ColorMode::None).ends_with("13.3k↓")
        );
        assert!(
            completion_line(0, Duration::ZERO, &usage, 120, ColorMode::TrueColor)
                .contains("\x1b[1m•")
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod display_regressions {
    use super::*;
    use serde_json::json;
    #[test]
    fn leading_blank_stream_chunks_do_not_detach_the_assistant_marker() {
        for mode in [ColorMode::None, ColorMode::TrueColor] {
            let mut stream = super::super::markdown::Markdown::new();
            stream.push("\n\n\n\n");
            let stable = stream.drain_stable(80, &Theme::default(), mode, 1);
            assert!(decorate_assistant_rows(stable, false, &Theme::default(), mode).is_empty());
            assert!(assistant_rows(&stream, 80, &Theme::default(), mode, false).is_empty());
            stream.push("Запускаю исследователей.");
            let rows = assistant_rows(&stream, 80, &Theme::default(), mode, false);
            let visible: Vec<_> = rows.iter().map(|row| widgets::unstyled(row)).collect();
            assert!(visible[0].starts_with("✿ Запускаю") || visible[0].starts_with("* Запускаю"));
            let final_rows = format_assistant_turn(&mut stream, false, 80, &Theme::default(), mode);
            let first = widgets::unstyled(&final_rows[0]);
            assert!(first.starts_with("✿ Запускаю") || first.starts_with("* Запускаю"));
        }
    }

    #[test]
    fn new_turn_resets_its_live_tokens_immediately_but_keeps_session_total() {
        let mut status = Status {
            input: 100,
            output: 13337,
            turn_output: 13291,
            elapsed_secs: 42,
            elapsed_millis: 42000,
            tokens_per_second: Some(12.0),
            output_estimated: true,
            ..Status::default()
        };
        start_turn_status(&mut status);
        assert!(status.busy);
        assert_eq!(status.turn_output, 0);
        assert_eq!(status.elapsed_secs, 0);
        assert_eq!(status.elapsed_millis, 0);
        assert_eq!(status.tokens_per_second, None);
        assert_eq!((status.input, status.output), (100, 13337));
        let frame = Frame::input(&Editor::default(), &status, 80, 24);
        assert!(frame
            .lines
            .iter()
            .any(|line| line.contains("Думаю… (0с · ↓ 0 токенов)")));
        status.compacting = true;
        let frame = Frame::input(&Editor::default(), &status, 80, 24);
        assert!(frame
            .lines
            .iter()
            .any(|line| line.contains("Сжимаю контекст…")));
        status.compacting = false;
        let frame = Frame::input(&Editor::default(), &status, 80, 24);
        assert!(frame.lines.iter().any(|line| line.contains("Думаю…")));
    }

    #[test]
    fn tool_rows_keep_rose_marker_white_invocation_and_placeholder_output() {
        use crate::theme::{default_foreground, foreground, ColorMode, Theme};
        let theme = Theme::default();
        let rows = crate::tui::tool_card::started("Bash", &json!({"command": "printf hello"}), 80);
        let styled = style_tool_rows(&rows, true, false, &theme, ColorMode::TrueColor);
        assert_eq!(
            styled[0],
            format!(
                "{}{}{} Bash(printf hello)",
                foreground(theme.rose, ColorMode::TrueColor),
                "●",
                default_foreground(ColorMode::TrueColor),
            )
        );
        let done = completed_tool_rows("Bash", "hello", false, 5, 80, &theme, ColorMode::TrueColor);
        assert!(done[0].starts_with(&foreground(theme.placeholder, ColorMode::TrueColor)));
        assert!(done[0].contains("⎿"));
        let failed = completed_tool_rows("Bash", "boom", true, 5, 80, &theme, ColorMode::TrueColor);
        assert!(failed[0].starts_with(&foreground(theme.error, ColorMode::TrueColor)));
    }

    #[test]
    fn copied_notice_expires_without_changing_other_toasts() {
        let mut renderer = Renderer::new(io::stdout(), 80, 24, ColorMode::None, false);
        let mut status = Status {
            copied_chars: Some(12),
            toast: Some("Другое уведомление".into()),
            ..Status::default()
        };
        let mut deadline = Some(Instant::now() - Duration::from_millis(1));
        assert!(update_copy_notice(&mut renderer, &mut status, &mut deadline).unwrap());
        assert_eq!(status.copied_chars, None);
        assert_eq!(status.toast.as_deref(), Some("Другое уведомление"));
        assert!(!update_copy_notice(&mut renderer, &mut status, &mut deadline).unwrap());
    }
}

fn background_count(handles: &std::sync::Mutex<Vec<tokio::task::AbortHandle>>) -> usize {
    let mut handles = handles.lock().unwrap_or_else(|e| e.into_inner());
    handles.retain(|handle| !handle.is_finished());
    handles.len()
}
