# macOS 交接：v0.9.0 的新 Agent 与共享改动

写于 2026-07-20，作者在 Windows 机器上完成 v0.9.0 的全部开发与验收。
**本轮所有改动都没有在 macOS 上运行过**——CI 的 macOS job 只证明能编译、能打包，
不证明运行时正确。这份文档列出 mac 那台需要做什么、哪些不用管。

与 `AGENTS.md` 的 "Durable cross-platform development workflow" 一致：
shared 代码写一次两平台都要验；两个 shell 的窗口形态与交互互不抄袭。

---

## 一、先跑一遍（10 分钟）

```bash
cd src-tauri
cargo check            # 依赖变动的第一道关
cargo test             # 应为 135 通过 / 4 忽略
cargo clippy -- -D warnings
cd .. && npm run build
npm run desktop:dev    # 实机跑起来
```

**依赖有一处需要注意**：`Cargo.toml` 里 rustls 改成了

```toml
rustls = { version = "0.23", default-features = false, features = ["ring", "std", "tls12", "logging"] }
```

起因是 Antigravity 的自签 TLS 第一次真跑时 rustls 0.23 直接 panic——ring 与
aws-lc-rs 同时启用，它无法自动确定 CryptoProvider，而且 panic 落在扫描线程上，
把整个快照拖垮（界面表现是**所有** Agent 都"读取失败"）。这条是 shared，mac 同样
受影响、同样受益。副作用：lockfile 移除了 aws-lc-rs 及其 C 构建链（cmake /
fs_extra / jobserver），**mac 首次构建会重新解析依赖**。若 mac 上有别的 crate 依赖
aws-lc-rs，`cargo check` 会立刻报出来。

`cargo test` 里 `managed_child_terminates_windows_process_tree_after_descendant_is_ready`
在 Windows 上间歇性失败（进程树时序，本轮之前就存在），重跑即过；mac 上该测试不适用。

---

## 二、必须实机验的三件事

### 1. WorkBuddy 配额（把握最低，优先验）

新增的 `workbuddy` Agent 有两条数据线，mac 上都没验过：

| 数据 | 路径 | 把握 |
|---|---|---|
| token 用量 | `~/.codebuddy/projects/**/*.jsonl`、`~/.workbuddy/projects/**/*.jsonl` | 高（home 目录，跨平台一致） |
| 官方 Credits 配额 | 凭据 `~/Library/Application Support/CodeBuddyExtension/Data/Public/auth/*.info` | **低（按惯例推断，未验证）** |

**怎么验**：装 CodeBuddy / WorkBuddy 并登录 → 设置里勾选 WorkBuddy → 看配额卡是否
显示 Credits 剩余百分比。

**不出数据时**：先确认那个 auth 目录在 mac 上的真实名字。只需要改
`src-tauri/src/coding_quota.rs` 的 `workbuddy_auth_dirs()`——它现在遍历
`dirs::data_local_dir()` 与 `dirs::data_dir()`（mac 上两者都是
`~/Library/Application Support`，已去重）。其余解析、聚合、HTTP 逻辑是 shared，
在 Windows 真机账号上验证通过，不用动。

接口本身（`POST /v2/billing/meter/get-user-resource`，跨套餐求和
`CycleCapacitySize`/`CycleCapacityRemain`）已用真实国内账号实测通过；host 候选
按凭据里的 `domain` 优先，再退到 `www.codebuddy.cn` → `copilot.tencent.com` →
`www.codebuddy.ai`。国际版账号走最后一个。

**凭据处理原则**（与 zcode / kimi 一致，别改成别的）：客户端把 accessToken
明文落在那个 `.info` 里，我们自动读、仅在内存中用于一次请求，不入库、不写日志、
错误信息里不带 token。

#### WorkBuddy 的 token 用量：只有 CLI 有，桌面版没有

adapter 扫的是 **CLI** 的转录 `~/.codebuddy/projects`、`~/.workbuddy/projects`
下的 JSONL。**桌面版（Electron）不写这些 JSONL**，Windows 真机确认：装了桌面版
并跑过会话后，两个目录都不存在，用量只落在 `~/.workbuddy/workbuddy.db` 的
`session_usage` 表里，形如

```
session_id | used=82275 | size=168000 | credit_json={"<req-id>":3.47, ...}
```

`size` 恰好是上下文窗口大小，所以 `used` **很可能是当前上下文占用量而非累计
处理量**——若当累计量记进账本，会漏掉输出 token 与跨轮缓存读，且上下文压缩后
还会倒退。这就是这张表至今没有接入的原因（见 `workbuddy.rs` 的 `coverage_gaps`，
它会如实上报"旧版数据库暂不支持读取"）。判定实验：在同一会话里持续追加对话，
看 `used` 是否突破 `size`——突破则为累计量可接，回落则为上下文占用不可接。

`credit_json` 是本地记录的待结算 Credits（实测与官方配额接口有延迟，接口那边
可能还显示已用 0），属于计费口径不是 token 口径，不要混进 token 统计。

结论：**mac 上若只装桌面版，WorkBuddy 显示 0 token 是正确行为**，不是 bug；
配额卡仍应正常显示。要有 token 统计需装 CodeBuddy Code CLI。

### 2. Antigravity 进程发现

Antigravity 在 Windows 上一共踩了三个坑，其中**两个是 shared、mac 直接受益**，
一个是各平台各自的：

| 问题 | 平台 | 状态 |
|---|---|---|
| 只认第一个 language_server 进程（实际 5 个里仅 1 个在监听） | shared | 已修，改为遍历全部候选 |
| token 是 JSON 字符串 `"21861"`，只认数字导致全读成 0 | shared | 已修 |
| PowerShell 按 GBK 输出，含中文路径就整段 JSON 解析失败 | Windows only | 已修（强制 UTF-8） |
| **`ps` 截断命令行导致取不到 `--csrf_token`** | **macOS/Linux** | **已改为 `ps -axww`，未验证** |

最后一条是我在交接前审出来的：macOS 的 `ps` 默认把每行截断到终端宽度，而
`--csrf_token` 在 language_server 那条长命令行的靠后位置——截断后 `extract_csrf`
取不到，端点发现静默失败，Antigravity 在 mac 上会**完全没有数据**。已加 `ww`
（Linux 的 ps 同样接受，两平台通用），但**没有 mac 机器可验**。

**怎么验**：Antigravity IDE 运行时打开 Metrik，看 token 用量与 Gemini 5h/周配额
是否出现。不出数据时在 mac 上手动跑一次：

```bash
/bin/ps -axww -o pid=,command= | grep language_server | grep -o -- '--csrf_token[= ][0-9a-f-]*'
```

能打印出完整 token 就说明 `ww` 生效；打印不出说明 mac 的 ps 行为还有别的差异。
再往下可参照 `src-tauri/src/adapters/antigravity.rs` 里 `discover_endpoint()` 的
逐进程逐端口 Heartbeat 逻辑手动复现（端口用 `lsof -Pan -p <pid> -iTCP -sTCP:LISTEN`）。

### 3. Agent 排序驱动 macOS 菜单栏

小组件的 Agent 选择卡新增了排序（↑ 上移），**顺序同时驱动三处**：小组件行、
配额卡轮换、以及 **macOS 菜单栏状态项**（`updateMacStatusItems` 按 `widgetAgents`
数组顺序迭代）。mac 上确认：在设置里调整顺序后，菜单栏状态项的排列跟着变、且符合预期。

顺序持久化在原有的 `metrik:widgetAgents` 里（本来就是数组），老数据直接兼容。

---

## 三、共享 UI 改动，扫一眼外观

这些 mac 也会渲染：

- **小卡片恢复了 token 大数字版式**（周期签 + 总用量 + 可轮换的配额卡）。
  相关的 `widget-metric` 行高从 0.86 改为 1，修的是 75% 缩放档下数字压盖标签。
- **设置页 9 张卡合并为 5 张**、分两排（长卡一排统一高度、短卡一排），文案精简约一半。
  其中 "显示的 Agent" 卡在 mac 上只显示小组件一栏（胶囊条是 Windows shell 专属）。
- **两个新 Agent 图标**：WorkBuddy 是 SVG、Qoder 是 180×180 PNG，在 Retina 下看清晰度。
  出处记在 `src/assets/ATTRIBUTION.md`。
- **Qoder 设置卡**：解释为什么需要 cookie + DevTools 分步引导 + 粘贴后立即验证。
  mac 上可顺带验一下引导文案里的浏览器操作路径是否贴合 macOS Chrome。

---

## 四、明确**不要**照抄的 Windows shell 部分

按双机规则，这些是 Windows 外壳专属，mac 侧自行决定要不要做等价物：

- **任务栏注册**：`set_taskbar_button` 用 `ITaskbarList::AddTab`/`DeleteTab` +
  `WS_EX_APPWINDOW` 让完整视图出现在任务栏。整段在 `#[cfg(windows)]` 里，
  非 Windows 是 no-op。mac 没有这个概念，**不需要跟进**。
  （踩坑记录：Tauri 的 `setSkipTaskbar(true)` 实现为 `DeleteTab`，光翻转窗口样式
  撤不掉这个注销——必须显式 `AddTab`。）
- **完整视图的最大化按钮**：加在 Windows 的自绘按钮条上。mac 用原生标题栏
  （`macMinimal`），按钮条本来就不显示，**不需要跟进**。
- **卡片拖边缘调整大小**：`startCompactResizeScale` 在 `isMacPlatform()` 时直接
  返回空操作。mac 的面板是锚定菜单栏的 NSPanel，拖拽语义完全不同，**做不做由 mac 侧定**。

---

## 五、新增 Agent 的产品约束（两平台一致，别改）

- **Qoder 是配额-only**：QoderWork 本地 `agents.db` 的 token 字段恒为 0、新版已删该
  字段，本地用量确实无来源。所以它**没有日志 adapter**，只在 `AGENT_IDS` 里占位 +
  配额来源。不要为了"看起来完整"给它编本地统计。
- **Qoder 的 cookie 必须由用户提供**：它的桌面端凭据是 Electron safeStorage 加密的
  （与 zcode 的 `enc:v1` 同性质），我们不解密、不碰浏览器 cookie 库。这条与
  WorkBuddy 的"明文凭据自动读"是两种情况，别混。
- **WorkBuddy 的 token 口径陷阱**：驼峰 `inputTokens` 是**含缓存**的 prompt 总量
  （要扣 `cache_read`），蛇形 `input_tokens` 是 Anthropic 风格不含缓存，
  `cachedMissTokens` 存在时最优先。这个已按真实夹具核实并有测试锁住，改动前先看测试。

---

## 六、版本号

v0.9.0 已发布（draft 里双平台产物齐全、`latest.json` 七个平台键签名齐全）。
版本号是双机共享的全局资源，发版串行——mac 侧做完验收如果需要发 v0.9.1，
按 `AGENTS.md` 里 zcode 新补的那两条来：main 是分支保护的，必须走
`release/vX.Y.Z` PR + 同会话推 tag；发布前确认**同一 tag 只有一个 draft**
（Release workflow 的两个平台 job 会竞态创建出两个各带一半产物的 draft）。
