//! Native read-only workspace tools with bounded output and permission checks.

/// Guarded, atomic workspace editing primitives.
pub mod editing;
/// Persistent non-interactive Bash sessions and permission checks.
pub mod shell;

use crate::{
    perm::{Decision, Engine},
    provider::canonical::Tool,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

/// Workspace tool executor, including read-before-edit state for later mutations.
pub struct Tools {
    /// Effective permission policy.
    pub permissions: Engine,
    /// Last observed file bytes; mutations must compare before overwriting.
    pub read_state: BTreeMap<PathBuf, Vec<u8>>,
    /// Active background shell jobs indexed by job identifier.
    jobs: BTreeMap<String, shell::Job>,
    /// Monotonic counter for background shell job identifiers.
    next_job_id: u64,
    job_handles: Arc<Mutex<Vec<tokio::task::AbortHandle>>>,
    /// Results of background jobs that finished on their own, awaiting delivery to the model.
    completed: Arc<Mutex<Vec<BackgroundCompletion>>>,
    completion_notify: Arc<tokio::sync::Notify>,
    /// Human-readable label and command for each live job.
    job_labels: BTreeMap<String, (String, String)>,
    todos: crate::agent::agency::TodoList,
    approved_shell_prefixes: Vec<String>,
    shell_selection: String,
    hooks: Vec<crate::extensibility::HookRule>,
    mcp_stdio: BTreeMap<String, (crate::mcp::McpServerConfig, crate::mcp::StdioMcpSession)>,
}

/// A finished background job waiting to be delivered to the model.
#[derive(Debug, Clone)]
pub struct BackgroundCompletion {
    /// Human-readable job label.
    pub label: String,
    /// Executed command.
    pub command: String,
    /// Captured output.
    pub output: String,
    /// Whether the job should be reported as a failure.
    pub is_error: bool,
    /// Exit code when the job exited normally.
    pub exit_code: Option<i32>,
    /// Whether the job hit its timeout.
    pub timed_out: bool,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
}

/// Short, readable label such as `cargo-build` taken from the command line.
fn job_label(command: &str) -> String {
    let lowered = command.trim().to_ascii_lowercase();
    let mut words: Vec<&str> = lowered
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '|' | '&' | '>'))
        .filter(|word| {
            !word.is_empty()
                && !matches!(
                    *word,
                    "sudo" | "env" | "nohup" | "time" | "then" | "do" | "cd" | "&&" | "||" | "exec"
                )
                && !word.starts_with('#')
        })
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_'))
        .filter(|word| !word.is_empty())
        .collect();
    if words.is_empty() {
        return "command".into();
    }
    if words[0] == "cargo" && words.len() > 1 {
        words.truncate(2);
    } else {
        words.truncate(1);
    }
    let label: String = words.join("-");
    label.chars().take(24).collect()
}

impl Tools {
    /// Register a fresh background session job and start its completion watcher.
    async fn register_background(
        &mut self,
        session: &shell::Session,
        command: &str,
        timeout: Duration,
        permissions: &Engine,
    ) -> Result<String, String> {
        let (job, watch) = session
            .background_watched(permissions, command, timeout)
            .await?;
        Ok(self.spawn_watcher(job, watch, command))
    }

    /// Adopt a job that already started as a foreground command and keep reporting it.
    async fn adopt_background(&mut self, job: shell::Job, command: &str) -> Result<String, String> {
        let watch = job.completion_watch();
        Ok(self.spawn_watcher(job, watch, command))
    }

    /// Store the job, watch it, and return its label.
    fn spawn_watcher(
        &mut self,
        job: shell::Job,
        mut watch: tokio::sync::watch::Receiver<Option<shell::CompletedJob>>,
        command: &str,
    ) -> String {
        let job_id = format!("job-{}", self.next_job_id);
        self.next_job_id += 1;
        let label = format!("{}-{}", job_label(command), self.next_job_id - 1);
        if let Some(handle) = job.completion_handle() {
            self.job_handles
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(handle);
        }
        self.job_labels
            .insert(job_id.clone(), (label.clone(), command.to_owned()));
        self.jobs.insert(job_id.clone(), job);
        let completed = self.completed.clone();
        let completion_notify = self.completion_notify.clone();
        let watcher_label = label.clone();
        let watcher_command = command.to_owned();
        tokio::spawn(async move {
            // Wait for the worker to publish its outcome; no polling involved.
            if watch.changed().await.is_err() {
                return;
            }
            let Some(outcome) = watch.borrow_and_update().clone() else {
                return;
            };
            let is_error = shell::Job::failed(&outcome);
            let describe = shell::Job::describe(&outcome);
            let mut text = outcome.output;
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            if outcome.truncated {
                text.push_str("[output truncated]\n");
            }
            text.push_str(&format!("[{describe}]"));
            completed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(BackgroundCompletion {
                    label: watcher_label,
                    command: watcher_command,
                    output: text,
                    is_error,
                    exit_code: outcome.exit_code,
                    timed_out: outcome.timed_out,
                    duration_ms: outcome.elapsed_ms,
                });
            completion_notify.notify_one();
        });
        label
    }
}

impl Tools {
    /// Create a tool executor at a canonical workspace root.
    pub fn new(permissions: Engine) -> Self {
        Self {
            permissions,
            read_state: BTreeMap::new(),
            jobs: BTreeMap::new(),
            next_job_id: 1,
            job_handles: Arc::new(Mutex::new(Vec::new())),
            completed: Arc::new(Mutex::new(Vec::new())),
            completion_notify: Arc::new(tokio::sync::Notify::new()),
            job_labels: BTreeMap::new(),
            todos: crate::agent::agency::TodoList::new(),
            approved_shell_prefixes: Vec::new(),
            shell_selection: "auto".into(),
            hooks: Vec::new(),
            mcp_stdio: BTreeMap::new(),
        }
    }

    /// Configure explicitly trusted lifecycle hooks.
    pub fn set_hooks(&mut self, hooks: Vec<crate::extensibility::HookRule>) {
        self.hooks = hooks;
    }

    /// Hooks configured for this executor.
    pub fn hooks(&self) -> &[crate::extensibility::HookRule] {
        &self.hooks
    }

    /// Shared completion handles for reporting active background jobs.
    pub fn background_handles(&self) -> Arc<Mutex<Vec<tokio::task::AbortHandle>>> {
        self.job_handles.clone()
    }

    /// Wake the interactive loop when a result is ready, including while idle.
    pub fn completion_notify(&self) -> Arc<tokio::sync::Notify> {
        self.completion_notify.clone()
    }

    /// Take every background result that arrived since the last call.
    ///
    /// The agent drains this at each request boundary, so completion reaches the
    /// conversation by itself instead of being polled for.
    pub fn take_completed(&self) -> Vec<BackgroundCompletion> {
        let mut completed = self.completed.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *completed)
    }

    /// Map a label, prefix, or internal identifier to the live job identifier.
    fn resolve_job(&self, requested: &str) -> Option<String> {
        if self.jobs.contains_key(requested) {
            return Some(requested.to_owned());
        }
        self.job_labels.iter().find_map(|(id, (label, command))| {
            (label == requested || command == requested).then(|| id.clone())
        })
    }

    /// Stop owned background jobs before releasing their workspace.
    pub async fn shutdown(&mut self) -> Result<(), String> {
        for (_, (_, session)) in std::mem::take(&mut self.mcp_stdio) {
            session.close().await;
        }
        for (_, job) in std::mem::take(&mut self.jobs) {
            job.kill().await?;
        }
        Ok(())
    }

    /// Create a tool executor with an explicitly validated native shell selection.
    pub fn with_shell(permissions: Engine, selection: &str) -> Result<Self, String> {
        crate::platform::detect_shell(selection)?;
        let mut tools = Self::new(permissions);
        tools.shell_selection = selection.into();
        Ok(tools)
    }

    /// Active interpreter information for system prompts and tool descriptions.
    pub fn shell(&self) -> Result<crate::platform::Shell, String> {
        crate::platform::detect_shell(&self.shell_selection)
    }

    /// Stable tool schemas used by all provider adapters.
    pub fn definitions() -> Vec<Tool> {
        vec![
            tool("Agent", "Delegate an independent, clearly scoped task to a specialized subagent. Multiple independent Agent calls run concurrently. The subagent has its own context and returns its findings. Available types: explore (read-only research), plan (read-only planning), general-purpose (research and coding), plus configured custom agents. Provide all necessary context in prompt. Use only when delegation is useful.", json!({"description":{"type":"string","description":"Short task title"},"prompt":{"type":"string","description":"Self-contained task and relevant context"},"subagent_type":{"type":"string","description":"Agent profile name"}}), &["description","prompt","subagent_type"]),
            tool("McpCall", "Call an explicitly configured MCP server tool; execution permission is required.", json!({"server":{"type":"string"},"tool":{"type":"string"},"arguments":{"type":"object"}}), &["server","tool","arguments"]),
            tool("WebFetch", "Fetch an explicitly authorized HTTP URL as untrusted text; network permission is required.", json!({"url":{"type":"string"}}), &["url"]),
            tool("Skill", "Load an installed skill by its registered name; its content does not override permission policy.", json!({"name":{"type":"string"}}), &["name"]),
            tool("TodoWrite", "Replace the task list with current progress.", json!({"todos":{"type":"array","items":{"type":"object","properties":{"content":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","completed"]},"activeForm":{"type":"string"}},"required":["content","status"]}}}), &["todos"]),
            tool("Write", "Atomically write text at any filesystem path. Existing files must be read first; approval is required.", json!({"path":{"type":"string"},"content":{"type":"string"}}), &["path","content"]),
            tool("Edit", "Replace exact unique text in a previously read file.", json!({"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"},"replace_all":{"type":"boolean"}}), &["path","old_string","new_string"]),
            tool("MultiEdit", "Apply an ordered atomic batch of exact replacements to one previously read file.", json!({"path":{"type":"string"},"edits":{"type":"array","items":{"type":"object","properties":{"old_string":{"type":"string"},"new_string":{"type":"string"},"replace_all":{"type":"boolean"}},"required":["old_string","new_string"],"additionalProperties":false}}}), &["path","edits"]),
            tool("NotebookEdit", "Edit a previously read notebook cell, preserving notebook metadata.", json!({"path":{"type":"string"},"cell_id":{"type":"string"},"cell_number":{"type":"integer","minimum":0},"edit_mode":{"type":"string","enum":["replace","insert","delete"]},"cell_type":{"type":"string","enum":["code","markdown","raw"]},"new_source":{"type":"string"}}), &["path","edit_mode"]),
            tool("apply_patch", "Apply a Codex Begin Patch / End Patch document. Read existing files first.", json!({"patch":{"type":"string"}}), &["patch"]),
            tool(
                "Read",
                "Read an existing text file at any filesystem path with numbered lines. Read before editing.",
                json!({"path":{"type":"string"},"offset":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1}}),
                &["path"],
            ),
            tool(
                "LS",
                "List any existing filesystem directory.",
                json!({"path":{"type":"string"}}),
                &["path"],
            ),
            tool(
                "Glob",
                "Find files by glob in the optional path (workspace by default), respecting ignore files.",
                json!({"pattern":{"type":"string"},"path":{"type":"string"}}),
                &["pattern"],
            ),
            tool(
                "Grep",
                "Search text files using a Rust regular expression; paths outside the workspace are allowed.",
                json!({"pattern":{"type":"string"},"path":{"type":"string","description":"File or directory to search (workspace by default)"},"ignore_case":{"type":"boolean"},"glob":{"type":"string"},"head_limit":{"type":"integer","minimum":1}}),
                &["pattern"],
            ),
            tool(
                "Bash",
                "Execute a command in its own non-interactive shell, starting in the workspace. Each call is independent: cd and export do not persist between calls. Wait up to 10 seconds by default (adjust with yield_time_ms); if still running, return a background task label and automatically report its result. Use run_in_background for long independent work. Set timeout_seconds to limit total runtime. Do not poll for background results.",
                json!({"command":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1},"yield_time_ms":{"type":"integer","minimum":250,"maximum":30000},"run_in_background":{"type":"boolean"}}),
                &["command"],
            ),
            tool(
                "KillShell",
                "Stop a running background task by its label, for example `cargo-build-1`.",
                json!({"job_id":{"type":"string","description":"Task label returned when it started"}}),
                &["job_id"],
            ),
        ]
    }

    fn new_shell(&self) -> Result<shell::Session, String> {
        let home = crate::platform::home_dir().unwrap_or_else(|| self.permissions.root.clone());
        shell::Session::with_shell(&self.permissions, &home, &self.shell_selection)
    }

    /// Remove remembered command approvals when a new conversation starts.
    pub fn clear_shell_approvals(&mut self) {
        self.approved_shell_prefixes.clear();
    }

    /// Whether this single command matches a remembered prefix and has no explicit ask rule.
    pub fn shell_is_remembered(&self, command: &str) -> bool {
        shell::similar_prefix(command).is_some_and(|prefix| {
            self.approved_shell_prefixes.contains(&prefix)
                && !self.permissions.explicitly_asks("Bash", command)
        })
    }

    /// Remember the displayed prefix for this conversation only.
    pub fn remember_shell_prefix(&mut self, command: &str) {
        if let Some(prefix) = shell::similar_prefix(command) {
            if !self.approved_shell_prefixes.contains(&prefix) {
                self.approved_shell_prefixes.push(prefix);
            }
        }
    }

    /// Execute a validated operation; errors become model-visible tool results.
    pub async fn execute(&mut self, name: &str, input: &Value) -> Result<String, String> {
        self.execute_inner(name, input, false).await
    }

    /// Execute the exact Bash invocation explicitly approved by the operator.
    pub async fn execute_approved_shell(&mut self, input: &Value) -> Result<String, String> {
        self.execute_inner("Bash", input, true).await
    }

    /// Execute one explicitly approved tool invocation while retaining deny and plan rules.
    pub async fn execute_approved_tool(
        &mut self,
        name: &str,
        input: &Value,
    ) -> Result<String, String> {
        if !matches!(
            name,
            "Write"
                | "Edit"
                | "MultiEdit"
                | "NotebookEdit"
                | "apply_patch"
                | "WebFetch"
                | "McpCall"
        ) {
            return Err("tool does not support interactive approval".into());
        }
        self.execute_inner(name, input, true).await
    }

    async fn execute_inner(
        &mut self,
        name: &str,
        input: &Value,
        approved: bool,
    ) -> Result<String, String> {
        self.jobs.retain(|_, job| !job.is_finished());
        self.job_labels.retain(|id, _| self.jobs.contains_key(id));
        match name {
            "TodoWrite" => return self.todos.update_from_value(input),
            "McpCall" => {
                let server = required(input, "server")?.to_owned();
                let tool = required(input, "tool")?.to_owned();
                let resource = crate::mcp::qualify_tool_name(&server, &tool)?;
                match self.permissions.decide("McpCall", &resource, true) {
                    Decision::Deny => return Err("permission denied by policy".into()),
                    Decision::Ask if !approved => {
                        return Err("permission requires explicit approval".into())
                    }
                    _ => {}
                }
                let arguments = input
                    .get("arguments")
                    .filter(|value| value.is_object())
                    .ok_or("arguments must be an object")?
                    .clone();
                let path = self.permissions.root.join(".mcp.json");
                let config =
                    tokio::task::spawn_blocking(move || crate::mcp::load_mcp_config(&path))
                        .await
                        .map_err(|_| "cannot load MCP configuration")?
                        .map_err(|_| "invalid MCP configuration")?;
                let configured = match config.servers.get(&server) {
                    Some(configured) => configured,
                    None => {
                        self.mcp_stdio.remove(&server);
                        return Err("unknown MCP server".into());
                    }
                };
                if !matches!(configured, crate::mcp::McpServerConfig::Stdio { .. }) {
                    self.mcp_stdio.remove(&server);
                }
                return match configured {
                    crate::mcp::McpServerConfig::Stdio { command, args, env } => {
                        if self
                            .mcp_stdio
                            .get(&server)
                            .is_some_and(|(old, _)| old != configured)
                        {
                            self.mcp_stdio.remove(&server);
                        }
                        if !self.mcp_stdio.contains_key(&server) {
                            let session = crate::mcp::StdioMcpSession::connect(
                                command,
                                args,
                                env,
                                Duration::from_secs(30),
                            )
                            .await
                            .map_err(|_| "MCP server initialization failed")?;
                            self.mcp_stdio
                                .insert(server.clone(), (configured.clone(), session));
                        }
                        // Remove the connection while the request is in flight: if
                        // the caller cancels, dropping the future kills the process
                        // instead of reusing a stream with an outstanding response.
                        let (configuration, mut session) = self
                            .mcp_stdio
                            .remove(&server)
                            .ok_or("MCP session unavailable")?;
                        let result = session.call_tool(&tool, arguments).await;
                        if result.is_ok() {
                            self.mcp_stdio.insert(server, (configuration, session));
                        }
                        result.map_err(|_| "MCP tool request failed".into())
                    }
                    crate::mcp::McpServerConfig::Http { url, headers } => {
                        crate::mcp::HttpMcpClient::new(
                            url,
                            headers.clone(),
                            Duration::from_secs(30),
                        )?
                        .call_tool(&tool, arguments)
                        .await
                        .map_err(|_| "MCP tool request failed".into())
                    }
                };
            }
            "WebFetch" => {
                let url = required(input, "url")?;
                match self.permissions.decide("WebFetch", url, true) {
                    Decision::Deny => return Err("permission denied by policy".into()),
                    Decision::Ask if !approved => {
                        return Err("permission requires explicit approval".into())
                    }
                    _ => {}
                }
                return crate::web_git::WebFetcher::new(
                    crate::web_git::DomainPolicy::default(),
                    Duration::from_secs(900),
                )?
                .fetch(url)
                .await;
            }
            "Skill" => {
                let name = required(input, "name")?.to_owned();
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .ok_or("HOME is required")?;
                let root = self.permissions.root.clone();
                if self.permissions.decide("Skill", &name, false) != Decision::Allow {
                    return Err("skill permission denied or requires approval".into());
                }
                return tokio::task::spawn_blocking(move || {
                    let skills = crate::extensibility::discover_skills(&home, &root)
                        .map_err(|e| e.to_string())?;
                    let skill = skills
                        .iter()
                        .find(|skill| skill.name == name)
                        .ok_or("unknown skill")?;
                    crate::extensibility::load_skill_body(skill).map_err(|e| e.to_string())
                })
                .await
                .map_err(|e| e.to_string())?
                .map(bound);
            }
            "Bash" => {
                if self.jobs.len() >= 64 {
                    return Err("too many running background commands; wait for one to finish or use KillShell".into());
                }
                let command = required(input, "command")?;
                let mut shell_permissions = self.permissions.clone();
                if approved {
                    if shell::permission_decision_for(
                        &shell_permissions,
                        command,
                        self.shell()?.kind,
                    )? == Decision::Deny
                    {
                        return Err("permission denied by policy".into());
                    }
                    // Approval applies only to this invocation; deny and plan restrictions remain.
                    shell_permissions.ask.clear();
                    shell_permissions
                        .allow
                        .push(crate::perm::Rule::parse("Bash(**)")?);
                }
                let timeout = input["timeout_seconds"]
                    .as_u64()
                    .map(Duration::from_secs)
                    .unwrap_or(Duration::MAX);
                if input["run_in_background"].as_bool().unwrap_or(false) {
                    let bg_session = self.new_shell()?;
                    let label = self
                        .register_background(&bg_session, command, timeout, &shell_permissions)
                        .await?;
                    return Ok(format!(
                        "Started background command {label} (`{command}`). Its result will appear in the conversation automatically; continue with other work meanwhile. Use KillShell to stop it."
                    ));
                }
                let session = self.new_shell()?;
                let mut job = session
                    .background(&shell_permissions, command, timeout)
                    .await?;
                let wait = input["yield_time_ms"].as_u64().unwrap_or(10_000);
                let wait = if cfg!(windows) {
                    wait.max(10_000)
                } else {
                    wait
                };
                let wait = wait.clamp(250, 30_000);
                job.wait_ready(Duration::from_millis(wait)).await;
                if !job.is_finished() {
                    let initial = job.poll()?;
                    let label = self.adopt_background(job, command).await?;
                    return Ok(bound(format!(
                        "Command still running as background task {label}. Its result will appear in the conversation automatically; continue with other work meanwhile.\n{initial}"
                    )));
                }
                let out = job.wait().await?;
                let mut text = out.output;
                if out.timed_out {
                    text.push_str("\n[command timed out]");
                } else if let Some(code) = out.exit_code {
                    if code != 0 {
                        text.push_str(&format!("\n[exit code: {code}]"));
                    }
                }
                if out.truncated {
                    text.push_str("\n[output truncated]");
                }
                return Ok(bound(text));
            }
            "KillShell" => {
                let requested = required(input, "job_id")?;
                let job_id = self
                    .resolve_job(requested)
                    .ok_or_else(|| format!("unknown background task: {requested}"))?;
                let label = self
                    .job_labels
                    .get(&job_id)
                    .map(|(label, _)| label.clone())
                    .unwrap_or_else(|| job_id.clone());
                let job = self
                    .jobs
                    .remove(&job_id)
                    .ok_or_else(|| format!("unknown background task: {requested}"))?;
                self.job_labels.remove(&job_id);
                job.kill().await?;
                return Ok(format!("Stopped background task {label}"));
            }
            _ => {}
        }
        let resource = input["path"]
            .as_str()
            .or_else(|| input["pattern"].as_str())
            .unwrap_or("");
        if matches!(
            name,
            "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "apply_patch"
        ) {
            let mut permissions = self.permissions.clone();
            if approved {
                permissions
                    .ask
                    .retain(|rule| rule.tool != name && rule.tool != "*");
                permissions
                    .allow
                    .push(crate::perm::Rule::parse(&format!("{name}(**)"))?);
            }
            let mut state = self.read_state.clone();
            let name = name.to_owned();
            let input = input.clone();
            let (result, state) = tokio::task::spawn_blocking(move || {
                let result = editing::execute(&permissions, &mut state, &name, &input);
                (result, state)
            })
            .await
            .map_err(|_| "editing worker failed")?;
            self.read_state = state;
            return result.map(|changes| {
                changes
                    .iter()
                    .map(|change| {
                        format!(
                            "Updated {} ({} -> {} bytes)",
                            change.path.display(),
                            change.before.as_ref().map_or(0, Vec::len),
                            change.after.as_ref().map_or(0, Vec::len)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        }
        match self.permissions.decide(name, resource, false) {
            Decision::Allow => {}
            Decision::Ask => return Err("permission requires explicit approval".into()),
            Decision::Deny => return Err("permission denied by policy".into()),
        }
        match name {
            "Read" => {
                let path = self
                    .permissions
                    .existing(Path::new(required(input, "path")?))?;
                let relative = self
                    .permissions
                    .workspace_for(&path)
                    .and_then(|root| path.strip_prefix(root).ok())
                    .unwrap_or(&path);
                for resource in [path.to_string_lossy(), relative.to_string_lossy()] {
                    if self.permissions.decide("Read", &resource, false) != Decision::Allow {
                        return Err("permission denied for resolved read path".into());
                    }
                }
                use tokio::io::AsyncReadExt;
                let file = tokio::fs::File::open(&path)
                    .await
                    .map_err(|_| "cannot read file")?;
                let mut bytes = Vec::new();
                file.take(16 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|_| "cannot read file")?;
                if bytes.len() > 16 * 1024 * 1024 {
                    return Err("file exceeds 16 MiB read limit".into());
                }
                if bytes.contains(&0) {
                    return Err("binary file requires an attachment-capable reader".into());
                }
                let text = std::str::from_utf8(&bytes).map_err(|_| "file is not UTF-8 text")?;
                let offset = input["offset"].as_u64().unwrap_or(1).max(1) as usize;
                let limit = (input["limit"].as_u64().unwrap_or(2000) as usize).min(10000);
                let output = text
                    .lines()
                    .enumerate()
                    .skip(offset - 1)
                    .take(limit)
                    .map(|(i, line)| format!("{}\t{line}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.read_state.insert(path, bytes);
                Ok(bound(output))
            }
            "LS" => {
                let path = self
                    .permissions
                    .existing(Path::new(required(input, "path")?))?;
                let mut entries = tokio::fs::read_dir(path)
                    .await
                    .map_err(|_| "cannot list directory")?;
                let mut names = Vec::new();
                while let Some(entry) = entries
                    .next_entry()
                    .await
                    .map_err(|_| "cannot read directory entry")?
                {
                    names.push(entry.file_name().to_string_lossy().into_owned());
                    if names.len() >= 10000 {
                        break;
                    }
                }
                names.sort();
                Ok(bound(names.join("\n")))
            }
            "Glob" | "Grep" => {
                let root = self.permissions.root.clone();
                let input = input.clone();
                let name = name.to_owned();
                let permissions = self.permissions.clone();
                tokio::task::spawn_blocking(move || search(&root, &name, &input, &permissions))
                    .await
                    .map_err(|_| "search worker failed")?
            }
            _ => Err("unknown tool".into()),
        }
    }
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Tool {
    Tool {
        name: name.into(),
        description: description.into(),
        input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    }
}

fn required<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input[key]
        .as_str()
        .ok_or_else(|| format!("missing string argument: {key}"))
}

fn bound(mut output: String) -> String {
    if output.len() > 100_000 {
        let mut end = 100_000;
        while !output.is_char_boundary(end) {
            end -= 1;
        }
        output.truncate(end);
        output.push_str("\n[output truncated]");
    }
    output
}

fn search(root: &Path, name: &str, input: &Value, permissions: &Engine) -> Result<String, String> {
    let pattern = required(input, "pattern")?;
    let search_root = if name == "Grep" {
        input["path"]
            .as_str()
            .map(|path| permissions.existing(Path::new(path)))
            .transpose()?
    } else {
        None
    };
    let glob = if name == "Glob" {
        Some(pattern)
    } else {
        input["glob"].as_str()
    }
    .map(|p| {
        globset::Glob::new(p)
            .map(|g| g.compile_matcher())
            .map_err(|_| "invalid glob")
    })
    .transpose()?;
    let regex = if name == "Grep" {
        Some(
            regex::RegexBuilder::new(pattern)
                .case_insensitive(input["ignore_case"].as_bool().unwrap_or(false))
                .size_limit(4 * 1024 * 1024)
                .build()
                .map_err(|_| "invalid regular expression")?,
        )
    } else {
        None
    };
    let mut output = Vec::new();
    let limit = input["head_limit"].as_u64().unwrap_or(1000).min(10000) as usize;
    for entry in ignore::WalkBuilder::new(search_root.as_deref().unwrap_or(root))
        .hidden(false)
        .follow_links(false)
        .build()
    {
        let entry = entry.map_err(|_| "cannot walk workspace")?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(path);
        if permissions.decide("Read", &relative.to_string_lossy(), false) != Decision::Allow {
            continue;
        }
        let match_path = search_root
            .as_deref()
            .and_then(|base| path.strip_prefix(base).ok())
            .unwrap_or(relative);
        if glob.as_ref().is_some_and(|g| !g.is_match(match_path)) {
            continue;
        }
        // Search results use a stable slash-separated display on every platform.
        // Keep native paths for filesystem access, matching and permissions above.
        let display_path = relative.to_string_lossy();
        #[cfg(windows)]
        let display_path = display_path.replace('\\', "/");
        if let Some(regex) = &regex {
            if entry
                .metadata()
                .map_err(|_| "cannot inspect search file")?
                .len()
                > 4 * 1024 * 1024
            {
                continue;
            }
            let bytes = std::fs::read(path).map_err(|_| "cannot read search file")?;
            if bytes.contains(&0) {
                continue;
            }
            if let Ok(text) = std::str::from_utf8(&bytes) {
                for (index, line) in text.lines().enumerate() {
                    if regex.is_match(line) {
                        output.push(format!(
                            "{}:{}:{}",
                            display_path,
                            index + 1,
                            line.chars().take(2000).collect::<String>()
                        ));
                        if output.len() >= limit {
                            break;
                        }
                    }
                }
            }
        } else {
            output.push(display_path.to_string());
        }
        if output.len() >= limit {
            break;
        }
    }
    if name == "Glob" {
        output.sort();
    }
    Ok(bound(output.join("\n")))
}
