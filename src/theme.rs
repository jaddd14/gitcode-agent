//! Flat terminal palette and deterministic capability fallbacks.

/// Flat sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Terminal output capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// ANSI 24-bit color.
    TrueColor,
    /// Indexed xterm palette.
    Indexed256,
    /// Basic ANSI palette.
    Ansi16,
    /// Plain output for NO_COLOR or non-TTY destinations.
    None,
}

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

/// Available rose accent palettes.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RosePreset {
    /// Classic rose accent.
    #[default]
    Rose,
    /// Darker rose accent.
    DarkRose,
    /// Muted rose accent.
    DustyRose,
    /// Wine-toned rose accent.
    WineRose,
}

impl RosePreset {
    /// Stable picker ordering.
    pub const ALL: [Self; 4] = [Self::Rose, Self::DarkRose, Self::DustyRose, Self::WineRose];

    /// User-facing palette name.
    pub fn label(self) -> &'static str {
        match self {
            Self::Rose => "Роза",
            Self::DarkRose => "Тёмная роза",
            Self::DustyRose => "Пыльная роза",
            Self::WineRose => "Винная роза",
        }
    }

    fn color(self) -> Rgb {
        match self {
            Self::Rose => Rgb(184, 74, 116),
            Self::DarkRose => Rgb(144, 57, 88),
            Self::DustyRose => Rgb(166, 99, 123),
            Self::WineRose => Rgb(143, 53, 83),
        }
    }
}

/// Text-white intensity independent of accent selection.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WhiteMode {
    /// Slightly softened white.
    #[default]
    Soft,
    /// Full-intensity white.
    Pure,
}

/// Serializable user palette preference.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeChoice {
    /// Selected rose accent.
    pub preset: RosePreset,
    /// Selected text-white intensity.
    pub white: WhiteMode,
}

static ACTIVE: AtomicU8 = AtomicU8::new(0);

/// Read the process-local palette preference without accessing configuration.
pub fn active_choice() -> ThemeChoice {
    let value = ACTIVE.load(Ordering::Relaxed);
    ThemeChoice {
        preset: RosePreset::ALL[usize::from(value & 3)],
        white: if value & 4 == 0 {
            WhiteMode::Soft
        } else {
            WhiteMode::Pure
        },
    }
}

/// Apply the process-local palette without writing user configuration.
pub fn set_active(choice: ThemeChoice) {
    ACTIVE.store(
        choice.preset as u8
            | if choice.white == WhiteMode::Pure {
                4
            } else {
                0
            },
        Ordering::Relaxed,
    );
}

/// Single source of terminal visual identity.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Main background.
    pub bg: Rgb,
    /// Raised panel background.
    pub panel: Rgb,
    /// Panel hover / subtle highlight background.
    pub panel_hover: Rgb,
    /// Selection row background.
    pub selection: Rgb,
    /// Structural border.
    pub border: Rgb,
    /// Strong rose action.
    pub rose_deep: Rgb,
    /// Primary action accent.
    pub rose: Rgb,
    /// Soft rose decoration.
    pub rose_soft: Rgb,
    /// Main text and metadata.
    pub text: Rgb,
    /// Success indicator.
    pub ok: Rgb,
    /// Warning indicator.
    pub warn: Rgb,
    /// Error indicator.
    pub error: Rgb,
    /// Flat background tint for added diff lines.
    pub diff_add_bg: Rgb,
    /// Flat background tint for deleted diff lines.
    pub diff_del_bg: Rgb,
}

impl Default for Theme {
    fn default() -> Self {
        Self::from_choice(active_choice())
    }
}

impl Theme {
    /// Resolve a palette deterministically without changing the active preference.
    pub fn from_choice(choice: ThemeChoice) -> Self {
        let rose = choice.preset.color();
        Self {
            bg: Rgb(20, 17, 19),
            panel: Rgb(29, 24, 27),
            panel_hover: Rgb(42, 31, 36),
            selection: Rgb(57, 35, 44),
            border: Rgb(87, 52, 66),
            rose,
            rose_soft: shift_hsl(rose, -0.05, 0.12),
            rose_deep: shift_hsl(rose, 0.0, -0.18),
            text: match choice.white {
                WhiteMode::Soft => Rgb(237, 233, 235),
                WhiteMode::Pure => Rgb(255, 255, 255),
            },
            ok: Rgb(123, 211, 137),
            warn: Rgb(244, 201, 93),
            error: Rgb(255, 92, 122),
            diff_add_bg: Rgb(22, 43, 34),
            diff_del_bg: Rgb(54, 20, 36),
        }
    }
}

fn to_hsl(Rgb(r, g, b): Rgb) -> (f64, f64, f64) {
    let (r, g, b) = (
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let light = (max + min) / 2.0;
    if delta == 0.0 {
        return (0.0, 0.0, light);
    }
    let hue = if max == r {
        ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    } / 6.0;
    (hue, delta / (1.0 - (2.0 * light - 1.0).abs()), light)
}

fn shift_hsl(color: Rgb, saturation: f64, lightness: f64) -> Rgb {
    let (h, s, l) = to_hsl(color);
    let s = (s + saturation).clamp(0.0, 1.0);
    let l = (l + lightness).clamp(0.0, 1.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h * 6.0).rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match (h * 6.0).floor() as u8 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let channel = |v: f64| ((v + l - c / 2.0) * 255.0).round() as u8;
    Rgb(channel(r), channel(g), channel(b))
}

const ANSI: [Rgb; 16] = [
    Rgb(0, 0, 0),
    Rgb(128, 0, 0),
    Rgb(0, 128, 0),
    Rgb(128, 128, 0),
    Rgb(0, 0, 128),
    Rgb(128, 0, 128),
    Rgb(0, 128, 128),
    Rgb(192, 192, 192),
    Rgb(128, 128, 128),
    Rgb(255, 0, 0),
    Rgb(0, 255, 0),
    Rgb(255, 255, 0),
    Rgb(0, 0, 255),
    Rgb(255, 0, 255),
    Rgb(0, 255, 255),
    Rgb(255, 255, 255),
];

fn indexed(index: u8) -> Rgb {
    match index {
        0..=15 => ANSI[index as usize],
        16..=231 => {
            let n = index - 16;
            let component = |x| if x == 0 { 0 } else { 55 + 40 * x };
            Rgb(component(n / 36), component(n / 6 % 6), component(n % 6))
        }
        _ => {
            let v = 8 + 10 * (index - 232);
            Rgb(v, v, v)
        }
    }
}

fn nearest(color: Rgb, count: u16) -> u8 {
    (0..count)
        .min_by_key(|i| {
            let p = indexed(*i as u8);
            let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
            d(color.0, p.0) + d(color.1, p.1) + d(color.2, p.2)
        })
        .unwrap_or(0) as u8
}

/// Format a foreground escape; plain mode emits nothing.
pub fn foreground(color: Rgb, mode: ColorMode) -> String {
    match mode {
        ColorMode::TrueColor => format!("\x1b[38;2;{};{};{}m", color.0, color.1, color.2),
        ColorMode::Indexed256 => format!("\x1b[38;5;{}m", nearest(color, 256)),
        ColorMode::Ansi16 => {
            let n = nearest(color, 16);
            format!("\x1b[{}m", if n < 8 { 30 + n } else { 90 + n - 8 })
        }
        ColorMode::None => String::new(),
    }
}

/// Format a background escape; plain mode emits nothing.
pub fn background(color: Rgb, mode: ColorMode) -> String {
    match mode {
        ColorMode::TrueColor => format!("\x1b[48;2;{};{};{}m", color.0, color.1, color.2),
        ColorMode::Indexed256 => format!("\x1b[48;5;{}m", nearest(color, 256)),
        ColorMode::Ansi16 => {
            let n = nearest(color, 16);
            format!("\x1b[{}m", if n < 8 { 40 + n } else { 100 + n - 8 })
        }
        ColorMode::None => String::new(),
    }
}

/// Format a bold escape; plain mode emits nothing.
pub fn bold(mode: ColorMode) -> &'static str {
    if mode == ColorMode::None {
        ""
    } else {
        "\x1b[1m"
    }
}

/// Format an attribute reset escape; plain mode emits nothing.
pub fn reset(mode: ColorMode) -> &'static str {
    if mode == ColorMode::None {
        ""
    } else {
        "\x1b[0m"
    }
}

/// Choose output mode without querying the terminal or making network calls.
pub fn detect(tty: bool, no_color: bool, colorterm: Option<&str>, term: Option<&str>) -> ColorMode {
    if !tty || no_color || term == Some("dumb") {
        ColorMode::None
    } else if matches!(colorterm, Some("truecolor" | "24bit")) {
        ColorMode::TrueColor
    } else if term.is_some_and(|s| s.contains("256color")) {
        ColorMode::Indexed256
    } else {
        ColorMode::Ansi16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rose_variants_follow_hsl_offsets() {
        for preset in RosePreset::ALL {
            let theme = Theme::from_choice(ThemeChoice {
                preset,
                white: WhiteMode::Soft,
            });
            let (h, s, l) = to_hsl(theme.rose);
            let (soft_h, soft_s, soft_l) = to_hsl(theme.rose_soft);
            let (deep_h, deep_s, deep_l) = to_hsl(theme.rose_deep);
            assert!((h - soft_h).abs() < 0.005);
            assert!((h - deep_h).abs() < 0.005);
            assert!((soft_s - (s - 0.05)).abs() < 0.01);
            assert!((soft_l - (l + 0.12)).abs() < 0.005);
            assert!((deep_s - s).abs() < 0.01);
            assert!((deep_l - (l - 0.18)).abs() < 0.005);
        }
        let pure = Theme::from_choice(ThemeChoice {
            white: WhiteMode::Pure,
            ..ThemeChoice::default()
        });
        assert_eq!(pure.text, Rgb(255, 255, 255));
    }

    #[test]
    fn rose_variants_use_hsl_offsets() {
        for preset in RosePreset::ALL {
            let theme = Theme::from_choice(ThemeChoice {
                preset,
                white: WhiteMode::Soft,
            });
            let (h, s, l) = to_hsl(theme.rose);
            let (soft_h, soft_s, soft_l) = to_hsl(theme.rose_soft);
            let (deep_h, deep_s, deep_l) = to_hsl(theme.rose_deep);
            assert!((soft_h - h).abs() < 0.005);
            assert!((soft_s - (s - 0.05)).abs() < 0.01);
            assert!((soft_l - (l + 0.12)).abs() < 0.005);
            assert!((deep_h - h).abs() < 0.005);
            assert!((deep_s - s).abs() < 0.01);
            assert!((deep_l - (l - 0.18)).abs() < 0.005);
        }
        assert_eq!(
            Theme::from_choice(ThemeChoice::default()).rose,
            Rgb(184, 74, 116)
        );
        assert_eq!(
            Theme::from_choice(ThemeChoice {
                white: WhiteMode::Pure,
                ..ThemeChoice::default()
            })
            .text,
            Rgb(255, 255, 255)
        );
    }

    #[test]
    fn flat_fallbacks_and_plain_output() {
        let theme = Theme::from_choice(ThemeChoice::default());
        assert_eq!(
            foreground(theme.rose, ColorMode::TrueColor),
            "\x1b[38;2;184;74;116m"
        );
        assert_eq!(foreground(theme.rose, ColorMode::None), "");
        assert_eq!(
            background(theme.panel_hover, ColorMode::TrueColor),
            "\x1b[48;2;42;31;36m"
        );
        assert_eq!(background(theme.selection, ColorMode::None), "");
        assert_eq!(bold(ColorMode::TrueColor), "\x1b[1m");
        assert_eq!(bold(ColorMode::None), "");
        assert_eq!(reset(ColorMode::TrueColor), "\x1b[0m");
        assert_eq!(reset(ColorMode::None), "");
        assert_eq!(theme.diff_add_bg, Rgb(22, 43, 34));
        assert_eq!(theme.diff_del_bg, Rgb(54, 20, 36));
        assert_eq!(
            detect(false, false, Some("truecolor"), None),
            ColorMode::None
        );
        assert_eq!(detect(true, true, Some("truecolor"), None), ColorMode::None);
        assert_eq!(nearest(Rgb(255, 0, 0), 16), 9);
        assert_eq!(indexed(255), Rgb(238, 238, 238));
    }
}
