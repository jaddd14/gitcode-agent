//! Conservative permission decisions independent of tool execution.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Unified interactive permission mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// Ask before mutations and commands.
    #[default]
    Default,
    /// Automatically approve workspace edits.
    AcceptEdits,
    /// Allow exploration only.
    Plan,
    /// Explicitly skip permission prompts.
    Bypass,
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
    /// Active permission mode.
    pub mode: Mode,
    /// Canonical workspace root.
    pub root: PathBuf,
    /// Explicit deny rules take highest precedence.
    pub deny: Vec<Rule>,
    /// Explicit ask rules precede allow rules.
    pub ask: Vec<Rule>,
    /// Explicit allow rules.
    pub allow: Vec<Rule>,
}

impl Engine {
    /// Initialize a workspace boundary using an existing directory.
    pub fn new(root: &Path, mode: Mode) -> Result<Self, String> {
        Ok(Self {
            mode,
            root: root
                .canonicalize()
                .map_err(|_| "cannot resolve workspace")?,
            deny: vec![],
            ask: vec![],
            allow: vec![],
        })
    }

    /// Decide using rule precedence and a read-only plan boundary.
    pub fn decide(&self, tool: &str, resource: &str, mutating: bool) -> Decision {
        if self.deny.iter().any(|r| r.matches(tool, resource)) {
            return Decision::Deny;
        }
        if self.mode == Mode::Plan && mutating {
            return Decision::Deny;
        }
        if self.mode == Mode::Bypass {
            return Decision::Allow;
        }
        if self.ask.iter().any(|r| r.matches(tool, resource)) {
            return Decision::Ask;
        }
        if self.allow.iter().any(|r| r.matches(tool, resource)) {
            return Decision::Allow;
        }
        if !mutating
            || self.mode == Mode::AcceptEdits
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
        if !crate::platform::path_within(&canonical, &self.root) {
            return Err("path is outside the workspace boundary".into());
        }
        Ok(canonical)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn deny_and_plan_override_allow() {
        let mut e = Engine::new(Path::new("."), Mode::Plan).unwrap();
        e.allow.push(Rule::parse("Edit(**)").unwrap());
        assert_eq!(e.decide("Edit", "src/a.rs", true), Decision::Deny);
        e.mode = Mode::Bypass;
        e.deny.push(Rule::parse("Read(**/secret)").unwrap());
        assert_eq!(e.decide("Read", "a/secret", false), Decision::Deny);
    }
    #[test]
    fn default_asks_before_mutations() {
        let e = Engine::new(Path::new("."), Mode::Default).unwrap();
        assert_eq!(e.decide("Read", "src/a", false), Decision::Allow);
        assert_eq!(e.decide("Write", "src/a", true), Decision::Ask);
        assert!(Rule::parse("Read(a").is_err());
    }
}
