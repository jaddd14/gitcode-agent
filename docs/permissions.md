# Permissions

Native tools consult deny/ask/allow rules. Interactive requests for shell execution, file changes, web fetches and MCP calls can be approved or rejected in the terminal. Noninteractive requests requiring approval fail closed. Editing also requires reading the file first and checks for external changes.

`--dangerously-skip-permissions` is an explicit bypass of permission rules and the built-in sensitive-path block, not an OS sandbox. Native file tools still enforce workspace boundaries and editing consistency checks; shell commands are not OS-confined. Plan mode remains read-only.

See [security policy](../SECURITY.md) and [remaining limitations](dev/PROGRESS.md#known-gaps).
