# Configuration

Layers: defaults, `~/.gitcode/config.toml`, `.gitcode/config.toml`, `.gitcode/config.local.toml`, environment, applicable CLI overrides. Unknown keys are rejected.

Project configuration cannot redirect the provider, change its credential name, or interpolate environment variables by default. Put provider settings in the user configuration or pass an explicit CLI endpoint. To trust a project configuration with these capabilities, set `trust_project_config = true` in the user configuration; a project cannot grant itself this trust.

See [the example](config.example.toml). Never put a key in this file: `provider.api_key_env` names its environment variable. A private saved credential at `~/.gitcode/credentials/<variable-name>` is the fallback.

`compaction_threshold` is validated configuration, not a promise of automatic semantic compaction. See [limitations](dev/PROGRESS.md#known-gaps) and [providers](providers.md).
