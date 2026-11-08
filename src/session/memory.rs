//! Blocking, confined hierarchical instruction loading; use `spawn_blocking`.
//!
//! Load one global file in the explicit home, then one file at each directory
//! from project root to working directory. Prefer GITCODE.md, then CLAUDE.md,
//! then AGENTS.md. Standalone `@relative/path` lines are expanded in place.
//! Global imports stay in home; project imports stay in project. No shell,
//! environment, tilde, URL or glob expansion is performed.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

/// Total source bytes, including all imported files.
pub const MAX_MEMORY_BYTES: u64 = 256 * 1024;
/// Maximum number of files visited (repeated imports count again).
pub const MAX_MEMORY_FILES: usize = 128;
/// Maximum import nesting, including the entry file.
pub const MAX_IMPORT_DEPTH: usize = 32;

/// A deterministic expanded instruction set.
#[derive(Debug, Clone)]
pub struct Memory {
    /// Expanded text, global first and nearest directory last.
    pub text: String,
    /// Source files in traversal order; duplicates are intentional.
    pub sources: Vec<PathBuf>,
}

/// Load instructions without accessing any implicit real home or external imports.
/// `cwd` must be inside `project`; all three arguments must exist as directories.
/// Missing entry files are optional; missing explicit imports are errors.
pub fn load(
    home: impl AsRef<Path>,
    project: impl AsRef<Path>,
    cwd: impl AsRef<Path>,
) -> io::Result<Memory> {
    let home = directory(home.as_ref())?;
    let project = directory(project.as_ref())?;
    let cwd = directory(cwd.as_ref())?;
    let relative = cwd
        .strip_prefix(&project)
        .map_err(|_| invalid("working directory is outside project"))?;
    let mut loader = Loader {
        memory: Memory {
            text: String::new(),
            sources: Vec::new(),
        },
        bytes: 0,
        active: HashSet::new(),
    };
    if let Some(path) = entry(&home)? {
        loader.expand(&path, &home, 0)?;
    }
    let mut current = project.clone();
    if current != home {
        if let Some(path) = entry(&current)? {
            loader.expand(&path, &project, 0)?;
        }
    }
    for component in relative.components() {
        current.push(component);
        if current == home {
            continue;
        }
        if let Some(path) = entry(&current)? {
            loader.expand(&path, &project, 0)?;
        }
    }
    Ok(loader.memory)
}

struct Loader {
    memory: Memory,
    bytes: u64,
    active: HashSet<PathBuf>,
}

impl Loader {
    fn expand(&mut self, path: &Path, boundary: &Path, depth: usize) -> io::Result<()> {
        if depth >= MAX_IMPORT_DEPTH || self.memory.sources.len() >= MAX_MEMORY_FILES {
            return Err(invalid("memory import depth or file limit exceeded"));
        }
        reject_symlinks(path)?;
        let path = fs::canonicalize(path)?;
        if !path.starts_with(boundary) {
            return Err(invalid("memory import escapes its boundary"));
        }
        if !self.active.insert(path.clone()) {
            return Err(invalid("memory import cycle"));
        }
        let metadata = fs::metadata(&path)?;
        if !metadata.is_file() || self.bytes.saturating_add(metadata.len()) > MAX_MEMORY_BYTES {
            return Err(invalid(
                "memory source exceeds size limit or is not a regular file",
            ));
        }
        let remaining = MAX_MEMORY_BYTES - self.bytes;
        let mut bytes = Vec::new();
        File::open(&path)?
            .take(remaining + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > remaining {
            return Err(invalid("memory source exceeds size limit"));
        }
        self.bytes += bytes.len() as u64;
        let text =
            std::str::from_utf8(&bytes).map_err(|_| invalid("memory source must be UTF-8"))?;
        self.memory.sources.push(path.clone());
        let parent = path
            .parent()
            .ok_or_else(|| invalid("missing import parent"))?;
        for line in text.lines() {
            let trimmed = line.trim();
            if let Some(import) = trimmed.strip_prefix('@') {
                if import.is_empty() || import.chars().any(char::is_whitespace) {
                    return Err(invalid(
                        "import must be a standalone relative path without whitespace",
                    ));
                }
                let import = Path::new(import);
                if import.is_absolute() {
                    return Err(invalid("absolute memory imports are not allowed"));
                }
                let imported = normalize(&parent.join(import))?;
                if !imported.starts_with(boundary) {
                    return Err(invalid("memory import escapes its boundary"));
                }
                self.expand(&imported, boundary, depth + 1)?;
            } else {
                self.memory.text.push_str(line);
                self.memory.text.push('\n');
                if self.memory.text.len() as u64 > MAX_MEMORY_BYTES {
                    return Err(invalid("expanded memory exceeds size limit"));
                }
            }
        }
        self.active.remove(&path);
        Ok(())
    }
}

fn entry(directory: &Path) -> io::Result<Option<PathBuf>> {
    for name in ["GITCODE.md", "CLAUDE.md", "AGENTS.md"] {
        let path = directory.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(invalid(
                    "memory entry must be a regular file, not a symlink",
                ))
            }
            Ok(_) => return Ok(Some(path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

fn directory(path: &Path) -> io::Result<PathBuf> {
    reject_symlinks(path)?;
    let path = fs::canonicalize(path)?;
    if !path.is_dir() {
        return Err(invalid("expected directory"));
    }
    Ok(path)
}

fn normalize(path: &Path) -> io::Result<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !result.pop() {
                    return Err(invalid("invalid import traversal"));
                }
            }
            Component::CurDir => (),
            other => result.push(other),
        }
    }
    Ok(result)
}

fn reject_symlinks(path: &Path) -> io::Result<()> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut current = PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(invalid("parent traversal must be normalized"));
        }
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(invalid("memory symlinks are not allowed"))
            }
            Ok(_) => (),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
