//! Long tool sequences and explicitly configured request limits.
#![allow(clippy::unwrap_used)]

use gitcode::{
    agent::{context::Compaction, Agent, TurnCheckpoint},
    config::Config,
    perm::{Engine, Mode},
    provider::{
        canonical::{Content, Message, Role},
        transport::Client,
    },
    tools::Tools,
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn scripted_server(
    steps: usize,
    final_answer: bool,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        for turn in 0..steps {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let body = if final_answer && turn + 1 == steps {
                "data: {\"type\":\"message_start\",\"message\":{}}\n\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"finished\"}}\n\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n".to_owned()
            } else {
                format!("data: {{\"type\":\"message_start\",\"message\":{{}}}}\n\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"read_{turn}\",\"name\":\"Read\",\"input\":{{\"path\":\"example.txt\"}}}}}}\n\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"tool_use\"}}}}\n\ndata: {{\"type\":\"message_stop\"}}\n\n")
            };
            socket
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
    (format!("http://{address}"), task)
}

fn agent(root: &std::path::Path, address: &str, max_turns: u32) -> Agent {
    Agent {
        client: Client::new(address, None, false, Duration::from_secs(5)).unwrap(),
        tools: Tools::new(Engine::new(root, Mode::Default).unwrap()),
        model: "mock".into(),
        system: "test".into(),
        max_turns,
        max_tokens: 128,
        compaction: Compaction::new(200_000, 0.9),
    }
}

#[tokio::test]
async fn default_allows_more_than_fifty_tool_steps_without_continuation() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("example.txt"), "example").unwrap();
    let (address, server) = scripted_server(53, true).await;
    let mut agent = agent(root.path(), &address, Config::default().max_turns);
    let outcome = tokio::time::timeout(
        Duration::from_secs(15),
        agent.run(vec![Message::text(Role::User, "work")], None),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(outcome.text, "finished");
    assert_eq!(outcome.requests, 53);
    assert_eq!(
        outcome
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter(|c| matches!(
                c,
                Content::ToolResult {
                    is_error: false,
                    ..
                }
            ))
            .count(),
        52
    );
    server.await.unwrap();
}

#[tokio::test]
async fn explicit_limit_still_stops_and_retains_completed_tool_results() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("example.txt"), "example").unwrap();
    let (address, server) = scripted_server(2, false).await;
    let mut agent = agent(root.path(), &address, 2);
    let history = vec![Message::text(Role::User, "work")];
    let checkpoint = TurnCheckpoint::new(history.clone());
    let error = agent
        .run_with_checkpoint(history, None, Some(&checkpoint))
        .await
        .unwrap_err();
    assert!(error.contains("turn limit (2)"));
    let recovered = checkpoint.failed("");
    assert_eq!(
        recovered
            .iter()
            .flat_map(|m| &m.content)
            .filter(|c| matches!(
                c,
                Content::ToolResult {
                    is_error: false,
                    ..
                }
            ))
            .count(),
        2
    );
    server.await.unwrap();
}
