# Changelog

All notable changes are documented here. This project follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

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
