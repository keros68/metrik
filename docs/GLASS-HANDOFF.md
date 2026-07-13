# 玻璃材质问题交接分析（供外部协作者）

写于 2026-07-13。Metrik 小插件的「毛玻璃」外观在多轮尝试后，开发环境实测与用户安装版观感仍不一致。本文件完整记录事实、已试方案、证据与待验证假设，供接手者继续排查。

## 1. 目标

320×320 的 Tauri 桌面小插件（`transparent: true`、`decorations: false`）要有 CodexBar（macOS）那种**高透、内容感知的毛玻璃**：桌面/背后窗口的颜色透进来并被模糊，亮色系统主题下面板发亮通透，深色主题下是深色玻璃。

## 2. 环境事实（用户机器，均已实测确认）

- Windows 11 Home China，**Insider 版本 10.0.26220**（这是关键变量，见 §5）
- 显示缩放 130.625%（scaleFactor 1.30625），屏幕 2560×1440 物理像素
- 系统浅色主题（`AppsUseLightTheme=1`、`SystemUsesLightTheme=1`）
- **透明效果已开启**（注册表 `HKCU\...\Themes\Personalize\EnableTransparency = 1`）
- Tauri 2.11.5 + WebView2；窗口配置 `transparent: true, decorations: false, shadow: true`

## 3. 相关代码位置

| 文件 | 内容 |
| --- | --- |
| `src-tauri/src/lib.rs` → `mod swca` + `set_glass_backdrop` 命令 | 当前方案：运行时 `GetProcAddress` 解析未文档化的 `user32!SetWindowCompositionAttribute`，用 `ACCENT_ENABLE_ACRYLICBLURBEHIND`(=4)、`accent_flags=2`、tint 为 AABBGGRR |
| `src/windowClient.js` → `setWindowGlass` / `glassTint` | Windows 调 `set_glass_backdrop`（亮主题 tint `[252,251,250,150]`，暗主题 `[24,26,32,170]`）；macOS 走 `setEffects(["popover","hudWindow","blur"])`。系统主题变化时由 `App.jsx` 的 matchMedia 监听重发 |
| `src/styles.css` → `.widget-shell--transparent` 系列 | 前端分层：亮玻璃=白色低 alpha 洗层（壳 `rgba(255,255,255,0.05)` + 高光渐变 0.32，卡片 0.3–0.34）；深玻璃在 `@media (prefers-color-scheme: dark)` 内（壳 `rgba(22,25,31,0.4)` + 白字 0.94/0.58/0.1 阶梯） |
| `src/App.jsx` | 玻璃开关（仅紧凑态）、默认开启（localStorage `metrik:transparent` 默认 true） |

## 4. 已试方案与结果

### 方案 A：Tauri `setEffects(["acrylic"])`（= window-vibrancy）
- Win11 build ≥22523 时 window-vibrancy 的 `apply_acrylic` 走 **DWM 路径**（`DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_TRANSIENTWINDOW)`），**完全忽略 tint 参数**（tint 只在老系统 SWCA 回退路径生效）——源码确认：window-vibrancy `src/windows.rs` `apply_acrylic`
- DWM Acrylic 的明暗跟随窗口主题；浅色主题下是乳白偏灰的材质，叠深色 CSS 层 → 用户反馈「灰蒙蒙」。改亮色 CSS 后观感仍被用户否定（整体发白发灰、看不出透）

### 方案 B：DWM Acrylic + 亮色 CSS 洗层
- 同上路径，只改前端。对比度可接受但「玻璃感」弱：模糊半径小、无色彩渗透。用户否定

### 方案 C（当前）：SWCA `ACCENT_ENABLE_ACRYLICBLURBEHIND` + 自定义 tint
- 直接调 `SetWindowCompositionAttribute`（该导出不在 user32 导入库，必须 `LoadLibraryA+GetProcAddress`；直接 `#[link]` 会 LNK2019）
- **开发环境实测（tauri dev，同一台机器）：有效**——截图可见窗口后深色终端的颜色透过玻璃渗出（左上暗、右下亮的内容感知渐变）。证据截图曾保存于会话 scratchpad `swca-glass2.png`
- **用户安装 release 版后反馈「还是没实现」**，其截图中面板呈均匀奶油色、无可见透色

## 5. 核心谜团与假设（按可能性排序）

**谜团：同一台机器，dev 运行有透色，安装版没有。**

1. **效果过于微妙**：tint alpha 150/255 + CSS 白洗层（高光 0.32）叠加后，在浅色壁纸上透色几乎不可辨。dev 截图里能看到是因为窗后恰好是**深色终端**。→ 快速验证：把安装版窗口拖到深色窗口/壁纸前看是否有渗色。若有，问题变成「参数调校」：降 tint alpha（150→90）、去掉 CSS 高光渐变，透感会强很多
2. **Insider 26220 上 SWCA 行为变化**：SWCA 是未文档化 API，微软在 Insider 版本多次改动过（曾出现 ACRYLICBLURBEHIND 变纯色/黑色/失效）。dev 与 release 的窗口创建路径有差异（dev 有 devtools/调试标志），可能触发不同代码路径。→ 验证：写 10 行的独立 Win32 测试程序在该机器上试 SWCA
3. **时序问题**：release 版在窗口完全创建/显示前调用 SWCA 可能被 DWM 忽略；dev 因加载慢时序不同。→ 验证：延迟 1s 重发 `set_glass_backdrop`，或在窗口 focus 事件后重发
4. **WS_EX_NOREDIRECTIONBITMAP / WebView2 合成**：WebView2 的视觉层合成与 SWCA 有已知冲突场景。→ 验证：对一个空白 Tauri 窗口（无 webview 内容差异）分别在 dev/release 试
5. **两条路径叠加冲突**：历史会话里曾经先 `setEffects`（DWM backdrop）后 SWCA；DWM backdrop 未清除时可能覆盖 SWCA 效果。当前代码只走 SWCA，但**用户机器的窗口可能残留了上次 DWM backdrop 状态**（同一 app identifier，窗口状态不持久，理论不该残留——但值得在 `set_glass_backdrop` 里先显式 `DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_DISABLE)` 再开 SWCA）

### 可以直接抄的参考实现
- **token-monitor（Electron）**：Windows 用 `backgroundMaterial: 'acrylic'`（= DWM Acrylic）+ `transparent: false` + CSS `backdrop-filter: blur(32px) saturate(115%)`、glass alpha 0.68。注意 Electron 的方案里 **CSS backdrop-filter 只模糊页面内内容**，真正的桌面模糊仍靠 DWM——它好看的关键是**深色底 + saturate + 用户可调**
- **codexU（Swift）**：macOS `NSVisualEffectView(.hudWindow)`/`NSGlassEffectView`，Windows 无解可抄
- 若 SWCA 死路：备选是 `Windows.UI.Composition` 的 `CreateHostBackdropBrush`（需要 DirectComposition 互操作，工程量大），或接受 DWM Acrylic 但把窗口主题强制深色（`DWMWA_USE_IMMERSIVE_DARK_MODE=1`）让 DWM 出深色磨砂，配深色 CSS——这是 Win11 上最稳的组合，代价是亮色主题下也是深色玻璃

## 6. 复现与验证方法

```bash
npm run desktop:dev        # 开发运行（读真实日志）
# 打包：npm run desktop:build → src-tauri/target/release/bundle/nsis/
```
- 给 WebView2 开调试口后可用 CDP 驱动应用：`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9223 npm run desktop:dev`，然后 `window.__TAURI_INTERNALS__.invoke('set_glass_backdrop', {enabled:true, tint:[252,251,250,90]})` 可在线调参数——**这是最快的调参路径**
- 截图对比务必让窗口前后各压一个深色/彩色窗口，浅色壁纸上任何方案都看不出透

## 7. 其他已知遗留问题（与玻璃无关）

- ~~紧凑态总用量数字超宽（如 103.20M 溢出省略号）~~ 已修：`compactTokens` 改为位数自适应（≥100 无小数）
- 30 天周期首次扫描需数分钟（整文件重扫，无追加游标）；扫描期间界面显示旧周期数据并带「正在统计…」提示
- Claude 的模型专属周限（官方 Usage 页的 "Fable 48%"）**不在** Claude Code statusLine 推送里，钩子拿不到；要它需读登录凭据调 API（项目红线，未做）。钩子已做通用捕获，官方将来推送即自动显示
- ZCode/GLM 官方配额需 bigmodel API Key（本地无缓存），未实现
- macOS/Linux 全部特性未实机验收；SWCA 是 Windows 专属，mac 走 `setEffects` popover 材质（未验证）
