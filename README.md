<p align="center">
  <img src="assets/icons/sakura.svg" alt="GitCode" width="72" height="72">
</p>

<h1 align="center">GitCode</h1>

<p align="center">The local-first terminal coding agent.</p>

<p align="center">
  <a href="https://github.com/jaddd14/gitcode-agent/releases/latest"><img alt="Release" src="https://img.shields.io/github/v/release/jaddd14/gitcode-agent?style=flat-square&labelColor=29232b"></a>
  <a href="https://github.com/jaddd14/gitcode-agent/actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/jaddd14/gitcode-agent/ci.yml?branch=main&style=flat-square&label=CI&labelColor=29232b"></a>
  <a href="https://github.com/jaddd14/gitcode-agent/actions/workflows/deploy.yml"><img alt="Deploy" src="https://img.shields.io/github/actions/workflow/status/jaddd14/gitcode-agent/deploy.yml?branch=main&style=flat-square&label=deploy&labelColor=29232b"></a>
  <a href="rust-toolchain.toml"><img alt="Rust 1.99+" src="https://img.shields.io/badge/Rust-1.99%2B-c87591?style=flat-square&logo=rust&logoColor=white&labelColor=29232b"></a>
  <a href="https://github.com/jaddd14/gitcode-agent/releases/latest"><img alt="Platforms" src="https://img.shields.io/badge/Linux%20%C2%B7%20macOS%20%C2%B7%20Windows-c87591?style=flat-square&label=platforms&labelColor=29232b"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/github/license/jaddd14/gitcode-agent?style=flat-square&label=license&color=c87591&labelColor=29232b"></a>
  <a href="https://github.com/jaddd14/gitcode-agent/stargazers"><img alt="Stars" src="https://img.shields.io/github/stars/jaddd14/gitcode-agent?style=flat-square&label=stars&labelColor=29232b"></a>
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="docs/providers.md">Providers</a> ·
  <a href="docs/configuration.md">Configuration</a> ·
  <a href="docs/permissions.md">Permissions</a> ·
  <a href="CONTRIBUTING.md">Contributing</a> ·
  <a href="SECURITY.md">Security</a>
</p>

GitCode is a local-first terminal coding agent written in Rust. It works with an existing repository, uses a configured language-model provider to investigate and change code, and asks for permission before actions that change files, run shell commands, or access network tools.

GitCode is under active development. Review proposed actions and changes, and use an isolated environment for work you do not trust. Model output and tool results can contain mistakes or hostile instructions.

## <img src="assets/icons/terminal.svg" alt="" width="20" height="20"> What GitCode does

- Reads and searches files, edits text and notebooks, applies patches, and runs shell commands with permission controls.
- Supports Anthropic Messages, OpenAI Chat Completions, OpenAI Responses, and compatible endpoints. Provider credentials stay in your environment or local credential store.
- Saves sessions locally, resumes conversations, and imports supported Claude Code and Codex journals. `/context` shows the estimated context and `/compact` creates a new summary on demand.
- Summarizes older history as the configured context limit approaches while keeping recent messages and tool-call pairs together. The original session remains available locally. Summaries are model-generated and can omit details; they incur provider usage.
- Offers terminal and noninteractive modes, usage accounting, configurable agents, skills, trusted pre/post tool hooks and local hook plugins, web fetch, and configured MCP tool calls with reusable stdio sessions. Integration coverage varies; see the guides below for limitations.

## <img src="assets/icons/rocket.svg" alt="" width="20" height="20"> Install

Prebuilt archives and checksums are available on [GitHub Releases](https://github.com/jaddd14/gitcode-agent/releases). Download the archive for your operating system, verify its SHA-256 against `SHA256SUMS`, and put the `gitcode` executable on your `PATH`. The release assets also include per-user installers.

The installers download the archive for the current platform, check its SHA-256 before touching your `PATH`, and fall back to GitHub Releases if the primary mirror is unavailable.

Linux and macOS:

```sh
curl -fsSL https://api.samp.team/gitcode/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://api.samp.team/gitcode/install.ps1 | iex
```

On Windows the binary is placed in `%LOCALAPPDATA%\Programs\gitcode\bin` and added to the user `PATH`; ARM64 runs the x64 build under emulation. Restart the shell afterwards. The Linux archives are built against glibc 2.34, so they run on Ubuntu 22.04 and newer, Debian 12 and newer, and equivalent distributions.


For a source build, install the toolchain specified in `rust-toolchain.toml` and run:

```sh
git clone https://github.com/jaddd14/gitcode-agent.git
cd gitcode-agent
cargo build --release --locked
./target/release/gitcode --version
```

For faster optimized local builds after the first build, use `cargo build --profile release-fast --locked`; the binary is `target/release-fast/gitcode`. The standard release profile remains the distribution build.

GitCode supports Linux, macOS, and Windows. Platform details and terminal requirements are documented in [Windows](docs/windows.md). Interactive disconnect recovery uses tmux on Unix when available; Windows uses psmux when configured.

## <img src="assets/icons/spark.svg" alt="" width="20" height="20"> Get started

Set a provider key in your environment and choose a model available to your account. For the default Anthropic protocol:

```sh
export ANTHROPIC_API_KEY="your-api-key"
gitcode --model YOUR_MODEL
```

Run from the project directory. Describe the task in the terminal; review permission requests and the resulting diff. GitCode uses Russian by default and follows the language of the conversation. For noninteractive use:

```sh
gitcode --model YOUR_MODEL -p "Explain the architecture of this repository"
gitcode doctor --json
```

Other providers require a matching protocol, endpoint, model and credential-variable configuration. Changing the protocol alone does not change the credential variable. See [provider setup](docs/providers.md) and the [configuration example](docs/config.example.toml).

## <img src="assets/icons/layers.svg" alt="" width="20" height="20"> Context and sessions

The green `ctx` bar and `/context` estimate the current model-facing prompt, including the system instructions, tools, and the projected conversation. They do not add up tokens spent across requests; after compaction the estimate can decrease.

`context_window` must match the model's available window; `compaction_threshold` controls when automatic summarization begins. Context sizes are estimates, not provider token guarantees. The recent tail is kept according to a token budget, and the summary checkpoint is reused only when it still matches the original history. `/compact` accepts optional focus instructions. See [configuration](docs/configuration.md) for costs and limits.

Use `gitcode --continue` or `/resume` to return to a session. `/import` can copy supported conversations into the current project; imported source journals are retained privately. See `/help` for current commands.

## <img src="assets/icons/shield.svg" alt="" width="20" height="20"> Security and privacy

Outside bypass mode, built-in file tools reject common sensitive paths such as `.env`, `.env.*`, `*.pem`, `*.key`, `credentials.toml`, `.ssh` and `.aws`, including resolved symlink targets. This is a limited safeguard, not secret detection: other filenames can contain credentials, and approved shell commands or external processes run with your operating-system privileges. The agent is not an OS sandbox.

Shell execution, workspace edits, web requests and MCP calls require permission in the default mode; headless operations requiring approval fail closed. `--dangerously-skip-permissions` skips prompts and should only be used in an environment you control. Prompt injection remains a risk. Conversations and tool results are sent to the selected provider when needed and are persisted locally; review the provider's data policies and avoid placing secrets in prompts.

Diagnostic logs redact registered credentials and some common token forms as a best effort, not a guarantee for arbitrary secrets. See [SECURITY.md](SECURITY.md) for the security model and private vulnerability reporting.

## <img src="assets/icons/wrench.svg" alt="" width="20" height="20"> Configuration and development

Configuration precedence is defaults, user config, project config, local project config, environment, then applicable CLI flags. A model selected interactively is remembered per provider and takes precedence over config files; `GITCODE_MODEL` and `--model` override it. Project config cannot redirect credentials without explicit user trust. See [configuration](docs/configuration.md), [permissions](docs/permissions.md), [subagents](docs/subagents.md), [MCP](docs/mcp.md), and [hooks](docs/hooks.md).

```sh
./scripts/check.sh
cargo audit
```

Tests use local mock providers and do not establish quality on real coding benchmarks. Contributions and bug reports are welcome; see [CONTRIBUTING.md](CONTRIBUTING.md) and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## <img src="assets/icons/book.svg" alt="" width="20" height="20"> License

[MIT](LICENSE). GitCode is an independent project and is not affiliated with OpenCode, Anthropic, or OpenAI.
