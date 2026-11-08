//! Bounded tool lifecycle presentation with sanitized arguments and output.

use super::{plain, widgets};

/// Render one completed tool invocation without trusting terminal controls.
pub fn completed(
    name: &str,
    output: &str,
    is_error: bool,
    duration_ms: u64,
    width: usize,
) -> Vec<String> {
    let glyphs = widgets::Glyphs::detect();
    let mark = match (is_error, glyphs.ascii) {
        (true, true) => "x",
        (true, false) => "✗",
        (false, true) => "+",
        (false, false) => "✓",
    };
    let clean = plain(output);
    let mut body = clean.lines().take(3).map(str::to_owned).collect::<Vec<_>>();
    let remaining = clean.lines().count().saturating_sub(3);
    if remaining > 0 {
        body.push(format!("… ещё {remaining} строк · Ctrl+O"));
    }
    widgets::card(
        &format!("{mark} {} · {duration_ms} мс", plain(name)),
        &body,
        "ctrl+o расшифровка",
        width,
    )
}
