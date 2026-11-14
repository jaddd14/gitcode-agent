//! Fixed composer and an in-memory, incrementally painted conversation viewport.
use super::render::Frame;
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
    target_offset: usize,
    previous_count: usize,
    screen: Vec<String>,
    drawn: Vec<String>,
    drawn_cursor: Option<(usize, usize)>,
    selection: Option<Selection>,
    // Geometry of the last painted frame, used to translate mouse rows into
    // conversation rows so a selection survives scrolling.
    view_begin: usize,
    view_chat_height: usize,
    view_width: usize,
}
/// Mouse selection anchored to conversation content, not to screen rows.
///
/// The anchor is where the button went down; the cursor follows the pointer, so
/// scrolling while the button is held extends the selection instead of dropping it.
struct Selection {
    anchor: (usize, usize),
    cursor: (usize, usize),
    // Last pointer position on screen, kept separately: the content row drifts
    // as the view scrolls, and only the screen row can tell whether the pointer
    // is resting on an edge.
    pointer: (usize, usize),
    changed: Instant,
    copied: bool,
    dragging: bool,
    composer: bool,
    text: String,
    start: (usize, usize),
    end: (usize, usize),
}
impl Viewport {
    pub fn append(&mut self, rows: &[String]) {
        // One empty row separates turns; models emit runs of newlines around
        // tool calls, and repeating them stretches the transcript.
        for row in rows {
            if row.is_empty() && self.history.last().is_some_and(String::is_empty) {
                continue;
            }
            self.history.push(row.clone());
        }
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn set_frame(&mut self, frame: &Frame, prefix: usize) {
        self.frame = Some(frame.clone());
        self.prefix = prefix.min(frame.lines.len());
    }
    fn content_point(&self, screen: (usize, usize)) -> (usize, usize) {
        let row = screen
            .0
            .min(self.view_chat_height.saturating_sub(1))
            .saturating_add(self.view_begin);
        (row, screen.1)
    }
    /// Resolve the content-anchored selection into ordered rows and plain text.
    fn resolve_selection(&mut self, width: usize) {
        let (history, frame_lines) = (&self.history, self.frame.as_ref());
        let Some(s) = &mut self.selection else {
            return;
        };
        let (a, b) = if s.anchor <= s.cursor {
            (s.anchor, s.cursor)
        } else {
            (s.cursor, s.anchor)
        };
        s.start = a;
        s.end = b;
        let empty: &[String] = &[];
        if s.composer {
            let mut parts = Vec::new();
            for row in a.0..=b.0.min(self.screen.len().saturating_sub(1)) {
                let line = super::plain(&self.screen[row]);
                let from = if row == a.0 { a.1.max(3) } else { 3 };
                let content_end = UnicodeWidthStr::width(line.as_str()).saturating_sub(1);
                let to = if row == b.0 {
                    b.1.saturating_add(1).min(content_end)
                } else {
                    content_end
                };
                parts.push(slice_cells(&line, from, to).trim_end().to_owned());
            }
            s.text = parts.join("\n");
            return;
        }
        s.text = selected_rows(
            history,
            frame_lines.map_or(empty, |f| f.lines.as_slice()),
            a,
            b,
            width,
        );
    }
    /// Extend a held selection by the rows the content moved under a fixed pointer.
    ///
    /// Wheel events arrive instead of drag events while the button stays down, so
    /// without this the endpoint would stay pinned to the old content row and the
    /// copy would stop at the first scroll. The shift is measured from the painted
    /// view, so clamping at the end of the conversation cannot over-extend it.
    fn follow_scroll(&mut self, rows: isize) {
        let Some(s) = &mut self.selection else {
            return;
        };
        if !s.dragging || s.composer {
            return;
        }
        s.cursor.0 = if rows < 0 {
            s.cursor.0.saturating_sub(rows.unsigned_abs())
        } else {
            s.cursor.0.saturating_add(rows as usize)
        };
        s.changed = Instant::now();
        s.copied = false;
    }
    pub fn mouse(&mut self, event: MouseEvent) {
        let screen = (event.row as usize, event.column as usize);
        match event.kind {
            MouseEventKind::ScrollUp => {
                self.target_offset = self.target_offset.saturating_add(6);
                if self
                    .selection
                    .as_ref()
                    .is_some_and(|s| s.dragging && !s.composer)
                {
                    self.offset = self.target_offset;
                }
            }
            MouseEventKind::ScrollDown => {
                self.target_offset = self.target_offset.saturating_sub(6);
                if self
                    .selection
                    .as_ref()
                    .is_some_and(|s| s.dragging && !s.composer)
                {
                    self.offset = self.target_offset;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let composer = screen.0 >= self.view_chat_height
                    && self.screen.get(screen.0).is_some_and(|row| {
                        let line = super::plain(row);
                        line.starts_with(" › ")
                            || line.starts_with("│  ")
                            || line.starts_with("|  ")
                    });
                if screen.0 >= self.view_chat_height && !composer {
                    return;
                }
                let point = if composer {
                    // Padding to the right of the prompt is not text: a click there
                    // snaps to the end of the line instead of parking a block cursor
                    // in empty space.
                    let limit = self
                        .screen
                        .get(screen.0)
                        .map_or(screen.1, |row| composer_text_end(row).max(3));
                    (screen.0, screen.1.min(limit))
                } else {
                    self.content_point(screen)
                };
                self.selection = Some(Selection {
                    anchor: point,
                    cursor: point,
                    pointer: screen,
                    changed: Instant::now(),
                    copied: false,
                    dragging: true,
                    composer,
                    text: String::new(),
                    start: point,
                    end: point,
                })
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let point = if self.selection.as_ref().is_some_and(|s| s.composer) {
                    screen
                } else {
                    self.content_point(screen)
                };
                if let Some(s) = &mut self.selection {
                    s.cursor = point;
                    s.pointer = screen;
                    s.changed = Instant::now();
                    s.copied = false;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let point = if self.selection.as_ref().is_some_and(|s| s.composer) {
                    screen
                } else {
                    self.content_point(screen)
                };
                if let Some(s) = &mut self.selection {
                    s.cursor = point;
                    s.pointer = screen;
                    s.changed = Instant::now();
                    s.copied = false;
                    s.dragging = false;
                }
            }
            _ => {}
        }
        if matches!(
            event.kind,
            MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::Drag(MouseButton::Left)
                | MouseEventKind::Up(MouseButton::Left)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
        ) {
            let width = self.view_width;
            self.resolve_selection(width);
        }
    }
    pub fn scroll_pending(&self) -> bool {
        self.offset != self.target_offset
    }
    pub fn copy_if_due(&mut self, writer: &mut impl Write) -> io::Result<Option<usize>> {
        let Some(s) = &mut self.selection else {
            return Ok(None);
        };
        if s.copied || s.start == s.end || s.changed.elapsed() < Duration::from_secs(1) {
            return Ok(None);
        }
        if !s.text.is_empty() {
            super::clipboard::write_osc52(writer, &s.text)?;
            s.copied = true;
            return Ok(Some(s.text.graphemes(true).count()));
        }
        s.copied = true;
        Ok(None)
    }
    #[allow(clippy::too_many_lines)]
    pub fn draw(&mut self, writer: &mut impl Write, size: (usize, usize)) -> io::Result<()> {
        let Some(frame) = self.frame.clone() else {
            return Ok(());
        };
        let (width, height) = size;
        let prefix = self.prefix.min(frame.lines.len());
        let composer = &frame.lines[prefix..];
        let chat_height = height.saturating_sub(composer.len());
        let count = self.history.len() + prefix;
        if self.offset > 0 {
            let added = count.saturating_sub(self.previous_count);
            self.offset = self.offset.saturating_add(added);
            self.target_offset = self.target_offset.saturating_add(added);
        }
        self.previous_count = count;
        let max_offset = count.saturating_sub(chat_height);
        self.target_offset = self.target_offset.min(max_offset);
        self.offset = self.offset.min(max_offset);
        if self.offset < self.target_offset {
            self.offset += 1;
        } else if self.offset > self.target_offset {
            self.offset -= 1;
        }
        if let Some(s) = &self.selection {
            if s.dragging && !s.composer {
                // A held drag that reaches an edge scrolls, so the copy keeps
                // following the pointer instead of stopping at the old view.
                let edge = chat_height.saturating_sub(1);
                if s.pointer.0 >= edge {
                    self.offset = self.offset.saturating_sub(3).min(max_offset);
                } else if s.pointer.0 == 0 {
                    self.offset = self.offset.saturating_add(3).min(max_offset);
                }
            }
        }
        if self
            .selection
            .as_ref()
            .is_some_and(|s| s.dragging && !s.composer)
        {
            self.target_offset = self.offset;
        }
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
        let shift = begin as isize - self.view_begin as isize;
        self.view_begin = begin;
        self.view_chat_height = chat_height;
        self.view_width = width;
        if shift != 0 {
            self.follow_scroll(shift);
        }
        if self.selection.is_some() {
            self.resolve_selection(width);
        }
        if let Some(s) = &self.selection {
            // In the composer the terminal already draws a blinking caret. A single
            // click with nothing dragged would paint one more reverse-video cell
            // right next to it, so only a real text range is highlighted.
            let skip = s.composer && s.text.trim().is_empty();
            let first = if s.composer { chat_height } else { 0 };
            let last = if s.composer {
                rows.len()
            } else {
                chat_height.min(rows.len())
            };
            for (screen_row, row) in rows
                .iter_mut()
                .enumerate()
                .take(if skip { 0 } else { last })
                .skip(if skip { 0 } else { first })
            {
                let content_row = if s.composer {
                    screen_row
                } else {
                    begin + screen_row
                };
                if content_row < s.start.0 || content_row > s.end.0 {
                    continue;
                }
                let plain = super::plain(row);
                let from = if content_row == s.start.0 {
                    s.start.1
                } else {
                    0
                };
                let limit = if s.composer {
                    composer_text_end(&plain).saturating_add(1)
                } else {
                    width
                };
                let to = if content_row == s.end.0 {
                    s.end.1.saturating_add(1)
                } else {
                    width
                }
                .min(limit);
                if to <= from {
                    continue;
                }
                *row = format!(
                    "{}\x1b[7m{}\x1b[0m{}",
                    slice_cells(&plain, 0, from),
                    slice_cells(&plain, from, to),
                    slice_cells(&plain, to, usize::MAX)
                );
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
/// Extract selected text from conversation content, independent of the viewport.
fn selected_rows(
    history: &[String],
    frame_lines: &[String],
    start: (usize, usize),
    end: (usize, usize),
    width: usize,
) -> String {
    if start == end {
        return String::new();
    }
    let total = history.len() + frame_lines.len();
    let last = end.0.min(total.saturating_sub(1));
    let mut rows = Vec::new();
    for row in start.0..=last {
        let source = if row < history.len() {
            history[row].clone()
        } else {
            frame_lines
                .get(row - history.len())
                .cloned()
                .unwrap_or_default()
        };
        let line = super::plain(&source);
        let from = if row == start.0 { start.1 } else { 0 };
        let to = if row == end.0 {
            end.1.saturating_add(1)
        } else {
            width
        };
        rows.push(slice_cells(&line, from, to).trim_end().to_owned());
    }
    rows.join("\n")
}
fn slice_cells(text: &str, from: usize, to: usize) -> String {
    let mut pos = 0;
    text.graphemes(true)
        .filter(|g| {
            let start = pos;
            pos += UnicodeWidthStr::width(*g);
            pos > from && start < to
        })
        .collect()
}
/// Last cell of real prompt text, ignoring the frame edge and right padding.
///
/// The composer paints every row as ` › text … │`, so a pointer placed past the
/// text must not anchor a selection or a block cursor in that empty space.
fn composer_text_end(row: &str) -> usize {
    let text = row
        .strip_suffix('│')
        .or_else(|| row.strip_suffix('|'))
        .unwrap_or(row);
    UnicodeWidthStr::width(text.trim_end())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
        while v.scroll_pending() {
            v.draw(&mut Vec::new(), (30, 5)).unwrap();
        }
        assert_eq!(v.screen, ["line 0", "line 1", "line 2", "line 3", "prompt"]);
        v.append(&["line 10".into()]);
        v.draw(&mut Vec::new(), (30, 5)).unwrap();
        assert_eq!(v.screen, ["line 0", "line 1", "line 2", "line 3", "prompt"]);
    }

    #[test]
    fn composer_selection_copies_only_prompt_text() {
        let mut v = Viewport::default();
        v.set_frame(
            &Frame {
                lines: vec![
                    "╭──────────────────╮".into(),
                    " › привет мир      │".into(),
                    "╰──────────────────╯".into(),
                ],
                cursor: None,
            },
            0,
        );
        v.draw(&mut Vec::new(), (20, 6)).unwrap();
        v.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4, 3));
        v.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 4, 8));
        assert_eq!(v.selection.as_ref().unwrap().text, "привет");
    }

    #[test]
    fn composer_click_past_the_text_stays_inside_prompt_content() {
        let mut v = Viewport::default();
        v.set_frame(
            &Frame {
                lines: vec![
                    "╭──────────────────╮".into(),
                    " › привет          │".into(),
                    "╰──────────────────╯".into(),
                ],
                cursor: Some((1, 2)),
            },
            0,
        );
        v.draw(&mut Vec::new(), (20, 6)).unwrap();
        // A click far right of the typed text must not anchor a block in padding.
        v.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4, 17));
        let selection = v.selection.as_ref().unwrap();
        assert!(selection.composer);
        assert_eq!(selection.anchor.1, composer_text_end(&v.screen[4]).max(3));
        v.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 4, 17));
        assert!(v.selection.as_ref().unwrap().text.trim().is_empty());
        assert!(!v.screen[4].contains("\x1b[7m"));
    }

    #[test]
    fn held_selection_keeps_extending_while_the_wheel_scrolls() {
        let mut v = Viewport::default();
        let rows: Vec<String> = (0..20).map(|n| format!("line {n}")).collect();
        v.append(&rows);
        v.set_frame(
            &Frame {
                lines: vec!["prompt".into()],
                cursor: None,
            },
            0,
        );
        v.draw(&mut Vec::new(), (30, 5)).unwrap();
        v.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
        v.mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 2, 0));
        let before = v.selection.as_ref().unwrap().text.clone();
        assert_eq!(before, "line 16\nline 17\nl");

        v.mouse(mouse(MouseEventKind::ScrollUp, 2, 0));
        v.draw(&mut Vec::new(), (30, 5)).unwrap();
        let after = v.selection.as_ref().unwrap().text.clone();
        assert!(
            after.lines().count() > before.lines().count(),
            "scroll must extend the selection: {before:?} -> {after:?}"
        );
        assert!(!after.contains("line 17"), "{after:?}");
        assert!(after.contains("line 14"), "{after:?}");

        let mut bytes = Vec::new();
        v.selection.as_mut().unwrap().changed -= Duration::from_secs(1);
        assert!(v.copy_if_due(&mut bytes).unwrap().is_some());
    }

    #[test]
    fn appended_separators_never_stack() {
        let mut v = Viewport::default();
        v.append(&["первый".into(), "".into()]);
        v.append(&["".into(), "".into(), "второй".into()]);
        assert_eq!(v.history, ["первый", "", "второй"]);
    }

    #[test]
    fn failed_selection_copy_is_retryable() {
        let mut v = Viewport::default();
        v.append(&["selected text".into()]);
        v.set_frame(
            &Frame {
                lines: vec!["prompt".into()],
                cursor: None,
            },
            0,
        );
        v.draw(&mut Vec::new(), (30, 4)).unwrap();
        v.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
        v.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 0, 7));
        v.selection.as_mut().unwrap().changed -= Duration::from_secs(1);
        assert!(v
            .copy_if_due(&mut std::io::Cursor::new(&mut [0u8; 0][..]))
            .is_err());
        assert!(!v.selection.as_ref().unwrap().copied);
        assert_eq!(v.copy_if_due(&mut Vec::new()).unwrap(), Some(8));
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
        assert_eq!(v.copy_if_due(&mut bytes).unwrap(), None);
        assert!(bytes.is_empty());
        v.selection.as_mut().unwrap().changed -= Duration::from_secs(1);
        assert_eq!(
            v.copy_if_due(&mut bytes).unwrap(),
            Some("год 1799\nрож".graphemes(true).count())
        );
        assert_eq!(String::from_utf8(bytes).unwrap(), {
            let mut expected = Vec::new();
            super::super::clipboard::write_osc52(&mut expected, "год 1799\nрож").unwrap();
            String::from_utf8(expected).unwrap()
        });
        let mut again = Vec::new();
        assert_eq!(v.copy_if_due(&mut again).unwrap(), None);
        assert!(again.is_empty());
    }
}
