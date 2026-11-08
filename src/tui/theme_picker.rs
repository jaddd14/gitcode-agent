//! Local palette selection with live preview and explicit confirmation.

use super::{
    render::{Frame, Renderer},
    widgets,
};
use crate::theme::{self, RosePreset, Theme, ThemeChoice, WhiteMode};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use std::io;

fn rows(choice: ThemeChoice, selected: usize, width: usize) -> Vec<String> {
    let mut body = vec!["Тема терминала".into(), String::new()];
    for (index, preset) in RosePreset::ALL.iter().enumerate() {
        body.push(format!(
            "{} {}{}",
            if index == selected { "›" } else { " " },
            preset.label(),
            if *preset == choice.preset {
                " · выбрана"
            } else {
                ""
            }
        ));
    }
    body.push(String::new());
    body.push(format!(
        "Белый текст: {}",
        match choice.white {
            WhiteMode::Soft => "мягкий",
            WhiteMode::Pure => "чистый",
        }
    ));
    body.push("↑↓ тема · tab/←→ белый · enter сохранить · esc отмена".into());
    body.into_iter()
        .map(|line| widgets::fit(&line, width))
        .collect()
}

/// Preview choices immediately; cancellation restores the previous runtime palette.
pub async fn select(
    renderer: &mut Renderer<io::Stdout>,
    events: &mut EventStream,
) -> io::Result<Option<ThemeChoice>> {
    let original = theme::active_choice();
    let mut choice = original;
    let mut selected = RosePreset::ALL
        .iter()
        .position(|preset| *preset == choice.preset)
        .unwrap_or(0);
    let result = async {
        loop {
            renderer.set_theme(Theme::from_choice(choice));
            let (width, height) = renderer.size();
            let lines = rows(choice, selected, width.saturating_sub(1))
                .into_iter()
                .take(height)
                .collect();
            renderer.paint(&Frame {
                lines,
                cursor: None,
            })?;
            match events.next().await {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(None)
                    }
                    KeyCode::Up => selected = (selected + 3) % 4,
                    KeyCode::Down => selected = (selected + 1) % 4,
                    KeyCode::Tab | KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                        choice.white = match choice.white {
                            WhiteMode::Soft => WhiteMode::Pure,
                            WhiteMode::Pure => WhiteMode::Soft,
                        }
                    }
                    KeyCode::Enter => return Ok(Some(choice)),
                    _ => {}
                },
                Some(Ok(Event::Resize(w, h))) => renderer.resize(w.into(), h.into()),
                Some(Err(error)) => return Err(error),
                None => return Ok(None),
                _ => {}
            }
            choice.preset = RosePreset::ALL[selected];
        }
    }
    .await;
    renderer.set_theme(Theme::from_choice(original));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exposes_four_presets_and_white_mode() {
        let body = rows(ThemeChoice::default(), 0, 100).join("\n");
        for preset in RosePreset::ALL {
            assert!(body.contains(preset.label()));
        }
        assert!(body.contains("мягкий"));
        let pure = ThemeChoice {
            white: WhiteMode::Pure,
            ..ThemeChoice::default()
        };
        assert!(rows(pure, 0, 100).join("\n").contains("чистый"));
    }
}
