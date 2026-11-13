//! Searchable, paginated model selection backed by the current provider.
use super::{
    picker,
    render::{Frame, Renderer},
};
use crate::{
    provider::transport::Client,
    theme::{ColorMode, Theme},
};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use std::io;

fn filter(models: &[String], query: &str) -> Vec<usize> {
    let query = query.to_lowercase();
    models
        .iter()
        .enumerate()
        .filter_map(|(i, model)| model.to_lowercase().contains(&query).then_some(i))
        .collect()
}

fn rows(
    models: &[String],
    results: &[usize],
    current: &str,
    query: &str,
    selected: usize,
    width: usize,
    height: usize,
    mode: ColorMode,
) -> Vec<String> {
    let theme = Theme::default();
    let capacity = height.saturating_sub(5).max(1);
    let pages = results.len().max(1).div_ceil(capacity);
    let page = (selected / capacity).min(pages - 1);
    let mut out = vec![
        picker::heading(
            &format!(
                "  Выбор модели · страница {} / {} · {} моделей",
                page + 1,
                pages,
                results.len()
            ),
            width,
            &theme,
            mode,
        ),
        picker::text(
            &format!(
                "  Поиск: {}",
                if query.is_empty() {
                    "начните печатать название"
                } else {
                    query
                }
            ),
            width,
            theme.panel_text,
            mode,
        ),
        String::new(),
    ];
    if results.is_empty() {
        out.push(picker::text(
            "  Модели не найдены",
            width,
            theme.placeholder,
            mode,
        ));
    } else {
        for (position, index) in results
            .iter()
            .enumerate()
            .skip(page * capacity)
            .take(capacity)
        {
            let model = &models[*index];
            let label = format!(
                "{}. {}{}",
                position + 1,
                super::plain(model),
                if model == current {
                    " (текущая)"
                } else {
                    ""
                }
            );
            out.push(picker::choice(
                position == selected,
                &label,
                "",
                width,
                &theme,
                mode,
            ));
        }
    }
    out.push(String::new());
    out.push(picker::text(
        "  ↑↓ выбор   PgUp/PgDn страницы   enter применить   esc назад",
        width,
        theme.placeholder,
        mode,
    ));
    picker::constrain(out, height)
}

/// Fetch and select a model; cancellation preserves the current selection.
pub async fn select(
    renderer: &mut Renderer<io::Stdout>,
    events: &mut EventStream,
    current: &str,
    client: &Client,
) -> io::Result<Option<String>> {
    let models = 'fetch: loop {
        let (width, _) = renderer.size();
        let theme = Theme::default();
        renderer.paint_styled(&Frame {
            lines: vec![
                picker::heading(
                    "  Выбор модели",
                    width.saturating_sub(1),
                    &theme,
                    renderer.color_mode(),
                ),
                String::new(),
                picker::text(
                    "  Загружаем модели провайдера…",
                    width.saturating_sub(1),
                    theme.panel_text,
                    renderer.color_mode(),
                ),
                String::new(),
                picker::text(
                    "  esc назад",
                    width.saturating_sub(1),
                    theme.placeholder,
                    renderer.color_mode(),
                ),
            ],
            cursor: None,
        })?;
        let request = client.list_models();
        tokio::pin!(request);
        let result = loop {
            tokio::select! {
                result = &mut request => break result,
                event = events.next() => match event {
                    Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release && (key.code == KeyCode::Esc || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))) => return Ok(None),
                    Some(Ok(Event::Resize(w, h))) => renderer.resize(w.into(), h.into()),
                    Some(Err(error)) => return Err(error),
                    None => return Ok(None),
                    _ => {}
                }
            }
        };
        match result {
            Ok(models) => break 'fetch models,
            Err(message) => loop {
                let (width, _) = renderer.size();
                let theme = Theme::default();
                renderer.paint_styled(&Frame {
                    lines: vec![
                        picker::heading(
                            "  Выбор модели",
                            width.saturating_sub(1),
                            &theme,
                            renderer.color_mode(),
                        ),
                        String::new(),
                        picker::text(
                            &format!("  Не удалось загрузить модели: {message}"),
                            width.saturating_sub(1),
                            theme.panel_text,
                            renderer.color_mode(),
                        ),
                        String::new(),
                        picker::text(
                            "  r повторить   esc назад",
                            width.saturating_sub(1),
                            theme.placeholder,
                            renderer.color_mode(),
                        ),
                    ],
                    cursor: None,
                })?;
                match events.next().await {
                    Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                        match key.code {
                            KeyCode::Char('r' | 'R' | 'к' | 'К') => continue 'fetch,
                            KeyCode::Esc => return Ok(None),
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                return Ok(None)
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(Event::Resize(w, h))) => renderer.resize(w.into(), h.into()),
                    Some(Err(error)) => return Err(error),
                    None => return Ok(None),
                    _ => {}
                }
            },
        }
    };
    let mut query = String::new();
    let mut selected = models.iter().position(|m| m == current).unwrap_or(0);
    loop {
        let results = filter(&models, &query);
        selected = selected.min(results.len().saturating_sub(1));
        let (width, height) = renderer.size();
        let menu_height = picker::height(height);
        let capacity = menu_height.saturating_sub(5).max(1);
        renderer.paint_styled(&Frame {
            lines: rows(
                &models,
                &results,
                current,
                &query,
                selected,
                width.saturating_sub(1),
                menu_height,
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
                KeyCode::Up => selected = selected.saturating_sub(1),
                KeyCode::Down => selected = (selected + 1).min(results.len().saturating_sub(1)),
                KeyCode::PageUp | KeyCode::Left => selected = selected.saturating_sub(capacity),
                KeyCode::PageDown | KeyCode::Right => {
                    selected = (selected + capacity).min(results.len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(index) = results.get(selected) {
                        return Ok(Some(models[*index].clone()));
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    selected = 0;
                }
                KeyCode::Char(c)
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) && query.chars().count() < 128 =>
                {
                    query.push(c);
                    selected = 0;
                }
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
    #[test]
    fn large_catalog_is_searchable_and_each_page_keeps_the_selection_visible() {
        let models: Vec<_> = (0..400).map(|i| format!("model-{i:03}")).collect();
        let results = filter(&models, "");
        for selected in [0, 4, 5, 120, 399] {
            let output = rows(
                &models,
                &results,
                "model-120",
                "",
                selected,
                80,
                10,
                ColorMode::None,
            );
            assert!(output.len() <= 10);
            assert!(output.join("\n").contains(&format!(
                "› {}. {}",
                selected + 1,
                models[selected]
            )));
            assert!(!output.join("\n").contains('╭'));
        }
        assert_eq!(filter(&models, "MODEL-399"), vec![399]);
        assert!(filter(&models, "absent").is_empty());
    }
}
