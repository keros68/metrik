<div align="center">

<img src="src-tauri/icons/128x128@2x.png" width="112" alt="Metrik icon">

# Metrik

[简体中文](README.md) · [Download](https://github.com/keros68/metrik/releases/latest) · [Get started](#get-started) · [User guide (Chinese)](docs/guide.md) · [Development (Chinese)](docs/development.md) · [License](#license)

**See remaining quota, reset times, and local token usage for multiple AI coding agents in one desktop widget.**

</div>

Metrik is a Tauri 2 desktop app for Windows, macOS, and Ubuntu 24.04 x86_64. Usage is parsed from local logs, quota comes from each agent's official endpoint, and there is no cloud service.

<p align="center">
  <img src="design/shot-glass.jpg" alt="Metrik desktop widget and quota strip, clear glass">
</p>

## Features

- **Quota cards**: Remaining quota, reset countdown, and burn-rate estimate for each agent. The headline value uses the window with the least quota left.
- **Desktop forms**: A desktop widget on Windows and a compact card on Ubuntu, both collapsible into a horizontal or vertical quota strip. On macOS, a menu bar panel and a native WidgetKit desktop widget.
- **Statistics and reports**: 26-week heatmap, weekly trends, agent share, and per-project session details, with CSV export.
- **Multi-device sync**: After you choose a shared folder (Jianguoyun, OneDrive, Syncthing, and similar), statistics events from the last 30 days on each device are merged automatically.
- **Privacy**: Prompts, responses, tool output, and credentials are never written to the database. The update check is the only network request the app makes on its own and can be turned off in settings.

## Get started

1. Open [Releases](https://github.com/keros68/metrik/releases/latest) and download the installer for your system: `Metrik_*_x64-setup.exe` for Windows x64, `Metrik_*_universal.dmg` for macOS, or `Metrik_*_amd64.deb` / `AppImage` for Ubuntu 24.04 x86_64.
2. Install and run it. The installers are not yet signed with commercial Windows / Apple code signing certificates, so allow the app manually on first launch.
3. On launch, Metrik detects installed agents and reads their logs and quota. Choose which agents to show under "显示的 Agent" (Displayed agents) in settings; agents not detected on this machine can still be enabled manually.

The app interface is in Chinese. See the [user guide](docs/guide.md) for platform details, data definitions, and known limitations.

## Supported agents

| Agent | Token data source | Official quota |
| --- | --- | --- |
| ChatGPT / Codex | `~/.codex/sessions` | ✅ Weekly |
| Claude | `~/.claude/projects` | ✅ 5-hour, weekly (status line hook / OAuth) |
| GLM / ZCode | `~/.zcode/cli/db/db.sqlite` | ✅ 5-hour, weekly |
| Kimi | `~/.kimi-code`, `~/.kimi` | ✅ 5-hour, weekly, monthly |
| OpenCode | `~/.local/share/opencode/storage` | ❌ |
| Antigravity | IDE language server RPC | ✅ |
| WorkBuddy / CodeBuddy | `~/.codebuddy/projects`, `~/.workbuddy/projects` | ✅ Official credits |
| Qoder | — | ✅ Official credits |
| Grok Build | `~/.grok/sessions/**/updates.jsonl` | ✅ Weekly credits (CLI log snapshot) |
| Pi | `~/.pi/agent/sessions`, `~/.omp/agent/sessions` | ❌ |
| Qwen | pi sessions attributed by Bailian Token Plan route | ❌ |
| Hermes | `~/.hermes/state.db` | ❌ |

Gemini CLI is not supported yet. Cursor will be evaluated once a separate credential authorization design is in place. See the [user guide](docs/guide.md#claude-配额的两种读取方式) for the two ways Claude quota is read and the terms-of-service risk of the OAuth option.

## Build from source

Requires Node.js 22+ and Rust 1.88+.

```bash
npm install
npm run desktop:dev    # desktop dev mode
npm run desktop:build  # build installers
```

See the [development notes](docs/development.md) for Ubuntu build dependencies and test commands.

## License

[AGPL-3.0-or-later](LICENSE), Copyright © 2026 keros68. If you distribute a modified version, or provide a network service based on one, you must release the corresponding source under AGPL-3.0. v0.10.0 and earlier are under MIT.

Thanks to the [LINUX DO community](https://linux.do/) for discussion and open-source promotion support.
