//! CLI provider presets must configure both protocol and credential.
#![allow(clippy::unwrap_used)]
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::Command,
};

#[test]
fn provider_preset_selects_protocol_and_credential() {
    let temp = tempfile::tempdir().unwrap();
    let temp_path = temp.path().canonicalize().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let (mut socket, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "provider was not contacted"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("provider listener failed: {error}"),
            }
        };
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let count = socket.read(&mut buffer).unwrap();
            assert_ne!(count, 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                let size: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse().ok())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + size {
                    assert!(headers.starts_with("post /v1/responses "));
                    assert!(headers.contains("authorization: bearer fixture-openai-key"));
                    break;
                }
            }
        }
        let body = concat!("data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n", "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n");
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    let output = Command::new(env!("CARGO_BIN_EXE_gitcode"))
        .args([
            "--provider",
            "openai",
            "--base-url",
            &format!("http://{address}/v1"),
            "--model",
            "fixture",
            "-p",
            "hi",
        ])
        .env_clear()
        .env("HOME", &temp_path)
        .env("USERPROFILE", &temp_path)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("OPENAI_API_KEY", "fixture-openai-key")
        .current_dir(&temp_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.join().unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).contains("ok"));
}
