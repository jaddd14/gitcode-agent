//! OpenAI fixtures test fragmentation, accounting, request encoding and failures.
#![allow(clippy::unwrap_used)]

use gitcode::{
    config::Protocol,
    provider::{
        canonical::{Event, Message, Request, Role},
        openai_chat, openai_responses,
        registry::{apply_chat_quirks, presets, Quirks},
        sse::{Decoder, Record},
        transport::Client,
    },
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const CHAT: &str = include_str!("../testdata/openai-chat-tool.sse");
const RESPONSES: &str = include_str!("../testdata/openai-responses-tool.sse");

fn request() -> Request {
    Request {
        model: "mock".into(),
        system: "Russian.".into(),
        messages: vec![Message::text(Role::User, "read")],
        tools: vec![],
        max_tokens: 32,
    }
}

#[test]
fn every_boundary_chat_and_responses() {
    for (fixture, chat) in [(CHAT, true), (RESPONSES, false)] {
        for boundary in 0..=fixture.len() {
            let mut decoder = Decoder::default();
            let mut c = openai_chat::Stream::default();
            let mut r = openai_responses::Stream::default();
            let mut events = Vec::new();
            for chunk in [
                &fixture.as_bytes()[..boundary],
                &fixture.as_bytes()[boundary..],
            ] {
                for record in decoder.push(chunk).unwrap() {
                    events.extend(
                        if chat {
                            c.accept(record)
                        } else {
                            r.accept(record)
                        }
                        .unwrap(),
                    );
                }
            }
            decoder.finish().unwrap();
            if chat {
                c.finish().unwrap();
            } else {
                r.finish().unwrap();
            }
            assert!(events.iter().any(|e|matches!(e,Event::ToolCall{name,input,..}if name=="Read"&&input["path"]=="src/main.rs")));
            assert!(events.iter().any(|e|matches!(e,Event::Usage{usage}if usage.input_tokens==8&&usage.output_tokens==8&&usage.cache_read_tokens==4)));
        }
    }
}

#[test]
fn malformed_and_incomplete_turns_fail() {
    assert!(openai_chat::Stream::default().finish().is_err());
    assert!(openai_chat::Stream::default()
        .accept(Record {
            event: String::new(),
            data: "[DONE]".into()
        })
        .is_err());
    assert!(openai_responses::Stream::default().finish().is_err());
    assert!(openai_responses::Stream::default()
        .accept(Record {
            event: String::new(),
            data: r#"{"type":"response.incomplete"}"#.into()
        })
        .is_err());
}

#[test]
fn encoders_and_quirks_preserve_contract() {
    let mut chat = openai_chat::encode(&request()).unwrap();
    assert_eq!(chat["stream_options"]["include_usage"], true);
    apply_chat_quirks(
        &mut chat,
        &Quirks {
            max_completion_tokens: true,
            omit_stream_options: true,
            omit_parallel_tool_calls: true,
        },
    );
    assert!(chat.get("max_tokens").is_none());
    assert_eq!(chat["max_completion_tokens"], 32);
    assert!(chat.get("stream_options").is_none());
    let responses = openai_responses::encode(&request()).unwrap();
    assert_eq!(responses["store"], false);
    assert_eq!(responses["include"][0], "reasoning.encrypted_content");
    assert_eq!(presets().len(), 9);
}

#[tokio::test]
async fn both_openai_protocols_use_real_http() {
    for (protocol, fixture, path) in [
        (Protocol::OpenaiChat, CHAT, "/v1/chat/completions"),
        (Protocol::OpenaiResponses, RESPONSES, "/v1/responses"),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
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
                        assert!(header.starts_with(&format!("POST {path} ")));
                        break;
                    }
                }
            }
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",fixture.len()).as_bytes()).await.unwrap();
            for chunk in fixture.as_bytes().chunks(3) {
                socket.write_all(chunk).await.unwrap();
            }
        });
        let client = Client::with_protocol(
            &format!("http://{address}/v1"),
            None,
            true,
            Duration::from_secs(3),
            protocol,
        )
        .unwrap();
        assert!(client
            .run(&request(), None)
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(e, Event::ToolCall { .. })));
        server.await.unwrap();
    }
}

#[test]
fn responses_incomplete_reasons_are_distinguished_without_accepting_partial_output() {
    let record = |reason: &str| {
        Record { event: String::new(), data: serde_json::json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":reason}}}).to_string() }
    };
    assert_eq!(
        openai_responses::Stream::default()
            .accept(record("max_output_tokens"))
            .unwrap_err(),
        "Responses output token limit reached"
    );
    assert_eq!(
        openai_responses::Stream::default()
            .accept(record("content_filter"))
            .unwrap_err(),
        "Responses content filtered"
    );
}
