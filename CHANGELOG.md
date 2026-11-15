# Changelog

All notable changes are documented here. This project follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.4.2] - 2026-11-15

### Fixed

- Shell commands run in independent sessions, wait ten seconds by default, and continue as named background tasks if still running. Completion wakes the idle interface without relying on a timing-sensitive status update. Shell state such as `cd` and `export` no longer persists between calls.
- Parallel read and search results are displayed alongside their respective calls, and Grep's optional path restricts the actual search to that workspace file or directory.
- Bypass mode now bypasses deny rules and the built-in sensitive-path block for native tools and shell commands, as its name promises. Workspace boundaries and editing consistency checks still apply.

## [0.4.1] - 2026-11-16

### Added

- Startup update banner. When a newer release is published, `gitcode` opens with a picker modelled on Codex: a bold `Update available · current → latest` headline, a dim release-notes link, and three choices — run the verified self-update now, skip this time, or skip until the next version. The published version is cached in `~/.gitcode/version.json` and refreshed in the background every 20 hours, so a slow or unreachable network never delays the first frame and the banner that is shown always comes from a completed lookup. A skipped version stays hidden until something newer appears.

## [0.4.0] - 2026-11-16

### Changed

- Permission mode switches now take effect immediately, including in the middle of a running turn. The engine keeps the mode in shared state, so a turn that started in `default` or `plan` stops asking for approval as soon as `bypass` is confirmed instead of waiting for the turn to finish.
- Background commands report their result by themselves. `BashOutput` is gone: a command that outlives `yield_time_ms` becomes a named task (`cargo-build-1`, `npm-test-2`) and its output is appended to the conversation as soon as it exits, both during a turn and while the session idles, so the model continues instead of polling. `KillShell` accepts the label.
- The session journal is updated while a turn is running: the operator prompt is written before the model runs and completed steps are flushed as tools finish. Reopening a chat after a dropped connection shows the current history immediately.
- `/resume` starts a clean transcript instead of appending under the previous one.

### Fixed

- Context grows much slower. Requests now send a trimmed projection where older tool results keep only a bounded prefix, and compaction occupancy is measured the same way the status bar measures it. Full history remains in the session and in `/transcript`.

## [0.3.3] - 2026-11-15

### Added

- `UserPromptSubmit`, `Stop`, `SubagentStop` and `PreCompact` lifecycle hooks now dispatch in interactive and headless sessions, including subagent turns. Their `additionalContext` is appended to the conversation as a `[Hook context]` message. `SessionStart`, `SessionEnd` and `Notification` remain rejected at configuration load rather than silently ignored.
- MCP clients expose `resources/list`, `resources/read`, `prompts/list` and `prompts/get` on both the stdio and HTTP transports, with bounded list sizes, truncated text and placeholder substitution for binary contents.
- Extended thinking is requested for Anthropic when a reasoning effort is configured; the budget is derived from effort and kept below `max_tokens`.

### Fixed

- Prompt caching now actually applies. Anthropic ignores a top-level `cache_control`, so the markers moved onto the operator prompt block and the trailing conversation blocks, which is where the Messages API reads them. Caching also no longer requires the official Anthropic host, so Anthropic-compatible relays benefit too.
- Removed the unused official-host tracking that the previous cache heuristic required.

## [0.3.2] - 2026-11-14

### Fixed

- Context summarization sends the whole older history in one request instead of 15–47 sequential chunked requests, matching OpenCode 2.2.6. Provider "too long" and "payload too large" rejections now shrink the transcript and retry instead of failing, and a failed summary keeps the existing checkpoint.
- A turn that reaches the output token limit is continued from the cut-off point (up to three times) instead of failing with `provider turn stopped without completion: max_tokens`.
- The per-request timeout now bounds each attempt rather than the whole retry loop, so a slow generation is retried instead of ending the turn.
- `provider.max_tokens` is configurable and defaults to 32,000, which stops long answers from being truncated mid-sentence.
- Completing a truncated answer collects its whole text rather than only the last part.

## [0.3.1] - 2026-11-14

### Added

- Automatic context summaries with retained session history, attachments and a live progress indicator.
- Windows session persistence through an installed psmux.
- Session import and a token-sized recent context tail for compaction.
- Remember the selected model per provider across launches and new sessions.

### Fixed

- Hardened project configuration, command completion, session recovery and interactive approvals.
- Deny common credential paths in native file tools and redact common credential forms in diagnostic logs.
- Update the README to describe current functionality, setup and security boundaries.
- Hide empty sessions from resume and continue, while retaining their journals.
- Close table borders, shorten long tool-call previews, and prevent stale mouse selections.
- Recognize current psmux version output on Windows.

## [0.3.0] - 2026-11-09

### Added

- Native Windows platform boundaries, shell selection, process-tree Job Objects, native home paths and credential ACL hardening.
- Checksum-verified per-user installers, bounded self-update and source revision in version output.
- Three-platform CI and native installer smoke workflows; Windows availability remains pending their successful execution.
- Distribution assembly and static-hosting deployment instructions. The custom-domain installer endpoint is not enabled yet.

## [0.2.2] - 2026-11-12

### Fixed

- Tool calls whose arguments arrive as several complete JSON objects in one streamed block are now expanded into separate calls instead of failing with `invalid tool argument JSON`; the silent whole-request retry that masked the lost call is gone.
- A held mouse selection keeps extending while the wheel scrolls the conversation, including the automatic edge scroll at the top and bottom of the viewport.
- The slash-command popup stays closed for pasted text, paths, multi-line input and commands with arguments.
- `2>/dev/null`, `>/dev/null` and `2>&1` pass the conservative Bash splitter; every other redirect still fails closed.
- Runs of blank lines between blocks collapse into a single separator, so model-emitted extra newlines no longer open wide gaps in the transcript.

### Changed

- Model guidance now asks for one short sentence before the first tool call and at direction changes, and for an outcome-first final message.
- Startup header lists a shorter command set, two releases, and a smaller emblem shifted left so it is not clipped on narrow terminals.

## [0.2.1] - 2026-11-08

### Fixed

- Unified terminal chrome around a muted rose hue with HSL-derived accents; removed gray metadata and purple chrome.
- Closed header edges, padded footer tips, shared frame-title indentation, and a chevron that replaces the first prompt-row border.
- Added a live `/theme` picker with four rose presets and soft/pure white text, preserving existing user configuration on save.
- Corrected terminal escape stripping and added cell-level palette regressions; preserved the original intro colors and timing.

## [0.2.0] - 2026-11-08

### Added

- Reusable terminal widget library (`Frame`, `Chip`, `Keycap`, `KeyHints`, `ProgressBar`, `Table`, `Tabs`, `BarChart`, `vertical_bar_chart`, `Sparkline`, `List`, `Spinner`, `Card`, `Divider`, `Toast`) with Unicode/ASCII fallback and snapshot coverage across widths 50–200.
- Framed startup header block (`✿ gitcode 0.2.0`) pairing the pixel sakura mascot with aligned Russian metadata (`Модель`, `Провайдер`, `Режим`, `Папка`, `Ветка`), changelog highlights (`Новое`), and contextual tips.
- Framed multiline prompt box (`gitcode (•ᴗ•) ~`) with mode chip footer (`DEFAULT · shift+tab`), contextual keycap hint line, and up to 12 visible input rows before internal scrolling.
- Instant synchronous `/` slash-command popup (`Команды`) directly above the prompt box with category chips, fuzzy/prefix filtering, and `↑`/`↓`/`Tab`/`Enter`/`Esc` keyboard navigation.
- Structured `Card` output for all 34 built-in slash commands (`docs/dev/COMMANDS.md`), including the interactive 5-tab `/usage` panel (`Сессия · Сегодня · 7 дней · 30 дней · Всё время`) with `По моделям` and `По агентам` tables, 14-day bar chart, and rate-limit summary.
- Segmented two-zone status bar on the panel background strip with live token counters, cache totals, throughput, Rotating Russian busy activity line (`✻ Колдую…`), in-place retry notice (`▲ Лимит запросов…`), and context progress bar.
- Native `aarch64-unknown-linux-gnu` release build target in the deployment pipeline.

## [0.1.0] - 2026-11-08

### Added

- Terminal coding agent with Anthropic Messages, OpenAI Chat Completions and Responses HTTP/SSE adapters.
- Bounded model/tool loop, native workspace read/search/edit tools and persistent shell sessions.
- Unicode terminal input, incremental Markdown and cumulative live token counters.
- JSONL conversation persistence, resume/continue and hierarchical project memory.
- Headless execution with live JSON deltas and validated-request usage records.
- Local TCP mock acceptance tests, retry/cancellation checks and repository quality gate.

### Known limitations

- Permission prompts, OS sandboxing, complete slash commands and several advanced integrations remain unfinished; see `docs/dev/PROGRESS.md`.
- Initial release verified with Linux/macOS CI, Security and successful staging/release deployments; documented feature gaps remain.
