//! Exercise real parent/child model loops with a local scripted provider.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use gitcode::{
    agent::{Agent, BASE_PROMPT},
    perm::{Engine, Mode},
    provider::{
        canonical::{Content, Event, Message, Role},
        transport::Client,
    },
    tools::Tools,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Barrier,
};

fn tool_reply(calls: &[(&str, &str, serde_json::Value)]) -> String {
    let mut s = "data: {\"type\":\"message_start\",\"message\":{}}\n\n".to_string();
    for (index, (id, name, input)) in calls.iter().enumerate() {
        s.push_str(&format!("data: {}\n\n", serde_json::json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":id,"name":name,"input":input}})));
        s.push_str(&format!(
            "data: {}\n\n",
            serde_json::json!({"type":"content_block_stop","index":index})
        ));
    }
    s.push_str("data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n");
    s
}
fn text_reply(text: &str) -> String {
    format!("data: {{\"type\":\"message_start\",\"message\":{{}}}}\n\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\ndata: {}\n\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}}}}\n\ndata: {{\"type\":\"message_stop\"}}\n\n", serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}))
}

#[tokio::test]
async fn children_run_in_parallel_with_isolated_context_and_enforced_read_only_tools() {
    let root = std::env::temp_dir().join(format!("gitcode-subagents-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let server = tokio::spawn(async move {
        let mut jobs = Vec::new();
        for _ in 0..5 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let barrier = barrier.clone();
            jobs.push(tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buf = [0; 8192];
                let body = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                        let len = String::from_utf8_lossy(&bytes[..end]).lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").and_then(|v| v.parse::<usize>().ok())).unwrap();
                        if bytes.len() >= end + 4 + len { break serde_json::from_slice::<serde_json::Value>(&bytes[end+4..end+4+len]).unwrap(); }
                    }
                };
                let messages = body["messages"].as_array().unwrap();
                let first = messages[0]["content"].to_string();
                let response = if first.contains("PARENT_REQUEST") {
                    if messages.len() == 1 {
                        assert!(body["tools"].as_array().unwrap().iter().any(|v| v["name"] == "Agent"));
                        tool_reply(&[
                            ("a1", "Agent", serde_json::json!({"subagent_type":"explore","description":"alpha","prompt":"TASK_ALPHA"})),
                            ("a2", "Agent", serde_json::json!({"subagent_type":"explore","description":"beta","prompt":"TASK_BETA"})),
                        ])
                    } else {
                        let all = body["messages"].to_string();
                        assert!(all.contains("alpha report") && all.contains("beta report"));
                        assert!(!all.contains("private child detail"));
                        text_reply("parent final")
                    }
                } else {
                    let definitions = body["tools"].as_array().unwrap();
                    assert!(!definitions.iter().any(|v| v["name"] == "Agent" || v["name"] == "Write"));
                    if messages.len() == 1 {
                        tokio::time::timeout(Duration::from_secs(5), barrier.wait()).await.expect("children must run concurrently");
                        if first.contains("TASK_ALPHA") {
                            tool_reply(&[("evil", "Write", serde_json::json!({"path":"blocked.txt","content":"forbidden"})), ("nested", "Agent", serde_json::json!({"subagent_type":"explore","description":"nested","prompt":"nested"}))])
                        } else {
                            assert!(first.contains("TASK_BETA"));
                            text_reply("beta report")
                        }
                    } else {
                        assert!(first.contains("TASK_ALPHA"));
                        assert!(body["messages"].to_string().contains("Tool Write is not permitted"));
                        assert!(body["messages"].to_string().contains("Tool Agent is not permitted"));
                        text_reply("alpha report")
                    }
                };
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
            }));
        }
        for job in jobs {
            job.await.unwrap();
        }
    });
    let client = Client::new(
        &format!("http://{address}"),
        None,
        false,
        Duration::from_secs(10),
    )
    .unwrap();
    let mut agent = Agent {
        client,
        tools: Tools::new(Engine::new(&root, Mode::Bypass).unwrap()),
        model: "mock".into(),
        system: BASE_PROMPT.into(),
        max_turns: 4,
        max_tokens: 128,
        compaction: gitcode::agent::context::Compaction::new(200_000, 0.9),
    };
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let outcome = tokio::time::timeout(
        Duration::from_secs(12),
        agent.run(
            vec![Message::text(Role::User, "PARENT_REQUEST")],
            Some(&sender),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    drop(sender);
    let mut started = Vec::new();
    let mut finished = Vec::new();
    while let Some(event) = receiver.recv().await {
        match event {
            Event::SubagentStarted { id, .. } => started.push(id),
            Event::SubagentFinished { id, success, .. } => {
                assert!(success);
                finished.push(id);
            }
            _ => {}
        }
    }
    started.sort();
    finished.sort();
    assert_eq!(started, ["a1", "a2"]);
    assert_eq!(finished, ["a1", "a2"]);
    assert_eq!(outcome.text, "parent final");
    assert!(!root.join("blocked.txt").exists());
    let results = &outcome.messages[2].content;
    assert!(
        matches!(&results[0], Content::ToolResult { tool_use_id, content, .. } if tool_use_id == "a1" && content == "alpha report")
    );
    assert!(
        matches!(&results[1], Content::ToolResult { tool_use_id, content, .. } if tool_use_id == "a2" && content == "beta report")
    );
    server.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn interrupt_keeps_completed_child_and_marks_only_the_unfinished_child() {
    use gitcode::agent::TurnCheckpoint;
    let root = std::env::temp_dir().join(format!("gitcode-child-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let server = tokio::spawn(async move {
        for _ in 0..3 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buf = [0; 8192];
                let body = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                        let len = String::from_utf8_lossy(&bytes[..end])
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|v| v.parse::<usize>().ok())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + len {
                            break serde_json::from_slice::<serde_json::Value>(
                                &bytes[end + 4..end + 4 + len],
                            )
                            .unwrap();
                        }
                    }
                };
                let text = body["messages"][0]["content"].to_string();
                if !text.contains("PARENT_CANCEL") {
                    barrier.wait().await;
                }
                let response = if text.contains("PARENT_CANCEL") {
                    tool_reply(&[
                        (
                            "fast",
                            "Agent",
                            serde_json::json!({"subagent_type":"explore","description":"fast","prompt":"TASK_FAST"}),
                        ),
                        (
                            "slow",
                            "Agent",
                            serde_json::json!({"subagent_type":"explore","description":"slow","prompt":"TASK_SLOW"}),
                        ),
                    ])
                } else if text.contains("TASK_FAST") {
                    text_reply("finished child")
                } else {
                    std::future::pending::<()>().await;
                    return;
                };
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
            });
        }
    });
    let client = Client::new(
        &format!("http://{address}"),
        None,
        false,
        Duration::from_secs(20),
    )
    .unwrap();
    let mut agent = Agent {
        client,
        tools: Tools::new(Engine::new(&root, Mode::Bypass).unwrap()),
        model: "mock".into(),
        system: BASE_PROMPT.into(),
        max_turns: 4,
        max_tokens: 128,
        compaction: gitcode::agent::context::Compaction::new(200_000, 0.9),
    };
    let initial = vec![Message::text(Role::User, "PARENT_CANCEL")];
    let checkpoint = TurnCheckpoint::new(initial.clone());
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let mut run = Box::pin(agent.run_with_checkpoint(initial, Some(&sender), Some(&checkpoint)));
    loop {
        tokio::select! {
            result = &mut run => panic!("finished unexpectedly: {result:?}"),
            event = receiver.recv() => if matches!(event, Some(Event::ToolFinished { ref id, .. }) if id == "fast") { break; },
            _ = tokio::time::sleep(Duration::from_secs(10)) => panic!("timeout"),
        }
    }
    drop(run);
    let recovered = checkpoint.interrupted("");
    let results = &recovered[2].content;
    assert!(
        matches!(&results[0], Content::ToolResult { tool_use_id, content, is_error: false } if tool_use_id == "fast" && content == "finished child")
    );
    assert!(
        matches!(&results[1], Content::ToolResult { tool_use_id, is_error: true, .. } if tool_use_id == "slow")
    );
    server.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
