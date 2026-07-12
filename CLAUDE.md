# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概述

Metrik 是一个本地优先的 AI Agent 用量统计桌面应用（Tauri 2 + React 19 + Vite），当前支持 ChatGPT/Codex 与 Claude Code 两个 Agent。默认形态是 320×320 的桌面小插件（compact），可一键展开为完整统计视图（expanded）。当前仅在 Windows 10/11 x64 实机验收；macOS/Linux 共用代码但未验收。

## 常用命令

```bash
npm install            # 安装前端依赖
npm run dev            # 仅浏览器预览（演示数据，不读取本机 Agent 日志）
npm run desktop:dev    # Tauri 桌面开发模式（读取真实本机日志）
npm run build          # 前端生产构建
npm run desktop:build  # 构建桌面安装包
```

Rust 测试（在 `src-tauri/` 目录下）：

```bash
cargo test                                             # 全部测试（约 48 项通过，2 项忽略）
cargo test <test_name>                                 # 运行单个测试
cargo test live_snapshot_smoke_test -- --ignored --nocapture  # 读真实本机日志的烟测，默认忽略
cargo clippy -- -D warnings                            # 发布前严格 Clippy
cargo fmt --check                                      # 格式检查
```

要求 Node.js 22+、Rust 1.88+。

## 架构

数据流（详见 `docs/ARCHITECTURE.md`）：

```
Codex/Claude JSONL 日志 → adapter → 规范化事件 → SQLite 账本 → 周期查询 → UI
Codex app-server → 官方配额快照 ──────────────────────────────────┘
```

- **前端**（`src/`）：几乎所有 UI 在 `src/App.jsx`（compact 小插件 + expanded 完整视图两种形态，由 URL 参数 `?view=expanded` 与运行时状态切换）。`src/usageClient.js` 封装对 Tauri 的调用，浏览器环境下自动降级为显式标注的演示数据；`src/windowClient.js` 管理窗口尺寸/置顶/透明；`src/styles.css` 为全部样式（含标准与透明两种材质）。
- **后端**（`src-tauri/src/`）：UI 只调用一个异步命令 `usage_snapshot(period)`（另有用户可达的 `rebuild_local_ledger`）。`engine.rs` 是扫描/对账核心（单一扫描锁 + `spawn_blocking`）；`adapters/`（codex.rs、claude.rs）实现 `AgentAdapter` trait 解析各 Agent 的 JSONL；`storage.rs` + `schema.rs` 管 SQLite 账本（WAL、迁移、恢复）；`app_server.rs` 取 Codex 官方配额；`domain.rs` 是共享类型。

### 事件身份与去重（改动 adapter 前必读）

- adapter 只负责解释来源；账本（storage）拥有身份、事务与去重。
- Codex 暴露累计计数器：记首个快照后只记正增量；Claude Code 同一 message 会渐进更新，按 provider `message.id` 跨会话合并、字段取分量最大值。
- 源文件路径是观察记录不是事件身份（`event_observation` 表），归档移动不会重复计数。
- 新增 Agent 必须实现 `AgentAdapter` trait，并补齐身份、部分输入、时间边界、缓存 token 语义的测试夹具后才能启用。

### Token 口径

`processed = input_uncached + cache_read + cache_write + output`；`reasoning_output` 是 output 子项，不重复叠加。这是处理量，不是账单金额。

## 硬性产品约束（来自 AGENTS.md，不可违反）

- 官方配额、本地解析用量、估算成本是三类不同事实，永远分开呈现；估算不得冒充官方账单。
- 桌面读取失败时显示"不可用"，**绝不**用演示数字或零值静默顶替；缺失/陈旧数据必须显式标注。浏览器预览必须显式标明演示模式。
- 隐私边界：数据库只存统计字段（时间、Agent、模型、会话标识、源文件定位），不存对话正文、Prompt、工具输出、凭据、原始文件内容。
- 本地优先；未来设备同步必须 opt-in、端到端加密，且不上传原始对话或凭据。
- Gemini CLI 明确不在支持范围。
- 视觉方向：Apple 式克制（字体、材质深度、留白、动效），参考图 `design/reference-option-2.png`；不模仿 Apple 品牌。
- 紧凑态不加载完整图表；置顶是用户主动选项，不强制。

## 其他

- 视觉对照记录在 `design-qa.md`；Windows 验收证据在 `ACCEPTANCE.md`。
- 坏行/读取异常将来源降级为 `partial`（部分覆盖），不伪装精确；`scan_source.last_error` 只存跳过行数，不存内容。
- `PARSER_VERSION`（当前 3）变更会强制保留历史的重建。
