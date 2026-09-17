<div align="center">

<img src="src-tauri/icons/128x128@2x.png" width="112" alt="Metrik 图标">

# Metrik

[English](README_en.md) · [下载](https://github.com/keros68/metrik/releases/latest) · [快速开始](#快速开始) · [使用说明](docs/guide.md) · [开发说明](docs/development.md) · [许可证](#许可证)

**在一个桌面组件里查看多个 AI 编程 Agent 的剩余额度、重置时间和本地 Token 用量。**

</div>

Metrik 是基于 Tauri 2 的桌面应用，支持 Windows、macOS 与 Ubuntu 24.04 x86_64。用量从本机日志解析，配额取自各 Agent 官方接口，无云端服务。

<p align="center">
  <img src="design/shot-glass.jpg" alt="Metrik 桌面小组件与配额胶囊条 · 透明档">
</p>

## 功能

- **配额卡片**：显示各 Agent 剩余额度、重置倒计时与消耗节奏预估，主数值取余量最低的窗口。
- **桌面形态**：Windows 提供桌面小组件，Ubuntu 提供紧凑卡片，两者均可收缩为横向 / 纵向配额胶囊条；macOS 提供菜单栏面板和原生 WidgetKit 桌面小组件。
- **统计与报表**：26 周热力图、周趋势、Agent 占比与按项目归集的会话明细，可导出 CSV。
- **多设备同步**：指定坚果云、OneDrive 或 Syncthing 等共享文件夹后，各设备近 30 天统计事件自动合并。
- **隐私**：数据库不写入提示词、回复正文、工具输出与凭据；更新检查是唯一主动发起的网络请求，可在设置中关闭。

## 快速开始

1. 打开 [Releases](https://github.com/keros68/metrik/releases/latest)，按系统下载安装包：Windows x64 选 `Metrik_*_x64-setup.exe`，macOS 选 `Metrik_*_universal.dmg`，Ubuntu 24.04 x86_64 选 `Metrik_*_amd64.deb` 或 `AppImage`。
2. 安装并运行。安装包尚未加入 Windows / Apple 商业代码签名，首次运行需按系统提示手动放行。
3. 启动后按安装痕迹检测本机 Agent，读取日志与配额。显示哪些 Agent 可在设置的「显示的 Agent」中调整，本机未检测到的 Agent 也可手动勾选。

各平台形态、数据说明与已知限制见[使用说明](docs/guide.md)。

## 支持的 Agent

| Agent | Token 数据来源 | 官方配额 |
| --- | --- | --- |
| ChatGPT / Codex | `~/.codex/sessions` | ✅ 每周 |
| Claude | `~/.claude/projects` | ✅ 5 小时、每周（状态栏钩子 / OAuth） |
| GLM / ZCode | `~/.zcode/cli/db/db.sqlite` | ✅ 5 小时、每周 |
| Kimi | `~/.kimi-code`、`~/.kimi` | ✅ 5 小时、每周、月度 |
| OpenCode | `~/.local/share/opencode/storage` | ❌ |
| Antigravity | IDE 语言服务 RPC | ✅ |
| WorkBuddy / CodeBuddy | `~/.codebuddy/projects`、`~/.workbuddy/projects` | ✅ 官方 Credits |
| Qoder | — | ✅ 官方 Credits |
| Grok Build | `~/.grok/sessions/**/updates.jsonl` | ✅ 周 Credits（CLI 日志快照） |
| Pi | `~/.pi/agent/sessions`、`~/.omp/agent/sessions` | ❌ |
| Qwen | pi 会话按百炼 Token Plan 路由归属 | ❌ |
| Hermes | `~/.hermes/state.db` | ❌ |

暂不支持 Gemini CLI；Cursor 待设计独立的凭据授权机制后再评估。Claude 配额的两种读取方式及 OAuth 条款风险见[使用说明](docs/guide.md#claude-配额的两种读取方式)。

## 从源码构建

依赖 Node.js 22+、Rust 1.88+。

```bash
npm install
npm run desktop:dev    # 桌面开发模式
npm run desktop:build  # 构建安装包
```

Ubuntu 构建依赖与测试命令见[开发说明](docs/development.md)。

## 许可证

[AGPL-3.0-or-later](LICENSE)，Copyright © 2026 keros68。分发修改版，或基于修改版对外提供网络服务时，需按 AGPL-3.0 开放对应源码。v0.10.0 及更早版本适用 MIT。

感谢 [LINUX DO 社区](https://linux.do/) 提供的交流氛围与开源推广支持。
