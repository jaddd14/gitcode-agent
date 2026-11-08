//! Live retry and local tool lifecycle events over a loopback provider.
#![allow(clippy::unwrap_used)]

use gitcode::{
    agent::{Agent, BASE_PROMPT},
    config::Protocol,
    credentials::Secret,
    perm::{Engine, Mode, Rule},
    provider::{
        canonical::{Content, Event, Message, Request, Role},
        transport::Client,
    },
    tools::Tools,
};
use serde_json::json;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};

async fn read_request(socket: &mut TcpStream) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let n = socket.read(&mut buffer).await.unwrap();
        assert_ne!(n, 0);
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let size: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            if bytes.len() >= end + 4 + size {
                return;
            }
        }
    }
}

fn completion(calls: serde_json::Value) -> String {
    let tool_turn = !calls.as_array().unwrap().is_empty();
    let delta = if tool_turn {
        json!({"tool_calls": calls})
    } else {
        json!({"content": "done"})
    };
    let chunk = json!({"choices":[{"index":0,"delta":delta,"finish_reason":if tool_turn {"tool_calls"} else {"stop"}}]});
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

async fn respond(socket: &mut TcpStream, body: &str) {
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
}

fn client(address: std::net::SocketAddr) -> Client {
    Client::with_protocol(
        &format!("http://{address}"),
        Some(Secret::new("progress-test-private-key".into())),
        true,
        Duration::from_secs(8),
        Protocol::OpenaiChat,
    )
    .unwrap()
}

fn request() -> Request {
    Request {
        model: "mock".into(),
        system: "test".into(),
        messages: vec![Message::text(Role::User, "hello")],
        tools: vec![],
        max_tokens: 32,
    }
}

#[tokio::test]
async fn retry_notice_is_live_before_backoff_and_contains_only_schedule() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        socket.write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        drop(socket);
        let started = tokio::time::Instant::now();
        let (mut socket, _) = listener.accept().await.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(950));
        read_request(&mut socket).await;
        respond(&mut socket, &completion(json!([]))).await;
    });
    let (tx, mut rx) = mpsc::channel(16);
    let run = tokio::spawn(async move { client(address).run(&request(), Some(&tx)).await });
    let notice = tokio::time::timeout(Duration::from_millis(800), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&notice).unwrap(),
        json!({"type":"retry_notice","attempt":2,"max_attempts":4,"delay_ms":1000})
    );
    assert!(!run.is_finished());
    let events = run.await.unwrap().unwrap();
    assert!(events
        .iter()
        .all(|event| !matches!(event, Event::RetryNotice { .. })));
    assert!(events
        .iter()
        .any(|event| matches!(event, Event::Done { .. })));
    server.await.unwrap();
}

#[tokio::test]
async fn permanent_failure_has_no_retry_notice_or_private_diagnostics() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        let body = "progress-test-private-key";
        socket.write_all(format!("HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        drop(socket);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let (tx, mut rx) = mpsc::channel(16);
    assert_eq!(
        client(address)
            .run(&request(), Some(&tx))
            .await
            .unwrap_err(),
        "provider HTTP status 401"
    );
    drop(tx);
    assert!(rx.recv().await.is_none());
    server.await.unwrap();
}

async fn tool_lifecycle(with_progress: bool, closed_progress: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let root = std::env::temp_dir().join(format!(
        "gitcode-progress-{}-{}",
        std::process::id(),
        address.port()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("example.txt"), "workspace content\n").unwrap();
    let calls = json!([
        {"index":0,"id":"shell_1","type":"function","function":{"name":"Bash","arguments":json!({"command":"sleep 0.2; printf verified"}).to_string()}},
        {"index":1,"id":"read_1","type":"function","function":{"name":"Read","arguments":json!({"path":"example.txt"}).to_string()}},
        {"index":2,"id":"write_1","type":"function","function":{"name":"Write","arguments":json!({"path":"denied.txt","content":"must not be written"}).to_string()}}
    ]);
    let server = tokio::spawn(async move {
        for body in [completion(calls), completion(json!([]))] {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            respond(&mut socket, &body).await;
        }
    });
    let mut permissions = Engine::new(&root, Mode::Default).unwrap();
    permissions.allow.push(Rule::parse("Bash").unwrap());
    let mut agent = Agent {
        client: client(address),
        tools: Tools::new(permissions),
        model: "mock".into(),
        system: BASE_PROMPT.into(),
        max_turns: 3,
        max_tokens: 128,
    };
    let (tx, rx) = mpsc::channel(1);
    let mut rx = if closed_progress {
        drop(rx);
        None
    } else {
        Some(rx)
    };
    let run = tokio::spawn(async move {
        agent
            .run(
                vec![Message::text(Role::User, "execute tools")],
                with_progress.then_some(&tx),
            )
            .await
    });
    let mut lifecycle = Vec::new();
    if with_progress {
        if let Some(rx) = &mut rx {
            while let Some(event) = rx.recv().await {
                match event {
                    Event::ToolStarted { .. } => {
                        if lifecycle.is_empty() {
                            assert!(
                                !run.is_finished(),
                                "start must be delivered before completion"
                            );
                        }
                        lifecycle.push(event);
                    }
                    Event::ToolFinished { .. } => lifecycle.push(event),
                    Event::ToolCall { .. } => panic!("unvalidated call exposed as progress"),
                    _ => {}
                }
            }
        }
    }
    let outcome = run.await.unwrap().unwrap();
    assert_eq!(outcome.text, "done");
    assert_eq!(outcome.requests, 2);
    assert!(!root.join("denied.txt").exists());
    if with_progress && !closed_progress {
        assert_eq!(lifecycle.len(), 6);
        let results: Vec<_> = outcome
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|content| {
                if let Content::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } = content
                {
                    Some((tool_use_id, content, is_error))
                } else {
                    None
                }
            })
            .collect();
        for (index, pair) in lifecycle.as_chunks::<2>().0.iter().enumerate() {
            let Event::ToolStarted { id, name, input } = &pair[0] else {
                panic!("missing start")
            };
            let Event::ToolFinished {
                id: finished_id,
                name: finished_name,
                output,
                is_error,
                duration_ms,
            } = &pair[1]
            else {
                panic!("missing finish")
            };
            assert_eq!((id, name), (finished_id, finished_name));
            assert_eq!((id, output, is_error), results[index]);
            assert_eq!(*is_error, index == 2);
            match index {
                0 => {
                    assert_eq!(input["command"], "sleep 0.2; printf verified");
                    assert!(output.contains("verified"));
                    assert!(*duration_ms >= 150);
                }
                1 => assert!(output.contains("workspace content")),
                2 => assert!(output.contains("approval")),
                _ => unreachable!(),
            }
            let encoded = serde_json::to_value(&pair[1]).unwrap();
            assert_eq!(encoded["type"], "tool_finished");
            assert_eq!(serde_json::from_value::<Event>(encoded).unwrap(), pair[1]);
        }
    }
    server.await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn tool_events_bracket_real_execution_success_and_denial() {
    tool_lifecycle(true, false).await;
}

#[tokio::test]
async fn tools_work_without_progress_or_with_closed_receiver() {
    tool_lifecycle(false, false).await;
    tool_lifecycle(true, true).await;
}
