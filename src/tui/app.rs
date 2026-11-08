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
    agent::Agent,
    provider::canonical::{Event as Progress, Message, Role, Usage},
    theme::{self, ColorMode, Theme},
};
use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use std::{
    collections::VecDeque,
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
}

impl LiveUsage {
    fn snapshot(&mut self, usage: Usage) {
        self.current = usage;
    }

    // Done is the explicit validated request boundary, not the end of the turn.
    fn finish_request(&mut self) {
        self.completed = self.total();
        self.current = Usage::default();
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
        status.input = base.input.saturating_add(total.input_tokens);
        status.output = base.output.saturating_add(total.output_tokens);
        status.cache_read = base.cache_read.saturating_add(total.cache_read_tokens);
        status.cache_write = base.cache_write.saturating_add(total.cache_write_tokens);
        status.tokens_per_second =
            Some(total.output_tokens as f64 / elapsed.as_secs_f64().max(0.001));
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

fn paint(
    renderer: &mut Renderer<io::Stdout>,
    editor: &Editor,
    status: &Status,
    stream: &super::markdown::Markdown,
) -> io::Result<()> {
    let (width, height) = renderer.size();
    let mut frame = Frame::input(editor, status, width.saturating_sub(1), height);
    let room = height.saturating_sub(frame.lines.len()).min(12);
    let rendered = stream.render(width, &Theme::default(), ColorMode::None);
    let rows = rendered.iter().rev().take(room).collect::<Vec<_>>();
    let count = rows.len();
    frame
        .lines
        .splice(0..0, rows.into_iter().rev().map(|s| super::clip(s, width)));
    if let Some((row, _)) = &mut frame.cursor {
        *row += count;
    }
    renderer.paint(&frame)
}

fn action(event: Event, editor: &mut Editor, renderer: &mut Renderer<io::Stdout>) -> Action {
    if let Event::Resize(w, h) = event {
        renderer.resize(w.into(), h.into());
    }
    editor.handle(event)
}

fn format_user_turn(prompt: &str, width: usize, theme: &Theme, mode: ColorMode) -> Vec<String> {
    let width = width.saturating_sub(1).max(20);
    let mut rows = Vec::new();
    for (idx, line) in super::plain(prompt).lines().enumerate() {
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

fn format_assistant_turn(text: &str, width: usize, theme: &Theme, mode: ColorMode) -> Vec<String> {
    let mut completed = super::markdown::Markdown::new();
    let glyphs = widgets::Glyphs::detect();
    let mark = if glyphs.ascii { "*" } else { "✿" };
    completed.push(&format!("{mark} {text}"));
    completed.finish();
    let mut rows = completed.render(width.min(100), theme, mode);
    rows.push(String::new());
    rows
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
            let card = commands::cycle_permission_card(agent, status, width);
            renderer.finalize_styled(&card)?;
        }
        Action::Rewind => {
            if let Some(card) =
                commands::execute_command("/rewind", agent, messages, status, width).await?
            {
                renderer.finalize_styled(&card)?;
            }
        }
        Action::Transcript => {
            let card = commands::transcript_card(messages, width);
            renderer.finalize_styled(&card)?;
        }
        Action::Todos => {
            let card = commands::todos_card(messages, width);
            renderer.finalize_styled(&card)?;
        }
        Action::SearchHistory => {
            let card = commands::history_card(editor.history(), editor.text(), width);
            renderer.finalize_styled(&card)?;
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
/// Completed turns are persisted when a session is supplied. Failed or interrupted
/// turns retain the last completed history rather than inventing tool results.
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
    renderer.finalize_styled(&header.lines(
        usize::from(width).saturating_sub(1),
        &Theme::default(),
        mode,
    ))?;
    let mut status = Status {
        model: options.model.into(),
        permission: options.permission.into(),
        branch: options.branch.into(),
        ..Status::default()
    };
    let persistence = options.session;
    let mut messages = persistence
        .as_ref()
        .map(|(_, session)| session.messages.clone())
        .unwrap_or_default();
    let mut queue = VecDeque::<String>::new();
    let mut exit_armed = false;
    loop {
        status.queued = queue.len();
        paint(
            &mut renderer,
            &editor,
            &status,
            &super::markdown::Markdown::new(),
        )?;
        let pending = if let Some(prompt) = queue.pop_front() {
            Action::Submit(prompt)
        } else {
            let event = match initial.take() {
                Some(event) => event,
                None => match events.next().await {
                    Some(event) => event?,
                    None => return Ok(()),
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
                    return Ok(());
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
                    let home = crate::platform::home_dir()
                        .ok_or_else(|| io::Error::other("home directory is required"))?;
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
                    renderer.finalize_styled(&card)?;
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
                let mut dirty = true;
                let base = status.clone();
                status.busy = true;
                status.elapsed_secs = 0;
                status.retry_notice = None;
                let mut live_usage = LiveUsage::default();
                let mut retry_deadline: Option<(Instant, u32, u32)> = None;
                let mut toast_deadline: Option<Instant> = None;
                let mut frames = frame_clock();
                let mut run = Box::pin(agent.run(request, Some(&sender)));
                loop {
                    tokio::select! {
                        result = &mut run => {
                            status.busy = false;
                            status.retry_notice = None;
                            match result {
                                Ok(outcome) => {
                                    status.input = base.input + outcome.usage.input_tokens;
                                    status.output = base.output + outcome.usage.output_tokens;
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
                                        let model = options.model.to_owned();
                                        tokio::task::spawn_blocking(move || {
                                            crate::usage::record_turn(&home, &id, &project, &provider, &model, &requests)?;
                                            for message in appended { store.append(&id, &message)?; }
                                            Ok::<_, io::Error>(())
                                        }).await.map_err(io::Error::other)??;
                                    }
                                    messages = outcome.messages;
                                    renderer.finalize_styled(&format_assistant_turn(&outcome.text, renderer.size().0, &Theme::default(), mode))?;
                                }
                                Err(error) => {
                                    let err_card = widgets::card(
                                        "Ошибка",
                                        &[
                                            format!("Причина: {error}"),
                                            "Совет: проверь соединение или выполни /doctor.".into(),
                                        ],
                                        "/doctor диагностика",
                                        renderer.size().0.saturating_sub(1),
                                    );
                                    renderer.finalize_styled(&err_card)?;
                                }
                            }
                            break;
                        }
                        event = progress.recv() => {
                            status.elapsed_secs = started.elapsed().as_secs();
                            match event {
                                Some(Progress::TextDelta { text }) => {
                                    retry_deadline = None;
                                    status.retry_notice = None;
                                    stream.push(&text);
                                }
                                Some(Progress::RetryNotice { attempt, max_attempts, delay_ms }) => {
                                    retry_deadline = Some((Instant::now() + Duration::from_millis(delay_ms), attempt, max_attempts));
                                }
                                Some(Progress::ToolStarted { name, .. }) => {
                                    retry_deadline = None;
                                    status.retry_notice = None;
                                    status.toast = Some(format!("Выполняется: {}", super::plain(&name)));
                                    toast_deadline = Some(Instant::now() + Duration::from_secs(2));
                                }
                                Some(Progress::ToolFinished { name, output, is_error, duration_ms, .. }) => {
                                    let card = super::tool_card::completed(&name, &output, is_error, duration_ms, renderer.size().0.saturating_sub(1));
                                    renderer.finalize_styled(&card)?;
                                    status.toast = None;
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
                                    renderer.finalize("Прервано. Уже выполненные операции не отменены.")?;
                                    break;
                                }
                                Action::Submit(prompt) => {
                                    queue.push_back(prompt);
                                    status.queued = queue.len();
                                }
                                Action::Transcript => {
                                    let w = renderer.size().0.saturating_sub(1).max(20);
                                    renderer.finalize_styled(&commands::transcript_card(&messages, w))?;
                                }
                                Action::Todos => {
                                    let w = renderer.size().0.saturating_sub(1).max(20);
                                    renderer.finalize_styled(&commands::todos_card(&messages, w))?;
                                }
                                Action::SearchHistory => {
                                    let w = renderer.size().0.saturating_sub(1).max(20);
                                    renderer.finalize_styled(&commands::history_card(editor.history(), editor.text(), w))?;
                                }
                                Action::CyclePermission | Action::Rewind | Action::Complete => {
                                    status.toast = Some("Заверши или отмени [esc] активный ход".into());
                                }
                                _ => {},
                            }
                            dirty = true;
                        }
                        _ = frames.tick(), if dirty || status.busy => {
                            status.elapsed_secs = started.elapsed().as_secs();
                            if let Some((deadline, attempt, max_attempts)) = retry_deadline {
                                let seconds = deadline.saturating_duration_since(Instant::now()).as_millis().div_ceil(1000);
                                status.retry_notice = Some(format!("Повтор через {seconds}с (попытка {attempt} из {max_attempts})"));
                            }
                            if toast_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                                status.toast = None;
                                toast_deadline = None;
                            }
                            paint(&mut renderer, &editor, &status, &stream)?;
                            dirty = false;
                        }
                    }
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
