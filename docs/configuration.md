# Configuration

An interactively selected model is remembered for its provider across launches and new sessions. It takes precedence over configuration files; `GITCODE_MODEL` and `--model` take precedence over the saved selection.

Layers: defaults, `~/.gitcode/config.toml`, `.gitcode/config.toml`, `.gitcode/config.local.toml`, environment, applicable CLI overrides. Unknown keys are rejected.

Project configuration cannot redirect the provider, change its credential name, configure executable hooks, enable plugins, or interpolate environment variables by default. Put provider settings in the user configuration or pass an explicit CLI endpoint. To trust a project configuration with these capabilities, set `trust_project_config = true` in the user configuration; a project cannot grant itself this trust.

See [the example](config.example.toml), [hook setup](hooks.md), and [local plugins](plugins.md). Never put a key in this file: `provider.api_key_env` names its environment variable. A private saved credential at `~/.gitcode/credentials/<variable-name>` is the fallback.

`max_turns` defaults to 0 (unlimited). A positive value limits model requests within one user turn, including tool-use steps. Set it in configuration, with `--max-turns`, or for the current interactive process with `/config max_turns <number>`. `/config max_turns 0` removes the limit for that process.

GitCode estimates the conversation size and asks the selected model to summarize older messages when it reaches `compaction_threshold` of `context_window`. Set `auto_compact = false` to disable automatic summarization; manual `/compact` continues to work on demand. The full session remains on disk; the summary and a recent tail sized to approximately 20% of the configured window are used for subsequent model requests. At least two recent records are retained and tool calls remain paired with their results. Summarization is a single provider request covering all older history, matching OpenCode 2.2.6: the whole conversation is sent once, and only a provider rejection shrinks it — first to a plain-text transcript, then, on repeated "too long" rejections, to a shorter transcript that drops the oldest exchanges while always keeping the newest one. A truncated or empty summary is retried once on a smaller transcript; a reply that ignores the required section headings gets one reminder. A failed summary never discards an existing checkpoint. Its token usage is recorded. The `/compact` command runs the same checkpoint mechanism on demand and accepts optional focus instructions. Successful checkpoints are stored privately with the session and reused only when the original conversation still matches. Model summaries can omit details, and the configured window is an estimate rather than a provider guarantee. Set `context_window` to the actual context size of your model when it differs from the default 200,000 tokens.

`mouse_scroll_speed` (default `3`, range `1..=50`) sets how many conversation rows each mouse-wheel notch scrolls. `scroll_animation` defaults to `false` for immediate scrolling; set it to `true` for animated wheel scrolling.

Saved provider profiles created via `/provider` are stored under `[[providers]]` in `~/.gitcode/config.toml`; credentials themselves stay in `~/.gitcode/credentials/`.

The token-sized recent tail and checkpoint handoff were informed by [OpenCode 2.2.6's context management](https://github.com/anomalyco/opencode/blob/v2.2.6/packages/core/src/session/compaction.ts). GitCode uses its own implementation and does not yet implement all of OpenCode's provider-native, background and overflow-recovery mechanisms.


`provider.timeout_seconds` bounds each request attempt, not the whole retry loop, so a slow generation is retried instead of failing the turn. `provider.max_tokens` defaults to 32,000 and caps generated tokens per request. Keep it high for models that write long answers: a low value truncates the reply, and GitCode then asks the model to continue from the cut-off point (up to three times) rather than failing the turn.

`provider.prompt_cache` defaults to `true`. For the official Anthropic HTTPS endpoint, GitCode requests automatic ephemeral prompt caching; for the official OpenAI Responses endpoint, it sends a random per-client cache routing key. Other endpoints are not modified. Set `prompt_cache = false` in `[provider]` to disable these hints. Providers decide cache eligibility and retention; caching is not guaranteed and cache writes can affect charges. Usage displays provider-reported cached tokens separately.
