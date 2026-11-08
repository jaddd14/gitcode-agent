//! Native read-only workspace tools with bounded output and permission checks.

/// Guarded, atomic workspace editing primitives.
pub mod editing;
/// Persistent non-interactive Bash sessions and conservative command splitting.
pub mod shell;

use crate::{
    perm::{Decision, Engine},
    provider::canonical::Tool,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

/// Workspace tool executor, including read-before-edit state for later mutations.
pub struct Tools {
    /// Effective permission policy.
    pub permissions: Engine,
    /// Last observed file bytes; mutations must compare before overwriting.
    pub read_state: BTreeMap<PathBuf, Vec<u8>>,
    /// Lazily initialized persistent shell session.
    shell_session: Option<shell::Session>,
    /// Active background shell jobs indexed by job identifier.
    jobs: BTreeMap<String, shell::Job>,
    /// Monotonic counter for background shell job identifiers.
    next_job_id: u64,
    todos: crate::agent::agency::TodoList,
}

impl Tools {
    /// Create a tool executor at a canonical workspace root.
    pub fn new(permissions: Engine) -> Self {
        Self {
            permissions,
            read_state: BTreeMap::new(),
            shell_session: None,
            jobs: BTreeMap::new(),
            next_job_id: 1,
            todos: crate::agent::agency::TodoList::new(),
        }
    }

    /// Stable tool schemas used by all provider adapters.
    pub fn definitions() -> Vec<Tool> {
        vec![
            tool("McpCall", "Call an explicitly configured MCP server tool; execution permission is required.", json!({"server":{"type":"string"},"tool":{"type":"string"},"arguments":{"type":"object"}}), &["server","tool","arguments"]),
            tool("WebFetch", "Fetch an explicitly authorized HTTP URL as untrusted text; network permission is required.", json!({"url":{"type":"string"}}), &["url"]),
            tool("Skill", "Load an installed skill by its registered name; its content does not override permission policy.", json!({"name":{"type":"string"}}), &["name"]),
            tool("TodoWrite", "Replace the task list with current progress.", json!({"todos":{"type":"array","items":{"type":"object","properties":{"content":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","completed"]},"activeForm":{"type":"string"}},"required":["content","status"]}}}), &["todos"]),
            tool("Write", "Atomically write workspace text. Existing files must be read first; approval is required.", json!({"path":{"type":"string"},"content":{"type":"string"}}), &["path","content"]),
            tool("Edit", "Replace exact unique text in a previously read file.", json!({"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"},"replace_all":{"type":"boolean"}}), &["path","old_string","new_string"]),
            tool("MultiEdit", "Apply an ordered atomic batch of exact replacements to one previously read file.", json!({"path":{"type":"string"},"edits":{"type":"array","items":{"type":"object","properties":{"old_string":{"type":"string"},"new_string":{"type":"string"},"replace_all":{"type":"boolean"}},"required":["old_string","new_string"],"additionalProperties":false}}}), &["path","edits"]),
            tool("NotebookEdit", "Edit a previously read notebook cell, preserving notebook metadata.", json!({"path":{"type":"string"},"cell_id":{"type":"string"},"cell_number":{"type":"integer","minimum":0},"edit_mode":{"type":"string","enum":["replace","insert","delete"]},"cell_type":{"type":"string","enum":["code","markdown","raw"]},"new_source":{"type":"string"}}), &["path","edit_mode"]),
            tool("apply_patch", "Apply a Codex Begin Patch / End Patch document. Read existing files first.", json!({"patch":{"type":"string"}}), &["patch"]),
            tool(
                "Read",
                "Read an existing workspace text file with numbered lines. Read before editing.",
                json!({"path":{"type":"string"},"offset":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1}}),
                &["path"],
            ),
            tool(
                "LS",
                "List an existing workspace directory.",
                json!({"path":{"type":"string"}}),
                &["path"],
            ),
            tool(
                "Glob",
                "Find workspace files by glob, respecting ignore files.",
                json!({"pattern":{"type":"string"}}),
                &["pattern"],
            ),
            tool(
                "Grep",
                "Search workspace text files using a Rust regular expression.",
                json!({"pattern":{"type":"string"},"ignore_case":{"type":"boolean"},"glob":{"type":"string"},"head_limit":{"type":"integer","minimum":1}}),
                &["pattern"],
            ),
            tool(
                "Bash",
                "Execute a command in a persistent non-interactive Bash session.",
                json!({"command":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":600},"run_in_background":{"type":"boolean"}}),
                &["command"],
            ),
            tool(
                "BashOutput",
                "Read incremental output from a background shell job.",
                json!({"job_id":{"type":"string"},"filter":{"type":"string"}}),
                &["job_id"],
            ),
            tool(
                "KillShell",
                "Terminate an active background shell job.",
                json!({"job_id":{"type":"string"}}),
                &["job_id"],
            ),
        ]
    }

    fn ensure_shell(&mut self) -> Result<shell::Session, String> {
        if let Some(session) = &self.shell_session {
            return Ok(session.clone());
        }
        let session = shell::Session::new(&self.permissions, &self.permissions.root)?;
        self.shell_session = Some(session.clone());
        Ok(session)
    }

    /// Execute a validated operation; errors become model-visible tool results.
    pub async fn execute(&mut self, name: &str, input: &Value) -> Result<String, String> {
        match name {
            "TodoWrite" => return self.todos.update_from_value(input),
            "McpCall" => {
                let server = required(input, "server")?.to_owned();
                let tool = required(input, "tool")?.to_owned();
                let resource = crate::mcp::qualify_tool_name(&server, &tool)?;
                if self.permissions.decide("McpCall", &resource, true) != Decision::Allow {
                    return Err("MCP execution denied or requires approval".into());
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
                let configured = config.servers.get(&server).ok_or("unknown MCP server")?;
                return match configured {
                    crate::mcp::McpServerConfig::Stdio { command, args, env } => {
                        crate::mcp::StdioMcpClient::new(
                            command,
                            args.clone(),
                            env.clone(),
                            Duration::from_secs(30),
                        )
                        .call_tool(&tool, arguments)
                        .await
                        .map_err(|_| "MCP tool request failed".into())
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
                if self.permissions.decide("WebFetch", url, true) != Decision::Allow {
                    return Err("network permission denied or requires approval".into());
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
                let command = required(input, "command")?;
                let secs = input["timeout_seconds"]
                    .as_u64()
                    .unwrap_or(120)
                    .clamp(1, 600);
                let timeout = Duration::from_secs(secs);
                if input["run_in_background"].as_bool().unwrap_or(false) {
                    let bg_session =
                        shell::Session::new(&self.permissions, &self.permissions.root)?;
                    let job = bg_session
                        .background(&self.permissions, command, timeout)
                        .await?;
                    let job_id = format!("job-{}", self.next_job_id);
                    self.next_job_id += 1;
                    self.jobs.insert(job_id.clone(), job);
                    return Ok(format!("Started background shell job {job_id}"));
                }
                let session = self.ensure_shell()?;
                let out = session.execute(&self.permissions, command, timeout).await?;
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
            "BashOutput" => {
                let job_id = required(input, "job_id")?;
                let job = self
                    .jobs
                    .get_mut(job_id)
                    .ok_or_else(|| format!("unknown background job: {job_id}"))?;
                let chunk = job.poll()?;
                let finished = job.is_finished();
                let filtered = if let Some(pattern) = input["filter"].as_str() {
                    let re = regex::RegexBuilder::new(pattern)
                        .size_limit(4 * 1024 * 1024)
                        .build()
                        .map_err(|_| "invalid filter regular expression")?;
                    chunk
                        .lines()
                        .filter(|line| re.is_match(line))
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    chunk
                };
                return Ok(bound(format!("finished={finished}\n{filtered}")));
            }
            "KillShell" => {
                let job_id = required(input, "job_id")?;
                let job = self
                    .jobs
                    .remove(job_id)
                    .ok_or_else(|| format!("unknown background job: {job_id}"))?;
                job.kill().await?;
                return Ok(format!("Killed background shell job {job_id}"));
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
            let permissions = self.permissions.clone();
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
                let relative = path
                    .strip_prefix(&self.permissions.root)
                    .map_err(|_| "invalid read boundary")?;
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
                    .map(|(i, line)| {
                        format!("{}\t{}", i + 1, line.chars().take(2000).collect::<String>())
                    })
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
    for entry in ignore::WalkBuilder::new(root)
        .hidden(false)
        .follow_links(false)
        .build()
    {
        let entry = entry.map_err(|_| "cannot walk workspace")?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "invalid search boundary")?;
        if relative.components().any(|c| c.as_os_str() == ".git") {
            continue;
        }
        if permissions.decide("Read", &relative.to_string_lossy(), false) != Decision::Allow {
            continue;
        }
        if glob.as_ref().is_some_and(|g| !g.is_match(relative)) {
            continue;
        }
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
                            relative.display(),
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
            output.push(relative.display().to_string());
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
