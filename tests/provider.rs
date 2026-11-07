//! Mock TCP provider exercises HTTP framing and fragmented SSE without live keys.
#![allow(clippy::unwrap_used)]

use gitcode::provider::{
    anthropic,
    canonical::{Event, Message, Request, Role},
    sse::Decoder,
    transport::Client,
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const FIXTURE: &str = include_str!("../testdata/anthropic-tool.sse");

fn request() -> Request {
    Request {
        model: "mock".into(),
        system: "Reply in Russian.".into(),
        messages: vec![Message::text(Role::User, "read the file")],
        tools: vec![],
        max_tokens: 1024,
    }
}

#[test]
fn fixture_reassembles_arguments_at_every_boundary() {
    for boundary in 0..=FIXTURE.len() {
        let mut decoder = Decoder::default();
        let mut stream = anthropic::Stream::default();
        let mut events = Vec::new();
        for chunk in [
            &FIXTURE.as_bytes()[..boundary],
            &FIXTURE.as_bytes()[boundary..],
        ] {
            for record in decoder.push(chunk).unwrap() {
                events.extend(stream.accept(record).unwrap());
            }
        }
        decoder.finish().unwrap();
        stream.finish().unwrap();
        assert!(events.iter().any(|e| matches!(e,Event::ToolCall {name,input,..} if name == "Read" && input["path"] == "src/main.rs")));
        assert!(events.iter().any(|e| matches!(e,Event::Usage {usage} if usage.input_tokens == 12 && usage.output_tokens == 8 && usage.cache_read_tokens == 4)));
    }
}

#[tokio::test]
async fn real_http_transport_parses_mock_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
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
                    assert!(header.starts_with("POST /v1/messages "));
                    assert!(header.contains("anthropic-version: 2023-06-01"));
                    break;
                }
            }
        }
        let header = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",FIXTURE.len());
        socket.write_all(header.as_bytes()).await.unwrap();
        for chunk in FIXTURE.as_bytes().chunks(7) {
            socket.write_all(chunk).await.unwrap();
        }
    });
    let client = Client::new(
        &format!("http://{address}"),
        None,
        false,
        Duration::from_secs(5),
    )
    .unwrap();
    let events = client.run(&request(), None).await.unwrap();
    assert!(matches!(events.last(),Some(Event::Done {reason}) if reason == "tool_use"));
    server.await.unwrap();
}

#[test]
fn missing_stop_is_not_executable() {
    let mut stream = anthropic::Stream::default();
    let mut decoder = Decoder::default();
    for record in decoder
        .push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\n")
        .unwrap()
    {
        stream.accept(record).unwrap();
    }
    assert!(stream.finish().is_err());
}
