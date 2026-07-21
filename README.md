# Metrik

本地优先的 AI Agent 用量统计桌面应用。读取本机 Agent 留下的日志，把**官方配额**、**本地解析的 Token**、**估算成本**三类事实分开呈现——估算是估算，不冒充账单。

Windows 上是 320 × 320 的桌面小组件，可再折叠成一根横向或竖立的**配额胶囊条**；macOS 上是菜单栏面板。两者都能一键展开为完整统计视图。

[![Download](https://img.shields.io/github/v/release/keros68/metrik?label=下载&color=success)](https://github.com/keros68/metrik/releases/latest)
[![License: AGPL v3](https://img.shields.io/badge/License-AGPL%20v3-blue.svg)](LICENSE)
[![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB.svg)](https://tauri.app/)
[![Platform](https://img.shields.io/badge/Windows%20%7C%20macOS-0078D6.svg)](#验收边界)

[下载最新版](https://github.com/keros68/metrik/releases/latest)：Windows `.exe` / macOS `.dmg`（通用包）。均未签名，首次运行需手动放行。

<p align="center">
  <img src="design/shot-widget.png" width="320" alt="Metrik 桌面小组件">
</p>

<p align="center">
  <img src="design/shot-strip-vertical.png" height="240" alt="配额胶囊条 · 竖条">
  &nbsp;&nbsp;&nbsp;
  <img src="design/shot-strip-horizontal.png" width="440" alt="配额胶囊条 · 横条">
</p>
<p align="center"><sub>小组件可折叠成配额胶囊条（Windows）：横条或竖立长条，只占一线屏幕。</sub></p>

![完整视图 · 概览](design/shot-overview.png)

> 截图为浏览器演示数据，不是真实用量。

<details>
<summary>更多截图（报告 / 用量 / 设置）</summary>

![报告](design/shot-reports.png)
![用量](design/shot-usage.png)
![设置](design/shot-settings.png)

</details>

## 支持的 Agent

| Agent | Token 来源 | 官方配额 |
| --- | --- | --- |
| ChatGPT / Codex | `~/.codex/sessions` 的会话日志 | ✅ 本机 `codex app-server` |
| Claude | `~/.claude/projects` 的会话日志 | ✅ statusLine 钩子或 OAuth 直连（见下） |
| ZCode / 智谱 GLM | `~/.zcode/cli/db/db.sqlite` 的 `model_usage` 表 | — |
| OpenCode | `~/.local/share/opencode/storage` | — |
| Kimi | `~/.kimi-code` 与 `~/.kimi` 的 `wire.jsonl` | — |
| Antigravity | 本机 language server 实时 RPC（IDE 需运行） | ✅ RPC 官方配额 |

Gemini CLI 明确不在支持范围。Cursor 依赖云端 API 和本地凭据提取，要先设计出显式的凭据授权机制才会考虑。

## 功能

- **配额卡片**：每个窗口三行——窗口名、进度条、已用百分比与重置倒计时，另附消耗节奏预测。数据陈旧或窗口已重置时会标注出来。
- **配额胶囊条**（Windows）：小组件一键折叠成横向或竖立的小条，每个 Agent 一格（图标 + 剩余百分比 + 品牌色进度条）；显示哪些 Agent 及顺序可在设置里自选，材质跟随小组件的玻璃开关。
- **报告**：26 周热力图 / 周趋势折线 / Agent 构成环形。
- **用量**：按天分组的会话明细，支持筛选、CSV 导出、复制会话 ID（方便 resume）。
- **成本估算**：静态价目表按 Token 分量计价，没有价目的模型归入"未计价"。
- **多设备同步**：指向一个共享文件夹（坚果云 / OneDrive / Syncthing 均可），各设备导出近 30 天的统计事件并自动合并。
- **更新检查**：设置页手动触发，不后台轮询——这是本应用唯一会主动发出的网络请求。更新包经 minisign 校验，签名不符拒绝安装。
- 托盘常驻、可选开机启动、单实例运行。

## 数据口径

首页的数字是**处理量**：`未缓存输入 + 缓存读取 + 缓存写入 + 输出`。推理 token 是输出的子项，不重复叠加。**它不是账单金额。**

三类事实始终分开：**官方配额**（Agent 官方推送的窗口百分比与重置时间）、**本地 Token**（从本机日志逐事件解析、去重后的处理量）、**估算成本**（按公开 API 价目折算，不是你的账单）。

解析遇到坏行或读取异常时，对应来源标为"部分覆盖"。

## 设计原则

- **缺失如实标注。** 读不到的数据显示为"不可用"、"未计价"或 `--`。
- **只存元数据。** 数据库保存时间、Agent、模型、会话标识和源文件位置，Prompt、回复正文、工具输出、凭据、原始文件都不入库。
- **同步走你自己的网盘。** 多设备同步通过你指定的共享文件夹完成，Metrik 不提供云端服务。
- **额度默认零凭据。** Claude 配额通过 statusLine 获取；OAuth 直连为可选功能，默认关闭。

## Claude 额度的两种来源

**statusLine 钩子**（默认）：Claude Code 本身会把官方额度推给状态栏脚本，钩子只提取额度数字写成本地文件，零网络请求、零凭据。已有自定义 statusLine 时会自动串联，卸载时原样恢复。局限是只有在终端里渲染出状态栏的交互式会话才会刷新，主要在 IDE 或网页里用 Claude 的话额度会停更。

**OAuth 直连**（可选，默认关闭）：读取 Claude Code 已保存的登录凭据，直接查询官方额度接口，覆盖网页版消耗。

> ⚠️ **条款风险**：Anthropic 2026 年 2 月更新的消费者条款禁止在第三方工具中使用 Claude 订阅的 OAuth 凭据。目前公开的封禁集中在借订阅做推理的工具，未见只读用量查询被封号的案例，但按条款字面本功能同样属于违规范围。**不愿承担风险请保持关闭。**

## 开发

要求 Node.js 22+、Rust 1.88+。

```bash
npm install
npm run desktop:dev    # 桌面开发模式（读取真实本机日志）
npm run dev            # 仅浏览器预览（演示数据，显式标注）
npm run desktop:build  # 构建安装包

npm run build                                                 # 验证
cd src-tauri && cargo test && cargo clippy -- -D warnings && cargo fmt --check
cargo test live_snapshot_smoke_test -- --ignored --nocapture  # 读真实本机日志的烟测
```

## 已知限制

- 安装包未做代码签名，首次运行需要在系统提示里手动放行。Release 页附 SHA256 可校验文件完整性。
- Windows 上没装 WebView2 的话，安装器需要联网获取运行时。
- 只提供 Windows 和 macOS 安装包，Linux 需自行构建。
- Kimi 与 Antigravity 的解析未经作者实机核对（本机没装这两个），数字有偏差欢迎提 issue。Antigravity 另外要求 IDE 正在运行才有数据。
- 首次索引大日志会占一段 CPU 和磁盘。期间界面照常可用、进度可见，未覆盖完整历史的数字会标注出来。

架构与去重逻辑见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)，视觉对照见 [design-qa.md](design-qa.md)，验收证据见 [ACCEPTANCE.md](ACCEPTANCE.md)。

## License

[AGPL-3.0-or-later](LICENSE)，Copyright © 2026 keros68。

自用、修改、fork 都没有限制。要求只有一条：**分发修改版，或者拿它对外提供网络服务，就得同样以 AGPL-3.0 开源**，包括你改过的部分。

v0.10.0 及更早的版本按 MIT 发布，那些版本仍然适用 MIT。
