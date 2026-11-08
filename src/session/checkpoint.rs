//! File-only shadow checkpoints stored as Git objects, never in a user's index or history.
//!
//! Callers must serialize checkpoint operations with workspace edits. Portable filesystem APIs
//! cannot protect against a hostile process swapping path components after validation.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const CONFIG: &str = "[core]\n\trepositoryformatversion = 0\n\tbare = true\n\tfilemode = true\n";
const MARKER: &[u8] = b"gitcode file checkpoints v1\n";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    files: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: PathBuf,
    blob: Option<String>,
    executable: bool,
}

/// A dedicated bare object store outside the workspace.
///
/// Paths denote individual regular files, not directories or symlinks. Missing files
/// are recorded explicitly. IDs name manifest blobs, not commits; no index or refs
/// are created. Git environment variables and global/system configuration are ignored.
pub struct Checkpoints {
    input_root: PathBuf,
    root: PathBuf,
    git_dir: PathBuf,
}

impl Checkpoints {
    /// Open or initialize an isolated checkpoint store outside the canonical workspace.
    ///
    /// Existing repositories without this format's marker are refused. Storage and
    /// workspace must be private to the caller while an operation is in progress.
    pub async fn open(root: &Path, storage: &Path) -> Result<Self, String> {
        let input_root = if root.is_absolute() {
            root.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| format!("resolve current directory: {e}"))?
                .join(root)
        };
        let root = std::fs::canonicalize(root).map_err(|e| format!("resolve workspace: {e}"))?;
        if !root.is_dir() {
            return Err("checkpoint workspace must be a directory".into());
        }
        std::fs::create_dir_all(storage).map_err(|e| format!("create checkpoint storage: {e}"))?;
        let storage = std::fs::canonicalize(storage)
            .map_err(|e| format!("resolve checkpoint storage: {e}"))?;
        let git_dir = storage.join("shadow.git");
        if git_dir.starts_with(&root) || root.starts_with(&git_dir) {
            return Err("checkpoint storage must be outside workspace".into());
        }
        let this = Self {
            input_root,
            root,
            git_dir,
        };
        match std::fs::symlink_metadata(&this.git_dir) {
            Ok(_) => this.validate_store()?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&this.git_dir)
                    .map_err(|e| format!("create shadow repository: {e}"))?;
                let output = Self::clean_git()
                    .args([
                        "init",
                        "--bare",
                        "--quiet",
                        "--template=",
                        "--object-format=sha1",
                    ])
                    .arg(&this.git_dir)
                    .output()
                    .await
                    .map_err(|e| format!("initialize shadow git: {e}"))?;
                if !output.status.success() {
                    return Err("shadow git initialization failed".into());
                }
                std::fs::write(this.git_dir.join("config"), CONFIG)
                    .map_err(|e| format!("write shadow configuration: {e}"))?;
                std::fs::write(this.git_dir.join("gitcode-checkpoint"), MARKER)
                    .map_err(|e| format!("write shadow marker: {e}"))?;
                this.validate_store()?;
            }
            Err(e) => return Err(format!("inspect shadow repository: {e}")),
        }
        Ok(this)
    }

    fn clean_git() -> Command {
        let mut command = Command::new("git");
        command.env_clear();
        for name in ["PATH", "SystemRoot", "COMSPEC"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", null)
            .env("GIT_CONFIG_GLOBAL", null)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C");
        command
    }

    fn validate_store(&self) -> Result<(), String> {
        if std::fs::canonicalize(&self.git_dir).ok().as_ref() != Some(&self.git_dir) {
            return Err("shadow storage was replaced or redirected".into());
        }
        fn check(path: &Path) -> Result<(), String> {
            let metadata = std::fs::symlink_metadata(path)
                .map_err(|e| format!("inspect shadow storage: {e}"))?;
            if metadata.file_type().is_symlink() {
                return Err("symlinks are forbidden in shadow storage".into());
            }
            if metadata.is_dir() {
                for child in
                    std::fs::read_dir(path).map_err(|e| format!("read shadow storage: {e}"))?
                {
                    check(&child.map_err(|e| format!("read shadow entry: {e}"))?.path())?;
                }
            } else if !metadata.is_file() {
                return Err("special files are forbidden in shadow storage".into());
            }
            Ok(())
        }
        check(&self.git_dir)?;
        if std::fs::read(self.git_dir.join("gitcode-checkpoint"))
            .ok()
            .as_deref()
            != Some(MARKER)
            || std::fs::read(self.git_dir.join("config")).ok().as_deref() != Some(CONFIG.as_bytes())
        {
            return Err("unrecognized or modified shadow repository".into());
        }
        if self.git_dir.join("objects/info/alternates").exists()
            || self.git_dir.join("objects/info/http-alternates").exists()
            || self.git_dir.join("commondir").exists()
        {
            return Err("external shadow object stores are forbidden".into());
        }
        Ok(())
    }

    async fn run(&self, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>, String> {
        self.validate_store()?;
        let mut command = Self::clean_git();
        command
            .arg("--git-dir")
            .arg(&self.git_dir)
            .current_dir(&self.git_dir)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| format!("run shadow git: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            if let Some(bytes) = input {
                stdin
                    .write_all(bytes)
                    .await
                    .map_err(|e| format!("write shadow input: {e}"))?;
            }
        }
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| format!("wait for shadow git: {e}"))?;
        if !output.status.success() {
            return Err("shadow git operation failed".into());
        }
        Ok(output.stdout)
    }

    fn relative(&self, path: &Path) -> Result<PathBuf, String> {
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.root)
                .or_else(|_| path.strip_prefix(&self.input_root))
                .map_err(|_| "checkpoint path is outside workspace")?
        } else {
            path
        };
        // Check the spelling too: Path::components normalizes interior '.' components.
        let spelling = relative.to_str().ok_or("checkpoint paths must be UTF-8")?;
        if spelling.is_empty()
            || spelling
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || relative.components().any(|part| match part {
                Component::Normal(name) => name.to_string_lossy().eq_ignore_ascii_case(".git"),
                _ => true,
            })
        {
            return Err("invalid checkpoint path or git metadata path".into());
        }
        Ok(relative.to_path_buf())
    }

    fn inspect(&self, relative: &Path) -> Result<Option<std::fs::Metadata>, String> {
        if std::fs::canonicalize(&self.root).ok().as_ref() != Some(&self.root) {
            return Err("checkpoint workspace was replaced".into());
        }
        let mut current = self.root.clone();
        let count = relative.components().count();
        for (index, part) in relative.components().enumerate() {
            current.push(part);
            match std::fs::symlink_metadata(&current) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        return Err("checkpoint paths cannot contain symlinks".into());
                    }
                    if index + 1 == count {
                        if !metadata.is_file() {
                            return Err("checkpoint paths must denote regular files".into());
                        }
                        return Ok(Some(metadata));
                    }
                    if !metadata.is_dir() {
                        return Err("checkpoint path parent is not a directory".into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(format!("inspect checkpoint path: {e}")),
            }
        }
        Err("empty checkpoint path".into())
    }

    fn valid_id(id: &str) -> bool {
        id.len() == 40 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    async fn blob(&self, bytes: &[u8]) -> Result<String, String> {
        let output = self
            .run(&["hash-object", "-w", "--stdin"], Some(bytes))
            .await?;
        let id = String::from_utf8(output).map_err(|_| "invalid shadow object identifier")?;
        let id = id.trim();
        if !Self::valid_id(id) {
            return Err("invalid shadow object identifier".into());
        }
        Ok(id.to_owned())
    }

    /// Snapshot exactly the selected files, including explicit absence for new files.
    ///
    /// Empty selections, directories, symlinks, traversal and `.git` components are
    /// rejected. Snapshots are independent, not cumulative; IDs identify manifest blobs.
    pub async fn snapshot(&self, paths: &[PathBuf]) -> Result<String, String> {
        if paths.is_empty() {
            return Err("checkpoint requires at least one file".into());
        }
        let mut selected = Vec::new();
        for path in paths {
            let relative = self.relative(path)?;
            let metadata = self.inspect(&relative)?;
            if selected.iter().any(|(existing, _)| existing == &relative) {
                return Err("duplicate checkpoint path".into());
            }
            selected.push((relative, metadata));
        }
        let mut files = Vec::new();
        for (path, metadata) in selected {
            let (blob, executable) = if let Some(metadata) = metadata {
                let bytes = std::fs::read(self.root.join(&path))
                    .map_err(|e| format!("read checkpoint file: {e}"))?;
                #[cfg(unix)]
                let executable = {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode() & 0o111 != 0
                };
                #[cfg(not(unix))]
                let executable = {
                    let _ = metadata;
                    false
                };
                (Some(self.blob(&bytes).await?), executable)
            } else {
                (None, false)
            };
            files.push(Entry {
                path,
                blob,
                executable,
            });
        }
        let bytes = serde_json::to_vec(&Manifest { version: 1, files })
            .map_err(|e| format!("encode checkpoint manifest: {e}"))?;
        self.blob(&bytes).await
    }

    /// Restore only selected files recorded in this snapshot, or delete them if absent.
    ///
    /// Approval belongs to the caller. All selections and blobs are validated before
    /// writes; I/O errors during application can still leave a partially restored set.
    /// Missing parent directories are created. Directories and symlinks are never removed.
    pub async fn restore(&self, id: &str, paths: &[PathBuf]) -> Result<(), String> {
        if !Self::valid_id(id) || paths.is_empty() {
            return Err("invalid checkpoint identifier or empty restore selection".into());
        }
        let bytes = self.run(&["cat-file", "blob", id], None).await?;
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .map_err(|e| format!("invalid checkpoint manifest: {e}"))?;
        if manifest.version != 1 || manifest.files.is_empty() {
            return Err("unsupported checkpoint manifest".into());
        }
        let mut recorded = std::collections::HashSet::new();
        for entry in &manifest.files {
            let relative = self.relative(&entry.path)?;
            if entry.path.is_absolute()
                || !recorded.insert(relative)
                || entry
                    .blob
                    .as_ref()
                    .is_some_and(|blob| !Self::valid_id(blob))
            {
                return Err("invalid checkpoint manifest entry".into());
            }
        }
        let mut pending = Vec::new();
        for path in paths {
            let relative = self.relative(path)?;
            self.inspect(&relative)?;
            if pending.iter().any(|(existing, _, _)| existing == &relative) {
                return Err("duplicate restore path".into());
            }
            let entry = manifest
                .files
                .iter()
                .find(|entry| entry.path == relative)
                .ok_or("file was not selected in this checkpoint")?;
            let content = match &entry.blob {
                Some(blob) => Some(self.run(&["cat-file", "blob", blob], None).await?),
                None => None,
            };
            pending.push((relative, content, entry.executable));
        }
        for (relative, content, executable) in pending {
            self.inspect(&relative)?;
            let path = self.root.join(&relative);
            if let Some(content) = content {
                let parent = path.parent().ok_or("restore path has no parent")?;
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create restore parents: {e}"))?;
                self.inspect(&relative)?;
                // Unlink first so a workspace hardlink cannot overwrite an external file.
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("replace restored file: {e}")),
                }
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .map_err(|e| format!("create restored file: {e}"))?;
                std::io::Write::write_all(&mut file, &content)
                    .map_err(|e| format!("write restored file: {e}"))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(std::fs::Permissions::from_mode(if executable {
                        0o755
                    } else {
                        0o644
                    }))
                    .map_err(|e| format!("restore file mode: {e}"))?;
                }
                #[cfg(not(unix))]
                let _ = executable;
            } else {
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("delete restored file: {e}")),
                }
            }
        }
        Ok(())
    }
}
