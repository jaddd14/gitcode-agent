//! Ledger-backed usage periods with deterministic UTC day boundaries.

use super::widgets::{tabs, vertical_bar_chart, wrap_text, Glyphs, Table};
use crate::usage::{UsageEntry, UsageSummary};
use std::collections::BTreeMap;

/// Period names in display order.
pub const PERIODS: [&str; 5] = ["Сессия", "Сегодня", "7 дней", "30 дней", "Всё время"];

/// Render a selected period using only recorded usage, with no invented prices or limits.
pub fn render(
    entries: &[UsageEntry],
    session: &str,
    now: u64,
    period: usize,
    cost_chart: bool,
    width: usize,
) -> Vec<String> {
    let day = 86_400_000;
    let today = now / day;
    let filtered = entries
        .iter()
        .filter(|e| {
            e.timestamp_ms <= now
                && match period {
                    0 => e.session_id == session,
                    1 => e.timestamp_ms / day == today,
                    2 => e.timestamp_ms / day >= today.saturating_sub(6),
                    3 => e.timestamp_ms / day >= today.saturating_sub(29),
                    _ => true,
                }
        })
        .cloned()
        .collect::<Vec<_>>();
    let summary = UsageSummary::from_entries(&filtered);
    let inner = width.saturating_sub(2);
    let mut body = vec![String::new()];
    body.extend(wrap_text(&tabs(&PERIODS, period, usize::MAX), inner));
    body.push(String::new());
    if filtered.is_empty() {
        for line in [
            "В этом периоде пока нет запросов.",
            "Отправь сообщение модели, затем открой /usage.",
        ] {
            body.extend(wrap_text(line, inner));
        }
    }
    let estimated = if filtered.iter().any(|e| e.estimated) {
        "~"
    } else {
        ""
    };
    let cost = cost_label(&summary);
    let metrics = [
        format!(
            " Запросы: {} · Токены вход / выход: {estimated}{} / {estimated}{}",
            summary.requests, summary.input_tokens, summary.output_tokens
        ),
        format!(
            " Кэш чтение / запись: {} / {} · Попадания кэша: {:.1}%",
            summary.cache_read_tokens,
            summary.cache_write_tokens,
            summary.cache_hit_rate * 100.0
        ),
        format!(
            " Стоимость: {cost} · Средняя скорость: {:.1} ток/с",
            summary.avg_tokens_per_sec
        ),
    ];
    for metric in metrics {
        body.extend(wrap_text(&metric, inner));
    }
    if summary.unpriced_requests > 0 {
        body.extend(wrap_text(
            &format!(
                "Цена модели не задана: {} из {} запросов; стоимость неполная.",
                summary.unpriced_requests, summary.requests
            ),
            inner,
        ));
    }
    if filtered.is_empty() {
        return finish(body, width);
    }
    for (title, by_agent) in [("По моделям", false), ("По агентам", true)] {
        let mut groups = BTreeMap::<String, Vec<UsageEntry>>::new();
        for entry in &filtered {
            groups
                .entry(if by_agent {
                    entry.agent.clone()
                } else {
                    entry.model.clone()
                })
                .or_default()
                .push(entry.clone());
        }
        let rows = groups
            .into_iter()
            .map(|(name, group)| {
                let s = UsageSummary::from_entries(&group);
                vec![
                    name,
                    s.requests.to_string(),
                    s.input_tokens.to_string(),
                    s.output_tokens.to_string(),
                    cost_label(&s),
                ]
            })
            .collect::<Vec<_>>();
        body.push(String::new());
        body.push(title.into());
        body.extend(
            Table {
                headers: &["Имя", "Запросы", "Вход", "Выход", "Стоимость"],
                rows: &rows,
                numeric: &[1, 2, 3, 4],
            }
            .render(inner),
        );
    }
    body.push(String::new());
    body.push(
        if cost_chart {
            "За 14 дней · стоимость, USD"
        } else {
            "За 14 дней · токены"
        }
        .into(),
    );
    let days = (today.saturating_sub(13)..=today)
        .map(|date| {
            let daily = filtered
                .iter()
                .filter(|e| e.timestamp_ms / day == date)
                .cloned()
                .collect::<Vec<_>>();
            let summary = UsageSummary::from_entries(&daily);
            let mut label = utc_date(date);
            let value = if cost_chart {
                if summary.unpriced_requests > 0 {
                    label.push_str(" (неполная)");
                }
                if summary.requests > 0 && summary.unpriced_requests == summary.requests {
                    f64::NAN
                } else {
                    summary.known_cost_usd
                }
            } else {
                summary.input_tokens as f64 + summary.output_tokens as f64
            };
            (label, value)
        })
        .collect::<Vec<_>>();
    body.extend(vertical_bar_chart(
        &days
            .iter()
            .map(|(label, value)| (label.as_str(), *value))
            .collect::<Vec<_>>(),
        inner,
        4,
        Glyphs::detect(),
    ));
    body.extend(wrap_text(
        "Лимиты: Провайдер не передал заголовки лимитов.",
        inner,
    ));
    finish(body, width)
}

fn finish(body: Vec<String>, width: usize) -> Vec<String> {
    let mut rows = vec![super::clip("  Использование", width)];
    rows.extend(
        body.into_iter()
            .map(|row| super::clip(&format!("  {row}"), width)),
    );
    rows.push(String::new());
    rows.push(super::clip(
        "  esc закрыть   ↑↓ прокрутка   ←→ период   t график",
        width,
    ));
    rows
}

fn cost_label(summary: &UsageSummary) -> String {
    if summary.requests == 0 || summary.unpriced_requests == summary.requests {
        "—".into()
    } else {
        format!(
            "{}${:.4}",
            if summary.unpriced_requests > 0 {
                "≥"
            } else {
                ""
            },
            summary.known_cost_usd
        )
    }
}

// Civil date conversion from Unix days, using the Gregorian 400-year era.
fn utc_date(days: u64) -> String {
    let z = days as i128 + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i128::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Fit the panel to a terminal height, pinning its title and footer while scrolling its body.
/// `scroll` is a body-row offset, clamped after resizes; heights zero through two are safe.
pub fn viewport(rows: &[String], height: usize, scroll: &mut usize) -> Vec<String> {
    if rows.len() <= height {
        *scroll = 0;
        return rows.to_vec();
    }
    if height == 0 || rows.is_empty() {
        *scroll = 0;
        return vec![];
    }
    let footer = rows.len().saturating_sub(1);
    if height == 1 || footer == 0 {
        *scroll = 0;
        return vec![rows[0].clone()];
    }
    let capacity = height - 2;
    let body_len = footer - 1;
    *scroll = (*scroll).min(body_len.saturating_sub(capacity));
    let mut visible = vec![rows[0].clone()];
    visible.extend(rows.iter().skip(1 + *scroll).take(capacity).cloned());
    visible.push(rows[footer].clone());
    visible
}
