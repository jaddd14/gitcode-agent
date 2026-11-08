//! Milestone M15 hardening: end-to-end soak session across Anthropic, OpenAI Chat,
//! and OpenAI Responses protocols, plus resize/paste/unicode edge cases.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use gitcode::{
    agent::{context, Agent, BASE_PROMPT},
    credentials::Secret,
    perm::{Engine, Mode, Rule},
    provider::{
        canonical::{Event, Message, Role},
        transport::Client,
    },
    session::{checkpoint::Checkpoints, memory, store::SessionStore},
    theme::{ColorMode, Theme},
    tools::Tools,
    tui::{
        diff,
        input::{Action, Editor},
        markdown::Markdown,
    },
};
use std::{path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
};

async fn spawn_mock_server(responses: Vec<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for body in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 16384];
            let _ = socket.read(&mut buf).await;
            let http = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(http.as_bytes()).await.unwrap();
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn end_to_end_soak_across_all_three_protocols_with_checkpoints_sessions_and_diffs() {
    let base = std::env::temp_dir().join(format!("gitcode-m15-soak-{}", std::process::id()));
    let home = base.join("home");
    let workspace = base.join("workspace");
    let shadow_storage = base.join("shadow");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("GITCODE.md"), "Всегда проверяй файлы.\n").unwrap();
    std::fs::write(workspace.join("app.txt"), "alpha\nbeta\n").unwrap();

    // 1. Load hierarchical memory
    let mem = memory::load(&home, &workspace, &workspace).unwrap();
    assert!(mem.text.contains("Всегда проверяй файлы."));

    // 2. Open isolated shadow checkpoints and snapshot initial state
    let checkpoints = Checkpoints::open(&workspace, &shadow_storage)
        .await
        .unwrap();
    let cp0 = checkpoints
        .snapshot(&[PathBuf::from("app.txt")])
        .await
        .unwrap();

    // 3. Protocol 1: Anthropic Messages (streaming thinking + Read + Edit + denied Read + final answer)
    let anthropic_turn1 = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Читаю и обновляю app.txt\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_read\",\"name\":\"Read\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"app.txt\\\"}\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_denied\",\"name\":\"Read\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"secret.env\\\"}\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":18}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n"
    ).to_owned();

    let anthropic_turn2 = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":40,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_edit\",\"name\":\"Edit\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"app.txt\\\",\\\"old_string\\\":\\\"beta\\\",\\\"new_string\\\":\\\"gamma\\\"}\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":15}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n"
    ).to_owned();

    let anthropic_turn3 = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":55,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"# Готово\\n\\nЯ обновил `app.txt`.\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":12}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n"
    ).to_owned();

    let anth_url = spawn_mock_server(vec![anthropic_turn1, anthropic_turn2, anthropic_turn3]).await;
    let mut engine = Engine::new(&workspace, Mode::AcceptEdits).unwrap();
    engine.deny.push(Rule::parse("Read(*secret*)").unwrap());
    let tools = Tools::new(engine);
    let client = Client::with_protocol(
        &anth_url,
        Some(Secret::new("test-key".into())),
        false,
        Duration::from_secs(10),
        gitcode::config::Protocol::Anthropic,
    )
    .unwrap();
    let mut agent = Agent {
        client,
        tools,
        model: "claude-opus-5-5".into(),
        system: format!("{BASE_PROMPT}\n{}", mem.text),
        max_turns: 8,
        max_tokens: 1024,
    };

    let (tx, mut rx) = mpsc::channel(64);
    let outcome = agent
        .run(vec![Message::text(Role::User, "Обнови app.txt")], Some(&tx))
        .await
        .unwrap();
    drop(tx);
    let mut saw_thinking = false;
    let mut md = Markdown::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            Event::ThinkingDelta { text } if text.contains("Читаю") => saw_thinking = true,
            Event::TextDelta { text } => md.push(&text),
            _ => {}
        }
    }
    md.finish();
    assert!(saw_thinking);
    assert_eq!(outcome.requests, 3);
    assert_eq!(
        std::fs::read_to_string(workspace.join("app.txt")).unwrap(),
        "alpha\ngamma\n"
    );
    let rendered_md = md.render(80, &Theme::default(), ColorMode::None);
    assert!(rendered_md.iter().any(|l| l.contains("Готово")));

    // Render diff and test checkpoint rewind
    let diff_lines = diff::render(
        Some(b"alpha\nbeta\n"),
        Some(b"alpha\ngamma\n"),
        80,
        ColorMode::None,
    );
    assert!(diff_lines.iter().any(|l| l.contains("- beta")));
    assert!(diff_lines.iter().any(|l| l.contains("+ gamma")));

    checkpoints
        .restore(&cp0, &[PathBuf::from("app.txt")])
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(workspace.join("app.txt")).unwrap(),
        "alpha\nbeta\n"
    );

    // 4. Persist session to JSONL store, trim tool results, and resume
    let store = SessionStore::open(&home, &workspace).unwrap();
    let created = store.create("Soak Session", Some("main".into())).unwrap();
    for msg in &outcome.messages {
        store.append(&created.id, msg).unwrap();
    }
    let mut resumed = store.resume(&created.id).unwrap();
    assert_eq!(resumed.messages.len(), outcome.messages.len());
    let _ = context::trim_tool_results(&mut resumed.messages, 1, 8);
    assert!(context::estimate(&resumed.messages).unwrap() > 0);

    // 5. Protocol 2: OpenAI Chat Completions on resumed conversation
    let chat_sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"Думаю...\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Продолжаю через OpenAI Chat.\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":30,\"completion_tokens\":8}}\n\n",
        "data: [DONE]\n\n"
    ).to_owned();
    let chat_url = spawn_mock_server(vec![chat_sse]).await;
    agent.client = Client::with_protocol(
        &chat_url,
        Some(Secret::new("test-key".into())),
        true,
        Duration::from_secs(10),
        gitcode::config::Protocol::OpenaiChat,
    )
    .unwrap();
    let mut chat_history = resumed.messages;
    chat_history.push(Message::text(Role::User, "Проверка Chat"));
    let chat_outcome = agent.run(chat_history, None).await.unwrap();
    assert_eq!(chat_outcome.text, "Продолжаю через OpenAI Chat.");

    // 6. Protocol 3: OpenAI Responses API on the same conversation
    let resp_sse = concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"И через Responses API тоже работает.\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":42,\"output_tokens\":9}}}\n\n"
    ).to_owned();
    let resp_url = spawn_mock_server(vec![resp_sse]).await;
    agent.client = Client::with_protocol(
        &resp_url,
        Some(Secret::new("test-key".into())),
        true,
        Duration::from_secs(10),
        gitcode::config::Protocol::OpenaiResponses,
    )
    .unwrap();
    let mut resp_history = chat_outcome.messages;
    resp_history.push(Message::text(Role::User, "Проверка Responses"));
    let resp_outcome = agent.run(resp_history, None).await.unwrap();
    assert_eq!(resp_outcome.text, "И через Responses API тоже работает.");
    assert_eq!(resp_outcome.usage.input_tokens, 42);

    // 7. Editor paste chip + Unicode + resize edge cases
    let mut editor = Editor::default();
    let pasted = "первая строка\nвторая строка\nтретья строка\nчетвёртая строка";
    let _ = editor.handle(crossterm::event::Event::Paste(pasted.into()));
    let view = editor.view(40);
    assert!(!view.lines.is_empty());
    if let Action::Submit(submitted) = editor.handle(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ),
    )) {
        assert_eq!(submitted, pasted);
    } else {
        panic!("expected submit from Enter after paste");
    }

    let _ = std::fs::remove_dir_all(&base);
}
