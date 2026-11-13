//! Shared, borderless styling for interactive selection menus.
use crate::theme::{self, ColorMode, Rgb, Theme};

pub(super) fn text(value: &str, width: usize, color: Rgb, mode: ColorMode) -> String {
    let value = super::widgets::ellipsis(value, width, super::widgets::Glyphs::detect().ascii);
    format!(
        "{}{}{}",
        theme::foreground(color, mode),
        value,
        theme::reset(mode)
    )
}

pub(super) fn heading(value: &str, width: usize, theme: &Theme, mode: ColorMode) -> String {
    format!(
        "{}{}",
        theme::bold(mode),
        text(value, width, theme.text, mode)
    )
}

pub(super) fn choice(
    marker: bool,
    primary: &str,
    description: &str,
    width: usize,
    theme: &Theme,
    mode: ColorMode,
) -> String {
    let marker_text = if marker { "› " } else { "  " };
    let marker_part = text(&format!(" {marker_text}"), width, theme.rose, mode);
    let remaining = width.saturating_sub(3);
    let primary_part = text(
        primary,
        remaining,
        if marker { theme.text } else { theme.panel_text },
        mode,
    );
    use unicode_width::UnicodeWidthStr;
    let used = super::widgets::unstyled(&primary_part).width();
    let description_part = text(
        description,
        remaining.saturating_sub(used),
        theme.placeholder,
        mode,
    );
    format!("{marker_part}{primary_part}{description_part}")
}

pub(super) fn height(terminal_height: usize) -> usize {
    terminal_height
        .saturating_mul(35)
        .div_ceil(100)
        .max(3)
        .min(terminal_height)
}

pub(super) fn constrain(mut lines: Vec<String>, height: usize) -> Vec<String> {
    while lines.len() > height {
        if let Some(index) = lines
            .iter()
            .position(|line| super::widgets::unstyled(line).trim().is_empty())
        {
            lines.remove(index);
        } else if lines.len() > 2 {
            lines.remove(1);
        } else {
            lines.remove(0);
        }
    }
    lines
}
