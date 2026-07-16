# 胶囊条（strip）第三形态实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 Metrik 增加第三形态"胶囊条"：一条约 40px 高的置顶小横条，每个有官方配额的 agent 一格（logo + 剩余百分比 + 微型进度条），作为 compact 的折叠态（strip ⇄ compact → expanded）。

**Architecture:** 复用现有主窗口变形机制（`applyWindowMode`）。前端三个文件改动：`src/windowClient.js`（strip 窗口尺寸/独立位置记忆）、`src/App.jsx`（形态状态、StripBar 组件、切换按钮）、`src/styles.css`（胶囊样式）。后端零改动。仅 Windows/Linux 生效，macOS 分支照旧全部跳过。

**Tech Stack:** React 19 + Tauri 2 window API。项目**没有前端测试框架**（无 vitest/jest），验证手段 = `npm run build`（编译通过）+ Windows 实机 `npm run desktop:dev` 手动验收。不为本任务引入测试框架。

**Spec:** `docs/superpowers/specs/2026-07-16-strip-mode-design.md`

## Global Constraints

- 官方配额与本地解析用量是两类事实，胶囊条**只显示官方配额**，无数据显示"配额不可用"，绝不填零值或演示数字。
- 陈旧数据 0.55 透明度 + tooltip 标注更新时间。
- 悬停详情用原生 `title` tooltip（窗口 40px 高，自绘弹层会被裁剪）。
- 回 compact 用末端按钮，**不依赖双击**（drag-region 吞双击）。
- strip 位置记忆用独立 key `metrik:stripPosition`；形态记忆 key `metrik:viewMode`。
- macOS（`IS_MAC` / `isMacPlatform()`）路径不改行为。
- 提交前逐文件看 diff，只 add 自己改过的文件（工作树可能有并行改动）。

---

### Task 1: windowClient.js — strip 窗口模式与按形态记位

**Files:**
- Modify: `src/windowClient.js`

**Interfaces:**
- Produces: `WINDOW_SIZES.strip`；`applyWindowMode("strip", { width })`；`resizeStripWindow(width)`（新导出）；`restoreWindowPosition(mode = "compact")`（签名加参数）；`startPositionMemory(getMode)` 在 strip 形态下也记位（写 `metrik:stripPosition`）。

- [ ] **Step 1: 尺寸表与按形态位置存储**

`WINDOW_SIZES` 增加 strip；把单一 `compactPosition`/`POSITION_KEY` 泛化为按形态的表（compact 键名不变，兼容既有数据）：

```js
const WINDOW_SIZES = {
  compact: { width: 320, height: 320, minWidth: 320, minHeight: 320 },
  expanded: { width: 1120, height: 760, minWidth: 960, minHeight: 700 },
  strip: { width: 240, height: 40, minWidth: 168, minHeight: 40 },
};

// compact 与 strip 各自记位，互不覆盖；expanded 不记位。
const POSITION_KEYS = {
  compact: "metrik:widgetPosition",
  strip: "metrik:stripPosition",
};

const lastPositions = { compact: null, strip: null };
```

`readStoredPosition(mode)`、`rememberWindowPosition(api, appWindow, mode)`（原 `rememberCompactPosition`，写对应 key 与 `lastPositions[mode]`）、`restoreWindowPosition(mode = "compact")`（读对应 key，屏幕校验逻辑不变，回退尺寸用 `WINDOW_SIZES[mode]`）、`startPositionMemory(getMode)` 中 `if (!POSITION_KEYS[getMode()]) return;` 后按当前形态记录。原 `compactPosition` 的所有读写点改为 `lastPositions.compact`（expanded 分支里捕获展开前位置的那一处也是）。

- [ ] **Step 2: applyWindowMode 增加 strip 分支 + resizeStripWindow**

在 `applyWindowMode(mode, options = {})` 的 expanded 分支之后、compact 逻辑之前插入：

```js
  if (mode === "strip") {
    if (await appWindow.isMaximized().catch(() => false)) {
      await appWindow.unmaximize();
    }
    await appWindow.hide().catch(() => {});
    await appWindow.setSkipTaskbar(true).catch(() => {});
    await invoke("set_taskbar_button", { visible: false }).catch(() => {});
    await appWindow.setMinSize(null);
    await appWindow.setMaximizable(false);
    const width = Math.max(size.minWidth, Math.round(options.width || size.width));
    await appWindow.setSize(new api.LogicalSize(width, size.height));
    await appWindow.setResizable(false);
    // 有记忆位置回记忆位置；首次进入保持当前位置（出现在卡片原地）。
    const stored = readStoredPosition("strip");
    const target =
      lastPositions.strip || (stored ? new api.PhysicalPosition(stored.x, stored.y) : null);
    if (target) await appWindow.setPosition(target).catch(() => {});
    await appWindow.show().catch(() => {});
    await appWindow.setFocus().catch(() => {});
    return;
  }
```

新增（用于 agent 格数变化时无闪烁调宽）并加入导出表：

```js
/// 胶囊条格数变化时只调宽度，不走 hide/show，避免闪烁。
async function resizeStripWindow(width) {
  const api = await windowApi();
  if (!api) return;
  const size = WINDOW_SIZES.strip;
  const target = Math.max(size.minWidth, Math.round(width));
  await api
    .getCurrentWindow()
    .setSize(new api.LogicalSize(target, size.height))
    .catch(() => {});
}
```

- [ ] **Step 3: 构建验证**

Run: `npm run build`
Expected: 编译通过无报错。

- [ ] **Step 4: Commit**

```bash
git add src/windowClient.js
git commit -m "Add strip window mode and per-mode position memory"
```

---

### Task 2: App.jsx — 形态状态、恢复与效果接线

**Files:**
- Modify: `src/App.jsx`（App 组件、`initialWindowMode`、刷新间隔、挂载效果）

**Interfaces:**
- Consumes: Task 1 的 `applyWindowMode("strip", { width })`、`resizeStripWindow`、`restoreWindowPosition(mode)`。
- Produces: `stripAgents`（有官方配额数据的 agent id 数组）、`stripWindowWidth(count)`、`handleWindowMode("strip")` 可用；渲染分支调用 Task 3 的 `<StripBar />`。

- [ ] **Step 1: 启动形态恢复**

```js
function initialWindowMode() {
  if (typeof window === "undefined") return "compact";
  if (new URLSearchParams(window.location.search).get("view") === "expanded") return "expanded";
  // 上次收成胶囊条则恢复；expanded 不恢复。
  return localStorage.getItem("metrik:viewMode") === "strip" ? "strip" : "compact";
}
```

- [ ] **Step 2: 宽度常量与 stripAgents**

模块级（`AGENT_ORDER` 附近）：

```js
// 胶囊条一格约 68px（图标 + 百分比 + 微型进度条），两端留白 + 回卡片按钮约 48px。
const STRIP_CELL_WIDTH = 68;
const STRIP_CHROME_WIDTH = 48;

function stripWindowWidth(count) {
  return STRIP_CHROME_WIDTH + STRIP_CELL_WIDTH * Math.max(1, count);
}
```

App 内（`quotaAgents` memo 旁）：

```js
  // 胶囊条只上有官方配额数据的 agent；两类事实不混排，无数据就是无数据。
  const stripAgents = useMemo(
    () =>
      (snapshot.agentQuotas || [])
        .filter(quotaHasData)
        .map((entry) => entry.agent)
        .filter((agent) => AGENT_META[agent]),
    [snapshot],
  );
```

- [ ] **Step 3: handleWindowMode 与窗口效果**

`handleWindowMode`：记住 compact/strip 选择；strip 的窗口变形交给专门 effect（同时覆盖启动恢复与格数变化）：

```js
  const handleWindowMode = useCallback((nextMode) => {
    // macOS：完整视图是另一个窗口，面板只负责把它开出来，自己保持原样。
    if (IS_MAC) {
      if (nextMode === "expanded") runWindowAction(() => openExpandedWindow());
      return;
    }
    setViewMode(nextMode);
    if (nextMode === "compact") setActiveNav("overview");
    if (nextMode !== "expanded") localStorage.setItem("metrik:viewMode", nextMode);
    // strip 的变形由 strip 专属 effect 统一处理（含启动恢复与格数变宽）。
    if (nextMode !== "strip") runWindowAction(() => applyWindowMode(nextMode));
  }, []);
```

strip 专属 effect（放在边缘挂靠 effect 之后）：

```js
  // 进入 strip 时整窗变形一次；之后 agent 格数变化只调宽度。
  const stripApplied = useRef(false);
  useEffect(() => {
    if (IS_MAC) return;
    if (viewMode !== "strip") {
      stripApplied.current = false;
      return;
    }
    const width = stripWindowWidth(stripAgents.length);
    if (stripApplied.current) {
      runWindowAction(() => resizeStripWindow(width));
    } else {
      stripApplied.current = true;
      runWindowAction(() => applyWindowMode("strip", { width }));
    }
  }, [viewMode, stripAgents.length]);
```

挂载效果（原 2314 行处）只在初始形态是 compact 时恢复坐标（strip 由上面 effect 定位）：

```js
  useEffect(() => {
    // macOS 面板由系统管层级和位置：不置顶、不恢复坐标。
    if (IS_MAC) return;
    if (pinned) runWindowAction(() => setWindowPinned(true));
    // 小组件回到上次摆放的位置（含固定位置），坐标已不在任何屏幕上时居中。
    // strip 形态的启动定位在 strip 专属 effect 里做。
    if (viewMode === "compact") runWindowAction(() => restoreWindowPosition("compact"));
  }, []);
```

刷新间隔：strip 与 compact 同档（300s）：

```js
    const refreshEvery = indexing ? 400 : viewMode === "expanded" ? 60_000 : 300_000;
```

玻璃效果 hook（`setWindowGlass(transparent && viewMode === "compact")`）**不改**：strip 不启用原生方形背板（会从胶囊圆角外露出来），胶囊外观由 CSS 近实心玻璃承担。

需要新导入：`resizeStripWindow`（加入现有 windowClient import 列表）。

- [ ] **Step 4: 渲染分支（暂用占位，Task 3 换成真组件）**

在 `if (viewMode === "compact")` 之前：

```jsx
  if (viewMode === "strip") {
    return (
      <StripBar
        snapshot={snapshot}
        agents={stripAgents}
        pinned={pinned}
        loading={appBusy}
        onRestore={() => handleWindowMode("compact")}
      />
    );
  }
```

Task 2 与 Task 3 同一提交落地（`StripBar` 未定义前 build 不过），此步只把接线写好。

---

### Task 3: StripBar 组件、compact 折叠按钮与样式

**Files:**
- Modify: `src/App.jsx`（新增 `stripCellData`/`stripTooltip`/`StripBar`，`WindowActions` 加折叠按钮）
- Modify: `src/styles.css`（新增 `.strip-*` 样式块）

**Interfaces:**
- Consumes: 既有 helper `agentQuotaFor`、`quotaHasData`、`quotaUsedPercent`、`quotaSeverity`、`shortWindowLabel`、`formatReset`、`formatQuotaAge`、`AGENT_META`；Task 2 的 props 约定 `{ snapshot, agents, pinned, loading, onRestore }`。
- Produces: `StripBar` 组件；`WindowActions` 在 compact 形态多一个"折叠为胶囊条"按钮（走已有 `onToggleMode("strip")`）。

- [ ] **Step 1: 数据 helper（放在 `compactQuotaWindows` 附近）**

```js
// 胶囊条一格取"最紧张"窗口：可用窗口里已用百分比最高者。
function stripCellData(entry) {
  const windows = (entry.windows || []).filter(
    (window) => window.view.available && !window.view.resetExpired,
  );
  if (!windows.length) return null;
  const tightest = windows.reduce(
    (worst, window) =>
      quotaUsedPercent(window.view) > quotaUsedPercent(worst.view) ? window : worst,
    windows[0],
  );
  return { tightest, windows };
}

// 原生 title tooltip：列出全部窗口的剩余与重置倒计时；快照数据标注更新时间。
function stripTooltip(agentId, windows) {
  const lines = windows.map((window) => {
    const view = window.view;
    const reset = Number.isFinite(view.resetsInMinutes)
      ? ` · ${formatReset(view.resetsInMinutes)}后重置`
      : "";
    return `${window.label || shortWindowLabel(window.key)}：剩余 ${Math.round(view.remainingPercent)}%${reset}`;
  });
  const first = windows[0].view;
  const head =
    first.stale || first.quality === "official_snapshot"
      ? `${AGENT_META[agentId].label}（官方快照 · ${formatQuotaAge(first.ageMinutes)}）`
      : AGENT_META[agentId].label;
  return [head, ...lines].join("\n");
}
```

- [ ] **Step 2: StripBar 组件（放在 `CompactWidget` 之前）**

```jsx
function StripBar({ snapshot, agents, pinned, loading, onRestore }) {
  const cells = agents
    .map((agentId) => ({ agentId, cell: stripCellData(agentQuotaFor(snapshot, agentId)) }))
    .filter((item) => item.cell);
  const dragProps = pinned ? {} : { "data-tauri-drag-region": true };
  return (
    <main className="strip-shell" {...dragProps} style={pinned ? { cursor: "default" } : undefined}>
      <h1 className="sr-only">Metrik 官方配额胶囊条</h1>
      {cells.length ? (
        cells.map(({ agentId, cell }) => {
          const meta = AGENT_META[agentId];
          const view = cell.tightest.view;
          const severity = quotaSeverity(view);
          const isSnapshot = view.stale || view.quality === "official_snapshot";
          return (
            <div
              key={agentId}
              className={`strip-cell ${isSnapshot ? "strip-cell--stale" : ""} ${severity ? `strip-cell--${severity}` : ""}`}
              style={{ "--quota-accent": meta.accent }}
              title={stripTooltip(agentId, cell.windows)}
              {...dragProps}
            >
              <img className="strip-cell-icon" src={meta.iconSrc} alt={meta.label} draggable={false} />
              <span className="strip-cell-body">
                <em>{Math.round(view.remainingPercent)}%</em>
                <span className="strip-cell-track" aria-hidden="true">
                  <i
                    style={{
                      transform: `scaleX(${Math.max(0, Math.min(1, view.remainingPercent / 100))})`,
                    }}
                  />
                </span>
              </span>
            </div>
          );
        })
      ) : (
        <span className="strip-empty" {...dragProps}>
          配额不可用
        </span>
      )}
      <i
        className={`status-dot ${loading ? "status-dot--loading" : ""} ${snapshot.loadError ? "status-dot--error" : ""}`}
        aria-hidden="true"
      />
      <button
        type="button"
        className="strip-restore"
        onClick={onRestore}
        aria-label="展开为桌面小插件"
        title="展开为桌面小插件"
      >
        <ArrowsOutSimple size={13} weight="light" aria-hidden="true" />
      </button>
    </main>
  );
}
```

要点：格子数字与进度条都表示**剩余**（tooltip 里写明"剩余"）；`.strip-cell` 的子元素 `pointer-events: none`（见 Step 4），拖动与 tooltip 都落在格子本体上。`ArrowsOutSimple` 需加入 phosphor 导入（若尚未导入）。

- [ ] **Step 3: WindowActions 加折叠按钮**

`WindowActions` 组件里，compact 形态的玻璃按钮之前插入：

```jsx
      {mode === "compact" && (
        <button
          type="button"
          className="window-action"
          onClick={() => onToggleMode("strip")}
          aria-label="折叠为胶囊条"
          title="折叠为胶囊条"
        >
          <ArrowsInLineVertical size={16} weight="light" aria-hidden="true" />
        </button>
      )}
```

`ArrowsInLineVertical` 加入 phosphor 导入。`CompactWidget` 已把 `onExpand`（即 `handleWindowMode`）作为 `onToggleMode` 传入，无需改 props。

- [ ] **Step 4: styles.css 新增胶囊样式（追加在 `.sr-only` 之前的小插件样式区末尾）**

```css
/* ============ 胶囊条（strip 第三形态） ============
   近实心深色玻璃拟态（窗口本身透明，圆角由 CSS 画）；
   不启用原生方形背板，避免从胶囊圆角外露出。 */
.strip-shell {
  display: flex;
  align-items: center;
  width: 100vw;
  height: 100dvh;
  padding: 0 6px 0 8px;
  gap: 4px;
  overflow: hidden;
  background:
    radial-gradient(120% 160% at 12% 0%, rgba(178, 200, 255, 0.07), transparent 55%),
    rgba(21, 23, 28, 0.93);
  border-radius: 13px;
  box-shadow:
    inset 0 0 0 1px rgba(255, 255, 255, 0.09),
    inset 0 1px rgba(255, 255, 255, 0.12);
  cursor: default;
  user-select: none;
}

.strip-cell {
  display: flex;
  flex: 1;
  align-items: center;
  gap: 6px;
  min-width: 0;
  padding: 4px 6px;
  border-radius: 9px;
  transition: background-color 240ms var(--motion);
}

.strip-cell > * { pointer-events: none; }

.strip-cell:hover { background: rgba(255, 255, 255, 0.07); }

.strip-cell-icon {
  width: 16px;
  height: 16px;
  border-radius: 4px;
}

.strip-cell-body {
  display: grid;
  flex: 1;
  min-width: 0;
  gap: 3px;
}

.strip-cell-body em {
  color: rgba(255, 255, 255, 0.92);
  font-style: normal;
  font-size: 11px;
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  line-height: 1;
}

.strip-cell-track {
  display: block;
  height: 4px;
  overflow: hidden;
  background: color-mix(in srgb, var(--quota-accent, #7fabef) 22%, rgba(255, 255, 255, 0.06));
  border-radius: 2px;
}

.strip-cell-track i {
  display: block;
  width: 100%;
  height: 100%;
  background: var(--quota-accent, #7fabef);
  border-radius: inherit;
  filter: brightness(1.22) saturate(0.92);
  transform-origin: left center;
  transition: transform 700ms var(--motion);
}

/* 陈旧快照整格降透明度；接近耗尽换警示色（沿用 85/95 分级）。 */
.strip-cell--stale { opacity: 0.55; }
.strip-cell--warn em { color: #e8b04a; }
.strip-cell--warn .strip-cell-track i { background: #e8b04a; filter: none; }
.strip-cell--critical em { color: #ec7a63; }
.strip-cell--critical .strip-cell-track i { background: #ec7a63; filter: none; }

.strip-empty {
  flex: 1;
  padding-left: 4px;
  color: rgba(255, 255, 255, 0.6);
  font-size: 11px;
}

.strip-shell .status-dot { flex: none; }

.strip-restore {
  display: grid;
  flex: none;
  place-items: center;
  width: 22px;
  height: 22px;
  color: rgba(255, 255, 255, 0.65);
  background: rgba(255, 255, 255, 0.08);
  border: none;
  border-radius: 7px;
  cursor: pointer;
  transition-property: color, background-color;
  transition-duration: 240ms;
  transition-timing-function: var(--motion);
}

.strip-restore:hover {
  color: rgba(255, 255, 255, 0.95);
  background: rgba(255, 255, 255, 0.16);
}
```

- [ ] **Step 5: 构建验证**

Run: `npm run build`
Expected: 编译通过无报错。

- [ ] **Step 6: Commit（Task 2 + Task 3 一并）**

```bash
git add src/App.jsx src/styles.css src/windowClient.js
git commit -m "Add the strip capsule bar as a third widget form"
```

（`src/windowClient.js` 若 Task 1 已单独提交则不再包含。）

---

### Task 4: Windows 实机验收

**Files:** 无代码改动；结果记录留待用户确认后补 `ACCEPTANCE.md`（本任务不改它）。

- [ ] **Step 1: 启动开发版**

Run: `npm run desktop:dev`（读取真实本机日志）

- [ ] **Step 2: 手动验收清单**

1. compact 标题栏出现"折叠为胶囊条"按钮；点击后变为一条约 40px 高的胶囊条，出现在卡片原位。
2. 胶囊条只显示有官方配额的 agent，每格 logo + 剩余% + 品牌色微型进度条；悬停出现系统 tooltip（各窗口剩余% + 重置倒计时；快照标注更新时间）。
3. 末端按钮回 compact；再点展开按钮进 expanded；expanded 收起回 compact。
4. 拖动胶囊条到别处 → 切回 compact（位置不同）→ 再折叠：胶囊条回到刚才拖到的位置（两套记位互不覆盖）。
5. 重启应用（strip 形态下退出）：恢复为 strip 且位置正确；compact 形态下退出则恢复 compact。
6. 置顶（固定）开关对胶囊条生效；固定时胶囊条不可拖动。
7. 断网或无配额环境：胶囊条显示"配额不可用"，无零值；陈旧快照格子半透明。
8. agent 配额数据条数变化时条宽自适应（可通过等待刷新或对比不同账号观察，允许仅代码走查确认）。

- [ ] **Step 3: 验收通过后由用户确认，异常项回到对应 Task 修复**

---

## Self-Review 结论

- Spec 覆盖：形态/平台（Task 1、2）、内容与事实分离（Task 3 Step 1-2）、切换与位置（Task 1 Step 1-2、Task 2 Step 3、Task 3 Step 3）、刷新（Task 2 Step 3）、验收清单（Task 4）——全部有对应任务。
- 无占位符；类型/命名前后一致（`applyWindowMode("strip", { width })` ↔ Task 2 调用、`onRestore` ↔ StripBar props）。
- 微小偏差已声明：进度条 4px（spec 写 6px，40px 高度里 6px 过重，验收时可调）。
