//! Headless execution (`-p` / `exec`), `stream-json` event protocol, shell completions, and `gitcode doctor`.

use crate::provider::canonical::Usage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

/// Output serialization format for headless `-p` and `exec` modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    /// Plain final assistant text on stdout.
    Text,
    /// Single JSON object printed on completion.
    Json,
    /// Newline-delimited JSON event stream (`stream-json`).
    StreamJson,
}

impl OutputFormat {
    /// Parse `--output-format` flag value.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            "stream-json" => Ok(Self::StreamJson),
            other => Err(format!("unknown output format `{other}`")),
        }
    }
}

/// Input serialization format for headless runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputFormat {
    /// Plain prompt string from CLI arguments or stdin.
    Text,
    /// Newline-delimited JSON prompt messages on stdin.
    StreamJson,
}

impl InputFormat {
    /// Parse `--input-format` flag value.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "text" => Ok(Self::Text),
            "stream-json" => Ok(Self::StreamJson),
            other => Err(format!("unknown input format `{other}`")),
        }
    }
}

/// Deterministic CLI exit codes for scripting and CI pipelines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    /// Clean completion (`0`).
    Success = 0,
    /// Runtime, model, or tool error (`1`).
    RuntimeError = 1,
    /// CLI usage or configuration validation failure (`2`).
    UsageOrConfigError = 2,
    /// Permission or policy denial (`3`).
    PermissionDenied = 3,
    /// Budget or turn ceiling exceeded (`4`).
    BudgetExceeded = 4,
}

/// Structured event emitted on stdout in `stream-json` mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HeadlessEvent {
    /// Initial session metadata event.
    Init {
        /// Session identifier.
        session_id: String,
        /// Active model identifier.
        model: String,
        /// Active provider preset or endpoint name.
        provider: String,
        /// Enabled tool names.
        tools: Vec<String>,
    },
    /// Incremental assistant text delta.
    MessageDelta {
        /// Delta text fragment.
        text: String,
    },
    /// Incremental reasoning/thinking delta.
    ThinkingDelta {
        /// Reasoning fragment.
        text: String,
    },
    /// Tool invocation started.
    ToolUse {
        /// Tool call identifier.
        id: String,
        /// Tool name.
        name: String,
        /// Structured JSON arguments.
        input: Value,
    },
    /// Tool invocation finished.
    ToolResult {
        /// Matching tool call identifier.
        tool_use_id: String,
        /// Bounded output text.
        content: String,
        /// Whether the tool returned an error or was denied.
        is_error: bool,
    },
    /// Final turn result summary.
    Result {
        /// Final assistant text.
        text: String,
        /// Aggregated token usage.
        usage: Usage,
        /// Number of model requests performed.
        requests: usize,
        /// Total wall-clock duration in milliseconds.
        duration_ms: u64,
        /// Whether the turn ended in error.
        is_error: bool,
    },
}

impl HeadlessEvent {
    /// Serialize the event as a single newline-terminated JSON line.
    pub fn to_jsonl(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|e| e.to_string())
    }
}

/// Parse user prompt lines from `stream-json` stdin input.
pub fn parse_stream_json_input(raw: &str) -> Result<Vec<String>, String> {
    let mut prompts = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let val: Value =
            serde_json::from_str(trimmed).map_err(|_| "invalid stream-json input line")?;
        if let Some(text) = val["content"]
            .as_str()
            .or_else(|| val["prompt"].as_str())
            .or_else(|| val["message"]["content"].as_str())
        {
            prompts.push(text.to_owned());
        } else {
            return Err("stream-json input line missing `content` or `prompt` string".into());
        }
    }
    Ok(prompts)
}

/// Generate shell completion scripts for `bash`, `zsh`, or `fish`.
pub fn generate_completions(shell: &str) -> Result<String, String> {
    match shell {
        "bash" => Ok(concat!(
            "_gitcode() {\n",
            "  local cur=\"${COMP_WORDS[COMP_CWORD]}\"\n",
            "  local opts=\"--help --version -p --prompt --model --base-url --protocol --max-turns --dangerously-skip-permissions --output-format --input-format exec doctor completions usage mcp\"\n",
            "  COMPREPLY=( $(compgen -W \"${opts}\" -- \"${cur}\") )\n",
            "}\n",
            "complete -F _gitcode gitcode\n"
        )
        .to_owned()),
        "zsh" => Ok(concat!(
            "#compdef gitcode\n",
            "_gitcode() {\n",
            "  _arguments \\\n",
            "    '--help[Show help]' \\\n",
            "    '--version[Show version]' \\\n",
            "    '-p[Print mode prompt]:prompt:' \\\n",
            "    '--model[Model identifier]:model:' \\\n",
            "    '--output-format[Output format]:format:(text json stream-json)' \\\n",
            "    '1:subcommand:(exec doctor completions usage mcp)'\n",
            "}\n",
            "_gitcode \"$@\"\n"
        )
        .to_owned()),
        "fish" => Ok(concat!(
            "complete -c gitcode -l help -d 'Show help'\n",
            "complete -c gitcode -l version -d 'Show version'\n",
            "complete -c gitcode -s p -l prompt -d 'Run single prompt in print mode'\n",
            "complete -c gitcode -l model -d 'Model identifier'\n",
            "complete -c gitcode -l output-format -a 'text json stream-json' -d 'Output format'\n",
            "complete -c gitcode -n '__fish_use_subcommand' -a 'exec doctor completions usage mcp'\n"
        )
        .to_owned()),
        other => Err(format!("unsupported shell `{other}` (expected bash, zsh, or fish)")),
    }
}

/// One diagnostic check item from `gitcode doctor`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorCheck {
    /// Check identifier.
    pub name: String,
    /// Whether the check passed.
    pub ok: bool,
    /// Safe diagnostic detail (never contains secret values).
    pub detail: String,
}

/// Complete `gitcode doctor` report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorReport {
    /// Overall health status.
    pub healthy: bool,
    /// Individual check outcomes.
    pub checks: Vec<DoctorCheck>,
}

/// Run diagnostic checks on git availability, workspace directory, config, and API key presence.
pub fn run_doctor(workspace: &Path, config_valid: bool, has_api_key: bool) -> DoctorReport {
    let env = std::env::vars().collect::<std::collections::BTreeMap<_, _>>();
    let selection = if let Some(home) = crate::platform::home_dir_from(&env) {
        crate::config::load(&home, workspace, &env, &crate::config::Overrides::new())
            .map(|config| config.shell)
            .map_err(|_| "cannot resolve shell: configuration validation failed")
    } else {
        Ok(env
            .get("GITCODE_SHELL")
            .cloned()
            .unwrap_or_else(|| "auto".into()))
    };
    let config_valid = config_valid && selection.is_ok();
    let shell = match selection {
        Ok(selection) => match crate::shell::Shell::detect(&selection) {
            Ok(shell) if shell.executable.is_file() => DoctorCheck {
                name: "shell".into(),
                ok: true,
                detail: format!(
                    "selected shell: {} (selection: {selection}); executable: {}",
                    shell.name(),
                    shell.executable.display()
                ),
            },
            _ => DoctorCheck {
                name: "shell".into(),
                ok: false,
                detail: "selected shell unavailable; install it or set config.shell to auto".into(),
            },
        },
        Err(detail) => DoctorCheck {
            name: "shell".into(),
            ok: false,
            detail: detail.into(),
        },
    };
    let git_ok = std::process::Command::new("git")
        .stdin(std::process::Stdio::null())
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let ws_ok = workspace.is_dir();

    #[allow(unused_mut)]
    let mut checks = vec![
        shell,
        DoctorCheck {
            name: "git".into(),
            ok: git_ok,
            detail: if git_ok {
                if cfg!(windows) {
                    "Git available; Git for Windows is required for native Windows use".into()
                } else {
                    "git binary available".into()
                }
            } else if cfg!(windows) {
                "Git for Windows unavailable; install with: winget install --id Git.Git -e".into()
            } else {
                "git binary not found in PATH or failed to run; install Git".into()
            },
        },
        DoctorCheck {
            name: "workspace".into(),
            ok: ws_ok,
            detail: if ws_ok {
                "workspace directory accessible".into()
            } else {
                "workspace directory missing".into()
            },
        },
        DoctorCheck {
            name: "config".into(),
            ok: config_valid,
            detail: if config_valid {
                "configuration valid".into()
            } else {
                "configuration validation failed".into()
            },
        },
        DoctorCheck {
            name: "credentials".into(),
            ok: has_api_key,
            detail: if has_api_key {
                "API credential present (redacted)".into()
            } else {
                "no API credential configured in environment or credential store".into()
            },
        },
    ];
    #[cfg(windows)]
    {
        checks.push(DoctorCheck {
            name: "sandbox".into(),
            ok: true,
            detail: "Windows sandbox: enforced at tool level, not OS level; tool path boundaries and command classification do not isolate arbitrary shell commands".into(),
        });
        let windows_terminal = env.get("WT_SESSION").is_some_and(|value| !value.is_empty());
        checks.push(DoctorCheck {
            name: "terminal".into(),
            ok: true,
            detail: if windows_terminal {
                "Windows Terminal detected".into()
            } else {
                "WARNING: Windows Terminal not detected; legacy console or another host may have limited keyboard and rendering support. Prefer Windows Terminal; use Alt+Enter, Ctrl+J, or a trailing backslash for newlines".into()
            },
        });
    }
    let healthy = checks.iter().all(|c| c.ok);
    DoctorReport { healthy, checks }
}
