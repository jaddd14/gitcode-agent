//! Scrollable, searchable project session selector.
use super::{
    picker,
    render::{Frame, Renderer},
    widgets,
};
use crate::{
    session::store::SessionSummary,
    theme::{ColorMode, Theme},
};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use std::{
    io,
    time::{SystemTime, UNIX_EPOCH},
};

fn age(timestamp: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    let minutes = now.saturating_sub(timestamp) / 60_000;
    match minutes {
        0 => "только что".into(),
        1..=59 => format!("{minutes} мин назад"),
        60..=1439 => format!("{} ч назад", minutes / 60),
        _ => format!("{} дн назад", minutes / 1440),
    }
}

fn safe(value: &str) -> String {
    super::plain(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn filtered(sessions: &[SessionSummary], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    sessions
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let matches = query.is_empty()
                || safe(&item.metadata.title).to_lowercase().contains(&query)
                || safe(item.metadata.branch.as_deref().unwrap_or(""))
                    .to_lowercase()
                    .contains(&query)
                || item.id.to_lowercase().contains(&query);
            matches.then_some(index)
        })
        .collect()
}

fn rows(
    sessions: &[SessionSummary],
    results: &[usize],
    selected: usize,
    start: usize,
    query: &str,
    width: usize,
    height: usize,
    mode: ColorMode,
) -> Vec<String> {
    let theme = Theme::default();
    let position = if results.is_empty() {
        "0 / 0".to_owned()
    } else {
        format!("{} / {}", selected + 1, results.len())
    };
    let mut out = vec![
        picker::heading(
            &format!("  Восстановить чат · {position}"),
            width,
            &theme,
            mode,
        ),
        picker::text(
            &format!(
                "  Поиск: {}",
                if query.is_empty() {
                    "начните печатать название, ветку или ID"
                } else {
                    query
                }
            ),
            width,
            theme.panel_text,
            mode,
        ),
    ];
    let capacity = height.saturating_sub(4).max(1);
    if results.is_empty() {
        out.push(picker::text(
            "  Ничего не найдено",
            width,
            theme.placeholder,
            mode,
        ));
    } else {
        for (position, index) in results.iter().enumerate().skip(start).take(capacity) {
            let item = &sessions[*index];
            let title = safe(&item.metadata.title);
            let info = format!(
                "{} · {} сообщ.",
                age(item.metadata.updated_at_ms),
                item.message_count
            );
            let primary_width = if width >= 65 {
                width
                    .saturating_sub(3 + 3 + unicode_width::UnicodeWidthStr::width(info.as_str()))
                    .max(8)
            } else {
                width.saturating_sub(3)
            };
            let title = widgets::ellipsis(&title, primary_width, false);
            let primary = if width >= 65 {
                widgets::fit(&title, primary_width)
            } else {
                title
            };
            let description = if width >= 65 {
                format!("   {info}")
            } else {
                String::new()
            };
            out.push(picker::choice(
                position == selected,
                &primary,
                &description,
                width,
                &theme,
                mode,
            ));
        }
    }
    if let Some(item) = results.get(selected).and_then(|index| sessions.get(*index)) {
        let branch = safe(item.metadata.branch.as_deref().unwrap_or("—"));
        let details = format!(
            "  {} · {} сообщ. · ветка {} · ID {}{}",
            age(item.metadata.created_at_ms),
            item.message_count,
            branch,
            item.id,
            if item.interrupted_bytes > 0 {
                " · есть незавершённый хвост"
            } else {
                ""
            }
        );
        out.push(picker::text(&details, width, theme.placeholder, mode));
    } else {
        out.push(String::new());
    }
    out.push(picker::text(
        "  ↑↓ выбор   enter открыть   esc отмена",
        width,
        theme.placeholder,
        mode,
    ));
    picker::constrain(out, height)
}

fn move_selection(selected: usize, count: usize, key: KeyCode) -> usize {
    if count == 0 {
        return 0;
    }
    match key {
        KeyCode::Up => selected.saturating_sub(1),
        KeyCode::Down => (selected + 1).min(count - 1),
        KeyCode::PageUp => selected.saturating_sub(8),
        KeyCode::PageDown => (selected + 8).min(count - 1),
        _ => selected,
    }
}

fn scroll_start(selected: usize, start: usize, count: usize, capacity: usize) -> usize {
    let capacity = capacity.max(1);
    let start = start.min(count.saturating_sub(capacity));
    if selected < start {
        selected
    } else if selected >= start.saturating_add(capacity) {
        selected + 1 - capacity
    } else {
        start
    }
}

/// Pick a session identifier without changing the current conversation.
pub async fn select(
    renderer: &mut Renderer<io::Stdout>,
    events: &mut EventStream,
    sessions: &[SessionSummary],
) -> io::Result<Option<String>> {
    if sessions.is_empty() {
        return Ok(None);
    }
    let mut selected = 0;
    let mut start = 0;
    let mut query = String::new();
    loop {
        let results = filtered(sessions, &query);
        selected = selected.min(results.len().saturating_sub(1));
        let (width, height) = renderer.size();
        let menu_height = picker::height(height);
        start = scroll_start(
            selected,
            start,
            results.len(),
            menu_height.saturating_sub(4),
        );
        renderer.paint_styled(&Frame {
            lines: rows(
                sessions,
                &results,
                selected,
                start,
                &query,
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
                KeyCode::Enter => {
                    if let Some(index) = results.get(selected) {
                        return Ok(Some(sessions[*index].id.clone()));
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    selected = 0;
                    start = 0;
                }
                KeyCode::Char(c)
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) && query.chars().count() < 128 =>
                {
                    query.push(c);
                    selected = 0;
                    start = 0;
                }
                code => selected = move_selection(selected, results.len(), code),
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
    use crate::session::store::Metadata;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn selection_scrolls_filters_and_shows_selected_session_metadata() {
        let sessions: Vec<_> = (0..15)
            .map(|i| SessionSummary {
                id: format!("session-{i}"),
                metadata: Metadata {
                    title: format!("Чат {i}"),
                    created_at_ms: 1,
                    updated_at_ms: 1,
                    branch: Some("main".into()),
                    forked_from: None,
                },
                message_count: i,
                interrupted_bytes: 0,
            })
            .collect();
        let mut index = 0;
        for _ in 0..12 {
            index = move_selection(index, sessions.len(), KeyCode::Down);
        }
        let results = filtered(&sessions, "");
        for width in [38, 80, 120] {
            let lines = rows(
                &sessions,
                &results,
                index,
                scroll_start(index, 0, results.len(), 8),
                "",
                width,
                18,
                ColorMode::None,
            );
            let visible = lines.join("\n");
            assert!(visible.contains("Чат 12"));
            if width >= 80 {
                assert!(visible.contains("session-12"));
                assert!(visible.contains("main"));
            }
            assert!(!visible.contains('╭'));
            assert!(lines.iter().all(|line| line.width() <= width));
        }
        assert_eq!(filtered(&sessions, "ЧАТ 12"), vec![12]);
        assert_eq!(filtered(&sessions, "session-9"), vec![9]);
        assert!(filtered(&sessions, "несуществующий").is_empty());
        assert_eq!(move_selection(index, sessions.len(), KeyCode::Up), 11);
        assert_eq!(move_selection(0, sessions.len(), KeyCode::Up), 0);
        assert_eq!(scroll_start(12, 0, 15, 8), 5);
        assert_eq!(scroll_start(11, 5, 15, 8), 5);
        assert_eq!(scroll_start(4, 5, 15, 8), 4);
        for terminal_height in [15, 20, 25, 40] {
            let limit = picker::height(terminal_height);
            let start = scroll_start(index, 0, results.len(), limit.saturating_sub(4));
            let compact = rows(
                &sessions,
                &results,
                index,
                start,
                "",
                80,
                limit,
                ColorMode::None,
            );
            assert!(compact.len() <= limit);
            assert!(compact.iter().any(|line| line.contains("› Чат 12")));
        }
    }
}
