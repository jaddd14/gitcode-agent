# Architecture

Single stable-Rust Cargo package, edition 2021. The binary edge owns process exit handling. Protocol adapters will translate a canonical content model; tools and permissions remain independent of provider wire formats.

## Bootstrap

- `src/main.rs`: executable entry point.
- `src/brand.rs`: central product identity.
- `scripts/check.sh`: mandatory pre-commit quality gate.

## Foundation and provider boundaries

`config`, `credentials`, `paths`, `theme`, `i18n`, `logging`, and `error` are library modules. Their synchronous filesystem operations must be wrapped in blocking tasks when invoked by async orchestration.

`provider/canonical.rs` owns protocol-independent conversation and event types. `sse.rs` handles fragmented UTF-8 and CRLF framing. `anthropic.rs` maps Messages payloads and indexed tool-argument deltas. `transport.rs` sends progress separately from the validated final event list: callers cannot execute tools from a partial turn. Dropping the request future cancels transport IO. `retry.rs` defines bounded retry scheduling.

## Agent, terminal and tool boundaries

`agent` builds canonical requests, executes only validated calls and retains tool-result history. `perm` applies deny/ask/allow precedence and a workspace boundary. `tools` implements native bounded reads/searches; `tools/editing` stages mutations on blocking workers with byte-comparison guards and serialized commits. Interactive and print entry points share the same agent. `tui` owns terminal restoration, native scrollback, grapheme-aware input, timed intro and event-driven redraw; provider and file contents remain untrusted terminal text.

## Planned boundaries

Config, provider adapters, agent orchestration, tools, permissions, sessions, terminal UI, extensibility, usage ledger, and headless interfaces are separate modules. No component is implemented merely by documenting its intended interface.
