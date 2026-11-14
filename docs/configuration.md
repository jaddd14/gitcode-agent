# Configuration

Layers: defaults, `~/.gitcode/config.toml`, `.gitcode/config.toml`, `.gitcode/config.local.toml`, environment, applicable CLI overrides. Unknown keys are rejected.

Project configuration cannot redirect the provider, change its credential name, or interpolate environment variables by default. Put provider settings in the user configuration or pass an explicit CLI endpoint. To trust a project configuration with these capabilities, set `trust_project_config = true` in the user configuration; a project cannot grant itself this trust.

See [the example](config.example.toml). Never put a key in this file: `provider.api_key_env` names its environment variable. A private saved credential at `~/.gitcode/credentials/<variable-name>` is the fallback.

`max_turns` defaults to 0 (unlimited). A positive value limits model requests within one user turn, including tool-use steps. Set it in configuration, with `--max-turns`, or for the current interactive process with `/config max_turns <number>`. `/config max_turns 0` removes the limit for that process.

GitCode estimates the conversation size and asks the selected model to summarize older messages when it reaches `compaction_threshold` of `context_window`. The full session remains on disk; the summary and a recent tail sized to approximately 20% of the configured window are used for subsequent model requests. At least two recent records are retained and tool calls remain paired with their results. Summarization is an additional provider request and its token usage is recorded. The `/compact` command runs the same checkpoint mechanism on demand and accepts optional focus instructions. Successful checkpoints are stored privately with the session and reused only when the original conversation still matches. Model summaries can omit details, and the configured window is an estimate rather than a provider guarantee. Set `context_window` to the actual context size of your model when it differs from the default 200,000 tokens.

The token-sized recent tail and checkpoint handoff were informed by [OpenCode 2.2.6's context management](https://github.com/anomalyco/opencode/blob/v2.2.6/packages/core/src/session/compaction.ts). GitCode uses its own implementation and does not yet implement all of OpenCode's provider-native, background and overflow-recovery mechanisms.


`provider.prompt_cache` defaults to `true`. For the official Anthropic HTTPS endpoint, GitCode requests automatic ephemeral prompt caching; for the official OpenAI Responses endpoint, it sends a random per-client cache routing key. Other endpoints are not modified. Set `prompt_cache = false` in `[provider]` to disable these hints. Providers decide cache eligibility and retention; caching is not guaranteed and cache writes can affect charges. Usage displays provider-reported cached tokens separately.
