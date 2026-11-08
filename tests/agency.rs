//! Integration tests for Milestone M9 (`src/agent/agency.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../src/agent/agency.rs"]
mod agency;

use agency::{
    load_agents, parse_agent_markdown, PlanDecision, PlanExecutionMode, PlanState, QuestionAnswer,
    QuestionPrompt, SubagentProfile, SubagentRunState, SubagentRunner, SubagentTree, TodoItem,
    TodoList, TodoStatus, DEFAULT_SUBAGENT_CONCURRENCY, MAX_AGENT_FILE_BYTES,
};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "gitcode_agency_{prefix}_{}_{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn builtin_subagent_profiles_enforce_read_only_and_task_restrictions() {
    let general = SubagentProfile::general_purpose();
    assert_eq!(general.name, "general-purpose");
    assert!(!general.read_only);
    assert!(general.allows_tool("Read"));
    assert!(general.allows_tool("Edit"));
    assert!(general.allows_tool("Bash"));
    assert!(!general.allows_tool("Task"));

    let explore = SubagentProfile::explore();
    assert_eq!(explore.name, "explore");
    assert!(explore.read_only);
    assert!(explore.allows_tool("Read"));
    assert!(explore.allows_tool("LS"));
    assert!(explore.allows_tool("Glob"));
    assert!(explore.allows_tool("Grep"));
    assert!(!explore.allows_tool("Write"));
    assert!(!explore.allows_tool("Bash"));
    assert!(!explore.allows_tool("Task"));

    let plan = SubagentProfile::plan();
    assert_eq!(plan.name, "plan");
    assert!(plan.read_only);
    assert!(plan.allows_tool("Read"));
    assert!(plan.allows_tool("Grep"));
    assert!(!plan.allows_tool("Edit"));

    let builtins = SubagentProfile::builtins();
    assert_eq!(builtins.len(), 3);
}

#[test]
fn parse_agent_markdown_parses_frontmatter_and_rejects_invalid_inputs() {
    let md = "\
---
name: security-auditor
description: Audits Rust code for unsafe blocks and path traversal
tools: [Read, Grep, Glob]
model: glm-5.3-prime
color: \"yellow\"
---
You are a security auditor. Check every file access and report vulnerabilities.
";
    let profile = parse_agent_markdown(md).unwrap();
    assert_eq!(profile.name, "security-auditor");
    assert_eq!(
        profile.description,
        "Audits Rust code for unsafe blocks and path traversal"
    );
    assert_eq!(profile.tools, vec!["Read", "Grep", "Glob"]);
    assert_eq!(profile.model.as_deref(), Some("glm-5.3-prime"));
    assert_eq!(profile.color.as_deref(), Some("yellow"));
    assert!(profile.read_only);
    assert!(profile.system_prompt.contains("security auditor"));

    assert!(parse_agent_markdown("no frontmatter").is_err());
    assert!(parse_agent_markdown("---\nname: foo\n---\nbody").is_err());
    assert!(parse_agent_markdown("---\nname: foo\ndescription: bar\n---\n   ").is_err());
    assert!(parse_agent_markdown("---\nname: ../bad\ndescription: bar\n---\nbody").is_err());
}

#[test]
fn load_agents_merges_home_and_project_and_enforces_security_limits() {
    let base = unique_temp_dir("load");
    let home = base.join("home");
    let project = base.join("project");
    let home_agents = home.join(".gitcode").join("agents");
    let proj_agents = project.join(".gitcode").join("agents");
    fs::create_dir_all(&home_agents).unwrap();
    fs::create_dir_all(&proj_agents).unwrap();

    fs::write(
        home_agents.join("reviewer.md"),
        "---\nname: reviewer\ndescription: Home reviewer\ntools: Read, Grep\n---\nHome prompt",
    )
    .unwrap();
    fs::write(
        proj_agents.join("reviewer.md"),
        "---\nname: reviewer\ndescription: Project reviewer\ntools: Read, Grep, Bash\n---\nProject prompt",
    )
    .unwrap();

    let agents = load_agents(&home, &project).unwrap();
    let reviewer = agents.iter().find(|a| a.name == "reviewer").unwrap();
    assert_eq!(reviewer.description, "Project reviewer");
    assert!(!reviewer.read_only);
    assert_eq!(reviewer.system_prompt, "Project prompt");

    let big_body = "a".repeat((MAX_AGENT_FILE_BYTES as usize) + 100);
    let oversized = format!("---\nname: big\ndescription: oversized\n---\n{big_body}");
    fs::write(proj_agents.join("big.md"), oversized).unwrap();
    assert!(load_agents(&home, &project).is_err());
    fs::remove_file(proj_agents.join("big.md")).unwrap();

    #[cfg(unix)]
    {
        let outside = base.join("outside.md");
        fs::write(
            &outside,
            "---\nname: sneaky\ndescription: symlinked\n---\nprompt",
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, proj_agents.join("sneaky.md")).unwrap();
        assert!(load_agents(&home, &project).is_err());
    }

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn todo_list_supports_todowrite_and_codex_update_plan_and_renders() {
    let mut list = TodoList::new();
    assert_eq!(list.render(), vec!["No todos."]);

    let todo_write_payload = json!({
        "todos": [
            { "id": "t1", "content": "Inspect agency requirements", "status": "completed" },
            { "id": "t2", "content": "Implement TodoList", "status": "in_progress" },
            { "id": "t3", "content": "Run integration tests", "status": "pending" }
        ]
    });
    let summary = list.update_from_value(&todo_write_payload).unwrap();
    assert_eq!(summary, "Updated todo list (1/3 completed)");
    assert_eq!(
        list.items,
        vec![
            TodoItem {
                id: "t1".into(),
                content: "Inspect agency requirements".into(),
                status: TodoStatus::Completed
            },
            TodoItem {
                id: "t2".into(),
                content: "Implement TodoList".into(),
                status: TodoStatus::InProgress
            },
            TodoItem {
                id: "t3".into(),
                content: "Run integration tests".into(),
                status: TodoStatus::Pending
            }
        ]
    );

    let codex_plan_payload = json!({
        "explanation": "Switching to verification phase",
        "plan": [
            { "step": "Implement TodoList", "status": "completed" },
            { "step": "Verify clippy clean", "status": "in_progress" }
        ]
    });
    let summary2 = list.update_from_value(&codex_plan_payload).unwrap();
    assert_eq!(summary2, "Updated todo list (1/2 completed)");
    let rendered = list.render();
    assert_eq!(rendered[0], "Note: Switching to verification phase");
    assert_eq!(rendered[1], "[x] Implement TodoList (1)");
    assert_eq!(rendered[2], "[*] Verify clippy clean (2)");

    let two_in_progress = json!({
        "todos": [
            { "id": "1", "content": "A", "status": "in_progress" },
            { "id": "2", "content": "B", "status": "in_progress" }
        ]
    });
    assert!(list.update_from_value(&two_in_progress).is_err());
}

#[test]
fn plan_state_tracks_exit_plan_mode_and_approval_transitions() {
    let mut state = PlanState::new();
    assert!(state.is_planning());
    assert!(state.apply_decision(PlanDecision::ApproveAuto).is_err());

    let submitted = state
        .submit_exit_plan_mode(&json!({ "plan": "1. Add agency.rs\n2. Add tests/agency.rs" }))
        .unwrap();
    assert!(submitted.contains("Add agency.rs"));

    let mode = state
        .apply_decision(PlanDecision::KeepPlanning(
            "Include semaphore concurrency test".into(),
        ))
        .unwrap();
    assert_eq!(mode, PlanExecutionMode::Planning);
    assert_eq!(
        state.feedback.as_deref(),
        Some("Include semaphore concurrency test")
    );

    let mode = state.apply_decision(PlanDecision::ApproveManual).unwrap();
    assert_eq!(mode, PlanExecutionMode::ApprovedManual);
    assert!(!state.is_planning());

    let mode = state.apply_decision(PlanDecision::ApproveAuto).unwrap();
    assert_eq!(mode, PlanExecutionMode::ApprovedAuto);
}

#[test]
fn question_prompt_validates_bounds_formats_and_resolves_answers() {
    let valid = json!({
        "question": "Which concurrency limit should we use?",
        "options": [
            { "label": "4 workers (Recommended)", "description": "Balanced default cap" },
            { "label": "2 workers", "description": "Lower memory footprint" }
        ]
    });
    let prompt = QuestionPrompt::from_value(&valid).unwrap();
    let lines = prompt.format_lines();
    assert_eq!(lines[0], "? Which concurrency limit should we use?");
    assert!(lines[1].contains("[1] 4 workers (Recommended) - Balanced default cap"));
    assert!(lines[3].contains("[3] Other"));

    assert_eq!(
        prompt.resolve_input(0, "1").unwrap(),
        QuestionAnswer::Selected {
            index: 0,
            label: "4 workers (Recommended)".into()
        }
    );
    assert_eq!(
        prompt.resolve_input(0, "8 workers custom").unwrap(),
        QuestionAnswer::Custom("8 workers custom".into())
    );
    assert!(prompt.resolve_input(0, "5").is_err());
    assert!(prompt.resolve_input(0, "   ").is_err());

    let too_many_options = json!({
        "question": "Pick one",
        "options": [
            { "label": "1", "description": "a" },
            { "label": "2", "description": "b" },
            { "label": "3", "description": "c" },
            { "label": "4", "description": "d" },
            { "label": "5", "description": "e" }
        ]
    });
    assert!(QuestionPrompt::from_value(&too_many_options).is_err());
}

#[test]
fn subagent_tree_tracks_live_telemetry_and_renders_within_width() {
    let mut tree = SubagentTree::new();
    assert!(tree.render_tree(80).is_empty());

    tree.start_run("r1", "explore", "Find auth middleware");
    tree.start_run("r2", "plan", "Design session refresh flow");
    assert!(tree.update_run("r1", Some("Grep".into()), 1420, 850));
    assert!(tree.finish_run("r2", true, 3200, 2400));

    assert_eq!(tree.runs[0].state, SubagentRunState::Running);
    assert_eq!(tree.runs[1].state, SubagentRunState::Completed);

    let full_lines = tree.render_tree(100);
    assert_eq!(full_lines.len(), 3);
    assert_eq!(full_lines[0], "Subagents (1/2 active)");
    assert!(full_lines[1].starts_with("├─ * explore (Find auth middleware) [Grep]"));
    assert!(full_lines[2].starts_with("└─ ✓ plan (Design session refresh flow)"));

    let narrow = tree.render_tree(28);
    for line in narrow {
        assert!(unicode_width::UnicodeWidthStr::width(line.as_str()) <= 28);
    }
}

#[tokio::test]
async fn subagent_runner_caps_concurrency_and_blocks_nested_spawns() {
    let default_runner = SubagentRunner::default();
    assert_eq!(
        default_runner.max_concurrency(),
        DEFAULT_SUBAGENT_CONCURRENCY
    );
    assert_eq!(default_runner.max_depth(), 1);

    let runner = SubagentRunner::new(2);
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let tasks: Vec<usize> = (0..6).collect();
    let active_clone = Arc::clone(&active);
    let peak_clone = Arc::clone(&peak);

    let results = runner
        .run_parallel(0, tasks, move |task_id, child_depth| {
            let active = Arc::clone(&active_clone);
            let peak = Arc::clone(&peak_clone);
            async move {
                assert_eq!(child_depth, 1);
                let cur = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(cur, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(25)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(format!("done-{task_id}"))
            }
        })
        .await
        .unwrap();

    assert_eq!(results.len(), 6);
    assert!(peak.load(Ordering::SeqCst) <= 2);

    let nested_attempt = runner
        .run_parallel(1, vec![1], |_id, _depth| async { Ok("unreachable".into()) })
        .await;
    assert!(nested_attempt.is_err());

    let deeper = SubagentRunner::new(2).with_max_depth(2);
    assert_eq!(deeper.max_depth(), 2);
    assert_eq!(deeper.check_depth(1), Ok(2));
}
