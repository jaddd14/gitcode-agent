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
/// Maximum status panel width from the reference layout.
pub const INFO_PANEL_WIDTH: usize = 96;
const PANEL_LEFT_WIDTH: usize = 26;
/// Logo width in terminal cells for the inline graphics fallback.
pub const LOGO_COLUMNS: usize = 10;
/// Logo height in terminal cells for the inline graphics fallback.
pub const LOGO_ROWS: usize = 5;
/// Number of interior rows in the wide information panel.
pub const INFO_PANEL_ROWS: usize = 14;
/// Logo's zero-based column inside the information panel.
pub const LOGO_COLUMN: usize = 1 + (PANEL_LEFT_WIDTH - LOGO_COLUMNS) / 2;
/// Logo width in source pixels.
pub const LOGO_PIXEL_WIDTH: usize = 90;
/// Logo height in source pixels.
pub const LOGO_PIXEL_HEIGHT: usize = 77;
/// Cropped, transparent logo supplied with the reference design.
pub const LOGO_PNG: &[u8] = include_bytes!("gitcode-logo.png");
// Compact text emblem based on the user-provided winged reference, with G at its center.
const WINGED_G: [&str; 14] = [
    "      ⢀⣄⣂⡆⢠⣐⣢ ⣆⣢⣀",
    "  ⣀⣠⡤⢬⣭⣭⣭⣵⡌⠙⠋⣤⣭⣭⣭⣭⠤⣤⣀⡀",
    "⠐⠿⠿⢉⣠⡾⠉⠟⠁⠙⠇⠸⠁⠽⠁⠈⠯⠙⣧⣌⡙⠿⠷",
    "   ⠈⠁    ⠒⠒⠂⠐⠒⠂    ⠉⠁",
    "        ⣠⣴⣾⡿⠿⢿⣷⡆",
    "       ⢰⣿⣿⠁    ⠁",
    "       ⢸⣿⣿  ⠿⢿⣿⡇",
    "        ⠻⣿⣷⣦⣤⣼⣿⡇",
    "          ⠉⠉⠉⠉⠁",
    "         ⠉⠩⣤⣀⠉⠁",
    "           ⢀⡉",
    "         ⢠⠔⢀⠉⢤⡄",
    "         ⠈⢄⠨ ⡌⠁",
    "          ⠈⠠⠉",
];
const WINGED_G_ASCII: [&str; 14] = [
    "      .:::.  .:::.      ",
    "  ..:::::::. .:::::::.. ",
    " .::'  '..'   '..'  '::.",
    "          ...           ",
    "        .:::::.         ",
    "       ::'   `:         ",
    "       ::  .:::         ",
    "       `::.  ::         ",
    "        `:::::'         ",
    "         .::.           ",
    "          ::            ",
    "         :  :           ",
    "          ::            ",
    "          '             ",
];
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
    /// Render a static initial panel, suitable for snapshots and noninteractive output.
    pub fn lines(&self, width: usize, theme: &Theme, mode: ColorMode) -> Vec<String> {
        let status = Status {
            model: self.model.into(),
            branch: self.branch.into(),
            ..Status::default()
        };
        let mut rows = self.live_lines(width, &status);
        if mode != ColorMode::None {
            rows = rows
                .iter()
                .map(|row| widgets::style_frame_row_with_panel(row, theme, mode, false))
                .collect();
        }
        rows
    }

    /// Render the command catalog above the prompt composer.
    pub fn live_lines(&self, width: usize, status: &Status) -> Vec<String> {
        self.live_lines_for_size(width, status, usize::MAX)
    }

    /// Keep the emblem compact and fit the panel to the terminal height.
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
        let glyphs = super::widgets::Glyphs::detect();
        let inner_width = width.saturating_sub(2);
        let body = if inner_width < 72 || height < INFO_PANEL_ROWS + 8 {
            vec![
                widgets::fit("/help — список команд", inner_width),
                widgets::fit("/model — выбрать модель", inner_width),
                widgets::fit("/context — состав контекста", inner_width),
                widgets::fit("/permissions — режим доступа", inner_width),
            ]
        } else {
            let left_width = PANEL_LEFT_WIDTH;
            let columns = inner_width.saturating_sub(left_width + 6);
            let middle_width = columns.div_ceil(2) + 2;
            let right_width = columns.saturating_sub(middle_width);
            let middle = [
                "СЕССИЯ И КОНТЕКСТ",
                "/help — список команд",
                "/clear — новый диалог",
                "/compact — сжать контекст",
                "/config — настройки",
                "/model — выбрать модель",
                "/provider — провайдер модели",
                "/usage — расход токенов",
                "/cost — стоимость сессии",
                "/status — статус проекта",
                "/context — состав контекста",
                "/resume — открыть сессию",
                "/rewind — откатить шаг",
                "/plan — режим планирования",
            ];
            let right = [
                "ПРОЕКТ И ИНСТРУМЕНТЫ",
                "/permissions — режим доступа",
                "/agents — список агентов",
                "/mcp — MCP-серверы",
                "/init — инструкции проекта",
                "/review — обзор изменений",
                "/diff — показать diff",
                "/commit — подготовить коммит",
                "/memory — память проекта",
                "/doctor — диагностика",
                "/theme — цветовая тема",
                "/vim — Vim-клавиши",
                "/export — сохранить диалог",
                "/copy — скопировать ответ",
            ];
            let divider = if glyphs.ascii { " | " } else { " │ " };
            let blank_divider = "   ";
            let emblem = if glyphs.ascii {
                &WINGED_G_ASCII
            } else {
                &WINGED_G
            };
            (0..INFO_PANEL_ROWS)
                .map(|row| {
                    let row_divider = if (4..9).contains(&row) {
                        divider
                    } else {
                        blank_divider
                    };
                    format!(
                        "{}{}{}{}{}",
                        widgets::fit(
                            &center(&widgets::fit(emblem[row], 24), left_width),
                            left_width
                        ),
                        row_divider,
                        widgets::fit(middle[row], middle_width),
                        row_divider,
                        widgets::fit(right[row], right_width),
                    )
                })
                .collect()
        };
        // Render the columns directly, without a title or outer frame.
        let mut rows = body
            .iter()
            .map(|line| format!(" {} ", widgets::fit(line, inner_width)))
            .collect::<Vec<_>>();
        rows.extend(std::iter::repeat_n(String::new(), HEADER_GAP));
        rows
    }
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
            assert!(rows.iter().any(|row| row.contains("/help")));
            assert!(rows.iter().any(|row| row.contains("/permissions")));
            assert!(!rows.iter().any(|row| row.contains("gitcode v")));
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
        assert!(rows.iter().all(|row| !row.contains(&fill)));
        assert!(rows
            .iter()
            .any(|row| row.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c))));
        assert!(WINGED_G_ASCII.iter().all(|row| row.is_ascii()));
        let status = Status {
            model: "test".into(),
            branch: "main".into(),
            ..Status::default()
        };
        assert!(header.live_lines_for_size(95, &status, 18).len() < rows.len());
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
                if width >= 74 {
                    for (index, row) in rows.iter().enumerate() {
                        let row = plain(row);
                        let expected = if (4..9).contains(&index) { 2 } else { 0 };
                        assert_eq!(row.matches('│').count(), expected, "{row}");
                    }
                }
                assert!(rows.iter().any(|row| plain(row).contains("/model")));
                assert!(rows.iter().any(|row| plain(row).contains("/context")));
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
