//! Native operating-system boundaries shared by the CLI and tools.

use std::{collections::BTreeMap, path::PathBuf};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;

/// Supported persistent command interpreters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// Bash, including Git for Windows.
    Bash,
    /// PowerShell 7.
    Pwsh,
    /// Windows PowerShell.
    PowerShell,
    /// Windows command processor.
    Cmd,
}

/// An explicitly resolved interpreter and its quoting dialect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    /// Interpreter dialect.
    pub kind: ShellKind,
    /// Native executable path.
    pub executable: PathBuf,
}
impl Shell {
    /// Stable configuration identifier.
    pub fn name(&self) -> &'static str {
        match self.kind {
            ShellKind::Bash => "bash",
            ShellKind::Pwsh => "pwsh",
            ShellKind::PowerShell => "powershell",
            ShellKind::Cmd => "cmd",
        }
    }
    /// Model-facing description of the active quoting rules.
    pub fn description(&self) -> &'static str {
        match self.kind {
            ShellKind::Bash => "Bash: POSIX single/double quoting; persistent cwd and environment.",
            ShellKind::Pwsh | ShellKind::PowerShell => "PowerShell: single quotes are literal; backtick escapes; use Set-Location and $env:NAME. Persistent cwd and environment.",
            ShellKind::Cmd => "cmd.exe: double quotes group paths; caret escapes; use cd /d and set NAME=value. Persistent cwd and environment.",
        }
    }
}

/// Resolve the native home directory without changing the process environment.
pub fn home_dir() -> Option<PathBuf> {
    home_dir_from(&std::env::vars().collect())
}

/// Resolve the native home directory from an injectable environment.
pub fn home_dir_from(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    #[cfg(windows)]
    let name = "USERPROFILE";
    #[cfg(not(windows))]
    let name = "HOME";
    env.get(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Component-aware filesystem boundary comparison of canonical paths.
pub fn path_within(path: &std::path::Path, root: &std::path::Path) -> bool {
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
    #[cfg(windows)]
    {
        let mut path = path.components();
        root.components().all(|expected| {
            path.next().is_some_and(|actual| {
                actual.as_os_str() == expected.as_os_str()
                    || actual
                        .as_os_str()
                        .to_str()
                        .zip(expected.as_os_str().to_str())
                        .is_some_and(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
            })
        })
    }
}

/// Reject reserved devices, alternate streams and ambiguous Windows names.
pub fn validate_windows_path(path: &std::path::Path) -> Result<(), String> {
    let text = path.to_string_lossy();
    let text = text
        .strip_prefix(r"\\?\UNC\")
        .or_else(|| text.strip_prefix(r"\\?\"))
        .unwrap_or(&text);
    for (index, part) in text.split(['/', '\\']).enumerate() {
        if part.is_empty() || part == "." || part == ".." {
            continue;
        }
        if index == 0
            && part.len() == 2
            && part.as_bytes()[0].is_ascii_alphabetic()
            && part.ends_with(':')
        {
            continue;
        }
        let stem = part
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches(' ')
            .to_ascii_uppercase();
        if part.ends_with([' ', '.'])
            || part.contains([':', '<', '>', '"', '|', '?', '*'])
            || part.chars().any(char::is_control)
            || matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            )
            || ["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|suffix| {
                    matches!(
                        suffix,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                })
            })
        {
            return Err("reserved or ambiguous Windows path".into());
        }
    }
    Ok(())
}

/// Prepare ANSI output before entering raw terminal mode.
pub fn prepare_terminal() -> std::io::Result<()> {
    #[cfg(windows)]
    enable_ansi_support::enable_ansi_support()?;
    Ok(())
}
