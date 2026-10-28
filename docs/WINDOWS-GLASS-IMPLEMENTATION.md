# Windows 真透明玻璃实现方案

> **适用状态：Windows shell 正式实现**
>
> 本文描述已经通过 Windows 11 原生验证、随主线发布的实现。它是实现规范，
> 不是产品约束的替代品；`docs/PRODUCT-CONSTRAINTS.md` 已同步为真实窗口
> Alpha 方案。
>
> 调查过程、失败对照和参考项目分析见
> [`WINDOWS-GLASS-NOTES.md`](./WINDOWS-GLASS-NOTES.md)。

## 1. 目标与结论

Metrik 的卡片、横向胶囊和纵向胶囊需要同时满足：

- 能透出真实桌面、桌面图标和位于 Metrik 后方的窗口；
- 背景有稳定的模糊、冷白 tint、边缘高光和柔和阴影；
- 卡片与胶囊互相变形、三种组件外观循环切换时不闪白、不残留白底；
- 浓度、DPI、缩放、尺寸测量和边缘自动隐藏互不干扰；
- 减少透明度、减少动画和强制色彩模式下仍然可读；
- Windows 实现不改变 macOS 的系统 vibrancy 管线。

当前验证通过的核心方案是：

> **窗口在创建期一次性获得逐像素 Alpha，整个 Windows 小组件生命周期内不再
> 改变 HWND/WebView2 的合成策略；所有外观切换只改变 React 状态和 CSS。**

这不是“把面板背景调得更透明”，而是严格分开三个层次：

1. Tauri/Windows 提供透明窗口承载面；
2. WebView 根节点对窗口后方内容进行一次模糊和 tint；
3. 卡片内容层只画文字、控件、分隔线和小面积交互反馈。

## 2. 实现边界

### 2.1 属于本方案

- Windows compact 卡片；
- Windows horizontal strip；
- Windows vertical strip；
- `dark → light → clear → dark` 三种用户可见组件外观；
- clear 档的指针边缘流光；
- 浏览器预览和无真实 Alpha 环境的 CSS 回落；
- Windows 辅助功能媒体查询。

### 2.2 不属于本方案

- expanded 完整视图：它始终由自己的不透明主题负责；
- macOS 菜单栏面板：继续使用 `hudWindow` vibrancy；
- 旧壁纸裁剪、定位和高斯模糊后端；
- Mica、Acrylic、HostBackdrop 或运行期 DWM 材质切换；
- 通过降低整个窗口 opacity 制造“半透明”。

## 3. 合成架构

```text
Windows 桌面 / 后方窗口
          │
          ▼
创建期 transparent: true 的 HWND + WebView2
          │  运行期保持不变
          ▼
#root：唯一的大面积 backdrop-filter
          │
          ├─ dark：深色 shell tint
          ├─ light：浅色 shell tint
          └─ clear：#root 白霜（深色字）或薄暗罩（白色字）
          │
          ▼
两端 #root：圆角按物理像素折算（--glass-radius），不随缩放变大
浏览器 #root：额外补一层模拟静态描边；桌面端不画
两端 #root::after：边缘流光
          │
          ▼
widget/strip shell：透明内容布局、文字、图标、进度和小面积反馈
```

### 3.1 创建期透明窗口

[`src-tauri/tauri.conf.json`](../src-tauri/tauri.conf.json) 中主窗口固定：

```json
{
  "decorations": false,
  "transparent": true
}
```

`transparent: true` 必须在窗口创建时生效。它使 WebView2 的透明像素能够参与
Windows 的逐像素合成，是后续真实背景采样成立的前提。

### 3.2 不可变的 Windows 合成策略

[`src/windowClient.js`](../src/windowClient.js) 的 `setWindowGlass()` 在 Windows
分支只计算并返回外观模式，不调用下面这些运行期 API：

- `getCurrentWebview().setBackgroundColor(...)`；
- `appWindow.setEffects(...)`；
- `appWindow.clearEffects()`；
- HostBackdrop、Acrylic、Mica 或 SWCA 切换；
- DWM frame、blur region 或 system backdrop 切换。

[`src/glassAppearance.js`](../src/glassAppearance.js) 中
`resolveWindowsGlassComposition()` 把这个约束显式编码为：

```js
{
  mode: enabled ? "alpha" : "off",
  resetWebviewBackground: false,
  mutateNativeBackdrop: false,
}
```

这里的 `off` 只表示页面暂时不绘制玻璃外观，不表示把透明 HWND 改回不透明
HWND。它属于 expanded/能力回落的内部状态，不再作为第四种组件外观暴露。

这个约束是实现中最重要的防回归边界。Windows 的三种组件外观始终共用一条原生
合成管线。

## 4. 外观状态模型

### 4.1 用户状态

当前用户状态由两个持久化值组成：

| 状态 | localStorage | 含义 |
|---|---|---|
| 组件外观 | `metrik:glassTint` | `dark`、`light` 或 `clear` |
| 透明档文字 | `metrik:glassInk` | `dark` 或 `light`，只在 clear 下生效 |
| 玻璃浓度 | `metrik:glassAlpha` | `0.05–0.96`，默认 `0.82` |

卡片标题栏按钮按下面顺序循环：

```text
dark → light → clear → dark
```

旧版本的 `metrik:transparent=false` 会在首次加载时迁移为默认 `dark`，随后删除
这个旧开关。设置页可以直接选择组件外观和浓度。expanded 模式不应用这些 tint，
但保留设置，返回 compact 或 strip 时继续使用。

### 4.2 内部模式

`GLASS_MODES` 有四个值：

| 模式 | 使用位置 | 含义 |
|---|---|---|
| `off` | expanded 或内部回落 | 页面暂时不绘制玻璃外观，Windows 合成承载面不变 |
| `alpha` | Windows 桌面包 | 创建期透明窗口可用，真实采样窗口后方内容 |
| `css` | 浏览器或透明能力不可用 | 近实心 CSS 回落，不声称拥有真实 Alpha |
| `native` | macOS | 系统 vibrancy 生效 |

Windows 的 `dark`、`light`、`clear` 全部解析为 `alpha`，不能把 tint 与原生合成
模式做一一映射。

### 4.3 class 组合

`glassShellAppearance()` 是 compact 和 strip 共用的唯一 class 解析器：

| 外观 | 主要 class |
|---|---|
| dark alpha | `--transparent` |
| light alpha | `--transparent --glass-light` |
| clear alpha（深色字） | `--transparent --glass-light --glass-clear` |
| clear alpha（白色字） | `--transparent --glass-clear --glass-ink-light` |
| CSS fallback | 在对应组合上再加 `--glass-css` |
| macOS | 加 `--mac`，忽略 Windows 的 light/clear 存储值 |

clear 只覆盖材质层：shell 自身完全透明，白霜/暗罩和模糊都画在 `#root` 上。
前景整套沿用另外两档——深色字复用 `--glass-light` 那套规则，白色字复用透明 HUD
的白色体系（即不挂 `--glass-light`）。这两组文字色与罩层的配对不能拆开，
理由见 §6.3。

只有 Windows clear 且模式为 `alpha` 时，shell 才带：

```html
data-glass-surface="true-alpha"
```

这个属性用于诊断和 QA，不参与合成决策。

## 5. 启动与切换流程

### 5.1 冷启动

```text
Tauri 创建 transparent 窗口
        ↓
React 读取 glassTint / glassAlpha，并迁移旧 transparent 开关
        ↓
Windows 桌面环境直接把初始 glassMode 解析为 alpha
        ↓
glassShellAppearance 生成首帧 class 与 CSS 变量
        ↓
#root 进行一次背景模糊，shell 绘制 tint/边界/内容
```

桌面端初始状态直接解析为 `alpha`，避免先出现一帧 `--glass-css` 近实心回落，再
切换到真透明。

### 5.2 tint 切换

```text
用户点击外观按钮
        ↓
更新 glassTint 并写 localStorage
        ↓
setWindowGlass 保持返回 alpha
        ↓
React 只更新 class 与 CSS 变量
        ↓
不重置 WebView，不修改 DWM，不重建窗口
```

### 5.3 形态切换

compact、horizontal strip 和 vertical strip 共用相同的 alpha 策略。形态切换只
负责窗口尺寸、位置、WebView zoom 和布局，不负责切换玻璃后端。

进入 expanded 时：

- `setWindowGlass(false)` 在 Windows 只返回 `off`；
- expanded 的 `.app-shell` 绘制完整不透明背景；
- 原生窗口仍然保留创建期透明能力；
- 返回 compact/strip 后可以立即恢复任一 tint。

## 6. CSS 材质实现

### 6.1 透明基线

`html`、`body` 和 `#root` 都保持 `background: transparent`。浏览器运行时单独绘制
测试背景，桌面包不绘制这层背景。

### 6.2 唯一的大面积模糊层

Windows alpha 模式只允许 `#root` 承担全窗口模糊：

```css
-webkit-backdrop-filter: blur(24px) saturate(160%);
backdrop-filter: blur(24px) saturate(160%);
```

不能在标题栏、周期控件、额度卡片等大面积子节点再次使用
`backdrop-filter`。多层背景采样会显著增加 GPU 成本，也会把 tint 叠成乳白板。

clear 卡片中的 compact 周期控件明确设置 `backdrop-filter: none`，防止继承或以后
新增通用玻璃规则时形成第二层模糊。

### 6.3 三种 tint

#### dark

- 深蓝黑半透明渐变；
- 白色文字；
- 冷色顶部高光；
- 暗色外收边与浅色内高光；
- 在复杂浅色壁纸上仍保持稳定对比度。

#### light

- 霜白半透明渐变；
- 深色文字；
- 白色外边与轻微暗色内边；
- 子面板允许适量白色层次，保证内容区分。

#### clear

clear 的文字颜色由用户选（`metrik:glassInk`），**罩层跟着文字走**：

| 文字 | 罩层 | 浓度区间 |
|---|---|---|
| 深色字（默认） | 白霜 `rgb(229 237 246 / a)` | `clamp(0.38, 0.3 + a * 0.36, 0.72)`，胶囊 `clamp(0.34, 0.26 + a * 0.32, 0.62)` |
| 白色字 | 暗罩 `rgb(18 24 36 / a)` | `clamp(0.14, 0.1 + a * 0.34, 0.44)` |

**白字配白霜是唯一必坏的组合，不提供。** 往白底上再加白不会拉开对比度：实测
浅色壁纸上白字对比度 1.10:1，把霜从 0.28 加到 0.44 也只到 1.12:1。同一组测量里
深色字配 0.44 白霜在亮壁纸上是 14.4:1、在暗背景上 4.0:1。所以
`glassAppearance.js` 把文字色和罩层绑成一对，`glassAppearance.test.js` 有断言
守着，改一边必须改另一边。

两档共同的部分：

- shell 本身 `background: transparent`，白霜/暗罩和唯一的 `backdrop-filter`
  都在 `#root` 上；
- 大面积子面板改为透明，仅保留 inset 分隔边；
- hover、selected 和按钮只使用小面积半透明反馈；
- 文字带一层与自身相反的阴影拉开局部对比（深色字配浅色光晕，白色字配深色
  阴影）。**Blink 的 UA 样式给 `input`/`button` 硬写了 `text-shadow: none`**，
  外壳那层阴影传不进按钮，周期签/额度卡/刷新/统计说明都要单独补一条；坐在
  实心药丸上的选中态则要显式关掉阴影，并保持与药丸相反的字色；
- 次级文字比另外两档更重（`0.82` / 字重 500）：薄罩下壁纸的明暗直接透上来，
  浅色档那套 `0.58` 的次级色局部对比会塌到 1.8:1；
- 浏览器没有真实 Alpha，clear 让位给浅色档的近实心回落，不假装透明。

### 6.4 浓度映射

clear 直接使用用户的 `0.05–0.96` 原始值，由 CSS 各自映射到白霜或暗罩的区间
（见 §6.3）。dark/light 先线性映射到 `0.55–0.96`——**0.55 是白色文字的可读下限，
不是历史补偿**：白字压在 0.55 深底上、透出亮壁纸时对比度 4.2:1，降到 0.22 只剩
1.9:1。

```js
t = (glassAlpha - 0.05) / (0.96 - 0.05)
shellGlassAlpha = 0.55 + clamp(t, 0, 1) * (0.96 - 0.55)
```

最终值同时写入：

- shell 行内变量 `--glass-alpha`；
- document 根变量 `--shell-glass-alpha`。

前者控制 dark/light 的 shell tint，后者控制 clear 的根级冷白 tint。

## 7. 圆角、裁剪与尺寸稳定

不再通过 `SetWindowRgn` 裁切 HWND。自定义 region 在卡片与胶囊变形、缩放和内容
测量并发时可能短暂沿用旧区域，实际表现就是标题栏残片、左右白条或底部被错误
切圆；它不适合作为当前窗口结构的稳定边界。

**圆角由 `#root` 画，两端同一条规则，写成物理像素。** 文档此前写"桌面端由 DWM
拥有外框、`#root` 保持零圆角"是错的：那条 `border-radius: 0` 的选择器
（`html:has(…--glass-clear) #root`，特指度 1,1,1）一直被更靠前也更具体的
`html:has(…--transparent:not(--mac):not(--glass-css)) #root`（1,3,1）压着，
从未生效。

CSS px 会被卡片和胶囊各自的 WebView 原生 zoom 放大，所以同样写 `8px`：

| 形态 | zoom | dpr | 实际画出 |
|---|---|---|---|
| 卡片（缩放 0.95） | 0.95 | 1.1875 | 9.5 物理像素 |
| 竖胶囊（缩放 1.75） | 1.75 | 2.1875 | **17.5 物理像素**——即被否掉的"大圆头" |

`App.jsx` 按 `devicePixelRatio` 把 `GLASS_RADIUS_PX`（8 物理像素，定义在 `appearanceOptions.js`）折算成
`--glass-radius`，`resize` 时重算。两种形态、任何缩放档都是同一个视觉半径。

静态外框只在浏览器补（`html[data-runtime="browser"]`）；桌面端不画，避免与
窗口自身的轮廓形成同心双框。尺寸切换、内容测量和 DPI 重断言只负责窗口尺寸，
不再同步第二套裁剪状态。

静态外框不使用普通 `border`，而使用 inset `box-shadow`：

```css
box-shadow: inset 0 0 0 1px rgba(...);
```

原因是 strip 尺寸由渲染内容测量。普通边框会改变 `clientWidth/clientHeight`，
可能让 ResizeObserver、原生 resize 与 WebView zoom 反复追逐 1–2 像素。

### 7.1 让 CSS 真正成为唯一轮廓

光靠"只有 CSS 画圆角"不够。窗口自身还有两处会画外轮廓，都要显式关掉，否则四角
外面会浮出第二层底——暗色档尤其明显。

**一、`"shadow": false`。** `decorations: false` + `shadow: true` 时，tao 走
`undecorated_with_shadows` 路径：`WM_NCCALCSIZE` 主动把客户区往里缩，空出一圈非
客户区给 DWM 画阴影。这圈区域不归我们画，DWM 会往里填框架色。实测 125% 缩放下：

| | window | client | 缩进 |
|---|---|---|---|
| `shadow: true` | 98×784 | 84×776 | 14 横 / 8 竖 |
| `shadow: false` | 380×504 | 380×504 | 0 |

代价是没有落影。想补的话只能自己画：给窗口留一圈透明外边距再上 CSS `box-shadow`，
而窗口尺寸是内容测量驱动的，改这里要连带复核 strip 的尺寸收敛（见第 9 节）。

**二、`DWMWCP_DONOTROUND`。** tao 的 `to_window_styles()` 无条件加 `WS_CAPTION`
（`decorations: false` 只在 `to_adjusted_window_styles()` 里剥，那个只用于算尺寸），
所以无边框窗口照样被 Win11 DWM 圆角并描边，而 tao / wry / tauri 三层都没设过这个
属性。DWM 的半径跟系统 DPI 走，`--glass-radius` 按物理像素钉死，不存在能重合的缩放
档。`lib.rs` 的 `disable_system_corner_rounding` 在 setup 里关掉它。

**不要改用 `SetWindowRgn`。** GDI region 是二值边界，压在 per-pixel alpha 上是硬
锯齿；而且它在变形与内容测量并发时会短暂沿用旧区域（本节开头那段）。pogget 那套
"DONOTROUND + region + 同半径 Border 描边盖锯齿"能成立，前提是它
`AllowsTransparency = false`——不透明窗口没有 alpha 要抗锯齿。我们有 alpha，用不着
它，也承受不起它的代价。

## 8. 指针边缘流光

clear shell 监听 `pointermove`，按 shell 的 `getBoundingClientRect()` 把指针坐标
归一化到 `0–100%`：

```text
--glass-pointer-x
--glass-pointer-y
--glass-edge-opacity
```

事件处理器直接把变量写到父级 `#root`，不写 React state，因此指针移动不会让整个
widget 重新渲染。

`#root::after` 绘制一个以指针为中心、半径约 `96px` 的蓝白径向渐变。双层
mask 只保留 `1px` 边环：

```text
完整伪元素
  − content-box 内部
  = 只剩边缘高光
```

实现约束：

- `pointer-events: none`：不拦截按钮和拖动区；
- `position: absolute`：不参与布局；
- `isolation: isolate`：高光不能逃出当前 shell 的堆叠上下文；
- 离开 shell 时只把 opacity 降为 0；
- 不使用 React 动画循环或 canvas；
- strip 测量结果不受影响。

## 9. 回落与辅助功能

### 9.1 浏览器/CSS fallback

浏览器没有透明 HWND，也不能证明真实外部背景采样。此时模式为 `css`：

- 增加 `--glass-css`；
- dark/light 使用约 `0.95–0.96` 的近实心背景；
- clear 的材质规则整条带 `:not(--glass-css)`，于是让位给浅色档的近实心回落，
  不假装透明；边缘交互仍可预览；
- 不设置 `data-glass-surface="true-alpha"`。

浏览器预览适合检查 class、尺寸和交互，**不能用来验收圆角**：桌面端 `#root`
的半径按物理像素折算（§7），浏览器补的是另一套模拟外框，两边量到的值本就
不同。真透明同样只能在 Windows 桌面窗口上验收。

### 9.2 减少透明度

`prefers-reduced-transparency: reduce` 下：

- 关闭 `#root` backdrop-filter；
- clear（深色字）与 light 回落为约 `0.98` 的霜白实色；
- clear（白色字）与 dark 回落为约 `0.98` 的深色——文字色决定回落到哪一侧，
  靠的是 clear 深色字挂着 `--glass-light`、白色字不挂；
- 关闭边缘流光。

### 9.3 减少动画

`prefers-reduced-motion: reduce` 下：

- 边缘流光不做 180ms 淡入淡出；
- 全局动画和过渡压缩到近乎即时。

### 9.4 强制色彩

`forced-colors: active` 下：

- 根层与 shell 使用 `Canvas` / `CanvasText`；
- 关闭 backdrop-filter 和边缘流光；
- compact 使用系统色边框；
- strip 使用内缩 outline，不增加测量尺寸。

## 10. 已失败方案与原因

### 10.1 运行期重置 WebView2 透明背景

失败操作：

```js
getCurrentWebview().setBackgroundColor([0, 0, 0, 0])
```

在当前 Tauri/WebView2/Windows 11 组合中，这会让 `backdrop-filter` 采样到不透明
白色重定向表面。调用看起来是在“确保透明”，实际破坏了创建期已经正常的 Alpha。

### 10.2 在多种 DWM 材质间切换

HostBackdrop、Acrylic、Mica 与 clear 不是可靠可逆的皮肤状态。窗口切过系统材质
后，即使清理 API 返回成功，也可能继续保留白色底板或旧重定向表面。

因此 Windows 日常状态机不能把每个 tint 映射到一种原生材质。

### 10.3 壁纸裁剪模拟透明

旧流程：

```text
读取壁纸 → 按显示器裁剪 → 高斯模糊 → 按窗口坐标负偏移
```

它的问题是：

- 无法透出桌面图标和后方窗口；
- 壁纸布局、DPI、多显示器和幻灯片切换会产生偏移；
- 窗口移动需要持续同步；
- 后端缓存和失效策略复杂；
- 很容易用乳白 tint 掩盖真实 Alpha 是否成功。

旧后端暂时保留用于回退研究，但当前前端不能重新接入主路径。

### 10.4 多层半透明面板

根 tint、标题栏、指标卡、额度卡同时铺白色透明背景，会产生“奶白塑料板”，背景
细节和层次都被累积 opacity 吃掉。clear 必须坚持单大面材质，小面积交互反馈。

### 10.5 用边框参与 strip 尺寸

strip 的普通 `1px border` 会改变测量尺寸，造成 ResizeObserver 与原生窗口尺寸
反复修正。边线必须使用 inset shadow 或内缩 outline。

## 11. 文件职责

| 文件 | 职责 |
|---|---|
| `src-tauri/tauri.conf.json` | 创建期透明窗口能力 |
| `src/glassAppearance.js` | 模式解析、class 组合、Windows 不可变合成决策 |
| `src/glassAppearance.test.js` | 状态转换与 class 防回归测试 |
| `src/windowClient.js` | 平台分流；Windows 不执行运行期合成变更 |
| `src/App.jsx` | 用户状态、持久化、三态循环、旧 off 迁移、浓度映射、圆角物理像素折算 |
| `src/widgets.jsx` | 玻璃指针变量 |
| `src/appearanceOptions.js` | 主题、玻璃浓度与字色选项及其归一化 |
| `src/styles.css` | 模糊、tint、圆角、层次、流光和辅助功能回落 |
| `docs/WINDOWS-GLASS-NOTES.md` | 调查记录、参考项目和失败对照 |

## 12. 自动化防回归

`src/glassAppearance.test.js` 当前覆盖：

1. Windows 三种 tint 都解析为同一个 `alpha` 模式；
2. 用户按钮严格循环 `dark → light → clear → dark`，旧 `off` 回到 `dark`；
3. 内部 `alpha → off → alpha` 转换不重置 WebView；
4. 同一转换不修改 native backdrop；
5. Windows alpha class 不误挂 `--glass-css`；
6. clear 的两种文字色各自绑定能承载它的罩层：深色字必须挂 `--glass-light`，
   白色字必须不挂——这条守着 §6.3 那个必坏组合；
7. 文字选项只作用于 clear，dark/light 不受影响；
8. compact 与 strip 共用 true-alpha 外观；
9. clear 不再生成任何 `--wall-*` 壁纸变量；
10. 浏览器 clear 保留交互但不声称 true alpha；
11. macOS 忽略 Windows 持久化的 clear tint。

基础检查：

```powershell
npm test
npm run build
Set-Location src-tauri
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

## 13. Windows 原生验收矩阵

每次修改合成、根背景、圆角或窗口切换代码后，至少验证：

| 场景 | 通过标准 |
|---|---|
| 冷启动 clear compact | 桌面、图标和后方窗口可见，无白板 |
| dark → light → clear → dark | 三种外观依次切换，无第四种“不透明” |
| clear 的深色字 ↔ 白色字 | 罩层跟着翻面；选中的周期签始终与药丸反色 |
| compact → horizontal strip → compact | 无闪白、无尺寸振荡 |
| compact → vertical strip → compact | 圆角和裁剪正确 |
| 卡片缩放 75%、胶囊缩放 175% | 两种形态的圆角物理半径一致，不随缩放变大 |
| clear → expanded → clear | expanded 不透明，返回后透明恢复 |
| 调整浓度 5% → 96% | 连续变化，无原生材质重建 |
| 指针靠近各边 | 高光跟随且只在边环内 |
| 指针离开 | 高光淡出，不阻挡自动隐藏 |
| 100%、125%、150% DPI | 无方角 tint、无 1px 测量循环 |
| 多显示器移动 | 背景无需坐标同步，窗口尺寸仍正确 |
| 减少透明度/动画 | 回落可读、无动态流光 |
| 强制色彩 | 使用系统色，strip 尺寸不变 |

原生证据必须来自 Windows 桌面窗口截图。浏览器背景图不能证明逐像素 Alpha。

## 14. 开发环境故障排查

### 14.1 完整视图突然“消失”

透明窗口在前端未加载时会直接露出后方应用，看起来像窗口不存在。开发环境中先检查：

1. 是否通过 `npm run desktop:dev` 启动完整 Tauri + Vite 链路；
2. `http://127.0.0.1:1420/` 是否仍能访问；
3. 是否只剩单独启动的 `target/debug/metrik.exe`；
4. 原生窗口尺寸与 React `viewMode` 是否因 HMR 暂时不同步。

不要把“透明空窗口”立即判断为玻璃 CSS 失败。先恢复 Vite，再做一次完整页面重载，
确认 expanded 页面已经渲染。

### 14.2 clear 变成白板

按优先级检查：

1. 是否新增了 `setBackgroundColor([0,0,0,0])`；
2. 是否在 Windows 路径调用了 `setEffects()` / `clearEffects()`；
3. 是否重新接入了 DWM/HostBackdrop/Acrylic 切换；
4. `#root` 是否仍为透明文档上的唯一 blur plane；
5. shell 是否误挂 `--glass-css`；
6. 是否有大面积子面板重新叠加白色 tint。

### 14.3 四角或边缘漏出方形色块

先量窗口，再查 CSS。这个现象绝大多数时候不是 CSS 画错，而是窗口自己多画了一层
（第 7.1 节）：

- `GetWindowRect` 与 `GetClientRect` 不相等 → `"shadow"` 被改回了 `true`；
- 两者相等但角上仍有一圈弧或描边 → `disable_system_corner_rounding` 没跑到。

排除这两条之后再看 CSS：静态描边不得加回桌面 shell，浏览器模拟圆角必须继续限制在
`html[data-runtime="browser"]`；动态流光可以画在 `#root`。Windows 端不得引入
`SetWindowRgn`。

## 15. 修改守则

以后调整玻璃参数时遵守以下顺序：

1. 先只改 CSS 数值：tint、blur、saturate、阴影或边缘渐变；
2. 浏览器验证布局、class、圆角和交互；
3. Windows 原生验证真实 Alpha；
4. 跑状态转换测试和基础构建检查；
5. 最后才考虑合成层变更。

任何需要新增 Windows 原生 backdrop API 的方案，都必须先回答：

- 为什么 CSS 层无法完成；
- 是否会破坏三种外观循环或内部 `alpha → off → alpha`；
- 如何证明 API 调用完全可逆；
- 冷启动、形态切换和多 DPI 下是否都有原生证据；
- 失败时能否安全回到当前不可变 Alpha 管线。

在这些问题没有原生验证前，不应修改当前 Windows 合成边界。
