# Contributing to gitcode

Thank you for contributing to `gitcode`. This guide explains how to set up a development environment, verify changes with the repository quality gate, extend providers/tools/commands, and cut releases.

## Prerequisites & Setup

1. Install the pinned stable Rust toolchain (`rustfmt` and `clippy` are configured via `rust-toolchain.toml`):

   ```bash
   git clone https://github.com/jaddd14/gitcode-agent.git
   cd gitcode-agent
   cargo build
   ```

2. Run the full quality gate before opening any pull request:

   ```bash
   ./scripts/check.sh
   ```

## Quality Gate (`scripts/check.sh`)

Every commit and pull request must pass `./scripts/check.sh` with zero warnings:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

If `cargo-deny` is installed locally, also verify license and advisory rules against `deny.toml`:

```bash
cargo deny check
```

## Workflow, Branches & Conventional Commits

1. Fork the repository and create a topic branch from `main` (`feat/<topic>`, `fix/<topic>`, `docs/<topic>`, `test/<topic>`, or `refactor/<topic>`).
2. Follow [Conventional Commits](https://www.conventionalcommits.org/):
   - `feat(provider): add endpoint preset for ...`
   - `fix(tools): preserve trailing newline in MultiEdit`
   - `test(sse): cover split multibyte UTF-8 sequence across chunks`
   - `docs(security): document conservative command splitter rules`
3. Keep commits focused, ensure `./scripts/check.sh` passes at every step, and open a pull request filling out `.github/PULL_REQUEST_TEMPLATE.md`.

## Coding Standards

- **Zero unsafe code**: `unsafe_code = "forbid"` is enforced in `Cargo.toml`.
- **No `.unwrap()` or `.expect()` in production library/binary code**: `clippy::unwrap_used` and `clippy::expect_used` are configured in `Cargo.toml`. Propagate typed errors through `crate::error::Error` and `crate::error::Result<T>`.
- **Documentation comments**: `missing_docs = "warn"` is enabled across the crate (`-D warnings` in CI). Every public struct, enum, variant, trait, function, and module must have a clear `///` or `//!` doc comment.
- **Formatting**: `rustfmt.toml` enforces `edition = "2021"` and `max_width = 100`. Run `cargo fmt` before committing.
- **Secret safety**: Never log raw headers, API keys, or unredacted environment variables. Always route diagnostics through `src/logging.rs` redaction helpers.

## Testing Guide

`gitcode` uses fast, deterministic offline tests with zero live network calls required in `cargo test`:

1. **Mock TCP HTTP/SSE Servers (`tests/provider.rs`, `tests/provider_failures.rs`, `tests/openai.rs`, `tests/agent.rs`)**
   - Tests bind an ephemeral `tokio::net::TcpListener` on `127.0.0.1:0` and stream canned HTTP responses and chunked SSE frames directly to the provider adapters.
   - Use mock servers to verify request headers, wire payloads, HTTP `429`/`5xx` retry backoff, `401` credential error redaction, and truncated stream handling.

2. **SSE Wire Fixtures (`testdata/`)**
   - Deterministic `.sse` fixtures live in `testdata/` (e.g., `anthropic_text.sse`, `anthropic_tool.sse`, `openai_chat_text.sse`, `openai_chat_tool_reasoning.sse`, `openai_responses.sse`).
   - Byte-boundary stress tests slice fixtures at every arbitrary chunk offset to verify that `SseDecoder` (`src/provider/sse.rs`) handles CRLF/LF splits and multi-byte UTF-8 boundaries without corruption.

3. **Terminal & Markdown Snapshot Tests (`tests/terminal.rs`, `tests/markdown.rs`, `tests/diff.rs`)**
   - Terminal UI components (`Header`, `Intro`, `InputEditor`, `StatusBar`, `MarkdownStreamer`, `render_diff`) are tested against deterministic width/theme inputs using an injected buffer backend rather than requiring a live TTY.

## How to Add a Provider Preset

1. Open `src/provider/registry.rs` and add the new preset identifier, base URL, wire protocol (`AnthropicMessages`, `OpenAiChatCompletions`, or `OpenAiResponses`), default environment variable name, and any endpoint quirks (`ChatQuirks`) to the preset catalog.
2. If the provider requires a new wire-level quirk (for example, custom reasoning field names or max-token parameter naming), extend `ChatQuirks` in `src/provider/openai_chat.rs` or the relevant adapter.
3. Add a `.sse` fixture under `testdata/` and a round-trip mock TCP test in `tests/openai.rs` or `tests/provider.rs`.

## How to Add a Tool

1. Define the tool's JSON Schema, input parameters, and execution handler in `src/tools/` (`src/tools/mod.rs`, `src/tools/editing.rs`, or `src/tools/shell.rs`).
2. Classify the tool's permission category (`ToolCategory::ReadOnly`, `WorkspaceWrite`, `ShellExec`, or `Network`) in `src/perm/mod.rs` and ensure all file paths pass through canonical workspace confinement checks.
3. Register the tool definition in `Tools::definitions()` and dispatch it in `Tools::execute()`.
4. Add unit and integration tests in `tests/tools.rs` (including permission denial and workspace escape tests).

## How to Add a Slash Command

1. Locate the slash command dispatcher in `src/tui/app.rs` (and any non-interactive CLI handling in `src/main.rs` if applicable).
2. Register the `/command` name, argument parser, and localized help description in `src/i18n.rs`.
3. Implement the command handler against the active `Agent` / `SessionStore` / `Config` state and render feedback into the transcript buffer.
4. Add a deterministic test in `tests/terminal.rs` or `tests/agent.rs` covering command execution and output rendering.

## Release Process

1. Verify that `CHANGELOG.md` is updated under the new `[X.Y.Z] - YYYY-MM-DD` heading following Keep a Changelog format.
2. Bump `version` in `Cargo.toml` and run `./scripts/check.sh` and `cargo build --release` to update `Cargo.lock`.
3. Commit the release preparation (`chore(release): vX.Y.Z`) and push to `main`.
4. Create and push an annotated git tag (`git tag -a vX.Y.Z -m "vX.Y.Z" && git push origin vX.Y.Z`) to trigger the release workflow and binary artifact publishing.
