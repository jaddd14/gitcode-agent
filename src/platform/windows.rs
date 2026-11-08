//! Native Windows discovery and safe process-tree ownership.

use super::{Shell, ShellKind};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn find(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}

fn git_bash() -> Option<PathBuf> {
    // PATH bash may be a WSL shim, which is not a native supported interpreter.
    let mut candidates = Vec::new();
    if let Some(git) = find("git.exe") {
        if let Some(root) = git.parent().and_then(Path::parent) {
            candidates.push(root.join("bin/bash.exe"));
            candidates.push(root.join("usr/bin/bash.exe"));
        }
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(root) = std::env::var_os(variable) {
            let root = PathBuf::from(root);
            candidates.push(root.join("Git/bin/bash.exe"));
            candidates.push(root.join("Programs/Git/bin/bash.exe"));
        }
    }
    if let Some(path) = find("bash.exe") {
        if !path
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("\\windows\\system32\\")
        {
            candidates.push(path);
        }
    }
    // Registry covers nonstandard Git for Windows installation directories.
    for hive in ["HKCU", "HKLM"] {
        if let Ok(output) = Command::new("reg.exe")
            .args([
                "query",
                &format!("{hive}\\SOFTWARE\\GitForWindows"),
                "/v",
                "InstallPath",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                if let Some((_, value)) = line.split_once("REG_SZ") {
                    candidates.push(PathBuf::from(value.trim()).join("bin/bash.exe"));
                }
            }
        }
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// Resolve a supported native shell in Git Bash, pwsh, PowerShell, cmd order.
pub fn detect_shell(selection: &str) -> Result<Shell, String> {
    let choices = [
        ("bash", ShellKind::Bash),
        ("pwsh", ShellKind::Pwsh),
        ("powershell", ShellKind::PowerShell),
        ("cmd", ShellKind::Cmd),
    ];
    for (name, kind) in choices {
        if selection != "auto" && selection != name {
            continue;
        }
        let executable = match kind {
            ShellKind::Bash => git_bash(),
            ShellKind::Cmd => std::env::var_os("COMSPEC")
                .map(PathBuf::from)
                .filter(|path| path.is_file())
                .or_else(|| find("cmd.exe")),
            _ => find(&format!("{name}.exe")),
        };
        if let Some(executable) = executable {
            return Ok(Shell { kind, executable });
        }
    }
    Err(format!("supported shell not found: {selection}"))
}

/// Construct an MCP process, resolving cmd/bat shims with strict argument quoting.
pub fn mcp_command(program: &str, args: &[String]) -> Result<tokio::process::Command, String> {
    let executable = crate::shell::find_executable(program).ok_or("MCP executable not found")?;
    let batch = executable
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
    if !batch {
        let mut command = tokio::process::Command::new(executable);
        command.args(args);
        return Ok(command);
    }
    let quote = |value: &str| -> Result<String, String> {
        if value.contains(['"', '%', '!', '^', '&', '|', '<', '>', '\r', '\n']) {
            return Err("unsupported metacharacter in MCP batch argument".into());
        }
        Ok(format!("\"{value}\""))
    };
    let mut line = quote(&executable.to_string_lossy())?;
    for arg in args {
        line.push(' ');
        line.push_str(&quote(arg)?);
    }
    let mut command = tokio::process::Command::new(
        crate::shell::find_executable("cmd.exe").ok_or("cmd.exe not found")?,
    );
    command
        .args(["/D", "/V:OFF", "/S", "/C"])
        .raw_arg(format!("\"{line}\""));
    Ok(command)
}

/// Best-effort ACL restriction. Callers must not claim Unix permission guarantees.
pub fn restrict_credentials(path: &Path) -> bool {
    let Some(mut grant) = std::env::var_os("USERNAME") else {
        return false;
    };
    grant.push(":F");
    Command::new("icacls.exe")
        .arg(path)
        .args(["/inheritance:r", "/grant:r"])
        .arg(grant)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Kill-on-close ownership for a child and all of its descendants.
pub struct ProcessTree(win32job::Job);
impl ProcessTree {
    /// Attach a freshly spawned, idle interpreter before sending untrusted commands.
    pub fn attach(child: &tokio::process::Child) -> Result<Self, String> {
        let mut limits = win32job::ExtendedLimitInfo::new();
        limits.limit_kill_on_job_close();
        let job = win32job::Job::create_with_limit_info(&limits)
            .map_err(|_| "cannot create process job")?;
        let handle = child.raw_handle().ok_or("child handle unavailable")?;
        job.assign_process(handle as isize)
            .map_err(|_| "cannot attach process job")?;
        Ok(Self(job))
    }
    /// Number of processes currently owned by this job.
    pub fn process_count(&self) -> Result<usize, String> {
        self.0
            .query_process_id_list()
            .map(|ids| ids.len())
            .map_err(|_| "cannot query process job".into())
    }
}
