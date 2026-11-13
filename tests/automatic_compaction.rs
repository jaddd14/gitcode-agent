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
