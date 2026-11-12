//! Width-safe, monochrome-readable building blocks for panels and overlays.

use super::{ellipsis, fit, Frame, Glyphs};
use crate::tui::clip;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Wrap plain text by terminal cells without dropping long words or splitting graphemes.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![];
    }
    let mut rows = Vec::new();
    for paragraph in crate::tui::plain(text).split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if !line.is_empty() && line.width() + 1 + word.width() <= width {
                line.push(' ');
                line.push_str(word);
                continue;
            }
            if !line.is_empty() {
                rows.push(std::mem::take(&mut line));
            }
            for grapheme in word.graphemes(true) {
                if line.width() + grapheme.width() > width && !line.is_empty() {
                    rows.push(std::mem::take(&mut line));
                }
                if grapheme.width() <= width {
                    line.push_str(grapheme);
                }
            }
        }
        rows.push(line);
    }
    rows
}

/// Small semantic label with a visible shape in monochrome.
pub fn chip(label: &str) -> String {
    format!("[{}]", crate::tui::plain(label))
}

/// Aligned table with bounded columns and explicit numeric alignment.
pub struct Table<'a> {
    /// Header labels.
    pub headers: &'a [&'a str],
    /// Cells in row order.
    pub rows: &'a [Vec<String>],
    /// Columns containing right-aligned numeric data.
    pub numeric: &'a [usize],
}
impl Table<'_> {
    /// Render headers and data without exceeding the available cells.
    pub fn render(&self, width: usize) -> Vec<String> {
        let n = self.headers.len();
        if n == 0 {
            return vec![];
        }
        let available = width.saturating_sub(n.saturating_sub(1));
        let mut widths = vec![available / n; n];
        for w in widths.iter_mut().take(available % n) {
            *w += 1;
        }
        let row = |cells: &[String]| {
            clip(
                &cells
                    .iter()
                    .enumerate()
                    .take(n)
                    .map(|(i, cell)| {
                        let value = ellipsis(cell, widths[i], false);
                        if self.numeric.contains(&i) {
                            format!(
                                "{}{value}",
                                " ".repeat(widths[i].saturating_sub(value.width()))
                            )
                        } else {
                            fit(&value, widths[i])
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                width,
            )
        };
        let mut rows = vec![row(&self
            .headers
            .iter()
            .map(|s| (*s).into())
            .collect::<Vec<_>>())];
        rows.extend(self.rows.iter().map(|cells| row(cells)));
        rows
    }
}

/// Tab strip whose selected state does not depend on color.
pub fn tabs(labels: &[&str], selected: usize, width: usize) -> String {
    clip(
        &labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                if i == selected {
                    format!("[{label}]")
                } else {
                    (*label).into()
                }
            })
            .collect::<Vec<_>>()
            .join(" · "),
        width,
    )
}

/// Scrollable selectable list with a stable, visible position indicator.
pub fn list(rows: &[String], selected: usize, capacity: usize, width: usize) -> Vec<String> {
    if rows.is_empty() || capacity == 0 {
        return vec![];
    }
    let selected = selected.min(rows.len() - 1);
    let start = (selected + 1).saturating_sub(capacity);
    let mut visible = rows
        .iter()
        .enumerate()
        .skip(start)
        .take(capacity)
        .map(|(i, row)| {
            fit(
                &format!("{} {row}", if i == selected { ">" } else { " " }),
                width,
            )
        })
        .collect::<Vec<_>>();
    visible.push(fit(&format!("{}/{}", selected + 1, rows.len()), width));
    visible
}

/// Horizontal magnitude chart; the numeric value remains the authoritative label.
pub fn bar_chart(values: &[(&str, f64)], width: usize, glyphs: Glyphs) -> Vec<String> {
    let max = values
        .iter()
        .map(|(_, n)| *n)
        .filter(|n| n.is_finite())
        .fold(0.0, f64::max);
    let label_width = (width / 3).min(16);
    let plot = width.saturating_sub(label_width + 12);
    values
        .iter()
        .map(|(label, value)| {
            let ratio = if max > 0.0 && value.is_finite() {
                (value / max).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let bar = (if glyphs.ascii { "#" } else { "█" })
                .repeat((ratio * plot as f64).round() as usize);
            clip(
                &format!("{} {bar} {value:.0}", fit(label, label_width)),
                width,
            )
        })
        .collect()
}

/// Vertical columns with a bottom label row and a wrapped, exact-value text alternative.
/// Non-finite values are unknown, never labeled as zero. `height` bounds the plot only.
pub fn vertical_bar_chart(
    values: &[(&str, f64)],
    width: usize,
    height: usize,
    glyphs: Glyphs,
) -> Vec<String> {
    if values.is_empty() || width == 0 || height == 0 {
        return vec![];
    }
    let plot_height = height.saturating_sub(1).max(1);
    let max = values
        .iter()
        .map(|(_, n)| *n)
        .filter(|n| n.is_finite())
        .fold(0.0, f64::max);
    let cols = values.len().min(width.div_ceil(2)).max(1);
    let col_width = ((width.saturating_sub(cols.saturating_sub(1))) / cols).clamp(1, 6);
    let blocks = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let mut rows = Vec::with_capacity(plot_height + 1);
    for row in (0..plot_height).rev() {
        let mut line = String::new();
        for (idx, (_, value)) in values.iter().take(cols).enumerate() {
            if idx > 0 {
                line.push(' ');
            }
            let ratio = if max > 0.0 && value.is_finite() {
                (value / max).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let units = ratio * plot_height as f64;
            let cell_fill = (units - row as f64).clamp(0.0, 1.0);
            let ch = if glyphs.ascii {
                if cell_fill >= 0.5 {
                    '#'
                } else {
                    ' '
                }
            } else {
                blocks[(cell_fill * 8.0).round() as usize]
            };
            line.push_str(&ch.to_string().repeat(col_width));
        }
        rows.push(fit(&line, width));
    }
    if height > 1 {
        let labels = values
            .iter()
            .take(cols)
            .map(|(label, _)| fit(&ellipsis(label, col_width, glyphs.ascii), col_width))
            .collect::<Vec<_>>()
            .join(" ");
        rows.push(fit(&labels, width));
    }
    let labels = values
        .iter()
        .map(|(label, value)| {
            let value = if value.is_finite() {
                value.to_string()
            } else {
                "—".into()
            };
            format!("{label}: {value}")
        })
        .collect::<Vec<_>>()
        .join(" · ");
    rows.extend(wrap_text(&labels, width));
    rows
}

/// Compact time-series magnitude marks with ASCII fallback.
pub fn sparkline(values: &[u64], width: usize, glyphs: Glyphs) -> String {
    let max = values.iter().copied().max().unwrap_or(0);
    let blocks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    values
        .iter()
        .take(width)
        .map(|n| {
            if glyphs.ascii {
                if *n == 0 {
                    '-'
                } else {
                    '#'
                }
            } else {
                blocks[if max == 0 {
                    0
                } else {
                    ((*n as f64 / max as f64) * 7.0) as usize
                }]
            }
        })
        .collect()
}

/// Monotonic spinner sampled at no more than twelve glyph changes per second.
pub fn spinner(elapsed: std::time::Duration, glyphs: Glyphs) -> char {
    let glyphs = if glyphs.ascii {
        ['|', '/', '-', '\\']
    } else {
        ['✻', '✿', '·', '✿']
    };
    glyphs[((elapsed.as_millis() / 84) % 4) as usize]
}

/// Quiet structural separator.
pub fn divider(width: usize, glyphs: Glyphs) -> String {
    (if glyphs.ascii { "-" } else { "─" }).repeat(width)
}

/// Scrollback card with consistent whitespace and footer conventions.
pub fn card(title: &str, body: &[String], footer: &str, width: usize) -> Vec<String> {
    card_with_glyphs(title, body, footer, width, Glyphs::detect())
}

/// Card with explicitly supplied capabilities for deterministic rendering.
pub fn card_with_glyphs(
    title: &str,
    body: &[String],
    footer: &str,
    width: usize,
    glyphs: Glyphs,
) -> Vec<String> {
    let padded = body
        .iter()
        .flat_map(|row| crate::tui::markdown::wrap(row, width.saturating_sub(4).max(1)))
        .map(|row| format!(" {row}"))
        .collect::<Vec<_>>();
    let mut rows = Frame {
        title,
        footer,
        glyphs,
    }
    .render(&padded, width);
    rows.push(String::new());
    rows
}

/// Overlay notification with an absolute expiration and no scrollback side effects.
pub struct Toast {
    /// Message displayed until expiration.
    pub message: String,
    /// Monotonic creation time.
    pub created: std::time::Instant,
}
impl Toast {
    /// Resolve a notification without extending its lifetime during redraw.
    pub fn line(&self, now: std::time::Instant, width: usize) -> Option<String> {
        (now.saturating_duration_since(self.created) < std::time::Duration::from_secs(2))
            .then(|| fit(&self.message, width))
    }
}
