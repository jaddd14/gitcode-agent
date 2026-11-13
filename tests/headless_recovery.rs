//! A failed provider request must not erase already executed tools from the session.
#![allow(clippy::unwrap_used)]
use gitcode::{provider::canonical::Content, session::store::SessionStore};
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::Command,
    time::Duration,
};

#[test]
fn failed_headless_turn_preserves_completed_tool_result() {
    let base =
        std::env::temp_dir().join(format!("gitcode-headless-recovery-{}", std::process::id()));
    std::fs::create_dir_all(base.join("home")).unwrap();
    std::fs::create_dir_all(base.join("project")).unwrap();
    let home = std::fs::canonicalize(base.join("home")).unwrap();
    let project = std::fs::canonicalize(base.join("project")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for index in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                let count = socket.read(&mut buffer).unwrap();
                assert_ne!(count, 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().ok())
                                .flatten()
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            if index == 0 {
                let body = concat!(
                    "data: {\"type\":\"message_start\",\"message\":{}}\n\n",
                    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"Bash\",\"input\":{\"command\":\"touch side-effect\"}}}\n\n",
                    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
                    "data: {\"type\":\"message_stop\"}\n\n");
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            } else {
                socket.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
        }
    });
    let output = Command::new(env!("CARGO_BIN_EXE_gitcode"))
        .args([
            "--base-url",
            &format!("http://{address}"),
            "--model",
            "fixture",
            "--dangerously-skip-permissions",
            "-p",
            "fixture",
        ])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .current_dir(&project)
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(!output.status.success());
    assert!(project.join("side-effect").exists());
    let store = SessionStore::open(&home, &project).unwrap();
    let id = store.list().unwrap().remove(0).id;
    let session = store.resume(&id).unwrap();
    assert!(session.messages.iter().flat_map(|m| &m.content).any(|c| matches!(c, Content::ToolResult { tool_use_id, is_error: false, .. } if tool_use_id == "call_1")));
    assert!(session
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .any(|c| matches!(c, Content::Text { text } if text == "fixture")));
    std::fs::remove_dir_all(base).unwrap();
}
