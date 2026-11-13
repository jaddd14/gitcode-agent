//! Interactive approval covers edits as well as shell commands.
#![allow(clippy::unwrap_used)]
use gitcode::{
    agent::{Agent, ShellApproval, BASE_PROMPT},
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
    sync::mpsc,
};

#[tokio::test]
async fn approved_write_runs_once_and_rejected_write_does_not_run() {
    let root = std::env::temp_dir().join(format!("gitcode-edit-approval-{}", std::process::id()));
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
                if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let size: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().ok())
                                .flatten()
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + size {
                        break;
                    }
                }
            }
            let body = if turn == 0 {
                concat!(
                    "data: {\"type\":\"message_start\",\"message\":{}}\n\n",
                    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"ok\",\"name\":\"Write\",\"input\":{\"path\":\"accepted.txt\",\"content\":\"accepted\"}}}\n\n",
                    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                    "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"no\",\"name\":\"Write\",\"input\":{\"path\":\"rejected.txt\",\"content\":\"rejected\"}}}\n\n",
                    "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
                    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
                    "data: {\"type\":\"message_stop\"}\n\n")
            } else {
                concat!("data: {\"type\":\"message_start\",\"message\":{}}\n\n", "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n", "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n", "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n", "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n", "data: {\"type\":\"message_stop\"}\n\n")
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
    let mut agent = Agent {
        client: Client::new(
            &format!("http://{address}"),
            None,
            false,
            Duration::from_secs(5),
        )
        .unwrap(),
        tools: Tools::new(Engine::new(&root, Mode::Default).unwrap()),
        model: "fixture".into(),
        system: BASE_PROMPT.into(),
        max_turns: 3,
        max_tokens: 128,
    };
    let (sender, mut requests) = mpsc::channel(4);
    let run = tokio::spawn(async move {
        agent
            .run_with_approvals(
                vec![Message::text(Role::User, "edit")],
                None,
                None,
                None,
                Some(&sender),
            )
            .await
    });
    let first = requests.recv().await.unwrap();
    assert!(first.command.contains("accepted.txt"));
    first.reply.send(ShellApproval::Once).unwrap();
    let second = requests.recv().await.unwrap();
    assert!(second.command.contains("rejected.txt"));
    second.reply.send(ShellApproval::Deny).unwrap();
    let outcome = run.await.unwrap().unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("accepted.txt")).unwrap(),
        "accepted"
    );
    assert!(!root.join("rejected.txt").exists());
    assert!(outcome.messages.iter().flat_map(|m| &m.content).any(|c| matches!(c, Content::ToolResult { tool_use_id, is_error: true, .. } if tool_use_id == "no")));
    server.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
