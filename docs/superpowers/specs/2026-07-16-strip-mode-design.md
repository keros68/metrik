# 胶囊条（strip）第三形态设计

> 后续 macOS 设计决定：strip 仅保留在 Windows。macOS 不再把菜单栏面板压成悬浮胶囊，而是采用 Metrik 自己的状态项语法：复用现有“小组件显示的 Agent”设置，为每个已选 Agent 显示一个单色品牌图标和对应官方额度数字，选择立即生效且至少保留一个；点击任一个都展开紧凑面板，缺失显示 `--`，陈旧显示 `~`。只借鉴 macOS 菜单栏工具的零占地交互范式，不复制 CodexBar 的布局、菜单或多账户信息密度。下文涉及 macOS strip 的描述已被此决定取代。

日期：2026-07-16
状态：已由用户批准

## 背景与目标

现有两种形态：320×320 小插件（compact）与完整统计视图（expanded）。compact 作为"桌面摆件"仍占不小面积；参考竞品（CodexBar 等菜单栏工具）的高密度、零占地展示，为 Metrik 增加第三形态——**胶囊条（strip）**：一条置顶小横条，每个 agent 一格，显示官方配额剩余百分比。不模仿菜单栏形态，是 Metrik 桌面小组件谱系的再折叠一档。

三态关系：**strip ⇄ compact → expanded**（胶囊是 compact 的折叠态；expanded 仍只从 compact 进入）。

## 形态与平台

- 新增 `mode === "strip"`，与 compact/expanded 同属主窗口变形，复用 `applyWindowMode` 机制（`src/windowClient.js`）。
- Windows/Linux 使用普通桌面浮窗；macOS 在菜单栏 NSPanel 内提供同样的卡片/胶囊入口。Mac 变形后必须重新锚定菜单栏图标，不启用拖动、置顶或位置记忆。
- 窗口参数：高约 36 逻辑像素；宽 = 格数 × ~64px + 两端留白，agent 格数变化时自动 `setSize`。无边框、不可缩放、`skipTaskbar`、置顶跟随现有"固定"开关。
- 外观走现有玻璃管线（`setWindowGlass`：原生可用走原生，否则 CSS 近实心玻璃），整体一枚大圆角胶囊。

## 内容（严守三类事实分离）

- 数据源：现有 `usage_snapshot` 返回的 `agentQuotas`（`AgentQuotaView`，见 `src-tauri/src/domain.rs`）。**只显示存在可用官方配额窗口的 agent**；本地解析用量不上条，两类事实不混排。
- 每格 = agent logo（复用前端 `AGENT_META`）+ 最紧张窗口（5h/周中已用百分比更高者）的剩余百分比数字 + 底部 6px 微型进度条。条色用 Agent 品牌色；>85% used 叠警示样式（沿用现有分级），不整条变红。
- 陈旧数据：沿用 0.55 alpha + tooltip 注明 "Updated Xm ago"。
- 全部 agent 均无配额时：显示"配额不可用"占位，**绝不显示零值或演示数字**；占位态仍可通过末端按钮回 compact。
- 悬停详情：第一版用原生 `title` tooltip，列出该 agent 全部窗口的剩余百分比与重置倒计时。原因：窗口仅 36px 高，自绘弹层会被窗口裁剪；hover 展开式详情面板留作后续增强。

## 切换与位置

- compact 卡片头部新增"折叠成条"按钮 → 进入 strip。
- strip 末端一个小按钮 → 回 compact。**不依赖双击**（Tauri 无边框窗口的 drag-region 会吞掉双击事件，不可靠）。
- 条身整体为拖动区（`data-tauri-drag-region`）。
- 位置独立记忆：新 localStorage key `metrik:stripPosition`，与 compact 的 `metrik:widgetPosition` 互不覆盖；恢复时同样校验坐标落在某块显示器可见区内。
- 启动恢复上次形态（compact 或 strip；expanded 不恢复），记忆键如 `metrik:viewMode`。
- 上缘挂靠（`startEdgeDock`）保持 compact 专属；strip 不参与挂靠。

## 刷新与实现范围

- 刷新沿用 compact 档的 300s 轮询，不新增频率档位。
- 改动集中在前端：`src/App.jsx`（新增 StripView 组件与形态状态）、`src/windowClient.js`（`WINDOW_SIZES` 增加 strip、位置记忆扩展、形态恢复）、`src/styles.css`。后端预计零改动。
- 紧凑态不加载完整图表的约束同样适用于 strip（它只渲染百分比与微型进度条）。

## 验收（Windows 实机）

1. 三态切换：compact →（折叠按钮）→ strip →（末端按钮）→ compact →（展开按钮）→ expanded → 回 compact。
2. strip 与 compact 位置各自记忆、互不覆盖；重启恢复上次形态与位置。
3. 置顶开关对 strip 生效。
4. 只显示有官方配额的 agent；无配额时显示"配额不可用"占位。
5. 陈旧数据降透明度并在 tooltip 标注更新时间。
6. 宽度随 agent 格数自适应。
