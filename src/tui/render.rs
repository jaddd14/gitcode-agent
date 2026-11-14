//! Native-scrollback output with a bounded, synchronized live area.

use super::header::{LOGO_COLUMNS, LOGO_ROWS};
use super::{clip, input::Editor, statusbar::Status};
use crate::theme::{ColorMode, Theme};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::io::{self, BufWriter, Write};

const KITTY_LOGO_ID: u32 = 7_431;
const KITTY_CHUNK_SIZE: usize = 4_096;

#[derive(Clone, Copy)]
struct InlineLogo<'a> {
    png: &'a [u8],
    row: usize,
    column: usize,
}

#[derive(Clone, Copy)]
struct InlinePanel {
    row: usize,
    height: usize,
}

// A gentle fill-and-empty cycle gives the running state intermediate frames.
fn subagent_spinner(elapsed_ms: u64, ascii: bool) -> &'static str {
    const CIRCLE: [&str; 8] = ["○", "◔", "◑", "◕", "●", "◕", "◑", "◔"];
    const ASCII: [&str; 8] = ["o", "o", "O", "O", "@", "O", "O", "o"];
    let frame = ((elapsed_ms / 350) % CIRCLE.len() as u64) as usize;
    if ascii {
        ASCII[frame]
    } else {
        CIRCLE[frame]
    }
}

pub(super) fn elapsed_duration(mut seconds: u64) -> String {
    const UNITS: [(u64, &str); 5] = [
        (365 * 24 * 60 * 60, "г"),
        (7 * 24 * 60 * 60, "нед"),
        (24 * 60 * 60, "д"),
        (60 * 60, "ч"),
        (60, "м"),
    ];
    let mut parts = Vec::new();
    for (size, suffix) in UNITS {
        let value = seconds / size;
        if value > 0 {
            parts.push(format!("{value}{suffix}"));
            seconds %= size;
        }
    }
    parts.push(format!("{seconds}с"));
    parts.join(" ")
}
use super::capabilities::Capabilities;
use crossterm::{
    terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate},
    QueueableCommand,
};

/// Cursor-aware projection ready to paint without provider or filesystem work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Visible logical rows, each constrained to the terminal width.
    pub lines: Vec<String>,
    /// Optional input cursor location, row then column.
    pub cursor: Option<(usize, usize)>,
}
impl Frame {
    /// Build a live input and status area, scrolling tall prompts around the cursor.
    pub fn input(editor: &Editor, status: &Status, width: usize, height: usize) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        if width < 8 || height < 5 {
            return Self {
                lines: vec![status.line(width)],
                cursor: None,
            };
        }
        let glyphs = super::widgets::Glyphs::detect();
        let view = editor.view(width.saturating_sub(4).max(1));
        let activity_rows = usize::from(status.busy)
            + usize::from(status.retry_notice.is_some())
            + usize::from(status.busy && status.retry_notice.is_some())
            + usize::from(status.copied_chars.is_some())
            + usize::from(
                status.busy || status.retry_notice.is_some() || status.copied_chars.is_some(),
            );
        let agent_rows = status
            .subagents
            .len()
            .min(3)
            .min(height.saturating_sub(8 + activity_rows));
        let capacity = height
            .saturating_sub(4 + activity_rows + agent_rows)
            .clamp(1, 12);
        let start = (view.cursor_row + 1).saturating_sub(capacity);
        let body = view
            .lines
            .iter()
            .skip(start)
            .take(capacity)
            .map(|text| {
                if text.is_empty() && view.lines.len() == 1 {
                    "Спроси, или / для команд".to_owned()
                } else {
                    text.clone()
                }
            })
            .collect::<Vec<_>>();
        let footer = format!("{} · shift+tab", status.permission);
        let mut lines = super::widgets::Frame {
            title: "",
            footer: &footer,
            glyphs,
        }
        .render(&body, width);
        for (index, row) in lines.iter_mut().skip(1).take(body.len()).enumerate() {
            let content = body.get(index).map(String::as_str).unwrap_or("");
            let prefix = if index == 0 {
                " › "
            } else if glyphs.ascii {
                "|  "
            } else {
                "│  "
            };
            let edge = if glyphs.ascii { '|' } else { '│' };
            *row = format!(
                "{prefix}{}{edge}",
                super::widgets::fit(content, width.saturating_sub(4))
            );
        }
        let mut cursor = Some((
            view.cursor_row - start + 1,
            (view.cursor_col + 3).min(width - 2),
        ));
        let mut above = Vec::new();
        if let Some(notice) = &status.retry_notice {
            let prefix = if notice.starts_with('▲') {
                ""
            } else {
                "▲ "
            };
            above.push(clip(&format!("{prefix}{notice}"), width));
            if status.busy {
                above.push(String::new());
            }
        }
        if status.busy {
            let verb = if status.compacting {
                "Сжимаю контекст"
            } else {
                "Думаю"
            };
            let mark = if glyphs.ascii { '*' } else { '✻' };
            above.push(clip(
                &format!(
                    " {mark} {verb}… ({} · ↓ {}{} токенов)",
                    elapsed_duration(status.elapsed_secs),
                    if status.output_estimated && status.turn_output > 0 {
                        "~"
                    } else {
                        ""
                    },
                    super::statusbar::compact(status.turn_output)
                ),
                width,
            ));
        }
        if let Some(count) = status.copied_chars {
            let word = match (count % 100, count % 10) {
                (11..=14, _) => "символов",
                (_, 1) => "символ",
                (_, 2..=4) => "символа",
                _ => "символов",
            };
            above.push(clip(
                &format!(
                    "  {} Скопировано · {count} {word}",
                    if glyphs.ascii { "+" } else { "✓" }
                ),
                width,
            ));
        }
        if !above.is_empty() {
            above.push(String::new());
            if let Some((row, _)) = &mut cursor {
                *row += above.len();
            }
            lines.splice(0..0, above);
        }
        let hint_line = if status.busy {
            String::new()
        } else if let Some(toast) = &status.toast {
            clip(toast, width)
        } else {
            String::new()
        };
        if !hint_line.is_empty() {
            lines.push(hint_line);
        }
        lines.push(status.line(width));
        lines.push(super::widgets::divider(width, glyphs));
        for run in status.subagents.iter().rev().take(agent_rows).rev() {
            let (mark, state) = match run.state {
                crate::agent::agency::SubagentRunState::Running => (
                    subagent_spinner(status.elapsed_millis, glyphs.ascii),
                    run.current_tool.as_deref().unwrap_or("Думает"),
                ),
                crate::agent::agency::SubagentRunState::Completed => {
                    (if glyphs.ascii { "+" } else { "✓" }, "Готово")
                }
                crate::agent::agency::SubagentRunState::Failed => {
                    (if glyphs.ascii { "x" } else { "×" }, "Ошибка")
                }
            };
            lines.push(clip(
                &format!(
                    "  {mark} {} · {} · {state} · {}↓",
                    super::plain(&run.name),
                    super::plain(&run.task),
                    super::statusbar::compact(run.tokens)
                )
                .replace('\n', " "),
                width,
            ));
        }
        if editor.completion_open() {
            let room = height.saturating_sub(lines.len() + 4).min(9);
            if room > 0 {
                let popup = super::commands::popup(
                    editor.text(),
                    editor.completion_selected(),
                    width,
                    room,
                );
                if let Some((row, _)) = &mut cursor {
                    *row += popup.len();
                }
                lines.splice(0..0, popup);
            }
        }
        Self { lines, cursor }
    }
}

/// Incremental painter that never enters alternate screen or captures mouse input.
pub struct Renderer<W: Write> {
    writer: BufWriter<W>,
    viewport: Option<super::viewport::Viewport>,
    in_sync: bool,
    live_rows: usize,
    live_lines: Vec<String>,
    live_cursor: Option<(usize, usize)>,
    live_valid: bool,
    live_bottom: bool,
    reserve_bottom: bool,
    remaining_bottom: Option<usize>,
    force_redraw: bool,
    cursor_saved: bool,
    size: (usize, usize),
    theme: Theme,
    mode: ColorMode,
    activity_millis: u64,
    interactive: bool,
    kitty_graphics: bool,
    kitty_logo_loaded: bool,
    kitty_logo_position: Option<(usize, usize)>,
    capabilities: Capabilities,
}
impl<W: Write> Renderer<W> {
    /// Create an output sink with historical synchronized byte output.
    /// Noninteractive mode writes finalized text only. Runtime callers should use
    /// `with_capabilities` with detected policy instead of assuming support.
    pub fn new(writer: W, width: usize, height: usize, mode: ColorMode, interactive: bool) -> Self {
        Self::with_capabilities(
            writer,
            width,
            height,
            mode,
            interactive,
            Capabilities {
                synchronized_output: true,
                ..Capabilities::default()
            },
        )
    }
    /// Create an output sink with explicit, deterministic extension policy.
    pub fn with_capabilities(
        writer: W,
        width: usize,
        height: usize,
        mode: ColorMode,
        interactive: bool,
        capabilities: Capabilities,
    ) -> Self {
        Self {
            writer: BufWriter::with_capacity(64 * 1024, writer),
            viewport: None,
            in_sync: false,
            live_rows: 0,
            live_lines: Vec::new(),
            live_cursor: None,
            live_valid: false,
            live_bottom: false,
            reserve_bottom: false,
            remaining_bottom: None,
            force_redraw: false,
            cursor_saved: false,
            size: (width.max(1), height.max(1)),
            theme: Theme::default(),
            mode,
            activity_millis: 0,
            interactive,
            kitty_graphics: interactive && kitty_graphics_available(),
            kitty_logo_loaded: false,
            kitty_logo_position: None,
            capabilities,
        }
    }
    /// Use a fixed composer with mouse scrolling and selection.
    pub fn enable_viewport(&mut self) -> io::Result<()> {
        self.viewport = Some(super::viewport::Viewport::default());
        self.writer
            .write_all(b"\x1b[2J\x1b[H\x1b[?1002h\x1b[?1006h")?;
        self.writer.flush()
    }
    /// Apply a mouse gesture to the conversation viewport.
    pub fn mouse(&mut self, event: crossterm::event::MouseEvent) {
        if let Some(viewport) = &mut self.viewport {
            viewport.mouse(event);
        }
    }
    /// Clear stale mouse highlighting when keyboard input resumes.
    pub fn clear_selection(&mut self) {
        if let Some(viewport) = &mut self.viewport {
            viewport.clear_selection();
        }
    }
    /// Whether the viewport has another scrolling animation frame.
    pub fn scroll_pending(&self) -> bool {
        self.viewport
            .as_ref()
            .is_some_and(super::viewport::Viewport::scroll_pending)
    }
    /// Copy a settled selection to terminals supporting OSC 52.
    pub fn copy_selection_if_due(&mut self) -> io::Result<Option<usize>> {
        if let Some(viewport) = &mut self.viewport {
            return viewport.copy_if_due(&mut self.writer);
        }
        Ok(None)
    }
    /// Update geometry; the next event-driven paint reflows its frame.
    pub fn resize(&mut self, width: usize, height: usize) {
        self.size = (width.max(1), height.max(1));
        self.force_redraw = true;
        if let Some(v) = &mut self.viewport {
            v.invalidate();
        }
    }
    /// Apply a selected palette to subsequent live and finalized output.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }
    /// Current terminal width and height.
    pub fn size(&self) -> (usize, usize) {
        self.size
    }
    /// Color capability selected for this renderer.
    pub fn color_mode(&self) -> ColorMode {
        self.mode
    }
    fn clear_live(&mut self) -> io::Result<()> {
        self.live_bottom = false;
        if self.cursor_saved {
            self.writer.write_all(b"\x1b8")?;
            self.cursor_saved = false;
        }
        if self.live_rows > 0 {
            write!(self.writer, "\r\x1b[{}A", self.live_rows)?;
            for row in 0..self.live_rows {
                self.writer.write_all(b"\x1b[2K")?;
                if row + 1 < self.live_rows {
                    self.writer.write_all(b"\x1b[1B\r")?;
                }
            }
            if self.live_rows > 1 {
                write!(self.writer, "\x1b[{}A", self.live_rows - 1)?;
            }
            self.live_rows = 0;
        }
        self.live_lines.clear();
        self.live_cursor = None;
        self.live_valid = false;
        Ok(())
    }
    fn sync(&mut self, body: impl FnOnce(&mut Self) -> io::Result<()>) -> io::Result<()> {
        if self.in_sync {
            return body(self);
        }
        if !self.capabilities.synchronized_output {
            let result = body(self);
            return result.and(self.writer.flush());
        }
        self.writer.queue(BeginSynchronizedUpdate)?;
        self.in_sync = true;
        let result = body(self);
        self.in_sync = false;
        // Close even when body fails so a recoverable write never strands sync mode.
        let close = self
            .writer
            .queue(EndSynchronizedUpdate)
            .and_then(|writer| writer.flush());
        result.and(close)
    }
    fn begin_output(&mut self, rows: usize) {
        if self.live_bottom && self.live_rows > 0 {
            self.reserve_bottom = true;
            self.remaining_bottom = Some(self.live_rows);
        }
        if let Some(remaining) = &mut self.remaining_bottom {
            *remaining = remaining.saturating_sub(rows);
        }
    }
    /// Permanently append text to native scrollback and remove the previous live area.
    pub fn finalize(&mut self, text: &str) -> io::Result<()> {
        let text = super::plain(text);
        if let Some(viewport) = &mut self.viewport {
            let rows = text
                .split('\n')
                .map(|line| super::widgets::style_frame_row(line, &self.theme, self.mode))
                .collect::<Vec<_>>();
            viewport.append(&rows);
            return Ok(());
        }
        if !self.interactive {
            writeln!(self.writer, "{text}")?;
            return self.writer.flush();
        }
        self.sync(|this| {
            this.begin_output(text.split('\n').count());
            this.clear_live()?;
            for line in text.split('\n') {
                let line = super::widgets::style_frame_row(line, &this.theme, this.mode);
                write!(this.writer, "{line}\x1b[0m\r\n")?;
            }
            Ok(())
        })
    }
    /// Append pre-sized, trusted theme-generated rows, preserving their color escapes.
    /// Never pass provider or user text to this method.
    pub fn finalize_styled(&mut self, rows: &[String]) -> io::Result<()> {
        if let Some(viewport) = &mut self.viewport {
            viewport.append(rows);
            return Ok(());
        }
        if !self.interactive {
            return self.finalize(&rows.join("\n"));
        }
        self.sync(|this| {
            this.begin_output(rows.len());
            this.clear_live()?;
            for row in rows {
                write!(this.writer, "{row}\x1b[0m\r\n")?;
            }
            Ok(())
        })
    }
    /// Append a command or notification card with a yellow outline.
    pub fn finalize_card(&mut self, rows: &[String]) -> io::Result<()> {
        if !self.interactive {
            return self.finalize(&rows.join("\n"));
        }
        let yellow = crate::theme::foreground(self.theme.warn, self.mode);
        let reset = crate::theme::reset(self.mode);
        let styled = rows
            .iter()
            .map(|row| {
                let row = super::plain(row);
                if row.starts_with(['╭', '╰', '+']) {
                    format!("{yellow}{row}{reset}")
                } else if (row.starts_with('│') && row.ends_with('│'))
                    || (row.starts_with('|') && row.ends_with('|'))
                {
                    let first = row.chars().next().unwrap_or(' ');
                    let last = row.chars().last().unwrap_or(' ');
                    let inner = &row[first.len_utf8()..row.len() - last.len_utf8()];
                    format!("{yellow}{first}{reset}{inner}{yellow}{last}{reset}")
                } else {
                    row
                }
            })
            .collect::<Vec<_>>();
        self.finalize_styled(&styled)
    }
    /// Append an information panel to scrollback so later conversation pushes it upward.
    pub fn finalize_panel(
        &mut self,
        rows: &[String],
        logo_png: &[u8],
        logo_row: usize,
        logo_column: usize,
        panel_height: usize,
    ) -> io::Result<()> {
        if !self.interactive {
            return self.finalize(&rows.join("\n"));
        }
        self.sync(|this| {
            this.begin_output(rows.len());
            this.clear_live()?;
            for (index, row) in rows.iter().enumerate() {
                let text = clip(row, this.size.0.saturating_sub(1).max(1));
                let styled = super::widgets::style_frame_row_with_panel(
                    &text,
                    &this.theme,
                    this.mode,
                    index < panel_height,
                );
                write!(this.writer, "{styled}\x1b[0m\r\n")?;
            }
            this.writer.write_all(b"\x1b7")?;
            this.place_kitty_logo(InlineLogo {
                png: logo_png,
                row: logo_row,
                column: logo_column,
            })?;
            this.writer.write_all(b"\x1b8")?;
            Ok(())
        })
    }
    /// Clear visible rows and scrollback before beginning a fresh conversation.
    pub fn clear_terminal(&mut self) -> io::Result<()> {
        if let Some(viewport) = &mut self.viewport {
            viewport.clear();
            return Ok(());
        }
        self.sync(|this| {
            this.clear_live()?;
            this.clear_kitty_logo()?;
            this.writer
                .write_all(super::term::CLEAR_SCREEN.as_bytes())?;
            this.reserve_bottom = false;
            this.remaining_bottom = None;
            this.writer.flush()
        })
    }
    /// Paint pre-sized, trusted theme-generated rows, preserving color escapes.
    /// Never pass provider or user text to this method.
    pub fn paint_styled(&mut self, frame: &Frame) -> io::Result<()> {
        self.paint_inner(frame, true, 0, None, None, false)
    }
    /// Paint only the mutable tail; callers coalesce stream updates before invoking it.
    pub fn paint(&mut self, frame: &Frame) -> io::Result<()> {
        self.paint_inner(frame, false, 0, None, None, false)
    }
    /// Paint the mutable tail at the bottom of the terminal.
    pub fn paint_bottom(&mut self, frame: &Frame) -> io::Result<()> {
        self.paint_inner(frame, false, 0, None, None, true)
    }
    /// Paint trusted styled response rows above the ordinary bottom composer.
    pub fn paint_bottom_with_styled_prefix(
        &mut self,
        frame: &Frame,
        prefix: usize,
    ) -> io::Result<()> {
        self.paint_inner(frame, false, prefix, None, None, true)
    }
    /// Paint the conversation and composer at the current position in scrollback.
    pub fn paint_flow_with_styled_prefix(
        &mut self,
        frame: &Frame,
        prefix: usize,
    ) -> io::Result<()> {
        self.paint_inner(frame, false, prefix, None, None, false)
    }
    /// Append stable streamed rows and continue the conversation in place.
    pub fn paint_stream_flow(
        &mut self,
        finalized: &[String],
        frame: &Frame,
        prefix: usize,
    ) -> io::Result<()> {
        if let Some(viewport) = &mut self.viewport {
            viewport.append(finalized);
            return self.paint_flow_with_styled_prefix(frame, prefix);
        }
        if !self.interactive {
            return if finalized.is_empty() {
                Ok(())
            } else {
                self.finalize_styled(finalized)
            };
        }
        if finalized.is_empty() {
            return self.paint_flow_with_styled_prefix(frame, prefix);
        }
        if self.live_valid
            && !self.live_bottom
            && !self.force_redraw
            && finalized.len() < self.live_rows
            && finalized
                .iter()
                .zip(&self.live_lines)
                .all(|(new, old)| super::widgets::unstyled(new) == super::widgets::unstyled(old))
        {
            self.live_lines.drain(..finalized.len());
            self.live_rows -= finalized.len();
            self.live_cursor = None;
            return self.paint_flow_with_styled_prefix(frame, prefix);
        }
        self.sync(|this| {
            this.finalize_styled(finalized)?;
            this.paint_flow_with_styled_prefix(frame, prefix)
        })
    }
    /// Append stable streamed rows and repaint their live continuation together.
    pub fn paint_stream(
        &mut self,
        finalized: &[String],
        frame: &Frame,
        prefix: usize,
    ) -> io::Result<()> {
        if !self.interactive {
            return if finalized.is_empty() {
                Ok(())
            } else {
                self.finalize_styled(finalized)
            };
        }
        if finalized.is_empty() {
            return self.paint_bottom_with_styled_prefix(frame, prefix);
        }
        // Already-visible stable rows can become scrollback in place. Keeping
        // the remaining rows on screen avoids erasing the response every time
        // a new line moves out of the live area.
        if self.live_valid
            && self.live_bottom
            && !self.force_redraw
            && !self.reserve_bottom
            && finalized.len() < self.live_rows
            && finalized
                .iter()
                .zip(&self.live_lines)
                .all(|(new, old)| super::widgets::unstyled(new) == super::widgets::unstyled(old))
        {
            self.live_lines.drain(..finalized.len());
            self.live_rows -= finalized.len();
            self.live_cursor = None;
            return self.paint_bottom_with_styled_prefix(frame, prefix);
        }
        self.sync(|this| {
            this.finalize_styled(finalized)?;
            this.paint_bottom_with_styled_prefix(frame, prefix)
        })
    }
    /// Paint the live frame with its tinted information panel and source logo.
    pub fn paint_with_chrome(
        &mut self,
        frame: &Frame,
        logo_png: &[u8],
        logo_row: usize,
        logo_column: usize,
        panel_row: usize,
        panel_height: usize,
    ) -> io::Result<()> {
        self.paint_inner(
            frame,
            false,
            0,
            Some(InlineLogo {
                png: logo_png,
                row: logo_row,
                column: logo_column,
            }),
            Some(InlinePanel {
                row: panel_row,
                height: panel_height,
            }),
            false,
        )
    }
    /// Set the animation phase used by the live activity indicator.
    pub fn set_activity_elapsed(&mut self, milliseconds: u64) {
        self.activity_millis = milliseconds;
    }

    fn paint_inner(
        &mut self,
        frame: &Frame,
        styled: bool,
        styled_prefix: usize,
        logo: Option<InlineLogo<'_>>,
        panel: Option<InlinePanel>,
        bottom: bool,
    ) -> io::Result<()> {
        if let Some(viewport) = &mut self.viewport {
            let mut frame = frame.clone();
            for (index, row) in frame.lines.iter_mut().enumerate() {
                if !styled && index >= styled_prefix {
                    *row = super::widgets::style_live_row(
                        row,
                        &self.theme,
                        self.mode,
                        self.activity_millis,
                    );
                }
            }
            viewport.set_frame(&frame, styled_prefix);
            return viewport.draw(&mut self.writer, self.size);
        }
        if !self.interactive {
            return Ok(());
        }
        let rows = frame.lines.len().min(self.size.1);
        let lines = frame
            .lines
            .iter()
            .take(rows)
            .enumerate()
            .map(|(index, line)| {
                if styled || index < styled_prefix {
                    return line.clone();
                }
                let text = clip(line, self.size.0.saturating_sub(1).max(1));
                let solid_panel = panel.is_some_and(|panel| {
                    index >= panel.row && index < panel.row.saturating_add(panel.height)
                });
                if solid_panel {
                    super::widgets::style_info_panel_row(&text, &self.theme, self.mode)
                } else {
                    super::widgets::style_live_row(
                        &text,
                        &self.theme,
                        self.mode,
                        self.activity_millis,
                    )
                }
            })
            .collect::<Vec<_>>();
        let logo_position = logo.map(|logo| (logo.row, logo.column));
        let logo_ready = logo_position.is_none_or(|position| {
            !self.kitty_graphics || self.kitty_logo_position == Some(position)
        });
        if self.live_valid
            && !self.force_redraw
            && self.live_lines == lines
            && self.live_cursor == frame.cursor
            && logo_ready
        {
            return Ok(());
        }
        if !bottom
            && self.live_valid
            && !self.live_bottom
            && !self.force_redraw
            && logo.is_none()
            && panel.is_none()
        {
            return self.sync(|this| {
                if this.cursor_saved {
                    this.writer.write_all(b"\x1b8")?;
                    this.cursor_saved = false;
                }
                if this.live_rows > 0 {
                    write!(this.writer, "\r\x1b[{}A", this.live_rows)?;
                }
                for (index, line) in lines.iter().enumerate() {
                    if this.live_lines.get(index) != Some(line) {
                        write!(this.writer, "{line}\x1b[0m\x1b[K")?;
                    }
                    this.writer.write_all(b"\r\n")?;
                }
                let shrink = this.live_rows.saturating_sub(lines.len());
                for index in 0..shrink {
                    this.writer.write_all(b"\x1b[2K")?;
                    if index + 1 < shrink {
                        this.writer.write_all(b"\x1b[1B\r")?;
                    }
                }
                if shrink > 1 {
                    write!(this.writer, "\x1b[{}A", shrink - 1)?;
                }
                this.live_rows = lines.len();
                this.live_lines = lines;
                this.live_cursor = frame.cursor;
                if let Some((row, col)) = frame.cursor.filter(|(row, _)| *row < this.live_rows) {
                    write!(this.writer, "\x1b7\x1b[{}A\r", this.live_rows - row)?;
                    let col = col.min(this.size.0 - 1);
                    if col > 0 {
                        write!(this.writer, "\x1b[{col}C")?;
                    }
                    this.writer.write_all(b"\x1b[?25h")?;
                    this.cursor_saved = true;
                } else {
                    this.writer.write_all(b"\x1b[?25l")?;
                }
                Ok(())
            });
        }
        if bottom && this_can_update_in_place(self, &lines) {
            return self.sync(|this| {
                if this.cursor_saved {
                    this.writer.write_all(b"\x1b8")?;
                    this.cursor_saved = false;
                }
                let old_rows = this.live_rows;
                let growth = lines.len().saturating_sub(old_rows);
                let shrink = old_rows.saturating_sub(lines.len());
                if growth > 0 {
                    write!(this.writer, "\x1b[{};1H", this.size.1)?;
                    for _ in 0..growth {
                        this.writer.write_all(b"\r\n")?;
                    }
                }
                let old_start = this.size.1.saturating_sub(old_rows).max(1);
                for index in 0..shrink {
                    write!(this.writer, "\x1b[{};1H\x1b[2K", old_start + index)?;
                }
                let start = this.size.1.saturating_sub(lines.len()).max(1);
                for (index, line) in lines.iter().enumerate() {
                    let previous = this.live_lines.get(index + shrink).map(String::as_str);
                    if previous != Some(line.as_str()) {
                        write!(this.writer, "\x1b[{};1H{line}\x1b[0m\x1b[K", start + index)?;
                    }
                }
                this.live_rows = lines.len();
                this.live_lines = lines;
                this.live_cursor = frame.cursor;
                write!(this.writer, "\x1b[{};1H", this.size.1)?;
                if let Some((row, col)) = frame.cursor.filter(|(row, _)| *row < this.live_rows) {
                    write!(this.writer, "\x1b7\x1b[{}A\r", this.live_rows - row)?;
                    let col = col.min(this.size.0 - 1);
                    if col > 0 {
                        write!(this.writer, "\x1b[{col}C")?;
                    }
                    this.writer.write_all(b"\x1b[?25h")?;
                    this.cursor_saved = true;
                } else {
                    this.writer.write_all(b"\x1b[?25l")?;
                }
                Ok(())
            });
        }
        self.sync(|this| {
            let row_count_changed = this.live_valid && this.live_rows != lines.len();
            let bottom_growth =
                if bottom && this.live_bottom && this.live_valid && !this.force_redraw {
                    lines.len().saturating_sub(this.live_rows)
                } else {
                    0
                };
            if this.force_redraw || row_count_changed {
                this.clear_live()?;
                this.force_redraw = false;
            }
            if bottom_growth > 0 {
                // A taller live area must scroll older conversation up, not paint over it.
                write!(this.writer, "\x1b[{};1H", this.size.1)?;
                for _ in 0..bottom_growth {
                    this.writer.write_all(b"\r\n")?;
                }
            }
            if this.cursor_saved {
                this.writer.write_all(b"\x1b8")?;
                this.cursor_saved = false;
            }
            if this.live_rows > 0 {
                write!(this.writer, "\r\x1b[{}A", this.live_rows)?;
            } else {
                this.writer.write_all(b"\r")?;
            }
            if bottom && this.reserve_bottom {
                // Make room below newly finalized scrollback before anchoring the composer.
                for _ in 0..lines.len() {
                    this.writer.write_all(b"\r\n")?;
                }
                let gap = this
                    .remaining_bottom
                    .take()
                    .unwrap_or(0)
                    .saturating_sub(lines.len());
                if gap > 0 {
                    // The finished live area may be taller than the new composer.
                    // Move its conversation down into the unused cleared space.
                    write!(this.writer, "\x1b[{gap}T")?;
                }
                this.reserve_bottom = false;
            }
            if bottom {
                write!(
                    this.writer,
                    "\x1b[{};1H",
                    this.size.1.saturating_sub(lines.len()).max(1)
                )?;
            }
            for (index, line) in lines.iter().enumerate() {
                let previous = this.live_lines.get(index).map(String::as_str).unwrap_or("");
                if line != previous {
                    write!(this.writer, "\x1b[2K{line}\x1b[0m")?;
                }
                this.writer.write_all(b"\r\n")?;
            }
            if let Some(logo) = logo {
                this.writer.write_all(b"\x1b7")?;
                this.place_kitty_logo(logo)?;
                this.writer.write_all(b"\x1b8")?;
            }
            this.live_rows = lines.len();
            this.live_lines = lines;
            this.live_cursor = frame.cursor;
            this.live_valid = true;
            this.live_bottom = bottom;
            if let Some((row, col)) = frame.cursor.filter(|(row, _)| *row < this.live_rows) {
                // Save the end of the live area, then place the cursor in the composer.
                write!(this.writer, "\x1b7\x1b[{}A\r", this.live_rows - row)?;
                let col = col.min(this.size.0 - 1);
                if col > 0 {
                    write!(this.writer, "\x1b[{col}C")?;
                }
                this.writer.write_all(b"\x1b[?25h")?;
                this.cursor_saved = true;
            } else {
                this.writer.write_all(b"\x1b[?25l")?;
            }
            Ok(())
        })
    }
    fn place_kitty_logo(&mut self, logo: InlineLogo<'_>) -> io::Result<()> {
        if !self.kitty_graphics || logo.row + LOGO_ROWS > self.size.1 {
            return Ok(());
        }
        let position = (logo.row, logo.column);
        if self.kitty_logo_position == Some(position) {
            return Ok(());
        }
        if self.kitty_logo_loaded {
            write!(self.writer, "\x1b_Ga=d,d=p,i={KITTY_LOGO_ID}\x1b\\")?;
        } else {
            transmit_kitty_png(&mut self.writer, KITTY_LOGO_ID, logo.png)?;
            self.kitty_logo_loaded = true;
        }
        write!(
            self.writer,
            "\x1b[{};{}H\x1b_Ga=p,i={KITTY_LOGO_ID},c={LOGO_COLUMNS},r={LOGO_ROWS},z=1,C=1\x1b\\",
            logo.row + 1,
            logo.column + 1
        )?;
        self.kitty_logo_position = Some(position);
        Ok(())
    }
    fn clear_kitty_logo(&mut self) -> io::Result<()> {
        if self.kitty_graphics && self.kitty_logo_loaded {
            write!(self.writer, "\x1b_Ga=d,d=i,i={KITTY_LOGO_ID}\x1b\\")?;
        }
        self.kitty_logo_loaded = false;
        self.kitty_logo_position = None;
        Ok(())
    }
    /// Return the output sink, useful for deterministic byte-level assertions.
    pub fn into_inner(self) -> W {
        self.writer.into_parts().0
    }
}

fn this_can_update_in_place<W: Write>(renderer: &Renderer<W>, lines: &[String]) -> bool {
    renderer.live_valid
        && renderer.live_bottom
        && !renderer.force_redraw
        && !renderer.reserve_bottom
        && lines.len() < renderer.size.1
}

fn transmit_kitty_png(writer: &mut impl Write, id: u32, png: &[u8]) -> io::Result<()> {
    let encoded = STANDARD.encode(png);
    let chunks = encoded.as_bytes().chunks(KITTY_CHUNK_SIZE);
    let count = chunks.len();
    for (index, chunk) in chunks.enumerate() {
        let more = usize::from(index + 1 < count);
        write!(
            writer,
            "\x1b_Ga=t,f=100,t=d,i={id},m={more};{}\x1b\\",
            std::str::from_utf8(chunk).map_err(io::Error::other)?
        )?;
    }
    Ok(())
}

fn kitty_graphics_available() -> bool {
    match std::env::var("GITCODE_INLINE_IMAGES").as_deref() {
        Ok("0") => return false,
        Ok("1") => return true,
        _ => {}
    }
    if std::env::var_os("TMUX").is_some() {
        return false;
    }
    let term = std::env::var("TERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let program = std::env::var("TERM_PROGRAM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    term.contains("kitty")
        || term.contains("ghostty")
        || program.contains("kitty")
        || program.contains("wezterm")
        || program.contains("ghostty")
        || std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var_os("WEZTERM_PANE").is_some()
        || std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some()
}

#[cfg(test)]
mod elapsed_tests {
    use super::elapsed_duration;

    #[test]
    fn displays_elapsed_time_across_unit_boundaries() {
        for (seconds, expected) in [
            (0, "0с"),
            (59, "59с"),
            (60, "1м 0с"),
            (72, "1м 12с"),
            (562, "9м 22с"),
            (3_600, "1ч 0с"),
            (6_852, "1ч 54м 12с"),
            (86_400, "1д 0с"),
            (604_800, "1нед 0с"),
            (31_536_000, "1г 0с"),
            (32_234_472, "1г 1нед 1д 2ч 1м 12с"),
        ] {
            assert_eq!(elapsed_duration(seconds), expected, "{seconds}");
        }
        assert!(!elapsed_duration(u64::MAX).is_empty());
    }
}

#[cfg(test)]
mod subagent_spinner_tests {
    use super::*;
    use crate::theme::{self, ColorMode, Theme};
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn circle_fills_and_empties_at_a_steady_pace_and_keeps_its_color() {
        let expected = ["○", "◔", "◑", "◕", "●", "◕", "◑", "◔", "○"];
        let theme = Theme::default();
        let rose = theme::foreground(theme.rose, ColorMode::TrueColor);
        for (step, glyph) in expected.iter().enumerate() {
            let ms = step as u64 * 350;
            assert_eq!(subagent_spinner(ms, false), *glyph);
            assert_eq!(subagent_spinner(ms + 349, false), *glyph);
            assert_eq!(glyph.width(), 1);
            let row = format!("  {glyph} explore · task · Думает · 0↓");
            let styled =
                super::super::widgets::style_live_row(&row, &theme, ColorMode::TrueColor, ms);
            assert!(styled.starts_with(&format!("  {rose}{glyph}")));
        }
        for frame in 0..8 {
            assert!(subagent_spinner(frame * 350, true).is_ascii());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod clipboard_notice_tests {
    use super::*;
    use crate::tui::input::Editor;
    #[test]
    fn copy_notice_is_above_composer_during_idle_and_generation() {
        for busy in [false, true] {
            let status = Status {
                busy,
                copied_chars: Some(23),
                ..Status::default()
            };
            let frame = Frame::input(&Editor::default(), &status, 80, 24);
            let notice = frame
                .lines
                .iter()
                .position(|line| line.contains("Скопировано · 23 символа"))
                .unwrap();
            let prompt = frame
                .lines
                .iter()
                .position(|line| line.contains("Спроси, или / для команд"))
                .unwrap();
            assert!(notice < prompt);
            let styled = super::super::widgets::style_frame_row(
                &frame.lines[notice],
                &Theme::default(),
                ColorMode::TrueColor,
            );
            assert!(styled.contains(&crate::theme::foreground(
                Theme::default().rose,
                ColorMode::TrueColor
            )));
        }
    }
}
