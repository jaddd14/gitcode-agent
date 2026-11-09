# Built-in slash commands

The dispatcher renders bounded cards, not interactive pickers unless noted below. Descriptions describe actual local behavior, not the full M18 target. Commands never call a model. Unsupported arguments are rejected rather than silently ignored. Changes to model, limits, and permission rules are process-local and are not saved to configuration.

| Command | Accepted arguments | Current UI and behavior |
|---|---|---|
| `/help` | None | Grouped command card and keyboard reference. |
| `/clear` | None | Clears in-memory messages; saved journals remain. |
| `/new` | None | Alias for `/clear`, not a new persistent session. |
| `/compact` | None | Trims long tool results; card reports approximate token reduction. No model summarization. |
| `/config` | `[model <id>\|max_turns <positive integer>]` | Settings table; only these two keys can be changed. Other displayed settings are read-only. |
| `/model` | `[model-id]` | Changes model identifier; static example identifiers are not a discovered/validated availability list. No picker. |
| `/provider` | None | Transport/auth configuration reference; does not switch providers. |
| `/usage` | None | Interactive period tabs and chart toggle in the TUI app; dispatcher returns a static card. See limitations below. |
| `/cost` | None | Current status counters and estimated cost; no top-model ranking. |
| `/status` | None | Model, permission, workspace, branch, message count and counters table. |
| `/context` | None | Approximate text-based token table and aggregate bar, assuming an explicitly labeled 200k window. Not a measured model window or stacked bar. |
| `/resume` | `[session-id]` | Lists up to eight project sessions. With ID, loads saved messages into memory; invalid/corrupt/inaccessible IDs preserve current context. Does not switch active persistence journal, restore counters, or restore workspace files. |
| `/rewind` | `[message-index]` | Lists recent messages or removes messages from a zero-based index onward. Does not restore files or shadow-git checkpoints. |
| `/plan` | `[task]` | Enables read-only permission mode. Task text is displayed, not submitted for execution. |
| `/permissions` | `[allow\|ask\|deny Tool(pattern)]` | Rule counts/list and process-local additions. No deletion/editor/persistence. |
| `/agents` | None | Profile table, not a launch/detail picker. |
| `/mcp` | None | Configured server/transport table; not live connection health or tool counts. |
| `/init` | None | Creates missing `GITCODE.md` using the shared Write executor, including path checks, permission rules, PLAN prohibition and read guards. Existing files are not overwritten. An Ask decision is reported, not auto-approved; no inline approval flow is wired. |
| `/review` | None | `git status --short --branch` summary only; no code review. |
| `/diff` | None | First 18 lines of unstaged tracked-file `git diff --no-ext-diff --no-color`; no staged/untracked view or hunk navigation. |
| `/commit` | `[draft-message]` | Readiness/status and draft only. Never stages or commits. |
| `/memory` | None | First 14 lines of workspace `GITCODE.md`; not a memory editor. |
| `/doctor` | None | Existing diagnostic checklist; not an exhaustive platform/network check. |
| `/theme` | None | Current palette and compatibility reference; no theme switching. |
| `/vim` | None | Explicitly reports that Vim mode is unavailable; shows actual editor shortcuts. |
| `/export` | `[relative-path.json]` | Serializes messages through shared Write permission/path/read guards. Without path shows usage. Ask is not auto-approved. |
| `/copy` | None | On an interactive Windows terminal, copies the last assistant text through a fixed PowerShell clipboard script with UTF-8 stdin and a five-second timeout. Success toast follows only a successful native exit. Redirected/non-Windows/unavailable clipboards retain an honest failure card and `/export` alternative. |
| `/hooks` | None | Supported-event reference, not a configured-hook viewer/editor. |
| `/skills` | None | Discovery list, not a detail/execute picker. |
| `/add-dir` | `[path]` | Validates a directory inside the existing workspace; does not add access roots. |
| `/login` | None | Credential setup reference; does not accept/store keys or perform authentication. |
| `/logout` | None | Credential-file presence and cleanup instructions; does not delete keys or unset environment variables. |
| `/terminal-setup` | None | Shortcut reference; does not modify terminal configuration. |
| `/exit` | None | App intercepts exact `/exit` and exits; direct dispatcher call returns a farewell card. |

## Verification

`cargo test --test command_actions` exercises commands offline with temporary HOME/workspace directories and a non-serving localhost provider URL. It checks permission rejection and accepted writes, context restore/failure preservation, honest clipboard/commit/Vim responses, config validation, exports and card cell bounds at widths 60/80/120. It does not contact a provider or use the operator's real `~/.gitcode`. The width checks execute command handlers, not synthetic frames, but are not per-command golden snapshots.

## Remaining M18 command gaps

- Model/provider, session/checkpoint, agent, permission and configuration screens are not interactive selection/editing flows. Session list lacks age; resume is context import only.
- Shared Write handles `/init` and `/export`, but their Ask decisions have no interactive approval continuation. Neither command grants itself approval.
- Clipboard integration, actual commit editor/confirmation, Vim mode, extra workspace roots, authentication/logout actions, theme switching and terminal setup are not implemented by these commands.
- Diff is truncated and static; no file/hunk navigation, staged/untracked coverage, or file restoration through `/rewind`. Review is git status only.
- Context window and categories are estimates, not provider-reported usage; no stacked segments. Compact trims results only. MCP health/tool counts, configured hook details and agent details are absent.
- The static usage dispatcher passes an empty session ID; current-session attribution requires the app's session-aware path. Ledger read failures produce an error card, not a fabricated empty state. Complete usage semantics/live rate-limit data must be verified separately in `usage_panel`/app, not inferred from this command table.
- Per-command golden snapshots at 60/80/120 and the full real-terminal M18 verification matrix remain required. Widget/card bounds tests alone do not fulfill those acceptance criteria.
