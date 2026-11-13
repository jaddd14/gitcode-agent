# Permissions

Native tools consult deny/ask/allow rules. Interactive requests for shell execution, file changes, web fetches and MCP calls can be approved or rejected in the terminal. Noninteractive requests requiring approval fail closed. Editing also requires reading the file first and checks for external changes.

`--dangerously-skip-permissions` is an explicit bypass, not an OS sandbox. Do not use it on untrusted repositories or model instructions. Shell commands are not OS-confined. MCP and web requests require an allow decision.

See [security policy](../SECURITY.md) and [remaining limitations](dev/PROGRESS.md#known-gaps).
