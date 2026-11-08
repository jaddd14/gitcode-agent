//! Bounded line diffs with explicit old/new line numbers and flat theme roles.

use crate::theme::{foreground, ColorMode, Theme};

/// Render a byte change as sanitized, numbered lines without interpreting file ANSI.
///
/// Common prefix and suffix lines are retained around a single replacement band.
/// This bounded algorithm is linear rather than quadratic on large generated files;
/// it deliberately does not claim a minimal edit script or word-level highlighting.
pub fn render(
    before: Option<&[u8]>,
    after: Option<&[u8]>,
    width: usize,
    mode: ColorMode,
) -> Vec<String> {
    let old = String::from_utf8_lossy(before.unwrap_or_default());
    let new = String::from_utf8_lossy(after.unwrap_or_default());
    let old: Vec<_> = old.lines().collect();
    let new: Vec<_> = new.lines().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let theme = Theme::default();
    let mut lines = Vec::new();
    let mut push = |sign: char, left: Option<usize>, right: Option<usize>, text: &str| {
        if lines.len() >= 200 {
            return;
        }
        let number = |n: Option<usize>| {
            n.map(|n| format!("{n:>5}"))
                .unwrap_or_else(|| "     ".into())
        };
        let row = format!(
            "{} {} {sign} {}",
            number(left),
            number(right),
            super::plain(text)
        );
        let row = super::clip(&row, width);
        let color = match sign {
            '+' => theme.ok,
            '-' => theme.error,
            _ => theme.text,
        };
        if mode == ColorMode::None {
            lines.push(row);
        } else {
            lines.push(format!("{}{row}\x1b[0m", foreground(color, mode)));
        }
    };
    for (i, text) in old
        .iter()
        .enumerate()
        .take(prefix)
        .skip(prefix.saturating_sub(3))
    {
        push(' ', Some(i + 1), Some(i + 1), text);
    }
    for (i, text) in old.iter().enumerate().take(old.len() - suffix).skip(prefix) {
        push('-', Some(i + 1), None, text);
    }
    for (i, text) in new.iter().enumerate().take(new.len() - suffix).skip(prefix) {
        push('+', None, Some(i + 1), text);
    }
    for offset in 0..suffix.min(3) {
        push(
            ' ',
            Some(old.len() - suffix + offset + 1),
            Some(new.len() - suffix + offset + 1),
            new[new.len() - suffix + offset],
        );
    }
    if lines.len() >= 200 {
        lines.push("[diff truncated at 200 rows]".into());
    }
    lines
}
