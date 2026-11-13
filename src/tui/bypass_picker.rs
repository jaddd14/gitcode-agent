//! Confirmation for entering the unrestricted permission mode.
use super::{picker, render::Frame};
use crate::theme::{ColorMode, Theme};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

pub(super) fn frame(confirmed: bool, width: usize, height: usize, mode: ColorMode) -> Frame {
    let theme = Theme::default();
    let width = width.max(1);
    let lines = vec![
        picker::heading("  Включить BYPASS?", width, &theme, mode),
        picker::text(
            "  Режим даёт полный доступ без запросов",
            width,
            theme.placeholder,
            mode,
        ),
        String::new(),
        picker::choice(
            !confirmed,
            "1. Нет, оставить текущий режим",
            "",
            width,
            &theme,
            mode,
        ),
        picker::choice(confirmed, "2. Да, включить BYPASS", "", width, &theme, mode),
        String::new(),
        picker::text(
            "  ↑↓ выбрать   enter подтвердить   esc назад",
            width,
            theme.placeholder,
            mode,
        ),
    ];
    let lines = if height < 4 {
        vec![
            lines[0].clone(),
            lines[if confirmed { 4 } else { 3 }].clone(),
            lines[6].clone(),
        ]
    } else if height < 7 {
        vec![
            lines[0].clone(),
            lines[1].clone(),
            lines[3].clone(),
            lines[4].clone(),
            lines[6].clone(),
        ]
    } else {
        lines
    };
    Frame {
        lines: picker::constrain(lines, height),
        cursor: None,
    }
}

/// Process a key in the menu, returning a decision only on explicit confirmation or cancellation.
pub(super) fn answer(event: &Event, selected_yes: &mut bool) -> Option<bool> {
    if let Event::Key(key) = event {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        return match key.code {
            KeyCode::Up | KeyCode::Down => {
                *selected_yes = !*selected_yes;
                None
            }
            KeyCode::Enter => Some(*selected_yes),
            KeyCode::Char('y' | 'Y' | 'д' | 'Д')
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                Some(true)
            }
            KeyCode::Esc | KeyCode::Char('n' | 'N' | 'н' | 'Н') => Some(false),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(false),
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use unicode_width::UnicodeWidthStr;
    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    #[test]
    fn menu_defaults_to_no_and_requires_a_decision() {
        let mut yes = false;
        assert_eq!(answer(&key(KeyCode::Enter), &mut yes), Some(false));
        assert_eq!(answer(&key(KeyCode::Down), &mut yes), None);
        assert!(yes);
        assert_eq!(answer(&key(KeyCode::Enter), &mut yes), Some(true));
        assert_eq!(answer(&key(KeyCode::Esc), &mut yes), Some(false));
        assert_eq!(answer(&key(KeyCode::Char('y')), &mut yes), Some(true));
        assert_eq!(answer(&key(KeyCode::BackTab), &mut yes), None);
    }
    #[test]
    fn menu_uses_borderless_picker_layout_and_fits() {
        for width in [38, 60, 100] {
            for yes in [false, true] {
                let frame = frame(yes, width, 10, ColorMode::None);
                let content = frame.lines.join("\n");
                assert!(content.contains("BYPASS"));
                assert!(content.contains(if yes { "› 2. Да" } else { "› 1. Нет" }));
                assert!(content.contains("enter подтвердить"));
                assert!(!content.contains('╭'));
                assert!(frame.lines.iter().all(|line| line.width() <= width));
            }
        }
    }
    #[test]
    fn selected_choice_remains_visible_in_a_small_terminal() {
        for height in 3..=7 {
            for yes in [false, true] {
                let frame = frame(yes, 60, height, ColorMode::None);
                assert!(frame.lines.len() <= height);
                assert!(frame.lines.join("\n").contains(if yes {
                    "› 2. Да"
                } else {
                    "› 1. Нет"
                }));
            }
        }
    }
}
