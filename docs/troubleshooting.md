# Troubleshooting

- Run `gitcode doctor --json` to inspect local configuration and key availability. No live request is required.
- Authentication: check the environment variable named by `provider.api_key_env`; protocol overrides do not change that name.
- Approval required: the current UI fails closed; do not solve this by blindly enabling bypass on untrusted workspaces.
- Invalid config: unsupported keys are rejected; start from [the example](config.example.toml).
- Truncated stream: incomplete provider responses are rejected rather than executing partial tool arguments; bounded retries may apply.
- Resume: use `--continue` or `--resume <id>`; interrupted-turn durability remains limited.

Never include credentials when sharing logs. See [security](../SECURITY.md), [configuration](configuration.md), and [Known gaps](dev/PROGRESS.md#known-gaps).
