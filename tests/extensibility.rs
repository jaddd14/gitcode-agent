//! Integration tests for Milestone M10 (Extensibility: slash command registry, custom commands, skills, hooks).
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../src/extensibility.rs"]
mod extensibility;

#[cfg(windows)]
mod platform {
    pub use gitcode::platform::*;
}

use extensibility::{
    complete_slash, complete_slash_with_custom, discover_skills, expand_command_template,
    format_skill_index, load_custom_commands, load_skill_body, run_hook, run_matching_hooks,
    ExtensionSource, HookDecision, HookEvent, HookRule, BUILTIN_COMMANDS,
};
use serde_json::json;
use std::collections::HashSet;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(windows)]
use std::os::windows::fs::symlink_file as symlink;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

struct TempWorkspace {
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
}

impl TempWorkspace {
    fn new(label: &str) -> Self {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let root = std::env::temp_dir().join(format!("gitcode_m10_{label}_{pid}_{id}"));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let project = root.join("project");
        fs::create_dir_all(&home).expect("create home dir");
        fs::create_dir_all(&project).expect("create project dir");
        Self {
            root,
            home,
            project,
        }
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn builtin_registry_has_all_33_required_commands() {
    let expected = [
        "help",
        "clear",
        "compact",
        "config",
        "model",
        "provider",
        "usage",
        "cost",
        "status",
        "context",
        "resume",
        "rewind",
        "plan",
        "permissions",
        "agents",
        "mcp",
        "init",
        "review",
        "diff",
        "commit",
        "memory",
        "doctor",
        "theme",
        "vim",
        "export",
        "copy",
        "hooks",
        "skills",
        "add-dir",
        "login",
        "logout",
        "terminal-setup",
        "exit",
    ];

    assert_eq!(BUILTIN_COMMANDS.len(), 33);
    let actual: HashSet<&str> = BUILTIN_COMMANDS.iter().map(|c| c.name).collect();
    for cmd in expected {
        assert!(actual.contains(cmd), "missing built-in command: /{cmd}");
    }
    for spec in BUILTIN_COMMANDS {
        assert!(
            !spec.description.is_empty(),
            "empty description for {}",
            spec.name
        );
    }
}

#[test]
fn slash_completion_supports_prefix_and_fuzzy_matching() {
    let all = complete_slash("");
    assert_eq!(all.len(), 33);

    let co_matches = complete_slash("/co");
    let names: Vec<&str> = co_matches.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"compact"));
    assert!(names.contains(&"config"));
    assert!(names.contains(&"commit"));
    assert!(names.contains(&"context"));
    assert!(names.contains(&"cost"));
    assert!(names.contains(&"copy"));

    let exact = complete_slash("/model");
    assert_eq!(exact.first().map(|c| c.name.as_str()), Some("model"));

    // Fuzzy subsequence match for "trms" -> "terminal-setup"
    let fuzzy = complete_slash("trms");
    assert!(fuzzy.iter().any(|c| c.name == "terminal-setup"));
}

#[test]
fn custom_commands_loader_parses_namespaces_frontmatter_and_expansions() {
    let ws = TempWorkspace::new("custom_cmds");

    let user_cmd_dir = ws.home.join(".gitcode/commands");
    let proj_cmd_dir = ws.project.join(".gitcode/commands/git");
    fs::create_dir_all(&user_cmd_dir).unwrap();
    fs::create_dir_all(&proj_cmd_dir).unwrap();

    fs::write(
        user_cmd_dir.join("greet.md"),
        "---\ndescription: Greet someone\nargument-hint: <name>\nmodel: glm-5\n---\nHello $1 ($ARGUMENTS)!",
    )
    .unwrap();

    fs::write(
        proj_cmd_dir.join("sync.md"),
        "---\ndescription: Sync branch with remote\nargument-hint: <remote> <branch>\nallowed-tools: [bash, git_status]\nmodel: kimi-k2\n---\nSyncing $1 into $2 using @docs/guide.txt and all args: $ARGUMENTS",
    )
    .unwrap();

    fs::create_dir_all(ws.project.join("docs")).unwrap();
    fs::write(ws.project.join("docs/guide.txt"), "MAIN_BRANCH_POLICY").unwrap();

    let cmds = load_custom_commands(&ws.home, &ws.project).unwrap();
    assert_eq!(cmds.len(), 2);

    let greet = cmds.iter().find(|c| c.name == "greet").unwrap();
    assert_eq!(greet.source, ExtensionSource::User);
    assert_eq!(greet.description, "Greet someone");
    assert_eq!(greet.argument_hint.as_deref(), Some("<name>"));
    assert_eq!(greet.model.as_deref(), Some("glm-5"));
    let rendered_greet = greet.render("Alice Wonderland", &ws.project).unwrap();
    assert_eq!(rendered_greet, "Hello Alice (Alice Wonderland)!");

    let sync = cmds.iter().find(|c| c.name == "git:sync").unwrap();
    assert_eq!(sync.source, ExtensionSource::Project);
    assert_eq!(sync.allowed_tools, vec!["bash", "git_status"]);
    let rendered_sync = sync.render("origin main", &ws.project).unwrap();
    assert_eq!(
        rendered_sync,
        "Syncing origin into main using MAIN_BRANCH_POLICY and all args: origin main"
    );

    let completed = complete_slash_with_custom("/git:", &cmds);
    assert_eq!(completed.first().map(|c| c.name.as_str()), Some("git:sync"));
}

#[test]
fn custom_command_file_inclusion_rejects_escape_and_symlinks() {
    let ws = TempWorkspace::new("inclusion_security");
    let secret_outside = ws.root.join("secret.txt");
    fs::write(&secret_outside, "TOP_SECRET").unwrap();

    // Reject .. escape
    let err_parent = expand_command_template("Read @../secret.txt", "", &ws.project);
    assert!(err_parent.is_err());

    // Reject symlink inside project pointing outside
    let link_path = ws.project.join("leak.txt");
    symlink(&secret_outside, &link_path).unwrap();
    let err_symlink = expand_command_template("Read @leak.txt", "", &ws.project);
    assert!(err_symlink.is_err());
}

#[test]
fn skills_discovery_and_progressive_disclosure_work_with_fallbacks() {
    let ws = TempWorkspace::new("skills");

    let codex_skill = ws.home.join(".codex/skills/legacy-lint");
    let claude_skill = ws.home.join(".claude/skills/audit");
    let user_skill = ws.home.join(".gitcode/skills/audit");
    let proj_skill = ws.project.join(".gitcode/skills/deploy");

    fs::create_dir_all(&codex_skill).unwrap();
    fs::create_dir_all(&claude_skill).unwrap();
    fs::create_dir_all(&user_skill).unwrap();
    fs::create_dir_all(&proj_skill).unwrap();

    fs::write(
        codex_skill.join("SKILL.md"),
        "---\nname: legacy-lint\ndescription: Run legacy linter rules\n---\nDetailed codex skill instructions.",
    )
    .unwrap();

    fs::write(
        claude_skill.join("SKILL.md"),
        "---\nname: audit\ndescription: Fallback claude audit\n---\nClaude body.",
    )
    .unwrap();

    // User .gitcode skill overrides ~/.claude fallback with the same name
    fs::write(
        user_skill.join("SKILL.md"),
        "---\nname: audit\ndescription: User gitcode security audit\n---\nFull user audit steps.",
    )
    .unwrap();

    fs::write(
        proj_skill.join("SKILL.md"),
        "---\nname: deploy\ndescription: Deploy service to production\n---\nStep 1: Build\nStep 2: Ship",
    )
    .unwrap();

    let skills = discover_skills(&ws.home, &ws.project).unwrap();
    assert_eq!(skills.len(), 3);

    let lint = skills.iter().find(|s| s.name == "legacy-lint").unwrap();
    assert!(lint.read_only);
    assert_eq!(lint.source, ExtensionSource::Fallback);

    let audit = skills.iter().find(|s| s.name == "audit").unwrap();
    assert!(!audit.read_only);
    assert_eq!(audit.source, ExtensionSource::User);
    assert_eq!(audit.description, "User gitcode security audit");

    let deploy = skills.iter().find(|s| s.name == "deploy").unwrap();
    assert_eq!(deploy.source, ExtensionSource::Project);

    let index = format_skill_index(&skills);
    assert!(index.contains("deploy: Deploy service to production"));
    assert!(!index.contains("Step 1: Build"));

    let body = load_skill_body(deploy).unwrap();
    assert_eq!(body, "Step 1: Build\nStep 2: Ship");
}

#[tokio::test]
async fn hooks_engine_handles_exit_0_json_exit_2_timeout_and_redacts_command() {
    // 1. Exit 0 with structured JSON decision + additionalContext
    let rule_json = HookRule {
        event: HookEvent::PreToolUse,
        matcher: Some("bash".to_string()),
        command: r#"cat >/dev/null && printf '{"decision":"deny","reason":"forbidden flag","additionalContext":"use safe wrapper"}'"#.to_string(),
        timeout_ms: 2000,
    };
    let out_json = run_hook(&rule_json, &json!({"tool": "bash"})).await;
    assert_eq!(out_json.decision, HookDecision::Deny);
    assert_eq!(out_json.reason.as_deref(), Some("forbidden flag"));
    assert_eq!(
        out_json.additional_context.as_deref(),
        Some("use safe wrapper")
    );

    // 2. Exit 2 blocks and feeds stderr back to the model
    let secret_marker = "SUPER_SECRET_HOOK_COMMAND_TOKEN_991";
    let rule_exit2 = HookRule {
        event: HookEvent::UserPromptSubmit,
        matcher: None,
        command: format!("echo 'Policy violation: secrets detected' >&2; exit 2 # {secret_marker}"),
        timeout_ms: 2000,
    };
    let out_exit2 = run_hook(&rule_exit2, &json!({"prompt": "hi"})).await;
    assert_eq!(out_exit2.decision, HookDecision::Deny);
    assert_eq!(
        out_exit2.reason.as_deref(),
        Some("Policy violation: secrets detected")
    );
    if let Some(diag) = &out_exit2.diagnostic {
        assert!(!diag.contains(secret_marker));
    }

    // 3. Timeout enforcement and command never leaks into diagnostic logs
    let rule_timeout = HookRule {
        event: HookEvent::SessionStart,
        matcher: None,
        command: format!("sleep 5 # {secret_marker}"),
        timeout_ms: 60,
    };
    let out_timeout = run_hook(&rule_timeout, &json!({})).await;
    assert_eq!(out_timeout.decision, HookDecision::Allow);
    let diag = out_timeout.diagnostic.expect("timeout diagnostic");
    assert!(diag.contains("timed out"));
    assert!(!diag.contains(secret_marker));

    // 4. Other non-zero exit code (e.g. exit 1) is non-blocking and never leaks command
    let rule_exit1 = HookRule {
        event: HookEvent::PostToolUse,
        matcher: Some("read_file".to_string()),
        command: format!("exit 1 # {secret_marker}"),
        timeout_ms: 2000,
    };
    let out_exit1 = run_matching_hooks(
        &[rule_exit1],
        HookEvent::PostToolUse,
        Some("read_file"),
        &json!({}),
    )
    .await;
    assert_eq!(out_exit1.decision, HookDecision::Allow);
    let diag1 = out_exit1.diagnostic.expect("exit 1 diagnostic");
    assert!(!diag1.contains(secret_marker));
}
