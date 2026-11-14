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

fn apply_model_effort(agent: &mut Agent, status: &mut Status) {
    let _ = agent.client.set_effort(Some("xhigh"));
    if let Some(home) = std::env::var_os("HOME") {
        if let Err(error) = crate::effort_preferences::apply(
            &mut agent.client,
            std::path::Path::new(&home),
            &agent.model,
        ) {
            status.toast = Some(format!("Не удалось загрузить effort: {error}"));
        }
    }
    status.effort = agent.client.effort().map(str::to_owned);
}

fn save_effort(agent: &Agent, status: &mut Status) {
    let result = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::other("HOME не задан"))
        .and_then(|home| {
            crate::effort_preferences::set(
                std::path::Path::new(&home),
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
                    calls.insert(id.as_str(), name);
                    let started =
                        super::tool_card::started(name, input, width.saturating_sub(1).max(20));
                    rows.extend(style_tool_rows(&started, true, false, theme, mode));
                }
                Content::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let name = calls.remove(tool_use_id.as_str()).unwrap_or("Tool");
                    rows.extend(completed_tool_rows(
                        name, content, *is_error, 0, width, theme, mode,
                    ));
                }
                // Live thinking is accounting-only; opaque provider state is never transcript text.
                Content::Thinking { .. } | Content::Image { .. } | Content::Document { .. } => {}
            }
        }
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
    fn replay_resolves_multiple_calls_by_id_and_keeps_block_order() {
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
            "Read(file.rs)",
            "Bash(false)",
            "Error: denied",
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
            if commands::next_permission_mode(agent.tools.permissions.mode)
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
    let header = Header {
        version: env!("CARGO_PKG_VERSION"),
        model: options.model,
        provider: options.provider,
        cwd: options.cwd,
        branch: options.branch,
    };
    let mut status = Status {
        model: options.model.into(),
        effort: agent.client.effort().map(str::to_owned),
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
    renderer.finalize_styled(&replay_rows(
        &messages,
        renderer.size().0,
        &Theme::default(),
        mode,
    ))?;
    let queue = InputQueue::default();
    let background_handles = agent.tools.background_handles();
    let mut exit_armed = false;
    let mut copy_deadline = None;
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
        let pending = if !status.bypass_confirmation && !queue.is_empty() {
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
                            agent.tools.permissions.mode = crate::perm::Mode::Bypass;
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
        match pending {
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
                        match tokio::task::spawn_blocking(move || list_store.list())
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
                                let mut body = vec![
                                    format!("Открыта сессия {id} · сообщений: {count}"),
                                    "Новые сообщения будут сохранены в выбранный чат.".into(),
                                ];
                                if interrupted {
                                    body.push("Незавершённый хвост журнала пропущен.".into());
                                }
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
                    continue;
                }
                renderer.finalize_styled(&format_user_turn(
                    &prompt,
                    renderer.size().0,
                    &Theme::default(),
                    mode,
                ))?;
                let mut request = messages.clone();
                let mut user_message = Message::text(Role::User, prompt);
                if let Some(image) = submitted_image {
                    user_message.content.push(image);
                }
                request.push(user_message);
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
                let mut retry_deadline: Option<(Instant, u32, u32)> = None;
                let mut pending_permission = None;
                let permission_mode = agent.tools.permissions.mode;
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
                                        let appended = outcome.messages[messages.len()..].to_vec();
                                        let requests = outcome.request_usage.clone();
                                        let home = crate::platform::home_dir().ok_or_else(|| io::Error::other("home directory is required"))?;
                                        let project = session.project.clone();
                                        let provider = options.provider.to_owned();
                                        let model = ledger_model.clone();
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
                                Some(Progress::CompactionStarted) => status.compacting = true,
                                Some(Progress::CompactionFinished) => status.compacting = false,
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
                                    running_tools = running_tools.saturating_sub(1);
                                    let rows = completed_tool_rows(&name, &output, is_error, duration_ms, renderer.size().0, &Theme::default(), mode);
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
                                        pending_permission = Some(crate::perm::Mode::Bypass);
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
                                    status.queued_prompts.push(prompt.clone());
                                    queue.push_with_image(prompt, editor.take_image());
                                    status.queued = queue.len();
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
                                    let current = pending_permission
                                        .unwrap_or(permission_mode);
                                    let next = commands::next_permission_mode(current);
                                    if next == crate::perm::Mode::Bypass {
                                        status.bypass_confirmation = true;
                                        status.bypass_selected_yes = false;
                                    } else {
                                        pending_permission = Some(next);
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
                            status.elapsed_secs = started.elapsed().as_secs();
                            status.elapsed_millis = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                            if let Some((deadline, attempt, max_attempts)) = retry_deadline {
                                let seconds = deadline.saturating_duration_since(Instant::now()).as_millis().div_ceil(1000);
                                status.retry_notice = Some(format!("Повтор через {seconds}с (попытка {attempt} из {max_attempts})"));
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
                if save_compaction_checkpoint(agent, &persistence, &messages)
                    .await
                    .is_err()
                {
                    status.toast = Some(
                        "Не удалось сохранить сжатый контекст; полная история сохранена".into(),
                    );
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
