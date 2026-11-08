# Decisions

- 2026-11-07: Add crossterm event streams for terminal input, unicode-segmentation/width for grapheme-safe editing, signal-hook for termination cleanup, ignore/regex/globset for native workspace search, and clap for typed CLI parsing. No terminal mouse capture or alternate screen is enabled.
- 2026-11-07: Until an explicit approval UI exists, permission decisions of Ask return an error rather than granting access. Editing runs on blocking workers, serially; individual renames are atomic, multi-file transactions use best-effort rollback and are not crash-atomic.
- 2026-11-07: Unsupported terminal actions are reported explicitly rather than simulated. Full parity remains an acceptance target, not a current product claim.

- 2026-11-07: Use one edition-2021 Cargo package, stable Rust 1.99.0 (installed and verified), to minimize integration and build overhead.
- 2026-11-07: Keep runtime branding in `src/brand.rs`; distributable names also occur in Cargo metadata, configuration directory names, memory file names, and environment variables as these interfaces are introduced.
- 2026-11-07: Support providers through raw HTTP in Rust; the task explicitly requires Anthropic, OpenAI Chat, and Responses wire compatibility without provider-specific SDK coupling.
- 2026-11-07: Bootstrap has no dependencies. M1 adds serde/serde_json for typed configuration and JSONL, toml 0.8 for layered configuration, and thiserror 2 for contextual typed failures.
- 2026-11-07: Test modules may use unwrap for direct assertions; production unwrap/expect remain warning-gated. Blocking foundation IO is synchronous by design; async integration must call it through spawn_blocking.
- 2026-11-07: Development takes place on main with local milestone commits; no remote is configured before M17.
- 2026-11-07: Never claim parity or mark incomplete milestones complete. Keep actual test evidence distinct from planned acceptance criteria.
- 2026-11-07: M3 uses the same bounded HTTP transport for all protocols. Missing usage is flagged estimated; malformed arguments reject a turn rather than being executed. Repair and model feedback are pending agent-loop integration.
- 2026-11-07: M2 adds Tokio for cancellation-safe async IO, reqwest with rustls/JSON/stream only for HTTPS without native TLS build dependencies, and futures-util for byte-stream iteration.
- 2026-11-07: Transport buffers validated final events while emitting only non-executable progress immediately. Truncated streams never expose executable tool calls; connection-drop retries and full reasoning/signature replay are deferred hardening work and must remain visible gaps until verified.
