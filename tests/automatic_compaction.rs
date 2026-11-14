//! Automatic summaries preserve the complete session and stay out of user output.
#![allow(clippy::unwrap_used)]
use gitcode::{
    agent::{context::Compaction, Agent},
    perm::{Engine, Mode},
    provider::{
        canonical::{Content, Event, Message, Role},
        transport::Client,
    },
    tools::Tools,
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn summaries_are_reused_and_full_history_is_preserved() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("example.txt"), "verified after compaction").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for turn in 0..6 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut data = Vec::new();
            let mut buf = [0; 8192];
            let body_start = loop {
                let n = stream.read(&mut buf).await.unwrap();
                assert_ne!(n, 0);
                data.extend_from_slice(&buf[..n]);
                if let Some(end) = data.windows(4).position(|x| x == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|l| {
                            l.strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap();
                    if data.len() >= end + 4 + length {
                        break end + 4;
                    }
                }
            };
            let request = String::from_utf8_lossy(&data[body_start..]);
            if turn < 3 {
                assert!(request.contains("Summarize the conversation"));
                if turn == 0 {
                    assert!(request.contains("old-important-marker"));
                } else {
                    assert!(request.contains("summary-important-marker"));
                }
                assert!(!request.contains("aW1hZ2UtZml4dHVyZQ=="));
            } else {
                assert!(request.contains("summary-important-marker"));
                assert!(request.contains("recent-marker"));
                assert!(!request.contains("old-important-marker"));
                assert!(request.contains("aW1hZ2UtZml4dHVyZQ=="));
                if turn >= 4 {
                    assert!(request.contains("verified after compaction"));
                }
            }
            let text = if turn < 3 {
                "summary-important-marker"
            } else {
                "answer"
            };
            let body = format!("data: {{\"type\":\"message_start\",\"message\":{{}}}}\n\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{text}\"}}}}\n\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}}}}\n\ndata: {{\"type\":\"message_stop\"}}\n\n");
            let body = if turn == 3 {
                concat!(
                    "data: {\"type\":\"message_start\",\"message\":{}}\n\n",
                    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"read_1\",\"name\":\"Read\",\"input\":{\"path\":\"example.txt\"}}}\n\n",
                    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
                    "data: {\"type\":\"message_stop\"}\n\n",
                ).to_owned()
            } else {
                body
            };
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let mut agent = Agent {
        client: Client::new(
            &format!("http://{address}"),
            None,
            false,
            Duration::from_secs(5),
        )
        .unwrap(),
        tools: Tools::new(Engine::new(root.path(), Mode::Default).unwrap()),
        model: "fixture".into(),
        system: String::new(),
        max_turns: 2,
        max_tokens: 128,
        compaction: Compaction::new(50_000, 0.6),
    };
    let mut history = vec![Message::text(
        Role::User,
        format!("old-important-marker {}", "x".repeat(120_000)),
    )];
    history[0].content.push(Content::Image {
        media_type: "image/png".into(),
        data: "aW1hZ2UtZml4dHVyZQ==".into(),
    });
    for i in 0..8 {
        history.push(Message::text(
            if i % 2 == 0 {
                Role::Assistant
            } else {
                Role::User
            },
            format!("recent-marker {i}"),
        ));
    }
    let original = history.clone();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
    let outcome = agent.run(history, Some(&sender)).await.unwrap();
    assert_eq!(&outcome.messages[..original.len()], original.as_slice());
    assert_eq!(outcome.text, "answer");
    assert_eq!(outcome.requests, 5);
    assert!(outcome.messages.iter().flat_map(|message| &message.content).any(|block|
        matches!(block, Content::ToolResult { content, is_error: false, .. } if content.contains("verified after compaction"))));
    let mut seen_start = false;
    let mut seen_finish = false;
    while let Ok(event) = receiver.try_recv() {
        match event {
            Event::CompactionStarted => seen_start = true,
            Event::CompactionFinished => seen_finish = true,
            Event::TextDelta { text } => assert!(!text.contains("summary-important-marker")),
            _ => {}
        }
    }
    assert!(seen_start && seen_finish);
    let mut next = outcome.messages;
    next.push(Message::text(Role::User, "recent-marker next"));
    assert_eq!(agent.run(next, None).await.unwrap().text, "answer");
    server.await.unwrap();
}

#[tokio::test]
async fn failed_summary_keeps_original_checkpoint() {
    use gitcode::agent::TurnCheckpoint;
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 8192];
        let _ = stream.read(&mut buffer).await.unwrap();
        stream
            .write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
    });
    let mut agent = Agent {
        client: Client::new(
            &format!("http://{address}"),
            None,
            false,
            Duration::from_secs(5),
        )
        .unwrap(),
        tools: Tools::new(Engine::new(root.path(), Mode::Default).unwrap()),
        model: "fixture".into(),
        system: String::new(),
        max_turns: 2,
        max_tokens: 128,
        compaction: Compaction::new(10_000, 0.2),
    };
    let mut history = vec![Message::text(Role::User, "old detail ".repeat(2000))];
    for i in 0..6 {
        history.push(Message::text(
            if i % 2 == 0 {
                Role::Assistant
            } else {
                Role::User
            },
            format!("recent {i}"),
        ));
    }
    let checkpoint = TurnCheckpoint::new(history.clone());
    let error = agent
        .run_with_checkpoint(history.clone(), None, Some(&checkpoint))
        .await
        .unwrap_err();
    assert!(error.contains("не удалось сжать контекст"));
    let recovered = checkpoint.interrupted("");
    assert_eq!(&recovered[..history.len()], history.as_slice());
    server.await.unwrap();
}

async fn read_json_request(stream: &mut tokio::net::TcpStream) -> serde_json::Value {
    let mut data = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = stream.read(&mut buf).await.unwrap();
        assert!(n > 0);
        data.extend_from_slice(&buf[..n]);
        if let Some(end) = data.windows(4).position(|part| part == b"\r\n\r\n") {
            let size = String::from_utf8_lossy(&data[..end])
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|n| n.parse::<usize>().ok())
                })
                .unwrap();
            if data.len() >= end + 4 + size {
                return serde_json::from_slice(&data[end + 4..end + 4 + size]).unwrap();
            }
        }
    }
}
fn summary_response(text: &str, reason: &str) -> String {
    format!("data: {{\"type\":\"message_start\",\"message\":{{}}}}\n\ndata: {}\n\ndata: {}\n\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\ndata: {}\n\ndata: {{\"type\":\"message_stop\"}}\n\n", serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}), serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}), serde_json::json!({"type":"message_delta","delta":{"stop_reason":reason},"usage":{"input_tokens":10,"output_tokens":5}}))
}
fn oversized_request() -> gitcode::provider::canonical::Request {
    let mut messages = vec![Message::text(
        Role::User,
        format!("IMPORTANT_GOAL {}", "x".repeat(6000)),
    )];
    for i in 0..6 {
        messages.push(Message::text(
            if i % 2 == 0 {
                Role::Assistant
            } else {
                Role::User
            },
            format!("recent {i}"),
        ));
    }
    gitcode::provider::canonical::Request {
        model: "fixture".into(),
        system: String::new(),
        messages,
        tools: vec![],
        max_tokens: 128,
    }
}

#[tokio::test]
async fn truncated_summary_retries_smaller_chunks_and_keeps_full_history() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for turn in 0..3 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_json_request(&mut socket).await;
            assert_eq!(request["output_config"]["effort"], "low");
            let body = request.to_string();
            if turn > 0 {
                assert!(!body.contains("UNFINISHED_SUMMARY"));
            }
            if turn == 2 {
                assert!(body.contains("first completed summary"));
            }
            let response = match turn {
                0 => summary_response("UNFINISHED_SUMMARY", "max_tokens"),
                1 => summary_response("first completed summary", "end_turn"),
                _ => summary_response("final completed summary", "end_turn"),
            };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        }
    });
    let mut client = Client::new(
        &format!("http://{address}"),
        None,
        false,
        Duration::from_secs(5),
    )
    .unwrap();
    client.set_effort(Some("xhigh")).unwrap();
    let request = oversized_request();
    let original = request.messages.clone();
    let mut compaction = Compaction::new(10_000, 0.2);
    let (projection, usage) = tokio::time::timeout(
        Duration::from_secs(10),
        compaction.prepare(&client, &request, None),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(usage.len(), 3);
    assert_eq!(request.messages, original);
    assert_eq!(client.effort(), Some("xhigh"));
    assert!(serde_json::to_string(&projection)
        .unwrap()
        .contains("final completed summary"));
    assert!(!serde_json::to_string(&projection)
        .unwrap()
        .contains("UNFINISHED_SUMMARY"));
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let store = gitcode::session::store::SessionStore::open(
        home.path().canonicalize().unwrap(),
        project.path().canonicalize().unwrap(),
    )
    .unwrap();
    let session = store.create("checkpoint", None).unwrap();
    for message in &original {
        store.append(&session.id, message).unwrap();
    }
    let snapshot = compaction.snapshot(&original).unwrap().unwrap();
    store.save_compaction(&session.id, &snapshot).unwrap();
    let restored_history = store.resume(&session.id).unwrap().messages;
    let stored = store.load_compaction(&session.id).unwrap().unwrap();
    let mut restored = Compaction::new(10_000, 0.2);
    assert!(restored.restore(&stored, &restored_history));
    let unavailable = Client::new(
        "http://127.0.0.1:9",
        None,
        false,
        Duration::from_millis(100),
    )
    .unwrap();
    let (restored_projection, retries) = restored
        .prepare(&unavailable, &request, None)
        .await
        .unwrap();
    assert!(retries.is_empty());
    assert_eq!(restored_projection, projection);
    let mut changed = restored_history.clone();
    changed[0] = Message::text(Role::User, "different conversation");
    assert!(!restored.restore(&stored, &changed));
    server.await.unwrap();
}

#[tokio::test]
async fn refused_summary_is_not_retried_or_accepted() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_json_request(&mut socket).await;
        let response = summary_response("declined", "refusal");
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_err()
        );
    });
    let client = Client::new(
        &format!("http://{address}"),
        None,
        false,
        Duration::from_secs(5),
    )
    .unwrap();
    let request = oversized_request();
    let mut compaction = Compaction::new(10_000, 0.2);
    let error = compaction
        .prepare(&client, &request, None)
        .await
        .unwrap_err();
    assert!(
        error.contains("отклонено") && error.contains("история сохранена"),
        "{error}"
    );
    assert_eq!(
        compaction.estimate_projection(&request.messages).unwrap(),
        gitcode::agent::context::estimate(&request.messages).unwrap()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn explicit_compact_generates_checkpoint_without_mutating_history() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_json_request(&mut socket).await;
        assert!(request.to_string().contains("Summarize the conversation"));
        assert!(request.to_string().contains("keep the test focus"));
        let response = summary_response("saved checkpoint", "end_turn");
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let client = Client::new(
        &format!("http://{address}"),
        None,
        false,
        Duration::from_secs(5),
    )
    .unwrap();
    let mut agent = Agent {
        client,
        tools: Tools::new(Engine::new(root.path(), Mode::Bypass).unwrap()),
        model: "fixture".into(),
        system: String::new(),
        max_turns: 2,
        max_tokens: 128,
        compaction: Compaction::new(200_000, 0.9),
    };
    let mut messages = (0..8)
        .map(|i| {
            Message::text(
                if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                format!("message {i} {}", "a".repeat(1000)),
            )
        })
        .collect::<Vec<_>>();
    let original = messages.clone();
    let mut status = gitcode::tui::statusbar::Status::default();
    let card = gitcode::tui::commands::execute_command(
        "/compact keep the test focus",
        &mut agent,
        &mut messages,
        &mut status,
        120,
    )
    .await
    .unwrap()
    .unwrap()
    .join("\n");
    assert!(card.contains("Контекст сжат"), "{card}");
    assert_eq!(messages, original);
    assert!(agent.compaction.snapshot(&messages).unwrap().is_some());
    assert_eq!(agent.compaction.take_manual_usage().len(), 1);
    server.await.unwrap();
}

#[tokio::test]
async fn empty_completed_summary_is_retried_once() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for response in [
            summary_response("", "end_turn"),
            summary_response("checkpoint after retry", "end_turn"),
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_json_request(&mut socket).await;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        }
    });
    let client = Client::new(
        &format!("http://{address}"),
        None,
        false,
        Duration::from_secs(5),
    )
    .unwrap();
    let mut request = oversized_request();
    request.messages[0] = Message::text(Role::User, "a".repeat(1500));
    let mut compaction = Compaction::new(4096, 0.1);
    let (projection, usage) = compaction.prepare(&client, &request, None).await.unwrap();
    assert_eq!(usage.len(), 2);
    assert!(serde_json::to_string(&projection)
        .unwrap()
        .contains("checkpoint after retry"));
    server.await.unwrap();
}

#[tokio::test]
async fn oversized_summary_does_not_replace_smaller_context() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_json_request(&mut socket).await;
        let response = summary_response(&"large summary ".repeat(200), "end_turn");
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let client = Client::new(
        &format!("http://{address}"),
        None,
        false,
        Duration::from_secs(5),
    )
    .unwrap();
    let mut request = oversized_request();
    request.messages[0] = Message::text(Role::User, "short original");
    let original = request.messages.clone();
    let mut compaction = Compaction::new(10_000, 0.9);
    let error = compaction
        .compact(&client, &request, None, None)
        .await
        .unwrap_err();
    assert!(error.contains("не уменьшило"));
    assert_eq!(request.messages, original);
    assert!(compaction.snapshot(&original).unwrap().is_none());
    server.await.unwrap();
}
