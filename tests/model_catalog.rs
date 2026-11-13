//! Provider catalog pagination and authentication over a real local HTTP server.
#![allow(clippy::unwrap_used)]
use gitcode::{config::Protocol, credentials::Secret, provider::transport::Client};
use serde_json::json;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

async fn request(socket: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut buf = [0; 4096];
    loop {
        let n = socket.read(&mut buf).await.unwrap();
        assert!(n > 0);
        data.extend_from_slice(&buf[..n]);
        if data.windows(4).any(|w| w == b"\r\n\r\n") {
            return String::from_utf8(data).unwrap().to_ascii_lowercase();
        }
    }
}
async fn respond(socket: &mut TcpStream, status: &str, body: &str) {
    let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    socket.write_all(response.as_bytes()).await.unwrap();
}
#[tokio::test]
async fn loads_every_page_with_current_anthropic_credential() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for page in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let headers = request(&mut socket).await;
            assert!(headers.starts_with("get /v1/models"));
            assert!(headers.contains("x-api-key: test-key\r\n"));
            if page == 1 {
                assert!(headers.starts_with("get /v1/models?after_id=model-199 "));
            }
            let start = page * 200;
            let data: Vec<_> = (start..start + 200)
                .map(|i| json!({"id": format!("model-{i:03}")}))
                .collect();
            let body = json!({"data": data, "has_more": page == 0, "last_id": format!("model-{:03}", start+199)}).to_string();
            respond(&mut socket, "200 OK", &body).await;
        }
    });
    let client = Client::with_protocol(
        &format!("http://{address}"),
        Some(Secret::new("test-key".into())),
        false,
        Duration::from_secs(5),
        Protocol::Anthropic,
    )
    .unwrap();
    let models = client.list_models().await.unwrap();
    assert_eq!(models.len(), 400);
    assert_eq!(models[399], "model-399");
    server.await.unwrap();
}
#[tokio::test]
async fn retries_with_bearer_for_compatible_model_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        assert!(request(&mut first)
            .await
            .contains("x-api-key: test-key\r\n"));
        respond(&mut first, "401 Unauthorized", "{}").await;
        let (mut second, _) = listener.accept().await.unwrap();
        assert!(request(&mut second)
            .await
            .contains("authorization: bearer test-key\r\n"));
        respond(&mut second, "200 OK", r#"{"data":[{"id":"model"}]}"#).await;
    });
    let client = Client::with_protocol(
        &format!("http://{address}"),
        Some(Secret::new("test-key".into())),
        false,
        Duration::from_secs(5),
        Protocol::Anthropic,
    )
    .unwrap();
    assert_eq!(client.list_models().await.unwrap(), vec!["model"]);
    server.await.unwrap();
}
