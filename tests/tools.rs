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
