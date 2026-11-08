//! Minimal interactive orchestration with native scrollback and cancellable turns.

use super::{
    header::Header,
    input::{Action, Editor},
    intro::{self, IntroEnd},
    render::{Frame, Renderer},
    statusbar::Status,
    term::TerminalGuard,
};
use crate::{
    agent::Agent,
    provider::canonical::{Event as Progress, Message, Role, Usage},
    theme::{self, Theme},
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
    let mut frame = Frame::input(editor, status, width, height.min(8));
    let room = height.saturating_sub(frame.lines.len()).min(12);
    let rendered = stream.render(width, &Theme::default(), crate::theme::ColorMode::None);
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
fn unsupported(renderer: &mut Renderer<io::Stdout>) -> io::Result<()> {
    renderer.finalize("Не поддерживается в текущей версии терминального интерфейса.")
}
fn local(prompt: &str, renderer: &mut Renderer<io::Stdout>) -> io::Result<bool> {
    if prompt.starts_with(['/', '!', '#']) {
        unsupported(renderer)?;
        Ok(true)
    } else {
        Ok(false)
    }
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
            Action::Submit(prompt) => {
                exit_armed = false;
                if prompt.trim() == "/exit" {
                    return Ok(());
                }
                if local(&prompt, &mut renderer)? {
                    continue;
                }
                renderer.finalize(&format!("› {prompt}"))?;
                let mut request = messages.clone();
                request.push(Message::text(Role::User, prompt));
                let (sender, mut progress) = mpsc::channel(256);
                let started = Instant::now();
                let mut stream = super::markdown::Markdown::new();
                let mut dirty = true;
                let base = status.clone();
                let mut live_usage = LiveUsage::default();
                let mut frames = frame_clock();
                let mut run = Box::pin(agent.run(request, Some(&sender)));
                loop {
                    tokio::select! {
                        result = &mut run => {
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
                                        tokio::task::spawn_blocking(move || {
                                            for message in appended { store.append(&id, &message)?; }
                                            Ok::<_, io::Error>(())
                                        }).await.map_err(io::Error::other)??;
                                    }
                                    messages = outcome.messages;
                                    let mut completed = super::markdown::Markdown::new();
                                    completed.push(&outcome.text);
                                    completed.finish();
                                    renderer.finalize_styled(&completed.render(renderer.size().0, &Theme::default(), mode))?;
                                }
                                Err(error) => renderer.finalize(&format!("Ошибка: {error}"))?,
                            }
                            break;
                        }
                        event = progress.recv() => {
                            match event {
                                Some(Progress::TextDelta { text }) => stream.push(&text),
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
                                Action::Interrupt => { renderer.finalize("Прервано. Уже выполненные операции не отменены.")?; break; }
                                Action::Submit(prompt) => { queue.push_back(prompt); }
                                Action::Rewind | Action::Transcript | Action::Todos | Action::CyclePermission | Action::Complete | Action::SearchHistory => unsupported(&mut renderer)?,
                                _ => {},
                            }
                            dirty = true;
                        }
                        _ = frames.tick(), if dirty => {
                            paint(&mut renderer, &editor, &status, &stream)?;
                            dirty = false;
                        }
                    }
                }
            }
            Action::Rewind
            | Action::Transcript
            | Action::Todos
            | Action::CyclePermission
            | Action::Complete
            | Action::SearchHistory => unsupported(&mut renderer)?,
            Action::None => {}
            _ => exit_armed = false,
        }
    }
}
