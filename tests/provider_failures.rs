//! Transport failures must not leak credentials or execute incomplete calls.
#![allow(clippy::unwrap_used)]

use gitcode::{
    credentials::Secret,
    provider::{
        canonical::{Message, Request, Role},
        transport::Client,
    },
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
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

#[tokio::test]
async fn authentication_error_is_safe_and_not_retried() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 8192];
        let _ = socket.read(&mut buffer).await.unwrap();
        let body = "credential-sensitive-value";
        socket.write_all(format!("HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    });
    let client = Client::new(
        &format!("http://{address}"),
        Some(Secret::new("credential-sensitive-value".into())),
        false,
        Duration::from_secs(2),
    )
    .unwrap();
    let error = client.run(&request(), None).await.unwrap_err();
    assert_eq!(error, "provider HTTP status 401");
    server.await.unwrap();
}

#[tokio::test]
async fn incomplete_success_body_is_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 8192];
        let _ = socket.read(&mut buffer).await.unwrap();
        let body = "data: {\"type\":\"message_start\",\"message\":{}}\n\n";
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
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(client.run(&request(), None).await.is_err());
    server.await.unwrap();
}

#[tokio::test]
async fn hard_timeout_cancels_the_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 8192];
        let _ = socket.read(&mut buffer).await.unwrap();
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
        // Hold the connection open until cancellation closes the peer.
        loop {
            match socket.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                _ => {}
            }
        }
    });
    let client = Client::new(
        &format!("http://{address}"),
        None,
        false,
        Duration::from_millis(100),
    )
    .unwrap();
    assert_eq!(
        client.run(&request(), None).await.unwrap_err(),
        "provider request timed out"
    );
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}
