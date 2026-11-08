# Development progress

## Current state

Compliance audit on 2026-11-08 supersedes earlier completion claims below. M0 bootstrap exists; M1–M16 are partial at product acceptance level and M17 is not started. Historical 102-test results apply only to the earlier core snapshot, not the current working tree. See [the per-milestone audit](docs/dev/COMPLIANCE-AUDIT.md). Task specification, credentials, and machine-readable recovery state remain outside this repository.

The A1.3 resume routine was exercised again: read external state and development records, inspected status/history/tags/tmux logs, and preserved existing work. Cargo metadata confirms one standalone package; no explicit workspace is declared. M4–M8 share one historical commit and need separate checked completion commits before retagging. Foundation integration (M1), then provider acceptance (M2/M3), precede further publication work. README now distinguishes runtime features from helpers and removes unsupported performance/parity claims. The fresh stable-tree quality gate passed: formatting, all-target Clippy with warnings denied, and 138 tests (`audit-stable-gate.log`, exit 0). This includes executable saved-key HTTP authentication and reflected-error redaction, retry/lifecycle regressions, agent request boundaries, and non-starving redraw/cumulative live-usage regressions. No current milestone completion tag has been moved.

## Milestones

- M0: complete. Source skeleton, profiles, identity, quality gate and recovery setup verified. `scripts/check.sh` and release build pass (log outside repository: `m0-check.log`, exit 0). Bootstrap contains no tests yet; test count is honestly zero.
- M1: complete. Typed errors, strict layered TOML configuration, explicit environment interpolation, flat theme fallbacks, ru/en UI labels, private credentials, and redacted JSONL logging. `scripts/check.sh` green: 7 unit and 2 integration tests. Release build green (`m1-green.log`, exit 0).
- M2: core verified. Canonical content model, fragmented UTF-8/CRLF SSE decoding, bounded HTTP status retries, Anthropic request/stream adapter and mock TCP tests. Quality gate and release build green (`m2-full.log`, exit 0): 10 unit + 8 integration tests. Every fixture byte boundary tested; 401 safe error, truncated stream rejection and timeout cancellation verified.
- M3: adapter core verified. Chat Completions and stateless Responses encoders/assemblers connected to HTTP transport; nine endpoint presets and explicit Chat quirks. 22 total tests pass including every fixture byte boundary and actual mock HTTP for both protocols. `m3-full.log` records quality gate and release build exit 0.
- M4: terminal foundation verified by 8 terminal tests (guard drop/panic/signal cleanup with injected backend, header snapshots, deterministic intro, Unicode/paste/history editor, native scrollback). Interactive entry point shares the agent with print mode.
- M5: end-to-end agent, permission engine, native Read/LS/Glob/Grep, and streaming incremental Markdown renderer (`10/10` tests in `tests/markdown.rs`) verified.
- M6: workspace editing (`Write`, `Edit`, `MultiEdit`, `NotebookEdit`, `apply_patch` — `23/23` tests), bounded line diff renderer (`2/2` tests), and isolated shadow-git checkpoint store (`12/12` tests) verified.
- M7: persistent non-interactive Bash session, background jobs (`BashOutput`, `KillShell`), and conservative quote-aware command splitter (`8/8` tests in `tests/shell.rs`) verified and wired into `Tools`.
- M8: append-only JSONL session store with interrupted-tail recovery, forking, private Unix modes (`0600`/`0700`), hierarchical `GITCODE.md`/`CLAUDE.md`/`AGENTS.md` loader with confined `@import` expansion (`11/11` tests in `tests/sessions.rs`), and context estimation/tool-result trimming verified.
- M9–M17: in progress.

### Deferred provider acceptance work

Signed/encrypted reasoning replay, advanced cache/tool-choice/thinking request options, connection-reset and mid-stream retry, strict event-order validation and full registry remain pending. The implemented core does not yet satisfy every provider detail in the task.

## Evidence

- Installed `rustc 1.99.0 (b940084d7 2026-09-28)` and Cargo 1.99.0.
- GitHub user identity verified through authenticated GET /user; no token persisted inside the repository.
- Work runs inside the existing `gitcode` tmux session.
- Recovery probe: interrupted a dedicated tmux work window, reloaded state, verified m1 still exists, and resumed M2 without reinitializing or repeating finished milestones (`resilience.log`).

## Known gaps

The executable supports interactive mode and `-p` with all three provider protocols. Interactive permission approval is not implemented: operations requiring approval fail closed. Persistent history, vim, autocomplete, attachments, custom statusLine, advanced overlays, extensibility, performance measurements and publication remain pending. Chat invalid-JSON repair, prompted-tools fallback, vision, registry hot reload/auth variants and full reasoning replay remain unimplemented. Editing uses per-file atomic renames and best-effort multi-file rollback; descriptor-relative race protection, ownership/ACL/xattr preservation and mixed-newline fidelity remain gaps. SIGKILL cannot be handled in-process. No DONE marker has been created.
