//! Import and resume external agent journals.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use gitcode::provider::canonical::{Content, Message, Role};
use gitcode::session::{import, store::SessionStore};
use serde_json::json;
use std::{fs, path::Path};

fn write_jsonl(path: &Path, records: &[serde_json::Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let contents = records.iter().map(|v| format!("{v}\n")).collect::<String>();
    fs::write(path, contents).unwrap();
}

#[test]
fn imports_claude_and_codex_into_resumable_sessions() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let store = SessionStore::open(root.path(), &project).unwrap();
    let claude = root.path().join(".claude/projects/project/claude.jsonl");
    write_jsonl(
        &claude,
        &[
            json!({"type":"user", "cwd":project, "message":{"role":"user", "content":"Fix the bug"}}),
            json!({"type":"assistant", "message":{"role":"assistant", "content":[{"type":"thinking","thinking":"private"},{"type":"text","text":"I will fix it"},{"type":"tool_use","id":"a","name":"Edit","input":{}}]}}),
            json!({"type":"user", "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"a","content":"done"}]}}),
            json!({"type":"assistant", "isSidechain":true, "message":{"role":"assistant","content":"side agent"}}),
        ],
    );
    let codex = root.path().join(".codex/sessions/2026/11/13/rollout.jsonl");
    write_jsonl(
        &codex,
        &[
            json!({"type":"session_meta","payload":{"cwd":project}}),
            json!({"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"instructions"}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Add tests"}]}}),
            json!({"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{}"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Tests added"}]}}),
            json!({"type":"event_msg","payload":{"type":"agent_message","message":"Tests added"}}),
        ],
    );
    for (path, source, user, answer) in [
        (&claude, "Claude Code", "Fix the bug", "I will fix it"),
        (&codex, "Codex", "Add tests", "Tests added"),
    ] {
        let conversation = import::read(path).unwrap();
        assert_eq!(conversation.source, source);
        assert!(conversation.messages.iter().any(|m| m
            .content
            .iter()
            .any(|c| matches!(c, Content::Text { text } if text.contains(user)))));
        assert!(conversation.messages.iter().any(|m| m
            .content
            .iter()
            .any(|c| matches!(c, Content::Text { text } if text.contains(answer)))));
        assert!(conversation.messages.iter().any(|m| m
            .content
            .iter()
            .any(|c| matches!(c, Content::ToolUse { .. }))));
        let session = import::save(&store, &conversation).unwrap();
        let source_file = store.directory().join(format!("{}.source", session.id));
        assert_eq!(fs::read(source_file).unwrap(), fs::read(path).unwrap());
        let loaded = store.resume(&session.id).unwrap();
        assert_eq!(loaded.messages, conversation.messages);
        assert!(loaded.metadata.title.contains(source));
        store
            .append(&session.id, &Message::text(Role::User, "Continue"))
            .unwrap();
        assert_eq!(
            store.resume(&session.id).unwrap().messages.len(),
            conversation.messages.len() + 1
        );
    }
    let list = store.list().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list
        .iter()
        .any(|s| s.metadata.title.starts_with("[Claude Code]")));
    assert!(list.iter().any(|s| s.metadata.title.starts_with("[Codex]")));
    assert!(matches!(
        &store.resume(&list[0].id).unwrap().messages[0].content[0],
        Content::Text { .. }
    ));
}

#[test]
fn malformed_inputs_are_reported_without_creating_sessions() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let file = root.path().join(".codex/sessions/broken.jsonl");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "{bad json\n").unwrap();
    let store = SessionStore::open(root.path(), &project).unwrap();
    let report = import::import_all(root.path(), &store, import::Source::Codex).unwrap();
    assert_eq!((report.imported, report.failed), (0, 1));
    assert!(report.errors[0].contains("broken.jsonl"));
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn inline_attachments_survive_import_resume_and_all_provider_encoders() {
    use gitcode::provider::{anthropic, canonical::Request, openai_chat, openai_responses};
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let path = root.path().join("claude.jsonl");
    write_jsonl(
        &path,
        &[
            json!({"type":"user", "cwd":project, "message":{"content":[
                {"type":"text","text":"Explain these"},
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":"aW1hZ2U="}},
                {"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"cGRm"}}
            ]}}),
            json!({"type":"user", "message":{"content":[{"type":"tool_result","tool_use_id":"id1", "content":[
                {"type":"text","text":"Screenshot"},
                {"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"cGhvdG8="}}
            ]}]}}),
            json!({"type":"user", "message":{"content":[{"type":"image","source":{"type":"url","url":"https://example.test/image"}}]}}),
            json!({"type":"attachment","attachment":{"type":"file","filename":"file.rs","content":{"type":"text","file":{"content":"fn imported() {}"}}}}),
            json!({"type":"attachment","attachment":{"type":"edited_text_file","filename":"file.rs","snippet":"updated snippet"}}),
        ],
    );
    let conversation = import::read(&path).unwrap();
    let store = SessionStore::open(root.path(), &project).unwrap();
    let session = import::save(&store, &conversation).unwrap();
    let messages = store.resume(&session.id).unwrap().messages;
    assert!(messages[0].content.iter().any(|c| matches!(c, Content::Image {media_type,data} if media_type == "image/png" && data == "aW1hZ2U=")));
    assert!(messages[0].content.iter().any(
        |c| matches!(c, Content::Document {media_type,..} if media_type == "application/pdf")
    ));
    assert!(messages[1]
        .content
        .iter()
        .any(|c| matches!(c, Content::Image {media_type,..} if media_type == "image/jpeg")));
    assert!(messages[2].content.iter().any(
        |c| matches!(c, Content::Text { text } if text.contains("только в исходном журнале"))
    ));
    assert!(messages[3]
        .content
        .iter()
        .any(|c| matches!(c, Content::Text { text } if text.contains("fn imported() {}"))));
    assert!(messages[4]
        .content
        .iter()
        .any(|c| matches!(c, Content::Text { text } if text.contains("updated snippet"))));
    let request = Request {
        model: "fixture".into(),
        system: "system".into(),
        messages,
        tools: vec![],
        max_tokens: 128,
    };
    let anthropic = anthropic::encode(&request).unwrap();
    assert_eq!(
        anthropic["messages"][0]["content"][1]["source"]["data"],
        "aW1hZ2U="
    );
    assert_eq!(
        anthropic["messages"][0]["content"][2]["source"]["data"],
        "cGRm"
    );
    let chat = openai_chat::encode(&request).unwrap();
    assert_eq!(
        chat["messages"][1]["content"][1]["image_url"]["url"],
        "data:image/png;base64,aW1hZ2U="
    );
    assert_eq!(
        chat["messages"][1]["content"][2]["file"]["file_data"],
        "data:application/pdf;base64,cGRm"
    );
    let responses = openai_responses::encode(&request).unwrap();
    assert!(responses["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["content"][0]["image_url"] == "data:image/png;base64,aW1hZ2U="));
    assert!(responses["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["content"][0]["file_data"] == "data:application/pdf;base64,cGRm"));
}

#[test]
fn bulk_imports_all_projects_with_original_titles_and_no_duplicates() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("current");
    fs::create_dir(&project).unwrap();
    let claude_a = root.path().join(".claude/projects/a/aaa.jsonl");
    let claude_b = root.path().join(".claude/projects/b/bbb.jsonl");
    write_jsonl(
        &claude_a,
        &[
            json!({"type":"user","sessionId":"aaa", "cwd":"/different/project", "message":{"content":"Original prompt A"}}),
            json!({"type":"ai-title", "sessionId":"aaa", "aiTitle":"Old automatic title"}),
            json!({"type":"custom-title", "sessionId":"aaa", "customTitle":"My renamed Claude chat"}),
            json!({"type":"ai-title", "sessionId":"aaa", "aiTitle":"Later automatic title"}),
        ],
    );
    write_jsonl(
        &claude_b,
        &[
            json!({"type":"user","sessionId":"bbb", "message":{"content":"Original prompt B"}}),
            json!({"type":"ai-title", "sessionId":"bbb", "aiTitle":"Generated title"}),
        ],
    );
    let codex = root.path().join(".codex/sessions/2026/01/01/rollout.jsonl");
    let archived = root.path().join(".codex/archived_sessions/old.jsonl");
    for (path, id) in [(&codex, "codex-a"), (&archived, "codex-b")] {
        write_jsonl(
            path,
            &[
                json!({"type":"session_meta","payload":{"id":id,"cwd":"/other/project"}}),
                json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Codex prompt"}]}}),
            ],
        );
    }
    let index = root.path().join(".codex/session_index.jsonl");
    write_jsonl(
        &index,
        &[
            json!({"id":"codex-a","thread_name":"Old name"}),
            json!({"id":"codex-a","thread_name":"My renamed Codex chat"}),
            json!({"id":"codex-b","thread_name":"Archived conversation"}),
        ],
    );
    let nested = root
        .path()
        .join(".claude/projects/a/aaa/subagents/agent.jsonl");
    write_jsonl(
        &nested,
        &[json!({"type":"user","sessionId":"child", "message":{"content":"subagent"}})],
    );
    let store = SessionStore::open(root.path(), &project).unwrap();
    let old = import::read(&claude_a).unwrap();
    let existing = store
        .import(
            "[Claude Code] Original prompt A",
            &old.messages,
            &old.original,
        )
        .unwrap();
    let report = import::import_all(root.path(), &store, import::Source::Claude).unwrap();
    assert_eq!(
        (
            report.imported,
            report.skipped,
            report.failed,
            report.renamed
        ),
        (1, 1, 0, 1)
    );
    assert_eq!(
        store.resume(&existing.id).unwrap().metadata.title,
        "[Claude Code] My renamed Claude chat"
    );
    let titles: Vec<_> = store
        .list()
        .unwrap()
        .into_iter()
        .map(|s| s.metadata.title)
        .collect();
    assert!(titles.contains(&"[Claude Code] My renamed Claude chat".to_owned()));
    assert!(titles.contains(&"[Claude Code] Generated title".to_owned()));
    assert_eq!(store.list().unwrap().len(), 2);
    let report = import::import_all(root.path(), &store, import::Source::Codex).unwrap();
    assert_eq!((report.imported, report.skipped, report.failed), (2, 0, 0));
    let titles: Vec<_> = store
        .list()
        .unwrap()
        .into_iter()
        .map(|s| s.metadata.title)
        .collect();
    assert!(titles.contains(&"[Codex] My renamed Codex chat".to_owned()));
    assert!(titles.contains(&"[Codex] Archived conversation".to_owned()));
    assert_eq!(
        import::read_named(&codex).unwrap().title.as_deref(),
        Some("My renamed Codex chat")
    );
    let report = import::import_all(root.path(), &store, import::Source::Codex).unwrap();
    assert_eq!((report.imported, report.skipped, report.failed), (0, 2, 0));
    store
        .update_metadata(&existing.id, "My own GitCode title", None)
        .unwrap();
    let report = import::import_all(root.path(), &store, import::Source::Claude).unwrap();
    assert_eq!(
        (
            report.imported,
            report.skipped,
            report.failed,
            report.renamed
        ),
        (0, 2, 0, 0)
    );
    assert_eq!(
        store.resume(&existing.id).unwrap().metadata.title,
        "My own GitCode title"
    );
    assert_eq!(store.list().unwrap().len(), 4);
}

#[test]
fn structured_blocks_keep_replay_identifiers_arguments_and_reasoning() {
    use gitcode::provider::{anthropic, canonical::Request, openai_chat, openai_responses};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("claude.jsonl");
    write_jsonl(
        &path,
        &[
            json!({"type":"assistant","message":{"content":[
                {"type":"thinking","thinking":"plan","signature":"signed"},
                {"type":"tool_use","id":"tool-1","name":"Read","input":{"path":"a.rs"}}
            ]}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tool-1","is_error":true,"content":[{"type":"text","text":"missing"}]}]}}),
        ],
    );
    let conversation = import::read(&path).unwrap();
    assert_eq!(
        conversation.messages[0].content[0],
        Content::Thinking {
            thinking: "plan".into(),
            signature: Some("signed".into()),
            encrypted_content: None,
        }
    );
    assert_eq!(
        conversation.messages[1].content[0],
        Content::ToolResult {
            tool_use_id: "tool-1".into(),
            content: "missing".into(),
            is_error: true,
        }
    );
    let mut request = Request {
        model: "fixture".into(),
        system: String::new(),
        messages: conversation.messages,
        tools: vec![],
        max_tokens: 128,
    };
    let wire = anthropic::encode(&request).unwrap();
    assert_eq!(wire["messages"][0]["content"][0]["signature"], "signed");
    assert_eq!(
        wire["messages"][0]["content"][1]["input"],
        json!({"path":"a.rs"})
    );
    let wire = openai_chat::encode(&request).unwrap();
    assert_eq!(wire["messages"][1]["tool_calls"][0]["id"], "tool-1");
    assert_eq!(wire["messages"][2]["tool_call_id"], "tool-1");
    write_jsonl(
        &path,
        &[
            json!({"type":"session_meta","payload":{"id":"thread"}}),
            json!({"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"plan"}],"encrypted_content":"opaque"}}),
            json!({"type":"response_item","payload":{"type":"function_call","id":"item-id","call_id":"call-id","name":"exec_command","arguments":"{\"cmd\":\"pwd\"}"}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"call-id","output":"/project"}}),
            json!({"type":"response_item","payload":{"type":"custom_tool_call","call_id":"patch-id","name":"apply_patch","input":"*** patch"}}),
            json!({"type":"response_item","payload":{"type":"local_shell_call","id":"shell-id","action":{"command":["pwd"]}}}),
        ],
    );
    request.messages = import::read(&path).unwrap().messages;
    assert!(
        matches!(&request.messages[1].content[0], Content::ToolUse {id, name, input} if id == "call-id" && name == "exec_command" && input == &json!({"cmd":"pwd"}))
    );
    assert!(
        matches!(&request.messages[3].content[0], Content::ToolUse {id, input,..} if id == "patch-id" && input == &json!({"input":"*** patch"}))
    );
    assert!(
        matches!(&request.messages[4].content[0], Content::ToolUse {id, name, input} if id == "shell-id" && name == "local_shell" && input == &json!({"command":["pwd"]}))
    );
    let wire = openai_responses::encode(&request).unwrap();
    assert_eq!(wire["input"][0]["encrypted_content"], "opaque");
    assert_eq!(wire["input"][1]["call_id"], "call-id");
    assert_eq!(wire["input"][2]["call_id"], "call-id");
}

#[test]
fn persistent_provenance_skips_sources_and_keeps_index_renames() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let path = root.path().join(".codex/sessions/session.jsonl");
    write_jsonl(
        &path,
        &[
            json!({"type":"session_meta","payload":{"id":"thread"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Prompt"}]}}),
        ],
    );
    let store = SessionStore::open(root.path(), &project).unwrap();
    assert_eq!(
        import::import_all(root.path(), &store, import::Source::Codex)
            .unwrap()
            .imported,
        1
    );
    let id = store.list().unwrap()[0].id.clone();
    // An unreadable legacy source must not be parsed once provenance exists.
    fs::write(store.directory().join(format!("{id}.source")), "not json").unwrap();
    let old = fs::metadata(&path).unwrap().modified().unwrap();
    let size = fs::metadata(&path).unwrap().len() as usize;
    fs::write(&path, vec![b'!'; size]).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(old))
        .unwrap();
    let store = SessionStore::open(root.path(), &project).unwrap();
    let index = root.path().join(".codex/session_index.jsonl");
    write_jsonl(&index, &[json!({"id":"thread","thread_name":"Renamed"})]);
    let report = import::import_all(root.path(), &store, import::Source::Codex).unwrap();
    assert_eq!(
        (
            report.imported,
            report.skipped,
            report.failed,
            report.renamed
        ),
        (0, 1, 0, 1)
    );
    assert_eq!(store.resume(&id).unwrap().metadata.title, "[Codex] Renamed");
    fs::write(&path, "changed file").unwrap();
    assert_eq!(
        import::import_all(root.path(), &store, import::Source::Codex)
            .unwrap()
            .failed,
        1
    );
}
