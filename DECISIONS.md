# Decisions

- 2026-11-07: Use one edition-2021 Cargo package, stable Rust 1.99.0 (installed and verified), to minimize integration and build overhead.
- 2026-11-07: Keep runtime branding in `src/brand.rs`; distributable names also occur in Cargo metadata, configuration directory names, memory file names, and environment variables as these interfaces are introduced.
- 2026-11-07: Support providers through raw HTTP in Rust; the task explicitly requires Anthropic, OpenAI Chat, and Responses wire compatibility without provider-specific SDK coupling.
- 2026-11-07: No dependencies in bootstrap; justify additions alongside their implementations.
- 2026-11-07: Development takes place on main with local milestone commits; no remote is configured before M17.
- 2026-11-07: Never claim parity or mark incomplete milestones complete. Keep actual test evidence distinct from planned acceptance criteria.
