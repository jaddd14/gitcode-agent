//! Original flat sakura bitmap and bundled, network-free startup header.

use crate::{
    theme::{foreground, ColorMode, Rgb, Theme},
    tui::{statusbar::Status, widgets},
};

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
/// One blank row between the status panel and input composer.
pub const HEADER_GAP: usize = 1;
/// The startup composition may use the full terminal width.
pub const INFO_PANEL_WIDTH: usize = usize::MAX;
const COPY_WIDTH: usize = 32;
const RELEASES_MIN_WIDTH: usize = 78;
const PANEL_LEFT_WIDTH: usize = 15;
/// Logo width in terminal cells for the inline graphics fallback.
pub const LOGO_COLUMNS: usize = 10;
/// Logo height in terminal cells for the inline graphics fallback.
pub const LOGO_ROWS: usize = 5;
/// Number of rows in the wide startup composition.
pub const INFO_PANEL_ROWS: usize = 11;
/// Logo's zero-based column inside the information panel.
pub const LOGO_COLUMN: usize = 1 + (PANEL_LEFT_WIDTH - LOGO_COLUMNS) / 2;
/// Logo width in source pixels.
pub const LOGO_PIXEL_WIDTH: usize = 90;
/// Logo height in source pixels.
pub const LOGO_PIXEL_HEIGHT: usize = 77;
/// Cropped, transparent logo supplied with the reference design.
pub const LOGO_PNG: &[u8] = include_bytes!("gitcode-logo.png");
// Compact text emblem based on the user-provided winged reference, with G at its center.
const WINGED_G: [&str; INFO_PANEL_ROWS] = [
    "",
    "   ⢀⣄⡀  ⢀⣄⡀",
    " ⢀⣴⣿⣿⣿⣶⣶⣿⣿⣷⡀",
    "⠰⣿⡿⠉⠁⢀⣤⣤⡀⠈⠙⠿⣷",
    " ⠈⠁  ⢸⣿⣿⣿⡇  ⠈⠁",
    "  ⣠⣀  ⠘⠛⠛⠃  ⣀⣄",
    " ⠘⠋      ⠙⠃",
    "",
    "",
    "",
    "",
];
const WINGED_G_ASCII: [&str; INFO_PANEL_ROWS] = [
    "",
    "   .:.  .:.",
    " ..::::  ::::..",
    " :::'  .::.  ':::",
    " '   :::::    '",
    "  ::   ''   ::",
    " '            '",
    "",
    "",
    "",
    "",
];
/// Release highlights from the bundled changelog, available without network access.
pub const NEWS: [(&str, &str); 2] = [
    ("0.2.2", "Надёжные вызовы инструментов"),
    ("0.2.1", "Темы и цвета интерфейса"),
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
    /// Render a static initial composition, suitable for snapshots and noninteractive output.
    pub fn lines(&self, width: usize, theme: &Theme, mode: ColorMode) -> Vec<String> {
        let status = Status {
            model: self.model.into(),
            branch: self.branch.into(),
            ..Status::default()
        };
        self.styled_lines_for_size(width, &status, usize::MAX, theme, mode)
    }

    /// Render the startup composition without terminal styling.
    pub fn live_lines(&self, width: usize, status: &Status) -> Vec<String> {
        self.live_lines_for_size(width, status, usize::MAX)
    }

    /// Fit the mark and the build highlights to the available terminal size.
    pub fn live_lines_for_size(
        &self,
        width: usize,
        _status: &Status,
        height: usize,
    ) -> Vec<String> {
        let width = width.min(INFO_PANEL_WIDTH);
        if width < 4 {
            return vec![String::new()];
        }
        let inner_width = width.saturating_sub(2);
        let ascii = widgets::Glyphs::detect().ascii;
        let bullet = if ascii { "*" } else { "●" };
        let body = if inner_width < 65 || height < INFO_PANEL_ROWS + 11 {
            let compact = [
                " КОМАНДЫ".into(),
                " /help · список команд".into(),
                " /model · модель  /effort · уровень".into(),
                format!(" {bullet} 0.2.2 · надёжные вызовы"),
                format!(" {bullet} 0.2.1 · темы и цвета"),
            ];
            compact
                .into_iter()
                .take(height.saturating_sub(6).max(1))
                .map(|line| widgets::fit(&line, inner_width))
                .collect::<Vec<_>>()
        } else {
            let logo = if ascii { &WINGED_G_ASCII } else { &WINGED_G };
            let copy_width = if width >= RELEASES_MIN_WIDTH {
                COPY_WIDTH
            } else {
                inner_width.saturating_sub(PANEL_LEFT_WIDTH + 3)
            };
            let release_width = inner_width.saturating_sub(PANEL_LEFT_WIDTH + 3 + COPY_WIDTH + 2);
            let copy = [
                center("КОМАНДЫ", copy_width),
                String::new(),
                "/help · список команд".into(),
                "/model · выбор модели".into(),
                "/effort · глубина рассуждений".into(),
                "/resume · открыть диалог".into(),
                "/theme · оформление".into(),
                "/usage · расход и статистика".into(),
                "/permissions · режим доступа".into(),
                String::new(),
                String::new(),
            ];
            let descriptions = if release_width >= 68 {
                [
                    "Разбор нескольких аргументов инструмента в одном блоке ответа и короткие пояснения модели по ходу работы; меню команд не открывается на обычный текст, а выделение продолжается при прокрутке.",
                    "Настройка /theme с четырьмя цветовыми схемами и вариантами белого текста; единая палитра интерфейса, аккуратные границы и исправленная обработка управляющих символов.",
                ]
            } else if release_width >= 40 {
                [
                    "Несколько вызовов инструмента в одном ответе, короткие пояснения по шагам и выделение, которое продолжается при прокрутке.",
                    "Настройка тем через /theme, единая палитра и более аккуратные элементы интерфейса.",
                ]
            } else {
                [
                    "Вызовы инструментов в одном ответе, пояснения по шагам и копирование при прокрутке.",
                    "Выбор тем через /theme и единая палитра интерфейса.",
                ]
            };
            let details = descriptions.map(|description| {
                release_detail(description, release_width.saturating_sub(3), ascii)
            });
            let releases = [
                center("ПОСЛЕДНИЕ ВЕРСИИ", release_width),
                String::new(),
                format!("{bullet}  {}", NEWS[0].0),
                format!("   {}", details[0][0]),
                format!("   {}", details[0][1]),
                String::new(),
                format!("{bullet}  {}", NEWS[1].0),
                format!("   {}", details[1][0]),
                format!("   {}", details[1][1]),
                String::new(),
                String::new(),
                String::new(),
            ];
            let mut rows = (0..INFO_PANEL_ROWS)
                .map(|row| {
                    if width >= RELEASES_MIN_WIDTH {
                        format!(
                            "{}   {}  {}",
                            center(logo[row], PANEL_LEFT_WIDTH),
                            widgets::fit(&copy[row], COPY_WIDTH),
                            widgets::fit(
                                &releases[row],
                                inner_width.saturating_sub(PANEL_LEFT_WIDTH + 3 + COPY_WIDTH + 2)
                            )
                        )
                    } else {
                        format!(
                            "{}   {}",
                            center(logo[row], PANEL_LEFT_WIDTH),
                            widgets::fit(
                                &copy[row],
                                inner_width.saturating_sub(PANEL_LEFT_WIDTH + 3)
                            )
                        )
                    }
                })
                .collect::<Vec<_>>();
            if width < RELEASES_MIN_WIDTH {
                rows.push(widgets::fit(" ПОСЛЕДНИЕ ВЕРСИИ", inner_width));
                for item in [
                    format!(" {bullet} 0.2.2 · надёжные вызовы"),
                    format!(" {bullet} 0.2.1 · темы и цвета"),
                ] {
                    rows.push(widgets::fit(&item, inner_width));
                }
            }
            rows
        };
        let mut rows = body
            .iter()
            .map(|line| format!(" {} ", widgets::fit(line, inner_width)))
            .collect::<Vec<_>>();
        rows.extend(std::iter::repeat_n(String::new(), HEADER_GAP));
        rows
    }

    /// Apply the active theme to the text and mark without drawing a surrounding panel.
    pub fn styled_lines_for_size(
        &self,
        width: usize,
        status: &Status,
        height: usize,
        theme: &Theme,
        mode: ColorMode,
    ) -> Vec<String> {
        let rows = self.live_lines_for_size(width, status, height);
        if mode == ColorMode::None {
            return rows;
        }
        let wide =
            width.min(INFO_PANEL_WIDTH).saturating_sub(2) >= 65 && height >= INFO_PANEL_ROWS + 11;
        rows.into_iter()
            .enumerate()
            .map(|(index, row)| {
                if row.is_empty() {
                    return row;
                }
                let (mark, copy) = if wide {
                    let split = row
                        .char_indices()
                        .nth(PANEL_LEFT_WIDTH + 1)
                        .map_or(row.len(), |(i, _)| i);
                    row.split_at(split)
                } else {
                    ("", row.as_str())
                };
                let color = if index == 0 {
                    theme.rose
                } else {
                    theme.panel_text
                };
                let emphasis = if index == 0 {
                    crate::theme::bold(mode)
                } else {
                    ""
                };
                let (copy, releases) = if wide && width.min(INFO_PANEL_WIDTH) >= RELEASES_MIN_WIDTH
                {
                    let split = copy
                        .char_indices()
                        .nth(3 + COPY_WIDTH + 2)
                        .map_or(copy.len(), |(i, _)| i);
                    copy.split_at(split)
                } else {
                    (copy, "")
                };
                let releases_color = if index == 0 {
                    theme.rose
                } else if matches!(index, 2 | 6 | 11) {
                    theme.text
                } else {
                    theme.panel_text
                };
                format!(
                    "{}{}{}{}{}{}{}{}{}{}",
                    foreground(theme.rose_soft, mode),
                    mark,
                    crate::theme::reset(mode),
                    foreground(color, mode),
                    emphasis,
                    copy,
                    crate::theme::reset(mode),
                    foreground(releases_color, mode),
                    releases,
                    crate::theme::reset(mode)
                )
            })
            .collect()
    }
}

fn release_detail(text: &str, width: usize, ascii: bool) -> [String; 2] {
    let rows = super::markdown::wrap_words(text, width);
    if rows.len() == 1 {
        use unicode_width::UnicodeWidthStr;
        let words: Vec<_> = text.split_whitespace().collect();
        if words.len() > 1 {
            let middle = text.width() / 2;
            let mut split = 1;
            let mut first_width = words[0].width();
            while split + 1 < words.len() && first_width + 1 + words[split].width() <= middle {
                first_width += 1 + words[split].width();
                split += 1;
            }
            return [words[..split].join(" "), words[split..].join(" ")];
        }
    }
    let first = rows.first().cloned().unwrap_or_default();
    let remainder = rows
        .iter()
        .skip(1)
        .map(|s| s.trim())
        .collect::<Vec<_>>()
        .join(" ");
    [first, widgets::ellipsis(&remainder, width, ascii)]
}

fn center(text: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let text_width = text.width().min(width);
    let left = width.saturating_sub(text_width) / 2;
    format!(
        "{}{}{}",
        " ".repeat(left),
        text,
        " ".repeat(width - left - text_width)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn header_closes_at_a_constant_column_with_command_descriptions() {
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
                assert_eq!(row.width(), width.min(INFO_PANEL_WIDTH), "{row}");
            }
            assert!(rows
                .iter()
                .any(|row| crate::tui::widgets::unstyled(row).contains("КОМАНДЫ")));
            assert!(rows
                .iter()
                .any(|row| crate::tui::widgets::unstyled(row).contains("0.2.1")));
            assert!(!rows.join("\n").contains("[DEFAULT]"));
        }
    }

    #[test]
    fn emblem_is_compact_and_does_not_fill_the_background() {
        let header = Header {
            version: "0.2.1",
            model: "test",
            provider: "test",
            cwd: ".",
            branch: "main",
        };
        let theme = Theme::default();
        let rows = header.lines(130, &theme, ColorMode::TrueColor);
        let fill = crate::theme::background(theme.panel, ColorMode::TrueColor);
        assert_eq!(rows.len(), INFO_PANEL_ROWS + HEADER_GAP);
        for (version, _) in NEWS {
            assert!(rows
                .iter()
                .any(|row| widgets::unstyled(row).contains(version)));
        }
        let last = widgets::unstyled(&rows[INFO_PANEL_ROWS - 1]);
        assert!(last.trim().is_empty(), "{last:?}");
        let text = rows
            .iter()
            .map(|row| widgets::unstyled(row))
            .collect::<Vec<_>>()
            .join("\n");
        for removed in [
            "0.3.0", "0.2.0", "/exit", "/clear", "/status", "/context", "/new",
        ] {
            assert!(!text.contains(removed), "{removed} must be gone");
        }
        assert!(text.contains("/permissions"));
        assert!(rows
            .iter()
            .any(|row| widgets::unstyled(row).contains("/usage")));
        assert!(rows.iter().all(|row| !row.contains(&fill)));
        assert!(WINGED_G.iter().any(|row| row.contains('⣿')));
        assert!(WINGED_G_ASCII.iter().all(|row| row.is_ascii()));
        let status = Status {
            model: "test".into(),
            branch: "main".into(),
            ..Status::default()
        };
        assert!(header.live_lines_for_size(95, &status, 18).len() < rows.len());
    }

    #[test]
    fn releases_form_a_separate_borderless_column_aligned_to_logo_bottom() {
        let header = Header {
            version: "0.2.1",
            model: "test",
            provider: "test",
            cwd: ".",
            branch: "main",
        };
        for width in [94, 100, 130, 160] {
            let rows = header.lines(width, &Theme::default(), ColorMode::None);
            let column = 1 + PANEL_LEFT_WIDTH + 3 + COPY_WIDTH + 2;
            for (row, text) in [(0, "ПОСЛЕДНИЕ ВЕРСИИ"), (2, "0.2.2"), (6, "0.2.1")]
            {
                let right: String = rows[row].chars().skip(column).collect();
                assert!(right.contains(text), "width {width}, row {row}: {right}");
            }
            let text = rows.join("\n");
            assert!(!text.contains("0.2.0"));
            for removed in ["/new", "/clear", "/status", "/context", "/exit"] {
                assert!(!text.contains(removed), "{removed} still listed");
            }
            let left: String = rows[0]
                .chars()
                .skip(1 + PANEL_LEFT_WIDTH + 3)
                .take(COPY_WIDTH)
                .collect();
            let right: String = rows[0]
                .chars()
                .skip(column)
                .take(width - column - 1)
                .collect();
            for heading in [&left, &right] {
                let leading = heading.len() - heading.trim_start().len();
                let trailing = heading.len() - heading.trim_end().len();
                assert!(leading.abs_diff(trailing) <= 1, "not centered: {heading:?}");
            }
            assert!(
                rows[8]
                    .chars()
                    .skip(column)
                    .collect::<String>()
                    .trim()
                    .len()
                    > 5
            );
            assert!(rows
                .iter()
                .take(INFO_PANEL_ROWS)
                .all(|row| !row.contains('│')));
        }
    }

    #[test]
    fn release_news_remain_visible_when_columns_do_not_fit() {
        let header = Header {
            version: "0.2.1",
            model: "test",
            provider: "test",
            cwd: ".",
            branch: "main",
        };
        let status = Status::default();
        for (width, height) in [(50, 30), (80, 30), (80, 23), (93, 24)] {
            let rows = header.live_lines_for_size(width, &status, height);
            let text = rows.join("\n");
            for version in ["0.2.2", "0.2.1"] {
                assert!(text.contains(version), "{width}x{height}: {text}");
            }
            assert!(rows.len() + 6 <= height);
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
    fn header_keeps_closed_constant_width_and_status_columns() {
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
                assert!(rows
                    .iter()
                    .all(|row| plain(row).width() == width.min(INFO_PANEL_WIDTH)));
                for row in rows {
                    let row = plain(row);
                    assert!(!row.starts_with(['╭', '╰', '│']));
                    assert!(!row.ends_with(['╮', '╯', '│']));
                }
                assert!(rows.iter().all(|row| !plain(row).contains('│')));
                assert!(rows.iter().any(|row| plain(row).contains("0.2.1")));
                assert!(!rows.join("\n").contains("0.2.0"));
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
