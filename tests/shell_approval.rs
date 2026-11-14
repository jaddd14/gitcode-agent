//! Approval travels from the agent to the operator before any shell side effects.
#![allow(clippy::unwrap_used)]
use gitcode::{
    agent::{Agent, ShellApproval, BASE_PROMPT},
    perm::{Engine, Mode, Rule},
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
    sync::mpsc,
};

#[tokio::test]
async fn approval_choices_remember_prefix_but_preserve_explicit_rules() {
    let root = std::env::temp_dir().join(format!("gitcode-approval-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for turn in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 8192];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&bytes[..end]);
                    let len: usize = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|v| v.parse().ok())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            let body = if turn == 0 {
                let mut body = "data: {\"type\":\"message_start\",\"message\":{}}\n\n".to_owned();
                for (index, command) in [
                    "mkdir -p first",
                    "mkdir -p second",
                    "mkdir -p forced",
                    "touch once",
                    "touch rejected",
                    "touch blocked; pwd",
                ]
                .iter()
                .enumerate()
                {
                    body.push_str(&format!("data: {{\"type\":\"content_block_start\",\"index\":{index},\"content_block\":{{\"type\":\"tool_use\",\"id\":\"tool_{index}\",\"name\":\"Bash\",\"input\":{{\"command\":\"{command}\"}}}}}}\n\ndata: {{\"type\":\"content_block_stop\",\"index\":{index}}}\n\n"));
                }
                body.push_str("data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n");
                body
            } else {
                "data: {\"type\":\"message_start\",\"message\":{}}\n\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n".to_owned()
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
    let mut engine = Engine::new(&root, Mode::Default).unwrap();
    engine
        .ask
        .push(Rule::parse("Bash(mkdir -p forced)").unwrap());
    engine
        .deny
        .push(Rule::parse("Bash(touch blocked)").unwrap());
    let mut agent = Agent {
        client: Client::new(
            &format!("http://{address}"),
            None,
            false,
            Duration::from_secs(5),
        )
        .unwrap(),
        tools: Tools::new(engine),
        model: "mock".into(),
        system: BASE_PROMPT.into(),
        max_turns: 3,
        max_tokens: 128,
        compaction: gitcode::agent::context::Compaction::new(200_000, 0.9),
    };
    let (sender, mut approvals) = mpsc::channel(1);
    let run = agent.run_with_approvals(
        vec![Message::text(Role::User, "test")],
        None,
        None,
        None,
        Some(&sender),
    );
    let handle = async {
        let expected = [
            ("mkdir -p first", ShellApproval::Similar),
            ("mkdir -p forced", ShellApproval::Deny),
            ("touch once", ShellApproval::Once),
            ("touch rejected", ShellApproval::Deny),
        ];
        for (command, answer) in expected {
            let request = tokio::time::timeout(Duration::from_secs(5), approvals.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(request.command, command);
            if command == "mkdir -p first" {
                assert_eq!(request.prefix.as_deref(), Some("mkdir -p"));
            }
            assert!(!root.join("first").exists() || command != "mkdir -p first");
            request.reply.send(answer).unwrap();
        }
    };
    let (outcome, ()) = tokio::join!(run, handle);
    let outcome = outcome.unwrap();
    assert!(root.join("first").is_dir());
    assert!(root.join("second").is_dir());
    assert!(root.join("once").is_file());
    for path in ["forced", "rejected", "blocked"] {
        assert!(!root.join(path).exists());
    }
    let errors = outcome
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|c| matches!(c, Content::ToolResult { is_error: true, .. }))
        .count();
    assert_eq!(errors, 3);
    server.await.unwrap();
    agent.tools.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
