# Architecture

Single stable-Rust Cargo package, edition 2021. The binary edge owns process exit handling. Protocol adapters will translate a canonical content model; tools and permissions remain independent of provider wire formats.

## Bootstrap

- `src/main.rs`: executable entry point.
- `src/brand.rs`: central product identity.
- `scripts/check.sh`: mandatory pre-commit quality gate.

## Planned boundaries

Config, provider adapters, agent orchestration, tools, permissions, sessions, terminal UI, extensibility, usage ledger, and headless interfaces are separate modules. No component is implemented merely by documenting its intended interface.
