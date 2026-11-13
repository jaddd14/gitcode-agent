# Usage and cost

CLI and TUI record validated per-request token usage, elapsed time and output tokens/second in a private JSONL ledger after completed turns. TUI counters update during streaming. `/usage` and `/cost` read the ledger; `gitcode usage --json` exposes a summary.

The borderless `/usage` panel is read-only: use Tab or the left/right arrows to change the period, `t` to switch the chart, the up/down arrows or Page Up/Page Down to scroll, and Escape to close it.

Provider/model pricing is not automatically known: cost is represented as unknown, not zero. Requests with a USD budget fail before network access if pricing cannot be enforced. Failed/interrupted requests are not fully captured in the ledger yet.

See [headless](headless.md) and [Known gaps](dev/PROGRESS.md#known-gaps).
