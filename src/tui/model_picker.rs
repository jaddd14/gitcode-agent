//! Keyboard-driven model selection without network requests.

use super::{
    render::{Frame, Renderer},
    widgets,
};
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use futures_util::StreamExt;
use std::io;

/// Select a model; cancellation preserves the current selection.
pub async fn select(
    renderer: &mut Renderer<io::Stdout>,
    events: &mut EventStream,
    current: &str,
) -> io::Result<Option<String>> {
    let mut models = vec![current.to_owned()];
    for model in ["claude-sonnet-4-20250514", "gpt-4.1", "deepseek-chat"] {
        if !models.iter().any(|m| m == model) {
            models.push(model.into());
        }
    }
    let mut selected = 0;
    loop {
        let (width, height) = renderer.size();
        let body = widgets::list(
            &models,
            selected,
            height.saturating_sub(5).max(1),
            width.saturating_sub(3),
        );
        let rows = widgets::card(
            "Модель",
            &body,
            "↑↓ выбор · enter применить · esc отмена",
            width.saturating_sub(1),
        );
        renderer.paint(&Frame {
            lines: rows,
            cursor: None,
        })?;
        match events.next().await {
            Some(Ok(Event::Key(key))) => match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None)
                }
                KeyCode::Up => selected = (selected + models.len() - 1) % models.len(),
                KeyCode::Down => selected = (selected + 1) % models.len(),
                KeyCode::Enter => return Ok(Some(models[selected].clone())),
                _ => {}
            },
            Some(Ok(Event::Resize(w, h))) => renderer.resize(w.into(), h.into()),
            Some(Err(error)) => return Err(error),
            None => return Ok(None),
            _ => {}
        }
    }
}
