//! Event-driven terminal foundation, independent of agent and provider orchestration.

pub mod app;
mod approval_picker;
mod bypass_picker;
pub mod capabilities;
pub mod clipboard;
pub mod commands;
pub mod diff;
pub mod effort_picker;
pub mod header;
mod import_picker;
pub mod input;
pub mod intro;
pub mod markdown;
pub mod model_picker;
mod picker;
mod question_picker;
pub mod render;
pub mod session_picker;
pub mod statusbar;
pub mod term;
pub mod theme_picker;
pub mod tool_card;
pub mod usage_panel;
mod viewport;
pub mod widgets;

/// Remove terminal control characters from untrusted display text.
pub fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.next() {
                Some('[') => {
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(next) = chars.next() {
                        if next == '\x07' {
                            break;
                        }
                        if next == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if !c.is_control() || c == '\n' {
            out.push(c);
        }
    }
    out
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
