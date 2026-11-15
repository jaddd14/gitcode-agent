# Permissions

Native tools consult deny/ask/allow rules. Interactive requests for shell execution, file changes, web fetches and MCP calls can be approved or rejected in the terminal. Noninteractive requests requiring approval fail closed. Editing also requires reading the file first and checks for external changes.

Native file tools allow paths outside the workspace and sensitive filenames in **all permission modes**. There is no built-in directory or sensitive-filename denylist, including for `.env`, `.ssh`, private keys, or `credentials.toml`. The workspace remains the default base for relative paths and searches, not a native file-access boundary. OS permissions still apply.

Outside bypass mode, explicit user rules retain their precedence: **Deny > Ask > Allow**. Default mode still requires approval for file edits, whether inside or outside the workspace; AcceptEdits allows edits by default. Plan mode remains read-only regardless of path. Read-before-write and stale-content checks still apply to edits in every mode.

`--dangerously-skip-permissions` bypasses permission rules and approval prompts, not OS permissions or editing consistency checks. Shell commands are not OS-confined. Internal checkpoint and memory/session import boundaries, including workspace-confined `@import` expansion, are unchanged by native file path access.

See [security policy](../SECURITY.md) and [remaining limitations](dev/PROGRESS.md#known-gaps).
