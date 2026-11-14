//! Import conversation history and preserve the original Claude Code or Codex journal.
use super::store::{Session, SessionStore};

const MAX_SOURCE_BYTES: u64 = 128 * 1024 * 1024;
use crate::provider::canonical::{Content, Message, Role};
use base64::Engine;
use serde_json::Value;
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
};

/// Source whose sessions should be imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Claude Code sessions.
    Claude,
    /// Codex sessions, including archived ones.
    Codex,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }
}

/// Counts and diagnostics from a bulk import.
#[derive(Debug, Default)]
pub struct ImportReport {
    /// Number of newly imported sessions.
    pub imported: usize,
    /// Number of sessions already present in the current project.
    pub skipped: usize,
    /// Existing default titles corrected from the source.
    pub renamed: usize,
    /// Number of source files that could not be imported.
    pub failed: usize,
    /// Examples of failed files and errors.
    pub errors: Vec<String>,
}

/// Parsed external conversation, with its original journal retained byte for byte.
#[derive(Debug)]
pub struct Conversation {
    /// Source name.
    pub source: &'static str,
    /// Original stable session identifier, when available.
    pub source_id: Option<String>,
    /// Original human-readable session title, when available.
    pub title: Option<String>,
    title_is_custom: bool,
    /// Project recorded by the external agent, if available.
    pub project: Option<PathBuf>,
    /// Conversation messages.
    pub messages: Vec<Message>,
    /// Exact source journal, including metadata and unsupported blocks.
    pub original: Vec<u8>,
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Read a single Claude Code or Codex JSONL file with bounded memory.
pub fn read(path: &Path) -> io::Result<Conversation> {
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("Ожидался файл сессии JSONL"));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(invalid("Файл сессии слишком большой (максимум 128 МиБ)"));
    }
    let mut source = None;
    let mut source_id = None;
    let mut custom_title: Option<String> = None;
    let mut auto_title: Option<String> = None;
    let mut project = None;
    let mut messages = Vec::new();
    for line in bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
    {
        let record: Value =
            serde_json::from_slice(line).map_err(|_| invalid("Повреждённый JSONL файл"))?;
        let kind = record.get("type").and_then(Value::as_str).unwrap_or("");
        if source_id.is_none() {
            source_id = record
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if kind == "custom-title" {
            custom_title = record
                .get("customTitle")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .or(custom_title);
        } else if kind == "ai-title" {
            auto_title = record
                .get("aiTitle")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .or(auto_title);
        }
        if kind == "session_meta" {
            source_id = record
                .pointer("/payload/id")
                .and_then(Value::as_str)
                .or_else(|| {
                    record
                        .pointer("/payload/session_id")
                        .and_then(Value::as_str)
                })
                .map(str::to_owned)
                .or(source_id);
            source = Some("Codex");
            project = record
                .pointer("/payload/cwd")
                .and_then(Value::as_str)
                .map(PathBuf::from);
        }
        if kind == "attachment"
            && source != Some("Codex")
            && record.get("isSidechain").and_then(Value::as_bool) != Some(true)
        {
            if let Some(attachment) = record.get("attachment") {
                add_claude_attachment(&mut messages, attachment);
            }
        }
        if (kind == "user" || kind == "assistant") && record.get("message").is_some() {
            if source == Some("Codex") {
                continue;
            }
            source = Some("Claude Code");
            if project.is_none() {
                project = record.get("cwd").and_then(Value::as_str).map(PathBuf::from);
            }
            if record.get("isSidechain").and_then(Value::as_bool) == Some(true)
                || record.get("isMeta").and_then(Value::as_bool) == Some(true)
            {
                continue;
            }
            let role = if kind == "user" {
                Role::User
            } else {
                Role::Assistant
            };
            if let Some(content) = record.pointer("/message/content") {
                add_content(&mut messages, role, content);
            }
        } else if kind == "response_item"
            && record.pointer("/payload/type").and_then(Value::as_str) == Some("message")
        {
            source = Some("Codex");
            let role = match record.pointer("/payload/role").and_then(Value::as_str) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => continue,
            };
            if let Some(content) = record.pointer("/payload/content") {
                add_content(&mut messages, role, content);
            }
        } else if kind == "response_item" {
            let payload = &record["payload"];
            source = Some("Codex");
            let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
            let block = match kind {
                "function_call" | "custom_tool_call" | "local_shell_call" => {
                    let id = call_id(payload)
                        .unwrap_or_else(|| format!("import-call-{}", messages.len()));
                    let name = payload.get("name").and_then(Value::as_str).unwrap_or(
                        if kind == "local_shell_call" {
                            "local_shell"
                        } else {
                            "custom_tool"
                        },
                    );
                    let input = if kind == "local_shell_call" {
                        payload.get("action").cloned().unwrap_or(Value::Null)
                    } else if kind == "custom_tool_call" {
                        serde_json::json!({"input": payload.get("input").cloned().unwrap_or(Value::Null)})
                    } else {
                        match payload.get("arguments") {
                            Some(Value::String(text)) => serde_json::from_str(text)
                                .unwrap_or_else(|_| serde_json::json!({"arguments": text})),
                            Some(value) => value.clone(),
                            None => serde_json::json!({}),
                        }
                    };
                    Some((
                        Role::Assistant,
                        Content::ToolUse {
                            id,
                            name: name.into(),
                            input,
                        },
                    ))
                }
                "function_call_output" | "custom_tool_call_output" | "local_shell_call_output" => {
                    call_id(payload).map(|tool_use_id| {
                        (
                            Role::User,
                            Content::ToolResult {
                                tool_use_id,
                                content: result_text(&payload["output"]),
                                is_error: payload
                                    .get("is_error")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                            },
                        )
                    })
                }
                "reasoning" => Some((
                    Role::Assistant,
                    Content::Thinking {
                        thinking: result_text(&payload["summary"]),
                        signature: None,
                        encrypted_content: payload
                            .get("encrypted_content")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    },
                )),
                _ => None,
            };
            if let Some((role, block)) = block {
                messages.push(Message {
                    role,
                    content: vec![block],
                });
            }
        }
    }
    let source =
        source.ok_or_else(|| invalid("Не удалось определить формат Claude Code или Codex"))?;
    if messages.is_empty() {
        return Err(invalid("В файле нет текстовых сообщений"));
    }
    let title_is_custom = custom_title.is_some();
    Ok(Conversation {
        source,
        source_id,
        title: custom_title.or(auto_title),
        title_is_custom,
        project,
        messages,
        original: bytes,
    })
}

fn call_id(payload: &Value) -> Option<String> {
    payload
        .get("call_id")
        .or_else(|| payload.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn result_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| block.to_string())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => value.to_string(),
    }
}

fn add_claude_attachment(messages: &mut Vec<Message>, attachment: &Value) {
    let kind = attachment.get("type").and_then(Value::as_str).unwrap_or("");
    let mut entries = Vec::new();
    match kind {
        "file" => {
            let filename = attachment
                .get("filename")
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(text) = attachment
                .pointer("/content/file/content")
                .and_then(Value::as_str)
            {
                entries.push(format!("[Файл из Claude Code: {filename}]\n{text}"));
            }
        }
        "edited_text_file" => {
            let filename = attachment
                .get("filename")
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(text) = attachment.get("snippet").and_then(Value::as_str) {
                entries.push(format!(
                    "[Изменённый файл из Claude Code: {filename}]\n{text}"
                ));
            }
        }
        "instructions" => {
            if let Some(files) = attachment.get("files").and_then(Value::as_array) {
                for file in files {
                    if let Some(text) = file.get("content").and_then(Value::as_str) {
                        let path = file.get("path").and_then(Value::as_str).unwrap_or("");
                        entries.push(format!(
                            "[Инструкции проекта из Claude Code: {path}]\n{text}"
                        ));
                    }
                }
            }
        }
        _ => (),
    }
    for entry in entries {
        messages.push(Message::text(Role::User, entry));
    }
}

fn add_content(messages: &mut Vec<Message>, role: Role, value: &Value) {
    let mut content = Vec::new();
    collect_content(&mut content, value);
    if !content.is_empty() {
        messages.push(Message { role, content });
    }
}

fn collect_content(content: &mut Vec<Content>, value: &Value) {
    if let Some(text) = value.as_str().filter(|text| !text.trim().is_empty()) {
        content.push(Content::Text {
            text: text.to_owned(),
        });
    }
    let Some(blocks) = value.as_array() else {
        return;
    };
    for block in blocks {
        let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "text" | "input_text" | "output_text" => {
                if let Some(text) = block.get("text") {
                    collect_content(content, text);
                }
            }
            "thinking" => {
                content.push(Content::Thinking {
                    thinking: block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                    signature: block
                        .get("signature")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    encrypted_content: None,
                });
            }
            "tool_use" | "server_tool_use" => {
                if let (Some(id), Some(name)) = (
                    block.get("id").and_then(Value::as_str),
                    block.get("name").and_then(Value::as_str),
                ) {
                    content.push(Content::ToolUse {
                        id: id.into(),
                        name: name.into(),
                        input: block
                            .get("input")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({})),
                    });
                }
            }
            "tool_result" => {
                if let Some(id) = block.get("tool_use_id").and_then(Value::as_str) {
                    content.push(Content::ToolResult {
                        tool_use_id: id.into(),
                        content: result_text(&block["content"]),
                        is_error: block
                            .get("is_error")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    });
                    let mut attachments = Vec::new();
                    collect_content(&mut attachments, &block["content"]);
                    content.extend(
                        attachments.into_iter().filter(|c| {
                            matches!(c, Content::Image { .. } | Content::Document { .. })
                        }),
                    );
                }
            }
            "image" | "document" | "input_image" | "input_file" => {
                if let Some(text) = block
                    .pointer("/source/data")
                    .and_then(Value::as_str)
                    .filter(|_| {
                        block.pointer("/source/type").and_then(Value::as_str) == Some("text")
                    })
                {
                    content.push(Content::Text {
                        text: format!("[Текст документа] {text}"),
                    });
                } else if let Some(attachment) = inline_attachment(block) {
                    content.push(attachment);
                } else {
                    content.push(Content::Text {
                        text: format!("[Вложение {kind} доступно только в исходном журнале]"),
                    });
                }
            }
            _ => (),
        }
    }
}

fn inline_attachment(block: &Value) -> Option<Content> {
    let kind = block.get("type")?.as_str()?;
    let source = block.get("source");
    let (media_type, data) =
        if source.and_then(|v| v.get("type")).and_then(Value::as_str) == Some("base64") {
            (
                source?.get("media_type")?.as_str()?,
                source?.get("data")?.as_str()?,
            )
        } else {
            let url = block
                .get("image_url")
                .or_else(|| block.get("file_data"))?
                .as_str()?;
            let (header, data) = url.strip_prefix("data:")?.split_once(",")?;
            (header.strip_suffix(";base64")?, data)
        };
    if data.is_empty()
        || base64::engine::general_purpose::STANDARD
            .decode(data)
            .is_err()
    {
        return None;
    }
    match (kind, media_type) {
        ("image" | "input_image", "image/png" | "image/jpeg" | "image/gif" | "image/webp") => {
            Some(Content::Image {
                media_type: media_type.into(),
                data: data.into(),
            })
        }
        ("document" | "input_file", "application/pdf") => Some(Content::Document {
            media_type: media_type.into(),
            data: data.into(),
        }),
        _ => None,
    }
}

fn fallback_title(conversation: &Conversation) -> String {
    conversation
        .messages
        .iter()
        .filter(|m| m.role == Role::User)
        .flat_map(|m| &m.content)
        .find_map(|content| match content {
            Content::Text { text } if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or("Без названия")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(100)
        .collect()
}

fn title(conversation: &Conversation) -> String {
    let text = conversation
        .title
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| fallback_title(conversation));
    format!("[{}] {}", conversation.source, text)
}

/// Save a parsed transcript as a new project-scoped GitCode session.
pub fn save(store: &SessionStore, conversation: &Conversation) -> io::Result<Session> {
    let session = store.import(
        title(conversation),
        &conversation.messages,
        &conversation.original,
    )?;
    write_provenance(store, &session.id, &provenance(conversation))?;
    Ok(session)
}

fn session_key(conversation: &Conversation) -> String {
    use sha2::{Digest, Sha256};
    if let Some(id) = conversation
        .source_id
        .as_deref()
        .filter(|id| !id.is_empty())
    {
        return format!("{}:{id}", conversation.source);
    }
    format!(
        "{}:{:x}",
        conversation.source,
        Sha256::digest(&conversation.original)
    )
}

fn source_roots(home: &Path, source: Source) -> Vec<PathBuf> {
    let is_real_home = crate::platform::home_dir().is_some_and(|p| p == home);
    match source {
        Source::Claude => {
            let base = if is_real_home {
                std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from)
            } else {
                None
            }
            .unwrap_or_else(|| home.join(".claude"));
            vec![base.join("projects")]
        }
        Source::Codex => {
            let base = if is_real_home {
                std::env::var_os("CODEX_HOME").map(PathBuf::from)
            } else {
                None
            }
            .unwrap_or_else(|| home.join(".codex"));
            vec![base.join("sessions"), base.join("archived_sessions")]
        }
    }
}

fn codex_titles(root: &Path) -> HashMap<String, String> {
    use std::io::{BufRead, BufReader};
    let mut titles = HashMap::new();
    let Some(base) = root.parent() else {
        return titles;
    };
    let Ok(file) = fs::File::open(base.join("session_index.jsonl")) else {
        return titles;
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let (Some(id), Some(name)) = (
            value.get("id").and_then(Value::as_str),
            value
                .get("thread_name")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty()),
        ) {
            titles.insert(id.to_owned(), name.to_owned());
        }
    }
    titles
}

fn claude_index_title(path: &Path, id: Option<&str>) -> Option<(String, bool)> {
    let index = fs::read(path.parent()?.join("sessions-index.json")).ok()?;
    if index.len() > 16 * 1024 * 1024 {
        return None;
    }
    let value: Value = serde_json::from_slice(&index).ok()?;
    let entry = value
        .get("entries")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("sessionId").and_then(Value::as_str) == id && id.is_some())?;
    ["customTitle", "summary", "firstPrompt"]
        .iter()
        .find_map(|key| {
            entry
                .get(*key)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(|s| (s.to_owned(), *key == "customTitle"))
        })
}

fn index_title(conversation: &mut Conversation, path: &Path, codex: &HashMap<String, String>) {
    if conversation.source == "Codex" {
        if let Some(name) = conversation
            .source_id
            .as_deref()
            .and_then(|id| codex.get(id))
        {
            conversation.title = Some(name.clone());
        }
    } else if !conversation.title_is_custom {
        if let Some((name, is_custom)) = claude_index_title(path, conversation.source_id.as_deref())
        {
            if is_custom || conversation.title.is_none() {
                conversation.title = Some(name);
            }
        }
    }
}

/// Read a journal and load its session title from a nearby source index when available.
pub fn read_named(path: &Path) -> io::Result<Conversation> {
    let mut conversation = read(path)?;
    let root = path.ancestors().find(|p| {
        p.file_name()
            .is_some_and(|n| n == "sessions" || n == "archived_sessions")
    });
    let names = root.map(codex_titles).unwrap_or_default();
    index_title(&mut conversation, path, &names);
    Ok(conversation)
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Provenance {
    key: String,
    fallback: String,
    source: String,
    source_id: Option<String>,
    title: Option<String>,
    title_is_custom: bool,
    path: Option<PathBuf>,
    stamp: Option<(u64, u128)>,
    checked_title: Option<String>,
}

fn provenance(conversation: &Conversation) -> Provenance {
    Provenance {
        key: session_key(conversation),
        fallback: format!("[{}] {}", conversation.source, fallback_title(conversation)),
        source: conversation.source.into(),
        source_id: conversation.source_id.clone(),
        title: conversation.title.clone(),
        title_is_custom: conversation.title_is_custom,
        path: None,
        stamp: None,
        checked_title: None,
    }
}

fn write_provenance(store: &SessionStore, id: &str, value: &Provenance) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    store.write_provenance(id, &bytes)
}

fn source_stamp(path: &Path) -> io::Result<(u64, u128)> {
    let metadata = fs::metadata(path)?;
    Ok((
        metadata.len(),
        metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos(),
    ))
}

fn cached_conversation(value: &Provenance, source: Source) -> Conversation {
    Conversation {
        source: source.label(),
        source_id: value.source_id.clone(),
        title: value.title.clone(),
        title_is_custom: value.title_is_custom,
        project: None,
        messages: Vec::new(),
        original: Vec::new(),
    }
}

/// Import all readable sessions for one source into the current GitCode project.
/// Existing imports with the same source identifier are skipped.
pub fn import_all(home: &Path, store: &SessionStore, source: Source) -> io::Result<ImportReport> {
    let roots = source_roots(home, source);
    let names = if source == Source::Codex {
        codex_titles(&roots[0])
    } else {
        HashMap::new()
    };
    let mut known = HashMap::new();
    for entry in fs::read_dir(store.directory())? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|x| x == "source") {
            if let Some(id) = path.file_stem().and_then(|s| s.to_str()) {
                let cached = store
                    .read_provenance(id)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Provenance>(&bytes).ok());
                let value = match cached {
                    Some(value) => value,
                    None => match read(&path) {
                        Ok(conversation) => {
                            let value = provenance(&conversation);
                            write_provenance(store, id, &value)?;
                            value
                        }
                        Err(_) => continue,
                    },
                };
                if store.directory().join(format!("{id}.jsonl")).is_file() {
                    known.insert(value.key.clone(), (id.to_owned(), value));
                }
            }
        }
    }
    let mut paths: HashMap<PathBuf, String> = known
        .iter()
        .filter_map(|(key, (_, value))| value.path.clone().map(|path| (path, key.clone())))
        .collect();
    let mut report = ImportReport::default();
    let mut stack: Vec<_> = roots.into_iter().map(|path| (path, 0usize)).collect();
    let mut visited = 0usize;
    while let Some((directory, depth)) = stack.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                record_error(&mut report, &directory, &error);
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    record_error(&mut report, &directory, &error);
                    continue;
                }
            };
            visited += 1;
            if visited > 50_000 {
                return Err(invalid("Слишком много файлов при поиске сессий"));
            }
            let ty = match entry.file_type() {
                Ok(ty) => ty,
                Err(error) => {
                    record_error(&mut report, &entry.path(), &error);
                    continue;
                }
            };
            if ty.is_dir() {
                // Claude's top-level project folders contain the actual sessions;
                // deeper subagent journals are not independent user conversations.
                if source != Source::Claude || depth == 0 {
                    stack.push((entry.path(), depth + 1));
                }
                continue;
            }
            if !ty.is_file() || entry.path().extension().is_none_or(|x| x != "jsonl") {
                continue;
            }
            let path = entry.path();
            let stamp = source_stamp(&path).ok();
            let cached_key = paths
                .get(&path)
                .filter(|key| {
                    let value = &known[*key].1;
                    value.source == source.label() && stamp.is_some() && value.stamp == stamp
                })
                .cloned();
            let mut conversation = if let Some(key) = &cached_key {
                cached_conversation(&known[key].1, source)
            } else {
                match read(&path) {
                    Ok(c) if c.source == source.label() => c,
                    Ok(_) => continue,
                    Err(error) => {
                        record_error(&mut report, &path, &error);
                        continue;
                    }
                }
            };
            let key = cached_key.unwrap_or_else(|| session_key(&conversation));
            let mut value = if conversation.original.is_empty() {
                known[&key].1.clone()
            } else {
                provenance(&conversation)
            };
            index_title(&mut conversation, &path, &names);
            let desired_title = title(&conversation);
            if let Some((id, previous)) = known.get(&key) {
                report.skipped += 1;
                if conversation.title.is_some()
                    && previous.checked_title.as_ref() != Some(&desired_title)
                {
                    match store.resume(id) {
                        Ok(existing)
                            if existing.metadata.title == previous.fallback
                                && existing.metadata.title != desired_title =>
                        {
                            match store.update_metadata(
                                id,
                                &desired_title,
                                existing.metadata.branch,
                            ) {
                                Ok(()) => report.renamed += 1,
                                Err(error) => {
                                    record_error(&mut report, &path, &error);
                                    continue;
                                }
                            }
                        }
                        Err(error) => {
                            record_error(&mut report, &path, &error);
                            continue;
                        }
                        _ => (),
                    }
                }
                value.path = Some(path.clone());
                value.stamp = stamp;
                value.checked_title = Some(desired_title);
                if previous.path != value.path
                    || previous.stamp != value.stamp
                    || previous.checked_title != value.checked_title
                {
                    if let Err(error) = write_provenance(store, id, &value) {
                        record_error(&mut report, &path, &error);
                    }
                }
                paths.insert(path.clone(), key.clone());
                known.insert(key, (id.clone(), value));
                continue;
            }
            match save(store, &conversation) {
                Ok(session) => {
                    value.path = Some(path.clone());
                    value.stamp = stamp;
                    value.checked_title = Some(desired_title);
                    if let Err(error) = write_provenance(store, &session.id, &value) {
                        record_error(&mut report, &path, &error);
                    }
                    paths.insert(path.clone(), key.clone());
                    known.insert(key, (session.id, value));
                    report.imported += 1;
                }
                Err(error) => record_error(&mut report, &path, &error),
            }
        }
    }
    Ok(report)
}

fn record_error(report: &mut ImportReport, path: &Path, error: &io::Error) {
    report.failed += 1;
    if report.errors.len() < 3 {
        report.errors.push(format!("{}: {error}", path.display()));
    }
}
