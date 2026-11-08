//! Real TCP acceptance coverage for bounded retries and replay isolation.
#![allow(clippy::unwrap_used)]

use gitcode::provider::{
    canonical::{Event, Message, Request, Role},
    transport::Client,
};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};

fn request() -> Request {
    Request {
        model: "mock".into(),
        system: "test".into(),
        messages: vec![Message::text(Role::User, "hello")],
        tools: vec![],
        max_tokens: 32,
    }
}

fn client(address: std::net::SocketAddr) -> Client {
    Client::new(
        &format!("http://{address}"),
        None,
        false,
        Duration::from_secs(8),
    )
    .unwrap()
}

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

fn prefix(text: &str) -> String {
    format!("data: {{\"type\":\"message_start\",\"message\":{{}}}}\n\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{text}\"}}}}\n\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\ndata: {{\"type\":\"content_block_start\",\"index\":1,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"call\",\"name\":\"Write\",\"input\":{{\"value\":\"{text}\"}}}}}}\n\ndata: {{\"type\":\"content_block_stop\",\"index\":1}}\n\n")
}

fn complete(text: &str) -> String {
    format!("{}data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"tool_use\"}},\"usage\":{{\"output_tokens\":2}}}}\n\ndata: {{\"type\":\"message_stop\"}}\n\n", prefix(text))
}

async fn respond(socket: &mut TcpStream, body: &str, extra_length: usize) {
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len() + extra_length).as_bytes()).await.unwrap();
    socket.shutdown().await.unwrap();
}

#[tokio::test]
async fn rate_limit_then_success_honors_retry_after() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        let started = tokio::time::Instant::now();
        socket.write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        drop(socket);
        let (mut socket, _) = listener.accept().await.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(950));
        read_request(&mut socket).await;
        respond(&mut socket, &complete("success"), 0).await;
    });
    assert!(client(address)
        .run(&request(), None)
        .await
        .unwrap()
        .iter()
        .any(|e| matches!(e, Event::Done { .. })));
    server.await.unwrap();
}

async fn replay(extra_length: usize, trailing: &str) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let trailing = trailing.to_owned();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        respond(
            &mut socket,
            &(prefix("provisional") + &trailing),
            extra_length,
        )
        .await;
        drop(socket);
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        respond(&mut socket, &complete("accepted"), 0).await;
    });
    let (tx, mut rx) = mpsc::channel(64);
    let events = client(address).run(&request(), Some(&tx)).await.unwrap();
    drop(tx);
    let mut live = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            Event::TextDelta { text } => live.push_str(&text),
            Event::ToolCall { .. } | Event::Done { .. } => {
                panic!("executable event escaped validation")
            }
            _ => {}
        }
    }
    assert_eq!(live, "provisional");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::ToolCall { .. }))
            .count(),
        1
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::ToolCall { input, .. } if input["value"] == "accepted")));
    assert!(!events
        .iter()
        .any(|e| matches!(e, Event::TextDelta { text } if text == "provisional")));
    server.await.unwrap();
}

#[tokio::test]
async fn missing_terminal_event_retries_without_duplicate_tools_or_progress() {
    replay(0, "").await;
}

#[tokio::test]
async fn dropped_http_body_retries_without_duplicate_tools_or_progress() {
    replay(100, "").await;
}

#[tokio::test]
async fn truncated_sse_record_retries_without_duplicate_tools_or_progress() {
    replay(0, "data: {\"type\":").await;
}

#[tokio::test]
async fn connection_reset_before_headers_then_success() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        drop(socket);
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        respond(&mut socket, &complete("accepted"), 0).await;
    });
    assert!(client(address).run(&request(), None).await.is_ok());
    server.await.unwrap();
}

#[tokio::test]
async fn repeated_drops_exhaust_exactly_four_attempts() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let accepted = count.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            accepted.fetch_add(1, Ordering::SeqCst);
            read_request(&mut socket).await;
            respond(&mut socket, &prefix("incomplete"), 100).await;
        }
    });
    assert_eq!(
        client(address).run(&request(), None).await.unwrap_err(),
        "provider stream disconnected"
    );
    assert_eq!(count.load(Ordering::SeqCst), 4);
    server.abort();
}

#[tokio::test]
async fn cancellation_during_backoff_never_opens_next_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        socket.write_all(b"HTTP/1.1 503 Unavailable\r\nRetry-After: 300\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        drop(socket);
        ready.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(600), listener.accept())
                .await
                .is_err()
        );
    });
    let run = tokio::spawn(async move { client(address).run(&request(), None).await });
    waiting.await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    run.abort();
    assert!(run.await.unwrap_err().is_cancelled());
    server.await.unwrap();
}

#[tokio::test]
async fn explicit_stream_cancellation_closes_socket_without_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
        ready.send(()).unwrap();
        let mut byte = [0; 1];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(600), listener.accept())
                .await
                .is_err()
        );
    });
    let run = tokio::spawn(async move { client(address).run(&request(), None).await });
    waiting.await.unwrap();
    run.abort();
    assert!(run.await.unwrap_err().is_cancelled());
    server.await.unwrap();
}
