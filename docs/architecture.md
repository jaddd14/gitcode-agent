# Architecture

One Cargo package exposes a library and executable. `main` resolves config/auth/session state; CLI and TUI share the bounded agent loop. Provider adapters translate canonical requests/events; tools enforce permissions before workspace or external operations. Session and usage stores persist completed turns.

```mermaid
flowchart LR
  Input[CLI or TUI] --> Agent[Bounded agent loop]
  Agent --> Transport[Retrying HTTP transport]
  Transport --> Adapter[Messages / Chat / Responses]
  Adapter --> Agent
  Agent --> Permissions[Permission engine]
  Permissions --> Tools[Native tools]
  Tools --> Agent
  Agent --> Store[Session and usage JSONL]
```

See [development architecture](dev/ARCHITECTURE.md), [providers](providers.md), [permissions](permissions.md), and [known gaps](dev/PROGRESS.md#known-gaps).
