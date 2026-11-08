# Providers

The executable supports `anthropic`, `openai-chat`, and `openai-responses` protocols through HTTP/SSE. Set `provider.base_url`, `provider.protocol`, `provider.api_key_env`, and the model in [configuration](configuration.md). CLI endpoint/protocol overrides are also available; `gitcode --help` is authoritative.

Changing protocol does not change the credential environment variable automatically. Base URLs for OpenAI-compatible services generally include `/v1`.

Completed Anthropic thinking text and its opaque signature are replayed unchanged. Advanced registry selection, encrypted Responses reasoning and other auth methods remain [known gaps](dev/PROGRESS.md#known-gaps). Tests use local TCP mocks, not real credentials.
