//! Pure, isolated ledger fixtures for responsive usage rendering.
use gitcode::tui::usage_panel::{render, viewport};
use gitcode::usage::UsageEntry;
use unicode_width::UnicodeWidthStr;

const DAY: u64 = 86_400_000;

fn entry(day: u64, cost: Option<f64>) -> UsageEntry {
    UsageEntry {
        timestamp_ms: day * DAY,
        session_id: "fixture".into(),
        project: "/fixture".into(),
        provider: "fixture".into(),
        model: "model".into(),
        agent: "main".into(),
        input_tokens: 123456,
        output_tokens: 98765,
        cache_read_tokens: 222222,
        cache_write_tokens: 333333,
        reasoning_tokens: 0,
        estimated: true,
        cost_usd: cost,
        latency_ms: 1000,
        tokens_per_sec: 98765.0,
    }
}

#[test]
fn metrics_wrap_without_clipping_at_sixty_columns() {
    let rows = render(
        &[entry(20000, Some(0.125))],
        "fixture",
        20000 * DAY,
        0,
        false,
        60,
    );
    assert!(rows.iter().all(|row| row.width() <= 60));
    let text = rows.join("\n");
    for value in [
        "~123456", "~98765", "222222", "333333", "98765.0", "$0.1250",
    ] {
        assert!(text.contains(value), "missing {value}: {text}");
    }
    assert!(text.contains("Попадания кэша:"));
    assert!(text.contains("Лимиты:"));
}

#[test]
fn dates_are_utc_and_cross_leap_day_and_year_boundaries() {
    for (day, expected) in [
        (0, "1970-01-01"),
        (19782, "2024-02-29"),
        (19723, "2024-01-01"),
    ] {
        let text = render(&[], "fixture", day * DAY, 4, false, 60).join("\n");
        assert!(text.contains(expected), "missing {expected}: {text}");
        assert!(!text.contains("д1"));
    }
}

#[test]
fn chart_has_exact_values_and_unknown_cost_is_not_free() {
    let entries = [entry(20000, Some(0.125)), entry(20000, None)];
    let rows = render(&entries, "fixture", 20000 * DAY, 4, true, 60);
    let text = rows.join("\n");
    assert!(text.contains("≥$0.1250"));
    assert!(text.contains("1 из 2 запросов"));
    assert!(text.contains("неполная"));
    assert!(text.contains("0.125"));
    let unknown = render(&[entry(20000, None)], "fixture", 20000 * DAY, 0, true, 60).join("\n");
    assert!(!unknown.contains("$0.0000"));
    assert!(unknown.contains("(неполная): —"));
    let tokens = render(&entries, "fixture", 20000 * DAY, 0, false, 60).join("\n");
    assert!(tokens.contains("444442"));
}

#[test]
fn all_periods_and_empty_states_fit_small_viewports_with_pinned_footer() {
    for entries in [vec![], vec![entry(20000, None)]] {
        for period in 0..5 {
            for cost_chart in [false, true] {
                let rows = render(&entries, "fixture", 20000 * DAY, period, cost_chart, 40);
                assert!(rows.iter().all(|row| row.width() <= 40));
                for height in [0, 1, 2, 11, 12, 1000] {
                    let mut scroll = usize::MAX;
                    let visible = viewport(&rows, height, &mut scroll);
                    assert!(visible.len() <= height);
                    if height >= 2 && rows.len() > height {
                        assert_eq!(visible.first(), rows.first());
                        assert_eq!(visible.last(), rows.get(rows.len() - 2));
                        assert!(visible
                            .last()
                            .is_some_and(|row| row.contains("esc закрыть")));
                    }
                }
                let mut scroll = 0;
                let mut visited = Vec::new();
                loop {
                    let visible = viewport(&rows, 12, &mut scroll);
                    visited.extend(visible);
                    let next = scroll + 1;
                    let mut clamped = next;
                    viewport(&rows, 12, &mut clamped);
                    if clamped < next {
                        break;
                    }
                    scroll = next;
                }
                assert!(rows
                    .iter()
                    .filter(|row| !row.is_empty())
                    .all(|row| visited.contains(row)));
            }
        }
    }
}

#[test]
fn viewport_handles_unframed_and_short_rows_without_panics() {
    for rows in [
        vec![],
        vec!["one".into()],
        vec!["title".into(), "footer".into()],
    ] {
        for height in 0..4 {
            let mut scroll = usize::MAX;
            assert!(viewport(&rows, height, &mut scroll).len() <= height);
        }
    }
}
