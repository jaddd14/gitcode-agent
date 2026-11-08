# Development progress

## Current state

Speed-first delivery policy accepted on 2026-11-08: at most 40 minutes per milestone, no independent audits of green milestones, and unresolved depth recorded under Known gaps. M0 and M1 are closed for the intermediate delivery; this is not a full-parity claim. M2 is receiving only minimal signed-thinking replay before proceeding toward a working M8. Intermediate publication is authorized after a green M8, with the original credential and secret-scan rules unchanged. See [the historical compliance audit](COMPLIANCE-AUDIT.md). Task specification, credentials, and machine-readable recovery state remain outside this repository.

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
- M9–M14: intermediate scope closed under the speed-first policy; see tested integrations and Known gaps below.
- M15: reproducible release/process measurements recorded; full PTY/soak budgets deferred.
- M16: documentation and release automation in progress.
- M17: intermediate publication verified; final release pending.

### Deferred provider acceptance work

Minimal Anthropic thinking replay now preserves text and concatenated signature verbatim through canonical events into retained agent history. The regression and full gate passed (`fast-m2-gate.log`, exit 0; 139 tests). M2 is closed for intermediate delivery. Advanced request options, encrypted Responses reasoning replay, provider overload classification and registry depth remain Known gaps, not blockers for the next milestone.

## Evidence

- Installed `rustc 1.99.0 (b940084d7 2026-09-28)` and Cargo 1.99.0.
- GitHub user identity verified through authenticated GET /user; no token persisted inside the repository.
- Work runs inside the existing `gitcode` tmux session.
- Recovery probe: interrupted a dedicated tmux work window, reloaded state, verified m1 still exists, and resumed M2 without reinitializing or repeating finished milestones (`resilience.log`).

## Speed-first delivery updates

- M13 intermediate: live message/thinking stream-JSON, actual session IDs/duration, bounded stdin and string-based stream-JSON input are wired. Executable mock regression verifies init/delta/result output.
- M12 intermediate: validated per-request usage and latency are retained by the agent and recorded by both CLI/TUI after completed turns. Unknown cost is explicit; USD budget requests fail closed before network when pricing cannot be enforced.
- Intermediate main `e521895`, followed by `37e26a7`, was pushed successfully after clean history and working-tree secret scans. GitHub Security run 42458989226 succeeded; CI run 42458989290 is still running. No release/deployment completion is claimed.
- Release build and `gitcode --version` passed locally (`fast-release-build.log`, `fast-repo-gate.log`). Workflow YAML was parsed locally. Community templates, pinned toolchain, dependency policy and source-verifying staging workflow are present.
- TodoWrite is now registered in native model tools and connected to the existing validated task-list implementation; deep child-agent runtime remains deferred.

## Latest speed-first checkpoint

- M9/M10 intermediate: TodoWrite and Skill are registered in the tool executor; skill summaries enter the prompt, full skill bodies load on demand, and custom TUI slash templates expand through confined file inclusion. Unsupported per-command tool/model overrides fail closed. Existing agency/extensibility regressions remain green; real child loops, hooks and interactive questions remain deferred.
- M14 intermediate: WebFetch is registered with explicit network permission, parsed URL domain enforcement, bounded streaming reads and untrusted wrapping. Search backends and local git wrapper have mock tests; full interactive git commands remain deferred.
- Published commit `3160799` has successful CI 42459578361, Security 42459578398 and staging Deploy 42459578362. This is an actual verified staging build/smoke deployment, not a running hosted service.

- M11 intermediate: permission-gated McpCall connects configured stdio/HTTP servers to the native executor. Default-mode denial regression and full quality gate passed (`fast-mcp-gate.log`, exit 0). Dynamic discovery, persistent connections and resources UI remain deferred.
- M15 measurements (`fast-measure-release.log`, exit 0): Linux 6.8 aarch64, Rust 1.99.0; fresh release 5,289,176 bytes; warm-cache version/help average 2.20/1.70 ms over ten launches; version-only peak RSS 3,456 KiB. No PTY-first-frame or active-session performance claim.

## Release preparation evidence

- `eeb73b9` passed the full quality gate, warnings-denied rustdoc and release build (`fast-release-commit.log`, exit 0), then was pushed after clean history/tree secret scans. Relative documentation links and all workflow YAML parsed successfully.
- M16 intermediate documentation includes configuration, providers, permissions, headless, usage, skills, hooks, MCP, agency, architecture and troubleshooting. Unsupported runtime features are explicitly labelled.
- Repository is public as specified. Description/topics/merge settings configured; vulnerability alerts, automated security fixes and private vulnerability reporting enabled. Community API reports 100% health and MIT; language API reports Rust. Checks annotation API returned 403 for the token; no extra permission was requested.
- Initial macOS-14 CI runner was cancelled before allocating a runner or executing steps. Moving to the currently supported macos-latest label; no tests are removed or weakened.
- v0.1.0 changelog and two-target artifact/release workflow prepared; final tag and release verification are still pending. Existing pushed history is retained without force rewriting.

## Verified v0.1.0 publication

- Release commit: `b3aca365d70cdcc35e95104b1f71ec47e9f08185`; annotated `v0.1.0` resolves to it remotely. Linux/macOS CI [42466293350](https://github.com/jaddd14/gitcode-agent/actions/runs/42466293350) and Security [42466293296](https://github.com/jaddd14/gitcode-agent/actions/runs/42466293296) succeeded.
- Staging [42466725316](https://github.com/jaddd14/gitcode-agent/actions/runs/42466725316) and release pipeline [42466971878](https://github.com/jaddd14/gitcode-agent/actions/runs/42466971878) succeeded. Latest staging deployment `7634015026` and release deployment `7634053941` both report `success` via API.
- [Latest release v0.1.0](https://github.com/jaddd14/gitcode-agent/releases/tag/v0.1.0) is published, not draft/prerelease, with Linux x86_64 and macOS aarch64 tarballs and SHA256SUMS. This is a speed-first initial release with the explicit limitations below, not full workflow parity.
- Public banner and preview raw URLs return HTTP 200; README and issue forms are available through API. Community health is 100%, license MIT, reported language Rust. Existing history was preserved; no force push performed.
- Final evidence-only main commit and its CI/staging will be verified after pushing. M17 tag marks this evidence checkpoint, while v0.1.0 remains on the tested release source. Social preview image upload remains a manual repository-settings action; the Checks annotations endpoint returned HTTP 403 with the current token.

## Known gaps

Speed-first deferrals (2026-11-08): advanced provider registry/auth/hot reload, encrypted reasoning replay and request options; Kitty negotiation; editing move-mode preservation and automatic rewind UI; interactive approval UI and OS sandbox; session picker, semantic compaction/output spilling and crash-consistent interrupted-turn persistence. Deep subagents, MCP, hooks, web and git integration are deferred until after intermediate publication. These are known limitations, not implemented parity.

The executable supports interactive mode and `-p` with all three provider protocols. Interactive permission approval is not implemented: operations requiring approval fail closed. Persistent history, vim, autocomplete, attachments, custom statusLine, advanced overlays, extensibility, performance measurements and publication remain pending. Chat invalid-JSON repair, prompted-tools fallback, vision, registry hot reload/auth variants and full reasoning replay remain unimplemented. Editing uses per-file atomic renames and best-effort multi-file rollback; descriptor-relative race protection, ownership/ACL/xattr preservation and mixed-newline fidelity remain gaps. SIGKILL cannot be handled in-process. No DONE marker has been created.
