//! Tests for Milestone M11 MCP stdio, streamable HTTP/SSE, tool qualification, and config.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use gitcode::mcp;

use mcp::{
    add_mcp_server, list_mcp_servers, parse_qualified_tool, qualify_tool_name, remove_mcp_server,
    render_mcp_status, HttpMcpClient, McpServerConfig, McpServerStatus, StdioMcpClient,
    MAX_MCP_OUTPUT_BYTES,
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
    #[cfg(windows)]
    let executable = gitcode::platform::detect_shell("bash").unwrap().executable;
    #[cfg(not(windows))]
    let executable = std::path::PathBuf::from("/bin/sh");
    let stdio = StdioMcpClient::new(
        executable.to_str().unwrap(),
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

#[cfg(unix)]
#[tokio::test]
async fn workspace_tools_reuse_stdio_session_and_recheck_permissions() {
    use gitcode::{
        perm::{Engine, Mode, Rule},
        tools::Tools,
    };
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let log = root.join("starts.log");
    let script = r#"
printf 'start\n' >> "$MCP_TEST_LOG"
IFS= read -r init || exit 1
printf '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05"}}\n'
IFS= read -r initialized || exit 1
n=2
while IFS= read -r request; do
  printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"call-%s"}]}}\n' "$n" "$n"
  n=$((n+1))
done
"#;
    let make_config = |variant: &str| McpServerConfig::Stdio {
        command: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        env: BTreeMap::from([
            ("MCP_TEST_LOG".into(), log.to_string_lossy().into_owned()),
            ("MCP_TEST_VARIANT".into(), variant.into()),
        ]),
    };
    let path = root.join(".mcp.json");
    add_mcp_server(&path, "local", make_config("one")).unwrap();
    let mut tools = Tools::new(Engine::new(&root, Mode::Bypass).unwrap());
    let input = json!({"server":"local","tool":"echo","arguments":{}});
    assert_eq!(tools.execute("McpCall", &input).await.unwrap(), "call-2");
    assert_eq!(tools.execute("McpCall", &input).await.unwrap(), "call-3");
    tools.permissions.set_mode(Mode::Default);
    tools
        .permissions
        .deny
        .push(Rule::parse("McpCall(*)").unwrap());
    assert!(tools.execute("McpCall", &input).await.is_err());
    tools.permissions.deny.clear();
    tools.permissions.set_mode(Mode::Bypass);
    assert_eq!(tools.execute("McpCall", &input).await.unwrap(), "call-4");
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 1);
    add_mcp_server(&path, "local", make_config("two")).unwrap();
    assert_eq!(tools.execute("McpCall", &input).await.unwrap(), "call-2");
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2);
    tools.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_stdio_request_does_not_reuse_an_unfinished_connection() {
    use gitcode::{
        perm::{Engine, Mode},
        tools::Tools,
    };
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let log = root.join("starts.log");
    let marker = root.join("blocked-once");
    let script = r#"
printf 'start\n' >> "$MCP_TEST_LOG"
IFS= read -r init || exit 1
printf '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05"}}\n'
IFS= read -r initialized || exit 1
while IFS= read -r request; do
  if [ ! -e "$MCP_TEST_MARKER" ]; then
    : > "$MCP_TEST_MARKER"
    sleep 10
  fi
  printf '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"fresh"}]}}\n'
done
"#;
    let server = McpServerConfig::Stdio {
        command: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        env: BTreeMap::from([
            ("MCP_TEST_LOG".into(), log.to_string_lossy().into_owned()),
            (
                "MCP_TEST_MARKER".into(),
                marker.to_string_lossy().into_owned(),
            ),
        ]),
    };
    add_mcp_server(&root.join(".mcp.json"), "local", server).unwrap();
    let mut tools = Tools::new(Engine::new(&root, Mode::Bypass).unwrap());
    let input = json!({"server":"local","tool":"echo","arguments":{}});
    assert!(
        tokio::time::timeout(Duration::from_millis(300), tools.execute("McpCall", &input))
            .await
            .is_err()
    );
    assert!(marker.exists());
    assert_eq!(tools.execute("McpCall", &input).await.unwrap(), "fresh");
    assert_eq!(std::fs::read_to_string(log).unwrap().lines().count(), 2);
    tools.shutdown().await.unwrap();
}

/// Stdio MCP server script that answers one `resources` or `prompts` request with `response`.
fn stdio_script(response: &str) -> String {
    format!(
        "read line1; read line2; read line3; printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocolVersion\":\"2024-11-05\"}}}}'; printf '%s\\n' '{response}'"
    )
}

#[cfg(not(windows))]
fn sh_command(script: String) -> StdioMcpClient {
    StdioMcpClient::new(
        "/bin/sh",
        vec!["-c".into(), script],
        BTreeMap::new(),
        Duration::from_secs(5),
    )
}

#[cfg(windows)]
fn sh_command(script: String) -> StdioMcpClient {
    let executable = gitcode::platform::detect_shell("bash").unwrap().executable;
    StdioMcpClient::new(
        executable.to_str().unwrap(),
        vec!["-c".into(), script],
        BTreeMap::new(),
        Duration::from_secs(5),
    )
}

/// Single-response HTTP MCP server stub on a loopback TCP socket.
async fn http_stub(response_body: String) -> HttpMcpClient {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 8192];
        let _ = sock.read(&mut buf).await;
        let body = format!("data: {response_body}\n\n");
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
    });
    HttpMcpClient::new(
        format!("http://{addr}/mcp"),
        BTreeMap::new(),
        Duration::from_secs(5),
    )
    .unwrap()
}

#[tokio::test]
async fn stdio_mcp_client_lists_and_reads_resources() {
    let list = sh_command(stdio_script(
        r#"{"jsonrpc":"2.0","id":2,"result":{"resources":[{"uri":"file:///a.txt","name":"a","description":"first","mimeType":"text/plain"},{"uri":"file:///b.bin"}]}}"#,
    ));
    let resources = list.list_resources().await.unwrap();
    assert_eq!(resources.len(), 2);
    assert_eq!(resources[0].uri, "file:///a.txt");
    assert_eq!(resources[0].name, "a");
    assert_eq!(resources[0].description, "first");
    assert_eq!(resources[0].mime_type.as_deref(), Some("text/plain"));
    assert_eq!(resources[1].uri, "file:///b.bin");
    assert!(resources[1].mime_type.is_none());

    let read = sh_command(stdio_script(
        r#"{"jsonrpc":"2.0","id":2,"result":{"contents":[{"uri":"file:///a.txt","text":"hello"},{"uri":"file:///b.bin","blob":"AAEC"}]}}"#,
    ));
    let text = read.read_resource("file:///a.txt").await.unwrap();
    assert_eq!(
        text, "hello\n[binary resource content omitted]",
        "binary contents must be summarized instead of dumped"
    );

    let http = http_stub(
        r#"{"jsonrpc":"2.0","id":1,"result":{"contents":[{"uri":"file:///a.txt","text":"sse-resource"}]}}"#
            .into(),
    )
    .await;
    assert_eq!(
        http.read_resource("file:///a.txt").await.unwrap(),
        "sse-resource"
    );
}

#[tokio::test]
async fn stdio_mcp_client_lists_and_gets_prompts() {
    let list = sh_command(stdio_script(
        r#"{"jsonrpc":"2.0","id":2,"result":{"prompts":[{"name":"review","description":"Review code","arguments":[{"name":"path","description":"File path","required":true}]},{"name":"plain"}]}}"#,
    ));
    let prompts = list.list_prompts().await.unwrap();
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[0].name, "review");
    assert_eq!(prompts[0].description, "Review code");
    assert_eq!(prompts[0].arguments.len(), 1);
    assert_eq!(prompts[0].arguments[0].name, "path");
    assert!(prompts[0].arguments[0].required);
    assert_eq!(prompts[1].name, "plain");
    assert!(prompts[1].arguments.is_empty());

    let get = sh_command(stdio_script(
        r#"{"jsonrpc":"2.0","id":2,"result":{"description":"Review code","messages":[{"role":"user","content":{"type":"text","text":"please review a.rs"}},{"role":"assistant","content":[{"type":"text","text":"ok"},{"type":"text","text":"second"}]}]}}"#,
    ));
    let messages = get
        .get_prompt("review", json!({"path": "a.rs"}))
        .await
        .unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[0].text, "please review a.rs");
    assert_eq!(messages[1].role, "assistant");
    assert_eq!(messages[1].text, "ok\nsecond");

    let http = http_stub(
        r#"{"jsonrpc":"2.0","id":1,"result":{"prompts":[{"name":"remote","description":"remote prompt"}]}}"#
            .into(),
    )
    .await;
    let remote = http.list_prompts().await.unwrap();
    assert_eq!(remote.len(), 1);
    assert_eq!(remote[0].name, "remote");
}

#[tokio::test]
async fn stdio_mcp_resources_and_prompts_reject_invalid_server_responses() {
    // `resources` is not a list.
    let bad_list = sh_command(stdio_script(
        r#"{"jsonrpc":"2.0","id":2,"result":{"resources":{"uri":"file:///a.txt"}}}"#,
    ));
    assert!(bad_list.list_resources().await.is_err());

    // Missing `contents` entirely.
    let bad_read = sh_command(stdio_script(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#));
    assert!(bad_read.read_resource("file:///a.txt").await.is_err());

    // Empty `contents` array.
    let empty_read = sh_command(stdio_script(
        r#"{"jsonrpc":"2.0","id":2,"result":{"contents":[]}}"#,
    ));
    assert!(empty_read.read_resource("file:///a.txt").await.is_err());

    // Prompt descriptors with a non-string name fail to deserialize.
    let bad_prompts = sh_command(stdio_script(
        r#"{"jsonrpc":"2.0","id":2,"result":{"prompts":[{"name":42}]}}"#,
    ));
    assert!(bad_prompts.list_prompts().await.is_err());

    // `messages` missing on `prompts/get`.
    let bad_get = sh_command(stdio_script(
        r#"{"jsonrpc":"2.0","id":2,"result":{"description":"no messages"}}"#,
    ));
    assert!(bad_get.get_prompt("review", json!({})).await.is_err());

    // Non-object prompt arguments are rejected before any request is made.
    assert!(bad_get
        .get_prompt("review", json!(["not", "an", "object"]))
        .await
        .is_err());

    // JSON-RPC error object from the server.
    let erroring = sh_command(stdio_script(
        r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"method not found"}}"#,
    ));
    assert!(erroring.list_resources().await.is_err());

    // HTTP transport: malformed JSON payload is an error, not a panic.
    let http = http_stub("not json at all".into()).await;
    assert!(http.read_resource("file:///a.txt").await.is_err());

    // HTTP transport: error object instead of result.
    let http_err =
        http_stub(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"bad uri"}}"#.into())
            .await;
    assert!(http_err.read_resource("file:///a.txt").await.is_err());
    let http_err_prompts =
        http_stub(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"boom"}}"#.into())
            .await;
    assert!(http_err_prompts.list_prompts().await.is_err());
}

#[tokio::test]
async fn oversized_mcp_resource_contents_are_truncated() {
    // The filler is produced by the shell, not passed as an argument: Windows
    // caps a command line at 32767 characters, so a 100 KiB argv would fail to
    // spawn there instead of testing truncation. printf repeats the literal
    // without any external utilities, which keeps this portable.
    let target = MAX_MCP_OUTPUT_BYTES + 1024;
    let block = "x".repeat(1000);
    let blocks = target / 1000;
    let script = format!(
        concat!(
            "read line1; read line2; read line3; ",
            "printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocolVersion\":\"2024-11-05\"}}}}'; ",
            "printf '%s' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"contents\":[{{\"uri\":\"file:///big\",\"text\":\"'; ",
            "printf '{block}%.0s' $(seq {blocks}); ",
            "printf '%s\\n' '\"}}]}}}}'"
        ),
        block = block,
        blocks = blocks
    );
    let server = sh_command(script);
    let text = server.read_resource("file:///big").await.unwrap();
    assert!(text.len() <= MAX_MCP_OUTPUT_BYTES + "\n[mcp output truncated]".len());
    assert!(text.ends_with("[mcp output truncated]"));
}
