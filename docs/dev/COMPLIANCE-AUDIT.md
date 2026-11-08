# Specification compliance audit

Audit date: 2026-11-08. Authority: the external task specification; it is not distributed with this repository. Findings combine three read-only subsystem audits and local build diagnostics. Source presence is not runtime integration or acceptance evidence.

## Milestone matrix

| Milestone | Status | Outstanding requirements |
| --- | --- | --- |
| M0 Bootstrap | Implemented; historical checks recorded | Preserve single package and existing history. |
| M1 Foundations | Runtime repair under verification | Credential-store fallback and redacted startup logger are now connected; executable HTTP authentication/redaction acceptance and a fresh full gate are pending. |
| M2 Provider core | Partial | Connection/reset/mid-stream retries, strict Anthropic event lifecycle, signed reasoning replay, request options, transport acceptance matrix. |
| M3 OpenAI adapters | Partial | Runtime registry/presets/quirks, reasoning replay, request options, malformed argument feedback, capabilities/auth/proxy/hot reload. |
| M4 Terminal | Partial | Completion/history/attachments/overlays and full keyboard behavior; acceptance beyond helper snapshots. |
| M5 Agent | Partial | Safe parallel reads, approval interaction, cancellation-consistent transcript and live tool blocks. |
| M6 Editing | Partial | Automatic checkpoints, runtime rewind/diffs, word-level highlights. |
| M7 Shell | Partial | Approval UI, complete conservative shell classification, sandbox enforcement and honest fallback. |
| M8 Sessions/context | Partial | Runtime persistence/resume/fork/memory, semantic compaction, output spilling and picker. |
| M9 Agency | Partial | Real child agent loops and registered tools, cancellation propagation, live tree/questions/plans/todos. |
| M10 Extensibility | Partial | Execute registered commands; integrate skills/hooks/config UI and template command injection. |
| M11 MCP | Partial | Persistent handshake/session lifecycle, HTTP/SSE framing, dynamic tools/permissions, resources/prompts/reconnect. |
| M12 Usage | Partial | Per-request recording/budgets, verified pricing overrides, cumulative live counters and rate-limit UI. |
| M13 Headless | Partial | Real streaming/stdin, session-aware flags, deterministic exit codes and CLI acceptance tests. |
| M14 Web/git | Partial | Tool registration, parsed URL policy, bounded reads, actual backend contracts and runtime git commands. |
| M15 Hardening | Partial | Long all-protocol acceptance sessions, current-build PTY performance, cancellation/security review. |
| M16 Repository | Partial | Complete truthful docs, community/toolchain files, workflows, links and history audit. |
| M17 Publish | Not started | No origin configured; no verified CI/deployments/release evidence. |

## Structural and history checks

`cargo metadata --no-deps` reports one package named `gitcode`, with library and executable targets. Cargo represents a standalone package as a one-member implicit workspace; there is no explicit `[workspace]` or multi-package layout. All five existing commits have the required maintainer identity.

Existing annotated tags: m0 -> bfbc484, m1 -> c991407, m2 -> 621ac0e, m3 -> 8be2212, m4 through m8 -> 7448c9b. The latter five tags share a combined implementation commit: separate completion boundaries do not exist. Keep historical work; create independently checked completion commits in specification order, then move each affected local tag to its actual acceptance commit. Never move tags merely to simulate completion. Original tag object IDs are backed up outside the repository.

## Critical evidence

- `src/agent/mod.rs`: a real provider -> validated tool calls -> tool results -> provider loop exists. Calls currently execute serially and thinking is discarded from replay history.
- `src/provider/transport.rs`: audit repairs add bounded connection/body/truncated-stream retries and a shared deadline, with retry-attempt live progress suppressed to avoid duplicated append-only text. Narrow retry tests passed; full acceptance remains incomplete.
- `src/provider/registry.rs`: nine preset descriptors and a quirk transformer exist, but runtime selection/application is missing.
- `tests/provider.rs`, `tests/openai.rs`, `tests/provider_failures.rs`, `tests/agent.rs`: real loopback TCP HTTP/SSE mock servers exist, not merely fixtures. Coverage is narrower than the required acceptance matrix.
- `src/tui/app.rs`: most slash commands and advanced input actions are rejected; metadata in `extensibility.rs` is not a command implementation.
- `src/main.rs`: several accepted flags have no behavioral effect. Headless JSON currently emits init/final only, not live deltas.
- `src/usage.rs`: unsourced built-in prices were removed during repair; only explicitly configured model rates resolve. Unknown pricing remains unknown rather than being advertised as exact.
- `src/tui/app.rs`: the original select-loop recreated a 33 ms sleep on every event, starving redraw under continuous streaming. Repair uses a monotonic 60 Hz interval with skipped missed ticks. Live usage now accumulates validated requests using explicit final Usage/Done boundaries emitted by the agent; final Outcome remains authoritative. Two focused regressions passed, with full integration validation pending.
- `src/tui/term.rs`: keyboard enhancement restoration is not backed by matching Kitty negotiation/push evidence; fake-backend restoration tests do not establish the real terminal contract.
- `src/tools/editing.rs`: patch moves require executable-mode preservation acceptance; existing atomic-write mode tests do not prove move behavior.
- `README.md`: prior parity, runtime integration, MSRV and performance claims exceeded evidence. Replace with verified current scope until features land.

## Local validation and repairs during audit

The initial quality gate failed: Rust 1.80 was incorrectly declared despite newer APIs. Restored the previously verified Rust 1.99 declaration. Follow-up checks exposed integration tests compiling source modules as separate crates, causing unresolved imports/dead-code errors; switched headless, MCP, usage and web/git tests to public library imports without suppressing checks. The new soak test also used async checkpoint APIs without await and passed borrowed strings to an owned Secret constructor; those compile errors were corrected. A fresh complete gate subsequently passed on the stable tree: formatting, all-target Clippy with warnings denied, and 138 tests (`audit-stable-gate.log`, exit 0), including saved-credential executable HTTP authentication and reflected-error secrecy. This is repair evidence, not complete product acceptance.

## Completion policy

Do not interpret existing tags, historical test counts, helper tests or artwork as complete milestone acceptance. First finish M1 runtime foundation integration, then M2 and M3 provider acceptance, then subsequent milestones in order. Do not publish, claim parity or create DONE until required evidence exists.
