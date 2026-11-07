# Development progress

## Current state

M0 bootstrap complete. Task specification is outside this repository and must remain there. Credentials and machine-readable recovery state are outside this repository.

## Milestones

- M0: complete. Source skeleton, profiles, identity, quality gate and recovery setup verified. `scripts/check.sh` and release build pass (log outside repository: `m0-check.log`, exit 0). Bootstrap contains no tests yet; test count is honestly zero.
- M1: complete. Typed errors, strict layered TOML configuration, explicit environment interpolation, flat theme fallbacks, ru/en UI labels, private credentials, and redacted JSONL logging. `scripts/check.sh` green: 7 unit and 2 integration tests. Release build green (`m1-green.log`, exit 0).
- M2: core verified. Canonical content model, fragmented UTF-8/CRLF SSE decoding, bounded HTTP status retries, Anthropic request/stream adapter and mock TCP tests. Quality gate and release build green (`m2-full.log`, exit 0): 10 unit + 8 integration tests. Every fixture byte boundary tested; 401 safe error, truncated stream rejection and timeout cancellation verified.
- M3–M17: not complete. An unconnected Chat adapter is in progress.

### Deferred provider acceptance work

Signed/encrypted reasoning replay, advanced cache/tool-choice/thinking request options, connection-reset and mid-stream retry, strict event-order validation and full registry remain pending. The implemented core does not yet satisfy every provider detail in the task.

## Evidence

- Installed `rustc 1.99.0 (b940084d7 2026-09-28)` and Cargo 1.99.0.
- GitHub user identity verified through authenticated GET /user; no token persisted inside the repository.
- Work runs inside the existing `gitcode` tmux session.
- Recovery probe: interrupted a dedicated tmux work window, reloaded state, verified m1 still exists, and resumed M2 without reinitializing or repeating finished milestones (`resilience.log`).

## Known gaps

Only the bootstrap version command exists. Provider adapters, agent functionality, tools, TUI, extensibility, performance measurements and publication are not yet implemented. No DONE marker has been created.
