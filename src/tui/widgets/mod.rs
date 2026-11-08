//! Cell-measured primitives shared by terminal panels and overlays.

mod components;
pub use components::*;

use crate::theme::{foreground, reset, ColorMode, Theme};
use crate::tui::{clip, plain};

/// Shared number of horizontal border cells before every frame title.
pub const TITLE_INDENT: usize = 3;
use unicode_width::UnicodeWidthStr;

/// Terminal glyph capability, independent of color support.
#[derive(Clone, Copy, Debug)]
pub struct Glyphs {
    /// Prefer conservative ASCII characters.
    pub ascii: bool,
}
impl Glyphs {
    /// Detect fallback without probing or changing terminal state.
    pub fn detect() -> Self {
        Self {
            ascii: std::env::var("GITCODE_ASCII").as_deref() == Ok("1")
                || std::env::var("TERM").as_deref() == Ok("linux"),
        }
    }
}

/// Fit untrusted text to an exact number of terminal cells.
pub fn fit(text: &str, width: usize) -> String {
    let text = clip(text, width);
    format!("{text}{}", " ".repeat(width.saturating_sub(text.width())))
}

/// Truncate long text with an explicit omission marker.
pub fn ellipsis(text: &str, width: usize, ascii: bool) -> String {
    let text = plain(text).replace('\n', " ");
    if text.width() <= width {
        return text;
    }
    let marker = if ascii { "." } else { "…" };
    if width == 0 {
        return String::new();
    }
    format!("{}{marker}", clip(&text, width - 1))
}

/// Rounded panel with title and bottom-right mode label.
pub struct Frame<'a> {
    /// Top-left title.
    pub title: &'a str,
    /// Bottom-right label.
    pub footer: &'a str,
    /// Capability-selected borders.
    pub glyphs: Glyphs,
}
impl Frame<'_> {
    /// Render plain rows; styling is applied by the caller's theme layer.
    pub fn render(&self, body: &[String], width: usize) -> Vec<String> {
        if width < 4 {
            return body.iter().map(|row| fit(row, width)).collect();
        }
        let (tl, tr, bl, br, horizontal, vertical) = if self.glyphs.ascii {
            ('+', '+', '+', '+', '-', '|')
        } else {
            ('╭', '╮', '╰', '╯', '─', '│')
        };
        let inner = width - 2;
        let indent = TITLE_INDENT.min(inner);
        let title = ellipsis(
            self.title,
            inner.saturating_sub(indent + 2),
            self.glyphs.ascii,
        );
        let title = if title.is_empty() {
            String::new()
        } else {
            format!(" {title} ")
        };
        let footer = ellipsis(self.footer, inner.saturating_sub(2), self.glyphs.ascii);
        let footer = if footer.is_empty() {
            String::new()
        } else {
            format!(" {footer} ")
        };
        let mut rows = vec![format!(
            "{tl}{}{title}{}{tr}",
            horizontal.to_string().repeat(indent),
            horizontal
                .to_string()
                .repeat(inner.saturating_sub(indent + title.width()))
        )];
        rows.extend(
            body.iter()
                .map(|row| format!("{vertical}{}{vertical}", fit(row, inner))),
        );
        rows.push(format!(
            "{bl}{}{footer}{br}",
            horizontal
                .to_string()
                .repeat(inner.saturating_sub(footer.width()))
        ));
        rows
    }

    /// Render structural borders independently from the panel's text.
    pub fn render_styled(
        &self,
        body: &[String],
        width: usize,
        theme: &Theme,
        mode: ColorMode,
    ) -> Vec<String> {
        style_frame_rows(&self.render(body, width), theme, mode)
    }
}

/// Strip trusted ANSI CSI sequences before measuring rendered terminal cells.
pub fn unstyled(text: &str) -> String {
    let mut chars = text.chars().peekable();
    let mut output = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for code in chars.by_ref() {
                if ('@'..='~').contains(&code) {
                    break;
                }
            }
        } else if !ch.is_control() || ch == '\n' {
            output.push(ch);
        }
    }
    output
}

/// Style plain frame rows without counting ANSI escapes as terminal cells.
pub fn style_frame_rows(rows: &[String], theme: &Theme, mode: ColorMode) -> Vec<String> {
    rows.iter()
        .map(|row| style_frame_row(row, theme, mode))
        .collect()
}

/// Keep complete top/bottom borders rose and body text independent of side borders.
pub fn style_frame_row(row: &str, theme: &Theme, mode: ColorMode) -> String {
    let row = unstyled(row);
    if mode == ColorMode::None || row.is_empty() {
        return row;
    }
    let rose = foreground(theme.rose, mode);
    let text = foreground(theme.text, mode);
    let reset = reset(mode);
    if row.starts_with(['╭', '╰', '+']) && row.ends_with(['╮', '╯', '+']) {
        return format!("{rose}{row}{reset}");
    }
    if (row.starts_with('│') && row.ends_with('│'))
        || (row.starts_with('|') && row.ends_with('|'))
        || ((row.starts_with(" › ") || row.starts_with(" > ")) && row.ends_with(['│', '|']))
    {
        let first = row.chars().next().unwrap_or(' ');
        let last = row.chars().next_back().unwrap_or(' ');
        let inner = &row[first.len_utf8()..row.len() - last.len_utf8()];
        let content = if !inner.trim().is_empty()
            && inner.trim().chars().all(|ch| matches!(ch, '─' | '-'))
        {
            format!("{}{inner}", foreground(theme.rose_deep, mode))
        } else {
            let prompt = inner.trim().strip_prefix(['›', '>']).map(str::trim);
            let inner = if prompt == Some("Спроси, или / для команд") {
                inner.replacen(
                    "Спроси, или / для команд",
                    &format!(
                        "{}Спроси, или / для команд{text}",
                        foreground(theme.rose_soft, mode)
                    ),
                    1,
                )
            } else {
                inner.to_owned()
            };
            let inner = if let Some(chevron) = inner
                .trim_start()
                .chars()
                .next()
                .filter(|ch| matches!(ch, '›' | '>'))
            {
                inner.replacen(chevron, &format!("{rose}{chevron}{text}"), 1)
            } else {
                inner
            };
            format!("{text}{inner}")
        };
        return format!("{rose}{first}{content}{rose}{last}{reset}");
    }
    let mut styled = text.clone();
    for ch in row.chars() {
        if matches!(
            ch,
            '│' | '─' | '✻' | '✿' | '█' | '▏' | '▎' | '▍' | '▌' | '▋' | '▊' | '▉'
        ) {
            styled.push_str(&rose);
            styled.push(ch);
            styled.push_str(&text);
        } else if ch == '░' {
            styled.push_str(&foreground(theme.rose_deep, mode));
            styled.push(ch);
            styled.push_str(&text);
        } else {
            styled.push(ch);
        }
    }
    format!("{styled}{reset}")
}

/// Compact visible key token, readable with or without colors.
pub fn keycap(key: &str) -> String {
    format!("[{}]", plain(key).replace('\n', " "))
}

/// A row of contextual keyboard actions.
pub fn key_hints(items: &[(&str, &str)], width: usize) -> String {
    clip(
        &items
            .iter()
            .map(|(key, label)| format!("{} {label}", keycap(key)))
            .collect::<Vec<_>>()
            .join(" · "),
        width,
    )
}

/// Flat progress mark; missing totals must be represented separately by callers.
pub fn progress(value: f64, width: usize, glyphs: Glyphs) -> String {
    let value = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let cells = value * width as f64;
    let filled = cells.floor() as usize;
    if glyphs.ascii {
        return format!("{}{}", "#".repeat(filled), "-".repeat(width - filled));
    }
    let fraction = ((cells - filled as f64) * 8.0).floor() as usize;
    let partial = ['░', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let mut result = "█".repeat(filled);
    if filled < width {
        result.push(partial[fraction.min(7)]);
        result.push_str(&"░".repeat(width - filled - 1));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_preserve_exact_cell_width_in_all_capabilities() {
        for width in [0, 1, 3, 50, 80, 100, 120, 200] {
            for ascii in [false, true] {
                let rows = Frame {
                    title: "Команды",
                    footer: "DEFAULT · shift+tab",
                    glyphs: Glyphs { ascii },
                }
                .render(
                    &[" › Спроси, или / для команд 界".into(), "x".repeat(300)],
                    width,
                );
                assert!(rows.iter().all(|row| row.width() == width));
                if ascii {
                    assert!(rows
                        .iter()
                        .all(|row| !row.contains(['╭', '╮', '╰', '╯', '│', '─'])));
                }
            }
        }
    }

    #[test]
    fn styled_frames_keep_geometry_and_text_separate() {
        let theme = Theme::default();
        let mode = ColorMode::TrueColor;
        let rose = foreground(theme.rose, mode);
        let text = foreground(theme.text, mode);
        for ascii in [false, true] {
            for width in 0..100 {
                let frame = Frame {
                    title: "Заголовок 界",
                    footer: "DEFAULT",
                    glyphs: Glyphs { ascii },
                };
                let rows = frame.render(&["  内容 e\u{301}".into()], width);
                let styled = style_frame_rows(&rows, &theme, mode);
                assert_eq!(styled.iter().map(|s| unstyled(s)).collect::<Vec<_>>(), rows);
                assert!(styled.iter().all(|s| unstyled(s).width() == width));
            }
        }
        let title = style_frame_row("╭─── Title ──╮", &theme, mode);
        assert!(title.starts_with(&format!("{rose}╭─── Title ")));
        let typed = style_frame_row(" › text     │", &theme, mode);
        assert!(typed.contains(&format!("{rose}›{text} text")));
        assert!(typed.ends_with(&format!("{rose}│{}", reset(mode))));
        let placeholder = style_frame_row(" › Спроси, или / для команд │", &theme, mode);
        assert!(placeholder.contains(&format!("{}Спроси", foreground(theme.rose_soft, mode))));
        let divider = style_frame_row("│  ───────  │", &theme, mode);
        assert!(divider.contains(&format!("{}  ───────  ", foreground(theme.rose_deep, mode))));
        let rows = Frame {
            title: "Title",
            footer: "",
            glyphs: Glyphs { ascii: false },
        }
        .render(&[], 30);
        assert!(rows[0].starts_with("╭─── Title "));
    }

    #[test]
    fn frame_title_indent_and_styles_are_cell_safe() {
        let theme = Theme::default();
        for ascii in [false, true] {
            let frame = Frame {
                title: "Title",
                footer: "DEFAULT",
                glyphs: Glyphs { ascii },
            };
            for width in 4..90 {
                let body = vec![
                    "  › Спроси, или / для команд  ".into(),
                    format!("  {}  ", if ascii { "-----" } else { "─────" }),
                ];
                let plain_rows = frame.render(&body, width);
                let styled = frame.render_styled(&body, width, &theme, ColorMode::TrueColor);
                assert_eq!(
                    styled.iter().map(|row| unstyled(row)).collect::<Vec<_>>(),
                    plain_rows
                );
                assert!(styled.iter().all(|row| unstyled(row).width() == width));
                assert!(styled[0].starts_with(&foreground(theme.rose, ColorMode::TrueColor)));
                if width >= 16 {
                    assert!(plain_rows[0].starts_with(if ascii {
                        "+--- Title "
                    } else {
                        "╭─── Title "
                    }));
                }
            }
        }
        let mode = ColorMode::TrueColor;
        let styled = style_frame_row("│  ordinary content  │", &theme, mode);
        assert!(styled.contains(&format!(
            "{}  ordinary content  ",
            foreground(theme.text, mode)
        )));
        let styled = style_frame_row("│  ─────  │", &theme, mode);
        assert!(styled.contains(&foreground(theme.rose_deep, mode)));
        let styled = style_frame_row("│  › Спроси, или / для команд  │", &theme, mode);
        assert!(styled.contains(&foreground(theme.rose_soft, mode)));
        let styled = style_frame_row("│  literal Спроси, или / для команд  │", &theme, mode);
        assert!(!styled.contains(&foreground(theme.rose_soft, mode)));
    }

    #[test]
    fn bounded_labels_and_progress() {
        assert_eq!(ellipsis("abcdef", 4, false), "abc…");
        assert_eq!(ellipsis("abcdef", 0, false), "");
        assert_eq!(progress(0.5, 6, Glyphs { ascii: true }), "###---");
        assert_eq!(progress(f64::NAN, 3, Glyphs { ascii: true }), "---");
        assert_eq!(key_hints(&[("esc", "закрыть")], 40), "[esc] закрыть");
    }
}
