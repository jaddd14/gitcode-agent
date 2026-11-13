//! Reasoning effort reaches each protocol's actual HTTP request body.
#![allow(clippy::unwrap_used)]
use gitcode::{
    config::Protocol,
    provider::{
        canonical::{Message, Request, Role},
        transport::Client,
    },
};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn selected_effort_is_sent_and_default_omits_it() {
    for protocol in [
        Protocol::Anthropic,
        Protocol::OpenaiChat,
        Protocol::OpenaiResponses,
    ] {
        for chosen in [Some("xhigh"), None] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut data = Vec::new();
                let mut chunk = [0; 4096];
                let (end, length) = loop {
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    data.extend_from_slice(&chunk[..n]);
                    if let Some(end) = data.windows(4).position(|x| x == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
                        if data.len() >= end + 4 + length {
                            break (end, length);
                        }
                    }
                };
                let body: Value = serde_json::from_slice(&data[end + 4..end + 4 + length]).unwrap();
                socket.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                body
            });
            let mut client = Client::with_protocol(
                &format!("http://{address}"),
                None,
                false,
                Duration::from_secs(5),
                protocol,
            )
            .unwrap();
            client.set_effort(chosen).unwrap();
            assert!(client.set_effort(Some("invalid")).is_err());
            assert_eq!(client.effort(), chosen);
            let request = Request {
                model: "test".into(),
                system: "".into(),
                messages: vec![Message::text(Role::User, "hello")],
                tools: vec![],
                max_tokens: 32,
            };
            assert!(client.run(&request, None).await.is_err());
            let body = server.await.unwrap();
            let actual = match protocol {
                Protocol::Anthropic => body.pointer("/output_config/effort"),
                Protocol::OpenaiChat => body.get("reasoning_effort"),
                Protocol::OpenaiResponses => body.pointer("/reasoning/effort"),
            };
            assert_eq!(actual, chosen.map(|x| json!(x)).as_ref());
        }
    }
}
