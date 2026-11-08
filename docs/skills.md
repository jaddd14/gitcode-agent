# Skills and custom commands

Skill discovery adds summaries to the model prompt; the permission-checked `Skill` tool loads the full body on demand. A project skill resides at `.gitcode/skills/<name>/SKILL.md` with name and description frontmatter.

Custom TUI slash templates use confined file inclusion. Commands with model or allowed-tools overrides fail closed because those overrides are not enforced yet. See `src/extensibility.rs` for supported discovery locations and parsing, and [known gaps](dev/PROGRESS.md#known-gaps) for missing interactive features.
