//! Native-scrollback output with a bounded, synchronized live area.

use super::{clip, input::Editor, statusbar::Status};
use crate::theme::{foreground, ColorMode, Theme};
use std::io::{self, Write};

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
        let activity_rows = usize::from(status.busy) + usize::from(status.retry_notice.is_some());
        let capacity = height.saturating_sub(4 + activity_rows).clamp(1, 12);
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
            title: if glyphs.ascii {
                "gitcode (^_^) ~"
            } else {
                "gitcode (•ᴗ•) ~"
            },
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
        }
        if status.busy {
            const VERBS: [&str; 5] = ["Думаю", "Колдую", "Цветём", "Собираю", "Проверяю"];
            let verb = VERBS[((status.elapsed_secs / 8) % 5) as usize];
            let mark = if glyphs.ascii { '*' } else { '✻' };
            above.push(clip(
                &format!(
                    "{mark} {verb}… ({}с · ↓ {} токенов)",
                    status.elapsed_secs,
                    super::statusbar::compact(status.output)
                ),
                width,
            ));
        }
        if !above.is_empty() {
            if let Some((row, _)) = &mut cursor {
                *row += above.len();
            }
            lines.splice(0..0, above);
        }
        let hint_line = if status.busy {
            let base = super::widgets::key_hints(&[("esc", "отменить")], width);
            if status.queued > 0 {
                clip(&format!("{base} · в очереди: {}", status.queued), width)
            } else {
                base
            }
        } else if let Some(toast) = &status.toast {
            clip(toast, width)
        } else {
            String::new()
        };
        lines.push(hint_line);
        lines.push(status.line(width));
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
    writer: W,
    live_rows: usize,
    cursor_saved: bool,
    size: (usize, usize),
    theme: Theme,
    mode: ColorMode,
    interactive: bool,
}
impl<W: Write> Renderer<W> {
    /// Create an output sink. Noninteractive mode writes finalized text only.
    pub fn new(writer: W, width: usize, height: usize, mode: ColorMode, interactive: bool) -> Self {
        Self {
            writer,
            live_rows: 0,
            cursor_saved: false,
            size: (width.max(1), height.max(1)),
            theme: Theme::default(),
            mode,
            interactive,
        }
    }
    /// Update geometry; the next event-driven paint reflows its frame.
    pub fn resize(&mut self, width: usize, height: usize) {
        self.size = (width.max(1), height.max(1));
    }
    /// Apply a selected palette to subsequent live and finalized output.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }
    /// Current terminal width and height.
    pub fn size(&self) -> (usize, usize) {
        self.size
    }
    fn clear_live(&mut self) -> io::Result<()> {
        if self.cursor_saved {
            self.writer.write_all(b"\x1b8")?;
            self.cursor_saved = false;
        }
        if self.live_rows > 0 {
            write!(self.writer, "\r\x1b[{}A\x1b[J", self.live_rows)?;
            self.live_rows = 0;
        }
        Ok(())
    }
    fn sync(&mut self, body: impl FnOnce(&mut Self) -> io::Result<()>) -> io::Result<()> {
        self.writer.write_all(b"\x1b[?2026h")?;
        let result = body(self);
        // Close even when body fails so a recoverable write never strands sync mode.
        let close = self
            .writer
            .write_all(b"\x1b[?2026l")
            .and_then(|()| self.writer.flush());
        result.and(close)
    }
    /// Permanently append text to native scrollback and remove the previous live area.
    pub fn finalize(&mut self, text: &str) -> io::Result<()> {
        let text = super::plain(text);
        if !self.interactive {
            writeln!(self.writer, "{text}")?;
            return self.writer.flush();
        }
        self.sync(|this| {
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
        if !self.interactive {
            return self.finalize(&rows.join("\n"));
        }
        self.sync(|this| {
            this.clear_live()?;
            for row in rows {
                write!(this.writer, "{row}\x1b[0m\r\n")?;
            }
            Ok(())
        })
    }
    /// Paint pre-sized, trusted theme-generated rows, preserving color escapes.
    /// Never pass provider or user text to this method.
    pub fn paint_styled(&mut self, frame: &Frame) -> io::Result<()> {
        self.paint_inner(frame, true)
    }
    /// Paint only the mutable tail; callers coalesce stream updates before invoking it.
    pub fn paint(&mut self, frame: &Frame) -> io::Result<()> {
        self.paint_inner(frame, false)
    }
    fn paint_inner(&mut self, frame: &Frame, styled: bool) -> io::Result<()> {
        if !self.interactive {
            return Ok(());
        }
        self.sync(|this| {
            this.clear_live()?;
            write!(
                this.writer,
                "\x1b[?25l{}",
                foreground(this.theme.text, this.mode)
            )?;
            let rows = frame.lines.len().min(this.size.1);
            for line in frame.lines.iter().take(rows) {
                write!(
                    this.writer,
                    "{}\x1b[0m\r\n",
                    if styled {
                        line.clone()
                    } else {
                        let text = clip(line, this.size.0.saturating_sub(1).max(1));
                        super::widgets::style_frame_row(&text, &this.theme, this.mode)
                    }
                )?;
            }
            this.live_rows = rows;
            if let Some((row, col)) = frame.cursor.filter(|(row, _)| *row < rows) {
                // Save the tail position so the next paint can erase only its own rows.
                write!(this.writer, "\x1b7\x1b[{}A\r", rows - row)?;
                let col = col.min(this.size.0 - 1);
                if col > 0 {
                    write!(this.writer, "\x1b[{col}C")?;
                }
                this.writer.write_all(b"\x1b[?25h")?;
                this.cursor_saved = true;
            }
            Ok(())
        })
    }
    /// Return the output sink, useful for deterministic byte-level assertions.
    pub fn into_inner(self) -> W {
        self.writer
    }
}
