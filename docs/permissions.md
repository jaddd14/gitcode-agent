# Permissions

Native tools consult deny/ask/allow rules. Default-mode writes and external execution can require approval; the current UI cannot grant interactive approval, so those requests fail closed. Editing also requires reading the file first and checks for external changes.

`--dangerously-skip-permissions` is an explicit bypass, not an OS sandbox. Do not use it on untrusted repositories or model instructions. Shell commands are not OS-confined. MCP and web requests require an allow decision.

See [security policy](../SECURITY.md) and [remaining limitations](dev/PROGRESS.md#known-gaps).
