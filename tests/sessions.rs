//! M8 storage and confined memory coverage, using only disposable explicit homes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod provider {
    pub use gitcode::provider::canonical;
}
#[path = "../src/session/memory.rs"]
mod memory;
#[path = "../src/session/store.rs"]
mod store;

use gitcode::provider::canonical::{Content, Message, Role};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use store::{SessionStore, MAX_SESSION_BYTES};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    base: PathBuf,
    home: PathBuf,
    project: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "gitcode-m8-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let home = base.join("home");
        let project = base.join("Project name.with spaces");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&project).unwrap();
        Self {
            base,
            home,
            project,
        }
    }
    fn store(&self) -> SessionStore {
        SessionStore::open(&self.home, &self.project).unwrap()
    }
    fn path(&self, store: &SessionStore, id: &str) -> PathBuf {
        store.directory().join(format!("{id}.jsonl"))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn canonical_roundtrip_metadata_list_and_stable_project_slug() {
    let f = Fixture::new();
    let store = f.store();
    assert_eq!(store.directory(), f.store().directory());
    assert!(store
        .directory()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
    let session = store
        .create("Original", Some("feature/test".into()))
        .unwrap();
    let message = Message {
        role: Role::Assistant,
        content: vec![
            Content::Text {
                text: "line 1\nline 2".into(),
            },
            Content::Thinking {
                thinking: "reason".into(),
                signature: Some("opaque".into()),
                encrypted_content: Some("encrypted".into()),
            },
            Content::ToolUse {
                id: "call-1".into(),
                name: "read".into(),
                input: serde_json::json!({"path":"file"}),
            },
            Content::ToolResult {
                tool_use_id: "call-1".into(),
                content: "result".into(),
                is_error: false,
            },
            Content::Image {
                media_type: "image/png".into(),
                data: "AA==".into(),
            },
            Content::Document {
                media_type: "application/pdf".into(),
                data: "AA==".into(),
            },
        ],
    };
    store.append(&session.id, &message).unwrap();
    let before = fs::read(f.path(&store, &session.id)).unwrap();
    store.update_metadata(&session.id, "Renamed", None).unwrap();
    assert!(fs::read(f.path(&store, &session.id))
        .unwrap()
        .starts_with(&before));
    let loaded = f.store().resume(&session.id).unwrap();
    assert_eq!(loaded.project, f.project);
    assert_eq!(loaded.messages, vec![message]);
    assert_eq!(loaded.metadata.title, "Renamed");
    assert_eq!(
        loaded.metadata.created_at_ms,
        session.metadata.created_at_ms
    );
    assert!(loaded.metadata.updated_at_ms >= session.metadata.created_at_ms);
    assert_eq!(loaded.metadata.branch, None);
    let list = store.list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, session.id);
    assert_eq!(list[0].metadata, loaded.metadata);
    assert_eq!(list[0].message_count, 1);
    assert_eq!(list[0].interrupted_bytes, 0);
}

#[test]
fn fork_copies_only_committed_prefix_and_preserves_source() {
    let f = Fixture::new();
    let store = f.store();
    let source = store.create("Source", Some("main".into())).unwrap();
    for text in ["first", "second"] {
        store
            .append(&source.id, &Message::text(Role::User, text))
            .unwrap();
    }
    let before = fs::read(f.path(&store, &source.id)).unwrap();
    let fork = store.fork(&source.id, 1, "Fork").unwrap();
    assert_ne!(fork.id, source.id);
    assert_eq!(fork.messages, vec![Message::text(Role::User, "first")]);
    assert_eq!(
        fork.metadata.forked_from.as_deref(),
        Some(source.id.as_str())
    );
    assert_eq!(fork.metadata.branch.as_deref(), Some("main"));
    assert_eq!(store.resume(&fork.id).unwrap().messages, fork.messages);
    assert!(store.fork(&source.id, 3, "Invalid").is_err());
    assert_eq!(fs::read(f.path(&store, &source.id)).unwrap(), before);
    assert!(store
        .fork(&source.id, 0, "Empty")
        .unwrap()
        .messages
        .is_empty());
}

#[test]
fn interrupted_tail_is_ignored_then_removed_without_inventing_a_message() {
    let f = Fixture::new();
    let store = f.store();
    let s = store.create("Recovery", None).unwrap();
    store
        .append(&s.id, &Message::text(Role::User, "saved"))
        .unwrap();
    let path = f.path(&store, &s.id);
    let committed = fs::read(&path).unwrap();
    let tail = b"{\"schema_version\":1,\"type\":\"message\",\"message\":";
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(tail)
        .unwrap();
    let resumed = store.resume(&s.id).unwrap();
    assert_eq!(resumed.interrupted_bytes, tail.len() as u64);
    assert_eq!(resumed.messages.len(), 1);
    assert_eq!(
        store.list().unwrap()[0].interrupted_bytes,
        tail.len() as u64
    );
    assert_eq!(fs::read(&path).unwrap().len(), committed.len() + tail.len());
    store
        .append(&s.id, &Message::text(Role::Assistant, "next"))
        .unwrap();
    let bytes = fs::read(&path).unwrap();
    assert!(bytes.starts_with(&committed));
    assert_eq!(store.resume(&s.id).unwrap().interrupted_bytes, 0);
    assert_eq!(store.resume(&s.id).unwrap().messages.len(), 2);
    // Even syntactically valid JSON without a newline is not a committed record.
    let last = bytes[committed.len()..bytes.len() - 1].to_vec();
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(&last)
        .unwrap();
    assert_eq!(store.resume(&s.id).unwrap().messages.len(), 2);
}

#[test]
fn corrupt_complete_records_missing_header_and_future_schema_fail_closed() {
    let f = Fixture::new();
    let store = f.store();
    for data in [b"".as_slice(), b"{\"partial\":", b"{}\n", b"not json\n"] {
        let s = store.create("Invalid", None).unwrap();
        fs::write(f.path(&store, &s.id), data).unwrap();
        assert!(store.resume(&s.id).is_err());
        assert!(store
            .append(&s.id, &Message::text(Role::User, "no repair"))
            .is_err());
        assert_eq!(fs::read(f.path(&store, &s.id)).unwrap(), data);
    }
    let s = store.create("Future", None).unwrap();
    let path = f.path(&store, &s.id);
    let bytes = fs::read_to_string(&path)
        .unwrap()
        .replace("\"schema_version\":1", "\"schema_version\":999");
    fs::write(&path, bytes).unwrap();
    assert!(store.resume(&s.id).is_err());
    assert!(store.list().is_err());
}

#[test]
fn wrong_project_header_invalid_ids_and_oversize_are_rejected() {
    let f = Fixture::new();
    let store = f.store();
    for id in ["", "../outside", "/tmp/session", "a/b", "a.jsonl", "A", "."] {
        assert!(store.resume(id).is_err());
    }
    let s = store.create("Bound", None).unwrap();
    let path = f.path(&store, &s.id);
    let original = fs::read(&path).unwrap();
    let large = Message::text(Role::User, "x".repeat(MAX_SESSION_BYTES as usize));
    assert!(store.append(&s.id, &large).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_SESSION_BYTES + 1)
        .unwrap();
    assert!(store.resume(&s.id).is_err());
    fs::write(&path, &original).unwrap();
    let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
    record["project"] = serde_json::json!(f.home);
    fs::write(&path, format!("{}\n", record)).unwrap();
    assert!(store.resume(&s.id).is_err());
}

#[cfg(unix)]
#[test]
fn private_modes_symlinks_hardlinks_and_unsafe_existing_modes() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let f = Fixture::new();
    let store = f.store();
    let s = store.create("Private", None).unwrap();
    let path = f.path(&store, &s.id);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    for path in [
        f.home.join(".gitcode"),
        f.home.join(".gitcode/sessions"),
        store.directory().to_path_buf(),
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    let alias = store.directory().join("alias.jsonl");
    symlink(&path, &alias).unwrap();
    assert!(store.resume("alias").is_err());
    assert!(store.list().is_err());
    fs::remove_file(alias).unwrap();
    let hardlink = f.base.join("hardlink");
    fs::hard_link(&path, &hardlink).unwrap();
    assert!(store.resume(&s.id).is_err());
    fs::remove_file(hardlink).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.resume(&s.id).is_err());
    let other = Fixture::new();
    symlink(&f.home, other.home.join(".gitcode")).unwrap();
    assert!(SessionStore::open(&other.home, &other.project).is_err());
    let project_alias = f.base.join("project-alias");
    symlink(&f.project, &project_alias).unwrap();
    assert!(SessionStore::open(&f.home, project_alias).is_err());
}

#[tokio::test]
async fn blocking_api_can_be_used_from_spawn_blocking_and_parallel_appends_are_serialized() {
    let f = Fixture::new();
    let store = f.store();
    let s = store.create("Parallel", None).unwrap();
    let mut tasks = Vec::new();
    for i in 0..12 {
        let store = store.clone();
        let id = s.id.clone();
        tasks.push(tokio::task::spawn_blocking(move || {
            store.append(&id, &Message::text(Role::User, format!("message-{i}")))
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    let resumed = tokio::task::spawn_blocking(move || store.resume(&s.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed.messages.len(), 12);
}

#[test]
fn memory_hierarchy_fallback_imports_and_override_order() {
    let f = Fixture::new();
    let cwd = f.project.join("src/nested");
    fs::create_dir_all(&cwd).unwrap();
    fs::write(f.home.join("AGENTS.md"), "global\n").unwrap();
    fs::write(f.project.join("GITCODE.md"), "root\n@rules.md\nafter\n").unwrap();
    fs::write(f.project.join("CLAUDE.md"), "ignored\n").unwrap();
    fs::write(f.project.join("rules.md"), "imported\n").unwrap();
    fs::write(f.project.join("src/CLAUDE.md"), "source\n").unwrap();
    fs::write(cwd.join("AGENTS.md"), "leaf\n@../../rules.md\n").unwrap();
    let loaded = memory::load(&f.home, &f.project, cwd).unwrap();
    assert_eq!(
        loaded.text,
        "global\nroot\nimported\nafter\nsource\nleaf\nimported\n"
    );
    assert_eq!(loaded.sources.len(), 6);
}

#[test]
fn memory_rejects_escapes_cycles_missing_imports_invalid_utf8_and_size_limits() {
    let f = Fixture::new();
    let entry = f.project.join("GITCODE.md");
    assert!(memory::load(&f.home, &f.project, &f.home).is_err());
    assert!(memory::load(&f.home, &f.project, &f.project)
        .unwrap()
        .text
        .is_empty());
    for text in [
        "@../outside.md\n",
        "@/etc/passwd\n",
        "@GITCODE.md\n",
        "@missing.md\n",
        "@bad path.md\n",
    ] {
        fs::write(&entry, text).unwrap();
        assert!(
            memory::load(&f.home, &f.project, &f.project).is_err(),
            "{text}"
        );
    }
    fs::write(&entry, [0xff]).unwrap();
    assert!(memory::load(&f.home, &f.project, &f.project).is_err());
    fs::write(&entry, "x".repeat(memory::MAX_MEMORY_BYTES as usize + 1)).unwrap();
    assert!(memory::load(&f.home, &f.project, &f.project).is_err());
    fs::write(&entry, "@a.md\n").unwrap();
    fs::write(f.project.join("a.md"), "@b.md\n").unwrap();
    fs::write(f.project.join("b.md"), "@a.md\n").unwrap();
    assert!(memory::load(&f.home, &f.project, &f.project).is_err());
}

#[cfg(unix)]
#[test]
fn memory_rejects_entry_and_import_symlinks_without_reading_target() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let target = f.base.join("private-fixture");
    fs::write(&target, "never import").unwrap();
    let entry = f.project.join("GITCODE.md");
    symlink(&target, &entry).unwrap();
    assert!(memory::load(&f.home, &f.project, &f.project).is_err());
    fs::remove_file(&entry).unwrap();
    fs::write(&entry, "@linked.md\n").unwrap();
    symlink(&target, f.project.join("linked.md")).unwrap();
    assert!(memory::load(&f.home, &f.project, &f.project).is_err());
}

#[test]
fn memory_depth_and_file_count_limits_and_home_equals_project() {
    let f = Fixture::new();
    for i in 0..=memory::MAX_IMPORT_DEPTH {
        fs::write(
            f.project.join(format!("chain-{i}.md")),
            format!("@chain-{}.md\n", i + 1),
        )
        .unwrap();
    }
    fs::write(f.project.join("GITCODE.md"), "@chain-0.md\n").unwrap();
    assert!(memory::load(&f.home, &f.project, &f.project).is_err());
    fs::write(f.project.join("small.md"), "ok\n").unwrap();
    fs::write(
        f.project.join("GITCODE.md"),
        "@small.md\n".repeat(memory::MAX_MEMORY_FILES),
    )
    .unwrap();
    assert!(memory::load(&f.home, &f.project, &f.project).is_err());
    fs::write(f.home.join("GITCODE.md"), "once\n").unwrap();
    assert_eq!(
        memory::load(&f.home, &f.home, &f.home).unwrap().text,
        "once\n"
    );
}
