!['aghub banner'](/docs/assets/gh_banner_cn.png)

!['aghub screenshot'](/docs/assets/app_screenshot.png)

**你的AI智能体配置中心**

[![Version](https://img.shields.io/github/v/release/audichuang/aghub?include_prereleases&label=release)](https://github.com/audichuang/aghub/releases)
[![Downloads](https://img.shields.io/github/downloads/audichuang/aghub/total.svg)](https://github.com/audichuang/aghub/releases)
[![Homebrew](https://img.shields.io/badge/homebrew-tap-orange?logo=homebrew)](https://github.com/audichuang/homebrew-tap)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey.svg)](https://github.com/audichuang/aghub/releases)
[![Built with Tauri](https://img.shields.io/badge/built%20with-Tauri%202-orange.svg)](https://tauri.app/)
[![License](https://img.shields.io/github/license/audichuang/aghub)](https://github.com/audichuang/aghub/blob/main/LICENSE)

<a href="https://www.producthunt.com/products/aghub/reviews/new?utm_source=badge-product_review&utm_medium=badge&utm_source=badge-aghub" target="_blank"><img src="https://api.producthunt.com/widgets/embed-image/v1/product_review.svg?product_id=1193657&theme=light" alt="AGHub - The&#32;hub&#32;for&#32;every&#32;AI&#32;agent&#32;that&#32;isn&#39;t&#32;slop&#46; | Product Hunt" style="width: 250px; height: 54px;" width="250" height="54" /></a>

[English Version](./README.md)

---

## 安装

### macOS (Homebrew)

```bash
# 安装桌面端应用
brew install --cask audichuang/tap/aghub
```

### 下载

| 平台                  | 下载                                                                                              |
| --------------------- | ------------------------------------------------------------------------------------------------- |
| Windows (实验性)      | [setup.exe](https://github.com/audichuang/aghub/releases/latest/download/aghub-windows-setup.exe) |
| macOS (Intel)         | [dmg](https://github.com/audichuang/aghub/releases/latest/download/aghub_mac_intel.dmg)           |
| macOS (Apple Silicon) | [dmg](https://github.com/audichuang/aghub/releases/latest/download/aghub_mac_arm.dmg)             |
| Linux                 | [AppImage](https://github.com/audichuang/aghub/releases/latest/download/aghub-linux.AppImage)     |

或访问 [Releases](https://github.com/audichuang/aghub/releases) 查看所有可用下载。

### 命令行工具 (`aghub-cli`)

桌面端已内置全部功能，但 aghub 同样支持无界面运行：

```bash
# Homebrew (macOS / Linux)
brew install audichuang/tap/aghub-cli
```

或从 [Releases](https://github.com/audichuang/aghub/releases) 下载对应平台的
`aghub-cli` 二进制（文件名形如 `aghub-cli-<target>`），或从源码构建：

```bash
cargo build --release -p aghub-cli   # -> target/release/aghub-cli
aghub-cli --help
```

### 系统要求

- Windows: Windows 10 及以上
- macOS: macOS 12 (Monterey) 及以上
- Linux: Ubuntu 22.04+ / Debian 11+ / Fedora 34+ 及其他主流发行版

### macOS：提示「aghub 已损坏，无法打开」

macOS 版本**未使用 Apple 开发者证书签名**，因此首次下载时会被 Gatekeeper 加上隔离标记并弹出「已损坏」提示。App 本身没有问题——把它移动到 `/Applications` 后清除隔离属性即可：

```bash
xattr -cr /Applications/aghub.app
```

如果仍无法打开，再显式移除隔离属性：

```bash
sudo xattr -dr com.apple.quarantine /Applications/aghub.app
```

之后正常打开即可。（如果还没移动到 `/Applications`，请把路径换成实际位置。）

## 功能

**统一 MCP 管理**

- 一次配置，部署到全部 25 个支持的助手
- 支持本地 Stdio 和远程（SSE 和 StreamableHttp）连线方式
- 无需删除即可启用或禁用服务器
- 单条命令查看和审计所有助手的服务器

**便携技能**

- 导入 `.skill` 包或使用 SKILL.md 前言编写技能
- 通过通用技能目录跨助手共享技能
- 技能安装一律以符号链接／junction 指向共享的 `.agents/skills` 主目录，不做隔离复制
- SHA-256 内容验证与来源追踪
- 浏览并安装 skills.sh 市场中的技能

**灵活的作用域**

- 按助手查看全局、项目或合并配置
- 按单个助手、逗号分隔列表筛选，或一次列出全部
- 每个配置资源的完整审计轨迹

**供应商管理**

- 从预设开始 — 通过内置预设快速创建常用供应商，或从头配置自定义端点
- 使用你自己的模型 — 让 Claude、Codex 和 OpenCode 指向任何自定义推理端点
- 支持所有 API 格式 — Anthropic Messages、OpenAI Chat Completions 以及 OpenAI Responses
- 密钥安全无忧 — API 密钥存储在操作系统原生的密钥环中，绝不以明文写入配置文件
- 逐代理模型选择 — 为每个代理独立挑选最合适的供应商与模型

**Claude Code 插件**

- 随处安装插件 — 可来自官方注册表、第三方 Git URL 或本地路径
- 内置市场 — 无需离开 aghub，即可发现、浏览并安装 Claude Code 插件（v2）
- 完整生命周期管理 — 一条命令完成插件的安装、更新、启用／禁用与移除
- 全局或项目范围 — 应用到所有项目，或仅限定于单个项目

**远程部署**（桌面端）

- 通过 SSH 管理远程机器的助手配置 — 把同一套 MCP、技能与供应商工具应用到 VM 或服务器
- 逐来源的 Git 凭据经隧道从本地机器安全转发

**命令行工具**（`aghub-cli`）

- 所有核心功能均可脚本化、无界面运行 — 适合 CI 与自动化
- `get` / `add` / `delete` / `enable` / `disable` 跨助手管理 MCP 服务器与技能
- `transfer` 在助手之间迁移资源；`coverage` 查看各处已配置的内容
- `source` 从 Git 同步、默认离线的 `check`，以及 `apply-update`
- `inference` 供应商与密钥、`plugin` 生命周期，以及 `doctor` 诊断

## 贡献

欢迎贡献！开始方式：

```bash
git clone https://github.com/audichuang/aghub.git
cd aghub
just desktop               # 桌面端调试构建
cargo build -p aghub-cli   # CLI 构建
just test                  # 运行测试
just lint                  # 运行 clippy
```

提交 Pull Request 前，请确保 `just test` 和 `just lint` 通过。

## 许可证

本项目基于 [MIT License](LICENSE) 协议进行开源。

## Star History

<a href="https://www.star-history.com/#audichuang/aghub&type=date&legend=top-left">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=audichuang/aghub&type=date&theme=dark&legend=top-left" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=audichuang/aghub&type=date&legend=top-left" />
   <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=audichuang/aghub&type=date&legend=top-left" />
 </picture>
</a>
