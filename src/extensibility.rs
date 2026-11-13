//! Extensibility subsystem: built-in slash command registry, custom markdown commands,
//! progressive-disclosure skills discovery, and lifecycle command hooks engine.

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// Metadata describing a built-in or custom slash command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommandInfo {
    /// Command name without the leading slash (e.g. `"help"` or `"git:sync"`).
    pub name: String,
    /// Concise human-readable description of what the command does.
    pub description: String,
    /// Optional hint describing expected arguments (e.g. `"[model-id]"`).
    pub argument_hint: Option<String>,
}

/// Static specification of a built-in slash command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinSlashSpec {
    /// Command name without the leading slash.
    pub name: &'static str,
    /// Concise human-readable description of the command.
    pub description: &'static str,
    /// Optional argument hint displayed during completion.
    pub argument_hint: Option<&'static str>,
}

impl BuiltinSlashSpec {
    /// Converts this static specification into an owned [`SlashCommandInfo`].
    #[must_use]
    pub fn to_info(self) -> SlashCommandInfo {
        SlashCommandInfo {
            name: self.name.to_string(),
            description: self.description.to_string(),
            argument_hint: self.argument_hint.map(str::to_string),
        }
    }
}

/// Complete registry of built-in slash commands.
pub const BUILTIN_COMMANDS: &[BuiltinSlashSpec] = &[
    BuiltinSlashSpec {
        name: "help",
        description: "Show available commands and keyboard shortcuts",
        argument_hint: Some("[topic]"),
    },
    BuiltinSlashSpec {
        name: "clear",
        description: "Clear conversation history and start a fresh context",
        argument_hint: None,
    },
    BuiltinSlashSpec {
        name: "compact",
        description: "Compact conversation history into a concise summary",
        argument_hint: Some("[instructions]"),
    },
    BuiltinSlashSpec {
        name: "config",
        description: "Inspect or update configuration settings",
        argument_hint: Some("[key] [value]"),
    },
    BuiltinSlashSpec {
        name: "model",
        description: "View or switch the active language model",
        argument_hint: Some("[model-id]"),
    },
    BuiltinSlashSpec {
        name: "effort",
        description: "Choose the reasoning effort for model requests",
        argument_hint: Some("[default|low|medium|high|xhigh|max]"),
    },
    BuiltinSlashSpec {
        name: "provider",
        description: "View or switch the active model provider",
        argument_hint: Some("[provider-name]"),
    },
    BuiltinSlashSpec {
        name: "usage",
        description: "Show token usage and context window utilization",
        argument_hint: None,
    },
    BuiltinSlashSpec {
        name: "cost",
        description: "Display cumulative session token usage and estimated cost",
        argument_hint: None,
    },
    BuiltinSlashSpec {
        name: "status",
        description: "Display current session, workspace, and provider status",
        argument_hint: None,
    },
    BuiltinSlashSpec {
        name: "context",
        description: "Inspect breakdown of token usage in the active context",
        argument_hint: None,
    },
    BuiltinSlashSpec {
        name: "resume",
        description: "Resume a previously saved conversation session",
        argument_hint: Some("[session-id]"),
    },
    BuiltinSlashSpec {
        name: "rewind",
        description: "Rewind conversation and workspace to an earlier checkpoint",
        argument_hint: Some("[turn-index]"),
    },
    BuiltinSlashSpec {
        name: "plan",
        description: "Toggle or inspect architectural planning mode",
        argument_hint: Some("[task]"),
    },
    BuiltinSlashSpec {
        name: "permissions",
        description: "View or modify tool execution permission rules",
        argument_hint: Some("[rule]"),
    },
    BuiltinSlashSpec {
        name: "agents",
        description: "List or manage specialized subagent definitions",
        argument_hint: Some("[agent-name]"),
    },
    BuiltinSlashSpec {
        name: "mcp",
        description: "Inspect and manage Model Context Protocol servers",
        argument_hint: Some("[server-name]"),
    },
    BuiltinSlashSpec {
        name: "init",
        description: "Initialize project instructions and workspace guide",
        argument_hint: None,
    },
    BuiltinSlashSpec {
        name: "review",
        description: "Review current working tree diff or branch changes",
        argument_hint: Some("[target]"),
    },
    BuiltinSlashSpec {
        name: "diff",
        description: "Show git diff of uncommitted workspace changes",
        argument_hint: Some("[path]"),
    },
    BuiltinSlashSpec {
        name: "commit",
        description: "Stage changes and create a structured git commit",
        argument_hint: Some("[message]"),
    },
    BuiltinSlashSpec {
        name: "memory",
        description: "View or edit persistent workspace memory files",
        argument_hint: Some("[note]"),
    },
    BuiltinSlashSpec {
        name: "doctor",
        description: "Run diagnostics on configuration, providers, and tools",
        argument_hint: None,
    },
    BuiltinSlashSpec {
        name: "theme",
        description: "Switch terminal color theme",
        argument_hint: Some("[theme-name]"),
    },
    BuiltinSlashSpec {
        name: "vim",
        description: "Toggle Vim keybindings in the prompt editor",
        argument_hint: None,
    },
    BuiltinSlashSpec {
        name: "export",
        description: "Export conversation transcript to a file",
        argument_hint: Some("[path]"),
    },
    BuiltinSlashSpec {
        name: "copy",
        description: "Copy the last assistant response or code block to clipboard",
        argument_hint: None,
    },
    BuiltinSlashSpec {
        name: "hooks",
        description: "Inspect configured lifecycle hooks",
        argument_hint: None,
    },
    BuiltinSlashSpec {
        name: "skills",
        description: "List discovered skills and their descriptions",
        argument_hint: None,
    },
    BuiltinSlashSpec {
        name: "add-dir",
        description: "Add an additional directory to the workspace scope",
        argument_hint: Some("<path>"),
    },
    BuiltinSlashSpec {
        name: "login",
        description: "Authenticate with the active model provider",
        argument_hint: Some("[provider]"),
    },
    BuiltinSlashSpec {
        name: "logout",
        description: "Clear stored credentials for a provider",
        argument_hint: Some("[provider]"),
    },
    BuiltinSlashSpec {
        name: "terminal-setup",
        description: "Configure terminal keybindings for multiline input",
        argument_hint: None,
    },
    BuiltinSlashSpec {
        name: "exit",
        description: "Exit the session and close the application",
        argument_hint: None,
    },
];

/// Completes built-in slash commands matching `prefix` using prefix, substring, and subsequence matching.
///
/// A leading `'/'` on `prefix` is stripped automatically. Results are ordered with exact matches first,
/// followed by prefix matches, substring matches, and fuzzy subsequence matches.
#[must_use]
pub fn complete_slash(prefix: &str) -> Vec<SlashCommandInfo> {
    complete_slash_with_custom(prefix, &[])
}

/// Completes both built-in and custom slash commands matching `prefix`.
#[must_use]
pub fn complete_slash_with_custom(prefix: &str, custom: &[CustomCommand]) -> Vec<SlashCommandInfo> {
    let query = prefix
        .trim()
        .strip_prefix('/')
        .unwrap_or(prefix.trim())
        .to_ascii_lowercase();
    let mut scored: Vec<(u8, usize, SlashCommandInfo)> = Vec::new();

    let candidates = BUILTIN_COMMANDS
        .iter()
        .map(|b| b.to_info())
        .chain(custom.iter().map(CustomCommand::to_slash_info));

    for (idx, info) in candidates.enumerate() {
        let name_lower = info.name.to_ascii_lowercase();
        if query.is_empty() {
            scored.push((0, idx, info));
            continue;
        }
        if name_lower == query {
            scored.push((0, idx, info));
        } else if name_lower.starts_with(&query) {
            scored.push((1, idx, info));
        } else if name_lower.contains(&query) {
            scored.push((2, idx, info));
        } else if is_subsequence(&query, &name_lower) {
            scored.push((3, idx, info));
        }
    }

    scored.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.2.name.cmp(&b.2.name))
            .then_with(|| a.1.cmp(&b.1))
    });
    scored.into_iter().map(|(_, _, info)| info).collect()
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut hay_chars = haystack.chars();
    for c in needle.chars() {
        if !hay_chars.any(|h| h == c) {
            return false;
        }
    }
    true
}

/// Scope from which a custom command or skill was loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtensionSource {
    /// Loaded from the user's home directory (`~/.gitcode/...`).
    User,
    /// Loaded from the project workspace (`.gitcode/...`).
    Project,
    /// Loaded from a read-only compatibility fallback (`~/.claude/...` or `~/.codex/...`).
    Fallback,
}

/// A custom slash command loaded from `.gitcode/commands/` or `~/.gitcode/commands/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomCommand {
    /// Command name (`namespace:name` for nested folders, or `name` at root).
    pub name: String,
    /// Description from frontmatter or first heading/paragraph.
    pub description: String,
    /// Optional argument hint from frontmatter (`argument-hint`).
    pub argument_hint: Option<String>,
    /// Optional list of allowed tools from frontmatter (`allowed-tools`).
    pub allowed_tools: Vec<String>,
    /// Optional model override from frontmatter (`model`).
    pub model: Option<String>,
    /// Raw markdown template body (with frontmatter stripped).
    pub template: String,
    /// Source file path on disk.
    pub path: PathBuf,
    /// Origin scope (`User` or `Project`).
    pub source: ExtensionSource,
}

impl CustomCommand {
    /// Converts this custom command into a [`SlashCommandInfo`] entry for completion.
    #[must_use]
    pub fn to_slash_info(&self) -> SlashCommandInfo {
        SlashCommandInfo {
            name: self.name.clone(),
            description: self.description.clone(),
            argument_hint: self.argument_hint.clone(),
        }
    }

    /// Expands `$ARGUMENTS`, positional `$1`..`$9`, and confined `@relative/path` file inclusions
    /// relative to `project_root`.
    ///
    /// # Errors
    /// Returns an [`io::Error`] if an `@file` reference escapes `project_root`, traverses a symlink,
    /// or cannot be read.
    pub fn render(&self, raw_args: &str, project_root: &Path) -> io::Result<String> {
        expand_command_template(&self.template, raw_args, project_root)
    }
}

/// Expands `$ARGUMENTS`, positional `$1`..`$9`, and confined `@relative/file` inclusions inside `template`.
///
/// # Errors
/// Returns an [`io::Error`] if any referenced `@relative/file` escapes `project_root`, points to or
/// traverses a symlink, or fails to be read.
pub fn expand_command_template(
    template: &str,
    raw_args: &str,
    project_root: &Path,
) -> io::Result<String> {
    let trimmed_args = raw_args.trim();
    let positional = split_arguments(trimmed_args);

    // First expand @relative/file inclusions from template before substituting untrusted user args,
    // or also expand any @path in template tokens.
    let with_files = expand_file_inclusions(template, project_root)?;

    let mut out = String::with_capacity(with_files.len() + trimmed_args.len());
    let chars: Vec<char> = with_files.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            let rest: String = chars[i + 1..].iter().collect();
            if rest.starts_with("ARGUMENTS") {
                out.push_str(trimmed_args);
                i += 1 + "ARGUMENTS".len();
                continue;
            }
            if let Some(&next_ch) = chars.get(i + 1) {
                if ('1'..='9').contains(&next_ch) {
                    let idx = (next_ch as usize) - ('1' as usize);
                    if let Some(val) = positional.get(idx) {
                        out.push_str(val);
                    }
                    i += 2;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }

    Ok(out)
}

fn split_arguments(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' if in_double || !in_single => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

fn expand_file_inclusions(template: &str, project_root: &Path) -> io::Result<String> {
    let mut out = String::with_capacity(template.len());
    let chars: Vec<char> = template.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '@' {
            let preceded_by_boundary = if i == 0 {
                true
            } else {
                let prev = chars[i - 1];
                prev.is_whitespace() || matches!(prev, '(' | '[' | '{' | '"' | '\'' | '`')
            };

            if preceded_by_boundary {
                let mut end = i + 1;
                while end < chars.len() && is_path_token_char(chars[end]) {
                    end += 1;
                }
                while end > i + 1
                    && matches!(chars[end - 1], '.' | ',' | ';' | ':' | ')' | ']' | '}')
                {
                    end -= 1;
                }
                if end > i + 1 {
                    let raw_rel: String = chars[i + 1..end].iter().collect();
                    let content = read_confined_project_file(project_root, &raw_rel)?;
                    out.push_str(&content);
                    i = end;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }

    Ok(out)
}

fn is_path_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-')
}

/// Reads a relative file inside `project_root`, rejecting `..`, absolute paths, and symlinks at any component.
///
/// # Errors
/// Returns [`io::ErrorKind::PermissionDenied`] or [`io::ErrorKind::InvalidInput`] if the path attempts
/// to escape `project_root` or traverses any symlink.
pub fn read_confined_project_file(project_root: &Path, relative_str: &str) -> io::Result<String> {
    let rel_path = Path::new(relative_str);
    if rel_path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "absolute file inclusion paths are forbidden",
        ));
    }

    let mut clean_rel = PathBuf::new();
    for comp in rel_path.components() {
        match comp {
            Component::Normal(part) => clean_rel.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "parent directory traversal (..) is forbidden in file inclusions",
                ));
            }
        }
    }

    if clean_rel.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty file inclusion path",
        ));
    }

    let canonical_root = fs::canonicalize(project_root)?;
    let mut cursor = canonical_root.clone();
    for comp in clean_rel.components() {
        if let Component::Normal(part) = comp {
            cursor.push(part);
            let meta = fs::symlink_metadata(&cursor)?;
            if meta.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "symlinks are forbidden in confined file inclusions",
                ));
            }
        }
    }

    let canonical_target = fs::canonicalize(&cursor)?;
    if !canonical_target.starts_with(&canonical_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "resolved file escapes project root",
        ));
    }

    fs::read_to_string(&canonical_target)
}

/// Loads custom markdown commands from `~/.gitcode/commands/` and `<project>/.gitcode/commands/`.
///
/// Nested folders produce namespaced commands (`namespace:name`). If both user and project define
/// the same command name, the project command takes precedence.
///
/// # Errors
/// Returns an [`io::Error`] if reading a directory or markdown file fails unexpectedly.
pub fn load_custom_commands(home: &Path, project: &Path) -> io::Result<Vec<CustomCommand>> {
    let mut by_name: BTreeMap<String, CustomCommand> = BTreeMap::new();

    let user_dir = home.join(".gitcode").join("commands");
    if user_dir.is_dir() {
        let mut user_cmds = Vec::new();
        collect_markdown_commands(&user_dir, &user_dir, ExtensionSource::User, &mut user_cmds)?;
        for cmd in user_cmds {
            by_name.insert(cmd.name.clone(), cmd);
        }
    }

    let project_dir = project.join(".gitcode").join("commands");
    if project_dir.is_dir() {
        let mut proj_cmds = Vec::new();
        collect_markdown_commands(
            &project_dir,
            &project_dir,
            ExtensionSource::Project,
            &mut proj_cmds,
        )?;
        for cmd in proj_cmds {
            by_name.insert(cmd.name.clone(), cmd);
        }
    }

    Ok(by_name.into_values().collect())
}

fn collect_markdown_commands(
    root: &Path,
    current: &Path,
    source: ExtensionSource,
    out: &mut Vec<CustomCommand>,
) -> io::Result<()> {
    let mut entries: Vec<fs::DirEntry> = Vec::new();
    for entry in fs::read_dir(current)? {
        entries.push(entry?);
    }
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_markdown_commands(root, &path, source, out)?;
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Some(cmd) = parse_custom_command_file(root, &path, source)? {
                out.push(cmd);
            }
        }
    }
    Ok(())
}

fn parse_custom_command_file(
    root: &Path,
    path: &Path,
    source: ExtensionSource,
) -> io::Result<Option<CustomCommand>> {
    let rel = match path.strip_prefix(root) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    let mut parts: Vec<String> = Vec::new();
    for comp in rel.components() {
        if let Component::Normal(os) = comp {
            if let Some(s) = os.to_str() {
                parts.push(s.to_string());
            }
        }
    }
    let Some(last) = parts.last_mut() else {
        return Ok(None);
    };
    if let Some(stem) = last.strip_suffix(".md") {
        *last = stem.to_string();
    }
    let name = parts.join(":");

    let raw = fs::read_to_string(path)?;
    let (frontmatter, body) = split_frontmatter(&raw);

    let description = frontmatter
        .get("description")
        .cloned()
        .filter(|s| !s.is_empty())
        .or_else(|| first_non_empty_line(body))
        .unwrap_or_else(|| format!("Custom command {name}"));

    let argument_hint = frontmatter
        .get("argument-hint")
        .or_else(|| frontmatter.get("argument_hint"))
        .cloned()
        .filter(|s| !s.is_empty());

    let allowed_tools = frontmatter
        .get("allowed-tools")
        .or_else(|| frontmatter.get("allowed_tools"))
        .map(|s| parse_tool_list(s))
        .unwrap_or_default();

    let model = frontmatter.get("model").cloned().filter(|s| !s.is_empty());

    Ok(Some(CustomCommand {
        name,
        description,
        argument_hint,
        allowed_tools,
        model,
        template: body.to_string(),
        path: path.to_path_buf(),
        source,
    }))
}

fn split_frontmatter(raw: &str) -> (BTreeMap<String, String>, &str) {
    let mut map = BTreeMap::new();
    let trimmed = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let Some(after_open) = trimmed
        .strip_prefix("---\r\n")
        .or_else(|| trimmed.strip_prefix("---\n"))
    else {
        return (map, trimmed);
    };

    let close_pos = after_open
        .find("\n---\n")
        .map(|idx| (idx, 5))
        .or_else(|| after_open.find("\n---\r\n").map(|idx| (idx, 6)))
        .or_else(|| {
            after_open
                .strip_prefix("---\n")
                .map(|_| (0, 4))
                .or_else(|| after_open.strip_prefix("---\r\n").map(|_| (0, 5)))
        });

    let Some((idx, delim_len)) = close_pos else {
        return (map, trimmed);
    };

    let yaml_block = &after_open[..idx];
    let body = &after_open[idx + delim_len..];

    let mut current_key: Option<String> = None;
    let mut list_items: Vec<String> = Vec::new();

    for line in yaml_block.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(item) = t.strip_prefix("- ") {
            if current_key.is_some() {
                list_items.push(unquote(item.trim()));
                continue;
            }
        }
        if let Some(prev_key) = current_key.take() {
            if !list_items.is_empty() {
                map.insert(prev_key, list_items.join(", "));
                list_items.clear();
            }
        }
        if let Some((k, v)) = t.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = unquote(v.trim());
            if val.is_empty() {
                current_key = Some(key);
            } else {
                map.insert(key, val);
            }
        }
    }

    if let Some(prev_key) = current_key.take() {
        if !list_items.is_empty() {
            map.insert(prev_key, list_items.join(", "));
        }
    }

    (map, body)
}

fn unquote(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        return trimmed[1..trimmed.len() - 1].to_string();
    }
    trimmed.to_string()
}

fn parse_tool_list(raw: &str) -> Vec<String> {
    let inner = raw
        .trim()
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(raw);
    inner
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(unquote)
        .filter(|s| !s.is_empty())
        .collect()
}

fn first_non_empty_line(body: &str) -> Option<String> {
    for line in body.lines() {
        let t = line.trim().trim_start_matches('#').trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    None
}

/// Lightweight progressive-disclosure specification of a discovered skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSpec {
    /// Skill identifier (from frontmatter `name` or parent directory name).
    pub name: String,
    /// Brief description kept in context summary for progressive disclosure.
    pub description: String,
    /// Filesystem path to `SKILL.md` for loading the full body on demand.
    pub path: PathBuf,
    /// Origin scope (`Project`, `User`, or read-only `Fallback`).
    pub source: ExtensionSource,
    /// Whether this skill comes from a read-only compatibility fallback (`~/.claude` or `~/.codex`).
    pub read_only: bool,
}

/// Discovers skills across `.gitcode/skills/*/SKILL.md`, `~/.gitcode/skills/*/SKILL.md`,
/// and read-only compatibility fallbacks `~/.claude/skills/*/SKILL.md` and `~/.codex/skills/*/SKILL.md`.
///
/// Precedence from lowest to highest: `~/.codex` -> `~/.claude` -> `~/.gitcode` -> `<project>/.gitcode`.
///
/// # Errors
/// Returns an [`io::Error`] if reading a skill directory or `SKILL.md` file fails.
pub fn discover_skills(home: &Path, project: &Path) -> io::Result<Vec<SkillSpec>> {
    let mut by_name: BTreeMap<String, SkillSpec> = BTreeMap::new();

    let search_roots = [
        (
            home.join(".codex").join("skills"),
            ExtensionSource::Fallback,
            true,
        ),
        (
            home.join(".claude").join("skills"),
            ExtensionSource::Fallback,
            true,
        ),
        (
            home.join(".gitcode").join("skills"),
            ExtensionSource::User,
            false,
        ),
        (
            project.join(".gitcode").join("skills"),
            ExtensionSource::Project,
            false,
        ),
    ];

    for (root, source, read_only) in search_roots {
        if !root.is_dir() {
            continue;
        }
        let mut entries: Vec<fs::DirEntry> = Vec::new();
        for entry in fs::read_dir(&root)? {
            entries.push(entry?);
        }
        entries.sort_by_key(fs::DirEntry::file_name);

        for entry in entries {
            let dir_path = entry.path();
            if !dir_path.is_dir() {
                continue;
            }
            let skill_md = dir_path.join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }
            let fallback_name = entry
                .file_name()
                .to_str()
                .unwrap_or("unknown-skill")
                .to_string();
            let raw = fs::read_to_string(&skill_md)?;
            let (fm, body) = split_frontmatter(&raw);
            let name = fm
                .get("name")
                .cloned()
                .filter(|s| !s.is_empty())
                .unwrap_or(fallback_name);
            let description = fm
                .get("description")
                .cloned()
                .filter(|s| !s.is_empty())
                .or_else(|| first_non_empty_line(body))
                .unwrap_or_else(|| format!("Skill {name}"));

            by_name.insert(
                name.clone(),
                SkillSpec {
                    name,
                    description,
                    path: skill_md,
                    source,
                    read_only,
                },
            );
        }
    }

    Ok(by_name.into_values().collect())
}

/// Formats a compact index of discovered skills (names, descriptions, and paths) for progressive disclosure.
#[must_use]
pub fn format_skill_index(skills: &[SkillSpec]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from("Available skills:\n");
    for skill in skills {
        out.push_str("- ");
        out.push_str(&skill.name);
        out.push_str(": ");
        out.push_str(&skill.description);
        out.push_str(" (");
        out.push_str(&skill.path.display().to_string());
        out.push_str(")\n");
    }
    out
}

/// Loads the full markdown body of a skill on demand from its `SKILL.md` path.
///
/// # Errors
/// Returns an [`io::Error`] if the skill file cannot be read.
pub fn load_skill_body(skill: &SkillSpec) -> io::Result<String> {
    let raw = fs::read_to_string(&skill.path)?;
    let (_, body) = split_frontmatter(&raw);
    Ok(body.trim().to_string())
}

/// Lifecycle hook event types supported by the hooks engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    /// Fired before a tool is executed.
    PreToolUse,
    /// Fired after a tool finishes executing.
    PostToolUse,
    /// Fired when the user submits a prompt.
    UserPromptSubmit,
    /// Fired when the main agent finishes responding.
    Stop,
    /// Fired when a subagent finishes responding.
    SubagentStop,
    /// Fired when a session starts.
    SessionStart,
    /// Fired when a session ends.
    SessionEnd,
    /// Fired when the agent emits a user notification.
    Notification,
    /// Fired before context compaction runs.
    PreCompact,
}

/// A configured command hook rule associated with a [`HookEvent`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRule {
    /// Lifecycle event that triggers this rule.
    pub event: HookEvent,
    /// Optional regex or exact matcher string tested against the target (e.g. tool name).
    pub matcher: Option<String>,
    /// Shell command executed via `/bin/sh -c`.
    pub command: String,
    /// Execution timeout in milliseconds.
    pub timeout_ms: u64,
}

impl HookRule {
    /// Checks whether this rule applies to `event` and optional `target` (such as a tool name).
    #[must_use]
    pub fn matches(&self, event: HookEvent, target: Option<&str>) -> bool {
        if self.event != event {
            return false;
        }
        let Some(pattern) = self.matcher.as_deref() else {
            return true;
        };
        if pattern.is_empty() || pattern == "*" {
            return true;
        }
        let candidate = target.unwrap_or("");
        if pattern == candidate {
            return true;
        }
        match Regex::new(pattern) {
            Ok(re) => re.is_match(candidate),
            Err(_) => false,
        }
    }
}

/// Decision returned by a hook execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookDecision {
    /// Allow the lifecycle action to proceed.
    Allow,
    /// Deny or block the lifecycle action.
    Deny,
}

/// Structured result of executing one or more lifecycle hooks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookOutcome {
    /// Whether the action is allowed or denied.
    pub decision: HookDecision,
    /// Optional reason or feedback message (fed back to the model when denied or blocked via exit code 2).
    pub reason: Option<String>,
    /// Optional additional context injected into the conversation.
    pub additional_context: Option<String>,
    /// Sanitized diagnostic log message (guaranteed never to contain the raw hook command string).
    pub diagnostic: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HookStdoutJson {
    decision: Option<HookDecision>,
    reason: Option<String>,
    #[serde(rename = "additionalContext", alias = "additional_context")]
    additional_context: Option<String>,
}

/// Executes a single [`HookRule`] via `/bin/sh -c`, passing `payload` as JSON on `stdin` and
/// enforcing `rule.timeout_ms`.
///
/// The raw `rule.command` string is never included in any error or diagnostic message.
pub async fn run_hook(rule: &HookRule, payload: &Value) -> HookOutcome {
    let timeout_dur = Duration::from_millis(rule.timeout_ms.max(1));
    let payload_bytes = serde_json::to_vec(payload).unwrap_or_else(|_| b"{}".to_vec());

    let mut child = match Command::new("/bin/sh")
        .arg("-c")
        .arg(&rule.command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            return HookOutcome {
                decision: HookDecision::Allow,
                reason: None,
                additional_context: None,
                diagnostic: Some(format!(
                    "hook for {:?} failed to spawn ({})",
                    rule.event,
                    err.kind()
                )),
            };
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&payload_bytes).await;
        let _ = stdin.shutdown().await;
    }

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    let wait_fut = async {
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        if let Some(ref mut out) = stdout_pipe {
            let _ = out.read_to_end(&mut stdout_buf).await;
        }
        if let Some(ref mut err) = stderr_pipe {
            let _ = err.read_to_end(&mut stderr_buf).await;
        }
        let status = child.wait().await;
        (status, stdout_buf, stderr_buf)
    };

    let (status_res, stdout_buf, stderr_buf) =
        match tokio::time::timeout(timeout_dur, wait_fut).await {
            Ok(res) => res,
            Err(_) => {
                let _ = child.kill().await;
                return HookOutcome {
                    decision: HookDecision::Allow,
                    reason: None,
                    additional_context: None,
                    diagnostic: Some(format!(
                        "hook for {:?} timed out after {}ms",
                        rule.event, rule.timeout_ms
                    )),
                };
            }
        };

    let status = match status_res {
        Ok(s) => s,
        Err(err) => {
            return HookOutcome {
                decision: HookDecision::Allow,
                reason: None,
                additional_context: None,
                diagnostic: Some(format!(
                    "hook for {:?} wait error ({})",
                    rule.event,
                    err.kind()
                )),
            };
        }
    };

    let stdout_str = String::from_utf8_lossy(&stdout_buf).trim().to_string();
    let stderr_str = String::from_utf8_lossy(&stderr_buf).trim().to_string();

    match status.code() {
        Some(0) => {
            if !stdout_str.is_empty() {
                if let Ok(parsed) = serde_json::from_str::<HookStdoutJson>(&stdout_str) {
                    return HookOutcome {
                        decision: parsed.decision.unwrap_or(HookDecision::Allow),
                        reason: parsed.reason,
                        additional_context: parsed.additional_context,
                        diagnostic: None,
                    };
                }
            }
            HookOutcome {
                decision: HookDecision::Allow,
                reason: None,
                additional_context: if stdout_str.is_empty() {
                    None
                } else {
                    Some(stdout_str)
                },
                diagnostic: None,
            }
        }
        Some(2) => {
            let feedback = if !stderr_str.is_empty() {
                stderr_str
            } else if !stdout_str.is_empty() {
                stdout_str
            } else {
                format!("blocked by {:?} hook", rule.event)
            };
            HookOutcome {
                decision: HookDecision::Deny,
                reason: Some(feedback),
                additional_context: None,
                diagnostic: Some(format!(
                    "hook for {:?} blocked action with exit code 2",
                    rule.event
                )),
            }
        }
        other_code => HookOutcome {
            decision: HookDecision::Allow,
            reason: None,
            additional_context: None,
            diagnostic: Some(format!(
                "hook for {:?} exited with non-blocking status {:?}",
                rule.event, other_code
            )),
        },
    }
}

/// Runs all matching rules for `event` and `target` in order, stopping early if a hook denies execution.
pub async fn run_matching_hooks(
    rules: &[HookRule],
    event: HookEvent,
    target: Option<&str>,
    payload: &Value,
) -> HookOutcome {
    let mut combined_context: Vec<String> = Vec::new();
    let mut last_diagnostic: Option<String> = None;

    for rule in rules {
        if !rule.matches(event, target) {
            continue;
        }
        let outcome = run_hook(rule, payload).await;
        if let Some(ctx) = outcome.additional_context {
            if !ctx.is_empty() {
                combined_context.push(ctx);
            }
        }
        if outcome.diagnostic.is_some() {
            last_diagnostic = outcome.diagnostic.clone();
        }
        if outcome.decision == HookDecision::Deny {
            return HookOutcome {
                decision: HookDecision::Deny,
                reason: outcome.reason,
                additional_context: if combined_context.is_empty() {
                    None
                } else {
                    Some(combined_context.join("\n"))
                },
                diagnostic: outcome.diagnostic,
            };
        }
    }

    HookOutcome {
        decision: HookDecision::Allow,
        reason: None,
        additional_context: if combined_context.is_empty() {
            None
        } else {
            Some(combined_context.join("\n"))
        },
        diagnostic: last_diagnostic,
    }
}
