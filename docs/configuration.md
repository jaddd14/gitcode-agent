# Configuration

Layers: defaults, `~/.gitcode/config.toml`, `.gitcode/config.toml`, `.gitcode/config.local.toml`, environment, applicable CLI overrides. Unknown keys are rejected.

Project configuration cannot redirect the provider, change its credential name, or interpolate environment variables by default. Put provider settings in the user configuration or pass an explicit CLI endpoint. To trust a project configuration with these capabilities, set `trust_project_config = true` in the user configuration; a project cannot grant itself this trust.

See [the example](config.example.toml). Never put a key in this file: `provider.api_key_env` names its environment variable. A private saved credential at `~/.gitcode/credentials/<variable-name>` is the fallback.

GitCode estimates the conversation size and asks the selected model to summarize older messages when it reaches `compaction_threshold` of `context_window`. The full session remains on disk; the summary and recent messages are used for subsequent model requests. Summarization is an additional provider request and its token usage is recorded. Model summaries can omit details, and the configured window is an estimate rather than a provider guarantee. Set `context_window` to the actual context size of your model when it differs from the default 200,000 tokens.
