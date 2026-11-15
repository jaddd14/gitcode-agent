//! Conservative permission decisions independent of tool execution.

use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
};

/// Unified interactive permission mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[repr(u8)]
pub enum Mode {
    /// Ask before mutations and commands.
    #[default]
    Default = 0,
    /// Automatically approve workspace edits.
    AcceptEdits = 1,
    /// Allow exploration only.
    Plan = 2,
    /// Explicitly skip permission prompts.
    Bypass = 3,
}

impl Mode {
    fn from_stored(value: u8) -> Self {
        match value {
            1 => Self::AcceptEdits,
            2 => Self::Plan,
            3 => Self::Bypass,
            _ => Self::Default,
        }
    }
}

/// Permission engine outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// May execute immediately.
    Allow,
    /// Needs an explicit operator decision.
    Ask,
    /// Cannot execute under current policy.
    Deny,
}

/// Pattern matching one tool and its resource, e.g. Read(src/**).
#[derive(Debug, Clone)]
pub struct Rule {
    /// Tool name, or wildcard.
    pub tool: String,
    /// Glob pattern for the resource.
    pub pattern: String,
}

impl Rule {
    /// Parse a tool-pattern rule, rejecting ambiguous syntax.
    pub fn parse(text: &str) -> Result<Self, String> {
        if let Some((tool, rest)) = text.split_once('(') {
            let pattern = rest
                .strip_suffix(')')
                .ok_or("unterminated permission rule")?;
            if tool.is_empty() || pattern.contains(['(', ')']) {
                return Err("invalid permission rule".into());
            }
            globset::Glob::new(pattern).map_err(|_| "invalid permission glob")?;
            Ok(Self {
                tool: tool.into(),
                pattern: pattern.into(),
            })
        } else if !text.is_empty() {
            Ok(Self {
                tool: text.into(),
                pattern: "**".into(),
            })
        } else {
            Err("empty permission rule".into())
        }
    }
    fn matches(&self, tool: &str, resource: &str) -> bool {
        (self.tool == tool || self.tool == "*")
            && globset::GlobBuilder::new(&self.pattern)
                .case_insensitive(
                    cfg!(windows)
                        && matches!(
                            tool,
                            "Read"
                                | "Write"
                                | "Edit"
                                | "MultiEdit"
                                | "NotebookEdit"
                                | "LS"
                                | "apply_patch"
                        ),
                )
                .backslash_escape(!cfg!(windows) || tool == "Bash")
                .build()
                .is_ok_and(|g| g.compile_matcher().is_match(resource))
    }
}

/// Workspace boundary and ordered deny/ask/allow rule lists.
#[derive(Debug, Clone)]
pub struct Engine {
    /// Canonical workspace root.
    pub root: PathBuf,
    /// Additional explicitly authorized workspace directories.
    pub additional_roots: Vec<PathBuf>,
    /// Explicit deny rules take precedence outside bypass mode.
    pub deny: Vec<Rule>,
    /// Explicit ask rules precede allow rules.
    pub ask: Vec<Rule>,
    /// Explicit allow rules.
    pub allow: Vec<Rule>,
    /// Active permission mode, shared so a mid-turn switch takes effect immediately.
    mode: Arc<AtomicU8>,
}

impl Engine {
    /// Active permission mode.
    pub fn mode(&self) -> Mode {
        Mode::from_stored(self.mode.load(Ordering::Relaxed))
    }

    /// Switch the active permission mode; visible to every clone sharing this engine.
    pub fn set_mode(&self, mode: Mode) {
        self.mode.store(mode as u8, Ordering::Relaxed);
    }

    /// Detached handle that can change the mode while the agent owns `Tools` mutably.
    pub fn mode_handle(&self) -> ModeHandle {
        ModeHandle(self.mode.clone())
    }
}

/// A permission-mode switch usable during an active turn.
#[derive(Debug, Clone)]
pub struct ModeHandle(Arc<AtomicU8>);

impl ModeHandle {
    /// Current mode.
    pub fn mode(&self) -> Mode {
        Mode::from_stored(self.0.load(Ordering::Relaxed))
    }

    /// Apply a mode change immediately; running tool calls observe it before deciding.
    pub fn set_mode(&self, mode: Mode) {
        self.0.store(mode as u8, Ordering::Relaxed);
    }
}

impl Engine {
    /// Initialize a workspace boundary using an existing directory.
    pub fn new(root: &Path, mode: Mode) -> Result<Self, String> {
        Ok(Self {
            mode: Arc::new(AtomicU8::new(mode as u8)),
            root: root
                .canonicalize()
                .map_err(|_| "cannot resolve workspace")?,
            additional_roots: vec![],
            deny: vec![],
            ask: vec![],
            allow: vec![],
        })
    }

    /// Add a directory explicitly authorized by the operator.
    pub fn add_workspace(&mut self, path: &Path) -> Result<PathBuf, String> {
        let requested = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        let resolved = requested
            .canonicalize()
            .map_err(|_| "cannot resolve additional directory")?;
        if !resolved.is_dir() {
            return Err("additional path must be a directory".into());
        }
        if !self.additional_roots.contains(&resolved) && resolved != self.root {
            self.additional_roots.push(resolved.clone());
        }
        Ok(resolved)
    }

    pub(crate) fn workspace_for(&self, path: &Path) -> Option<&Path> {
        std::iter::once(self.root.as_path())
            .chain(self.additional_roots.iter().map(PathBuf::as_path))
            .find(|root| crate::platform::path_within(path, root))
    }

    /// Decide using rule precedence and a read-only plan boundary.
    pub fn decide(&self, tool: &str, resource: &str, mutating: bool) -> Decision {
        let mode = self.mode();
        if mode == Mode::Bypass {
            return Decision::Allow;
        }
        if matches!(
            tool,
            "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "apply_patch"
        ) && sensitive_path(resource)
        {
            return Decision::Deny;
        }
        if self.deny.iter().any(|r| r.matches(tool, resource)) {
            return Decision::Deny;
        }
        if mode == Mode::Plan && mutating {
            return Decision::Deny;
        }
        if self.ask.iter().any(|r| r.matches(tool, resource)) {
            return Decision::Ask;
        }
        if self.allow.iter().any(|r| r.matches(tool, resource)) {
            return Decision::Allow;
        }
        if !mutating
            || mode == Mode::AcceptEdits
                && matches!(
                    tool,
                    "Write" | "Edit" | "MultiEdit" | "apply_patch" | "NotebookEdit"
                )
        {
            Decision::Allow
        } else {
            Decision::Ask
        }
    }

    pub(crate) fn explicitly_asks(&self, tool: &str, resource: &str) -> bool {
        self.ask.iter().any(|rule| rule.matches(tool, resource))
    }

    /// Resolve an existing resource and reject symlink escapes from workspace.
    pub fn existing(&self, path: &Path) -> Result<PathBuf, String> {
        #[cfg(windows)]
        crate::platform::validate_windows_path(path)?;
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            self.root.join(path)
        };
        let canonical = path
            .canonicalize()
            .map_err(|_| "cannot resolve tool path")?;
        if self.workspace_for(&canonical).is_none() {
            return Err("path is outside the workspace boundary".into());
        }
        Ok(canonical)
    }
}

// Native tools do not silently expose common credential files, including through
// absolute paths and resolved symlinks. Shell processes remain outside this policy.
fn sensitive_path(resource: &str) -> bool {
    let normalized = resource.replace('\\', "/");
    normalized.split('/').any(|component| {
        let name = if cfg!(windows) {
            component.to_ascii_lowercase()
        } else {
            component.to_owned()
        };
        name == ".env"
            || name.starts_with(".env.")
            || name == ".ssh"
            || name == ".aws"
            || name == "credentials.toml"
            || name.ends_with(".pem")
            || name.ends_with(".key")
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn deny_and_plan_override_allow_but_bypass_overrides_deny() {
        let mut e = Engine::new(Path::new("."), Mode::Plan).unwrap();
        e.allow.push(Rule::parse("Edit(**)").unwrap());
        assert_eq!(e.decide("Edit", "src/a.rs", true), Decision::Deny);
        e.set_mode(Mode::Bypass);
        e.deny.push(Rule::parse("Read(**/secret)").unwrap());
        assert_eq!(e.decide("Read", "a/secret", false), Decision::Allow);
    }
    #[test]
    fn mode_switch_reaches_existing_clones_mid_turn() {
        let engine = Engine::new(Path::new("."), Mode::Default).unwrap();
        let handle = engine.mode_handle();
        let clone = engine.clone();
        assert_eq!(clone.decide("Write", "src/a", true), Decision::Ask);
        // The TUI holds only this handle while a turn borrows the agent.
        handle.set_mode(Mode::Bypass);
        assert_eq!(clone.decide("Write", "src/a", true), Decision::Allow);
    }
    #[test]
    fn default_asks_before_mutations() {
        let e = Engine::new(Path::new("."), Mode::Default).unwrap();
        assert_eq!(e.decide("Read", "src/a", false), Decision::Allow);
        assert_eq!(e.decide("Write", "src/a", true), Decision::Ask);
        assert!(Rule::parse("Read(a").is_err());
    }
}
