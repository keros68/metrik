# Metrik Design QA

- Primary source visual truth: `F:\文档\xwechat_files\wxid_76st6qr3yybp22_7a50\temp\InputTemp\7169a345-f017-4d90-9db8-e5c5f0af3d8a.png`
- Supplementary references: `design/ref-cc-bar-overview.png`, `design/ref-codexbar-popover.png`, `design/reference-option-2.png`
- Compact standard implementation: `design/metrik-compact-standard-final.png`
- Compact transparent implementation: `design/metrik-compact-transparent-final.png`
- Expanded implementation: `design/metrik-expanded-final-v3.png`
- Full-view comparison evidence: `design/comparison-widget-v3.png`, `design/comparison-expanded-v3.png`
- Focused comparison evidence: `design/comparison-widget-period-v3.png`
- Browser-rendered surface: `http://127.0.0.1:4173/`
- Browser viewport: 320 × 320 for compact evidence; 1280 × 720 for expanded evidence
- Native state checked: Windows Tauri 320 × 320, standard and transparent materials, real local data
- Browser state checked: today/all Agents/demo data, seven-day state, Agent-filtered state, compact and expanded views

## Findings

No actionable P0, P1, or P2 findings remain.

- [P3] The compact product intentionally does not reproduce cc-bar's dense colored popover or CodexBar's long provider menu.
  - The references are used for material, official provider identification, thin quota bars, and scan-first hierarchy. Detailed charts, source diagnostics, and secondary quota stay in the expanded view.
- [P3] Browser screenshots can only demonstrate the two material opacities against the preview stage.
  - The native Windows check confirmed that the desktop wallpaper is visibly transmitted through the transparent Tauri window while the text cards stay readable.
- [P3] A third-party Claude review retained an objection that the CSS outer shadow has no transparent client-area margin inside the exact 320 × 320 footprint.
  - This is accepted for the current compact target: increasing the window to 360 × 360 would enlarge the click-intercepting footprint by 26.6%. The native window keeps Tauri's system shadow plus a visible inset edge; compactness wins over a browser-only ambient shadow.
- [P3] Official service marks need a final brand-permission review before mass commercial distribution.
  - The current use is the narrow identification case requested by the user, uses official downloadable assets, keeps source pixels unchanged, records provenance, and explicitly disclaims endorsement.

## Required fidelity surfaces

- Fonts and typography: passed. Geist carries compact UI text; Newsreader and Instrument Serif retain the editorial number treatment. Large values use lining/tabular numerals and remain inside the 320 px frame at million and billion scale.
- Spacing and layout rhythm: passed. In the actual 320px viewport, the period selector is `top 44 / bottom 78`, the summary begins at `82`, and measured overlap is `0`. The summary, Agent card, and footer each have 4px separation; no compact region scrolls, clips, or overlaps. Footer bottom is at 312px inside the 320px shell, leaving 8px total bottom clearance.
- Colors and visual tokens: passed. Standard compact and expanded modes are fully opaque. Transparent compact uses a 0.72 shell, protected title/metric surfaces, darker `#35373b` small text, and reduced-transparency/forced-colors fallbacks. Warm neutral, graphite, cobalt, Claude coral, and green provenance remain distinct.
- Image quality and asset fidelity: passed. ChatGPT uses OpenAI's official published app icon and Claude Code uses Anthropic's official Claude app icon from its App Store listing. Source pixels and aspect ratio are unchanged; CSS only clips their display to the rounded icon frame. No placeholder, emoji, CSS drawing, inline SVG, or approximated provider mark remains.
- Copy and content: passed. The visible provider is named ChatGPT while the compact quota explicitly says `ChatGPT · Codex 短窗`; expanded sources use `ChatGPT / Codex`. Demo provenance appears inside both the quota card and footer. Claude Code is unchanged, and “桌面小插件” replaces the earlier imprecise “挂件” wording.
- Icons and controls: passed. Window controls remain one maintained Phosphor family; the transparent toggle has distinct on/off labels, `aria-pressed`, focus treatment, and a persisted state.
- Accessibility and resilience: passed for the current desktop target. The period selector is an explicitly labelled group; controls are semantic buttons; selected states are exposed; focus remains visible; reduced motion/transparency and forced colors are supported; and the actual 320px compact frame has no horizontal or vertical overflow. The 30px window controls are appropriate for a pointer-first desktop widget, not a touch target.

## Comparison history

### Pass 1 — blocked

- [P1] The supplied screenshot showed the period selector touching the metric-label band, visually merging “今日 / 7 天 / 30 天” with “总用量 / Codex 短窗”.
  - Fix: changed compact geometry from 380 × 440 to a 320 × 320 desktop-widget composition, reduced the title bar to 42 px, moved the 34 px selector directly below it, and gave the summary its own following row.
  - Evidence: `design/comparison-widget-period-v3.png`.
- [P2] The first 320 px implementation allocated 76 px to a summary whose children had an 86 px minimum height, producing a 6 px visual collision with the Agent card.
  - Fix: allocated 86 px to the summary, reduced the Agent card to 106 px and the footer to 30 px, and added `min-height: 0` to the summary grid items.
  - Post-fix evidence: browser measurements show summary `clientHeight = scrollHeight = 86`, no region overflow, and 4 px gaps from selector → summary → Agent card → footer. Final raster: `design/metrik-compact-standard-final.png`.

### Pass 2 — passed locally

- Opened the user screenshot, cc-bar, CodexBar, standard compact, and transparent compact together in `design/comparison-widget-v3.png`.
- Opened the selected expanded direction and current expanded implementation together in `design/comparison-expanded-v3.png`.
- Rechecked the focused selector/metric region in `design/comparison-widget-period-v3.png`.
- No remaining P0/P1/P2 mismatch was found.

### Pass 3 — Claude R1 blocked, then fixed

- Claude reported three P2 items: harden the zero-spare summary row, protect transparent small-text contrast, and make compact quota provenance as explicit as expanded mode.
- Fixes: explicit metric/quota containment; transparent alpha/text/fallback revisions; compact `ChatGPT · Codex` wording; period `role="group"`; attribution wording.
- Independent verification: all compact elements had equal client/scroll dimensions; worst-case black-wallpaper contrast was calculated at 5.56:1 for shell text and 6.61:1 for card text; full build/test/lint passed.
- Evidence: `.dispatch/20260712-1544-claude-opus-review-r1.md`.

### Pass 4 — Claude R2 found a real native-width regression, then fixed

- Claude found that `@media (max-width: 880px)` leaked `margin-top: 22px` into compact mode. Wide browser evidence had not triggered that rule; a real 320px viewport would overlap the summary by 18px.
- Fix: compact period control now explicitly resets `margin: 0` and `max-width: none`.
- Additional fix: compact quota now says `ChatGPT · Codex 短窗`, uses a 116px protected column, and prioritizes demo provenance.
- Post-fix actual-viewport evidence: media query matched, computed margin was `0px`, period bottom was 78px, summary top was 82px, overlap was 0, and quota client/scroll size was exactly 116 × 86.
- Evidence: `.dispatch/20260712-1554-claude-opus-review-r2.md`.

### Pass 5 — final local/native pass after the three-round Claude cap

- Claude R3 confirmed all user-targeted behavior and visual corrections, then retained three advisory P2 classifications: external-shadow breathing room, possible DPI rounding, and brand-permission process.
- DPI concern was hardened with 2px of unused grid budget; actual footer clearance is 8px and the shell has no scroll overflow.
- Shadow concern is an explicit compactness tradeoff after native inspection; window size remains 320 × 320 rather than adding a click-intercepting 360px transparent perimeter.
- Brand concern is retained as a public-commercial-release check; it does not replace the official icons the user explicitly requested.
- Under the three-round review cap, Claude's final label remained `NEEDS_FIX`; local/browser/native design QA classifies the remaining items as P3 constraints rather than current Windows P0/P1/P2 defects.

## Interaction and runtime checks

- Transparent toggle changes the material, exposes `aria-pressed`, and survives a reload.
- Native Windows Tauri alpha was checked in both modes; standard mode was fully opaque, while wallpaper content was visible in transparent mode behind protected title/metric/card surfaces.
- Compact → expanded → compact works; expanded mode remains an opaque 1120 × 760 analysis view.
- Today and 7-day controls were clicked; the selected state and totals updated.
- ChatGPT Agent filtering was clicked; the metric label, total, and comparison copy updated.
- Official provider images loaded in both compact and expanded views.
- Browser page logs were checked after the primary journey: no application warnings or errors.
- Final Windows release build produced both MSI and NSIS bundles. The release executable was launched and checked at 320 × 320, in transparent mode, expanded at 1120 × 760, and collapsed back with transparency preserved.
- Frontend production build passed. Rust result: 48 passed, 0 failed, 2 live-environment tests ignored. Rust formatting and strict Clippy passed.

## Implementation checklist

- [x] Raised and separated the period control.
- [x] Reframed compact mode as a true 320 × 320 desktop small widget.
- [x] Added persisted standard/transparent modes and native window alpha.
- [x] Replaced provider glyph approximations with official ChatGPT and Claude app icons.
- [x] Preserved the full analysis view and accurate source semantics.
- [x] Compared full and focused source/implementation evidence after the final fix.
- [x] Checked overflow, interactions, console output, native alpha, build, tests, formatting, and lint.

final result: passed

---

## 2026-07-13 视觉与功能重构（竞品分析驱动）

依据 codexbar / Win-CodexBar / cc-bar / usage / codexU / token-monitor 的竞品分析（结论存 Claude 记忆 metrik-design-audit-2026-07）：

- 玻璃分层：`setWindowGlass` 返回 native/css/off；原生 Acrylic 不可用或失效时回落 CSS 近实心玻璃拟态（`.widget-shell--glass-css`，明暗双套，参数对标 Win-CodexBar 0.94 底 + 细白边 + 分层阴影）。release 玻璃失效不再呈现为"坏掉的白板"。
- 配额元语：进度条改 Agent 品牌色（`--quota-accent`），5/6px 胶囊；≥85% 警示黄、≥95% 告急红；陈旧快照改用透明度 0.55 编码（不再用棕色）；周窗口新增 Pace 节奏行（本地推算，三档表述）。
- 趋势图："全部"视图改多 Agent 分色曲线（不再求和成单线），图例随数据；tooltip 支持多行。
- 新增区块：概览页 Token 构成（未缓存输入/缓存读/缓存写/输出堆叠条）与模型分布 Top6（后端 snapshot 新增 agents 分量字段与 models 聚合）。
- 应用图标重绘：深色玻璃底 + 蓝橙双弧仪表（design/app-icon-1024.png，tauri icon 重新生成全套）。

验收截图：output/playwright/compact-light.png、compact-dark.png、expanded-light-fixed.png、expanded-light-tall.png（浏览器演示模式）。npm run build 通过；cargo test 67 通过 / 2 忽略；cargo clippy -D warnings 干净。桌面实机（原生玻璃路径与托盘图标）待人工验收。

## 2026-07-13（下午）功能补全与交互修正

- 玻璃改为固定深色 HUD（浅色玻璃方向废弃）；DWM 系统圆角 + CSS 8px 贴合修复四角漏白；tint 0.84→用户反馈灰蒙蒙→定稿 0.82 冷色偏置。
- 固定按钮语义修正：固定 = 置顶 + 锁定位置（禁用拖动与边缘挂靠）；取消固定恢复。
- expanded 三区独立滚动（dashboard/inspector/settings/reports）。
- 小组件 Agent 自选：设置页勾选（至少 1 个），compact 列表 1–4 行密度自适应（:has 选择器）。
- OpenCode 图标换官方 apple-touch-icon（ATTRIBUTION.md 已记录）；其余三家图标核对无误。
- 新增报告页：usage_report 命令只读账本不触发扫描（WAL 并发读），26 周热力图（蓝色序列 5 档、分位数阈值）+ Agent/模型排行 + streak。
- 新增成本估算：pricing.rs 静态价表（OpenAI/Anthropic 官方价，2026-07-13 查证；GLM 因价格来源矛盾不计价），最长前缀匹配，按分量计价；概览成本卡明确标注"非官方账单"与未计价 token。
- cargo test 78 通过 / 2 忽略；clippy -D warnings 干净；npm build 通过。桌面实机与真实账本数据待人工验收。

## 2026-07-13（晚）会话明细页与修正

- 「用量」页落地为会话明细流（用户选定形态）：usage_sessions 命令按 (agent, session_id) 聚合（只读账本、300 条截断如实标注、无 session 维度的远端同步事件不计入），前端按天分组 + Agent/模型筛选 + CSV 导出（RFC4180 + BOM，只含统计字段）。
- codex adapter 修复 model 归属：跟踪 turn_context/session_meta 的 model 上下文（实机确认 596M "未标注" 全部来自此缺口）；历史事件需「重建本地账本」才能重新归属。
- 任务栏语义：compact skipTaskbar，expanded 出现在任务栏。
- 周期切换条加毛玻璃底（backdrop-filter）解决滚动压盖观感；报告页顶部留白收紧。
- 新 Agent（Cursor/Antigravity/Kimi/Qwen）本机均未安装，按实机验收红线暂不实现，已记路线图。
- cargo test 85 通过 / 2 忽略；clippy 干净；npm build 通过。

## 2026-07-14 报告图表切换、设置布局、玻璃浓度

- 报告页图表可切换：热力图 / 周趋势折线（SVG 多 Agent 分色）/ Agent 构成环形（中心总量 + 图例数值）。
- 设置页改双列卡片网格（auto-fit minmax 400px），消除右侧大空白；新增「小组件玻璃浓度」滑杆（60–96%，localStorage 持久化，写入 --glass-alpha）。
- 玻璃配方对标 ModernFlyouts 调研结论：半透明纯色板 + 系统模糊 + 浓度可调；描边独立于背景浓度（外圈 rgba(0,0,0,.38) 收边 + 内侧白高光），CSS 回落层锁定 0.95 近实心（FallbackColor 思路）。
- 「未标注模型」复核：账本 2191 事件源文件均含 turn_context（gpt-5.6-sol），adapter 修复有效，等待用户执行「重建本地账本」重新归属。
- ChatMem 整合调研完成（红线评估入 handoff）：P0 项目分组、P1 打开源文件/复制 resume 命令、P1 收藏；全文搜索/垃圾桶/迁移因隐私红线或产品边界排除。

## 2026-07-14（二）图表美学、会话 ID、加载阻塞修复

- 图表配色降饱和一档（#5586d4/#d98663/#8b80d9/#4aa392），报告周趋势改 Catmull-Rom 平滑曲线 + 淡面积渐变；概览 uPlot 多线恢复低透明面积，图例同步。
- 用量页会话行加 ID 芯片：点击复制完整会话 ID（resume 用），绿色打勾反馈；不加删除/迁移等重操作（红线与产品边界）。
- 布局收紧：dashboard 顶距 136→112、主数字 clamp 84–118px、图表高 clamp 260–380px，默认 1120×760 一屏可见主图与分解卡。
- 报告/用量页阻塞根因：open_database 每次执行 ensure_schema 的 PRAGMA user_version（写操作）与扫描写锁互斥 → 新增 open_database_read_only（SQLITE_OPEN_READ_ONLY、不跑迁移、busy_timeout 5s），build_report/build_sessions 改用之；账本未建时显式报错不再挂起。cargo test 87 通过。

## 2026-07-14（三）fork 重复计数、CSV 导出、Claude 额度诊断

- 「未标注模型」真根因（纠正昨日结论）：596M 全部来自 5 个 Codex Desktop subagent fork 文件——session_meta 带 forked_from_id，文件头部回放父线程的累计 token_count（时间戳挤在 ~100ms 内），首个 turn_context 在回放之后才出现。这些计数父线程文件已入账（实机比对到相同累计值 398220），属于重复计数而非缺模型。
- adapter 修复：fork 文件跳过首个 turn_context 之前的 token_count（仍推进基线，首个真实增量只计 fork 自己的量），回放期的 rate_limits 一并丢弃；新增测试夹具。PARSER_VERSION 3→4 自动触发全量重扫，无需手点重建。
- 重扫实机验证：未标注模型 2191 事件/596M → 0；迁移同时按保留期回填了首扫窗口漏掉的历史（codex 18k→40k 事件，属补全非虚增）。
- CSV 导出点击无反应根因：Tauri WebView 不处理 blob 下载。新增 export_csv 命令写入系统「下载」目录（文件名消毒、重名自动加序号），前端显示完整导出路径；浏览器演示模式退回 blob 下载。导出内容仅账本统计字段（时间/Agent/模型/token 分量/估算成本/事件数/会话 ID），不含对话正文。
- Claude 额度不更新诊断：statusLine 脚本与 settings.json 配置实测均正常（手动喂样例立即生效），但 Claude Code 近期会话未调用 statusline 命令（配额文件停在旧时间戳而会话日志持续写入）——额度数据依赖终端里渲染出状态栏的交互会话。UI 侧过期窗口已按"--·已重置"处理，不显示陈旧百分比。
- cargo test 88 通过 / 2 忽略；clippy -D warnings 干净；npm build 通过。

## 2026-07-14（四）Claude 官方额度直连（OAuth，opt-in）

- 新增 claude_oauth.rs：读取 Claude Code 自存凭据（~/.claude/.credentials.json 的 claudeAiOauth.accessToken，需 user:profile scope），GET api.anthropic.com/api/oauth/usage（anthropic-beta: oauth-2025-04-20），utilization→remaining、resets_at ISO8601→ms，窗口键与 statusLine 钩子一致（five_hour/seven_day），下游展示零改动。响应结构对照 CodexBar 实现核实。
- 红线落实：默认关闭（app_setting: claude_oauth_quota_enabled），设置页「Claude Code 官方配额」卡内新增"官方额度直连"子块显式开启；token 只在内存用于单次请求，不入库/不上传/不写日志，错误信息不含请求头；状态命令只回布尔（凭据存在/scope 满足），永不回传 token。
- 取数策略：开启时优先 OAuth（缓存 120s，失败哨兵 300s 限流友好，超时 6s），失败或未开启回落 statusLine 钩子文件；关闭时清除 OAuth 来源的展示行。额度为账户级合并值（含网页版消耗），解决"statusline 不渲染额度就冻结"的短板。
- 新依赖 ureq 2（rustls）。cargo test 90 通过 / 2 忽略；clippy -D warnings 干净；cargo fmt --check 全仓干净（顺带修平 3 个历史漂移文件）；npm build 通过。实机开关与真实拉取待用户在设置页验收。
- 补充（同日）：经查 Anthropic 2026-02 消费者条款明确禁止第三方工具使用订阅 OAuth token（执法集中在第三方推理代理，未见只读用量查询封号案例），已在开关说明中加入条款风险须知，保持默认关闭 + 知情同意。

## 2026-07-14（五）statusLine 钩子串联化 + 任务栏修复

- 额度冻结真正根因落定：用户另一 agent 把 statusLine 换成了自定义 statusline.ps1（只显示目录/模型/上下文/成本，不落额度数据），metrik-quota.json 因此停更——并非 Claude Code 不渲染状态栏。
- 钩子改为串联而非独占：安装时若已有 command 型 statusLine，原设置备份到 metrik-statusline.backup.json，Metrik 脚本落完额度后把 stdin 转给原命令渲染、行尾追加 5h/7d；卸载原样恢复备份。缺 command 字段的才算冲突拒装。分发场景下用户无需统一 statusLine 设置，点安装即自动接管。
- Windows 串联实现：PowerShell 脚本走进程内执行（[Console]::SetIn 喂 stdin，零跨进程编码转换），非 PS 命令回退子进程 $input 管道；实测整链输出与 UTF-8 字节正确。排查中发现用户 statusline.ps1 本身是无 BOM UTF-8（PS 5.1 按 GBK 读源码导致省略号乱码，直跑即可复现），补 BOM 根治。
- 本机已按安装器产物手动落地串联（脚本/备份/settings 指回 metrik-statusline.ps1），下次状态栏渲染起额度恢复更新。
- expanded 不上任务栏根因：窗口以 skipTaskbar:true 创建，运行中直接改样式 shell 不重读；改为切换前 hide、改后 show+focus（Windows 机制代价是切换瞬间短暂闪隐）。
- cargo test 91 通过 / 2 忽略；clippy -D warnings 干净；fmt 干净；npm build 通过。

## 2026-07-14（六）小组件位置持久化 + 开机启动

- 位置不持久根因：compactPosition 只是模块内存变量，重启即丢；窗口 conf 又是 center:true。改为 onMoved 防抖 400ms 后把物理坐标写入 localStorage(metrik:widgetPosition)，启动时 restoreWindowPosition 恢复。
- 越界保护：写入前若窗口 y 低于显示器顶（边缘挂靠滑出屏幕的中间态）则不记；恢复时用 availableMonitors 校验坐标至少部分落在某块屏内，否则居中——拔掉扩展屏不会把小组件丢到屏外。
- 新增开机启动（tauri-plugin-autostart 2.5.1 + @tauri-apps/plugin-autostart，capabilities 加 autostart:allow-enable/disable/is-enabled）：设置页「启动与位置」卡片 opt-in，默认关闭。
- cargo test 91 通过 / 2 忽略；clippy -D warnings 干净；fmt 干净；npm build 通过。实机验收：拖动小组件→重启应用应回原位；开机启动开关需重启 Windows 验证。

## 2026-07-14（七）Codex 额度窗口纠错 + Kimi 接入 + Antigravity 评估

### Codex 额度不准（两个叠加 bug，实机确认并修复）
- **窗口语义硬编码**：primary/secondary 只是槽位不是语义。实测本机 prolite 套餐返回 `primary:{usedPercent:25, windowDurationMins:10080}`（10080 分钟 = 7 天周窗）、`secondary:null`，而代码把 primary 一律当 5 小时窗 → 把周额度标成"5 小时额度"。改为按 windowDurationMins 归类（≤1440 分钟为会话窗，否则周窗），槽位仅作缺时长时的回退；app-server 与日志两个来源都带该字段（app-server: windowDurationMins；日志: window_minutes）。
- **僵尸窗口**：套餐变更后官方不再返回的窗口，旧日志快照仍滞留展示（本机残留 07-12 的 secondary=48%）。改为 app-server 拉取成功时整体替换 codex 额度行（与 Claude 一致）。
- 实机验证：修复后账本只剩一条 codex secondary=75%（7-20 重置），僵尸行已清。

### Kimi 接入（新 Agent，未实机验收）
- 格式经调研核实（ccusage/官方 wire 协议文档），两代格式分支：新版 `~/.kimi-code/sessions/**/agents/*/wire.jsonl` 顶层 `usage.record`（camelCase、time 毫秒），**只计 `usageScope=="turn"`（单轮增量），`session` 作用域是会话累计总量、计入必重复计数**；旧版 `~/.kimi/sessions/**/wire.jsonl` 的 `StatusUpdate`（snake_case、timestamp 为 Unix 秒浮点），按 message_id 合并取分量最大值（防渐进更新重复计数）。
- 会话 ID 取自目录路径（记录内没有）；旧版 StatusUpdate 无模型名 → 诚实留 None，不猜。
- 定价：Kimi 订阅制、公开单价未核实 → 不计价（测试钉死 kimi 模型不得借用他家价格）。
- 官方额度：无本地可读快照（需调 api.kimi.com 且端点未公开）→ 不做。
- 测试夹具覆盖：turn/session 作用域区分、旧版 message_id 合并、时间戳单位、部分输入降级。**本机未安装 Kimi，未实机验收**；discover 找不到目录时为 0，不推算。

### Antigravity（评估后暂不接入，理由如实记录）
- 数据不在本地日志：token 只存在于本机 language_server 的私有 RPC（`exa.language_server_pb`，自签 TLS + 进程命令行取 csrf_token），需 IDE 常驻运行。
- 三个阻断项：①模型名是会随版本腐烂的占位符别名表（MODEL_PLACEHOLDER_M37 → gemini-3.1-pro-high）；②cache 分量字段无任何 fixture 证明存在；③两个上游参考实现（CodexBar / antigravity-token-monitor）**都不支持 Windows**（进程/端口发现依赖 lsof），Windows 发现层无人验证过。
- 参考实现的降级策略是"按字符数 /4 估算 token"，直接违反本项目红线（估算不得冒充解析）。
- 结论：等用户实机安装后现场探测真实 RPC 响应再实现；强行按文档猜测实现会重蹈 Codex 窗口语义硬编码的覆辙。

cargo test 96 通过 / 2 忽略；clippy -D warnings 干净；fmt 干净；npm build 通过。

## 2026-07-14（八）任务栏修复、报告卡片统一、macOS 上 CI、Antigravity 接入

- **任务栏真根因**：窗口是无边框（decorations:false），Windows 下这类窗口默认无任务栏按钮——由 WS_EX_APPWINDOW 决定，而 setSkipTaskbar 只动 WS_EX_TOOLWINDOW，补不上。新增 set_taskbar_button 命令直接改扩展样式（隐藏态改、重显后 shell 重读）。之前两次"先藏后显"没用就是因为动错了标志。
- **报告卡片忽大忽小**：三视图自然高度差大（折线 SVG 按 viewBox 撑高）。加固定高度容器 .report-view-body（272px），折线 SVG 改 height:100% + max-height。
- **macOS 上 CI**：加 macos-latest 到 CI/Release 矩阵。**实测 macOS 编译通过、95 项测试全绿**（比预期乐观：玻璃走 setEffects、托盘走 Accessory，路径都是跨平台的）。唯一失败是 setup-node 的 npm 缓存后置步骤，去掉 cache:npm 即绿。Release 出 universal-apple-darwin 的 dmg。macOS 仍未实机验收（托盘/边缘挂靠/开机启动行为待核）。
- **Antigravity 接入**（上次结论被推翻）：tokscale（Rust，今日仍在维护）证明 Windows 进程/端口发现可行（Get-CimInstance + netstat）。字段路径经 tokscale/CodexBar/antigravity-token-monitor 三方交叉核实。
  - 架构：RPC 轮询套进 AgentAdapter（discover=列 cascade+配额入口，parse=取会话 generatorMetadata）。每次全量返回→按 responseId 去重，storage 新增 antigravity "response:" 键走渐进合并（与 Claude 同型）。
  - TLS：rustls dangerous verifier 接受自签证书，仅连 127.0.0.1。
  - 口径风险（均已编码 + 标注，未真机验证）：无进程=0 不估算（45s 负缓存防高频扫描）；cache_write 无证据恒 0；thinking 作 output 子项不重复计入（tokscale 相反假设，真机对不上就改这里）；占位符模型查表转真名，未知原样保留。
  - Windows/macOS/Linux 三平台进程发现都实现了；均未实机验收（本机无 Antigravity）。
- cargo test 101 通过 / 2 忽略；clippy 干净；fmt 干净；npm build 通过。

## 2026-07-14（九）7 天 / 30 天卡死：解析窗口不该跟着 UI 周期走

用户报「统计 7 天、30 天非常慢甚至刷不出来」。先量后改，实测（真实账本：Codex 3.0 GB / 583 个日志，Claude 235 MB / 274 个）：

| 周期 | 修复前 | 修复后 |
| --- | --- | --- |
| 今天 | 16.8 s | ~4.4 s |
| 7 天 | 14.6 s | 5.7 s |
| 30 天 | **1402 s（23 分钟）** | **7.1 s** |

**根因一：解析视界跟着 UI 周期变。** `discovery_cutoff_ms` 让「今天」按 8 天、「7 天」按 15 天、「30 天」按 61 天解析。`scan_source.coverage_start_ms` 记着每个文件解析到多久以前，一旦选了更宽的周期，之前按窄窗口解析过的源全部失效、整份重扫——而且是同步跑在一次 `usage_snapshot` 请求里。实测切到 30 天要重扫 691 个文件（今天只要 0 个）。改成**固定按保留期（65 天）解析**：每个文件只在新增/变化时解析一次，切周期退化成纯 SQL 查询。

**根因二（更致命）：单源入库代价与账本总量成正比。** 一个 70 KB、只含 1 个事件的日志，入库要 1.3–2.4 秒，其中真正写事件只花 0.3 ms：
- 清旧观察记录 0.7–1.4 s —— `event_observation` 主键是 `(event_id, source_id)`，按 `source_id` 删只能全表扫 49k 行；外加一个无谓的 `event_id IN (SELECT …)` 子查询。
- 孤儿清扫 0.5–1.0 s —— 每个源都做一次 `usage_event` 全表 NOT EXISTS 扫描。

690 个源 × 2 秒 ≈ 1400 秒，与实测的 1402 秒吻合。修法：加 `idx_event_observation_source` 索引；视界统一后去掉时间窗口切片；孤儿清扫只检查**刚失去观察记录的那批 event_id**，不再扫全表。单源入库 **1300–2400 ms → 4 ms**。

纯解析吞吐实测 52 MB/s（3 GB 约 60 秒），一直不是瓶颈——95% 的时间都在写库。

**不阻塞界面**：待解析的源按 mtime 倒序排队（最近的先做，当前周期最先变准），每次快照只花 `PARSE_BUDGET`（1.5 s），剩余量走 `indexing.pending` 返回前端。补齐期间界面显式标注「正在补齐历史索引，还剩 N 个日志文件，历史周期的数字尚不完整」，并把轮询提到 400 ms 直到补完——**不拿不完整的账本冒充精确结果**。

老库升级后会一次性补齐（约 1 分钟后台解析），全程界面可用、有进度。新索引经 `001_init.sql` 的 `IF NOT EXISTS` 自动补建，无需 `PARSER_VERSION` 变更。

cargo test 101 通过 / 2 忽略；clippy -D warnings 干净；fmt 干净；npm build 通过。
