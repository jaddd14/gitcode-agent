//! Workspace mutations with byte-based read guards and staged atomic replacement.
//!
//! Integration: export this module from `tools`, then call the mutation executor from a
//! blocking worker, serializing mutations and returning the updated read state.
//! Permission checks belong here as well as in any interactive approval layer.

use crate::perm::{Decision, Engine};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions, Permissions},
    io::Write,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Bytes most recently observed by Read, indexed by canonical absolute path.
pub type ReadState = BTreeMap<PathBuf, Vec<u8>>;

/// One committed change, suitable for diff rendering and per-turn tracking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Canonical workspace path.
    pub path: PathBuf,
    /// Previous bytes; absent for a new file.
    pub before: Option<Vec<u8>>,
    /// New bytes; absent for deletion.
    pub after: Option<Vec<u8>>,
}

/// An exact replacement in an ordered MultiEdit operation.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replacement {
    /// Literal text to find, without line-number prefixes.
    pub old_string: String,
    /// Literal replacement, normalized to the file's newline convention.
    pub new_string: String,
    /// Replace every non-overlapping match rather than requiring uniqueness.
    #[serde(default)]
    pub replace_all: bool,
}

/// Execute Write, Edit, MultiEdit, NotebookEdit or Codex apply_patch.
///
/// This function performs blocking filesystem IO. Existing files must have been
/// read, and their current bytes must equal the saved bytes. Successful writes
/// refresh that state; failed validation leaves every file unchanged. Multi-file
/// commits use individual atomic renames and best-effort rollback, not a global
/// filesystem transaction. No API here grants approval for `Decision::Ask`.
pub fn execute(
    permissions: &Engine,
    read_state: &mut ReadState,
    name: &str,
    input: &Value,
) -> Result<Vec<Change>, String> {
    let mut changes = Vec::new();
    match name {
        "apply_patch" => {
            let patch = input["patch"]
                .as_str()
                .or_else(|| input["input"].as_str())
                .ok_or("missing string argument: patch")?;
            for operation in parse_patch(patch)? {
                plan_patch(permissions, read_state, operation, &mut changes)?;
            }
        }
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            let raw = input["path"]
                .as_str()
                .or_else(|| input["file_path"].as_str())
                .or_else(|| input["notebook_path"].as_str())
                .ok_or("missing string argument: path")?;
            let path = resolve(permissions, name, raw)?;
            let before = observed(&path, read_state)?;
            let after = match name {
                "Write" => {
                    let content = string(input, "content")?;
                    let content = match &before {
                        Some(bytes) => normalize_newlines(content, text(bytes)?),
                        None => content.to_owned(),
                    };
                    content.into_bytes()
                }
                "Edit" | "MultiEdit" => {
                    let bytes = before.as_ref().ok_or("cannot edit a missing file")?;
                    let mut content = text(bytes)?.to_owned();
                    let edits = if name == "MultiEdit" {
                        serde_json::from_value::<Vec<Replacement>>(input["edits"].clone())
                            .map_err(|e| format!("invalid edits: {e}"))?
                    } else {
                        vec![Replacement {
                            old_string: string(input, "old_string")?.into(),
                            new_string: string(input, "new_string")?.into(),
                            replace_all: boolean(input, "replace_all")?,
                        }]
                    };
                    if edits.is_empty() {
                        return Err("edits must not be empty".into());
                    }
                    for (index, edit) in edits.iter().enumerate() {
                        content = replace(&content, edit)
                            .map_err(|e| format!("edit {}: {e}", index + 1))?;
                    }
                    content.into_bytes()
                }
                _ => notebook(before.as_deref().ok_or("notebook does not exist")?, input)?,
            };
            changes.push(Change {
                path,
                before,
                after: Some(after),
            });
        }
        _ => return Err("unknown editing tool".into()),
    }
    commit(&changes, read_state)?;
    Ok(changes)
}

fn string<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input[key]
        .as_str()
        .ok_or_else(|| format!("missing string argument: {key}"))
}

fn boolean(input: &Value, key: &str) -> Result<bool, String> {
    match input.get(key) {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("{key} must be boolean")),
    }
}

fn resolve(engine: &Engine, tool: &str, raw: &str) -> Result<PathBuf, String> {
    let requested = Path::new(raw);
    if raw.is_empty()
        || requested
            .components()
            .any(|c| matches!(c, Component::ParentDir))
    {
        return Err("empty path or parent traversal is not allowed".into());
    }
    let absolute = if requested.is_absolute() {
        requested.to_owned()
    } else {
        engine.root.join(requested)
    };
    let mut cursor = absolute.clone();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err("symlink mutation is not allowed".into());
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(cursor.file_name().ok_or("invalid path")?.to_owned());
                if !cursor.pop() {
                    return Err("cannot resolve path".into());
                }
            }
            Err(error) => return Err(format!("cannot inspect path: {error}")),
        }
    }
    let mut path = cursor
        .canonicalize()
        .map_err(|e| format!("cannot resolve path: {e}"))?;
    for part in missing.into_iter().rev() {
        path.push(part);
    }
    if !path.starts_with(&engine.root) || path == engine.root {
        return Err("path is outside workspace or is workspace root".into());
    }
    let relative = path
        .strip_prefix(&engine.root)
        .map_err(|_| "invalid workspace path")?
        .to_string_lossy();
    // Check both spellings: absolute callers must not bypass relative deny rules.
    for resource in [
        raw,
        relative.as_ref(),
        path.to_str().ok_or("path is not UTF-8")?,
    ] {
        match engine.decide(tool, resource, true) {
            Decision::Allow => {}
            Decision::Ask => return Err("permission requires explicit approval".into()),
            Decision::Deny => return Err("permission denied by policy".into()),
        }
    }
    Ok(path)
}

fn observed(path: &Path, state: &ReadState) -> Result<Option<Vec<u8>>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err("target is not a regular file".into());
            }
            if metadata.len() > 16 * 1024 * 1024 {
                return Err("file exceeds 16 MiB editing limit".into());
            }
            let bytes = fs::read(path).map_err(|e| format!("cannot read target: {e}"))?;
            let saved = state.get(path).ok_or("Read the file before modifying it")?;
            if saved != &bytes {
                return Err("file changed externally; Read it again before editing".into());
            }
            Ok(Some(bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if state.contains_key(path) {
                Err("file was deleted externally; refresh Read state".into())
            } else {
                Ok(None)
            }
        }
        Err(error) => Err(format!("cannot inspect target: {error}")),
    }
}

fn text(bytes: &[u8]) -> Result<&str, String> {
    if bytes.contains(&0) {
        return Err("binary files cannot be edited as text".into());
    }
    std::str::from_utf8(bytes).map_err(|_| "file is not UTF-8 text".into())
}

fn normalize_newlines(value: &str, original: &str) -> String {
    let lf = value.replace("\r\n", "\n");
    if original.contains("\r\n") {
        lf.replace('\n', "\r\n")
    } else {
        lf
    }
}

fn replace(content: &str, edit: &Replacement) -> Result<String, String> {
    if edit.old_string.is_empty() {
        return Err("old_string must not be empty; use Write to create content".into());
    }
    let count = content.match_indices(&edit.old_string).count();
    if count == 0 {
        let needle = edit.old_string.lines().next().unwrap_or_default().trim();
        let nearest = content
            .lines()
            .enumerate()
            .min_by_key(|(_, line)| distance(line.trim(), needle));
        let hint = nearest
            .map(|(index, line)| {
                format!(
                    "; nearest line {}: {:?}",
                    index + 1,
                    line.chars().take(160).collect::<String>()
                )
            })
            .unwrap_or_default();
        return Err(format!(
            "old_string not found (matching is whitespace-exact){hint}"
        ));
    }
    if count > 1 && !edit.replace_all {
        return Err(format!(
            "old_string matches {count} times; provide more context or replace_all"
        ));
    }
    let replacement = normalize_newlines(&edit.new_string, content);
    if edit.old_string == replacement {
        return Err("replacement would not change the file".into());
    }
    Ok(if edit.replace_all {
        content.replace(&edit.old_string, &replacement)
    } else {
        content.replacen(&edit.old_string, &replacement, 1)
    })
}

fn distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().take(200).collect();
    let mut row: Vec<usize> = (0..=right.len()).collect();
    for (i, a) in left.chars().take(200).enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, b) in right.iter().enumerate() {
            let old = row[j + 1];
            row[j + 1] = (row[j] + 1)
                .min(old + 1)
                .min(diagonal + usize::from(a != *b));
            diagonal = old;
        }
    }
    row[right.len()]
}

fn notebook(bytes: &[u8], input: &Value) -> Result<Vec<u8>, String> {
    let original = text(bytes)?;
    let mut document: Value =
        serde_json::from_str(original).map_err(|e| format!("invalid notebook JSON: {e}"))?;
    let cells = document["cells"]
        .as_array_mut()
        .ok_or("notebook cells must be an array")?;
    let mode = input["edit_mode"].as_str().unwrap_or("replace");
    let selected = if let Some(id) = input["cell_id"].as_str() {
        Some(
            cells
                .iter()
                .position(|cell| cell["id"].as_str() == Some(id))
                .ok_or("cell_id not found")?,
        )
    } else if let Some(index) = input.get("cell_number") {
        Some(
            usize::try_from(
                index
                    .as_u64()
                    .ok_or("cell_number must be a zero-based integer")?,
            )
            .map_err(|_| "cell_number too large")?,
        )
    } else {
        None
    };
    let index = selected.unwrap_or(cells.len());
    match mode {
        "delete" => {
            if index >= cells.len() {
                return Err("cell index out of range".into());
            }
            cells.remove(index);
        }
        "replace" | "insert" => {
            let source = string(input, "new_source")?;
            let source: Vec<Value> = source
                .split_inclusive('\n')
                .map(|s| Value::String(s.into()))
                .collect();
            if mode == "replace" {
                let cell = cells.get_mut(index).ok_or("cell index out of range")?;
                let kind = input["cell_type"]
                    .as_str()
                    .or_else(|| cell["cell_type"].as_str())
                    .ok_or("missing cell_type")?
                    .to_owned();
                validate_cell_type(&kind)?;
                cell["cell_type"] = kind.clone().into();
                cell["source"] = source.into();
                reset_outputs(cell, &kind);
            } else {
                let kind = string(input, "cell_type")?;
                validate_cell_type(kind)?;
                let position = if input["cell_id"].is_string() {
                    index + 1
                } else {
                    index
                };
                if position > cells.len() {
                    return Err("cell index out of range".into());
                }
                let mut cell = serde_json::json!({"cell_type":kind,"metadata":{},"source":source});
                if document_minor(bytes)? >= 5 {
                    cell["id"] = format!(
                        "gitcode-{:x}-{:x}",
                        std::process::id(),
                        NEXT.fetch_add(1, Ordering::Relaxed)
                    )
                    .into();
                }
                reset_outputs(&mut cell, kind);
                cells.insert(position, cell);
            }
        }
        _ => return Err("edit_mode must be replace, insert or delete".into()),
    }
    let mut output = serde_json::to_string_pretty(&document)
        .map_err(|e| format!("cannot serialize notebook: {e}"))?;
    if original.ends_with('\n') {
        output.push('\n');
    }
    Ok(normalize_newlines(&output, original).into_bytes())
}

fn document_minor(bytes: &[u8]) -> Result<u64, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    Ok(value["nbformat_minor"].as_u64().unwrap_or(0))
}
fn validate_cell_type(kind: &str) -> Result<(), String> {
    if matches!(kind, "code" | "markdown" | "raw") {
        Ok(())
    } else {
        Err("cell_type must be code, markdown or raw".into())
    }
}
fn reset_outputs(cell: &mut Value, kind: &str) {
    if kind == "code" {
        cell["outputs"] = serde_json::json!([]);
        cell["execution_count"] = Value::Null;
    } else if let Some(object) = cell.as_object_mut() {
        object.remove("outputs");
        object.remove("execution_count");
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Staged(PathBuf);
impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn stage(path: &Path, bytes: &[u8], mode: Option<Permissions>) -> Result<Staged, String> {
    let parent = path.parent().ok_or("target has no parent")?;
    fs::create_dir_all(parent).map_err(|e| format!("cannot create parent directories: {e}"))?;
    for _ in 0..100 {
        let temp = parent.join(format!(
            ".gitcode-edit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temp) {
            Ok(mut file) => {
                let staged = Staged(temp);
                file.write_all(bytes)
                    .map_err(|e| format!("cannot stage file: {e}"))?;
                if let Some(mode) = mode {
                    file.set_permissions(mode)
                        .map_err(|e| format!("cannot preserve permissions: {e}"))?;
                }
                file.sync_all()
                    .map_err(|e| format!("cannot sync staged file: {e}"))?;
                return Ok(staged);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("cannot create staged file: {error}")),
        }
    }
    Err("cannot allocate a unique temporary file".into())
}

fn verify(change: &Change) -> Result<(), String> {
    let current = match fs::symlink_metadata(&change.path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
            Some(fs::read(&change.path).map_err(|e| e.to_string())?)
        }
        Ok(_) => return Err("target became a symlink or non-file".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    if current != change.before {
        return Err("file changed externally during editing; Read it again".into());
    }
    Ok(())
}

fn commit(changes: &[Change], state: &mut ReadState) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for change in changes {
        if !seen.insert(&change.path) {
            return Err("patch touches the same path more than once".into());
        }
        verify(change)?;
    }
    let mut staged = Vec::new();
    let mut backups = Vec::new();
    for change in changes {
        let mode = if change.before.is_some() {
            Some(
                fs::metadata(&change.path)
                    .map_err(|e| e.to_string())?
                    .permissions(),
            )
        } else {
            None
        };
        backups.push(
            change
                .before
                .as_ref()
                .map(|bytes| stage(&change.path, bytes, mode.clone()))
                .transpose()?,
        );
        staged.push(
            change
                .after
                .as_ref()
                .map(|bytes| stage(&change.path, bytes, mode))
                .transpose()?,
        );
    }
    for change in changes {
        verify(change)?;
    }
    for (index, change) in changes.iter().enumerate() {
        let result = if let Some(staged) = &staged[index] {
            fs::rename(&staged.0, &change.path)
        } else {
            fs::remove_file(&change.path)
        };
        if let Err(error) = result {
            let mut rollback_errors = Vec::new();
            for previous in (0..index).rev() {
                let restored = if let Some(backup) = &backups[previous] {
                    fs::rename(&backup.0, &changes[previous].path)
                } else {
                    fs::remove_file(&changes[previous].path)
                };
                if let Err(e) = restored {
                    rollback_errors.push(e.to_string());
                }
            }
            return Err(format!(
                "commit failed: {error}; rollback errors: {rollback_errors:?}"
            ));
        }
    }
    for change in changes {
        if let Some(bytes) = &change.after {
            state.insert(change.path.clone(), bytes.clone());
        } else {
            state.remove(&change.path);
        }
        if let Some(parent) = change.path.parent() {
            if let Ok(directory) = File::open(parent) {
                let _ = directory.sync_all();
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
enum Patch {
    Add(String, String),
    Delete(String),
    Update {
        path: String,
        destination: Option<String>,
        hunks: Vec<Hunk>,
    },
}
#[derive(Debug, Default)]
struct Hunk {
    anchor: Option<String>,
    lines: Vec<(char, String)>,
    eof: bool,
}

fn parse_patch(patch: &str) -> Result<Vec<Patch>, String> {
    let lines: Vec<&str> = patch.lines().collect();
    if lines.first() != Some(&"*** Begin Patch") || lines.last() != Some(&"*** End Patch") {
        return Err("patch must start with *** Begin Patch and end with *** End Patch".into());
    }
    let mut result = Vec::new();
    let mut i = 1;
    while i + 1 < lines.len() {
        let header = lines[i];
        i += 1;
        if let Some(path) = header.strip_prefix("*** Add File: ") {
            let mut content = String::new();
            while i + 1 < lines.len() && !lines[i].starts_with("*** ") {
                let line = lines[i].strip_prefix('+').ok_or_else(|| {
                    format!("patch line {}: Add File lines must start with +", i + 1)
                })?;
                content.push_str(line);
                content.push('\n');
                i += 1;
            }
            result.push(Patch::Add(path.into(), content));
        } else if let Some(path) = header.strip_prefix("*** Delete File: ") {
            result.push(Patch::Delete(path.into()));
        } else if let Some(path) = header.strip_prefix("*** Update File: ") {
            let destination = lines
                .get(i)
                .and_then(|line| line.strip_prefix("*** Move to: "))
                .map(str::to_owned);
            if destination.is_some() {
                i += 1;
            }
            let mut hunks = Vec::new();
            while i + 1 < lines.len()
                && (!lines[i].starts_with("*** ") || lines[i] == "*** End of File")
            {
                let mut hunk = Hunk::default();
                if lines[i] == "@@" {
                    i += 1;
                } else if let Some(anchor) = lines[i].strip_prefix("@@ ") {
                    hunk.anchor = Some(anchor.into());
                    i += 1;
                } else if !hunks.is_empty() {
                    return Err(format!("patch line {}: expected @@ hunk", i + 1));
                }
                while i + 1 < lines.len()
                    && !lines[i].starts_with("@@")
                    && !lines[i].starts_with("*** ")
                {
                    let line = lines[i];
                    let (kind, value) = if line.is_empty() {
                        (' ', "")
                    } else {
                        {
                            let kind = line.chars().next().ok_or("empty patch line")?;
                            (kind, &line[kind.len_utf8()..])
                        }
                    };
                    if !matches!(kind, ' ' | '+' | '-') {
                        return Err(format!("patch line {}: expected space, + or -", i + 1));
                    }
                    hunk.lines.push((kind, value.into()));
                    i += 1;
                }
                if lines.get(i) == Some(&"*** End of File") {
                    hunk.eof = true;
                    i += 1;
                }
                if hunk.lines.is_empty() {
                    return Err("empty update hunk".into());
                }
                hunks.push(hunk);
            }
            if hunks.is_empty() {
                return Err("Update File requires a hunk".into());
            }
            result.push(Patch::Update {
                path: path.into(),
                destination,
                hunks,
            });
        } else {
            return Err(format!("patch line {}: unrecognized file directive", i));
        }
    }
    if result.is_empty() {
        return Err("patch contains no file operations".into());
    }
    Ok(result)
}

fn plan_patch(
    engine: &Engine,
    state: &ReadState,
    operation: Patch,
    changes: &mut Vec<Change>,
) -> Result<(), String> {
    match operation {
        Patch::Add(raw, content) => {
            let path = resolve(engine, "apply_patch", &raw)?;
            let before = observed(&path, state)?;
            if before.is_some() {
                return Err(format!("Add File target already exists: {raw}"));
            }
            changes.push(Change {
                path,
                before,
                after: Some(content.into_bytes()),
            });
        }
        Patch::Delete(raw) => {
            let path = resolve(engine, "apply_patch", &raw)?;
            let before = Some(
                observed(&path, state)?
                    .ok_or_else(|| format!("Delete File target is missing: {raw}"))?,
            );
            changes.push(Change {
                path,
                before,
                after: None,
            });
        }
        Patch::Update {
            path: raw,
            destination,
            hunks,
        } => {
            let path = resolve(engine, "apply_patch", &raw)?;
            let before = observed(&path, state)?
                .ok_or_else(|| format!("Update File target is missing: {raw}"))?;
            let after = apply_hunks(text(&before)?, &hunks)?.into_bytes();
            if let Some(destination) = destination {
                let target = resolve(engine, "apply_patch", &destination)?;
                if target == path {
                    return Err("Move to must name a different path".into());
                }
                if observed(&target, state)?.is_some() {
                    return Err("Move to target already exists".into());
                }
                changes.push(Change {
                    path,
                    before: Some(before),
                    after: None,
                });
                changes.push(Change {
                    path: target,
                    before: None,
                    after: Some(after),
                });
            } else {
                changes.push(Change {
                    path,
                    before: Some(before),
                    after: Some(after),
                });
            }
        }
    }
    Ok(())
}

fn fuzzy(value: &str, level: usize) -> String {
    match level {
        0 => value.into(),
        1 => value.trim_end().into(),
        2 => value.trim().into(),
        _ => value
            .trim()
            .chars()
            .map(|c| match c {
                '‘' | '’' => '\'',
                '“' | '”' => '"',
                '–' | '—' | '−' => '-',
                '\u{a0}' => ' ',
                _ => c,
            })
            .collect(),
    }
}

fn locate(lines: &[String], needle: &[String], start: usize, eof: bool) -> Result<usize, String> {
    if needle.is_empty() {
        return Ok(if eof { lines.len() } else { start });
    }
    if needle.len() > lines.len() || start > lines.len() - needle.len() {
        return Err("patch context not found".into());
    }
    for level in 0..4 {
        let matches: Vec<usize> = (start..=lines.len() - needle.len())
            .filter(|&position| {
                (!eof || position + needle.len() == lines.len())
                    && needle.iter().enumerate().all(|(offset, expected)| {
                        fuzzy(&lines[position + offset], level) == fuzzy(expected, level)
                    })
            })
            .collect();
        if let Some(position) = matches.first() {
            return Ok(*position);
        }
    }
    Err(format!(
        "patch context not found after line {}; expected {:?}",
        start,
        needle.first()
    ))
}

fn apply_hunks(original: &str, hunks: &[Hunk]) -> Result<String, String> {
    let normalized = original.replace("\r\n", "\n");
    let mut lines: Vec<String> = normalized
        .split_terminator('\n')
        .map(str::to_owned)
        .collect();
    let mut cursor = 0;
    for (index, hunk) in hunks.iter().enumerate() {
        if let Some(anchor) = &hunk.anchor {
            cursor = locate(&lines, std::slice::from_ref(anchor), cursor, false)
                .map_err(|e| format!("hunk {} anchor: {e}", index + 1))?
                + 1;
        }
        let old: Vec<String> = hunk
            .lines
            .iter()
            .filter(|(kind, _)| *kind != '+')
            .map(|(_, line)| line.clone())
            .collect();
        let position = locate(&lines, &old, cursor, hunk.eof || old.is_empty())
            .map_err(|e| format!("hunk {}: {e}", index + 1))?;
        let mut replacement = Vec::new();
        let mut offset = 0;
        for (kind, value) in &hunk.lines {
            match kind {
                ' ' => {
                    replacement.push(lines[position + offset].clone());
                    offset += 1;
                }
                '-' => offset += 1,
                '+' => replacement.push(value.clone()),
                _ => return Err("invalid hunk kind".into()),
            }
        }
        cursor = position + replacement.len();
        lines.splice(position..position + old.len(), replacement);
    }
    let mut output = lines.join("\n");
    if original.ends_with('\n') && !lines.is_empty() {
        output.push('\n');
    }
    Ok(normalize_newlines(&output, original))
}
