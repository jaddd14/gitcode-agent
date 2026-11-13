# Headless execution

```bash
gitcode -p 'Summarize src/lib.rs' --output-format stream-json
printf 'Summarize src/lib.rs' | gitcode --output-format json
```

Live stream-JSON contains `init`, `message_delta`, `thinking_delta`, and final `result` events. Init identifies the real session; result includes elapsed duration. This is newline-delimited JSON, not one JSON array. See `src/headless.rs` for the full typed event schema.

Stdin is bounded to 8 MiB. Stream-JSON input accepts lines with string `text` or string `message.content`; arbitrary content-block arrays are unsupported. Tool/error event parity and some CLI options remain [known gaps](dev/PROGRESS.md#known-gaps).

Completed turns persist and can be resumed with `--resume <id>` or `--continue`. On a reported failure, completed tool results are checkpointed so the session can be resumed; abrupt process termination can still lose in-flight work. See [usage](usage-and-cost.md).
