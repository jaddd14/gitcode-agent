//! Interactive command approval, using the same borderless picker as /effort.
use super::{picker, render::Frame};
use crate::{
    agent::{ShellApproval, ShellApprovalRequest},
    theme::{ColorMode, Theme},
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) struct Pending {
    request: ShellApprovalRequest,
    selected: usize,
    scroll: usize,
}

impl Pending {
    pub fn new(request: ShellApprovalRequest) -> Self {
        Self {
            request,
            selected: 2,
            scroll: 0,
        }
    }

    fn command_rows(&self, width: usize) -> Vec<String> {
        let limit = width.saturating_sub(4).max(1);
        let mut rows = vec![String::new()];
        let mut used = 0;
        for grapheme in self.request.command.graphemes(true) {
            if grapheme == "\n" || grapheme == "\r\n" {
                rows.push(String::new());
                used = 0;
                continue;
            }
            let cells = grapheme.width();
            if used + cells > limit && used > 0 {
                rows.push(String::new());
                used = 0;
            }
            if let Some(row) = rows.last_mut() {
                row.push_str(grapheme);
            }
            used += cells;
        }
        rows
    }

    /// Rows available for the command text, and whether a paging hint is shown.
    ///
    /// `frame` and `answer` must agree: when they disagreed, the picker drew
    /// fewer rows than it assumed, so `scroll` could never reach `max_scroll`
    /// and Enter was silently dropped.
    fn layout(height: usize, rows: usize) -> (usize, bool) {
        // heading + blank + three choices + blank + hint
        let reserved = 7;
        let plain = height.saturating_sub(reserved).max(1);
        if rows > plain {
            // A paging hint costs one more row; only it knows the real capacity.
            (height.saturating_sub(reserved + 1).max(1), true)
        } else {
            (plain, false)
        }
    }

    pub fn frame(&self, width: usize, height: usize, mode: ColorMode) -> Frame {
        let theme = Theme::default();
        let width = width.max(1);
        if height < 7 || width < 16 {
            return Frame {
                lines: picker::constrain(
                    vec![
                        picker::heading("  Увеличь окно терминала", width, &theme, mode),
                        picker::text("  esc отказ", width, theme.placeholder, mode),
                    ],
                    height,
                ),
                cursor: None,
            };
        }
        let command = self.command_rows(width);
        let (capacity, paged) = Self::layout(height, command.len());
        let max_scroll = command.len().saturating_sub(capacity);
        let scroll = self.scroll.min(max_scroll);
        let mut lines = vec![picker::heading(
            "  Разрешить выполнение команды?",
            width,
            &theme,
            mode,
        )];
        for line in command.iter().skip(scroll).take(capacity) {
            lines.push(picker::text(
                &format!("  {line}"),
                width,
                theme.panel_text,
                mode,
            ));
        }
        if paged {
            lines.push(picker::text(
                &format!(
                    "  Команда: {}–{} / {} · PgUp/PgDn",
                    scroll + 1,
                    (scroll + capacity).min(command.len()),
                    command.len()
                ),
                width,
                theme.placeholder,
                mode,
            ));
        }
        lines.push(String::new());
        let similar = self.request.prefix.as_ref().map_or_else(
            || "2. Похожие команды недоступны".to_owned(),
            |prefix| format!("2. Похожие: {prefix} … (до конца чата)"),
        );
        let options = ["1. Да, один раз".to_owned(), similar, "3. Нет".to_owned()];
        if height < 6 {
            lines.push(picker::choice(
                true,
                &options[self.selected],
                "",
                width,
                &theme,
                mode,
            ));
        } else {
            for (index, label) in options.iter().enumerate() {
                lines.push(picker::choice(
                    index == self.selected,
                    label,
                    "",
                    width,
                    &theme,
                    mode,
                ));
            }
        }
        lines.push(String::new());
        lines.push(picker::text(
            "  ↑↓ выбрать   enter подтвердить   esc отказ",
            width,
            theme.placeholder,
            mode,
        ));
        Frame {
            lines: picker::constrain(lines, height),
            cursor: None,
        }
    }

    /// Returns a decision only after the entire command was made visible.
    pub fn answer(&mut self, event: &Event, width: usize, height: usize) -> Option<ShellApproval> {
        let Event::Key(key) = event else { return None };
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let rows = self.command_rows(width);
        let (capacity, _) = Self::layout(height, rows.len());
        let max_scroll = rows.len().saturating_sub(capacity);
        if height < 7 || width < 16 {
            return match key.code {
                KeyCode::Esc => Some(ShellApproval::Deny),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Some(ShellApproval::Deny)
                }
                _ => None,
            };
        }
        match key.code {
            KeyCode::Esc => Some(ShellApproval::Deny),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(ShellApproval::Deny)
            }
            KeyCode::Up => {
                self.selected = (self.selected + 2) % 3;
                None
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1) % 3;
                None
            }
            KeyCode::PageDown => {
                self.scroll = (self.scroll + capacity).min(max_scroll);
                None
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(capacity);
                None
            }
            // Denying is always safe, even while the command is still unread.
            KeyCode::Enter if self.selected == 2 => Some(ShellApproval::Deny),
            // Approving needs the full command on screen, but a partially scrolled
            // view must not swallow Enter: jump to the tail and ask again instead.
            KeyCode::Enter if self.selected == 0 => {
                if self.scroll < max_scroll {
                    self.scroll = max_scroll;
                    return None;
                }
                Some(ShellApproval::Once)
            }
            KeyCode::Enter if self.selected == 1 => {
                // Never gate this on the rendered label width: a long prefix used
                // to make Enter a silent no-op. The prefix itself is what matters.
                if self.request.prefix.is_some() {
                    return Some(ShellApproval::Similar);
                }
                // Nothing to widen: fall back to the scope that always applies and
                // let the moved marker show the key was handled.
                self.selected = 0;
                None
            }
            _ => None,
        }
    }

    pub fn resized(&mut self) {
        self.scroll = 0;
    }

    pub fn finish(self, decision: ShellApproval) {
        let _ = self.request.reply.send(decision);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use tokio::sync::oneshot;

    fn pending(command: &str, prefix: Option<&str>) -> Pending {
        let (reply, _) = oneshot::channel();
        Pending::new(ShellApprovalRequest {
            command: command.into(),
            prefix: prefix.map(str::to_owned),
            reply,
        })
    }
    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn command_options_and_hint_have_spacing_when_height_allows() {
        let p = pending("git status", Some("git status"));
        let lines = p.frame(80, 12, ColorMode::None).lines;
        assert!(lines[1].contains("git status"));
        assert!(lines[2].is_empty());
        assert!(lines[3].contains("1. Да"));
        assert!(lines[6].is_empty());
        assert!(lines[7].contains("enter подтвердить"));
        for height in 7..=12 {
            let lines = p.frame(80, height, ColorMode::None).lines;
            assert!(lines.len() <= height);
            assert!(lines.iter().any(|line| line.contains("git status")));
            assert!(lines.iter().any(|line| line.contains("› 3. Нет")));
            assert!(lines.last().unwrap().contains("enter подтвердить"));
        }
    }

    #[test]
    fn defaults_to_deny_and_exposes_scope() {
        let mut p = pending("git status --short", Some("git status"));
        let lines = p.frame(80, 12, ColorMode::None).lines.join("\n");
        assert!(lines.contains("git status --short"));
        assert!(lines.contains("Похожие: git status"));
        assert!(lines.contains("› 3. Нет"));
        assert_eq!(
            p.answer(&key(KeyCode::Enter), 80, 12),
            Some(ShellApproval::Deny)
        );
        p.answer(&key(KeyCode::Up), 80, 12);
        assert_eq!(
            p.answer(&key(KeyCode::Enter), 80, 12),
            Some(ShellApproval::Similar)
        );
    }

    #[test]
    fn long_commands_must_be_scrolled_and_small_windows_cannot_approve() {
        let mut p = pending(&format!("printf {}", "x".repeat(200)), None);
        p.answer(&key(KeyCode::Down), 40, 8);
        assert_eq!(p.answer(&key(KeyCode::Enter), 40, 8), None);
        for _ in 0..20 {
            p.answer(&key(KeyCode::PageDown), 40, 8);
        }
        assert_eq!(
            p.answer(&key(KeyCode::Enter), 40, 8),
            Some(ShellApproval::Once)
        );
        assert_eq!(p.answer(&key(KeyCode::Enter), 40, 5), None);
        assert_eq!(
            p.answer(&key(KeyCode::Esc), 40, 5),
            Some(ShellApproval::Deny)
        );
        let frame = p.frame(40, 5, ColorMode::None);
        assert!(frame.lines.join("\n").contains("Увеличь окно"));
    }

    #[test]
    fn multiline_command_must_be_reviewed_through_the_last_line() {
        let mut p = pending("first line\nsecond line\nthird line\nlast line", None);
        let first = p.frame(40, 8, ColorMode::None).lines.join("\n");
        assert!(first.contains("first line"));
        assert!(!first.contains("last line"));
        p.answer(&key(KeyCode::Down), 40, 8);
        assert_eq!(p.answer(&key(KeyCode::Enter), 40, 8), None);
        p.answer(&key(KeyCode::PageDown), 40, 8);
        let last = p.frame(40, 8, ColorMode::None).lines.join("\n");
        assert!(last.contains("last line"));
        assert_eq!(
            p.answer(&key(KeyCode::Enter), 40, 8),
            Some(ShellApproval::Once)
        );
    }

    #[test]
    fn clipped_scope_label_still_accepts_enter() {
        // A long prefix used to make the label wider than the terminal, and the
        // picker then swallowed Enter; the label is merely clipped, so choosing
        // the scope must still work.
        let mut p = pending("git status", Some("git status"));
        p.answer(&key(KeyCode::Up), 20, 8);
        assert_eq!(
            p.answer(&key(KeyCode::Enter), 20, 8),
            Some(ShellApproval::Similar)
        );
    }

    #[test]
    fn missing_scope_moves_marker_instead_of_ignoring_enter() {
        let mut p = pending("pwd", None);
        p.answer(&key(KeyCode::Up), 80, 8);
        // No prefix to widen: the marker falls back to the safe default choice.
        assert_eq!(p.answer(&key(KeyCode::Enter), 80, 8), None);
        assert_eq!(
            p.answer(&key(KeyCode::Enter), 80, 8),
            Some(ShellApproval::Once)
        );
    }

    #[test]
    fn enter_approves_after_scrolling_without_hunting_the_exact_end() {
        // Pressing Enter on a long command jumps to the last page and approves on
        // the next press, so a scrolled picker can never silently drop the key.
        let mut p = pending(&format!("printf {}", "y".repeat(400)), None);
        p.answer(&key(KeyCode::Down), 40, 10);
        assert_eq!(p.answer(&key(KeyCode::Enter), 40, 10), None);
        assert_eq!(
            p.answer(&key(KeyCode::Enter), 40, 10),
            Some(ShellApproval::Once)
        );
    }
}
