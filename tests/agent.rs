//! Scripted model-tool-model conversation over an actual local HTTP endpoint.
#![allow(clippy::unwrap_used)]

use gitcode::{
    agent::{Agent, BASE_PROMPT},
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

#[tokio::test]
async fn scripted_read_then_answer_retains_tool_result() {
    let root = std::env::temp_dir().join(format!("gitcode-agent-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("example.txt"), "verified workspace content\n").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for turn in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    let len = header
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|v| v.parse::<usize>().ok())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + len {
                        if turn == 1 {
                            assert!(String::from_utf8_lossy(&bytes[end + 4..])
                                .contains("verified workspace content"));
                        }
                        break;
                    }
                }
            }
            let body = if turn == 0 {
                "data: {\"type\":\"message_start\",\"message\":{}}\n\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"read_1\",\"name\":\"Read\",\"input\":{\"path\":\"example.txt\"}}}\n\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n"
            } else {
                "data: {\"type\":\"message_start\",\"message\":{}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Проверил файл.\"}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n"
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
    let client = Client::new(
        &format!("http://{address}"),
        None,
        false,
        Duration::from_secs(5),
    )
    .unwrap();
    let mut agent = Agent {
        client,
        tools: Tools::new(Engine::new(&root, Mode::Default).unwrap()),
        model: "mock".into(),
        system: BASE_PROMPT.into(),
        max_turns: 4,
        max_tokens: 128,
    };
    let outcome = agent
        .run(vec![Message::text(Role::User, "read example")], None)
        .await
        .unwrap();
    assert_eq!(outcome.text, "Проверил файл.");
    assert_eq!(outcome.requests, 2);
    assert!(outcome
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .any(|c| matches!(
            c,
            Content::ToolResult {
                is_error: false,
                ..
            }
        )));
    server.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
