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

/// Single source of terminal visual identity.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Main background.
    pub bg: Rgb,
    /// Raised panel background.
    pub panel: Rgb,
    /// Structural border.
    pub border: Rgb,
    /// Strong purple structure.
    pub purple_deep: Rgb,
    /// Purple secondary accent.
    pub purple: Rgb,
    /// Soft purple decoration.
    pub purple_soft: Rgb,
    /// Strong pink action.
    pub pink_deep: Rgb,
    /// Primary action accent.
    pub pink: Rgb,
    /// Soft pink decoration.
    pub pink_soft: Rgb,
    /// Main text.
    pub text: Rgb,
    /// Metadata text.
    pub dim: Rgb,
    /// Success indicator.
    pub ok: Rgb,
    /// Warning indicator.
    pub warn: Rgb,
    /// Error indicator.
    pub error: Rgb,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: Rgb(19, 11, 29),
            panel: Rgb(28, 17, 40),
            border: Rgb(59, 35, 86),
            purple_deep: Rgb(74, 29, 122),
            purple: Rgb(122, 63, 181),
            purple_soft: Rgb(165, 123, 214),
            pink_deep: Rgb(163, 20, 79),
            pink: Rgb(214, 51, 108),
            pink_soft: Rgb(240, 101, 149),
            text: Rgb(239, 230, 247),
            dim: Rgb(150, 132, 173),
            ok: Rgb(123, 211, 137),
            warn: Rgb(244, 201, 93),
            error: Rgb(255, 92, 122),
        }
    }
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
    fn flat_fallbacks_and_plain_output() {
        assert_eq!(
            foreground(Theme::default().pink, ColorMode::TrueColor),
            "\x1b[38;2;214;51;108m"
        );
        assert_eq!(foreground(Theme::default().pink, ColorMode::None), "");
        assert_eq!(
            detect(false, false, Some("truecolor"), None),
            ColorMode::None
        );
        assert_eq!(detect(true, true, Some("truecolor"), None), ColorMode::None);
        assert_eq!(nearest(Rgb(255, 0, 0), 16), 9);
        assert_eq!(indexed(255), Rgb(238, 238, 238));
    }
}
