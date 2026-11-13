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
/// Windows Terminal uses a native Windows fallback, never an assumed OSC 9.
/// Environment identity is a conservative hint, not protocol negotiation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalFeedback {
    title: bool,
    notification: bool,
    native_notification: bool,
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
            // WezTerm takes precedence over inherited WT_SESSION hints. The
            // native fallback is additionally platform-gated at execution time.
            native_notification: windows_terminal && !wezterm,
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
            write!(
                writer,
                "\x1b]9;gitcode: {}\x1b\\",
                crate::i18n::text(
                    crate::i18n::Language::default(),
                    crate::i18n::Key::TurnCompleted,
                )
            )?;
            writer.flush()?;
        }
        Ok(())
    }

    /// Submit the static native fallback, when selected by host policy.
    /// Run off the input/render path. Success means submission only, not display;
    /// missing identity, disabled notifications, process errors and timeouts fail.
    /// This never installs/registers an identity or changes notification settings.
    pub async fn notify_completed_turn_native(self) -> io::Result<()> {
        if !self.native_notification {
            return Ok(());
        }
        #[cfg(windows)]
        {
            use std::process::Stdio;
            // Use the built-in Windows PowerShell, not a workspace/PATH executable.
            let root = std::env::var_os("SystemRoot")
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "SystemRoot unavailable"))?;
            let shell = std::path::PathBuf::from(root)
                .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
            let mut command = tokio::process::Command::new(shell);
            command
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-STA",
                    "-Command",
                    WINDOWS_TOAST_SCRIPT,
                ])
                // CREATE_NO_WINDOW: feedback must not open/focus a console.
                .creation_flags(0x08000000)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            let mut child = command.spawn()?;
            wait_for_notification(&mut child, std::time::Duration::from_secs(5)).await
        }
        #[cfg(not(windows))]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows toast notifications require Windows",
        ))
    }
}

// Fixed ASCII script, passed as one argument. Only the pre-existing Windows
// PowerShell Start application identity is allowed; never invent/register one or
// borrow a provider/project/user value. Toast attribution therefore is PowerShell.
#[cfg(any(windows, test))]
const WINDOWS_TOAST_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
try {
    $appId = 'Microsoft.Windows.PowerShell'
    $app = Get-StartApps | Where-Object { $_.AppID -ceq $appId } | Select-Object -First 1
    if ($null -eq $app) { throw 'Existing PowerShell application identity unavailable' }
    [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null
    [Windows.UI.Notifications.ToastNotification, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null
    [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] > $null
    $xml = New-Object Windows.Data.Xml.Dom.XmlDocument
    $xml.LoadXml('<toast><visual><binding template="ToastGeneric"><text>gitcode</text><text>&#x425;&#x43E;&#x434;&#x20;&#x437;&#x430;&#x432;&#x435;&#x440;&#x448;&#x451;&#x43D;</text></binding></visual><audio silent="true"/></toast>')
    $notifier = [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($appId)
    if ([int]$notifier.Setting -ne 0) { throw 'Notifications unavailable or disabled' }
    $toast = [Windows.UI.Notifications.ToastNotification]::new($xml)
    $toast.ExpirationTime = [DateTimeOffset]::Now.AddMinutes(1)
    $notifier.Show($toast)
    exit 0
} catch { exit 1 }
"#;

#[cfg(any(windows, test))]
async fn wait_for_notification(
    child: &mut tokio::process::Child,
    timeout: std::time::Duration,
) -> io::Result<()> {
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(_)) => Err(io::Error::other("Windows toast submission failed")),
        Ok(Err(error)) => Err(error),
        Err(_) => {
            // Explicitly kill on timeout, then allow bounded reaping. kill_on_drop
            // also covers cancellation/runtime shutdown and cleanup errors.
            let _ = child.start_kill();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(1), child.wait()).await;
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Windows toast submission timed out",
            ))
        }
    }
}

#[cfg(test)]
mod notification_tests {
    use super::*;

    fn feedback(interactive: bool, vars: &[(&str, &str)]) -> TerminalFeedback {
        TerminalFeedback::detect_with(interactive, |key| {
            vars.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn native_fallback_is_only_for_unmultiplexed_interactive_windows_terminal() {
        let wt = [("WT_SESSION", "session")];
        assert!(feedback(true, &wt).native_notification);
        assert!(!feedback(false, &wt).native_notification);
        for vars in [
            vec![],
            vec![("WT_SESSION", "")],
            vec![("TERM_PROGRAM", "WezTerm"), ("WT_SESSION", "session")],
            vec![("WT_SESSION", "session"), ("TERM", "dumb")],
            vec![("WT_SESSION", "session"), ("TERM", "unknown")],
            vec![("WT_SESSION", "session"), ("TERM", "screen-256color")],
            vec![("WT_SESSION", "session"), ("TERM", "tmux-256color")],
            vec![("WT_SESSION", "session"), ("TMUX", "present")],
            vec![("WT_SESSION", "session"), ("STY", "present")],
        ] {
            assert!(!feedback(true, &vars).native_notification, "{vars:?}");
        }
    }

    #[test]
    fn toast_script_is_fixed_and_checks_existing_identity_and_settings() {
        assert!(WINDOWS_TOAST_SCRIPT.is_ascii());
        assert!(WINDOWS_TOAST_SCRIPT.contains("$appId = 'Microsoft.Windows.PowerShell'"));
        assert!(WINDOWS_TOAST_SCRIPT.contains("Get-StartApps"));
        assert!(WINDOWS_TOAST_SCRIPT.contains("$_.AppID -ceq $appId"));
        assert!(WINDOWS_TOAST_SCRIPT.contains("if ($null -eq $app) { throw"));
        assert!(WINDOWS_TOAST_SCRIPT.contains("$notifier.Setting -ne 0"));
        assert!(WINDOWS_TOAST_SCRIPT.contains("<text>gitcode</text><text>&#x425;&#x43E;&#x434;&#x20;&#x437;&#x430;&#x432;&#x435;&#x440;&#x448;&#x451;&#x43D;</text>"));
        // ASCII numeric XML entities avoid PowerShell command-line encoding
        // ambiguities while matching the existing fixed Russian UI message.
        let encoded_body: String =
            crate::i18n::text(crate::i18n::Language::Ru, crate::i18n::Key::TurnCompleted)
                .chars()
                .map(|character| format!("&#x{:X};", u32::from(character)))
                .collect();
        assert!(WINDOWS_TOAST_SCRIPT.contains(&format!("<text>{encoded_body}</text>")));
        assert!(WINDOWS_TOAST_SCRIPT.contains("$notifier.Show($toast)"));
        assert!(WINDOWS_TOAST_SCRIPT.contains("catch { exit 1 }"));
        for forbidden in [
            "$env:",
            "$args",
            "ReadToEnd",
            "Register-",
            "New-Item",
            "Set-Item",
        ] {
            assert!(!WINDOWS_TOAST_SCRIPT.contains(forbidden));
        }
    }

    #[tokio::test]
    async fn disabled_feedback_is_a_noop() -> io::Result<()> {
        TerminalFeedback::default()
            .notify_completed_turn_native()
            .await
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn inherited_windows_hint_cannot_launch_a_helper_on_unix() {
        assert_eq!(
            feedback(true, &[("WT_SESSION", "session")])
                .notify_completed_turn_native()
                .await
                .err()
                .map(|error| error.kind()),
            Some(io::ErrorKind::Unsupported)
        );
    }

    // Exercise the real child wait/kill boundary without displaying notifications.
    #[cfg(unix)]
    #[tokio::test]
    async fn child_exit_failure_and_timeout_are_not_success() -> io::Result<()> {
        use std::{process::Stdio, time::Duration};
        for (script, expected) in [
            ("exit 0", None),
            ("exit 7", Some(io::ErrorKind::Other)),
            ("exec sleep 30", Some(io::ErrorKind::TimedOut)),
        ] {
            let mut child = tokio::process::Command::new("/bin/sh")
                .args(["-c", script])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()?;
            let result = wait_for_notification(&mut child, Duration::from_millis(100)).await;
            assert_eq!(result.err().map(|error| error.kind()), expected);
            assert!(child.try_wait()?.is_some(), "child must be reaped");
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
