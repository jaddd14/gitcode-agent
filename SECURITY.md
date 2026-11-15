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
- An assessment of the security impact (e.g., internal checkpoint/import boundary traversal, permission policy bypass, credential disclosure).

### Response Time Targets & Disclosure Policy

| Stage | Target |
| ----- | ------ |
| Initial acknowledgment | Within **48 hours** |
| Triage & severity assessment | Within **5 business days** |
| Fix & coordinated release | Within **14–30 days** depending on complexity |

We follow **coordinated vulnerability disclosure**. Once a fix is verified and released, we publish the GitHub Security Advisory and credit the reporter (unless anonymity is requested). We ask that reporters allow up to 30 days (or a mutually agreed window) before public disclosure.

## Scope

### In Scope

- **Internal boundary escapes**: escaping checkpoint or memory/session import boundaries, including `@import` memory boundaries via symlinks, relative traversal (`..`), or race conditions. Native file tools (`Read`, `Write`, `Edit`, `MultiEdit`, `NotebookEdit`, `apply_patch`, `LS`, `Glob`, `Grep`) intentionally allow paths outside the workspace and sensitive filenames in every permission mode, subject to the applicable permission policy and OS permissions; that access alone is not a confinement escape.
- **Permission engine bypasses**: outside bypass mode, executing denied tools, bypassing rule evaluation precedence (`Deny` > `Ask` > `Allow`), or circumventing read-only enforcement in `Plan` mode.
- **Command splitter bypasses**: outside bypass mode, tricking the conservative shell command parser (`src/tools/shell.rs`) into auto-approving compound or subshell constructs (`$(...)`, backticks, redirections, pipelines, control operators) under a simple prefix allow rule.
- **Credential & secret leakage**: exposing API keys, bearer tokens, or private headers in JSONL logs, session transcripts, error messages, or terminal redraws, or creating credential/session files without strict `0600`/`0700` Unix permissions.
- **Prompt-injection-driven tool abuse**: untrusted repository files or tool outputs escalating privileges without matching explicit `Allow` rules or user confirmation.

### Out of Scope

- Intentional execution of commands when the user explicitly runs with `--dangerously-skip-permissions` (`Yolo` mode) or approves an explicit command prompt.
- Denial-of-service caused solely by user-configured local resource exhaustion.
- Vulnerabilities in third-party LLM provider endpoints or upstream models unrelated to `gitcode`'s client adapter or permission controls.

## Security Model

`gitcode` is a local-first terminal coding agent that interacts with local files and shell processes. Its defense-in-depth architecture relies on four core mechanisms and carries explicit boundaries users must understand:

1. **Permission Engine (`src/perm/mod.rs`)**
   - Outside bypass mode, tool invocations are evaluated against layered rules in strict precedence: **`Deny` overrides `Ask`, and `Ask` overrides `Allow`**.
   - Operating modes constrain default behavior:
     - `Plan`: allows `ReadOnly` operations only; all writes and shell executions are denied.
     - `Default`: allows `ReadOnly` operations and requires approval (`Ask`) for file writes, shell execution, and network access, whether file paths are inside or outside the workspace.
     - `AcceptEdits`: permits file edits inside or outside the workspace while still gating shell execution.
     - `Yolo` (`--dangerously-skip-permissions`): bypasses permission rules; OS permissions, read-before-write, and stale-content checks still apply.
   - In every mode, native file tools have no built-in directory or sensitive-filename denylist: paths such as `.env`, `.env.*`, `*.pem`, `*.key`, `.ssh`, `.aws`, or `credentials.toml` are not automatically blocked. Explicit user deny/ask/allow rules still control access outside bypass mode. Read-before-write and stale-content checks remain in force for edits in every mode.

2. **Path Resolution & Internal Boundaries (`src/tools/mod.rs`, `src/session/memory.rs`)**
   - The workspace remains the default base for relative native file paths and searches, not a confinement boundary. Native file tools can use outside-workspace paths in every permission mode, subject to the applicable permission policy and OS permissions.
   - Internal checkpoint and memory/session import boundaries are unchanged. In particular, hierarchical `GITCODE.md`/`CLAUDE.md`/`AGENTS.md` `@import` expansion retains its canonical workspace boundary; unrestricted native file paths do not relax that internal boundary.

3. **Conservative Command Splitter (`src/tools/shell.rs`)**
   - Before matching a `Bash(prefix:*)` permission rule, the command string is inspected by a quote-aware parser.
   - Any unquoted shell control operator (`;`, `|`, `&&`, `||`, `&`, newlines), redirection (`>`, `>>`, `<`), command substitution (`$(...)`, `` `...` ``), or unclosed quote forces the command out of single-command prefix auto-approval so compound payloads cannot piggyback on benign prefixes.

4. **Private Storage & Secret Redaction (`src/credentials.rs`, `src/logging.rs`, `src/session/store.rs`)**
   - Credentials (`credentials.toml`) and session JSONL stores are written with `0700` directory modes and `0600` file modes on Unix.
   - Structured diagnostic logs redact registered credentials and recognizable `sk-*`, `Bearer`, `api_key`, `authorization`, and `x-api-key` forms. This is best effort, not a guarantee that arbitrary secrets are detected. Sessions and tool output may contain sensitive information.

### Honest Limitations

- **Not an OS Kernel Sandbox by Default**: `gitcode` enforces permission rules inside its own built-in tools and command parser, and retains internal checkpoint/import boundaries, **not** via an operating-system kernel sandbox (`seccomp`, `landlock`, `Bubblewrap`, `seatbelt`, VMs, or containers). Native file tools are not workspace-confined and do not detect or automatically block secret files; host OS permissions still apply.
- Once a `Bash` command or external process is permitted and spawned, that child process executes with the ambient OS privileges of the user running `gitcode` and can access any files or network interfaces permitted by the host operating system.
- When running against untrusted repositories or with relaxed permission modes (`Yolo`), run `gitcode` inside an isolated container, VM, or dedicated unprivileged OS user account.
