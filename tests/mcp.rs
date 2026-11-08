//! Tests for Milestone M11 MCP stdio, streamable HTTP/SSE, tool qualification, and config.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use gitcode::mcp;

use mcp::{
    add_mcp_server, list_mcp_servers, parse_qualified_tool, qualify_tool_name, remove_mcp_server,
    render_mcp_status, HttpMcpClient, McpServerConfig, McpServerStatus, StdioMcpClient,
};
use serde_json::json;
use std::{collections::BTreeMap, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[test]
fn qualify_and_parse_mcp_tool_names_and_persist_config() {
    let q = qualify_tool_name("github_srv", "search-issues").unwrap();
    assert_eq!(q, "mcp__github_srv__search-issues");
    assert_eq!(
        parse_qualified_tool(&q),
        Some(("github_srv", "search-issues"))
    );
    assert!(qualify_tool_name("bad__name", "tool").is_err());
    assert_eq!(parse_qualified_tool("Read"), None);

    let path = std::env::temp_dir().join(format!("gitcode-mcp-{}.json", std::process::id()));
    add_mcp_server(
        &path,
        "local",
        McpServerConfig::Stdio {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "true".into()],
            env: BTreeMap::new(),
        },
    )
    .unwrap();
    let list = list_mcp_servers(&path).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, "local");
    assert!(remove_mcp_server(&path, "local").unwrap());
    assert!(list_mcp_servers(&path).unwrap().is_empty());
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn stdio_and_http_sse_mcp_clients_execute_tools() {
    let script = concat!(
        "read line1; read line2; read line3; ",
        "printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2024-11-05\"}}\\n'; ",
        "printf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"stdio-ok\"}]}}\\n'"
    );
    let stdio = StdioMcpClient::new(
        "/bin/sh",
        vec!["-c".into(), script.into()],
        BTreeMap::new(),
        Duration::from_secs(5),
    );
    let out = stdio.call_tool("echo", json!({"msg": "hi"})).await.unwrap();
    assert_eq!(out, "stdio-ok");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"sse-ok\"}]}}\n\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
    });

    let http = HttpMcpClient::new(
        format!("http://{addr}/mcp"),
        BTreeMap::new(),
        Duration::from_secs(5),
    )
    .unwrap();
    let sse_out = http.call_tool("ping", json!({})).await.unwrap();
    assert_eq!(sse_out, "sse-ok");

    let lines = render_mcp_status(&[McpServerStatus {
        name: "local".into(),
        transport: "stdio".into(),
        connected: true,
        tool_count: 2,
    }]);
    assert!(lines[0].contains("local [stdio] — connected (2 tools)"));
}
