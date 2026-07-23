!['aghub banner'](/docs/assets/gh_banner.png)

!['aghub screenshot'](/docs/assets/app_screenshot.png)

**One hub for every AI coding agent.**

[![Version](https://img.shields.io/github/v/release/audichuang/aghub?include_prereleases&label=release)](https://github.com/audichuang/aghub/releases)
[![Downloads](https://img.shields.io/github/downloads/audichuang/aghub/total.svg)](https://github.com/audichuang/aghub/releases)
[![Homebrew](https://img.shields.io/badge/homebrew-tap-orange?logo=homebrew)](https://github.com/audichuang/homebrew-tap)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey.svg)](https://github.com/audichuang/aghub/releases)
[![Built with Tauri](https://img.shields.io/badge/built%20with-Tauri%202-orange.svg)](https://tauri.app/)
[![License](https://img.shields.io/github/license/audichuang/aghub)](https://github.com/audichuang/aghub/blob/main/LICENSE)

<a href="https://www.producthunt.com/products/aghub/reviews/new?utm_source=badge-product_review&utm_medium=badge&utm_source=badge-aghub" target="_blank"><img src="https://api.producthunt.com/widgets/embed-image/v1/product_review.svg?product_id=1193657&theme=light" alt="AGHub - The&#32;hub&#32;for&#32;every&#32;AI&#32;agent&#32;that&#32;isn&#39;t&#32;slop&#46; | Product Hunt" style="width: 250px; height: 54px;" width="250" height="54" /></a>

[中文版本](./README.CN.md)

aghub is a desktop app **and** CLI for managing **MCP servers, skills,
sub-agents, inference providers, and Claude Code plugins** across all
[25 supported agents](#supported-agents). Configure a capability once and deploy
it everywhere — aghub reads your real config files and changes only what you ask
it to.

## Installation

The fastest way to install is [Homebrew](https://brew.sh) on macOS / Linux.

### Desktop app

```bash
# macOS
brew install --cask audichuang/tap/aghub
```

On **Windows or Linux**, download a build below (or from the
[Releases](https://github.com/audichuang/aghub/releases) page):

| Platform               | Download                                                                                          |
| ---------------------- | ------------------------------------------------------------------------------------------------- |
| Windows (experimental) | [setup.exe](https://github.com/audichuang/aghub/releases/latest/download/aghub-windows-setup.exe) |
| macOS (Intel)          | [dmg](https://github.com/audichuang/aghub/releases/latest/download/aghub_mac_intel.dmg)           |
| macOS (Apple Silicon)  | [dmg](https://github.com/audichuang/aghub/releases/latest/download/aghub_mac_arm.dmg)             |
| Linux                  | [AppImage](https://github.com/audichuang/aghub/releases/latest/download/aghub-linux.AppImage)     |

### CLI (`aghub-cli`)

Run aghub headlessly — ideal for scripting and CI.

```bash
# macOS / Linux
brew install audichuang/tap/aghub-cli
aghub-cli --help
```

Prefer a raw binary? Download `aghub-cli-<target>` from
[Releases](https://github.com/audichuang/aghub/releases), or build from source
with `cargo build --release -p aghub-cli`.

### System Requirements

- Windows: Windows 10 and above
- macOS: macOS 12 (Monterey) and above
- Linux: Ubuntu 22.04+ / Debian 11+ / Fedora 34+ and other mainstream distributions

### macOS: "aghub is damaged and can't be opened"

The macOS builds are **not signed with an Apple Developer certificate**, so
Gatekeeper quarantines the app on first download and shows a "damaged" warning.
The app is fine — just clear the quarantine attribute after moving it to
`/Applications`:

```bash
xattr -cr /Applications/aghub.app
```

If that doesn't help, remove the attribute explicitly:

```bash
sudo xattr -dr com.apple.quarantine /Applications/aghub.app
```

Then open the app normally. (Adjust the path if you haven't moved it to
`/Applications` yet.)

---

## Features

**Unified MCP Management** — one config, deployed to every agent

- Configure once, deploy to any of the 25 supported agents
- Stdio, SSE, and StreamableHttp transports
- Enable or disable servers without removing them
- View and audit servers across all agents in one command

**Portable Skills** — author once, share across agents

- Import `.skill` packages or author skills with SKILL.md frontmatter
- Share skills across agents via the universal skills directory
- Skill installs are always symlink/junction to a shared `.agents/skills` master; no isolated-copy install mode
- SHA-256 content verification and source provenance tracking
- Browse and install from the skills.sh marketplace

**Flexible Scoping** — global, per-project, or merged

- View global, project, or merged config per agent
- Filter by a single agent, a comma-separated list, or all at once
- Full audit trail for every configured resource

**Inference Providers** — bring your own model to any agent

- **Start from a preset** — spin up popular providers from built-in presets, or configure a custom endpoint from
  scratch
- **Bring your own model** — point Claude, Codex, and OpenCode at any custom inference endpoint
- **Every API format** — Anthropic Messages, OpenAI Chat Completions, and OpenAI Responses
- **Keys stay safe** — API keys are stored in your OS-native keychain, never in plaintext config
- **Per-agent model selection** — choose the right provider and model for each agent independently

**Claude Code Plugins** — discover, install, and manage from a built-in marketplace

- **Install plugins from anywhere** — the official registry, third-party Git URLs, or a local path
- **Marketplace built in** — discover, browse, and install Claude Code plugins (v2) without leaving aghub
- **Full lifecycle management** — install, update, enable/disable, and remove plugins with one command
- **Global or project scope** — apply plugins everywhere or pin them to a single project

**Remote Deployment** (Desktop) — manage a remote machine over SSH

- Manage a remote machine's agent configs over SSH — the same MCP, skills, and provider tooling, applied to a VM or server
- Per-source Git credentials are forwarded securely from your local machine over the tunnel

**Command-Line Interface** (`aghub-cli`) — every capability, scriptable

- Every core capability, scriptable and headless — ideal for CI and automation
- `get` / `add` / `delete` / `enable` / `disable` MCP servers and skills across agents
- `transfer` resources between agents; `coverage` shows what's configured where
- `source` sync from Git, offline-by-default `check`, and `apply-update`
- `inference` providers + keys, `plugin` lifecycle, and `doctor` diagnostics

## Supported Agents

25 agents, one interface:

Claude Code · Cursor · Windsurf · GitHub Copilot · Gemini CLI · OpenAI Codex ·
OpenCode · Zed · Warp · Cline · RooCode · KiloCode · AugmentCode · Amp · Kiro ·
Trae · Factory · Kimi Code CLI · Mistral Le Chat · Pi Coding Agent ·
JetBrains AI · Hermes · Grok · Antigravity · OpenClaw

## Contributing

Contributions are welcome! To get started:

```bash
git clone https://github.com/audichuang/aghub.git
cd aghub
just desktop               # Desktop debug build
cargo build -p aghub-cli   # CLI build
just test                  # Run tests
just lint                  # Run clippy
```

Please ensure `just test` and `just lint` pass before submitting a pull request.

## License

This project is licensed under the [MIT License](LICENSE).

## Star History

<a href="https://www.star-history.com/#audichuang/aghub&type=date&legend=top-left">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=audichuang/aghub&type=date&theme=dark&legend=top-left" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=audichuang/aghub&type=date&legend=top-left" />
   <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=audichuang/aghub&type=date&legend=top-left" />
 </picture>
</a>
