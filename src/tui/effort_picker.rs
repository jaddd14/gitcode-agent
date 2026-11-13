//! Keyboard-driven reasoning effort selection.
use super::{
    picker,
    render::{Frame, Renderer},
    widgets,
};
use crate::theme::{ColorMode, Theme};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use std::io;

const LEVELS: [(&str, &str); 6] = [
    ("default", "Настройка провайдера по умолчанию"),
    ("low", "Быстрые ответы для простых задач"),
    ("medium", "Баланс скорости и глубины рассуждения"),
    ("high", "Больше внимания сложным задачам"),
    ("xhigh", "Очень глубокое рассуждение для сложной работы"),
    ("max", "Наибольшая глубина, если поддерживается моделью"),
];

fn rows(
    model: &str,
    current: Option<&str>,
    selected: usize,
    width: usize,
    height: usize,
    mode: ColorMode,
) -> Vec<String> {
    let theme = Theme::default();
    let width = width.max(1);
    let mut out = vec![picker::heading(
        &format!(
            "  Уровень рассуждения · {}",
            super::plain(model).replace('\n', " ")
        ),
        width,
        &theme,
        mode,
    )];
    out.push(String::new());
    let capacity = height.saturating_sub(4).max(1).min(LEVELS.len());
    let start = (selected + 1).saturating_sub(capacity);
    let label_width = width.saturating_sub(5).min(24);
    for (index, (level, description)) in LEVELS.iter().enumerate().skip(start).take(capacity) {
        let label = format!(
            "{}. {}{}",
            index + 1,
            level,
            if *level == current.unwrap_or("default") {
                " (текущий)"
            } else {
                ""
            }
        );
        let primary = if width >= 60 {
            widgets::fit(&label, label_width)
        } else {
            widgets::ellipsis(&label, width.saturating_sub(3), false)
        };
        let secondary = if width >= 60 {
            format!("  {description}")
        } else {
            String::new()
        };
        out.push(picker::choice(
            index == selected,
            &primary,
            &secondary,
            width,
            &theme,
            mode,
        ));
    }
    out.push(String::new());
    out.push(picker::text(
        "  ↑↓ выбрать   enter применить   esc назад",
        width,
        theme.placeholder,
        mode,
    ));
    picker::constrain(out, height)
}

/// Pick a level, or return `None` on cancellation.
pub async fn select(
    renderer: &mut Renderer<io::Stdout>,
    events: &mut EventStream,
    model: &str,
    current: Option<&str>,
) -> io::Result<Option<String>> {
    let mut selected = LEVELS
        .iter()
        .position(|(level, _)| *level == current.unwrap_or("default"))
        .unwrap_or(0);
    loop {
        let (width, height) = renderer.size();
        renderer.paint_styled(&Frame {
            lines: rows(
                model,
                current,
                selected,
                width.saturating_sub(1),
                picker::height(height),
                renderer.color_mode(),
            ),
            cursor: None,
        })?;
        match events.next().await {
            Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None)
                }
                KeyCode::Up => selected = (selected + LEVELS.len() - 1) % LEVELS.len(),
                KeyCode::Down => selected = (selected + 1) % LEVELS.len(),
                KeyCode::Enter => return Ok(Some(LEVELS[selected].0.to_owned())),
                _ => {}
            },
            Some(Ok(Event::Resize(w, h))) => renderer.resize(w.into(), h.into()),
            Some(Err(error)) => return Err(error),
            None => return Ok(None),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;
    #[test]
    fn menu_is_borderless_readable_and_bounded() {
        for width in [38, 60, 100] {
            let lines = rows("gpt-6.1-sol", Some("xhigh"), 4, width, 18, ColorMode::None);
            let text = lines.join("\n");
            assert!(text.contains("gpt-6.1-sol"));
            assert!(text.contains("› 5. xhigh (текущий)"));
            assert!(text.contains("enter применить"));
            assert!(!text.contains('╭'));
            assert!(lines.iter().all(|line| line.width() <= width));
        }
    }

    #[test]
    fn menu_fits_about_a_third_of_the_terminal_with_selected_level_visible() {
        for height in [15, 20, 25, 40] {
            let limit = picker::height(height);
            for (selected, (level, _)) in LEVELS.iter().enumerate() {
                let lines = rows("gpt-6.1-sol", None, selected, 80, limit, ColorMode::None);
                assert!(lines.len() <= limit);
                assert!(lines.iter().any(|line| line.contains(&format!(
                    "› {}. {}",
                    selected + 1,
                    level
                ))));
            }
        }
    }
}
