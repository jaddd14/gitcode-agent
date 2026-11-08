//! Event-driven terminal foundation, independent of agent and provider orchestration.

pub mod app;
pub mod diff;
pub mod header;
pub mod input;
pub mod intro;
pub mod markdown;
pub mod render;
pub mod statusbar;
pub mod term;

/// Remove terminal control characters from untrusted display text.
pub fn plain(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect()
}

/// Truncate text on grapheme boundaries to a terminal-cell budget.
pub fn clip(text: &str, width: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let mut used = 0;
    plain(text)
        .graphemes(true)
        .take_while(|g| {
            used += UnicodeWidthStr::width(*g);
            used <= width
        })
        .collect()
}
