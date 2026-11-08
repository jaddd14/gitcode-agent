# MCP

The native `McpCall` tool takes `server`, `tool`, and object `arguments`. It checks permission before loading workspace `.mcp.json` and connects the configured stdio or HTTP client with a 30-second timeout. Configuration parsing and transport examples are covered by `tests/mcp.rs`.

Default-mode execution requires approval and therefore fails closed in the current UI. Never configure an untrusted stdio executable: launching it executes local code. Server credentials belong in private local configuration, never repository history.

Persistent server lifecycle, dynamic tool discovery and resources UI remain [known gaps](dev/PROGRESS.md#known-gaps).
