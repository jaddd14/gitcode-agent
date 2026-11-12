//! Minimal interactive orchestration with native scrollback and cancellable turns.

use super::{
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
    provider::canonical::{Event as Progress, Message, Role, Usage},
    theme::{self, ColorMode, Theme},
};
use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use std::{
    io,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

// Keep deadlines independent of event arrival; dropped select futures must not
// restart the frame delay. Skip missed frames rather than burst-rendering them.
fn frame_clock() -> tokio::time::Interval {
    let mut clock = tokio::time::interval(Duration::from_nanos(1_000_000_000 / 60));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    clock
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
        status.output_estimated = self.completed.estimated
            || self.current.estimated
            || estimate > self.current.output_tokens;
        status.cache_read = base.cache_read.saturating_add(total.cache_read_tokens);
        status.cache_write = base.cache_write.saturating_add(total.cache_write_tokens);
        status.tokens_per_second = Some(turn_output as f64 / elapsed.as_secs_f64().max(0.001));
    }
}

#[cfg(test)]
#[path = "../../tests/tui_progress/regression.rs"]
mod regression;

/// Cached startup metadata; construction must not block on git or network work.
pub struct Options<'a> {
    /// Model display name.
    pub model: &'a str,
    /// Provider display name.
    pub provider: &'a str,
    /// Working directory display path.
    pub cwd: &'a str,
    /// Cached branch name, or an unknown marker.
    pub branch: &'a str,
    /// Current effective permission mode label.
    pub permission: &'a str,
    /// Disable the skippable startup animation.
    pub no_intro: bool,
    /// Persistent project session and recovered conversation.
    pub session: Option<(
        crate::session::store::SessionStore,
        crate::session::store::Session,
    )>,
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

fn paint(
    renderer: &mut Renderer<io::Stdout>,
    editor: &Editor,
    status: &Status,
    stream: &mut super::markdown::Markdown,
    stream_started: &mut bool,
    tool_running: bool,
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
        *stream_started = true;
        rows
    } else {
        Vec::new()
    };
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
    let lines = header.live_lines_for_size(width, status, height);
    let rows = lines
        .iter()
        .map(|row| widgets::style_frame_row_with_panel(row, &Theme::default(), mode, false))
        .collect::<Vec<_>>();
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
    editor.handle(event)
}

fn format_user_turn(prompt: &str, width: usize, theme: &Theme, mode: ColorMode) -> Vec<String> {
    let width = width.saturating_sub(1).max(1);
    let mut rows = Vec::new();
    for (idx, line) in super::markdown::wrap_words(prompt, width.saturating_sub(4).max(1))
        .iter()
        .enumerate()
    {
        let prefix = if idx == 0 { "│ › " } else { "│   " };
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
            "│ › abcdefg",
            "│   hijklmn",
            "│   op",
            "│   вторая ",
            "│   строка",
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

fn finish_turn_status(status: &mut Status, succeeded: bool) {
    status.busy = false;
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
                    theme::reset(mode)
                )
            } else {
                let color = if error {
                    theme::foreground(theme.error, mode)
                } else if started {
                    theme::default_foreground(mode).to_owned()
                } else {
                    theme::foreground(theme.panel_text, mode)
                };
                format!("{color}{row}{}", theme::reset(mode))
            }
        })
        .collect()
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
            commands::cycle_permission(agent, status);
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
    let _guard = TerminalGuard::enter()?;
    let (width, height) = crossterm::terminal::size()?;
    let mode = theme::detect(
        true,
        std::env::var_os("NO_COLOR").is_some(),
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    );
    let mut renderer = Renderer::new(io::stdout(), width.into(), height.into(), mode, true);
    let mut events = EventStream::new();
    let mut editor = Editor::default();
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
    let header = Header {
        version: env!("CARGO_PKG_VERSION"),
        model: options.model,
        provider: options.provider,
        cwd: options.cwd,
        branch: options.branch,
    };
    let mut status = Status {
        model: options.model.into(),
        permission: options.permission.into(),
        branch: options.branch.into(),
        ..Status::default()
    };
    renderer.enable_viewport()?;
    draw_header(&mut renderer, &header, &status, mode)?;
    let mut persistence = options.session;
    let mut messages = persistence
        .as_ref()
        .map(|(_, session)| session.messages.clone())
        .unwrap_or_default();
    let queue = InputQueue::default();
    let mut exit_armed = false;
    loop {
        status.queued = queue.len();
        paint(
            &mut renderer,
            &editor,
            &status,
            &mut super::markdown::Markdown::new(),
            &mut false,
            false,
            mode,
        )?;
        let pending = if let Some(prompt) = queue.pop_front() {
            remove_queued_prompt(&mut status, &prompt);
            status.queued = queue.len();
            Action::Submit(prompt)
        } else {
            let event = match initial.take() {
                Some(event) => event,
                None => loop {
                    tokio::select! {
                        event = events.next() => match event { Some(event) => break event?, None => return Ok(()) },
                        _ = tokio::time::sleep(Duration::from_millis(100)) => renderer.copy_selection_if_due()?,
                    }
                },
            };
            action(event, &mut editor, &mut renderer)
        };
        match pending {
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
                if let Some(command) = prompt.strip_prefix('/') {
                    let (name, args) = command
                        .split_once(char::is_whitespace)
                        .unwrap_or((command, ""));
                    let is_builtin = crate::extensibility::BUILTIN_COMMANDS
                        .iter()
                        .any(|c| c.name == name)
                        || name == "new";
                    if !is_builtin {
                        let home = std::env::var_os("HOME")
                            .map(std::path::PathBuf::from)
                            .ok_or_else(|| io::Error::other("HOME is required"))?;
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
                        permission,
                        branch,
                        ..Status::default()
                    };
                    messages.clear();
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
                        let home = std::env::var_os("HOME")
                            .map(std::path::PathBuf::from)
                            .ok_or_else(|| io::Error::other("HOME is required"))?;
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
                                status.toast = Some(format!("Тема не сохранена: {error}"))
                            }
                        }
                    }
                    continue;
                }
                if prompt.trim() == "/model" {
                    if let Some(model) =
                        super::model_picker::select(&mut renderer, &mut events, &agent.model)
                            .await?
                    {
                        agent.model = model.clone();
                        status.model = model;
                    }
                    continue;
                }
                if prompt.trim() == "/usage" {
                    let home = std::env::var_os("HOME")
                        .map(std::path::PathBuf::from)
                        .ok_or_else(|| io::Error::other("HOME is required"))?;
                    let entries = tokio::task::spawn_blocking(move || {
                        crate::usage::UsageLedger::open(&home)?.entries(None)
                    })
                    .await
                    .map_err(io::Error::other)??;
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
                        renderer.paint(&Frame {
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
                if let Some(card) = commands::execute_command(
                    &prompt,
                    agent,
                    &mut messages,
                    &mut status,
                    card_width,
                )
                .await?
                {
                    renderer.finalize_card(&card)?;
                    continue;
                }
                renderer.finalize_styled(&format_user_turn(
                    &prompt,
                    renderer.size().0,
                    &Theme::default(),
                    mode,
                ))?;
                let mut request = messages.clone();
                request.push(Message::text(Role::User, prompt));
                let (sender, mut progress) = mpsc::channel(256);
                let started = Instant::now();
                let mut stream = super::markdown::Markdown::new();
                let mut stream_started = false;
                let mut tool_running = false;
                let mut response_chunk = String::new();
                let mut committed_response_text = false;
                let mut dirty = true;
                let base = status.clone();
                status.busy = true;
                status.subagents.clear();
                status.elapsed_secs = 0;
                status.elapsed_millis = 0;
                status.retry_notice = None;
                let mut live_usage = LiveUsage::default();
                let mut retry_deadline: Option<(Instant, u32, u32)> = None;
                let mut pending_permission = None;
                let permission_mode = agent.tools.permissions.mode;
                let mut frames = frame_clock();
                let checkpoint = crate::agent::TurnCheckpoint::new(request.clone());
                let mut recovery = None;
                let mut run = Box::pin(agent.run_with_steering(
                    request,
                    Some(&sender),
                    Some(&checkpoint),
                    Some(&queue),
                ));
                loop {
                    tokio::select! {
                        result = &mut run => {
                            finish_turn_status(&mut status, result.is_ok());
                            match result {
                                Ok(outcome) => {
                                    status.input = base.input + outcome.usage.input_tokens;
                                    status.output = base.output + outcome.usage.output_tokens;
                                    status.turn_output = outcome.usage.output_tokens;
                                    status.output_estimated = outcome.usage.estimated;
                                    status.cache_read = base.cache_read + outcome.usage.cache_read_tokens;
                                    status.cache_write = base.cache_write + outcome.usage.cache_write_tokens;
                                    status.tokens_per_second = Some(outcome.usage.output_tokens as f64 / started.elapsed().as_secs_f64().max(0.001));
                                    if let Some((store, session)) = &persistence {
                                        let store = store.clone();
                                        let id = session.id.clone();
                                        let appended = outcome.messages[messages.len()..].to_vec();
                                        let requests = outcome.request_usage.clone();
                                        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).ok_or_else(|| io::Error::other("HOME is required"))?;
                                        let project = session.project.clone();
                                        let provider = options.provider.to_owned();
                                        let model = options.model.to_owned();
                                        tokio::task::spawn_blocking(move || {
                                            crate::usage::record_turn(&home, &id, &project, &provider, &model, &requests)?;
                                            for message in appended { store.append(&id, &message)?; }
                                            Ok::<_, io::Error>(())
                                        }).await.map_err(io::Error::other)??;
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
                                    let err_card = widgets::card(
                                        "Ошибка",
                                        &[
                                            format!("Причина: {error}"),
                                            "Совет: проверь соединение или выполни /doctor.".into(),
                                        ],
                                        "/doctor диагностика",
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
                                Some(Progress::TextDelta { text }) => {
                                    retry_deadline = None;
                                    status.retry_notice = None;
                                    response_chunk.push_str(&text);
                                    stream.push(&text);
                                    live_usage.add_text(&text);
                                    live_usage.apply(&mut status, &base, started.elapsed());
                                }
                                Some(Progress::ThinkingDelta { text }) => {
                                    live_usage.add_text(&text);
                                    live_usage.apply(&mut status, &base, started.elapsed());
                                }
                                Some(Progress::RetryNotice { attempt, max_attempts, delay_ms }) => {
                                    retry_deadline = Some((Instant::now() + Duration::from_millis(delay_ms), attempt, max_attempts));
                                }
                                Some(Progress::ToolStarted { name, input, .. }) => {
                                    tool_running = true;
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
                                    tool_running = false;
                                    let width = renderer.size().0.saturating_sub(1).max(20);
                                    let mut rows = if name == "Agent" && !is_error {
                                        super::tool_card::completed_agent(&output, renderer.size().0.saturating_sub(1).max(1), &Theme::default(), mode)
                                    } else {
                                        let rows = super::tool_card::completed(&name, &output, is_error, duration_ms, width);
                                        style_tool_rows(&rows, false, is_error || output.contains("[exit code:") || output.contains("[command timed out]"), &Theme::default(), mode)
                                    };
                                    rows.push(String::new());
                                    renderer.finalize_styled(&rows)?;
                                    status.toast = None;
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
                        event = events.next() => {
                            let Some(event) = event else { return Ok(()); };
                            match action(event?, &mut editor, &mut renderer) {
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
                                    if tool_running { renderer.finalize("")?; }
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
                                    status.queued_prompts.push(prompt.clone());
                                    queue.push_back(prompt);
                                    status.queued = queue.len();
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
                                    let current = pending_permission
                                        .unwrap_or(permission_mode);
                                    let next = commands::next_permission_mode(current);
                                    pending_permission = Some(next);
                                    status.permission =
                                        commands::permission_label(next).into();
                                    status.toast = None;
                                }
                                Action::Rewind | Action::Complete => {
                                    status.toast = Some("Заверши или отмени активный ход".into());
                                }
                                _ => {},
                            }
                            dirty = true;
                        }
                        _ = frames.tick(), if dirty || status.busy => {
                            renderer.copy_selection_if_due()?;
                            status.elapsed_secs = started.elapsed().as_secs();
                            status.elapsed_millis = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                            if let Some((deadline, attempt, max_attempts)) = retry_deadline {
                                let seconds = deadline.saturating_duration_since(Instant::now()).as_millis().div_ceil(1000);
                                status.retry_notice = Some(format!("Повтор через {seconds}с (попытка {attempt} из {max_attempts})"));
                            }
                            paint(&mut renderer, &editor, &status, &mut stream, &mut stream_started, tool_running, mode)?;
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
                        let appended = recovered[messages.len()..].to_vec();
                        tokio::task::spawn_blocking(move || {
                            for message in appended {
                                store.append(&id, &message)?;
                            }
                            Ok::<_, io::Error>(())
                        })
                        .await
                        .map_err(io::Error::other)??;
                    }
                    messages = recovered;
                }
                if let Some(permission) = pending_permission {
                    agent.tools.permissions.mode = permission;
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
            }
            Action::None => {}
            _ => exit_armed = false,
        }
    }
}
