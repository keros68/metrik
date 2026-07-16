# macOS 原生化设计（菜单栏面板 + 独立完整视图）

日期：2026-07-14
状态：已批准，待实现

## 背景

macOS 上的 Metrik 目前和 Windows 版共用同一套无边框窗口 + 自绘窗口按钮 + Windows 玻璃方案，
在 mac 上看起来是一个"跑错平台的窗口"：彩色 App 图标直接贴进菜单栏，右上角是 Windows 式的
最小化/关闭按钮，玻璃材质是一块不透明深灰卡片。

代码层面有三个确定的缺陷（都只影响 macOS）：

1. `src-tauri/src/lib.rs:980` 托盘直接用 `app.default_window_icon()`（彩色圆角方块）。
   macOS 菜单栏要求 template image（纯黑 + alpha），由系统按浅/深色菜单栏反色。
2. `src/windowClient.js:195` 一次性传入 `["popover", "hudWindow", "blur"]` 三个 vibrancy 材质。
   macOS 的 vibrancy 是单选的，传一串等于结果不可控。
3. `src-tauri/tauri.conf.json` 有 `transparent: true` 但**没有 `macOSPrivateApi: true`**，
   Tauri 在 macOS 上的透明窗口依赖该开关；同时 macOS 分支从未调用 `makeWebviewTransparent()`
   （只有 Windows 分支调了），WKWebView 的不透明底色会盖住 vibrancy 层。
   这两条叠加，导致 mac 上根本没有真实的系统模糊。

## 目标

macOS 上表现为一个原生的菜单栏应用（参照 CodexBar 类工具的形态）：
菜单栏 template 图标 → 点击弹出面板 → 面板不抢焦点、失焦自动收起 → "完整视图"开一个标准 mac 窗口。

**Windows / Linux 行为零改动**：所有新逻辑都在 `cfg(target_os = "macos")` 或前端的
`isMacPlatform()` 分支内，Windows 侧继续走"单窗口变形 + 自绘按钮 + 边缘挂靠"那一套。

## 非目标

- 不额外设计一套独立的浅色主题；面板使用原生菜单材质并跟随系统当前外观，做法参照 CodexBar 的 NSMenu，WebView 只提供保证对比度所需的浅/深内容层。
- 菜单栏图标旁显示用量数字（tray title）：本轮只做图标。
- 真 NSPopover（带指向箭头）：收益仅一个箭头，需要把 webview 从 Tauri 窗口体系里挖出来，
  风险与收益不成正比。用 NSPanel 模拟（无箭头，行为一致）。
- Linux：不在本轮范围，继续走 Windows 那条通用路径。

## 设计

### 1. 窗口结构

macOS 上从"一个窗口两种形态"改为两个窗口：

| | Windows / Linux（不变） | macOS（新） |
|---|---|---|
| compact | `main` 窗口变形 | `main` → 转成 NSPanel，定位在菜单栏图标下方，失焦隐藏，永不变形 |
| expanded | 同一个 `main` 变形 | 新建 `expanded` 窗口：`decorations: true`，原生红绿灯，可缩放 |

理由：NSPanel 的样式掩码（`NSWindowStyleMaskNonActivatingPanel`）与标准可缩放窗口冲突，
运行时来回切换会产生不可预期的行为。两个窗口各自遵守各自的平台规矩。

代价：`expanded` 是新建窗口，会重新加载前端并重新拉一次 snapshot（数百毫秒）；
两个窗口的内存状态不共享（读的是同一个 SQLite，无一致性问题）。

### 2. 菜单栏图标

- 新增 `src-tauri/icons/tray-macos.png`（22×22）与 `tray-macos@2x.png`（44×44）：
  取现有 logo 的仪表盘弧形+指针剪影，纯黑 + alpha，去掉圆角方块背景与全部颜色。
- macOS 分支用 `Image::from_bytes(include_bytes!(...))` + `.icon_as_template(true)`。
  这一个调用就是"与系统适配"的开关。
- Windows/Linux 继续 `default_window_icon()`。
- 需要给 tauri 开 `image-png` feature。

### 3. 面板（compact）

- 启动时不可见；`tauri-nspanel`（git 依赖，`branch = "v2"`）把 `main` 转成 panel：
  - `set_style_mask(NSWindowStyleMaskNonActivatingPanel)` —— 弹出时不夺走前台 App 焦点。
  - `set_level(NSMainMenuWindowLevel + 1)`、collection behavior 含
    `CanJoinAllSpaces | Stationary | FullScreenAuxiliary` —— 可浮在全屏应用之上。
  - `panel_delegate!` 监听 `window_did_resign_key` → 隐藏面板（失焦收起）。
- 定位：托盘左键点击事件带 `rect`（图标的屏幕矩形），面板水平居中对齐图标、
  垂直贴在菜单栏下方；靠近屏幕右缘时向内收，不出屏。再次点击图标收起。
- 面板内：无 `WindowActions`（玻璃/固定/最小化/关闭四个按钮在 macOS 上全部不渲染），
  无拖动区（面板贴着图标，拖动无意义）。标题栏只剩 `Metrik` + 状态点，底部保留"完整视图"。
- macOS 上禁用：边缘挂靠、位置记忆、置顶开关、玻璃开关（这些在 `windowClient.js` 里按平台短路）。

### 4. 菜单栏右键菜单

承接被移除的功能：`显示 / 隐藏`、`完整视图`、`设置`、`退出 Metrik`。
（Windows 托盘菜单保持现状：`显示 / 隐藏`、`退出 Metrik`。）

### 5. 完整视图（expanded）

- Rust 命令 `open_expanded_window`：创建/显示 `expanded` 窗口
  （`decorations: true`、1120×760、最小 960×700、可缩放、URL `index.html?view=expanded`），
  并 `set_activation_policy(Regular)`（进 Dock、可 Cmd-Tab）。
- 窗口销毁 → 切回 `Accessory`（退出 Dock），只剩菜单栏图标。
- "设置"菜单项走同一命令，附加 `&nav=settings`；前端从 URL 读初始导航项。
- 前端在 macOS 的 expanded 下隐藏自绘的 `WindowActions` 与 `expanded-drag-region`，
  避免和原生红绿灯打架。

### 6. 材质 / 透明

- `tauri.conf.json` 增加 `"macOSPrivateApi": true`（macOS 透明窗口的前提），
  Cargo 开 `macos-private-api` feature。
- macOS 也调用 `setBackgroundColor([0,0,0,0])` 让 webview 透明。
- vibrancy 只设**一个**材质：`hudWindow`（深色 HUD 材质，不随系统浅色模式变白），
  并用 Tauri 的 `radius` 参数给窗口圆角（12），与面板 CSS 圆角对齐。
- CSS：新增 `.widget-shell--mac` 变体，把 Windows 那层 `--glass-alpha: 0.82` 的近实心深色 tint
  降到低透明度薄层（系统材质负责暗度与模糊），描边/内高光保留。
  `platform-mac` 类由前端写在 `documentElement` 上。
- macOS 上 vibrancy 恒开，不再是可切换项。

## 依赖

- `tauri-nspanel = { git = "https://github.com/ahkohd/tauri-nspanel", branch = "v2" }`
  —— 仅 `cfg(target_os = "macos")`。不在 crates.io 上，CI 的 macOS 构建需要能拉 GitHub。
  上游若停更，Tauri 升级时可能需要自行维护。已知风险，接受。
- tauri feature 增加 `image-png`、`macos-private-api`。
- 不引入 `tauri-plugin-positioner`：托盘点击事件自带 `rect`，定位自己算即可。

## 验证

**开发机是 Windows，以下全部无法在本机验证**，只能由用户在 Mac 上 `npm run desktop:dev` 验收。
本机与 CI 能覆盖的部分：

- 本机：`cargo test`、`cargo clippy -D warnings`、`cargo fmt --check`、`npm run build`
  （Windows 路径不回归）。
- CI：macOS 构建与 clippy（能验证 macOS 分支能否编译、依赖能否拉取）。

用户在 Mac 上的验收清单：

1. 菜单栏图标是单色字形，浅色/深色菜单栏下都清晰，不是彩色方块。
2. 点图标 → 面板从图标正下方弹出；再点 → 收起。
3. 面板弹出时**当前 App 不失焦**（在编辑器里打字，弹出面板后仍能继续打字）。
4. 点面板外任意位置 → 面板自动收起。
5. 面板有真实的系统模糊（背后窗口内容可辨认的景深），不是一块实心深灰。
6. 面板里没有最小化/关闭/固定/玻璃四个按钮。
7. 点"完整视图" → 面板收起，弹出带原生红绿灯的标准窗口，可缩放，出现在 Dock 与 Cmd-Tab。
8. 关闭完整视图窗口 → 回到只有菜单栏图标的状态（Dock 里消失）。
9. 右键菜单栏图标 → 显示/隐藏、完整视图、设置、退出 四项均可用。
10. 面板可浮在全屏应用之上。

任何一条不过，回来调整；NSPanel 路线若整体走不通，退路是"无边框窗口 + 手动定位 + 失焦隐藏"
（放弃"不抢焦点"这一条），或改用 `tauri-plugin-nspopover`。
