# Decisions

- 2026-11-09: Session creation always syncs the regular session file; directory fsync is Unix-only because ordinary Windows directory opens fail with access denied. Memory ancestor validation now skips only incomplete Windows prefix components, matching session storage, and still validates completed roots and every descendant. Native Windows CI remains required.
- 2026-11-09: Optional terminal extensions use a conservative known-host/platform policy rather than blocking startup probes. Unknown hosts run without synchronized-output/Kitty extensions; graphics remain unsupported. Clipboard access is explicit-user-action only, never startup or background polling; transcript text is passed as stdin data to a fixed native script, not interpolated into shell code.

- 2026-11-09: Session-storage ancestor validation accumulates Windows Prefix components without probing them as files, then validates the complete root and all descendants. Bare verbatim drive/UNC prefixes are not filesystem entries; this preserves per-component symlink and parent-traversal rejection.

- 2026-11-08: Checkpoint literal-pathspec tests respect native filename rules: Unix accepts `:(glob)*` literally, while Windows must reject it without mutation. Shared `[literal]` versus `l` round-trip coverage verifies that glob metacharacters are never interpreted on either platform.

- 2026-11-08: Shadow Git commands use `--git-dir .` inside the validated canonical shadow-store cwd. This avoids passing Rust's Windows verbatim (`\\?\`) paths through Git's argument parser without stripping prefixes, relaxing storage validation, or inheriting user Git configuration. Native Windows CI must verify this repair.

- 2026-11-08: M19 preserves the complete worktree in an external source archive before moving the uncommitted Windows delta onto the current main commit. The main worktree and stable binary remain unchanged. Native three-platform CI is mandatory before merge; missing local MinGW is not Windows compilation evidence.
- 2026-11-08: The updater owns the sole install-origin source constant. Installer rendering reads it; generated installer copies are not independent configuration sources. The custom domain returned 404, so deployment documentation labels it to be enabled.
- 2026-11-08: Canonicalize temporary test roots before strict session-store checks. macOS system temporary-directory symlinks must not weaken production symlink rejection. Windows subprocess tests isolate both HOME and USERPROFILE.

- 2026-11-07: Add crossterm event streams for terminal input, unicode-segmentation/width for grapheme-safe editing, signal-hook for termination cleanup, ignore/regex/globset for native workspace search, and clap for typed CLI parsing. No terminal mouse capture or alternate screen is enabled.
- 2026-11-07: Until an explicit approval UI exists, permission decisions of Ask return an error rather than granting access. Editing runs on blocking workers, serially; individual renames are atomic, multi-file transactions use best-effort rollback and are not crash-atomic.
- 2026-11-07: Unsupported terminal actions are reported explicitly rather than simulated. Full parity remains an acceptance target, not a current product claim.

- 2026-11-07: Use one edition-2021 Cargo package, stable Rust 1.99.0 (installed and verified), to minimize integration and build overhead.
- 2026-11-07: Keep runtime branding in `src/brand.rs`; distributable names also occur in Cargo metadata, configuration directory names, memory file names, and environment variables as these interfaces are introduced.
- 2026-11-07: Support providers through raw HTTP in Rust; the task explicitly requires Anthropic, OpenAI Chat, and Responses wire compatibility without provider-specific SDK coupling.
- 2026-11-07: Bootstrap has no dependencies. M1 adds serde/serde_json for typed configuration and JSONL, toml 0.8 for layered configuration, and thiserror 2 for contextual typed failures.
- 2026-11-07: Test modules may use unwrap for direct assertions; production unwrap/expect remain warning-gated. Blocking foundation IO is synchronous by design; async integration must call it through spawn_blocking.
- 2026-11-07: Development takes place on main with local milestone commits; no remote is configured before M17.
- 2026-11-07: Never claim parity or mark incomplete milestones complete. Keep actual test evidence distinct from planned acceptance criteria.
- 2026-11-07: M3 uses the same bounded HTTP transport for all protocols. Missing usage is flagged estimated; malformed arguments reject a turn rather than being executed. Repair and model feedback are pending agent-loop integration.
- 2026-11-07: M2 adds Tokio for cancellation-safe async IO, reqwest with rustls/JSON/stream only for HTTPS without native TLS build dependencies, and futures-util for byte-stream iteration.
- 2026-11-07: Transport buffers validated final events while emitting only non-executable progress immediately. Truncated streams never expose executable tool calls; connection-drop retries and full reasoning/signature replay are deferred hardening work and must remain visible gaps until verified.
- 2026-11-08: Priority UI hotfix supersedes the previous chrome palette: one rose hue (#B84A74), soft HSL S−5/L+12 percentage points, deep L−18; all body/metadata text uses the same soft or pure white. Four accent presets are selected live with `/theme` and saved only on confirmation. Original intro and mascot RGB colors remain fixed; frozen intro geometry/timing goldens are unchanged. Shared frame title indentation is three cells; only the first visible prompt row replaces its left border with a chevron at column one.
- 2026-11-08: M18 adds 4 extra flat shades to `Theme` (`panel_hover: Rgb(38, 23, 54)`, `selection: Rgb(52, 24, 86)`, `diff_add_bg: Rgb(22, 43, 34)`, `diff_del_bg: Rgb(54, 20, 36)`) along with background/bold/reset ANSI helpers for widget and diff rendering without gradients.
