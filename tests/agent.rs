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
                "data: {\"type\":\"message_start\",\"message\":{}}\n\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Проверил файл.\"}}\n\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n"
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
    let (progress, mut receiver) = tokio::sync::mpsc::channel(32);
    let outcome = agent
        .run(
            vec![Message::text(Role::User, "read example")],
            Some(&progress),
        )
        .await
        .unwrap();
    drop(progress);
    let mut boundaries = Vec::new();
    let mut previous_was_usage = false;
    while let Some(event) = receiver.recv().await {
        match event {
            gitcode::provider::canonical::Event::Done { reason } => {
                assert!(previous_was_usage, "request boundary requires final usage");
                boundaries.push(reason);
                previous_was_usage = false;
            }
            gitcode::provider::canonical::Event::Usage { .. } => previous_was_usage = true,
            _ => previous_was_usage = false,
        }
    }
    assert_eq!(boundaries, ["tool_use", "end_turn"]);
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

#[tokio::test]
async fn interrupted_turn_keeps_request_completed_tools_and_marks_running_tool() {
    use gitcode::agent::TurnCheckpoint;
    use gitcode::provider::canonical::{Event, Request};

    let root = std::env::temp_dir().join(format!("gitcode-interrupt-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("example.txt"), "important discovery\n").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = vec![0; 65536];
        socket.read(&mut bytes).await.unwrap();
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"read_1\",\"name\":\"Read\",\"input\":{\"path\":\"example.txt\"}}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"bash_1\",\"name\":\"Bash\",\"input\":{\"command\":\"sleep 30\"}}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
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
        tools: Tools::new(Engine::new(&root, Mode::Bypass).unwrap()),
        model: "mock".into(),
        system: BASE_PROMPT.into(),
        max_turns: 4,
        max_tokens: 128,
    };
    let initial = vec![Message::text(Role::User, "посмотри dotagent")];
    let checkpoint = TurnCheckpoint::new(initial.clone());
    let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
    let mut run = Box::pin(agent.run_with_checkpoint(initial, Some(&sender), Some(&checkpoint)));
    loop {
        tokio::select! {
            result = &mut run => panic!("turn finished before interruption: {result:?}"),
            event = receiver.recv() => if matches!(event, Some(Event::ToolStarted { ref name, .. }) if name == "Bash") { break; },
            _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("timed out waiting for tool"),
        }
    }
    drop(run);
    let mut recovered = checkpoint.interrupted("");
    assert!(
        matches!(&recovered[0].content[0], Content::Text { text } if text.contains("dotagent"))
    );
    let results = &recovered[2].content;
    assert!(
        matches!(&results[0], Content::ToolResult { content, is_error: false, .. } if content.contains("important discovery"))
    );
    assert!(
        matches!(&results[1], Content::ToolResult { content, is_error: true, .. } if content.contains("Прервано во время"))
    );
    let store = gitcode::session::store::SessionStore::open(&root, &root).unwrap();
    let session = store.create("interrupted", None).unwrap();
    for message in &recovered {
        store.append(&session.id, message).unwrap();
    }
    recovered = store.resume(&session.id).unwrap().messages;
    recovered.push(Message::text(Role::User, "Продолжай"));
    let request = Request {
        model: "mock".into(),
        system: "".into(),
        messages: recovered,
        tools: vec![],
        max_tokens: 128,
    };
    let anthropic = gitcode::provider::anthropic::encode(&request)
        .unwrap()
        .to_string();
    let openai = gitcode::provider::openai_chat::encode(&request)
        .unwrap()
        .to_string();
    let responses = gitcode::provider::openai_responses::encode(&request)
        .unwrap()
        .to_string();
    for encoded in [anthropic, openai, responses] {
        assert!(
            encoded.contains("dotagent")
                && encoded.contains("important discovery")
                && encoded.contains("Продолжай")
        );
        assert!(encoded.contains("read_1") && encoded.contains("bash_1"));
    }
    server.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn interruption_during_generation_retains_the_partial_answer_and_request() {
    let checkpoint =
        gitcode::agent::TurnCheckpoint::new(vec![Message::text(Role::User, "исследуй проект")]);
    let recovered = checkpoint.interrupted("Уже изучил конфигурацию.");
    assert!(
        matches!(&recovered[0].content[0], Content::Text { text } if text == "исследуй проект")
    );
    assert!(
        matches!(&recovered[1].content[0], Content::Text { text } if text.contains("Уже изучил") && text.contains("прерван"))
    );
}

#[tokio::test]
async fn queued_messages_reach_the_model_after_tools_in_order_without_reexecution() {
    use gitcode::{
        agent::{InputQueue, TurnCheckpoint},
        provider::canonical::Event,
    };
    let root = std::env::temp_dir().join(format!("gitcode-steering-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("example.txt"), "read-result\n").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for turn in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 8192];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                    let len = String::from_utf8_lossy(&bytes[..end])
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|n| n.parse::<usize>().ok())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + len {
                        if turn == 1 {
                            let body = String::from_utf8_lossy(&bytes[end + 4..end + 4 + len]);
                            assert!(body.contains("read-result") && body.contains("first-result"));
                            assert!(
                                body.find("уточнение первое").unwrap()
                                    < body.find("уточнение второе").unwrap()
                            );
                            assert!(!body.contains("после команды"));
                        }
                        break;
                    }
                }
            }
            let body = if turn == 0 {
                concat!(
                "data: {\"type\":\"message_start\",\"message\":{}}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"bash_1\",\"name\":\"Bash\",\"input\":{\"command\":\"sleep 0.2; printf first-result\"}}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"read_1\",\"name\":\"Read\",\"input\":{\"path\":\"example.txt\"}}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            )
            } else {
                concat!(
                "data: {\"type\":\"message_start\",\"message\":{}}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Учёл.\"}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            )
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
        tools: Tools::new(Engine::new(&root, Mode::Bypass).unwrap()),
        model: "mock".into(),
        system: BASE_PROMPT.into(),
        max_turns: 4,
        max_tokens: 128,
    };
    let initial = vec![Message::text(Role::User, "начни")];
    let checkpoint = TurnCheckpoint::new(initial.clone());
    let queue = InputQueue::default();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
    let mut run =
        Box::pin(agent.run_with_steering(initial, Some(&sender), Some(&checkpoint), Some(&queue)));
    let mut finished = Vec::new();
    let outcome = loop {
        tokio::select! {
            result = &mut run => break result.unwrap(),
            event = receiver.recv() => match event.unwrap() {
                Event::ToolStarted { name, .. } if name == "Bash" => {
                    queue.push_back("уточнение первое".into());
                    queue.push_back("уточнение второе".into());
                    queue.push_back("/status".into());
                    queue.push_back("после команды".into());
                }
                Event::ToolFinished { name, .. } => finished.push(name),
                Event::SteeringApplied { messages } => {
                    assert_eq!(finished, ["Bash", "Read"]);
                    assert_eq!(messages, ["уточнение первое", "уточнение второе"]);
                    let recovered = checkpoint.interrupted("");
                    let text = serde_json::to_string(&recovered).unwrap();
                    assert!(text.contains("уточнение первое") && text.contains("read-result"));
                    assert!(!text.contains("после команды"));
                    queue.acknowledge();
                }
                _ => {}
            },
            _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("timed out"),
        }
    };
    assert_eq!(outcome.text, "Учёл.");
    assert_eq!(queue.pop_front().as_deref(), Some("/status"));
    assert_eq!(queue.pop_front().as_deref(), Some("после команды"));
    assert_eq!(
        outcome
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter(|c| matches!(c, Content::ToolResult { .. }))
            .count(),
        2
    );
    server.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
