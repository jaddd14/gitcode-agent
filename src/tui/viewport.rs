//! Fixed composer and an in-memory, incrementally painted conversation viewport.
use super::render::Frame;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use std::{
    io::{self, Write},
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub(super) struct Viewport {
    history: Vec<String>,
    frame: Option<Frame>,
    prefix: usize,
    offset: usize,
    previous_count: usize,
    screen: Vec<String>,
    drawn: Vec<String>,
    drawn_cursor: Option<(usize, usize)>,
    selection: Option<Selection>,
}
struct Selection {
    start: (usize, usize),
    end: (usize, usize),
    changed: Instant,
    copied: bool,
    dragging: bool,
    text: String,
}
impl Viewport {
    pub fn append(&mut self, rows: &[String]) {
        self.history.extend_from_slice(rows);
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn set_frame(&mut self, frame: &Frame, prefix: usize) {
        self.frame = Some(frame.clone());
        self.prefix = prefix.min(frame.lines.len());
    }
    pub fn mouse(&mut self, event: MouseEvent) {
        let point = (event.row as usize, event.column as usize);
        match event.kind {
            MouseEventKind::ScrollUp => {
                self.offset = self.offset.saturating_add(6);
                self.selection = None;
            }
            MouseEventKind::ScrollDown => {
                self.offset = self.offset.saturating_sub(6);
                self.selection = None;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection = Some(Selection {
                    start: point,
                    end: point,
                    changed: Instant::now(),
                    copied: false,
                    dragging: true,
                    text: String::new(),
                })
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(s) = &mut self.selection {
                    s.end = point;
                    s.changed = Instant::now();
                    s.copied = false;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(s) = &mut self.selection {
                    if s.end != point {
                        s.end = point;
                        s.changed = Instant::now();
                        s.copied = false;
                    }
                    s.dragging = false;
                }
            }
            _ => {}
        }
        if let Some(selection) = &mut self.selection {
            if matches!(
                event.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            ) {
                selection.text = selected_text(&self.screen, selection.start, selection.end);
            }
        }
    }
    pub fn copy_if_due(&mut self, writer: &mut impl Write) -> io::Result<()> {
        let Some(s) = &mut self.selection else {
            return Ok(());
        };
        if s.copied || s.start == s.end || s.changed.elapsed() < Duration::from_secs(1) {
            return Ok(());
        }
        if !s.text.is_empty() {
            write!(
                writer,
                "\x1b]52;c;{}\x07",
                STANDARD.encode(s.text.as_bytes())
            )?;
            writer.flush()?;
        }
        s.copied = true;
        Ok(())
    }
    pub fn draw(&mut self, writer: &mut impl Write, size: (usize, usize)) -> io::Result<()> {
        let Some(frame) = &self.frame else {
            return Ok(());
        };
        let (width, height) = size;
        let prefix = self.prefix.min(frame.lines.len());
        let composer = &frame.lines[prefix..];
        let chat_height = height.saturating_sub(composer.len());
        let count = self.history.len() + prefix;
        if self.offset > 0 {
            self.offset = self
                .offset
                .saturating_add(count.saturating_sub(self.previous_count));
        }
        self.previous_count = count;
        self.offset = self.offset.min(count.saturating_sub(chat_height));
        let end = count.saturating_sub(self.offset);
        let begin = end.saturating_sub(chat_height);
        let mut rows = vec![String::new(); height];
        for (screen_row, idx) in (begin..end).enumerate() {
            rows[screen_row] = if idx < self.history.len() {
                self.history[idx].clone()
            } else {
                frame.lines[idx - self.history.len()].clone()
            };
        }
        for (i, row) in composer.iter().take(height).enumerate() {
            let target = chat_height + i;
            if target < height {
                rows[target] = row.clone();
            }
        }
        for row in &mut rows {
            if UnicodeWidthStr::width(super::plain(row).as_str()) >= width {
                *row = super::clip(row, width.saturating_sub(1));
            }
        }
        self.screen = rows.clone();
        if let Some(s) = &self.selection {
            if s.start != s.end {
                let (start, end) = if s.start <= s.end {
                    (s.start, s.end)
                } else {
                    (s.end, s.start)
                };
                for row in start.0..=end.0.min(height.saturating_sub(1)) {
                    let plain = super::plain(&rows[row]);
                    let from = if row == start.0 { start.1 } else { 0 };
                    let to = if row == end.0 {
                        end.1.saturating_add(1)
                    } else {
                        width
                    };
                    rows[row] = format!(
                        "{}\x1b[7m{}\x1b[0m{}",
                        slice_cells(&plain, 0, from),
                        slice_cells(&plain, from, to),
                        slice_cells(&plain, to, usize::MAX)
                    );
                }
            }
        }
        let cursor = frame.cursor.and_then(|(r, c)| {
            (r >= prefix && chat_height + r - prefix < height)
                .then(|| (chat_height + r - prefix, c.min(width.saturating_sub(1))))
        });
        if self.drawn == rows && self.drawn_cursor == cursor {
            return Ok(());
        }
        self.drawn_cursor = cursor;
        write!(writer, "\x1b[?2026h\x1b[?25l")?;
        for (i, row) in rows.iter().enumerate() {
            if self.drawn.get(i) != Some(row) {
                write!(writer, "\x1b[{};1H{}\x1b[0m\x1b[K", i + 1, row)?;
            }
        }
        self.drawn = rows;
        if let Some((row, col)) = cursor {
            write!(writer, "\x1b[{};{}H\x1b[?25h", row + 1, col + 1)?;
        }
        write!(writer, "\x1b[?2026l")?;
        writer.flush()
    }
    pub fn invalidate(&mut self) {
        self.drawn.clear();
    }
}
fn selected_text(screen: &[String], a: (usize, usize), b: (usize, usize)) -> String {
    if a == b {
        return String::new();
    }
    let (start, end) = if a <= b { (a, b) } else { (b, a) };
    let mut rows = Vec::new();
    for row in start.0..=end.0.min(screen.len().saturating_sub(1)) {
        let line = super::plain(&screen[row]);
        let from = if row == start.0 { start.1 } else { 0 };
        let to = if row == end.0 {
            end.1.saturating_add(1)
        } else {
            usize::MAX
        };
        rows.push(slice_cells(&line, from, to).trim_end().to_owned());
    }
    rows.join("\n")
}
fn slice_cells(text: &str, from: usize, to: usize) -> String {
    let mut pos = 0;
    text.graphemes(true)
        .filter_map(|g| {
            let start = pos;
            pos += UnicodeWidthStr::width(g);
            if pos > from && start < to {
                Some(g)
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseEvent};

    fn mouse(kind: MouseEventKind, row: u16, column: u16) -> MouseEvent {
        MouseEvent {
            kind,
            row,
            column,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn scroll_keeps_composer_pinned_and_new_output_does_not_jump() {
        let mut v = Viewport::default();
        let rows: Vec<String> = (0..10).map(|n| format!("line {n}")).collect();
        v.append(&rows);
        v.set_frame(
            &Frame {
                lines: vec!["prompt".into()],
                cursor: Some((0, 0)),
            },
            0,
        );
        v.draw(&mut Vec::new(), (30, 5)).unwrap();
        assert_eq!(v.screen, ["line 6", "line 7", "line 8", "line 9", "prompt"]);
        v.mouse(mouse(MouseEventKind::ScrollUp, 0, 0));
        v.draw(&mut Vec::new(), (30, 5)).unwrap();
        assert_eq!(v.screen, ["line 0", "line 1", "line 2", "line 3", "prompt"]);
        v.append(&["line 10".into()]);
        v.draw(&mut Vec::new(), (30, 5)).unwrap();
        assert_eq!(v.screen, ["line 0", "line 1", "line 2", "line 3", "prompt"]);
    }

    #[test]
    fn copies_only_after_selection_has_been_idle_for_a_second() {
        let mut v = Viewport::default();
        v.append(&["год 1799".into(), "рождение".into()]);
        v.set_frame(
            &Frame {
                lines: vec!["prompt".into()],
                cursor: None,
            },
            0,
        );
        v.draw(&mut Vec::new(), (30, 4)).unwrap();
        v.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
        v.mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 1, 2));
        let mut bytes = Vec::new();
        v.copy_if_due(&mut bytes).unwrap();
        assert!(bytes.is_empty());
        v.selection.as_mut().unwrap().changed -= Duration::from_secs(1);
        v.copy_if_due(&mut bytes).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            format!("\x1b]52;c;{}\x07", STANDARD.encode("год 1799\nрож"))
        );
        let mut again = Vec::new();
        v.copy_if_due(&mut again).unwrap();
        assert!(again.is_empty());
    }
}
