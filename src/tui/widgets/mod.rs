//! Cell-measured primitives shared by terminal panels and overlays.

mod components;
pub use components::*;

use crate::theme::{background, default_foreground, foreground, reset, ColorMode, Rgb, Theme};
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

/// Style neutral panel borders while preserving the rose prompt and status accents.
pub fn style_frame_row(row: &str, theme: &Theme, mode: ColorMode) -> String {
    style_frame_row_with_panel(row, theme, mode, true)
}

/// Fill every cell of an information panel, including its border cells.
pub fn style_info_panel_row(row: &str, theme: &Theme, mode: ColorMode) -> String {
    if row.is_empty() {
        return String::new();
    }
    // The top and bottom outline cells straddle the outside of the rounded frame.
    // Keep those cells transparent; fill the entire width only on interior rows.
    let outline = row.starts_with(['╭', '╰', '+']);
    let fill = if outline {
        String::new()
    } else {
        background(theme.panel, mode)
    };
    format!(
        "{}{}",
        fill,
        style_frame_row_with_panel(row, theme, mode, false)
    )
}

/// Style a live row, pulsing the activity glyph without changing the glyph itself.
pub(crate) fn style_live_row(row: &str, theme: &Theme, mode: ColorMode, elapsed_ms: u64) -> String {
    let row = unstyled(row);
    let activity = row.trim_start_matches(' ');
    if (activity.starts_with("✻ ") || activity.starts_with("* ")) && activity.contains("Думаю…")
    {
        let marker_start = row.len() - activity.len();
        let marker_end = marker_start + activity.chars().next().map(char::len_utf8).unwrap_or(0);
        let split = row.find(" (").unwrap_or(row.len());
        let level = [100u16, 90, 80, 70, 80, 90][((elapsed_ms / 150) % 6) as usize];
        let crate::theme::Rgb(r, g, b) = theme.warn;
        let pulse = crate::theme::Rgb(
            (u16::from(r) * level / 100) as u8,
            (u16::from(g) * level / 100) as u8,
            (u16::from(b) * level / 100) as u8,
        );
        return format!(
            "{}{}{}{}{}{}{}{}",
            &row[..marker_start],
            foreground(pulse, mode),
            &row[marker_start..marker_end],
            foreground(theme.warn, mode),
            &row[marker_end..split],
            foreground(theme.placeholder, mode),
            &row[split..],
            reset(mode)
        );
    }
    style_frame_row(&row, theme, mode)
}

/// Style a frame row, optionally leaving its background transparent for image overlays.
pub fn style_frame_row_with_panel(
    row: &str,
    theme: &Theme,
    mode: ColorMode,
    solid_panel: bool,
) -> String {
    let row = unstyled(row);
    if row.is_empty() {
        return row;
    }
    let activity = row.trim_start_matches(' ');
    if (activity.starts_with("✻ ") || activity.starts_with("* ")) && activity.contains("Думаю…")
    {
        let split = row.find(" (").unwrap_or(row.len());
        return format!(
            "{}{}{}{}{}",
            foreground(theme.warn, mode),
            &row[..split],
            foreground(theme.placeholder, mode),
            &row[split..],
            reset(mode)
        );
    }
    if [
        "  ○ ", "  ◔ ", "  ◑ ", "  ◕ ", "  ● ", "  ✓ ", "  × ", "  * ", "  o ", "  O ", "  @ ",
        "  + ", "  x ",
    ]
    .iter()
    .any(|prefix| row.starts_with(prefix))
    {
        let marker_end = row[2..].chars().next().map_or(2, |c| 2 + c.len_utf8());
        return format!(
            "  {}{}{}{}{}",
            foreground(theme.rose, mode),
            &row[2..marker_end],
            foreground(theme.panel_text, mode),
            &row[marker_end..],
            reset(mode)
        );
    }
    let rose = foreground(theme.rose, mode);
    let text = default_foreground(mode).to_owned();
    let panel_text = foreground(theme.panel_text, mode);
    let panel_background = if solid_panel {
        background(theme.panel, mode)
    } else {
        String::new()
    };
    let border = foreground(theme.border, mode);
    let reset = reset(mode);
    let top = row.starts_with(['╭', '+']) && row.ends_with(['╮', '+']);
    let bottom = row.starts_with(['╰', '+']) && row.ends_with(['╯', '+']);
    if top {
        let marker = row
            .find("✿")
            .map(|index| (index, "✿"))
            .or_else(|| row.find("* gitcode").map(|index| (index, "*")));
        if let Some((start, marker)) = marker {
            let title_start = start + marker.len();
            let title_end = row[title_start..]
                .find(['─', '-'])
                .map_or(row.len(), |index| title_start + index);
            let mut styled = border.clone();
            for (index, ch) in row.char_indices() {
                if index == start {
                    styled.push_str(&rose);
                } else if index == title_start {
                    styled.push_str(&panel_text);
                } else if index == title_end {
                    styled.push_str(&border);
                }
                styled.push(ch);
            }
            return format!("{styled}{reset}");
        }
        if row
            .chars()
            .skip(1)
            .take(row.chars().count().saturating_sub(2))
            .all(|ch| matches!(ch, '─' | '-'))
        {
            let mut styled = border.clone();
            for (position, ch) in row.chars().enumerate() {
                styled.push_str(if position < 4 { &rose } else { &border });
                styled.push(ch);
            }
            return format!("{styled}{reset}");
        }
        return format!("{border}{row}{reset}");
    }
    if bottom && row.contains("shift+tab") {
        let label_color = if row.contains("BYPASS") {
            foreground(theme.warn, mode)
        } else {
            panel_text.clone()
        };
        let shift = row.find("shift+tab").unwrap_or(row.len());
        let label = row[..shift].rfind(['─', '-']).map_or(shift, |index| {
            index + row[index..].chars().next().map_or(0, char::len_utf8)
        });
        let label_end = row
            .char_indices()
            .last()
            .map_or(row.len(), |(index, _)| index);
        let mut styled = border.clone();
        for (position, (index, ch)) in row.char_indices().enumerate() {
            let color = if position < 4 {
                &rose
            } else if label <= index && index < label_end {
                &label_color
            } else {
                &border
            };
            styled.push_str(color);
            styled.push(ch);
        }
        return format!("{styled}{reset}");
    }
    if bottom || row.chars().all(|ch| matches!(ch, '─' | '-')) {
        return format!("{border}{row}{reset}");
    }

    let framed = (row.starts_with('│') && row.ends_with('│'))
        || (row.starts_with('|') && row.ends_with('|'));
    let prompt_edge =
        (row.starts_with(" › ") || row.starts_with(" > ")) && row.ends_with(['│', '|']);
    let header_row = row.contains("Model    :")
        || row.contains("Session  :")
        || row.contains("Cache    :")
        || row.contains("Speed    :")
        || row.contains("Output   :")
        || row.contains("Context  :")
        || row.contains("Working  :")
        || row.contains("Working :")
        || row.contains("Mode    :")
        || row.contains("Tools    :")
        || row
            .chars()
            .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
        || row
            .chars()
            .any(|ch| (0xE000..0xE400).contains(&(ch as u32)));
    let header_columns = !framed && header_row && (row.contains(" │ ") || row.contains(" | "));
    let output_span = output_token_span(&row);
    let placeholder_start = if row.contains(" › Спроси, или / для команд")
        || row.contains(" > Спроси, или / для команд")
    {
        row.find("Спроси, или / для команд")
    } else {
        None
    };
    let placeholder_end = placeholder_start.map(|start| start + "Спроси, или / для команд".len());
    let row_text = if header_row {
        panel_text.clone()
    } else {
        text.clone()
    };
    let fill_bounds = (solid_panel && header_row && framed)
        .then(|| panel_inner_bounds(&row))
        .flatten();
    let mut styled = row_text.clone();
    for (index, ch) in row.char_indices() {
        if let Some((inner_start, inner_end)) = fill_bounds {
            if index == inner_start {
                styled.push_str(&panel_background);
                styled.push_str(&row_text);
            } else if index == inner_end {
                styled.push_str(&reset);
                styled.push_str(&row_text);
            }
        }
        if let Some(pixel) = logo_pixel(ch, theme, mode) {
            styled.push_str(&pixel);
            continue;
        }
        if let Some((start, end)) = output_span {
            if index == start {
                styled.push_str(&foreground(theme.ok, mode));
                styled.push_str(&row[start..end]);
                styled.push_str(&row_text);
                continue;
            }
            if index > start && index < end {
                continue;
            }
        }
        if let (Some(start), Some(end)) = (placeholder_start, placeholder_end) {
            if index == start {
                styled.push_str(&foreground(theme.placeholder, mode));
                styled.push_str(&row[start..end]);
                styled.push_str(&row_text);
                continue;
            }
            if index > start && index < end {
                continue;
            }
        }
        if (framed || header_columns) && ('\u{2801}'..='\u{28ff}').contains(&ch) {
            styled.push_str(&foreground(Rgb(244, 181, 44), mode));
            styled.push(ch);
            styled.push_str(&row_text);
        } else if header_row && ch == '▏' {
            styled.push_str(&rose);
            styled.push(ch);
            styled.push_str(&row_text);
        } else if header_row && index == 0 && matches!(ch, '│' | '|') {
            styled.push_str(&rose);
            styled.push(ch);
            styled.push_str(&row_text);
        } else if header_row && matches!(ch, '▟' | '▙' | '▛' | '▜' | '▝' | '▀' | '█')
        {
            styled.push_str(&foreground(theme.rose_soft, mode));
            styled.push(ch);
            styled.push_str(&row_text);
        } else if (framed || prompt_edge || header_columns)
            && matches!(ch, '│' | '|' | '─' | '-')
            && (!prompt_edge || index + ch.len_utf8() == row.len() || framed || header_columns)
        {
            styled.push_str(&border);
            styled.push(ch);
            styled.push_str(&row_text);
        } else if matches!(
            ch,
            '│' | '›' | '✻' | '✿' | '▏' | '▎' | '▍' | '▌' | '▋' | '▊' | '▉'
        ) {
            styled.push_str(&rose);
            styled.push(ch);
            styled.push_str(&row_text);
        } else if ch == '─' {
            styled.push_str(&border);
            styled.push(ch);
            styled.push_str(&row_text);
        } else if matches!(ch, '░' | '▯') {
            styled.push_str(&foreground(theme.rose_deep, mode));
            styled.push(ch);
            styled.push_str(&row_text);
        } else {
            styled.push(ch);
        }
    }
    format!("{styled}{reset}")
}

const LOGO_TOKEN_BASE: u32 = 0xE000;
const LOGO_PALETTE: [Rgb; 31] = [
    Rgb(154, 70, 94),
    Rgb(141, 63, 87),
    Rgb(120, 61, 78),
    Rgb(108, 52, 68),
    Rgb(101, 49, 65),
    Rgb(102, 43, 63),
    Rgb(90, 46, 59),
    Rgb(84, 39, 52),
    Rgb(75, 45, 54),
    Rgb(79, 40, 52),
    Rgb(63, 40, 48),
    Rgb(58, 39, 45),
    Rgb(68, 37, 46),
    Rgb(62, 33, 43),
    Rgb(54, 34, 41),
    Rgb(54, 26, 36),
    Rgb(46, 27, 34),
    Rgb(42, 26, 32),
    Rgb(38, 25, 29),
    Rgb(31, 17, 22),
    Rgb(23, 15, 18),
    Rgb(20, 13, 16),
    Rgb(18, 14, 16),
    Rgb(16, 12, 14),
    Rgb(14, 11, 14),
    Rgb(14, 11, 13),
    Rgb(13, 11, 13),
    Rgb(13, 10, 13),
    Rgb(13, 10, 12),
    Rgb(12, 10, 12),
    Rgb(10, 10, 10),
];

fn logo_pixel(token: char, theme: &Theme, mode: ColorMode) -> Option<String> {
    let encoded = (token as u32).checked_sub(LOGO_TOKEN_BASE)?;
    if encoded >= 1024 {
        return None;
    }
    let top = (encoded / 32) as usize;
    let bottom = (encoded % 32) as usize;
    let glyph = match (top, bottom) {
        (0, 0) => return Some(" ".to_owned()),
        (0, _) => '▄',
        (_, _) => '▀',
    };
    if mode == ColorMode::None {
        return Some(glyph.to_string());
    }
    let mut styled = String::new();
    if top > 0 {
        styled.push_str(&foreground(LOGO_PALETTE[top - 1], mode));
    }
    if top > 0 && bottom > 0 {
        styled.push_str(&background(LOGO_PALETTE[bottom - 1], mode));
    } else if top == 0 && bottom > 0 {
        styled.push_str(&foreground(LOGO_PALETTE[bottom - 1], mode));
    }
    styled.push(glyph);
    styled.push_str(reset(mode));
    styled.push_str(&background(theme.panel, mode));
    styled.push_str(&foreground(theme.panel_text, mode));
    Some(styled)
}

fn panel_inner_bounds(row: &str) -> Option<(usize, usize)> {
    let inner_start = row.char_indices().nth(1)?.0;
    let inner_end = row.char_indices().last()?.0;
    Some((inner_start, inner_end))
}

fn output_token_span(row: &str) -> Option<(usize, usize)> {
    if let Some(output) = row.find("Output   : ") {
        let start = output + "Output   : ".len();
        let end = row[start..]
            .find(char::is_whitespace)
            .map_or(row.len(), |index| start + index);
        return (start < end).then_some((start, end));
    }
    if let Some(session) = row.find("Session  : ") {
        let counters_start = session + "Session  : ".len();
        let start = counters_start + row[counters_start..].find("↑ ")? + "↑ ".len();
        let end = row[start..].find('↓').map_or_else(
            || {
                row[start..]
                    .find(char::is_whitespace)
                    .map_or(row.len(), |index| start + index)
            },
            |index| start + index,
        );
        return (start < end).then_some((start, end));
    }
    let counters_start = row.find(" │ ")? + " │ ".len();
    let counters_end = row[counters_start..]
        .find(" │ ")
        .map_or(row.len(), |index| counters_start + index);
    let counters = &row[counters_start..counters_end];
    let start = counters_start + counters.find("↑ ")? + "↑ ".len();
    let end = row[start..].find('↓').map_or_else(
        || {
            row[start..]
                .find(char::is_whitespace)
                .map_or(counters_end, |index| start + index)
        },
        |index| start + index,
    );
    (start < end).then_some((start, end))
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
        let text = default_foreground(mode);
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
        assert!(title.starts_with(&format!("{}╭─── ", foreground(theme.border, mode))));
        let typed = style_frame_row(" › text     │", &theme, mode);
        assert!(typed.contains(&format!("{rose}›{text} text")));
        assert!(typed.contains(&format!("{}│", foreground(theme.border, mode))));
        assert!(typed.ends_with(reset(mode)));
        let placeholder = style_frame_row(" › Спроси, или / для команд │", &theme, mode);
        assert!(placeholder.contains(&format!("{}Спроси", foreground(theme.placeholder, mode))));
        let divider = style_frame_row("│  ───────  │", &theme, mode);
        assert_eq!(unstyled(&divider), "│  ───────  │");
        assert!(divider.contains(&format!("{}─", foreground(theme.border, mode))));
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
                assert!(styled[0].starts_with(&foreground(theme.border, ColorMode::TrueColor)));
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
        assert!(styled.contains(&format!("{}  ordinary content  ", default_foreground(mode))));
        let styled = style_frame_row("│  ─────  │", &theme, mode);
        assert!(styled.contains(&foreground(theme.border, mode)));
        let styled = style_frame_row("│  › Спроси, или / для команд  │", &theme, mode);
        assert!(styled.contains(&foreground(theme.placeholder, mode)));
        let styled = style_frame_row("│  literal Спроси, или / для команд  │", &theme, mode);
        assert!(!styled.contains(&foreground(theme.placeholder, mode)));
    }

    #[test]
    fn bypass_mode_and_shortcut_are_yellow_other_modes_gray() {
        let theme = Theme::default();
        let gray = foreground(theme.panel_text, ColorMode::TrueColor);
        for label in ["DEFAULT", "ACCEPT EDITS", "PLAN", "BYPASS"] {
            let gray = if label == "BYPASS" {
                foreground(theme.warn, ColorMode::TrueColor)
            } else {
                gray.clone()
            };
            let row = format!("╰──────── {label} · shift+tab ╯");
            let styled = style_frame_row(&row, &theme, ColorMode::TrueColor);
            for ch in format!("{label} · shift+tab").chars() {
                assert!(styled.contains(&format!("{gray}{ch}")), "{label}: {ch}");
            }
        }
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
