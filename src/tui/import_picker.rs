//! Borderless import mode and path input, matching the other command pickers.
use super::{
    picker,
    render::{Frame, Renderer},
};
use crate::theme::{ColorMode, Theme};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use std::{io, path::PathBuf};

/// Chosen import mode.
pub enum Choice {
    /// Import all sessions from the chosen agent.
    Automatic(crate::session::import::Source),
    /// Enter a journal path.
    Path(PathBuf),
}

fn rows(
    selected: usize,
    entering: bool,
    path: &str,
    width: usize,
    height: usize,
    mode: ColorMode,
) -> Vec<String> {
    let theme = Theme::default();
    let mut lines = vec![
        picker::heading("  Импорт сессии · Claude Code / Codex", width, &theme, mode),
        String::new(),
    ];
    if entering {
        lines.push(picker::text(
            &format!("  Путь: {path}▏"),
            width,
            theme.text,
            mode,
        ));
        lines.push(String::new());
        lines.push(picker::text(
            "  Укажите путь к файлу .jsonl",
            width,
            theme.placeholder,
            mode,
        ));
        lines.push(picker::text(
            "  enter импортировать   esc назад",
            width,
            theme.placeholder,
            mode,
        ));
    } else {
        let choices = [
            ("1. Claude Code", "  Импортировать все сессии"),
            ("2. Codex", "  Импортировать все сессии"),
            ("3. Указать путь", "  Импортировать один файл"),
        ];
        let capacity = height.saturating_sub(4).clamp(1, choices.len());
        let start = (selected + 1).saturating_sub(capacity);
        for (index, (label, description)) in choices.iter().enumerate().skip(start).take(capacity) {
            lines.push(picker::choice(
                index == selected,
                label,
                description,
                width,
                &theme,
                mode,
            ));
        }
        lines.push(String::new());
        lines.push(picker::text(
            "  ↑↓ выбрать   enter продолжить   esc назад",
            width,
            theme.placeholder,
            mode,
        ));
    }
    picker::constrain(lines, height)
}

/// Choose discovery or enter a local path.
pub async fn select(
    renderer: &mut Renderer<io::Stdout>,
    events: &mut EventStream,
) -> io::Result<Option<Choice>> {
    let mut selected = 0usize;
    let mut entering = false;
    let mut path = String::new();
    loop {
        let (width, height) = renderer.size();
        let width = width.saturating_sub(1);
        let mode = renderer.color_mode();
        renderer.paint_styled(&Frame {
            lines: rows(
                selected,
                entering,
                &path,
                width,
                picker::height(height),
                mode,
            ),
            cursor: None,
        })?;
        match events.next().await {
            Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None)
                }
                KeyCode::Esc if entering => entering = false,
                KeyCode::Esc => return Ok(None),
                KeyCode::Up if !entering => selected = (selected + 2) % 3,
                KeyCode::Down if !entering => selected = (selected + 1) % 3,
                KeyCode::Enter if !entering && selected == 0 => {
                    return Ok(Some(Choice::Automatic(
                        crate::session::import::Source::Claude,
                    )))
                }
                KeyCode::Enter if !entering && selected == 1 => {
                    return Ok(Some(Choice::Automatic(
                        crate::session::import::Source::Codex,
                    )))
                }
                KeyCode::Enter if !entering => entering = true,
                KeyCode::Enter if !path.trim().is_empty() => {
                    return Ok(Some(Choice::Path(expand_path(path.trim()))))
                }
                KeyCode::Backspace if entering => {
                    path.pop();
                }
                KeyCode::Char(c)
                    if entering
                        && !key.modifiers.intersects(
                            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                        )
                        && path.len() < 8192 =>
                {
                    path.push(c)
                }
                _ => (),
            },
            Some(Ok(Event::Paste(text))) if entering && path.len() + text.len() < 8192 => {
                path.push_str(&text)
            }
            Some(Ok(Event::Resize(w, h))) => renderer.resize(w.into(), h.into()),
            Some(Err(e)) => return Err(e),
            None => return Ok(None),
            _ => (),
        }
    }
}

/// Expand a home-relative path, leaving spaces intact.
pub fn expand_path(path: &str) -> PathBuf {
    let path = path.trim();
    let path = if path.len() >= 2
        && ((path.starts_with('"') && path.ends_with('"'))
            || (path.starts_with('\'') && path.ends_with('\'')))
    {
        &path[1..path.len() - 1]
    } else {
        path
    };
    if path == "~" {
        return crate::platform::home_dir().unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = crate::platform::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn menu_and_path_input_separate_content_from_heading_and_hints() {
        for entering in [false, true] {
            let lines = rows(0, entering, "chat.jsonl", 100, 18, ColorMode::None);
            assert!(lines[1].is_empty());
            if entering {
                assert!(lines[2].contains("Путь:"));
                assert!(lines[3].is_empty());
            } else {
                assert!(lines[2].contains("› 1. Claude Code"));
                assert!(lines[5].is_empty());
            }
            assert!(lines.last().unwrap().contains("enter"));
        }
    }

    #[test]
    fn compact_menu_keeps_selection_and_spacing_when_there_is_room() {
        for width in [38, 80] {
            for height in 3..=12 {
                for selected in 0..3 {
                    let lines = rows(selected, false, "", width, height, ColorMode::None);
                    assert!(lines.len() <= height);
                    assert!(lines.iter().all(|line| line.width() <= width));
                    assert!(lines
                        .iter()
                        .any(|line| line.contains(&format!("› {}.", selected + 1))));
                    assert!(lines.last().unwrap().contains("enter"));
                    if height >= 5 {
                        assert!(lines[1].is_empty());
                        assert!(lines[lines.len() - 2].is_empty());
                    }
                }
            }
        }
    }
}
