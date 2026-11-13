//! Integration coverage for isolated file checkpoints.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[deny(clippy::unwrap_used, clippy::expect_used)]
#[path = "../src/session/checkpoint.rs"]
mod checkpoint;

use checkpoint::Checkpoints;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    storage: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "gitcode-checkpoint-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let root = base.join("workspace");
        let storage = base.join("storage");
        std::fs::create_dir_all(&root).unwrap();
        Self {
            base,
            root,
            storage,
        }
    }

    async fn open(&self) -> Checkpoints {
        Checkpoints::open(&self.root, &self.storage).await.unwrap()
    }

    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn git(&self, args: &[&str]) -> Vec<u8> {
        let mut command = Command::new("git");
        command.env_clear();
        for name in ["PATH", "SystemRoot", "COMSPEC"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let output = command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", null)
            .env("GIT_CONFIG_GLOBAL", null)
            .args([
                "-c",
                "user.name=Checkpoint test",
                "-c",
                "user.email=test@localhost",
            ])
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}: {:?}", args, output);
        output.stdout
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn paths(names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

#[tokio::test]
async fn nonexistent_files_and_deleted_files_restore_exactly() {
    let f = Fixture::new();
    let cp = f.open().await;
    let new = paths(&["new/deep/file"]);
    let absent = cp.snapshot(&new).await.unwrap();
    cp.restore(&absent, &new).await.unwrap();
    assert!(!f.root.join("new").exists());
    f.write("new/deep/file", b"created");
    cp.restore(&absent, &new).await.unwrap();
    assert!(!f.root.join("new/deep/file").exists());
    f.write("file", b"original\0\xff");
    let saved = cp.snapshot(&paths(&["file"])).await.unwrap();
    std::fs::remove_file(f.root.join("file")).unwrap();
    let deleted = cp.snapshot(&paths(&["file"])).await.unwrap();
    cp.restore(&saved, &paths(&["file"])).await.unwrap();
    assert_eq!(
        std::fs::read(f.root.join("file")).unwrap(),
        b"original\0\xff"
    );
    cp.restore(&deleted, &paths(&["file"])).await.unwrap();
    assert!(!f.root.join("file").exists());
}

#[tokio::test]
async fn snapshots_are_independent_selected_only_and_reopenable() {
    let f = Fixture::new();
    let cp = f.open().await;
    f.write("a", b"A");
    f.write("b", b"B");
    let a = cp.snapshot(&paths(&["a"])).await.unwrap();
    let b = cp.snapshot(&paths(&["b"])).await.unwrap();
    f.write("a", b"edited");
    assert!(cp.restore(&b, &paths(&["a"])).await.is_err());
    assert_eq!(std::fs::read(f.root.join("a")).unwrap(), b"edited");
    f.open()
        .await
        .restore(&a, &[f.root.join("a")])
        .await
        .unwrap();
    assert_eq!(std::fs::read(f.root.join("a")).unwrap(), b"A");
    assert!(!f.storage.join("shadow.git/index").exists());
    assert!(!f.storage.join("shadow.git/refs/heads/master").exists());
}

#[tokio::test]
async fn canonical_storage_with_spaces_and_unicode_round_trips_binary_files() {
    let f = Fixture::new();
    let storage = f.base.join("checkpoint store Юникод");
    std::fs::create_dir(&storage).unwrap();
    let storage = std::fs::canonicalize(storage).unwrap();
    #[cfg(windows)]
    assert!(matches!(
        storage.components().next(),
        Some(std::path::Component::Prefix(prefix))
            if matches!(prefix.kind(), std::path::Prefix::VerbatimDisk(_))
    ));
    let cp = Checkpoints::open(&f.root, &storage).await.unwrap();
    f.write("nested/file", b"saved\r\n\0\xff");
    let selected = paths(&["nested/file", "absent"]);
    let id = cp.snapshot(&selected).await.unwrap();
    f.write("nested/file", b"changed");
    f.write("absent", b"created");
    let reopened = Checkpoints::open(&f.root, &storage).await.unwrap();
    reopened.restore(&id, &selected).await.unwrap();
    assert_eq!(
        std::fs::read(f.root.join("nested/file")).unwrap(),
        b"saved\r\n\0\xff"
    );
    assert!(!f.root.join("absent").exists());
    assert!(!storage.join("shadow.git/index").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_alias_accepts_absolute_paths_but_not_symlink_files() {
    let f = Fixture::new();
    f.write("a", b"original");
    let alias = f.base.join("alias");
    std::os::unix::fs::symlink(&f.root, &alias).unwrap();
    let cp = Checkpoints::open(&alias, &f.storage).await.unwrap();
    let id = cp.snapshot(&[alias.join("a")]).await.unwrap();
    f.write("a", b"changed");
    cp.restore(&id, &[alias.join("a")]).await.unwrap();
    assert_eq!(std::fs::read(f.root.join("a")).unwrap(), b"original");
    std::os::unix::fs::symlink(f.root.join("a"), f.root.join("link")).unwrap();
    assert!(cp.snapshot(&[alias.join("link")]).await.is_err());
    assert!(cp.snapshot(&[f.base.join("outside")]).await.is_err());
}

#[tokio::test]
async fn never_changes_user_index_history_or_unselected_files() {
    let f = Fixture::new();
    f.git(&["init", "--quiet"]);
    f.write("tracked", b"committed");
    f.git(&["add", "."]);
    f.git(&["commit", "--quiet", "-m", "user history"]);
    f.write("tracked", b"staged");
    f.git(&["add", "tracked"]);
    f.write("tracked", b"unstaged");
    f.write(".gitignore", b"ignored\n");
    f.write("ignored", b"ignored original");
    f.write("untouched", b"untouched");
    let index = std::fs::read(f.root.join(".git/index")).unwrap();
    let head = f.git(&["rev-parse", "HEAD"]);
    let reflog = std::fs::read(f.root.join(".git/logs/HEAD")).unwrap();
    let cp = f.open().await;
    let id = cp.snapshot(&paths(&["tracked", "ignored"])).await.unwrap();
    f.write("tracked", b"changed");
    f.write("ignored", b"changed");
    cp.restore(&id, &paths(&["tracked", "ignored"]))
        .await
        .unwrap();
    assert_eq!(std::fs::read(f.root.join("tracked")).unwrap(), b"unstaged");
    assert_eq!(
        std::fs::read(f.root.join("ignored")).unwrap(),
        b"ignored original"
    );
    assert_eq!(
        std::fs::read(f.root.join("untouched")).unwrap(),
        b"untouched"
    );
    assert_eq!(std::fs::read(f.root.join(".git/index")).unwrap(), index);
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(
        std::fs::read(f.root.join(".git/logs/HEAD")).unwrap(),
        reflog
    );
}

#[tokio::test]
async fn pathspec_metacharacters_are_literal_on_every_platform() {
    let f = Fixture::new();
    let cp = f.open().await;
    f.write("[literal]", b"selected");
    f.write("l", b"unselected");
    let selected = paths(&["[literal]"]);
    let id = cp.snapshot(&selected).await.unwrap();
    f.write("[literal]", b"changed");
    f.write("l", b"must stay changed");
    cp.restore(&id, &selected).await.unwrap();
    assert_eq!(
        std::fs::read(f.root.join("[literal]")).unwrap(),
        b"selected"
    );
    assert_eq!(
        std::fs::read(f.root.join("l")).unwrap(),
        b"must stay changed"
    );
}

#[tokio::test]
async fn invalid_paths_and_selections_are_rejected_before_changes() {
    let f = Fixture::new();
    let cp = f.open().await;
    f.write("safe", b"original");
    let id = cp.snapshot(&paths(&["safe"])).await.unwrap();
    f.write("safe", b"changed");
    std::fs::create_dir(f.root.join("dir")).unwrap();
    for bad in [
        "",
        ".",
        "..",
        "../outside",
        "a/../safe",
        "a/./safe",
        ".git/config",
        "x/.git/index",
        ".GIT/config",
        "dir",
        "safe/child",
        ":(glob)*",
        "/outside",
    ] {
        if bad == ":(glob)*" && cfg!(unix) {
            // Unix permits these bytes; Windows must reject the invalid filename.
            // Literal pathspec-like filenames are never interpreted by Git.
            let literal = cp.snapshot(&paths(&[bad])).await.unwrap();
            cp.restore(&literal, &paths(&[bad])).await.unwrap();
            continue;
        }
        assert!(cp.snapshot(&paths(&[bad])).await.is_err(), "snapshot {bad}");
        assert!(
            cp.restore(&id, &paths(&["safe", bad])).await.is_err(),
            "restore {bad}"
        );
        assert_eq!(std::fs::read(f.root.join("safe")).unwrap(), b"changed");
    }
    assert!(cp.snapshot(&[]).await.is_err());
    assert!(cp.snapshot(&paths(&["safe", "safe"])).await.is_err());
    assert!(cp.restore("--help", &paths(&["safe"])).await.is_err());
    assert!(cp
        .restore(&"0".repeat(40), &paths(&["safe"]))
        .await
        .is_err());
    assert!(cp.restore(&id, &paths(&["unknown"])).await.is_err());
    assert!(cp.restore(&id, &paths(&["safe", "safe"])).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_parents_and_directory_replacements_are_refused() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let outside = f.base.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("file"), b"external").unwrap();
    let cp = f.open().await;
    let absent = cp.snapshot(&paths(&["parent/file"])).await.unwrap();
    symlink(&outside, f.root.join("parent")).unwrap();
    assert!(cp.snapshot(&paths(&["parent/file"])).await.is_err());
    assert!(cp.restore(&absent, &paths(&["parent/file"])).await.is_err());
    symlink(outside.join("file"), f.root.join("link")).unwrap();
    assert!(cp.snapshot(&paths(&["link"])).await.is_err());
    symlink(outside.join("missing"), f.root.join("dangling")).unwrap();
    assert!(cp.snapshot(&paths(&["dangling"])).await.is_err());
    let absent_file = cp.snapshot(&paths(&["directory"])).await.unwrap();
    std::fs::create_dir(f.root.join("directory")).unwrap();
    assert!(cp
        .restore(&absent_file, &paths(&["directory"]))
        .await
        .is_err());
    assert_eq!(std::fs::read(outside.join("file")).unwrap(), b"external");
    assert!(f.root.join("directory").is_dir());
}

#[cfg(unix)]
#[tokio::test]
async fn restores_executable_mode_and_breaks_hardlinks() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let cp = f.open().await;
    f.write("script", b"saved");
    std::fs::set_permissions(
        f.root.join("script"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let id = cp.snapshot(&paths(&["script"])).await.unwrap();
    let external = f.base.join("external");
    std::fs::write(&external, b"external").unwrap();
    std::fs::remove_file(f.root.join("script")).unwrap();
    std::fs::hard_link(&external, f.root.join("script")).unwrap();
    cp.restore(&id, &paths(&["script"])).await.unwrap();
    assert_eq!(std::fs::read(&external).unwrap(), b"external");
    assert_eq!(std::fs::read(f.root.join("script")).unwrap(), b"saved");
    assert_eq!(
        std::fs::metadata(f.root.join("script"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[tokio::test]
async fn unsafe_storage_and_modified_configuration_are_refused() {
    let f = Fixture::new();
    assert!(Checkpoints::open(&f.root, &f.root.join("storage"))
        .await
        .is_err());
    let cp = f.open().await;
    std::fs::write(
        f.storage.join("shadow.git/config"),
        b"[include]\npath=/tmp/config\n",
    )
    .unwrap();
    assert!(Checkpoints::open(&f.root, &f.storage).await.is_err());
    assert!(cp.snapshot(&paths(&["missing"])).await.is_err());
    let other = Fixture::new();
    std::fs::create_dir_all(other.storage.join("shadow.git")).unwrap();
    assert!(Checkpoints::open(&other.root, &other.storage)
        .await
        .is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn shadow_storage_symlinks_and_alternates_are_refused() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let cp = f.open().await;
    let outside = f.base.join("outside");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, f.storage.join("shadow.git/objects/aa")).unwrap();
    assert!(cp.snapshot(&paths(&["missing"])).await.is_err());
    assert!(Checkpoints::open(&f.root, &f.storage).await.is_err());
    std::fs::remove_file(f.storage.join("shadow.git/objects/aa")).unwrap();
    std::fs::write(
        f.storage.join("shadow.git/objects/info/alternates"),
        outside.as_os_str().as_encoded_bytes(),
    )
    .unwrap();
    assert!(cp.snapshot(&paths(&["missing"])).await.is_err());
}

#[tokio::test]
async fn recreates_missing_parents_and_preflights_all_selected_files() {
    let f = Fixture::new();
    let cp = f.open().await;
    f.write("nested/deep/file", b"saved");
    f.write("second", b"second saved");
    let id = cp
        .snapshot(&paths(&["nested/deep/file", "second"]))
        .await
        .unwrap();
    std::fs::remove_dir_all(f.root.join("nested")).unwrap();
    cp.restore(&id, &paths(&["nested/deep/file"]))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(f.root.join("nested/deep/file")).unwrap(),
        b"saved"
    );
    f.write("nested/deep/file", b"changed");
    std::fs::remove_file(f.root.join("second")).unwrap();
    std::fs::create_dir(f.root.join("second")).unwrap();
    assert!(cp
        .restore(&id, &paths(&["nested/deep/file", "second"]))
        .await
        .is_err());
    assert_eq!(
        std::fs::read(f.root.join("nested/deep/file")).unwrap(),
        b"changed"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn redirected_storage_parent_is_rejected_after_open() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let cp = f.open().await;
    let moved = f.base.join("moved-storage");
    std::fs::rename(&f.storage, &moved).unwrap();
    symlink(&moved, &f.storage).unwrap();
    assert!(cp.snapshot(&paths(&["missing"])).await.is_err());
}

#[test]
fn inherited_git_configuration_cannot_escape_the_shadow_store() {
    let f = Fixture::new();
    let malicious = f.base.join("malicious-config");
    std::fs::write(&malicious, b"[core]\nrepositoryformatversion = 999\n").unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "hostile_environment_child", "--nocapture"])
        .env("CHECKPOINT_ENV_CHILD", &f.base)
        .env("HOME", &f.base)
        .env("GIT_CONFIG", &malicious)
        .env("GIT_CONFIG_GLOBAL", &malicious)
        .env("GIT_CONFIG_SYSTEM", &malicious)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.repositoryformatversion")
        .env("GIT_CONFIG_VALUE_0", "999")
        .env(
            "GIT_CONFIG_PARAMETERS",
            "'core.repositoryformatversion=999'",
        )
        .env("GIT_DIR", f.base.join("user.git"))
        .env("GIT_WORK_TREE", &f.base)
        .env("GIT_COMMON_DIR", f.base.join("user.git"))
        .env("GIT_INDEX_FILE", f.base.join("user-index"))
        .env("GIT_OBJECT_DIRECTORY", f.base.join("user-objects"))
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", "/nonexistent")
        .env("GIT_NAMESPACE", "hostile")
        .env("GIT_SHALLOW_FILE", f.base.join("user-shallow"))
        .env("GIT_DEFAULT_HASH", "sha256")
        .env("GIT_TEMPLATE_DIR", "/nonexistent")
        .env("GIT_EXEC_PATH", "/nonexistent")
        .env("GIT_TRACE", f.base.join("leaked-trace"))
        .env("GIT_TRACE2_EVENT", f.base.join("leaked-trace2"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    for name in [
        "user.git",
        "user-index",
        "user-objects",
        "user-shallow",
        "leaked-trace",
        "leaked-trace2",
    ] {
        assert!(!f.base.join(name).exists(), "leaked {name}");
    }
}

#[tokio::test]
async fn hostile_environment_child() {
    let Some(base) = std::env::var_os("CHECKPOINT_ENV_CHILD") else {
        return;
    };
    let base = Path::new(&base);
    let root = base.join("workspace");
    let cp = Checkpoints::open(&root, &base.join("hostile-storage"))
        .await
        .unwrap();
    std::fs::write(root.join("file"), b"saved").unwrap();
    let id = cp.snapshot(&paths(&["file", "new"])).await.unwrap();
    assert_eq!(id.len(), 40);
    std::fs::write(root.join("file"), b"changed").unwrap();
    std::fs::write(root.join("new"), b"created").unwrap();
    cp.restore(&id, &paths(&["file", "new"])).await.unwrap();
    assert_eq!(std::fs::read(root.join("file")).unwrap(), b"saved");
    assert!(!root.join("new").exists());
}
