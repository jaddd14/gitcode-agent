# Changelog

All notable changes are documented here. This project follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.3.1] - Unreleased

### Fixed

- Hardened project configuration, command completion, session recovery and interactive approvals.

## [0.3.0] - Unreleased

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
