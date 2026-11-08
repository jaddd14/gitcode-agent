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
        let view = editor.view(width.saturating_sub(2).max(1));
        if height == 1 {
            return Self {
                lines: vec![status.line(width)],
                cursor: None,
            };
        }
        let capacity = height - 1;
        let start = (view.cursor_row + 1).saturating_sub(capacity);
        let mut lines = view
            .lines
            .iter()
            .skip(start)
            .take(capacity)
            .map(|s| clip(&format!("› {s}"), width))
            .collect::<Vec<_>>();
        let cursor = Some((
            view.cursor_row - start,
            (view.cursor_col + 2).min(width - 1),
        ));
        lines.push(status.line(width));
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
                write!(this.writer, "{line}\r\n")?;
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
                        clip(line, this.size.0.saturating_sub(1).max(1))
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
