//! Local palette selection with live preview and explicit confirmation.

use super::{
    picker,
    render::{Frame, Renderer},
};
use crate::theme::{self, ColorMode, RosePreset, Theme, ThemeChoice, WhiteMode};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use std::io;

fn rows(
    choice: ThemeChoice,
    selected: usize,
    width: usize,
    height: usize,
    mode: ColorMode,
) -> Vec<String> {
    let theme = Theme::from_choice(choice);
    let mut out = vec![
        picker::heading("  Тема терминала", width, &theme, mode),
        String::new(),
    ];
    let capacity = height.saturating_sub(5).max(1);
    let start = (selected + 1).saturating_sub(capacity);
    for (index, preset) in RosePreset::ALL
        .iter()
        .enumerate()
        .skip(start)
        .take(capacity)
    {
        let label = format!(
            "{}. {}{}",
            index + 1,
            preset.label(),
            if *preset == choice.preset {
                " · выбрана"
            } else {
                ""
            }
        );
        out.push(picker::choice(
            index == selected,
            &label,
            "",
            width,
            &theme,
            mode,
        ));
    }
    out.push(String::new());
    out.push(picker::text(
        &format!(
            "  Белый в diff: {}",
            match choice.white {
                WhiteMode::Soft => "мягкий",
                WhiteMode::Pure => "чистый",
            }
        ),
        width,
        theme.placeholder,
        mode,
    ));
    out.push(picker::text(
        "  ↑↓ тема   tab белый в diff   enter сохранить   esc назад",
        width,
        theme.placeholder,
        mode,
    ));
    picker::constrain(out, height)
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
            let lines = rows(
                choice,
                selected,
                width.saturating_sub(1),
                picker::height(height),
                renderer.color_mode(),
            );
            renderer.paint_styled(&Frame {
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
        let body = rows(ThemeChoice::default(), 0, 100, 20, ColorMode::None).join("\n");
        for preset in RosePreset::ALL {
            assert!(body.contains(preset.label()));
        }
        assert!(body.contains("чистый"));
        let soft = ThemeChoice {
            white: WhiteMode::Soft,
            ..ThemeChoice::default()
        };
        assert!(rows(soft, 0, 100, 20, ColorMode::None)
            .join("\n")
            .contains("мягкий"));
        let pure = ThemeChoice {
            white: WhiteMode::Pure,
            ..ThemeChoice::default()
        };
        assert!(rows(pure, 0, 100, 20, ColorMode::None)
            .join("\n")
            .contains("чистый"));
    }
}
