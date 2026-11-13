//! Editing acceptance tests; path import works before the tool dispatcher export.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use editing::{execute, ReadState};
use gitcode::perm::{Engine, Mode, Rule};
use gitcode::tools::editing;
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Workspace {
    root: PathBuf,
    permissions: Engine,
    state: ReadState,
}
impl Workspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "gitcode-edit-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let permissions = Engine::new(&root, Mode::AcceptEdits).unwrap();
        Self {
            root,
            permissions,
            state: ReadState::new(),
        }
    }
    fn seed(&mut self, name: &str, bytes: &[u8]) {
        let path = self.root.join(name);
        fs::write(&path, bytes).unwrap();
        self.state.insert(path, bytes.to_vec());
    }
    fn run(&mut self, tool: &str, input: Value) -> Result<Vec<editing::Change>, String> {
        execute(&self.permissions, &mut self.state, tool, &input)
    }
    fn read(&self, name: &str) -> Vec<u8> {
        fs::read(self.root.join(name)).unwrap()
    }
    fn patch(&mut self, patch: &str) -> Result<Vec<editing::Change>, String> {
        self.run("apply_patch", json!({"patch":patch}))
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn write_requires_read_and_compares_bytes_not_mtime_or_length() {
    let mut w = Workspace::new();
    fs::write(w.root.join("a"), "old").unwrap();
    assert!(w
        .run("Write", json!({"path":"a","content":"new"}))
        .unwrap_err()
        .contains("Read"));
    w.seed("a", b"old");
    fs::write(w.root.join("a"), "OLD").unwrap();
    assert!(w
        .run("Write", json!({"path":"a","content":"new"}))
        .unwrap_err()
        .contains("externally"));
    assert_eq!(w.read("a"), b"OLD");
    assert_eq!(w.state[&w.root.join("a")], b"old");
}

#[test]
fn write_creates_nested_file_and_refreshes_state() {
    let mut w = Workspace::new();
    let changes = w
        .run("Write", json!({"path":"nested/a","content":"hello"}))
        .unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].before, None);
    assert_eq!(w.state[&w.root.join("nested/a")], b"hello");
    w.run(
        "Edit",
        json!({"path":"nested/a","old_string":"hello","new_string":"world"}),
    )
    .unwrap();
    assert_eq!(w.read("nested/a"), b"world");
}

#[test]
fn write_preserves_newline_style_and_final_newline_intent() {
    let mut w = Workspace::new();
    w.seed("a", b"old\r\n");
    w.run("Write", json!({"file_path":"a","content":"one\ntwo\n"}))
        .unwrap();
    assert_eq!(w.read("a"), b"one\r\ntwo\r\n");
    w.run("Write", json!({"path":"a","content":"last"}))
        .unwrap();
    assert_eq!(w.read("a"), b"last");
}

#[test]
#[cfg(unix)]
fn atomic_write_preserves_mode_and_leaves_no_tempfiles() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let mut w = Workspace::new();
    w.seed("a", b"old");
    fs::set_permissions(w.root.join("a"), fs::Permissions::from_mode(0o751)).unwrap();
    let inode = fs::metadata(w.root.join("a")).unwrap().ino();
    w.run("Write", json!({"path":"a","content":"new"})).unwrap();
    let metadata = fs::metadata(w.root.join("a")).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o751);
    assert_ne!(metadata.ino(), inode);
    assert_eq!(fs::read_dir(&w.root).unwrap().count(), 1);
}

#[test]
fn edit_requires_unique_exact_nonempty_non_noop_match() {
    let mut w = Workspace::new();
    w.seed("a", b"let x = 1;\nlet x = 1;\n");
    for (old, new) in [("", "x"), ("let x = 1;", "x"), ("let x=1;", "x")] {
        assert!(w
            .run(
                "Edit",
                json!({"path":"a","old_string":old,"new_string":new})
            )
            .is_err());
    }
    let error = w
        .run(
            "Edit",
            json!({"path":"a","old_string":"let x=1;","new_string":"x"}),
        )
        .unwrap_err();
    assert!(error.contains("nearest line 1"));
    w.run(
        "Edit",
        json!({"path":"a","old_string":"let x = 1;","new_string":"let y = 2;","replace_all":true}),
    )
    .unwrap();
    assert_eq!(w.read("a"), b"let y = 2;\nlet y = 2;\n");
    assert!(w.run("Edit", json!({"path":"a","old_string":"let y = 2;","new_string":"let y = 2;","replace_all":true})).is_err());
}

#[test]
fn edit_is_whitespace_exact_but_preserves_replacement_crlf() {
    let mut w = Workspace::new();
    w.seed("a", b"one\r\ntwo\r\n");
    assert!(w
        .run(
            "Edit",
            json!({"path":"a","old_string":"one\ntwo","new_string":"x"})
        )
        .is_err());
    w.run(
        "Edit",
        json!({"path":"a","old_string":"one\r\ntwo","new_string":"new\nlines"}),
    )
    .unwrap();
    assert_eq!(w.read("a"), b"new\r\nlines\r\n");
}

#[test]
fn multiedit_is_ordered_and_atomic_on_validation_failure() {
    let mut w = Workspace::new();
    w.seed("a", b"start");
    let state = w.state.clone();
    assert!(w
        .run(
            "MultiEdit",
            json!({"path":"a","edits":[
                {"old_string":"start","new_string":"middle"},
                {"old_string":"missing","new_string":"end"}
            ]})
        )
        .is_err());
    assert_eq!(w.read("a"), b"start");
    assert_eq!(w.state, state);
    w.run(
        "MultiEdit",
        json!({"path":"a","edits":[
            {"old_string":"start","new_string":"middle"},
            {"old_string":"middle","new_string":"end"}
        ]}),
    )
    .unwrap();
    assert_eq!(w.read("a"), b"end");
}

#[test]
fn multiedit_rejects_empty_batch_and_bad_types() {
    let mut w = Workspace::new();
    w.seed("a", b"start");
    assert!(w.run("MultiEdit", json!({"path":"a","edits":[]})).is_err());
    assert!(w
        .run(
            "Edit",
            json!({"path":"a","old_string":"start","new_string":"x","replace_all":"true"})
        )
        .is_err());
}

#[test]
fn missing_after_read_is_external_deletion() {
    let mut w = Workspace::new();
    w.seed("a", b"old");
    fs::remove_file(w.root.join("a")).unwrap();
    assert!(w
        .run("Write", json!({"path":"a","content":"new"}))
        .unwrap_err()
        .contains("deleted externally"));
    assert!(!w.root.join("a").exists());
}

#[test]
fn mutation_permissions_use_real_tool_and_mutating_flag() {
    let mut w = Workspace::new();
    w.permissions.mode = Mode::Default;
    assert!(w
        .run("Write", json!({"path":"a","content":"x"}))
        .unwrap_err()
        .contains("approval"));
    w.permissions.mode = Mode::Plan;
    assert!(w
        .patch("*** Begin Patch\n*** Add File: a\n+x\n*** End Patch")
        .unwrap_err()
        .contains("denied"));
    w.permissions.mode = Mode::AcceptEdits;
    w.permissions
        .deny
        .push(Rule::parse("Write(secret)").unwrap());
    let absolute = w.root.join("secret");
    assert!(w
        .run("Write", json!({"path":absolute,"content":"x"}))
        .is_err());
}

#[test]
fn rejects_parent_traversal_and_workspace_root() {
    let mut w = Workspace::new();
    for path in ["../escape", "nested/../../escape", ".", ""] {
        assert!(w.run("Write", json!({"path":path,"content":"x"})).is_err());
    }
}

#[test]
#[cfg(unix)]
fn rejects_symlink_escape_and_symlink_file_targets() {
    use std::os::unix::fs::symlink;
    let mut w = Workspace::new();
    let outside = Workspace::new();
    symlink(&outside.root, w.root.join("escape")).unwrap();
    assert!(w
        .run("Write", json!({"path":"escape/a","content":"x"}))
        .is_err());
    w.seed("a", b"old");
    symlink(w.root.join("a"), w.root.join("link")).unwrap();
    assert!(w
        .run("Write", json!({"path":"link","content":"x"}))
        .is_err());
    assert_eq!(w.read("a"), b"old");
}

#[test]
fn notebook_replace_insert_delete_by_id_and_index() {
    let mut w = Workspace::new();
    let book = json!({"nbformat":4,"nbformat_minor":5,"metadata":{"keep":true},"cells":[
        {"id":"first","cell_type":"code","metadata":{"tag":1},"source":["old"],"outputs":[{"output_type":"stream","text":"old"}],"execution_count":4}
    ]});
    w.seed("n.ipynb", &serde_json::to_vec(&book).unwrap());
    w.run(
        "NotebookEdit",
        json!({"notebook_path":"n.ipynb","cell_id":"first","new_source":"a\nb"}),
    )
    .unwrap();
    let updated: Value = serde_json::from_slice(&w.read("n.ipynb")).unwrap();
    assert_eq!(updated["cells"][0]["source"], json!(["a\n", "b"]));
    assert_eq!(updated["cells"][0]["outputs"], json!([]));
    assert!(updated["cells"][0]["execution_count"].is_null());
    assert_eq!(updated["metadata"], book["metadata"]);
    w.run("NotebookEdit", json!({"path":"n.ipynb","cell_id":"first","edit_mode":"insert","cell_type":"markdown","new_source":"# title"})).unwrap();
    let updated: Value = serde_json::from_slice(&w.read("n.ipynb")).unwrap();
    assert_eq!(updated["cells"].as_array().unwrap().len(), 2);
    assert!(updated["cells"][1]["id"].is_string());
    assert!(updated["cells"][1].get("outputs").is_none());
    w.run(
        "NotebookEdit",
        json!({"path":"n.ipynb","cell_number":0,"edit_mode":"delete"}),
    )
    .unwrap();
    let updated: Value = serde_json::from_slice(&w.read("n.ipynb")).unwrap();
    assert_eq!(updated["cells"][0]["cell_type"], "markdown");
}

#[test]
fn notebook_invalid_operations_leave_bytes_unchanged() {
    let mut w = Workspace::new();
    let bytes = br#"{"nbformat":4,"cells":[]}"#;
    w.seed("n", bytes);
    for input in [
        json!({"path":"n","cell_number":0,"new_source":"x"}),
        json!({"path":"n","edit_mode":"insert","cell_type":"invalid","new_source":"x"}),
        json!({"path":"n","edit_mode":"delete","cell_id":"missing"}),
    ] {
        assert!(w.run("NotebookEdit", input).is_err());
        assert_eq!(w.read("n"), bytes);
    }
}

#[test]
fn patch_add_update_delete_move_and_state_tracking() {
    let mut w = Workspace::new();
    w.patch("*** Begin Patch\n*** Add File: a\n+first\n+second\n*** End Patch")
        .unwrap();
    w.patch("*** Begin Patch\n*** Update File: a\n*** Move to: sub/b\n@@\n first\n-second\n+third\n*** End Patch").unwrap();
    assert!(!w.root.join("a").exists());
    assert!(!w.state.contains_key(&w.root.join("a")));
    assert_eq!(w.read("sub/b"), b"first\nthird\n");
    w.patch("*** Begin Patch\n*** Delete File: sub/b\n*** End Patch")
        .unwrap();
    assert!(!w.root.join("sub/b").exists());
    assert!(w.state.is_empty());
}

#[test]
fn patch_fuzzy_hunks_anchors_eof_preserve_context_whitespace() {
    let mut w = Workspace::new();
    w.seed(
        "a",
        "fn start() {\r\n    α  \r\n    old\r\n}\r\n".as_bytes(),
    );
    w.patch("*** Begin Patch\n*** Update File: a\n@@ fn start() {\n α\n-old\n+new\n }\n*** End of File\n*** End Patch").unwrap();
    assert_eq!(
        String::from_utf8(w.read("a")).unwrap(),
        "fn start() {\r\n    α  \r\nnew\r\n}\r\n"
    );
}

#[test]
fn patch_multiple_hunks_and_implicit_first_hunk() {
    let mut w = Workspace::new();
    w.seed("a", b"one\ntwo\nthree\nfour");
    w.patch("*** Begin Patch\n*** Update File: a\n-one\n+ONE\n@@\n-four\n+FOUR\n*** End of File\n*** End Patch").unwrap();
    assert_eq!(w.read("a"), b"ONE\ntwo\nthree\nFOUR");
}

#[test]
fn patch_validation_is_atomic_across_files() {
    let mut w = Workspace::new();
    w.seed("a", b"old\n");
    let state = w.state.clone();
    assert!(w.patch("*** Begin Patch\n*** Update File: a\n@@\n-old\n+new\n*** Add File: added\n+x\n*** Update File: missing\n@@\n-x\n+y\n*** End Patch").is_err());
    assert_eq!(w.read("a"), b"old\n");
    assert!(!w.root.join("added").exists());
    assert_eq!(w.state, state);
}

#[test]
fn patch_requires_read_and_does_not_clobber_move_or_add_targets() {
    let mut w = Workspace::new();
    fs::write(w.root.join("a"), "x").unwrap();
    assert!(w
        .patch("*** Begin Patch\n*** Delete File: a\n*** End Patch")
        .is_err());
    w.seed("a", b"x\n");
    w.seed("b", b"b\n");
    assert!(w
        .patch("*** Begin Patch\n*** Add File: a\n+y\n*** End Patch")
        .is_err());
    assert!(w
        .patch("*** Begin Patch\n*** Update File: a\n*** Move to: b\n@@\n-x\n+y\n*** End Patch")
        .is_err());
    assert_eq!(w.read("a"), b"x\n");
    assert_eq!(w.read("b"), b"b\n");
}

#[test]
fn malformed_patch_and_duplicate_targets_are_rejected() {
    let mut w = Workspace::new();
    for patch in [
        "garbage",
        "*** Begin Patch\n*** End Patch",
        "*** Begin Patch\n*** Add File: a\nnot plus\n*** End Patch",
        "*** Begin Patch\n*** Update File: a\n*** End Patch",
        "*** Begin Patch\n*** Add File: a\n+x\n*** Add File: a\n+y\n*** End Patch",
    ] {
        assert!(w.patch(patch).is_err());
        assert!(!w.root.join("a").exists());
    }
}

#[test]
fn patch_unicode_malformed_prefix_returns_error_not_panic() {
    let mut w = Workspace::new();
    w.seed("a", b"old\n");
    assert!(w
        .patch("*** Begin Patch\n*** Update File: a\n@@\nα invalid\n*** End Patch")
        .is_err());
    assert_eq!(w.read("a"), b"old\n");
}

#[test]
fn patch_pure_addition_appends_and_unicode_fuzzy_context_matches() {
    let mut w = Workspace::new();
    w.seed("a", "‘old’\n".as_bytes());
    w.patch("*** Begin Patch\n*** Update File: a\n@@\n-'old'\n+new\n@@\n+tail\n*** End Patch")
        .unwrap();
    assert_eq!(w.read("a"), b"new\ntail\n");
}

#[test]
fn binary_and_invalid_utf8_edits_fail_without_data_loss() {
    let mut w = Workspace::new();
    for bytes in [&b"a\0b"[..], &b"\xff"[..]] {
        w.seed("a", bytes);
        assert!(w
            .run(
                "Edit",
                json!({"path":"a","old_string":"a","new_string":"b"})
            )
            .is_err());
        assert_eq!(w.read("a"), bytes);
    }
}
