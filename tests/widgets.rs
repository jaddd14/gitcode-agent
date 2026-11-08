//! Committed cell snapshots for every reusable widget across supported widths.
#![allow(clippy::unwrap_used)]
use gitcode::tui::widgets::*;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

fn snapshot(width: usize, ascii: bool) -> String {
    let glyphs = Glyphs { ascii };
    let mut rows = Frame {
        title: "Frame",
        footer: "DEFAULT",
        glyphs,
    }
    .render(&["body 界".into()], width);
    rows.push(chip("Chip"));
    rows.push(keycap("esc"));
    rows.push(key_hints(&[("esc", "close"), ("tab", "select")], width));
    rows.push(progress(0.73, width, glyphs));
    rows.extend(
        Table {
            headers: &["Name", "Count"],
            rows: &[vec!["very long model 界".into(), "1200".into()]],
            numeric: &[1],
        }
        .render(width),
    );
    rows.push(tabs(&["Session", "Today", "Week"], 1, width));
    rows.extend(bar_chart(
        &[("day 1", 0.0), ("day 2", 42.0), ("day 3", 21.0)],
        width,
        glyphs,
    ));
    rows.push(sparkline(&[0, 1, 2, 4, 8], width, glyphs));
    rows.extend(list(
        &["one".into(), "two".into(), "three".into()],
        2,
        2,
        width,
    ));
    rows.push(spinner(Duration::from_millis(84), glyphs).to_string());
    rows.extend(card_with_glyphs(
        "Card",
        &["Body".into()],
        "esc",
        width,
        glyphs,
    ));
    rows.push(divider(width, glyphs));
    let now = Instant::now();
    rows.push(
        Toast {
            message: "Saved".into(),
            created: now,
        }
        .line(now, width)
        .unwrap(),
    );
    assert!(rows.iter().all(|s| s.width() <= width));
    rows.join("\n") + "\n"
}

#[test]
fn committed_widget_snapshots() {
    for width in [50, 80, 100, 120, 200] {
        for ascii in [false, true] {
            let name = format!(
                "{}/testdata/ui/widgets-{width}-{}.txt",
                env!("CARGO_MANIFEST_DIR"),
                if ascii { "ascii" } else { "unicode" }
            );
            let actual = snapshot(width, ascii);
            if std::env::var_os("GITCODE_UPDATE_SNAPSHOTS").is_some() {
                std::fs::create_dir_all(std::path::Path::new(&name).parent().unwrap()).unwrap();
                std::fs::write(&name, &actual).unwrap();
            }
            assert_eq!(std::fs::read_to_string(&name).unwrap(), actual, "{name}");
        }
    }
}

#[test]
fn wrapped_text_preserves_words_and_graphemes() {
    assert_eq!(wrap_text("один два три", 7), ["один", "два три"]);
    assert_eq!(wrap_text("123456789", 4), ["1234", "5678", "9"]);
    assert_eq!(wrap_text("界界 e\u{301}", 4), ["界界", "e\u{301}"]);
    assert!(wrap_text("text", 0).is_empty());
    assert_eq!(wrap_text("one\ntwo", 4), ["one", "two"]);
}

#[test]
fn vertical_chart_keeps_all_exact_values_in_accessible_text() {
    for ascii in [false, true] {
        for width in [20, 40, 60, 100] {
            let values = [
                ("2024-02-29", 12345.0),
                ("2024-03-01", 0.0000125),
                ("2024-03-02", f64::NAN),
            ];
            let rows = vertical_bar_chart(&values, width, 4, Glyphs { ascii });
            assert!(rows.iter().all(|row| row.width() <= width));
            let text = rows.join("\n");
            for expected in [
                "2024-02-29:",
                "12345",
                "2024-03-01:",
                "0.0000125",
                "2024-03-02:",
            ] {
                assert!(
                    text.contains(expected),
                    "missing {expected} at {width}: {text}"
                );
            }
            assert!(text.contains("—"));
        }
    }
    let values = (0..30).map(|_| ("day", 7.5)).collect::<Vec<_>>();
    let rows = vertical_bar_chart(&values, 20, 4, Glyphs { ascii: true });
    assert_eq!(rows.join("\n").matches("day:").count(), 30);
}

#[test]
fn toast_expires_and_spinner_respects_twelve_fps() {
    let now = Instant::now();
    let toast = Toast {
        message: "saved".into(),
        created: now,
    };
    assert!(toast.line(now + Duration::from_secs(2), 80).is_none());
    assert_eq!(
        spinner(Duration::ZERO, Glyphs { ascii: true }),
        spinner(Duration::from_millis(83), Glyphs { ascii: true })
    );
}
