//! Original flat sakura bitmap and bundled, network-free startup header.

use super::clip;
use crate::theme::{foreground, ColorMode, Rgb, Theme};
use unicode_width::UnicodeWidthStr;

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
        b'1' => theme.pink_deep,
        b'2' => theme.pink,
        b'3' => theme.pink_soft,
        b'4' => theme.text,
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
        let text = [
            format!("gitcode {}", self.version),
            format!("{} · {}", self.model, self.provider),
            format!("{} · {}", self.cwd, self.branch),
            String::new(),
            NEWS[0].into(),
            NEWS[1].into(),
        ];
        let mut rows = if width >= 40 {
            mascot(theme, mode)
                .into_iter()
                .zip(text)
                .map(|(logo, text)| format!("{logo}   {}", clip(&text, width.saturating_sub(19))))
                .collect::<Vec<_>>()
        } else {
            text.iter().map(|s| clip(s, width)).collect()
        };
        while rows
            .last()
            .is_some_and(|r| UnicodeWidthStr::width(r.as_str()) == 0)
        {
            rows.pop();
        }
        rows.extend(std::iter::repeat_n(String::new(), HEADER_GAP));
        rows
    }
}
