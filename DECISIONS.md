# Decisions

- 2026-11-07: Use one edition-2021 Cargo package, stable Rust 1.99.0 (installed and verified), to minimize integration and build overhead.
- 2026-11-07: Keep runtime branding in `src/brand.rs`; distributable names also occur in Cargo metadata, configuration directory names, memory file names, and environment variables as these interfaces are introduced.
- 2026-11-07: Support providers through raw HTTP in Rust; the task explicitly requires Anthropic, OpenAI Chat, and Responses wire compatibility without provider-specific SDK coupling.
- 2026-11-07: Bootstrap has no dependencies. M1 adds serde/serde_json for typed configuration and JSONL, toml 0.8 for layered configuration, and thiserror 2 for contextual typed failures.
- 2026-11-07: Test modules may use unwrap for direct assertions; production unwrap/expect remain warning-gated. Blocking foundation IO is synchronous by design; async integration must call it through spawn_blocking.
- 2026-11-07: Development takes place on main with local milestone commits; no remote is configured before M17.
- 2026-11-07: Never claim parity or mark incomplete milestones complete. Keep actual test evidence distinct from planned acceptance criteria.
