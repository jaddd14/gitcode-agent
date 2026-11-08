//! Original flat sakura bitmap and bundled, network-free startup header.

use super::clip;
use crate::theme::{foreground, ColorMode, Rgb, Theme};

/// Original 16 by 12 sakura sprite: transparent, deep pink, pink, soft pink, text.
pub const SAKURA: [&str; 12] = [
    "      2332      ",
    "     233332     ",
    "  222233332222  ",
    " 23333322333332 ",
    "2333332222333332",
    " 23332244223332 ",
    "  222244442222  ",
    " 23332244223332 ",
    "2333332222333332",
    " 233332  233332 ",
    "  2222    2222  ",
    "   11      11   ",
];
/// Extra blank rows between header and input.
pub const HEADER_GAP: usize = 3;
/// Bundled news; remote fetching is deliberately outside the startup path.
pub const NEWS: [&str; 2] = [
    "Новое: терминальный интерфейс",
    "Совет: Shift+Enter — новая строка",
];

/// Header metadata supplied by orchestration without synchronous git or network calls.
#[derive(Debug, Clone)]
pub struct Header<'a> {
    /// Application version.
    pub version: &'a str,
    /// Selected model.
    pub model: &'a str,
    /// Selected provider display name.
    pub provider: &'a str,
    /// Current working directory.
    pub cwd: &'a str,
    /// Cached branch name.
    pub branch: &'a str,
}
fn pixel(index: u8, theme: &Theme) -> Rgb {
    match index {
        b'1' => Rgb(163, 20, 79),
        b'2' => Rgb(214, 51, 108),
        b'3' => Rgb(240, 101, 149),
        b'4' => Rgb(239, 230, 247),
        _ => theme.bg,
    }
}
fn background(color: Rgb, mode: ColorMode) -> String {
    foreground(color, mode)
        .replacen("[38;", "[48;", 1)
        .replace("[3", "[4")
        .replace("[9", "[10")
}
/// Render six half-block rows, or uncolored silhouette cells in plain mode.
pub fn mascot(theme: &Theme, mode: ColorMode) -> Vec<String> {
    SAKURA
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let mut line = String::new();
            for (top, bottom) in pair[0].bytes().zip(pair[1].bytes()) {
                if mode == ColorMode::None {
                    line.push(if top == b' ' && bottom == b' ' {
                        ' '
                    } else {
                        '▀'
                    });
                } else {
                    line.push_str(&foreground(pixel(top, theme), mode));
                    line.push_str(&background(pixel(bottom, theme), mode));
                    line.push('▀');
                }
            }
            if mode != ColorMode::None {
                line.push_str("\x1b[0m");
            }
            line
        })
        .collect()
}
impl Header<'_> {
    /// Compose metadata and news to the right of the mascot, stacking on narrow screens.
    pub fn lines(&self, width: usize, theme: &Theme, mode: ColorMode) -> Vec<String> {
        let width = width.min(100);
        if width < 4 {
            return vec![clip(
                &format!("gitcode {} · {}", self.version, self.model),
                width,
            )];
        }
        let glyphs = super::widgets::Glyphs::detect();
        let info = vec![
            format!("Модель     {}", self.model),
            format!("Провайдер  {}", self.provider),
            "Режим      DEFAULT".into(),
            format!("Папка      {}", self.cwd),
            format!("Ветка      {}", self.branch),
            String::new(),
            "Новое".into(),
            "+ Локальные инструменты и сессии".into(),
            "+ Три протокола провайдера".into(),
        ];
        let logo = if width >= 64 && !glyphs.ascii {
            mascot(theme, ColorMode::None)
        } else {
            vec![]
        };
        let content_width = width.saturating_sub(6);
        let mut body = info
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                let content = if logo.is_empty() {
                    text
                } else {
                    format!(
                        "{}  {text}",
                        super::widgets::fit(logo.get(i).map(String::as_str).unwrap_or(""), 16)
                    )
                };
                format!("  {}  ", super::widgets::fit(&content, content_width))
            })
            .collect::<Vec<_>>();
        body.push(format!(
            "  {}  ",
            super::widgets::divider(content_width, glyphs)
        ));
        body.push(format!(
            "  {}  ",
            super::widgets::fit(
                "Совет: Shift+Enter — новая строка; / — команды",
                content_width
            )
        ));
        let title = format!(
            "{}gitcode {}",
            if glyphs.ascii { "" } else { "✿ " },
            self.version
        );
        let mut rows = super::widgets::Frame {
            title: &title,
            footer: "",
            glyphs,
        }
        .render(&body, width);
        rows = super::widgets::style_frame_rows(&rows, theme, mode);
        if mode != ColorMode::None {
            let colored_logo = mascot(theme, mode);
            for (index, row) in rows.iter_mut().enumerate() {
                if let Some(logo_row) = index.checked_sub(1).and_then(|i| logo.get(i)) {
                    if let Some(colored) = index.checked_sub(1).and_then(|i| colored_logo.get(i)) {
                        *row = row.replacen(
                            logo_row,
                            &format!("{colored}{}", foreground(theme.text, mode)),
                            1,
                        );
                    }
                }
                if index == 3 {
                    *row = row.replacen(
                        "DEFAULT",
                        &format!(
                            "{}{}DEFAULT{}{}",
                            foreground(theme.text, mode),
                            crate::theme::background(theme.rose_deep, mode),
                            crate::theme::reset(mode),
                            foreground(theme.text, mode),
                        ),
                        1,
                    );
                }
            }
        }
        rows.extend(std::iter::repeat_n(String::new(), HEADER_GAP));
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn header_closes_at_a_constant_column_with_inner_footer() {
        let header = Header {
            version: "0.2.0",
            model: "очень длинная модель 界",
            provider: "провайдер",
            cwd: "/очень/длинная/папка/界",
            branch: "main",
        };
        let theme = Theme::default();
        for width in [0, 1, 39, 40, 50, 64, 80, 100, 120] {
            let rows = header.lines(width, &theme, ColorMode::TrueColor);
            if width < 4 {
                assert!(rows
                    .iter()
                    .all(|row| crate::tui::widgets::unstyled(row).width() <= width));
                continue;
            }
            let rows = &rows[..rows.len() - HEADER_GAP];
            for row in rows {
                let row = crate::tui::widgets::unstyled(row);
                assert_eq!(row.width(), width.min(100), "{row}");
            }
            let tip = crate::tui::widgets::unstyled(&rows[rows.len() - 2]);
            assert!(tip.starts_with("│  Совет") || tip.starts_with("|  Совет"));
            let divider = crate::tui::widgets::unstyled(&rows[rows.len() - 3]);
            assert!(divider.contains("────") || divider.contains("----"));
            assert!(!rows.join("\n").contains("[DEFAULT]"));
        }
    }

    #[test]
    fn mascot_pixels_retain_the_original_palette() {
        let original = Theme::default();
        let mut changed = original.clone();
        changed.rose = Rgb(1, 2, 3);
        changed.rose_soft = Rgb(4, 5, 6);
        changed.rose_deep = Rgb(7, 8, 9);
        changed.text = Rgb(10, 11, 12);
        assert_eq!(
            mascot(&original, ColorMode::TrueColor),
            mascot(&changed, ColorMode::TrueColor)
        );
        let rows = mascot(&changed, ColorMode::TrueColor).join("\n");
        for color in [
            Rgb(163, 20, 79),
            Rgb(214, 51, 108),
            Rgb(240, 101, 149),
            Rgb(239, 230, 247),
        ] {
            assert!(
                rows.contains(&foreground(color, ColorMode::TrueColor))
                    || rows.contains(&crate::theme::background(color, ColorMode::TrueColor))
            );
        }
    }
}

#[cfg(test)]
mod style_tests {
    use super::*;
    use crate::tui::widgets::unstyled as plain;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn header_keeps_closed_constant_width_and_padded_tip() {
        let path = "/very/long/path/".repeat(20);
        let header = Header {
            version: "0.1.0",
            model: "model 界",
            provider: "provider",
            cwd: &path,
            branch: "main",
        };
        for width in [20, 39, 40, 50, 64, 80, 100, 120] {
            for mode in [ColorMode::None, ColorMode::TrueColor, ColorMode::Ansi16] {
                let rows = header.lines(width, &Theme::default(), mode);
                let rows = &rows[..rows.len() - HEADER_GAP];
                assert!(rows.iter().all(|row| plain(row).width() == width.min(100)));
                for row in &rows[1..rows.len() - 1] {
                    let row = plain(row);
                    assert!(row.starts_with("│  ") || row.starts_with("|  "));
                    assert!(row.ends_with("  │") || row.ends_with("  |"));
                }
                let tip = plain(&rows[rows.len() - 2]);
                assert!(tip.contains("Совет:"));
                assert!(!rows.iter().any(|row| row.contains("[DEFAULT]")));
            }
        }
    }

    #[test]
    fn mascot_palette_does_not_follow_accent_or_text() {
        let theme = Theme::default();
        let mut alternate = theme.clone();
        alternate.rose = Rgb(1, 2, 3);
        alternate.rose_soft = Rgb(4, 5, 6);
        alternate.rose_deep = Rgb(7, 8, 9);
        alternate.text = Rgb(10, 11, 12);
        for mode in [
            ColorMode::TrueColor,
            ColorMode::Indexed256,
            ColorMode::Ansi16,
        ] {
            assert_eq!(mascot(&theme, mode), mascot(&alternate, mode));
        }
    }
}
