# Metrik

Metrik 是一个本地优先的 AI Agent 用量统计桌面应用。首版支持 ChatGPT / Codex Agent 和 Claude Code，明确区分官方配额、本地 Token 记录和估算值。

![Metrik desktop widget](design/metrik-compact-standard-final.png)

## 现在已经具备

- ChatGPT / Codex Agent 今日、7 天和 30 天 Token 统计
- Claude Code 今日、7 天和 30 天 Token 统计
- 默认 320 × 320 桌面小插件，一键展开完整统计
- 标准与透明两种材质；透明模式使用系统级毛玻璃（Windows Acrylic / macOS Vibrancy），偏好保存在本机，Windows 毛玻璃已实机验证
- ChatGPT 与 Claude Code 使用各自官方应用图标，仅用于识别对应服务
- 可选置顶、展开后收起回原位；前台紧凑态每 5 分钟、完整态每 60 秒刷新，重新获得焦点时立即刷新
- 系统托盘常驻：不占任务栏（Windows）/ Dock（macOS），关闭按钮收进托盘，左键图标切换显示，右键菜单退出；Windows 已实测，macOS 菜单栏行为待实机验收
- ChatGPT / Codex 主短窗与次级长窗官方配额、采集时间、陈旧状态及重置时间；桌面小插件只显示短窗，完整视图显示两者
- Agent 筛选、趋势悬停和数据来源说明
- Windows 已构建和实测；macOS、Linux 共用 Tauri/Rust 代码基础，仍需各自机器验收
- 本地 SQLite 事件账本，重复扫描不会重复入账
- 坏行或读取异常会把对应来源降级为“部分覆盖”，不会继续伪装成精确结果
- 数据库使用系统本机数据目录，并从旧的漫游目录安全复制；旧库不删除、本机新库不覆盖
- 单实例运行；重复启动只会唤回已有窗口，不会再开一套扫描进程
- 旧版或不兼容的派生账本会做 schema 检查并从本机 Agent 日志安全重建，不要求用户手删数据库
- 数据来源抽屉提供两步确认的“重建本地账本”；只清理 Metrik 的派生统计表，不改写 Agent 原始日志或无关数据表
- 即使旧库迁移和恢复库创建都失败，应用也会退到唯一的临时账本继续启动，不因存储目录异常直接闪退
- 浏览器预览显式使用演示数据；桌面读取失败时显示不可用，不用演示数字冒充真实值
- 紧凑态不加载完整图表，扫描在后台线程执行，不阻塞窗口操作

Gemini CLI 不在支持范围内。

## 数据口径

Metrik 不把所有数字混成一个“用量”。

| 类型 | 来源 | 处理方式 |
| --- | --- | --- |
| ChatGPT / Codex 配额 | 本机 `codex app-server` | 官方滚动窗口；失败时回退到日志内的官方快照 |
| ChatGPT / Codex Token | `~/.codex/sessions` 与 `archived_sessions` | 累计快照转正增量；相同会话跨路径去重 |
| Claude Code Token | `~/.claude/projects` | 按 provider `message.id` 跨会话合并，字段取最大值；`requestId` 与模型只做冲突检测，冲突消息会拒绝并标记“部分覆盖” |
| Claude Code 配额 | 无稳定本地来源 | 显示不可用，不根据 Token 猜测 |

首页显示的是处理总量：未缓存输入 + 缓存读取 + 缓存写入 + 输出。缓存读取会计入处理量，但缓存/推理子项不会再次叠加。它不是账单金额。

## 隐私

Metrik 会在本机顺序扫描 JSONL 日志，但只反序列化并保存统计所需字段。数据库不保存：

- 对话正文和 Prompt
- 工具输出
- 登录凭据和 API Key
- 原始文件内容

SQLite 会保存用量时间、Agent、模型、会话标识和本机源文件定位信息；当前没有应用层静态加密。这些数据不会上传，首版也没有远程同步服务。

## 当前验收边界

- 当前安装包仅在 Windows 10/11 x64 实机构建和验收。macOS、Linux 与 Windows ARM64 有共同代码基础，但还没有对应产物和实机结论。
- 跨设备同步尚未实现；每台设备现在维护自己的本地账本。
- 安装包尚未数字签名，Windows 可能提示“未知发布者”。目标机未安装 WebView2 时，默认安装器需要联网获取运行时。
- Token 统计来自本地 Agent 日志，不是官方账单。若来源页显示“部分覆盖”，当前总量只包含成功解析的记录，可能不完整。
- 运行内存主要由系统 WebView2 决定。未变化文件会跳过，但持续增长的大型日志当前仍需整文件重扫；首次索引和活跃会话期间可能出现明显 CPU、磁盘与内存占用。

## 本地运行

要求 Node.js 22+、Rust 1.88+，以及对应系统的 Tauri 依赖。

```bash
npm install
npm run desktop:dev
```

只预览界面：

```bash
npm run dev
```

浏览器预览不会读取 Agent 日志，会明确显示演示模式。

## 验证

```bash
npm run build
cd src-tauri
cargo test
```

当前自动化结果为 48 项通过、2 项真实环境烟测默认忽略；发布前另运行严格 Clippy、格式检查和前端生产构建。

读取当前机器真实日志的烟测默认被忽略，需要手动运行：

```bash
cargo test live_snapshot_smoke_test -- --ignored --nocapture
```

视觉对照记录见 [design-qa.md](design-qa.md)，Windows x64 安装包、对账、资源与原生交互证据见 [ACCEPTANCE.md](ACCEPTANCE.md)。

## 路线

1. 当前：320 × 320 透明桌面小插件、准确的 ChatGPT / Codex 与 Claude Code 本地统计和可信度说明
2. 下一步：真正的追加游标、macOS/Linux 构建验收（含托盘/菜单栏行为），再评估开机启动
3. 再下一步：端到端加密的可选设备同步，不上传原始对话或凭据
4. 后续 Agent：OpenCode、Cursor 等按适配器成熟度逐个接入

详细边界见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。
