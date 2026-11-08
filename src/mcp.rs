//! Model Context Protocol (MCP) client supporting stdio and streamable HTTP JSON-RPC 2.0.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::Path,
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

/// Maximum output bytes returned from an MCP tool or resource call before truncation.
pub const MAX_MCP_OUTPUT_BYTES: usize = 100_000;

/// Transport configuration for a single MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum McpServerConfig {
    /// Child process speaking newline-delimited JSON-RPC 2.0 over stdin/stdout.
    Stdio {
        /// Executable name or path.
        command: String,
        /// Command-line arguments.
        #[serde(default)]
        args: Vec<String>,
        /// Environment variables passed to the server process.
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    /// Streamable HTTP or SSE JSON-RPC 2.0 endpoint.
    Http {
        /// HTTP or HTTPS endpoint URL.
        url: String,
        /// Optional HTTP request headers.
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

/// Persisted `.mcp.json` configuration file structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpConfigFile {
    /// Configured MCP servers keyed by server identifier.
    #[serde(default, rename = "mcpServers")]
    pub servers: BTreeMap<String, McpServerConfig>,
}

/// Discovered MCP tool descriptor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpToolInfo {
    /// Unqualified server tool name.
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// JSON Schema for tool input arguments.
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

/// Connection status entry for `/mcp` UI rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerStatus {
    /// Server name.
    pub name: String,
    /// Transport summary (`stdio` or `http`).
    pub transport: String,
    /// Whether the server responded to initialization.
    pub connected: bool,
    /// Number of exposed tools.
    pub tool_count: usize,
}

fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && !s.contains("__")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Format a qualified MCP tool name as `mcp__<server>__<tool>`.
pub fn qualify_tool_name(server: &str, tool: &str) -> Result<String, String> {
    if !valid_ident(server) || !valid_ident(tool) {
        return Err("invalid MCP server or tool identifier".into());
    }
    Ok(format!("mcp__{server}__{tool}"))
}

/// Parse a qualified `mcp__<server>__<tool>` name into `(server, tool)`.
pub fn parse_qualified_tool(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if valid_ident(server) && valid_ident(tool) {
        Some((server, tool))
    } else {
        None
    }
}

/// Load MCP configuration from a JSON file, returning an empty map if absent.
pub fn load_mcp_config(path: &Path) -> io::Result<McpConfigFile> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "MCP config must not be a symlink",
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(McpConfigFile::default()),
        Err(e) => return Err(e),
    }
    let text = fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Save MCP configuration atomically with private permissions on Unix.
pub fn save_mcp_config(path: &Path, config: &McpConfigFile) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let encoded = serde_json::to_string_pretty(config)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(encoded.as_bytes())?;
    file.sync_all()
}

/// Add or replace an MCP server entry in a configuration file.
pub fn add_mcp_server(path: &Path, name: &str, server: McpServerConfig) -> Result<(), String> {
    if !valid_ident(name) {
        return Err("invalid MCP server identifier".into());
    }
    let mut cfg = load_mcp_config(path).map_err(|e| e.to_string())?;
    cfg.servers.insert(name.to_owned(), server);
    save_mcp_config(path, &cfg).map_err(|e| e.to_string())
}

/// Remove an MCP server entry from a configuration file.
pub fn remove_mcp_server(path: &Path, name: &str) -> Result<bool, String> {
    let mut cfg = load_mcp_config(path).map_err(|e| e.to_string())?;
    let existed = cfg.servers.remove(name).is_some();
    save_mcp_config(path, &cfg).map_err(|e| e.to_string())?;
    Ok(existed)
}

/// List configured MCP servers in deterministic key order.
pub fn list_mcp_servers(path: &Path) -> Result<Vec<(String, McpServerConfig)>, String> {
    let cfg = load_mcp_config(path).map_err(|e| e.to_string())?;
    Ok(cfg.servers.into_iter().collect())
}

fn bound_output(mut text: String) -> String {
    if text.len() > MAX_MCP_OUTPUT_BYTES {
        let mut end = MAX_MCP_OUTPUT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[mcp output truncated]");
    }
    text
}

/// Stdio JSON-RPC 2.0 MCP client.
pub struct StdioMcpClient {
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    timeout: Duration,
}

impl StdioMcpClient {
    /// Construct a stdio MCP client with an explicit per-request timeout.
    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Self {
        Self {
            command: command.into(),
            args,
            env,
            timeout,
        }
    }

    /// Perform an `initialize` + request roundtrip against the stdio MCP process.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let work = async {
            let mut cmd = Command::new(&self.command);
            cmd.args(&self.args)
                .envs(&self.env)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            let mut child = cmd.spawn().map_err(|_| "cannot spawn stdio MCP server")?;
            let mut stdin = child.stdin.take().ok_or("missing MCP stdin")?;
            let stdout = child.stdout.take().ok_or("missing MCP stdout")?;
            let mut reader = BufReader::new(stdout).lines();

            let init_req = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "gitcode", "version": "0.1.0" }
                }
            });
            let notif = json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            });
            let call_req = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": method,
                "params": params
            });
            let payload = format!("{init_req}\n{notif}\n{call_req}\n");
            stdin
                .write_all(payload.as_bytes())
                .await
                .map_err(|_| "cannot write to MCP server")?;
            stdin.flush().await.map_err(|_| "cannot flush MCP stdin")?;

            while let Some(line) = reader
                .next_line()
                .await
                .map_err(|_| "cannot read MCP stdout")?
            {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let parsed: Value =
                    serde_json::from_str(trimmed).map_err(|_| "invalid MCP JSON-RPC line")?;
                if parsed["id"].as_i64() == Some(2) {
                    if let Some(err) = parsed.get("error") {
                        return Err(format!("MCP error: {err}"));
                    }
                    return Ok(parsed["result"].clone());
                }
            }
            Err("MCP server closed stream before responding".into())
        };
        tokio::time::timeout(self.timeout, work)
            .await
            .map_err(|_| "MCP stdio request timed out".to_owned())?
    }

    /// List tools exposed by the stdio MCP server.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, String> {
        let result = self.request("tools/list", json!({})).await?;
        serde_json::from_value(result["tools"].clone()).map_err(|_| "invalid MCP tools list".into())
    }

    /// Invoke a tool on the stdio MCP server and return bounded text.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, String> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        let text = result["content"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| result.to_string());
        Ok(bound_output(text))
    }
}

/// Streamable HTTP and SSE JSON-RPC 2.0 MCP client.
pub struct HttpMcpClient {
    url: String,
    headers: BTreeMap<String, String>,
    client: reqwest::Client,
}

impl HttpMcpClient {
    /// Construct an HTTP MCP client with a bounded request timeout.
    pub fn new(
        url: impl Into<String>,
        headers: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<Self, String> {
        let url = url.into();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("MCP HTTP URL must start with http:// or https://".into());
        }
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "cannot build MCP HTTP client")?;
        Ok(Self {
            url,
            headers,
            client,
        })
    }

    /// Send a JSON-RPC 2.0 request over HTTP, accepting either JSON or `text/event-stream`.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params
        });
        let mut req = self
            .client
            .post(&self.url)
            .header("Accept", "application/json, text/event-stream")
            .json(&body);
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|_| "MCP HTTP request failed")?;
        if !resp.status().is_success() {
            return Err(format!("MCP HTTP status {}", resp.status().as_u16()));
        }
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let text = resp
            .text()
            .await
            .map_err(|_| "cannot read MCP HTTP response")?;
        let payload = if content_type.contains("text/event-stream") {
            text.lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim)
                .find(|data| !data.is_empty() && *data != "[DONE]")
                .ok_or("empty MCP SSE response")?
        } else {
            text.trim()
        };
        let parsed: Value =
            serde_json::from_str(payload).map_err(|_| "invalid MCP HTTP JSON-RPC response")?;
        if let Some(err) = parsed.get("error") {
            return Err(format!("MCP error: {err}"));
        }
        Ok(parsed["result"].clone())
    }

    /// Invoke a tool over HTTP/SSE MCP and return bounded text.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, String> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        let text = result["content"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| result.to_string());
        Ok(bound_output(text))
    }
}

/// Render `/mcp` server status lines for the terminal UI.
pub fn render_mcp_status(servers: &[McpServerStatus]) -> Vec<String> {
    if servers.is_empty() {
        return vec!["No MCP servers configured.".into()];
    }
    servers
        .iter()
        .map(|s| {
            let state = if s.connected {
                "connected"
            } else {
                "disconnected"
            };
            format!(
                "• {} [{}] — {} ({} tools)",
                s.name, s.transport, state, s.tool_count
            )
        })
        .collect()
}
