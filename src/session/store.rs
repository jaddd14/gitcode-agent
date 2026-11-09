//! Blocking, bounded JSONL session storage. Run operations in `tokio::task::spawn_blocking`.
//!
//! Each complete line is committed independently. An unterminated tail is never replayed;
//! the next mutation truncates that tail before appending. Complete corrupt lines fail closed.
//! Managed paths reject symlinks. The caller must keep storage ancestors trusted: portable
//! std filesystem APIs cannot prevent a hostile process from replacing ancestor directories.

use crate::provider::canonical::Message;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum on-disk bytes per session, including an interrupted tail.
pub const MAX_SESSION_BYTES: u64 = 16 * 1024 * 1024;
/// Current JSONL record schema.
pub const SCHEMA_VERSION: u32 = 1;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// Session metadata, stored as a complete snapshot on every metadata update.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    /// Human-readable title.
    pub title: String,
    /// Creation time in Unix milliseconds.
    pub created_at_ms: u64,
    /// Last metadata or message append time in Unix milliseconds.
    pub updated_at_ms: u64,
    /// Optional branch label; no git process is invoked.
    pub branch: Option<String>,
    /// Source session for a fork.
    pub forked_from: Option<String>,
}

/// Fully recovered session, containing only committed canonical messages.
#[derive(Debug, Clone)]
pub struct Session {
    /// Stable filename-safe session identifier.
    pub id: String,
    /// Canonical project path captured at creation.
    pub project: PathBuf,
    /// Latest committed metadata.
    pub metadata: Metadata,
    /// Committed messages in append order.
    pub messages: Vec<Message>,
    /// Number of ignored bytes following the last newline.
    pub interrupted_bytes: u64,
}

/// Bounded session index entry.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    /// Session identifier.
    pub id: String,
    /// Latest committed metadata.
    pub metadata: Metadata,
    /// Number of committed messages.
    pub message_count: usize,
    /// Ignored trailing byte count.
    pub interrupted_bytes: u64,
}

#[derive(Serialize, Deserialize)]
struct Record {
    schema_version: u32,
    #[serde(flatten)]
    body: Body,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Body {
    Header {
        id: String,
        project: PathBuf,
        metadata: Metadata,
    },
    Message {
        message: Message,
        updated_at_ms: u64,
    },
    Metadata {
        metadata: Metadata,
    },
}

/// A project-scoped session store. All methods perform blocking filesystem I/O.
#[derive(Debug, Clone)]
pub struct SessionStore {
    directory: PathBuf,
    project: PathBuf,
}

impl SessionStore {
    /// Open/create `<home>/.gitcode/sessions/<stable-project-slug>` privately.
    /// `home` is explicit so tests never need the operator's real home.
    pub fn open(home: impl AsRef<Path>, project: impl AsRef<Path>) -> io::Result<Self> {
        reject_symlinks(home.as_ref())?;
        reject_symlinks(project.as_ref())?;
        let home = fs::canonicalize(home)?;
        let project = fs::canonicalize(project)?;
        if !home.is_dir() || !project.is_dir() {
            return Err(invalid("home and project must be directories"));
        }
        let base = home.join(".gitcode");
        private_directory(&base)?;
        let sessions = base.join("sessions");
        private_directory(&sessions)?;
        let directory = sessions.join(project_slug(&project));
        private_directory(&directory)?;
        Ok(Self { directory, project })
    }

    /// Directory containing this project's session files.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Create a session without overwriting any existing session.
    pub fn create(&self, title: impl Into<String>, branch: Option<String>) -> io::Result<Session> {
        self.create_with_messages(title.into(), branch, None, &[])
    }

    /// Recover complete records. A newline-free or corrupt header is not fabricated.
    pub fn resume(&self, id: &str) -> io::Result<Session> {
        let mut file = self.open_file(id)?;
        file.lock_shared()?;
        self.read_session(id, &mut file).map(|(session, _)| session)
    }

    /// Append one canonical message and sync it before returning.
    pub fn append(&self, id: &str, message: &Message) -> io::Result<()> {
        let mut file = self.open_file(id)?;
        file.lock()?;
        let (session, committed) = self.read_session(id, &mut file)?;
        let bytes = encode(Body::Message {
            message: message.clone(),
            updated_at_ms: now_ms()?.max(session.metadata.updated_at_ms),
        })?;
        append_bytes(&mut file, committed, &bytes)
    }

    /// Append title/branch metadata without rewriting committed records.
    pub fn update_metadata(
        &self,
        id: &str,
        title: impl Into<String>,
        branch: Option<String>,
    ) -> io::Result<()> {
        let mut file = self.open_file(id)?;
        file.lock()?;
        let (session, committed) = self.read_session(id, &mut file)?;
        let mut metadata = session.metadata;
        metadata.title = title.into();
        metadata.branch = branch;
        metadata.updated_at_ms = now_ms()?.max(metadata.updated_at_ms);
        append_bytes(&mut file, committed, &encode(Body::Metadata { metadata })?)
    }

    /// List this project's sessions, newest metadata time first, then identifier.
    /// Any invalid session fails the operation rather than silently disappearing.
    pub fn list(&self) -> io::Result<Vec<SessionSummary>> {
        self.check_directory()?;
        let mut result = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            if entry.path().extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            let path = entry.path();
            let id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| invalid("non-UTF-8 session identifier"))?;
            let session = self.resume(id)?;
            result.push(SessionSummary {
                id: session.id,
                metadata: session.metadata,
                message_count: session.messages.len(),
                interrupted_bytes: session.interrupted_bytes,
            });
        }
        result.sort_by(|a, b| {
            b.metadata
                .updated_at_ms
                .cmp(&a.metadata.updated_at_ms)
                .then(a.id.cmp(&b.id))
        });
        Ok(result)
    }

    /// Fork exactly the first `message_count` committed messages into a new session.
    /// Reject an out-of-range count; never invent missing messages from a damaged tail.
    pub fn fork(
        &self,
        id: &str,
        message_count: usize,
        title: impl Into<String>,
    ) -> io::Result<Session> {
        let source = self.resume(id)?;
        if message_count > source.messages.len() {
            return Err(invalid("fork count exceeds committed messages"));
        }
        self.create_with_messages(
            title.into(),
            source.metadata.branch,
            Some(id.to_owned()),
            &source.messages[..message_count],
        )
    }

    fn check_directory(&self) -> io::Result<()> {
        reject_symlinks(&self.directory)?;
        check_private(&self.directory, true)
    }

    fn path(&self, id: &str) -> io::Result<PathBuf> {
        if id.is_empty()
            || id.len() > 96
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(invalid("invalid session identifier"));
        }
        Ok(self.directory.join(format!("{id}.jsonl")))
    }

    fn open_file(&self, id: &str) -> io::Result<File> {
        self.check_directory()?;
        let path = self.path(id)?;
        reject_symlinks(&path)?;
        check_private(&path, false)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        no_follow(&mut options);
        let file = options.open(&path)?;
        check_file(&file)?;
        Ok(file)
    }

    fn create_with_messages(
        &self,
        title: String,
        branch: Option<String>,
        forked_from: Option<String>,
        messages: &[Message],
    ) -> io::Result<Session> {
        self.check_directory()?;
        let time = now_ms()?;
        let metadata = Metadata {
            title,
            branch,
            forked_from,
            created_at_ms: time,
            updated_at_ms: time,
        };
        for _ in 0..32 {
            let id = format!(
                "s-{time:x}-{:x}-{:x}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            );
            let path = self.path(&id)?;
            let mut bytes = encode(Body::Header {
                id: id.clone(),
                project: self.project.clone(),
                metadata: metadata.clone(),
            })?;
            for message in messages {
                let record = encode(Body::Message {
                    message: message.clone(),
                    updated_at_ms: time,
                })?;
                if (bytes.len() as u64).saturating_add(record.len() as u64) > MAX_SESSION_BYTES {
                    return Err(invalid("session exceeds byte limit"));
                }
                bytes.extend(record);
            }
            if bytes.len() as u64 > MAX_SESSION_BYTES {
                return Err(invalid("session exceeds byte limit"));
            }
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            private_mode(&mut options);
            let mut file = match options.open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            file.lock()?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            // Ordinary directory handles cannot be opened/synced on Windows.
            // The regular session file is still synchronously committed above.
            #[cfg(unix)]
            File::open(&self.directory)?.sync_all()?;
            return Ok(Session {
                id,
                project: self.project.clone(),
                metadata,
                messages: messages.to_vec(),
                interrupted_bytes: 0,
            });
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "session identifier collisions",
        ))
    }

    fn read_session(&self, id: &str, file: &mut File) -> io::Result<(Session, u64)> {
        if file.metadata()?.len() > MAX_SESSION_BYTES {
            return Err(invalid("session exceeds byte limit"));
        }
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        file.take(MAX_SESSION_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_SESSION_BYTES {
            return Err(invalid("session exceeds byte limit"));
        }
        let committed = bytes
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |index| index + 1);
        let mut session: Option<Session> = None;
        for line in bytes[..committed.saturating_sub(1)]
            .split(|b| *b == b'\n')
            .filter(|_| committed != 0)
        {
            let record: Record =
                serde_json::from_slice(line).map_err(|error| invalid(error.to_string()))?;
            if record.schema_version != SCHEMA_VERSION {
                return Err(invalid("unsupported session schema"));
            }
            match (record.body, session.as_mut()) {
                (
                    Body::Header {
                        id: stored_id,
                        project,
                        metadata,
                    },
                    None,
                ) if stored_id == id && project == self.project => {
                    validate_metadata(&metadata)?;
                    session = Some(Session {
                        id: stored_id,
                        project,
                        metadata,
                        messages: Vec::new(),
                        interrupted_bytes: (bytes.len() - committed) as u64,
                    });
                }
                (
                    Body::Message {
                        message,
                        updated_at_ms,
                    },
                    Some(session),
                ) => {
                    if updated_at_ms < session.metadata.updated_at_ms {
                        return Err(invalid("nonmonotonic message time"));
                    }
                    session.metadata.updated_at_ms = updated_at_ms;
                    session.messages.push(message);
                }
                (Body::Metadata { metadata }, Some(session)) => {
                    validate_metadata(&metadata)?;
                    if metadata.created_at_ms != session.metadata.created_at_ms
                        || metadata.forked_from != session.metadata.forked_from
                        || metadata.updated_at_ms < session.metadata.updated_at_ms
                    {
                        return Err(invalid("invalid metadata transition"));
                    }
                    session.metadata = metadata;
                }
                _ => return Err(invalid("missing, duplicate, or mismatched session header")),
            }
        }
        session
            .map(|session| (session, committed as u64))
            .ok_or_else(|| invalid("no committed session header"))
    }
}

fn validate_metadata(metadata: &Metadata) -> io::Result<()> {
    if metadata.updated_at_ms < metadata.created_at_ms {
        return Err(invalid("invalid metadata times"));
    }
    Ok(())
}

fn append_bytes(file: &mut File, committed: u64, bytes: &[u8]) -> io::Result<()> {
    if committed.saturating_add(bytes.len() as u64) > MAX_SESSION_BYTES {
        return Err(invalid("session exceeds byte limit"));
    }
    file.set_len(committed)?;
    file.seek(SeekFrom::Start(committed))?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn encode(body: Body) -> io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(&Record {
        schema_version: SCHEMA_VERSION,
        body,
    })
    .map_err(|error| invalid(error.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn now_ms() -> io::Result<u64> {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_millis();
    u64::try_from(value).map_err(|_| invalid("timestamp overflow"))
}

fn project_slug(path: &Path) -> String {
    // FNV-1a is deterministic across processes, unlike DefaultHasher. The header
    // also verifies the full canonical path, so a hash collision fails closed.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let label: String = name
        .chars()
        .take(40)
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!(
        "{}-{hash:016x}",
        if label.is_empty() { "project" } else { &label }
    )
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(crate) fn reject_symlinks(path: &Path) -> io::Result<()> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut current = PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(invalid("parent traversal is not allowed"));
        }
        current.push(component);
        // A Windows drive/UNC prefix is not a filesystem entry until RootDir
        // completes it; probing a bare verbatim prefix fails with ERROR_INVALID_FUNCTION.
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(invalid("symlinks are not allowed"))
            }
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn private_directory(path: &Path) -> io::Result<()> {
    reject_symlinks(path)?;
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    match builder.create(path) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    check_private(path, true)
}

fn check_private(path: &Path, directory: bool) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || (if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file()
        })
    {
        return Err(invalid("unexpected storage file type"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let expected = if directory { 0o700 } else { 0o600 };
        if metadata.permissions().mode() & 0o777 != expected {
            return Err(invalid("storage permissions must be private"));
        }
    }
    Ok(())
}

fn check_file(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid("session must be a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.permissions().mode() & 0o777 != 0o600 || metadata.nlink() != 1 {
            return Err(invalid("session must be private and have one link"));
        }
    }
    Ok(())
}

fn private_mode(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    no_follow(options);
}

fn no_follow(options: &mut OpenOptions) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x20000); // O_NOFOLLOW, without adding a libc dependency.
    }
    #[cfg(not(target_os = "linux"))]
    let _ = options;
}
