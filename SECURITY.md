# Security Policy

## Supported Versions

Only the latest `0.3.x` release series receives security updates and vulnerability fixes.

| Version | Supported |
| ------- | --------- |
| 0.3.x   | Yes       |
| < 0.3.0 | No        |

## Reporting a Vulnerability

Please **do not** report security vulnerabilities through public GitHub issues, pull requests, or discussions.

Use **GitHub Security Advisories** for private reporting:

1. Navigate to the [Security tab](https://github.com/jaddd14/gitcode-agent/security/advisories/new) of the `jaddd14/gitcode-agent` repository.
2. Click **"Report a vulnerability"** to open a private advisory draft with the maintainer (`@jaddd14`).

### What to Include

To help triage and reproduce the issue quickly, please include:

- The exact `gitcode` version, commit hash, OS, and terminal environment.
- Relevant sanitized configuration (`config.toml`, permission policy rules, active mode) with all secrets removed.
- Minimal step-by-step reproduction instructions or a proof-of-concept transcript/fixture.
- An assessment of the security impact (e.g., workspace boundary traversal, permission policy bypass, credential disclosure).

### Response Time Targets & Disclosure Policy

| Stage | Target |
| ----- | ------ |
| Initial acknowledgment | Within **48 hours** |
| Triage & severity assessment | Within **5 business days** |
| Fix & coordinated release | Within **14–30 days** depending on complexity |

We follow **coordinated vulnerability disclosure**. Once a fix is verified and released, we publish the GitHub Security Advisory and credit the reporter (unless anonymity is requested). We ask that reporters allow up to 30 days (or a mutually agreed window) before public disclosure.

## Scope

### In Scope

- **Workspace confinement escapes**: bypassing canonical path checks in file tools (`Read`, `Write`, `Edit`, `MultiEdit`, `NotebookEdit`, `apply_patch`, `LS`, `Glob`, `Grep`) or `@import` memory instructions via symlinks, relative traversal (`..`), or race conditions.
- **Permission engine bypasses**: executing denied tools, bypassing rule evaluation precedence (`Deny` > `Ask` > `Allow`), or circumventing read-only enforcement in `Plan` mode.
- **Command splitter bypasses**: tricking the conservative shell command parser (`src/tools/shell.rs`) into auto-approving compound or subshell constructs (`$(...)`, backticks, redirections, pipelines, control operators) under a simple prefix allow rule.
- **Credential & secret leakage**: exposing API keys, bearer tokens, or private headers in JSONL logs, session transcripts, error messages, or terminal redraws, or creating credential/session files without strict `0600`/`0700` Unix permissions.
- **Prompt-injection-driven tool abuse**: untrusted repository files or tool outputs escalating privileges without matching explicit `Allow` rules or user confirmation.

### Out of Scope

- Intentional execution of commands when the user explicitly runs with `--dangerously-skip-permissions` (`Yolo` mode) or approves an explicit command prompt.
- Denial-of-service caused solely by user-configured local resource exhaustion.
- Vulnerabilities in third-party LLM provider endpoints or upstream models unrelated to `gitcode`'s client adapter or permission controls.

## Security Model

`gitcode` is a local-first terminal coding agent that interacts with local files and shell processes. Its defense-in-depth architecture relies on four core mechanisms and carries explicit boundaries users must understand:

1. **Permission Engine (`src/perm/mod.rs`)**
   - Every tool invocation is classified into `ReadOnly`, `WorkspaceWrite`, `ShellExec`, or `Network` and evaluated against layered rules in strict precedence: **`Deny` overrides `Ask`, and `Ask` overrides `Allow`**.
   - Operating modes constrain default behavior:
     - `Plan`: allows `ReadOnly` operations only; all writes and shell executions are denied.
     - `Default`: allows `ReadOnly` inside the workspace and requires approval (`Ask`) for workspace writes, shell execution, and network access.
     - `AcceptEdits`: permits workspace file edits while still gating shell execution and external paths.
     - `Yolo` (`--dangerously-skip-permissions`): bypasses interactive prompts unless an explicit `Deny` rule matches.
   - Native file tools deny paths containing `.env`, `.env.*`, `*.pem`, `*.key`, `.ssh`, `.aws`, or `credentials.toml`, including resolved read paths. This remains active in bypass mode. Other filenames can still contain secrets; approved shell commands and external processes are not confined by these rules.

2. **Workspace Path Confinement (`src/tools/mod.rs`, `src/session/memory.rs`)**
   - Built-in file inspection, editing, and hierarchical `GITCODE.md`/`CLAUDE.md`/`AGENTS.md` `@import` expansion resolve paths against the canonical workspace root.
   - Symlinks and parent-directory traversals (`..`) are canonicalized (including existing ancestor resolution for newly created files) so operations cannot silently escape the workspace root without explicit permission evaluation.

3. **Conservative Command Splitter (`src/tools/shell.rs`)**
   - Before matching a `Bash(prefix:*)` permission rule, the command string is inspected by a quote-aware parser.
   - Any unquoted shell control operator (`;`, `|`, `&&`, `||`, `&`, newlines), redirection (`>`, `>>`, `<`), command substitution (`$(...)`, `` `...` ``), or unclosed quote forces the command out of single-command prefix auto-approval so compound payloads cannot piggyback on benign prefixes.

4. **Private Storage & Secret Redaction (`src/credentials.rs`, `src/logging.rs`, `src/session/store.rs`)**
   - Credentials (`credentials.toml`) and session JSONL stores are written with `0700` directory modes and `0600` file modes on Unix.
   - Structured diagnostic logs redact registered credentials and recognizable `sk-*`, `Bearer`, `api_key`, `authorization`, and `x-api-key` forms. This is best effort, not a guarantee that arbitrary secrets are detected. Sessions and tool output may contain sensitive information.

### Honest Limitations

- **Not an OS Kernel Sandbox by Default**: `gitcode` enforces path confinement and permission rules inside its own built-in tools and command parser, **not** via an operating-system kernel sandbox (`seccomp`, `landlock`, `Bubblewrap`, `seatbelt`, VMs, or containers).
- Once a `Bash` command or external process is permitted and spawned, that child process executes with the ambient OS privileges of the user running `gitcode` and can access any files or network interfaces permitted by the host operating system.
- When running against untrusted repositories or with relaxed permission modes (`Yolo`), run `gitcode` inside an isolated container, VM, or dedicated unprivileged OS user account.
