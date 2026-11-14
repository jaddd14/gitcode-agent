//! Subagent profiles, custom agent markdown loading, plan mode, todo tracking,
//! interactive user questions, live subagent status tree, and bounded parallel execution.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    future::Future,
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Semaphore;
use unicode_width::UnicodeWidthStr;

/// Maximum byte size allowed for a custom subagent markdown definition file (64 KiB).
pub const MAX_AGENT_FILE_BYTES: u64 = 64 * 1024;

/// Default maximum number of concurrent subagent executions.
pub const DEFAULT_SUBAGENT_CONCURRENCY: usize = 4;

/// Read-only tool names granted to the built-in `explore` and `plan` subagents.
pub const READ_ONLY_TOOLS: &[&str] = &["Read", "LS", "Glob", "Grep"];

/// Full workspace tool set granted to the built-in `general-purpose` subagent.
pub const GENERAL_PURPOSE_TOOLS: &[&str] = &[
    "Read",
    "Write",
    "Edit",
    "MultiEdit",
    "Bash",
    "LS",
    "Glob",
    "Grep",
    "TodoWrite",
    "NotebookEdit",
    "apply_patch",
    "BashOutput",
    "KillShell",
    "McpCall",
    "WebFetch",
    "Skill",
];

/// Definition of a built-in or user-defined subagent profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentProfile {
    /// Unique identifier for the subagent (e.g. `general-purpose`, `explore`, `plan`).
    pub name: String,
    /// Short description explaining when to delegate tasks to this subagent.
    pub description: String,
    /// Explicit list of allowed tool names; empty means unrestricted unless `read_only` is set.
    pub tools: Vec<String>,
    /// Optional model override for this subagent.
    pub model: Option<String>,
    /// Optional UI accent color name or hex code.
    pub color: Option<String>,
    /// System prompt body guiding the subagent's behavior.
    pub system_prompt: String,
    /// Whether the subagent is restricted to read-only operations.
    pub read_only: bool,
}

impl SubagentProfile {
    /// Built-in general-purpose subagent capable of researching and modifying files.
    #[must_use]
    pub fn general_purpose() -> Self {
        Self {
            name: "general-purpose".to_string(),
            description: "General-purpose agent for complex multi-step research and coding tasks."
                .to_string(),
            tools: GENERAL_PURPOSE_TOOLS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            model: None,
            color: Some("cyan".to_string()),
            system_prompt: "You are a general-purpose subagent. Complete the assigned task thoroughly and return a concise report.".to_string(),
            read_only: false,
        }
    }

    /// Built-in fast read-only exploration subagent restricted to `Read`, `LS`, `Glob`, and `Grep`.
    #[must_use]
    pub fn explore() -> Self {
        Self {
            name: "explore".to_string(),
            description:
                "Fast read-only codebase exploration agent using Read, LS, Glob, and Grep."
                    .to_string(),
            tools: READ_ONLY_TOOLS.iter().map(|s| (*s).to_string()).collect(),
            model: None,
            color: Some("blue".to_string()),
            system_prompt: "You are a read-only exploration subagent. Inspect files and search the codebase without modifying anything.".to_string(),
            read_only: true,
        }
    }

    /// Built-in read-only software architect subagent for designing implementation plans.
    #[must_use]
    pub fn plan() -> Self {
        Self {
            name: "plan".to_string(),
            description:
                "Read-only architect agent for designing implementation plans before coding."
                    .to_string(),
            tools: READ_ONLY_TOOLS.iter().map(|s| (*s).to_string()).collect(),
            model: None,
            color: Some("magenta".to_string()),
            system_prompt: "You are a read-only software architect subagent. Analyze the codebase and produce a clear step-by-step implementation plan.".to_string(),
            read_only: true,
        }
    }

    /// Returns the three built-in subagent profiles (`general-purpose`, `explore`, `plan`).
    #[must_use]
    pub fn builtins() -> Vec<Self> {
        vec![Self::general_purpose(), Self::explore(), Self::plan()]
    }

    /// Checks whether this profile permits invoking the given tool name.
    #[must_use]
    pub fn allows_tool(&self, tool: &str) -> bool {
        if matches!(tool, "Task" | "Agent") {
            return false;
        }
        if self.read_only && !READ_ONLY_TOOLS.contains(&tool) {
            return false;
        }
        if self.tools.is_empty() {
            !self.read_only || READ_ONLY_TOOLS.contains(&tool)
        } else {
            self.tools.iter().any(|allowed| allowed == tool)
        }
    }
}

/// Parses a custom subagent markdown file containing YAML-like `---` frontmatter and a prompt body.
///
/// Expected frontmatter fields:
/// - `name`: required non-empty identifier
/// - `description`: required non-empty summary
/// - `tools`: optional comma-separated or bracketed list of tool names
/// - `model`: optional model identifier
/// - `color`: optional color token
pub fn parse_agent_markdown(text: &str) -> Result<SubagentProfile, String> {
    let trimmed = text.trim_start_matches('\u{feff}');
    let rest = trimmed
        .strip_prefix("---\n")
        .or_else(|| trimmed.strip_prefix("---\r\n"))
        .ok_or_else(|| "missing opening YAML frontmatter delimiter '---'".to_string())?;

    let (frontmatter, body) = split_frontmatter(rest)
        .ok_or_else(|| "missing closing YAML frontmatter delimiter '---'".to_string())?;

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut tools: Vec<String> = Vec::new();
    let mut model: Option<String> = None;
    let mut color: Option<String> = None;

    for raw_line in frontmatter.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, raw_val) = line
            .split_once(':')
            .ok_or_else(|| format!("invalid frontmatter line: {line}"))?;
        let key = key.trim();
        let val = unquote(raw_val.trim());

        match key {
            "name" if !val.is_empty() => {
                name = Some(val);
            }
            "description" if !val.is_empty() => {
                description = Some(val);
            }
            "tools" => {
                tools = parse_tools_field(&val);
            }
            "model" if !val.is_empty() => {
                model = Some(val);
            }
            "color" if !val.is_empty() => {
                color = Some(val);
            }
            _ => {}
        }
    }

    let name = name.ok_or_else(|| "frontmatter missing required 'name' field".to_string())?;
    if !is_valid_agent_name(&name) {
        return Err(format!("invalid subagent name '{name}'"));
    }
    let description = description
        .ok_or_else(|| "frontmatter missing required 'description' field".to_string())?;
    let system_prompt = body.trim().to_string();
    if system_prompt.is_empty() {
        return Err("subagent markdown body (system prompt) must not be empty".to_string());
    }

    let read_only = !tools.is_empty()
        && tools
            .iter()
            .all(|t| READ_ONLY_TOOLS.iter().any(|ro| ro == t));

    Ok(SubagentProfile {
        name,
        description,
        tools,
        model,
        color,
        system_prompt,
        read_only,
    })
}

fn split_frontmatter(rest: &str) -> Option<(&str, &str)> {
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            let fm = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return Some((fm, body));
        }
        offset += line.len();
    }
    if rest.ends_with("\n---") {
        let idx = rest.len() - 3;
        return Some((&rest[..idx], ""));
    }
    None
}

fn unquote(s: &str) -> String {
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn parse_tools_field(raw: &str) -> Vec<String> {
    let inner = raw
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(raw);
    inner
        .split(',')
        .map(|item| unquote(item.trim()))
        .filter(|item| !item.is_empty())
        .collect()
}

fn is_valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Loads built-in and custom subagent profiles from `~/.gitcode/agents/*.md` and `.gitcode/agents/*.md`.
///
/// Project agents override user-home agents with the same `name`, and custom agents can override
/// built-in profiles of the same name. Symlinks, paths escaping the agents directory, and files
/// exceeding [`MAX_AGENT_FILE_BYTES`] (64 KiB) are rejected.
pub fn load_agents(home: &Path, project: &Path) -> io::Result<Vec<SubagentProfile>> {
    let mut by_name: BTreeMap<String, SubagentProfile> = BTreeMap::new();
    for builtin in SubagentProfile::builtins() {
        by_name.insert(builtin.name.clone(), builtin);
    }

    let user_dir = home.join(".gitcode").join("agents");
    for profile in load_agents_from_dir(&user_dir)? {
        by_name.insert(profile.name.clone(), profile);
    }

    let project_dir = project.join(".gitcode").join("agents");
    for profile in load_agents_from_dir(&project_dir)? {
        by_name.insert(profile.name.clone(), profile);
    }

    Ok(by_name.into_values().collect())
}

fn load_agents_from_dir(dir: &Path) -> io::Result<Vec<SubagentProfile>> {
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("agents directory must not be a symlink: {}", dir.display()),
                ));
            }
            if !meta.is_dir() {
                return Ok(Vec::new());
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    }

    let canonical_dir = fs::canonicalize(dir)?;
    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("symlinked agent file rejected: {}", path.display()),
            ));
        }
        if !file_type.is_file() {
            continue;
        }
        let is_md = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
        if !is_md {
            continue;
        }
        if has_parent_traversal(&path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("parent path traversal rejected: {}", path.display()),
            ));
        }
        let canonical_file = fs::canonicalize(&path)?;
        if !canonical_file.starts_with(&canonical_dir) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "agent file escapes confined directory: {}",
                    canonical_file.display()
                ),
            ));
        }
        entries.push(canonical_file);
    }

    entries.sort();

    let mut profiles = Vec::with_capacity(entries.len());
    for file_path in entries {
        let meta = fs::symlink_metadata(&file_path)?;
        if meta.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("symlinked agent file rejected: {}", file_path.display()),
            ));
        }
        if meta.len() > MAX_AGENT_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "agent file exceeds 64KiB limit ({} bytes): {}",
                    meta.len(),
                    file_path.display()
                ),
            ));
        }
        let raw = fs::read_to_string(&file_path)?;
        let profile = parse_agent_markdown(&raw)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        profiles.push(profile);
    }

    Ok(profiles)
}

fn has_parent_traversal(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::ParentDir))
}

/// Execution status of a single todo item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Item has not been started yet.
    Pending,
    /// Item is actively being worked on.
    InProgress,
    /// Item has been finished.
    Completed,
}

/// A single tracked checklist item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    /// Stable identifier for the todo item.
    pub id: String,
    /// Human-readable task description.
    pub content: String,
    /// Current lifecycle state of the item.
    pub status: TodoStatus,
}

/// Session todo list supporting both Claude-style `TodoWrite` and Codex-style `update_plan` payloads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoList {
    /// Ordered todo items in the checklist.
    pub items: Vec<TodoItem>,
    /// Optional explanation message supplied by `update_plan`.
    pub explanation: Option<String>,
}

impl TodoList {
    /// Creates an empty [`TodoList`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates the checklist from a JSON payload supporting both `TodoWrite` (`{"todos": [...]}`)
    /// and Codex `update_plan` (`{"plan": [{"step": "...", "status": "..."}], "explanation": "..."}`).
    ///
    /// At most one item may be in [`TodoStatus::InProgress`] at a time.
    pub fn update_from_value(&mut self, input: &Value) -> Result<String, String> {
        let obj = input
            .as_object()
            .ok_or_else(|| "todo update input must be a JSON object".to_string())?;

        let (parsed_items, explanation) = if let Some(todos_val) = obj.get("todos") {
            let arr = todos_val
                .as_array()
                .ok_or_else(|| "'todos' field must be an array".to_string())?;
            let mut items = Vec::with_capacity(arr.len());
            for (idx, entry) in arr.iter().enumerate() {
                let item_obj = entry
                    .as_object()
                    .ok_or_else(|| format!("todo at index {idx} must be an object"))?;
                let content = item_obj
                    .get("content")
                    .or_else(|| item_obj.get("step"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| format!("todo at index {idx} requires non-empty 'content'"))?
                    .to_string();
                let id = item_obj
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map_or_else(|| (idx + 1).to_string(), ToString::to_string);
                let status_str = item_obj
                    .get("status")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("todo at index {idx} requires 'status'"))?;
                let status = parse_todo_status(status_str)?;
                items.push(TodoItem {
                    id,
                    content,
                    status,
                });
            }
            (items, None)
        } else if let Some(plan_val) = obj.get("plan") {
            let arr = plan_val
                .as_array()
                .ok_or_else(|| "'plan' field must be an array".to_string())?;
            let mut items = Vec::with_capacity(arr.len());
            for (idx, entry) in arr.iter().enumerate() {
                let step_obj = entry
                    .as_object()
                    .ok_or_else(|| format!("plan entry at index {idx} must be an object"))?;
                let content = step_obj
                    .get("step")
                    .or_else(|| step_obj.get("content"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| format!("plan entry at index {idx} requires non-empty 'step'"))?
                    .to_string();
                let id = step_obj
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map_or_else(|| (idx + 1).to_string(), ToString::to_string);
                let status_str = step_obj
                    .get("status")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("plan entry at index {idx} requires 'status'"))?;
                let status = parse_todo_status(status_str)?;
                items.push(TodoItem {
                    id,
                    content,
                    status,
                });
            }
            let explanation = obj
                .get("explanation")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            (items, explanation)
        } else {
            return Err("expected 'todos' or 'plan' array in input".to_string());
        };

        let in_progress_count = parsed_items
            .iter()
            .filter(|i| i.status == TodoStatus::InProgress)
            .count();
        if in_progress_count > 1 {
            return Err("at most one todo item may be 'in_progress' at a time".to_string());
        }

        self.items = parsed_items;
        self.explanation = explanation;

        let completed = self
            .items
            .iter()
            .filter(|i| i.status == TodoStatus::Completed)
            .count();
        Ok(format!(
            "Updated todo list ({}/{} completed)",
            completed,
            self.items.len()
        ))
    }

    /// Renders the checklist as human-readable terminal lines.
    #[must_use]
    pub fn render(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(exp) = &self.explanation {
            lines.push(format!("Note: {exp}"));
        }
        if self.items.is_empty() {
            lines.push("No todos.".to_string());
            return lines;
        }
        for item in &self.items {
            let marker = match item.status {
                TodoStatus::Pending => "[ ]",
                TodoStatus::InProgress => "[*]",
                TodoStatus::Completed => "[x]",
            };
            lines.push(format!("{marker} {} ({})", item.content, item.id));
        }
        lines
    }
}

fn parse_todo_status(raw: &str) -> Result<TodoStatus, String> {
    match raw.trim() {
        "pending" => Ok(TodoStatus::Pending),
        "in_progress" => Ok(TodoStatus::InProgress),
        "completed" => Ok(TodoStatus::Completed),
        other => Err(format!("invalid todo status '{other}'")),
    }
}

/// User decision when reviewing a plan proposed via `ExitPlanMode`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanDecision {
    /// Approve the plan and allow automatic execution of edits.
    ApproveAuto,
    /// Approve the plan while keeping manual confirmation for edits.
    ApproveManual,
    /// Reject the current plan and continue planning with user feedback.
    KeepPlanning(String),
}

/// Execution mode produced after transitioning out of plan review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanExecutionMode {
    /// Plan mode is active; only read-only exploration and plan updates are allowed.
    Planning,
    /// Plan was approved for automatic tool execution.
    ApprovedAuto,
    /// Plan was approved for manual step-by-step confirmation.
    ApprovedManual,
}

/// Tracks plan mode lifecycle and `ExitPlanMode` approval transitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanState {
    /// Current plan mode state.
    pub mode: PlanExecutionMode,
    /// Latest proposed plan markdown submitted via `ExitPlanMode`.
    pub proposed_plan: Option<String>,
    /// Latest user feedback when `KeepPlanning` was chosen.
    pub feedback: Option<String>,
}

impl Default for PlanState {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanState {
    /// Starts a new [`PlanState`] in [`PlanExecutionMode::Planning`] mode.
    #[must_use]
    pub fn new() -> Self {
        Self {
            mode: PlanExecutionMode::Planning,
            proposed_plan: None,
            feedback: None,
        }
    }

    /// Returns `true` while the session is still in read-only planning mode.
    #[must_use]
    pub fn is_planning(&self) -> bool {
        self.mode == PlanExecutionMode::Planning
    }

    /// Parses an `ExitPlanMode` tool input (`{"plan": "..."}`) and records the proposed plan text.
    pub fn submit_exit_plan_mode(&mut self, input: &Value) -> Result<String, String> {
        let plan = input
            .get("plan")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "ExitPlanMode requires a non-empty 'plan' string".to_string())?;
        self.proposed_plan = Some(plan.to_string());
        Ok(plan.to_string())
    }

    /// Applies the user's [`PlanDecision`] to transition out of or remain in plan mode.
    pub fn apply_decision(&mut self, decision: PlanDecision) -> Result<PlanExecutionMode, String> {
        if self.proposed_plan.is_none() {
            return Err("cannot apply plan decision before a plan is proposed".to_string());
        }
        match decision {
            PlanDecision::ApproveAuto => {
                self.mode = PlanExecutionMode::ApprovedAuto;
                self.feedback = None;
            }
            PlanDecision::ApproveManual => {
                self.mode = PlanExecutionMode::ApprovedManual;
                self.feedback = None;
            }
            PlanDecision::KeepPlanning(note) => {
                let trimmed = note.trim().to_string();
                self.mode = PlanExecutionMode::Planning;
                self.feedback = Some(trimmed);
            }
        }
        Ok(self.mode)
    }
}

/// A selectable choice in an `AskUserQuestion` prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    /// Concise option label (1-5 words).
    pub label: String,
    /// Explanation of trade-offs or implications if chosen.
    pub description: String,
}

/// A single structured question with 1..=4 selectable options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionItem {
    /// Question text shown to the user.
    pub question: String,
    /// Available choices (between 1 and 4).
    pub options: Vec<QuestionOption>,
    /// Allow selecting several options.
    #[serde(default)]
    pub multi_select: bool,
}

/// Resolved user answer to a [`QuestionItem`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuestionAnswer {
    /// User picked one of the predefined options by 0-based index.
    Selected {
        /// Zero-based index of the chosen option.
        index: usize,
        /// Label of the chosen option.
        label: String,
    },
    /// User provided a custom free-text response.
    Custom(String),
}

/// Validated `AskUserQuestion` prompt supporting 1..=4 questions and 1..=4 options per question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionPrompt {
    /// Validated list of 1..=4 questions.
    pub questions: Vec<QuestionItem>,
}

impl QuestionPrompt {
    /// Parses and validates an `AskUserQuestion` JSON payload.
    ///
    /// Supports both single-question payloads (`{"question": "...", "options": [...]}`)
    /// and multi-question payloads (`{"questions": [{"question": "...", "options": [...]}]}`).
    pub fn from_value(input: &Value) -> Result<Self, String> {
        let obj = input
            .as_object()
            .ok_or_else(|| "AskUserQuestion input must be a JSON object".to_string())?;

        let raw_questions: Vec<&Value> = if let Some(arr) = obj.get("questions") {
            let list = arr
                .as_array()
                .ok_or_else(|| "'questions' must be an array".to_string())?;
            list.iter().collect()
        } else if obj.contains_key("question") {
            vec![input]
        } else {
            return Err("AskUserQuestion requires 'question' or 'questions'".to_string());
        };

        if !(1..=4).contains(&raw_questions.len()) {
            return Err(format!(
                "AskUserQuestion requires between 1 and 4 questions, got {}",
                raw_questions.len()
            ));
        }

        let mut questions = Vec::with_capacity(raw_questions.len());
        for (q_idx, q_val) in raw_questions.iter().enumerate() {
            let q_obj = q_val
                .as_object()
                .ok_or_else(|| format!("question at index {q_idx} must be an object"))?;
            let question = q_obj
                .get("question")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    format!("question at index {q_idx} requires non-empty 'question' text")
                })?
                .to_string();
            let opts_val = q_obj
                .get("options")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("question at index {q_idx} requires 'options' array"))?;
            if !(1..=4).contains(&opts_val.len()) {
                return Err(format!(
                    "question at index {q_idx} requires between 1 and 4 options, got {}",
                    opts_val.len()
                ));
            }
            let mut options = Vec::with_capacity(opts_val.len());
            for (o_idx, o_val) in opts_val.iter().enumerate() {
                let o_obj = o_val
                    .as_object()
                    .ok_or_else(|| format!("option {o_idx} in question {q_idx} must be object"))?;
                let label = o_obj
                    .get("label")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        format!("option {o_idx} in question {q_idx} requires non-empty 'label'")
                    })?
                    .to_string();
                let description = o_obj
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or("")
                    .to_string();
                options.push(QuestionOption { label, description });
            }
            let multi_select = q_obj
                .get("multiSelect")
                .or_else(|| q_obj.get("multi_select"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            questions.push(QuestionItem {
                question,
                options,
                multi_select,
            });
        }

        Ok(Self { questions })
    }

    /// Formats keyboard-first prompt lines for terminal display.
    #[must_use]
    pub fn format_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (q_idx, item) in self.questions.iter().enumerate() {
            if self.questions.len() > 1 {
                lines.push(format!("Q{}: {}", q_idx + 1, item.question));
            } else {
                lines.push(format!("? {}", item.question));
            }
            for (opt_idx, opt) in item.options.iter().enumerate() {
                let key_num = opt_idx + 1;
                if opt.description.is_empty() {
                    lines.push(format!("  [{key_num}] {}", opt.label));
                } else {
                    lines.push(format!("  [{key_num}] {} - {}", opt.label, opt.description));
                }
            }
            lines.push(format!(
                "  [{}] Other (type a custom reply)",
                item.options.len() + 1
            ));
        }
        lines
    }

    /// Resolves a user keyboard selection (1-based number or custom text) for the given question index.
    pub fn resolve_input(
        &self,
        question_index: usize,
        raw_input: &str,
    ) -> Result<QuestionAnswer, String> {
        let item = self
            .questions
            .get(question_index)
            .ok_or_else(|| format!("question index {question_index} out of bounds"))?;
        let trimmed = raw_input.trim();
        if trimmed.is_empty() {
            return Err("reply must not be empty".to_string());
        }
        if let Ok(choice_num) = trimmed.parse::<usize>() {
            if (1..=item.options.len()).contains(&choice_num) {
                let idx = choice_num - 1;
                let label = item.options[idx].label.clone();
                return Ok(QuestionAnswer::Selected { index: idx, label });
            }
            return Err(format!(
                "selection {choice_num} out of range (1..={})",
                item.options.len()
            ));
        }
        Ok(QuestionAnswer::Custom(trimmed.to_string()))
    }
}

/// Lifecycle state of a tracked subagent run in [`SubagentTree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentRunState {
    /// Subagent is actively executing.
    Running,
    /// Subagent completed successfully.
    Completed,
    /// Subagent terminated with an error.
    Failed,
}

/// Snapshot of a single active or finished subagent execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentRun {
    /// Unique identifier for this run.
    pub id: String,
    /// Subagent profile name (e.g. `explore`, `plan`).
    pub name: String,
    /// Brief description of the delegated task.
    pub task: String,
    /// Name of the tool currently executing, if any.
    pub current_tool: Option<String>,
    /// Total tokens consumed by this subagent so far.
    pub tokens: u64,
    /// Elapsed wall-clock time in milliseconds.
    pub elapsed_ms: u64,
    /// Current lifecycle state.
    pub state: SubagentRunState,
}

/// Live status tracker that renders active and completed subagent runs as a compact tree.
#[derive(Debug, Clone, Default)]
pub struct SubagentTree {
    /// Ordered list of subagent runs.
    pub runs: Vec<SubagentRun>,
}

impl SubagentTree {
    /// Creates an empty [`SubagentTree`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a new running subagent entry and returns its index.
    pub fn start_run(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        task: impl Into<String>,
    ) -> usize {
        let idx = self.runs.len();
        self.runs.push(SubagentRun {
            id: id.into(),
            name: name.into(),
            task: task.into(),
            current_tool: None,
            tokens: 0,
            elapsed_ms: 0,
            state: SubagentRunState::Running,
        });
        idx
    }

    /// Updates live telemetry (current tool, token count, elapsed milliseconds) for a run by ID.
    pub fn update_run(
        &mut self,
        id: &str,
        current_tool: Option<String>,
        tokens: u64,
        elapsed_ms: u64,
    ) -> bool {
        if let Some(run) = self.runs.iter_mut().find(|r| r.id == id) {
            run.current_tool = current_tool;
            run.tokens = tokens;
            run.elapsed_ms = elapsed_ms;
            true
        } else {
            false
        }
    }

    /// Marks a subagent run as finished (`Completed` or `Failed`) with final token and time stats.
    pub fn finish_run(&mut self, id: &str, succeeded: bool, tokens: u64, elapsed_ms: u64) -> bool {
        if let Some(run) = self.runs.iter_mut().find(|r| r.id == id) {
            run.current_tool = None;
            run.tokens = tokens;
            run.elapsed_ms = elapsed_ms;
            run.state = if succeeded {
                SubagentRunState::Completed
            } else {
                SubagentRunState::Failed
            };
            true
        } else {
            false
        }
    }

    /// Renders a compact borderless tree view fitted to `width` columns.
    #[must_use]
    pub fn render_tree(&self, width: usize) -> Vec<String> {
        if self.runs.is_empty() {
            return Vec::new();
        }
        let max_width = width.max(20);
        let mut lines = Vec::with_capacity(self.runs.len() + 1);
        let active = self
            .runs
            .iter()
            .filter(|r| r.state == SubagentRunState::Running)
            .count();
        let header = format!("Subagents ({active}/{} active)", self.runs.len());
        lines.push(truncate_to_width(&header, max_width));

        let last_idx = self.runs.len().saturating_sub(1);
        for (idx, run) in self.runs.iter().enumerate() {
            let branch = if idx == last_idx { "└─" } else { "├─" };
            let status_icon = match run.state {
                SubagentRunState::Running => "*",
                SubagentRunState::Completed => "✓",
                SubagentRunState::Failed => "✗",
            };
            let tool_part = match &run.current_tool {
                Some(tool) => format!(" [{tool}]"),
                None => String::new(),
            };
            let secs = run.elapsed_ms / 1000;
            let tenths = (run.elapsed_ms % 1000) / 100;
            let line = format!(
                "{branch} {status_icon} {} ({}){tool_part} · {} tok · {secs}.{tenths}s",
                run.name, run.task, run.tokens
            );
            lines.push(truncate_to_width(&line, max_width));
        }
        lines
    }
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let target = max_width - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthStr::width(ch.encode_utf8(&mut [0; 4]) as &str);
        if used + ch_width > target {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out.push('…');
    out
}

/// Bounded parallel runner for subagent tasks using a [`tokio::sync::Semaphore`] and depth guard.
#[derive(Debug, Clone)]
pub struct SubagentRunner {
    semaphore: Arc<Semaphore>,
    max_concurrency: usize,
    max_depth: usize,
}

impl Default for SubagentRunner {
    fn default() -> Self {
        Self::new(DEFAULT_SUBAGENT_CONCURRENCY)
    }
}

impl SubagentRunner {
    /// Creates a new [`SubagentRunner`] with the given concurrency cap and a default depth limit of `1`
    /// (preventing subagents at depth >= 1 from spawning nested subagents).
    #[must_use]
    pub fn new(max_concurrency: usize) -> Self {
        let cap = max_concurrency.max(1);
        Self {
            semaphore: Arc::new(Semaphore::new(cap)),
            max_concurrency: cap,
            max_depth: 1,
        }
    }

    /// Returns a new [`SubagentRunner`] with a custom maximum nesting depth.
    #[must_use]
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Returns the configured maximum concurrency.
    #[must_use]
    pub fn max_concurrency(&self) -> usize {
        self.max_concurrency
    }

    /// Returns the configured maximum nesting depth.
    #[must_use]
    pub fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// Checks whether a caller at `current_depth` is allowed to spawn a subagent.
    pub fn check_depth(&self, current_depth: usize) -> Result<usize, String> {
        if current_depth >= self.max_depth {
            Err(format!(
                "subagent spawning rejected: depth {current_depth} reaches max_depth {}",
                self.max_depth
            ))
        } else {
            Ok(current_depth + 1)
        }
    }

    /// Executes a batch of async subagent tasks concurrently, bounded by the configured semaphore
    /// and guarded against nested subagent spawning when `current_depth >= max_depth`.
    pub async fn run_parallel<T, Fut, F>(
        &self,
        current_depth: usize,
        tasks: Vec<T>,
        worker: F,
    ) -> Result<Vec<Result<String, String>>, String>
    where
        T: Send + 'static,
        F: Fn(T, usize) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        let child_depth = self.check_depth(current_depth)?;
        let worker = Arc::new(worker);
        let mut handles = Vec::with_capacity(tasks.len());

        for task in tasks {
            let sem = Arc::clone(&self.semaphore);
            let worker = Arc::clone(&worker);
            handles.push(tokio::spawn(async move {
                let permit = sem
                    .acquire_owned()
                    .await
                    .map_err(|_| "subagent semaphore closed".to_string())?;
                let res = worker(task, child_depth).await;
                drop(permit);
                res
            }));
        }

        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            match handle.await {
                Ok(out) => results.push(out),
                Err(join_err) => results.push(Err(format!("subagent task panicked: {join_err}"))),
            }
        }
        Ok(results)
    }
}
