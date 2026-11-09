//! Conservative terminal-extension policy, not a terminal protocol probe.
//!
//! Host environment hints are allowlisted; unknown hosts and multiplexers get
//! ordinary input/output. Hints can be stale (for example after SSH), so these
//! are best-effort assumptions, not negotiated support. We intentionally avoid
//! `supports_keyboard_enhancement()` at startup: crossterm 0.29's Unix query can
//! wait two seconds and contend with the event reader. Its Windows implementation
//! always returns false, and keyboard enhancement commands lack a WinAPI backend.

use std::io::{self, Write};

/// Privacy-safe optional host feedback. No API accepts user/provider text.
///
/// OSC 2 and OSC 9 are documented at <https://wezterm.org/escape-sequences.html>.
/// Windows console OSC 2 is documented at
/// <https://learn.microsoft.com/en-us/windows/console/console-virtual-terminal-sequences#window-title>.
/// Windows Terminal is title-only: OSC 9 notification behavior is not assumed.
/// Environment identity is a conservative hint, not protocol negotiation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalFeedback {
    title: bool,
    notification: bool,
}

impl TerminalFeedback {
    /// Detect feedback support without terminal probes or background processes.
    pub fn detect(interactive: bool) -> Self {
        Self::detect_with(interactive, |name| std::env::var(name).ok())
    }

    /// Injectable detection for deterministic tests, with multiplexer suppression.
    pub fn detect_with(interactive: bool, mut env: impl FnMut(&str) -> Option<String>) -> Self {
        if !interactive {
            return Self::default();
        }
        let term = env("TERM").unwrap_or_default();
        if matches!(term.as_str(), "dumb" | "unknown")
            || term.starts_with("screen")
            || term.starts_with("tmux")
            || env("TMUX").is_some()
            || env("STY").is_some()
        {
            return Self::default();
        }
        let wezterm = env("TERM_PROGRAM").as_deref() == Some("WezTerm");
        let windows_terminal = env("WT_SESSION").is_some_and(|value| !value.is_empty());
        Self {
            title: wezterm || windows_terminal,
            notification: wezterm,
        }
    }

    /// Set the static product title after terminal ownership is acquired.
    /// This never reads or replaces a title with conversation/project metadata.
    pub fn set_title(self, writer: &mut impl Write) -> io::Result<()> {
        if self.title {
            writer.write_all(b"\x1b]2;gitcode\x1b\\")?;
            writer.flush()?;
        }
        Ok(())
    }

    /// Notify only after a successful busy turn resolves; not on cancel/error.
    /// The host/OS may suppress notifications; a successful write is not evidence
    /// that a desktop notification was displayed. No reply/callback is requested.
    pub fn notify_completed_turn(self, writer: &mut impl Write) -> io::Result<()> {
        if self.notification {
            writer.write_all(b"\x1b]9;gitcode: Turn completed\x1b\\")?;
            writer.flush()?;
        }
        Ok(())
    }
}

/// Explicit runtime policy for optional terminal protocols.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Request disambiguated keys on a known compatible Unix terminal.
    pub kitty_keyboard: bool,
    /// Graphics are unsupported by our text renderer, even on a Kitty host.
    pub kitty_graphics: bool,
    /// Wrap live writes in DEC synchronized-output mode (2026).
    pub synchronized_output: bool,
}

impl Capabilities {
    /// Detect without terminal IO, subprocesses, or blocking protocol queries.
    pub fn detect(interactive: bool) -> Self {
        Self::detect_with(interactive, cfg!(windows), |name| std::env::var(name).ok())
    }

    /// Injectable environment/platform boundary; never mutates process globals.
    ///
    /// Windows Terminal/conhost, generic xterm, and version-dependent protocols
    /// default off. WezTerm is allowlisted for synchronized output only; its Kitty
    /// keyboard support is opt-in. Kitty/foot enable keyboard on Unix only.
    pub fn detect_with(
        interactive: bool,
        windows: bool,
        mut env: impl FnMut(&str) -> Option<String>,
    ) -> Self {
        if !interactive {
            return Self::default();
        }
        let term = env("TERM").unwrap_or_default();
        if term == "dumb" || term == "unknown" {
            return Self::default();
        }
        // An outer host's hints do not establish multiplexer passthrough support.
        if env("TMUX").is_some()
            || env("STY").is_some()
            || term.starts_with("screen")
            || term.starts_with("tmux")
        {
            return Self::default();
        }
        let program = env("TERM_PROGRAM").unwrap_or_default();
        let kitty = term == "xterm-kitty" || program == "kitty";
        let foot = term == "foot" || term == "foot-extra";
        let wezterm = program == "WezTerm";
        Self {
            kitty_keyboard: !windows && (kitty || foot),
            // No graphics encoder/painter exists; never emit graphics commands.
            kitty_graphics: false,
            synchronized_output: (!windows && (kitty || foot)) || wezterm,
        }
    }

    /// Enable only the keyboard extension consumed by the existing editor.
    /// The session's TerminalGuard owns the matching pop after a successful push.
    /// Windows remains on native/legacy input even for explicitly injected flags.
    pub fn enable_keyboard(self, writer: &mut impl Write) -> io::Result<()> {
        #[cfg(not(windows))]
        if self.kitty_keyboard {
            use crossterm::{
                event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags},
                QueueableCommand,
            };
            writer.queue(PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
            ))?;
            writer.flush()?;
        }
        #[cfg(windows)]
        let _ = (self, writer);
        Ok(())
    }
}
