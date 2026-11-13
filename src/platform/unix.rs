//! Unix platform behavior, kept independent of native Windows adapters.

/// Construct an MCP executable without an intermediate shell.
pub fn mcp_command(program: &str, args: &[String]) -> Result<tokio::process::Command, String> {
    let mut command = tokio::process::Command::new(program);
    command.args(args);
    Ok(command)
}

use super::{Shell, ShellKind};
use std::path::PathBuf;

/// Resolve the configured shell; auto preserves the original /bin/bash behavior.
pub fn detect_shell(selection: &str) -> Result<Shell, String> {
    match selection {
        "auto" | "bash" => Ok(Shell {
            kind: ShellKind::Bash,
            executable: PathBuf::from("/bin/bash"),
        }),
        _ => Err("this platform supports the bash shell".into()),
    }
}
