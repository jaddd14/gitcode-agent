# Native Windows port

The Windows port is under validation. Native CI and installer smoke results, not a Linux cross-check, determine release readiness. Do not assume an unreleased build is supported.

## Requirements and shells

Target: Windows 10 1809 or later, or Windows 11. Windows Terminal is the primary terminal host. Git for Windows is recommended and is required for Git-backed features:

```powershell
winget install --id Git.Git -e
```

Automatic shell selection prefers Git Bash, PowerShell 7 (`pwsh`), Windows PowerShell, then `cmd.exe`. Set `shell = "auto"`, `"bash"`, `"pwsh"`, `"powershell"`, or `"cmd"` in `%USERPROFILE%\.gitcode\config.toml`; `GITCODE_SHELL` overrides the file. Shell syntax is not interchangeable. The selected interpreter is included in the model's instructions. Run `gitcode doctor --json` for diagnostics.

Configuration, sessions and credentials use `%USERPROFILE%\.gitcode`. Credential ACL restriction is best effort; it is not a guarantee against administrators or other processes running as the same user.

## Terminal and input

Use Windows Terminal with a Unicode-capable font. Some terminal hosts cannot distinguish Shift+Enter. Use Alt+Enter or Ctrl+J for a newline; terminal-specific keyboard capabilities may vary. Legacy conhost, synchronized painting, graphics and keyboard extensions require native validation and may degrade. The intro animation is unchanged.

## Security boundaries

Windows has no integrated Landlock or seatbelt equivalent here. Workspace file tools enforce path boundaries and shell requests pass through permission checks and command classification. This is **tool-level enforcement, not OS-level sandboxing**. An approved shell command can access resources available to the current user. Do not run untrusted commands on the assumption that the operating system isolates them.

Child shells use kill-on-close Job Objects for process-tree ownership. Native cancellation, background jobs and each interpreter must pass Windows CI before release.

## Installation and update

The intended per-user install directory is `%LOCALAPPDATA%\Programs\gitcode\bin`; no administrator rights are needed. Installers verify archive SHA256 before replacement. `GITCODE_INSTALL_DIR` overrides the destination and `GITCODE_VERSION` pins a release. Until a Windows release and its installer CI are green, build artifacts are development builds, not a supported published installer.

After installation, open a new terminal if PATH has not refreshed. Run `gitcode --version` to see both version and source revision. Use `gitcode update` for a checksum-verified update. Windows updates preserve the previous executable as `.old`, which is cleaned up on the next start.

Binaries are not code-signed. SmartScreen may warn; verify the release source and checksums rather than disabling system protection globally.

## Troubleshooting and removal

- Missing Git: install Git for Windows using the command above, then reopen the terminal.
- Shell discovery failure: install a supported interpreter or set an explicit shell selection.
- MCP batch shims: `.cmd` and `.bat` launch through `cmd.exe`; arguments containing unsupported shell metacharacters are rejected rather than silently reinterpreted.
- Permission errors: close other running instances and verify write access to the installation directory.
- Remove the installed `gitcode.exe` and any `.old` file, then remove only its installation directory from the user PATH. Keep `%USERPROFILE%\.gitcode` unless intentionally deleting configuration, credentials and sessions.
