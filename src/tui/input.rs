//! Pure grapheme-aware editor driven by terminal events; no polling or IO.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Action consumed by application orchestration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Visible state changed.
    Redraw,
    /// No action, including key release events.
    None,
    /// Submit the complete text, including collapsed paste contents.
    Submit(String),
    /// Interrupt the active turn or arm an application-level exit confirmation.
    Interrupt,
    /// Request application shutdown on an empty buffer.
    Exit,
    /// Open conversation rewind after two consecutive Escape keys.
    Rewind,
    /// Repaint after a resize or Ctrl+L.
    Refresh,
    /// Show transcript overlay.
    Transcript,
    /// Toggle todo overlay.
    Todos,
    /// Cycle permission mode.
    CyclePermission,
    /// Let an external completion controller accept or offer a completion.
    Complete,
    /// Let an external history controller open fuzzy history search.
    SearchHistory,
    /// Explicitly request an image from the native clipboard (no IO in the editor).
    PasteImage,
}
/// Plain editor projection with a cell-based cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputView {
    /// Wrapped display rows; contains no ANSI controls.
    pub lines: Vec<String>,
    /// Cursor row within the display rows.
    pub cursor_row: usize,
    /// Cursor column measured in terminal cells.
    pub cursor_col: usize,
}
/// One normalized paste range in the UTF-8 buffer.
#[derive(Debug, Clone)]
struct Paste {
    start: usize,
    end: usize,
    lines: usize,
}

/// Multiline UTF-8 buffer with session history and atomic large-paste chips.
#[derive(Debug, Default)]
pub struct Editor {
    text: String,
    cursor: usize,
    pastes: Vec<Paste>,
    history: Vec<String>,
    history_at: Option<usize>,
    draft: String,
    escaped: bool,
    completion_closed: bool,
    completion_selected: usize,
    delete_repeats: usize,
    image: Option<crate::provider::canonical::Content>,
}
impl Editor {
    /// Replace the explicitly staged image; at most one image is retained per draft.
    pub fn set_image(&mut self, image: crate::provider::canonical::Content) {
        self.image = Some(image);
    }
    /// Move the attachment together with its submitted prompt, never a later draft.
    pub fn take_image(&mut self) -> Option<crate::provider::canonical::Content> {
        self.image.take()
    }
    /// Current full prompt, without display-only paste chips.
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Whether slash completion should be visible without any IO.
    ///
    /// Only a bare command name opens the menu: `/`, `/usa`, or `/usage` while the
    /// name is still being typed. Anything else that merely starts with a slash —
    /// a pasted path, a multi-line paste, or `команда аргументы` — is an ordinary
    /// prompt and must not summon a command list.
    pub fn completion_open(&self) -> bool {
        if self.completion_closed {
            return false;
        }
        let trimmed = self.text.trim_end();
        let Some(name) = trimmed.strip_prefix('/') else {
            return false;
        };
        name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !super::commands::candidates(trimmed).is_empty()
    }
    /// Current bounded slash selection.
    pub fn completion_selected(&self) -> usize {
        self.completion_selected.min(
            super::commands::candidates(&self.text)
                .len()
                .saturating_sub(1),
        )
    }
    /// Current cursor as a UTF-8 byte offset on a grapheme boundary.
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    /// Load history supplied by a persistence layer, oldest first.
    pub fn set_history(&mut self, history: Vec<String>) {
        self.history = history
            .into_iter()
            .rev()
            .take(100)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        self.history_at = None;
    }
    /// Expose session history for a persistence layer.
    pub fn history(&self) -> &[String] {
        &self.history
    }
    /// Replace the buffer, for example after accepting an external completion.
    pub fn set_text(&mut self, text: &str) {
        self.completion_closed = false;
        self.completion_selected = 0;
        self.image = None;
        self.text = normalize(text);
        self.cursor = self.text.len();
        self.pastes.clear();
        self.history_at = None;
    }
    fn previous(&self) -> usize {
        if let Some(p) = self.pastes.iter().find(|p| p.end == self.cursor) {
            return p.start;
        }
        self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }
    fn next(&self) -> usize {
        if let Some(p) = self.pastes.iter().find(|p| p.start == self.cursor) {
            return p.end;
        }
        self.text[self.cursor..]
            .graphemes(true)
            .next()
            .map_or(self.cursor, |g| self.cursor + g.len())
    }
    fn replace(&mut self, start: usize, end: usize, replacement: &str) {
        self.completion_closed = false;
        self.completion_selected = 0;
        let added = replacement.len();
        let removed = end - start;
        self.pastes.retain_mut(|p| {
            if p.end <= start {
                true
            } else if p.start >= end {
                p.start = p.start + added - removed;
                p.end = p.end + added - removed;
                true
            } else {
                false
            }
        });
        self.text.replace_range(start..end, replacement);
        self.cursor = start + added;
    }
    fn insert(&mut self, text: &str, paste: bool) {
        let text = normalize(text);
        let start = self.cursor;
        self.replace(start, start, &text);
        let lines = text.lines().count().max(1);
        if paste && (lines > 4 || text.chars().count() > 500) {
            self.pastes.push(Paste {
                start,
                end: self.cursor,
                lines,
            });
            self.pastes.sort_by_key(|p| p.start);
        }
    }
    fn vertical(&mut self, down: bool) {
        let start = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
        let end = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i);
        if (!down && start == 0) || (down && end == self.text.len()) {
            self.recall(down);
            return;
        }
        let col = UnicodeWidthStr::width(&self.text[start..self.cursor]);
        let target_start = if down {
            end + 1
        } else {
            self.text[..start - 1].rfind('\n').map_or(0, |i| i + 1)
        };
        let target_end = self.text[target_start..]
            .find('\n')
            .map_or(self.text.len(), |i| target_start + i);
        let mut used = 0;
        self.cursor = target_start;
        for (i, g) in self.text[target_start..target_end].grapheme_indices(true) {
            used += UnicodeWidthStr::width(g);
            if used > col {
                break;
            }
            self.cursor = target_start + i + g.len();
        }
        // Vertical motion cannot place an invisible cursor inside a collapsed paste.
        if let Some(p) = self
            .pastes
            .iter()
            .find(|p| p.start < self.cursor && self.cursor < p.end)
        {
            self.cursor = if down { p.end } else { p.start };
        }
    }
    fn recall(&mut self, down: bool) {
        if self.history.is_empty() {
            return;
        }
        let next = match (self.history_at, down) {
            (None, true) => return,
            (None, false) => {
                self.draft = self.text.clone();
                self.history.len() - 1
            }
            (Some(i), false) => i.saturating_sub(1),
            (Some(i), true) if i + 1 < self.history.len() => i + 1,
            (Some(_), true) => {
                let draft = self.draft.clone();
                self.set_text(&draft);
                return;
            }
        };
        let text = self.history[next].clone();
        self.set_text(&text);
        self.completion_closed = true;
        self.history_at = Some(next);
    }
    /// Apply one terminal event and return an orchestration action immediately.
    pub fn handle(&mut self, event: Event) -> Action {
        match event {
            Event::Paste(text) => {
                self.escaped = false;
                self.insert(&text, true);
                Action::Redraw
            }
            Event::Resize(_, _) => Action::Refresh,
            Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key),
            _ => Action::None,
        }
    }
    fn key(&mut self, key: KeyEvent) -> Action {
        use KeyCode::*;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let deleting = matches!(key.code, Backspace | Delete);
        if deleting && key.kind == KeyEventKind::Repeat {
            self.delete_repeats = self.delete_repeats.saturating_add(1);
        } else {
            self.delete_repeats = 0;
        }
        let delete_count = 1 + (self.delete_repeats / 8).min(3);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if key.code == Esc {
            self.image = None;
        }
        if self.completion_open() && !ctrl {
            let choices = super::commands::candidates(&self.text);
            let selected = self.completion_selected();
            match key.code {
                Esc => {
                    self.completion_closed = true;
                    self.escaped = false;
                    return Action::Redraw;
                }
                Up => {
                    self.completion_selected = selected.saturating_sub(1);
                    return Action::Redraw;
                }
                Down => {
                    self.completion_selected = (selected + 1).min(choices.len().saturating_sub(1));
                    return Action::Redraw;
                }
                Tab | Enter if !shift && !alt && !self.text.contains(char::is_whitespace) => {
                    if let Some(command) = choices.get(selected) {
                        let command = format!("/{}", command.name);
                        self.set_text(&command);
                        if key.code == Tab {
                            self.text.push(' ');
                            self.cursor = self.text.len();
                            return Action::Redraw;
                        }
                        self.completion_closed = true;
                    }
                }
                _ => {}
            }
        }
        let was_escaped = std::mem::replace(&mut self.escaped, key.code == Esc);
        match key.code {
            Esc => {
                self.image = None;
                return if was_escaped {
                    self.escaped = false;
                    Action::Rewind
                } else {
                    Action::Interrupt
                };
            }
            Char('c') if ctrl => {
                if self.text.is_empty() && self.image.is_none() {
                    return Action::Interrupt;
                }
                self.set_text("");
            }
            Char('d') if ctrl => {
                if self.text.is_empty() {
                    return Action::Exit;
                }
                self.replace(self.cursor, self.next(), "");
            }
            Char('v' | 'V') if ctrl && !alt && !shift && key.kind == KeyEventKind::Press => {
                return Action::PasteImage;
            }
            Char('l') if ctrl => return Action::Refresh,
            Char('o') if ctrl => return Action::Transcript,
            Char('t') if ctrl => return Action::Todos,
            Char('r') if ctrl => return Action::SearchHistory,
            BackTab => return Action::CyclePermission,
            Tab => return Action::Complete,
            Char('j') if ctrl => self.insert("\n", false),
            Enter if shift || alt => self.insert("\n", false),
            Enter if self.text[..self.cursor].ends_with('\\') => {
                self.replace(self.cursor - 1, self.cursor, "\n");
            }
            Enter => {
                if self.text.trim().is_empty() && self.image.is_none() {
                    return Action::None;
                }
                let text = std::mem::take(&mut self.text);
                if !text.is_empty() {
                    self.history.push(text.clone());
                    if self.history.len() > 100 {
                        self.history.remove(0);
                    }
                }
                self.cursor = 0;
                self.pastes.clear();
                self.history_at = None;
                return Action::Submit(text);
            }
            Left | Char('b') if key.code == Left || ctrl => self.cursor = self.previous(),
            Right | Char('f') if key.code == Right || ctrl => self.cursor = self.next(),
            Home | Char('a') if key.code == Home || ctrl => {
                self.cursor = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
            }
            End | Char('e') if key.code == End || ctrl => {
                self.cursor += self.text[self.cursor..]
                    .find('\n')
                    .unwrap_or(self.text.len() - self.cursor);
            }
            Up | Char('p') if key.code == Up || ctrl => self.vertical(false),
            Down | Char('n') if key.code == Down || ctrl => self.vertical(true),
            Backspace => {
                for _ in 0..delete_count {
                    self.replace(self.previous(), self.cursor, "");
                }
            }
            Delete => {
                for _ in 0..delete_count {
                    self.replace(self.cursor, self.next(), "");
                }
            }
            Char('u') if ctrl => {
                let start = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
                self.replace(start, self.cursor, "");
            }
            Char('k') if ctrl => {
                let end = self.text[self.cursor..]
                    .find('\n')
                    .map_or(self.text.len(), |i| self.cursor + i);
                self.replace(self.cursor, end, "");
            }
            Char('w') if ctrl => {
                let mut start = self.cursor;
                while start > 0
                    && self.text[..start]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace)
                {
                    start = self.text[..start]
                        .grapheme_indices(true)
                        .next_back()
                        .map_or(0, |(i, _)| i);
                }
                while start > 0
                    && self.text[..start]
                        .chars()
                        .next_back()
                        .is_some_and(|c| !c.is_whitespace())
                {
                    start = self.text[..start]
                        .grapheme_indices(true)
                        .next_back()
                        .map_or(0, |(i, _)| i);
                }
                self.replace(start, self.cursor, "");
            }
            Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert(&c.to_string(), false)
            }
            _ => return Action::None,
        }
        // Readline motions may jump across a chip's hidden logical lines.
        if let Some(p) = self
            .pastes
            .iter()
            .find(|p| p.start < self.cursor && self.cursor < p.end)
        {
            self.cursor = p.start;
        }
        Action::Redraw
    }
    /// Project the buffer to wrapped rows, collapsing large paste regions atomically.
    pub fn view(&self, width: usize) -> InputView {
        let mut display = String::new();
        let mut display_cursor = 0;
        let mut at = 0;
        for p in &self.pastes {
            display.push_str(&self.text[at..p.start]);
            if self.cursor <= p.start && self.cursor >= at {
                display_cursor = display.len() - (p.start - self.cursor);
            }
            display.push_str(&format!("[pasted {} lines]", p.lines));
            at = p.end;
        }
        let tail = display.len();
        display.push_str(&self.text[at..]);
        if self.cursor >= at {
            display_cursor = tail + self.cursor - at;
        }
        if self.image.is_some() {
            display.push('\n');
            display.push_str(crate::i18n::text(
                crate::i18n::Language::default(),
                crate::i18n::Key::ImageAttached,
            ));
        }
        let width = width.max(1);
        let mut view = InputView {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
        };
        let mut col = 0;
        let mut soft_wrap = false;
        for (start, part) in display.split_word_bound_indices() {
            if !part.chars().all(char::is_whitespace)
                && col > 0
                && col + UnicodeWidthStr::width(part) > width
            {
                view.lines.push(String::new());
                col = 0;
                soft_wrap = true;
            }
            for (offset, g) in part.grapheme_indices(true) {
                let cells = UnicodeWidthStr::width(g);
                if g != "\n" && col + cells > width && col > 0 {
                    view.lines.push(String::new());
                    col = 0;
                    soft_wrap = true;
                }
                if start + offset == display_cursor {
                    view.cursor_row = view.lines.len() - 1;
                    view.cursor_col = col;
                }
                if g == "\n" {
                    view.lines.push(String::new());
                    col = 0;
                    soft_wrap = false;
                } else {
                    // A separator after an exact-width word belongs to the soft
                    // break, not to the next row. Keep it in the source buffer.
                    if soft_wrap && col == 0 && g.chars().all(char::is_whitespace) {
                        continue;
                    }
                    soft_wrap = false;
                    if let Some(line) = view.lines.last_mut() {
                        line.push_str(if cells > width { "�" } else { g });
                    }
                    col += cells.min(width);
                    if col == width {
                        view.lines.push(String::new());
                        col = 0;
                        soft_wrap = true;
                    }
                }
            }
        }
        if display_cursor == display.len() {
            view.cursor_row = view.lines.len() - 1;
            view.cursor_col = col;
        }
        view
    }
}

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .flat_map(|c| {
            if c == '\t' {
                "    ".chars().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

#[cfg(test)]
mod newline_tests {
    use super::*;

    fn image() -> crate::provider::canonical::Content {
        crate::provider::canonical::Content::Image {
            media_type: "image/png".into(),
            data: "fixture".into(),
        }
    }

    #[test]
    fn image_read_requires_explicit_ctrl_v_press() {
        let mut editor = Editor::default();
        assert_eq!(editor.handle(Event::Paste("text".into())), Action::Redraw);
        assert_eq!(editor.text(), "text");
        assert_eq!(
            editor.handle(Event::Key(KeyEvent::new(
                KeyCode::Char('v'),
                KeyModifiers::CONTROL
            ))),
            Action::PasteImage
        );
        assert_eq!(editor.text(), "text");
        let mut release = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);
        release.kind = KeyEventKind::Release;
        assert_eq!(editor.handle(Event::Key(release)), Action::None);
        release.kind = KeyEventKind::Repeat;
        assert_eq!(editor.handle(Event::Key(release)), Action::None);
    }

    #[test]
    fn image_only_submission_moves_once_and_cancel_or_clear_discards() {
        let mut editor = Editor::default();
        editor.set_image(image());
        assert!(editor
            .view(80)
            .lines
            .iter()
            .any(|line| line.contains("PNG прикреплён")));
        assert_eq!(
            editor.handle(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            ))),
            Action::Submit("".into())
        );
        let queued_image = editor.take_image();
        editor.set_text("later draft");
        editor.set_image(image());
        assert!(queued_image.is_some());
        assert!(editor.take_image().is_some());
        assert!(editor.take_image().is_none());
        for (code, modifiers) in [
            (KeyCode::Esc, KeyModifiers::NONE),
            (KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            editor.set_image(image());
            editor.handle(Event::Key(KeyEvent::new(code, modifiers)));
            assert!(editor.take_image().is_none());
        }
        editor.set_image(image());
        editor.set_text("");
        assert!(editor.take_image().is_none());
    }

    #[test]
    fn newline_shortcuts_do_not_submit_or_accept_slash_completion() {
        for (code, modifiers) in [
            (KeyCode::Enter, KeyModifiers::ALT),
            (KeyCode::Char('j'), KeyModifiers::CONTROL),
            (KeyCode::Enter, KeyModifiers::SHIFT),
        ] {
            for text in ["hello", "/hel", "hello\\"] {
                let mut editor = Editor::default();
                editor.set_text(text);
                let action = editor.handle(Event::Key(KeyEvent::new(code, modifiers)));
                assert_eq!(action, Action::Redraw);
                assert_eq!(editor.text(), format!("{text}\n"));
                assert_eq!(editor.cursor(), editor.text().len());
                assert!(editor.history.is_empty());
            }
        }
    }

    #[test]
    fn ordinary_enter_and_backslash_continuation_keep_their_behavior() {
        let mut editor = Editor::default();
        editor.set_text("hello");
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(editor.handle(enter.clone()), Action::Submit("hello".into()));
        assert_eq!(editor.text(), "");
        editor.set_text("hello\\");
        assert_eq!(editor.handle(enter.clone()), Action::Redraw);
        assert_eq!(editor.text(), "hello\n");
        assert_eq!(editor.handle(enter), Action::Submit("hello\n".into()));
    }
}
