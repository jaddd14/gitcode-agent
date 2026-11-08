# Hooks

Hook parsing and execution helpers exist in `src/extensibility.rs` with regression coverage. The executable does not yet dispatch the full lifecycle hook set. Do not rely on hooks for access control or auditing.

Runtime lifecycle wiring is a speed-first deferred feature; see [Known gaps](dev/PROGRESS.md#known-gaps). Native [permissions](permissions.md) remain the executable enforcement layer.
