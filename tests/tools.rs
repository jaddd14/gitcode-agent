//! Native workspace searches and path confinement use isolated fixtures.
#![allow(clippy::unwrap_used)]

use gitcode::{
    perm::{Engine, Mode, Rule},
    tools::Tools,
};
use serde_json::json;

#[tokio::test]
async fn read_search_and_deny_rules() {
    let root = std::env::temp_dir().join(format!("gitcode-tools-{}", std::process::id()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "fn main() {}\nneedle\n").unwrap();
    let mut tools = Tools::new(Engine::new(&root, Mode::Default).unwrap());
    assert!(Tools::definitions()
        .iter()
        .any(|tool| tool.name == "TodoWrite"));
    assert!(Tools::definitions().iter().any(|tool| tool.name == "Skill"));
    assert!(Tools::definitions()
        .iter()
        .any(|tool| tool.name == "McpCall"));
    assert!(tools
        .execute(
            "McpCall",
            &json!({"server":"fixture","tool":"echo","arguments":{}})
        )
        .await
        .unwrap_err()
        .contains("approval"));
    std::fs::create_dir_all(root.join(".gitcode/skills/fixture")).unwrap();
    std::fs::write(
        root.join(".gitcode/skills/fixture/SKILL.md"),
        "---\nname: fixture\ndescription: Test fixture\n---\nVerify workspace facts with Read.",
    )
    .unwrap();
    assert!(tools
        .execute("Skill", &json!({"name":"fixture"}))
        .await
        .unwrap()
        .contains("Verify workspace facts"));
    assert!(tools.execute("TodoWrite", &json!({"todos":[{"content":"Verify workspace","status":"in_progress","activeForm":"Verifying workspace"}]})).await.is_ok());
    assert!(tools
        .execute("Read", &json!({"path":"src/a.rs","offset":2,"limit":1}))
        .await
        .unwrap()
        .starts_with("2\tneedle"));
    assert_eq!(
        tools
            .execute("Glob", &json!({"pattern":"src/*.rs"}))
            .await
            .unwrap(),
        "src/a.rs"
    );
    assert!(tools
        .execute("Grep", &json!({"pattern":"NEEDLE","ignore_case":true}))
        .await
        .unwrap()
        .contains("src/a.rs:2:needle"));
    tools
        .permissions
        .deny
        .push(Rule::parse("Read(**)").unwrap());
    assert!(tools
        .execute("Read", &json!({"path":"src/a.rs"}))
        .await
        .is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc", root.join("escape")).unwrap();
        assert!(tools
            .permissions
            .existing(&root.join("escape/passwd"))
            .is_err());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn sensitive_files_are_not_exposed_by_native_reads_or_search() {
    let root = tempfile::tempdir().unwrap();
    for path in [".env", ".env.production", "private.key", "credentials.toml"] {
        std::fs::write(root.path().join(path), "fixture-secret-marker").unwrap();
    }
    std::fs::write(root.path().join("visible.txt"), "ordinary-marker").unwrap();
    let mut tools = Tools::new(Engine::new(root.path(), Mode::Default).unwrap());
    for path in [".env", ".env.production", "private.key", "credentials.toml"] {
        assert!(tools.execute("Read", &json!({"path": path})).await.is_err());
    }
    assert!(!tools
        .execute("Grep", &json!({"pattern":"fixture-secret-marker"}))
        .await
        .unwrap()
        .contains("fixture-secret-marker"));
    assert!(tools
        .execute("Read", &json!({"path":"visible.txt"}))
        .await
        .is_ok());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(".env", root.path().join("alias.txt")).unwrap();
        assert!(tools
            .execute("Read", &json!({"path":"alias.txt"}))
            .await
            .is_err());
    }
    tools.permissions.mode = Mode::Bypass;
    assert!(tools
        .execute("Read", &json!({"path":".env"}))
        .await
        .is_err());
}

#[tokio::test]
async fn edits_are_guarded_and_connected_to_native_executor() {
    let root = std::env::temp_dir().join(format!("gitcode-tools-edit-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("file.txt"), "before\n").unwrap();
    let mut tools = Tools::new(Engine::new(&root, Mode::Default).unwrap());
    let edit = json!({"path":"file.txt","old_string":"before","new_string":"after"});
    assert!(tools
        .execute("Edit", &edit)
        .await
        .unwrap_err()
        .contains("approval"));
    tools.permissions.mode = Mode::AcceptEdits;
    assert!(tools.execute("Edit", &edit).await.is_err());
    tools
        .execute("Read", &json!({"path":"file.txt"}))
        .await
        .unwrap();
    tools.execute("Edit", &edit).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("file.txt")).unwrap(),
        "after\n"
    );
    std::fs::write(root.join("file.txt"), "external\n").unwrap();
    assert!(tools
        .execute("Write", &json!({"path":"file.txt","content":"overwrite"}))
        .await
        .is_err());
    assert_eq!(
        std::fs::read_to_string(root.join("file.txt")).unwrap(),
        "external\n"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn read_does_not_silently_cut_long_source_lines() {
    let root = std::env::temp_dir().join(format!("gitcode-tools-long-line-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("long.txt"),
        format!("{}END-OF-LINE\n", "x".repeat(4000)),
    )
    .unwrap();
    let mut tools = Tools::new(Engine::new(&root, Mode::Default).unwrap());
    let output = tools
        .execute("Read", &json!({"path":"long.txt"}))
        .await
        .unwrap();
    assert!(output.contains("END-OF-LINE"));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn approved_edit_never_overrides_explicit_deny_or_plan_mode() {
    use gitcode::perm::Rule;
    let root = std::env::temp_dir().join(format!("gitcode-edit-deny-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut engine = Engine::new(&root, Mode::Default).unwrap();
    engine.deny.push(Rule::parse("Write(blocked.txt)").unwrap());
    let mut tools = Tools::new(engine);
    let input = json!({"path":"blocked.txt","content":"bad"});
    assert!(tools.execute_approved_tool("Write", &input).await.is_err());
    assert!(!root.join("blocked.txt").exists());
    tools.permissions.deny.clear();
    tools.permissions.mode = Mode::Plan;
    assert!(tools.execute_approved_tool("Write", &input).await.is_err());
    assert!(!root.join("blocked.txt").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn explicitly_added_directory_supports_read_and_edit_without_opening_siblings() {
    let base = std::env::temp_dir().join(format!("gitcode-extra-root-{}", std::process::id()));
    let root = base.join("project");
    let extra = base.join("extra");
    let sibling = base.join("unapproved");
    for path in [&root, &extra, &sibling] {
        std::fs::create_dir_all(path).unwrap();
    }
    let target = extra.join("file.txt");
    std::fs::write(&target, "before").unwrap();
    std::fs::write(sibling.join("secret.txt"), "secret").unwrap();
    let mut engine = Engine::new(&root, Mode::AcceptEdits).unwrap();
    assert!(engine.existing(&target).is_err());
    engine.add_workspace(&extra).unwrap();
    assert!(engine.existing(&sibling.join("secret.txt")).is_err());
    let mut tools = Tools::new(engine);
    tools
        .execute("Read", &json!({"path":target}))
        .await
        .unwrap();
    tools
        .execute(
            "Edit",
            &json!({"path":target,"old_string":"before","new_string":"after"}),
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(target).unwrap(), "after");
    std::fs::remove_dir_all(base).unwrap();
}
