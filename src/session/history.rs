//! Private prompt history shared across projects and sessions.
use super::store::{check_file, no_follow, private_directory, private_mode, reject_symlinks};
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

fn directory(home: &Path) -> io::Result<PathBuf> {
    reject_symlinks(home)?;
    let dir = home.join(".gitcode");
    private_directory(&dir)?;
    Ok(dir)
}
fn lock(dir: &Path) -> io::Result<File> {
    let path = dir.join("prompt-history.lock");
    reject_symlinks(&path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    private_mode(&mut options);
    let file = options.open(path)?;
    check_file(&file)?;
    file.lock()?;
    Ok(file)
}
fn read_file(dir: &Path) -> io::Result<Option<Vec<String>>> {
    let path = dir.join("prompt-history.jsonl");
    reject_symlinks(&path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    no_follow(&mut options);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    check_file(&file)?;
    let mut entries = VecDeque::new();
    for line in BufReader::new(file).lines() {
        if let Ok(entry) = serde_json::from_str::<String>(&line?) {
            entries.push_back(entry);
            if entries.len() > 100 {
                entries.pop_front();
            }
        }
    }
    Ok(Some(entries.into()))
}
fn save(dir: &Path, entries: &[String]) -> io::Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    for entry in entries {
        serde_json::to_writer(&mut file, entry)?;
        file.write_all(b"\n")?;
    }
    file.flush()?;
    file.persist(dir.join("prompt-history.jsonl"))
        .map_err(|e| e.error)?;
    Ok(())
}
fn legacy(dir: &Path) -> io::Result<Vec<String>> {
    let sessions = dir.join("sessions");
    reject_symlinks(&sessions)?;
    let projects = match fs::read_dir(sessions) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut prompts = Vec::new();
    for project in projects.flatten() {
        let project_path = project.path();
        if !project.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(files) = fs::read_dir(project_path) else {
            continue;
        };
        for entry in files.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") || reject_symlinks(&path).is_err()
            {
                continue;
            }
            let mut options = OpenOptions::new();
            options.read(true);
            no_follow(&mut options);
            let Ok(file) = options.open(path) else {
                continue;
            };
            if check_file(&file).is_err()
                || file
                    .metadata()
                    .is_ok_and(|m| m.len() > super::store::MAX_SESSION_BYTES)
            {
                continue;
            }
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if record["type"] != "message" || record["message"]["role"] != "user" {
                    continue;
                }
                let Some(blocks) = record["message"]["content"].as_array() else {
                    continue;
                };
                if blocks.iter().any(|block| block["type"] == "tool_result") {
                    continue;
                }
                for text in blocks.iter().filter_map(|block| {
                    (block["type"] == "text")
                        .then(|| block["text"].as_str())
                        .flatten()
                }) {
                    if !text.trim().is_empty() && !text.starts_with("[Background subagent ") {
                        prompts.push((
                            record["updated_at_ms"].as_u64().unwrap_or(0),
                            text.to_owned(),
                        ));
                    }
                }
            }
        }
    }
    prompts.sort_by_key(|(time, _)| *time);
    Ok(prompts
        .into_iter()
        .rev()
        .take(100)
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect())
}
/// Read the most recent 100 prompt entries across sessions.
pub fn load(home: &Path) -> io::Result<Vec<String>> {
    let dir = directory(home)?;
    let _lock = lock(&dir)?;
    if let Some(entries) = read_file(&dir)? {
        return Ok(entries);
    }
    let entries = legacy(&dir)?;
    save(&dir, &entries)?;
    Ok(entries)
}
/// Persist one submitted prompt or command, retaining the newest 100.
pub fn append(home: &Path, entry: &str) -> io::Result<()> {
    if entry.is_empty() {
        return Ok(());
    }
    let dir = directory(home)?;
    let _lock = lock(&dir)?;
    let mut entries = match read_file(&dir)? {
        Some(entries) => entries,
        None => legacy(&dir)?,
    };
    entries.push(entry.to_owned());
    if entries.len() > 100 {
        entries.drain(..entries.len() - 100);
    }
    save(&dir, &entries)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn records_commands_multiline_duplicates_and_keeps_last_hundred() {
        let home = tempfile::tempdir().unwrap();
        append(home.path(), "/help").unwrap();
        append(home.path(), "line one\nline two").unwrap();
        assert_eq!(
            load(home.path()).unwrap(),
            vec!["/help", "line one\nline two"]
        );
        for _ in 0..101 {
            append(home.path(), "same").unwrap();
        }
        assert_eq!(load(home.path()).unwrap(), vec!["same"; 100]);
        assert_eq!(
            fs::read_to_string(home.path().join(".gitcode/prompt-history.jsonl"))
                .unwrap()
                .lines()
                .count(),
            100
        );
    }
    #[test]
    fn imports_existing_project_sessions_and_skips_tool_results() {
        use crate::provider::canonical::{Content, Message, Role};
        let home = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let a = crate::session::store::SessionStore::open(home.path(), first.path()).unwrap();
        let b = crate::session::store::SessionStore::open(home.path(), second.path()).unwrap();
        let a_session = a.create("a", None).unwrap();
        let b_session = b.create("b", None).unwrap();
        a.append(&a_session.id, &Message::text(Role::User, "first prompt"))
            .unwrap();
        a.append(
            &a_session.id,
            &Message {
                role: Role::User,
                content: vec![Content::ToolResult {
                    tool_use_id: "t".into(),
                    content: "hidden output".into(),
                    is_error: false,
                }],
            },
        )
        .unwrap();
        b.append(&b_session.id, &Message::text(Role::User, "second prompt"))
            .unwrap();
        let entries = load(home.path()).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.contains(&"first prompt".into()));
        assert!(entries.contains(&"second prompt".into()));
    }
}
