import { invoke } from "@tauri-apps/api/core";
import { platform as tauriPlatform } from "@tauri-apps/plugin-os";
import {
  resolveGlassMode,
  resolveWindowsGlassComposition,
} from "./glassAppearance.js";
import { detectRuntimePlatform } from "./platformDetection";
import {
  renderTrayQuotaBadge,
  trayBadgeKey,
} from "./trayBadge.js";
import {
  monitorForWindowPosition,
  physicalWindowSize,
  viewportCorrectedPhysicalSize,
  viewportCorrectedZoom,
} from "./windowGeometry";

const WINDOW_SIZES = {
  // minHeight 260 = 标题栏+周期签+摘要双块+底栏（约 208）+ 单行 Agent（52）：
  // Agent 只留一行时卡片允许收短，不再空一截（高度仍由内容自愈驱动）。
  compact: { width: 320, height: 320, minWidth: 320, minHeight: 260 },
  expanded: { width: 1120, height: 760, minWidth: 960, minHeight: 700 },
  // 横条高 28（26px 控件槽 + 呼吸），竖条宽 42：最小尺寸必须低于两者，
  // 否则窗口卡在下限上，内容测量再准也收不回去。
  strip: { width: 240, height: 28, minWidth: 28, minHeight: 28 },
};

// 卡片/胶囊的整体缩放系数（连续值）。窗口尺寸与页面 zoom 同乘一个系数，
// 比例不变所以不会变形；完整视图窗口本身可自由拖拽，不参与缩放（zoom 恒为 1）。
const UI_SCALE_KEY = "metrik:uiScale";
const UI_SCALE_RANGE = { min: 0.75, max: 2 };

function normalizeScale(value, range) {
  const parsed = Number.parseFloat(value);
  if (!Number.isFinite(parsed)) return 1;
  return Math.min(range.max, Math.max(range.min, parsed));
}

/// 连续缩放系数：parseFloat 后钳进范围，NaN/非数回退 1。
/// 旧档位值（1 / 1.25 / 1.5）都在范围内，存量存储自然兼容。
function normalizeUiScale(value) {
  return normalizeScale(value, UI_SCALE_RANGE);
}

function readStoredUiScale() {
  try {
    return normalizeUiScale(localStorage.getItem(UI_SCALE_KEY));
  } catch {
    return 1;
  }
}

let uiScale = readStoredUiScale();

/// 设置卡片/胶囊缩放系数（任意浮点，钳到 0.75–2.0）并持久化；
/// 下次进入卡片/胶囊时应用，返回实际生效值（调用方用它回填 UI 状态）。
function setWindowUiScale(scale) {
  uiScale = normalizeUiScale(scale);
  try {
    localStorage.setItem(UI_SCALE_KEY, String(uiScale));
  } catch {}
  return uiScale;
}

/// 窗口层当前实际使用的卡片/胶囊缩放系数（模块加载时已从存储归一化）。
function readUiScale() {
  return uiScale;
}

// 胶囊条的独立缩放系数：与小组件互不干扰（用户反馈共用一个比例不合适）。
const STRIP_SCALE_KEY = "metrik:stripScale";

function readStoredStripScale() {
  try {
    return normalizeUiScale(localStorage.getItem(STRIP_SCALE_KEY));
  } catch {
    return 1;
  }
}

let stripScale = readStoredStripScale();

/// 设置胶囊条缩放系数（钳到 0.75–2.0）并持久化；返回实际生效值。
/// 生效在形态切换里（进入胶囊条时应用），这里只管存储。
function setStripScale(scale) {
  stripScale = normalizeUiScale(scale);
  try {
    localStorage.setItem(STRIP_SCALE_KEY, String(stripScale));
  } catch {}
  return stripScale;
}

/// 窗口层当前实际使用的胶囊条缩放系数。
function readStripScale() {
  return stripScale;
}

// 各形态最近一次由内容测量收敛出的窗口尺寸（CSS px，跨会话持久化）。
// 变形首帧直接按它设置，避免"先按常量估计显示、240ms 后再跳到真实尺寸"
// 的两段式卡顿；内容变化时仍由测量观察器静悄悄修正。
const STRIP_SIZE_CACHE_KEY = "metrik:stripContentSize";
const COMPACT_HEIGHT_CACHE_KEY = "metrik:compactContentHeight";

function readJson(key) {
  try {
    const value = JSON.parse(localStorage.getItem(key) || "null");
    return value && typeof value === "object" ? value : null;
  } catch {
    return null;
  }
}

/// 只接受合理范围的缓存尺寸；损坏/越界一律丢弃回退常量。
function saneSize(value, minW, minH, maxW, maxH) {
  if (!value || !Number.isFinite(value.width) || !Number.isFinite(value.height)) return null;
  if (value.width < minW || value.width > maxW) return null;
  if (value.height < minH || value.height > maxH) return null;
  return { width: Math.round(value.width), height: Math.round(value.height) };
}

let stripSizeCache = readJson(STRIP_SIZE_CACHE_KEY) || {};
let compactHeightCache = saneSize(
  { width: 320, height: Number(readJson(COMPACT_HEIGHT_CACHE_KEY)?.height) },
  320, 320, 320, 2000,
)?.height || null;

/// 胶囊条变形时的首帧尺寸：优先上次测量缓存，没有就用常量估计。
function stripContentSize(orientation, fallback) {
  const cached = saneSize(stripSizeCache[orientation], 40, 40, 2000, 2000);
  return cached || fallback;
}

function rememberStripSize(width, height) {
  // 竖条恒为窄高、横条恒为宽矮，从尺寸本身推断方向。
  const orientation = height > width ? "vertical" : "horizontal";
  stripSizeCache = { ...stripSizeCache, [orientation]: { width, height } };
  try {
    localStorage.setItem(STRIP_SIZE_CACHE_KEY, JSON.stringify(stripSizeCache));
  } catch {}
}

function compactContentHeight(fallback) {
  return compactHeightCache || fallback;
}

function rememberCompactHeight(height) {
  compactHeightCache = height;
  try {
    localStorage.setItem(COMPACT_HEIGHT_CACHE_KEY, JSON.stringify({ height }));
  } catch {}
}

/// compact/strip 的窗口尺寸：乘各自缩放系数后取整到物理像素再下发，
/// 避免分数 DPI（125%/150%）下逻辑尺寸取整产生的半像素裁切。
/// 小组件用 uiScale（默认参数），胶囊条传自己的 stripScale。
async function scaledPhysicalSize(
  api,
  appWindow,
  width,
  height,
  scale = uiScale,
  targetScaleFactor = null,
) {
  const factor =
    Number.isFinite(targetScaleFactor) && targetScaleFactor > 0
      ? targetScaleFactor
      : await appWindow.scaleFactor().catch(() => 1);
  const physical = physicalWindowSize(width, height, scale, factor);
  return new api.PhysicalSize(physical.width, physical.height);
}

/// 内容缩放用 WebView 原生 zoom（等同浏览器 Ctrl+缩放）：视口单位、媒体查询
/// 全部自洽。CSS zoom 做不到——100vw 元素在 zoom 下会溢出视口（实测）。
async function applyWebviewZoom(factor) {
  if (!isDesktop()) return;
  const api = await import("@tauri-apps/api/webview");
  await api
    .getCurrentWebview()
    .setZoom(factor)
    .catch((error) => {
      // zoom 失败会让视口与物理尺寸失配（320 内容被裁），不能静默吞掉。
      console.warn("Unable to apply the webview zoom.", error);
    });
}

/// 启动时就地应用缩放系数（不走 applyWindowMode 的 hide/show，避免闪烁）。
/// strip 的启动尺寸由 strip 专属 effect 走 applyWindowMode，这里只管 compact。
async function applyStartupUiScale(mode) {
  if (isMacPlatform()) return;
  const api = await windowApi();
  if (!api) return;
  await applyWebviewZoom(uiScale);
  if (mode !== "compact") return;
  const appWindow = api.getCurrentWindow();
  const size = WINDOW_SIZES.compact;
  const height = compactContentHeight(size.height);
  const placement = await floatingPlacement(
    api,
    "compact",
    { width: size.width, height },
    uiScale,
  );
  // tauri.conf 的 320×320 只负责首帧；内容可能因 75% 缩放或单行 Agent
  // 收到更小，启动路径也必须像形态切换一样解除配置最小尺寸。
  await appWindow.setMinSize(null);
  // GTK 会把初始不可缩放的尺寸当成硬约束；这只在 Linux 启动路径解除。
  // Windows 保留原有的启动时锁定行为。
  if (isLinuxPlatform()) await appWindow.setResizable(true).catch(() => {});
  else await appWindow.setResizable(false);
  const physical = await scaledPhysicalSize(
    api,
    appWindow,
    size.width,
    height,
    uiScale,
    placement.monitor?.scaleFactor,
  );
  try {
    await appWindow.setSize(physical);
    await reconcileFloatingSizeAfterShow(
      api,
      appWindow,
      size.width,
      height,
      uiScale,
      physical,
    );
  } catch (error) {
    // 隐藏状态下某些 GTK 合成器会暂时拒绝 resize；位置已经在前一步恢复，
    // 不能因为首帧尺寸校正失败把应用永久留在托盘。
    console.warn("Unable to apply the hidden Linux startup size.", error);
  } finally {
    // Linux 首窗在 tauri.linux.conf.json 中以 hidden 创建。位置和首帧尺寸已
    // 全部恢复后才首次映射，避免 GTK 先按默认居中位置闪现再移动。
    if (isLinuxPlatform()) {
      await appWindow.show().catch(() => {});
      await appWindow.setFocus().catch(() => {});
    }
  }
}

/// 窗口重设尺寸后可能伸出屏幕（固定状态下竖条切横条最典型：位置不动、
/// 宽度暴涨，控制按钮全在屏幕外，固定态又没有拖拽区，用户就被锁死了）。
/// 把窗口钳回重叠面积最大的显示器工作区内；完全不与任何屏幕重叠时
/// 返回 false，由调用方居中。
/// knownOuter：调用方刚算出的物理尺寸。setSize 异步生效，紧接着读 outerSize
/// 会拿到旧值（实测拿到过 ~0），把变高的窗口钳错——传入时跳过 outerSize 读取。
async function clampIntoWorkArea(api, appWindow, knownOuter = null) {
  if (isLinuxPlatform() && !(await supportsGlobalWindowCoordinates())) return false;
  const [pos, readOuter, monitors] = await Promise.all([
    appWindow.outerPosition().catch(() => null),
    knownOuter ? null : appWindow.outerSize().catch(() => null),
    api.availableMonitors().catch(() => []),
  ]);
  const outer = knownOuter || readOuter;
  if (!pos || !outer || !(monitors || []).length) return false;
  let best = null;
  let bestOverlap = 0;
  monitors.forEach((monitor) => {
    const area = {
      x: monitor.workArea?.position?.x ?? monitor.position.x,
      y: monitor.workArea?.position?.y ?? monitor.position.y,
      width: monitor.workArea?.size?.width ?? monitor.size.width,
      height: monitor.workArea?.size?.height ?? monitor.size.height,
    };
    const overlapX = Math.min(pos.x + outer.width, area.x + area.width) - Math.max(pos.x, area.x);
    const overlapY = Math.min(pos.y + outer.height, area.y + area.height) - Math.max(pos.y, area.y);
    const overlap = Math.max(0, overlapX) * Math.max(0, overlapY);
    if (overlap > bestOverlap) {
      bestOverlap = overlap;
      best = area;
    }
  });
  if (!best) return false;
  const x = Math.min(Math.max(pos.x, best.x), best.x + best.width - outer.width);
  const y = Math.min(Math.max(pos.y, best.y), best.y + best.height - outer.height);
  if (x !== pos.x || y !== pos.y) {
    await appWindow.setPosition(new api.PhysicalPosition(Math.round(x), Math.round(y))).catch(() => {});
  }
  return true;
}

/// Windows 偶尔丢弃隐藏窗口的 setPosition：显示后校验一次坐标，
/// 与钳位/居中后的预期不符就补发并重新钳位（否则窗口"复位"到变形前位置）。
async function ensurePositionAfterShow(api, appWindow, target) {
  if (isLinuxPlatform() && !(await supportsGlobalWindowCoordinates())) return;
  if (!target) return;
  const current = await appWindow.outerPosition().catch(() => null);
  if (!current) return;
  if (Math.abs(current.x - target.x) <= 2 && Math.abs(current.y - target.y) <= 2) return;
  await appWindow
    .setPosition(new api.PhysicalPosition(Math.round(target.x), Math.round(target.y)))
    .catch(() => {});
  await clampIntoWorkArea(api, appWindow);
}

/// 形态记忆位置可能在另一台 DPI 不同的屏幕上。先用目标坐标选显示器，再用该
/// 显示器的 scaleFactor 算物理尺寸；不能先按当前屏幕算完再搬过去。
/// 记忆坐标来自 outerPosition()，但 Linux/GTK（GNOME X11 实测）下 setPosition
/// 把目标解释成内容区（inner）原点：直接回填 outer 坐标会让窗口再上移一截
/// 并在每次重启累计窗口管理器的装饰偏移。恢复时动态测量 inner−outer 偏移并
/// 补回目标，让内容区落在用户上次摆放的位置；不假设特定窗口管理器或固定像素。
/// Windows 无边框窗口内外坐标一致，Wayland 不走这条路径，偏移自然为 0。
async function positionRestoreOffset(api, appWindow) {
  if (!isLinuxPlatform() || !(await supportsGlobalWindowCoordinates())) {
    return { x: 0, y: 0 };
  }
  const read = async () => {
    const [outer, inner] = await Promise.all([
      appWindow.outerPosition().catch(() => null),
      appWindow.innerPosition().catch(() => null),
    ]);
    if (!outer || !inner) return null;
    const offset = {
      x: Math.round(inner.x - outer.x),
      y: Math.round(inner.y - outer.y),
    };
    return offset;
  };
  const first = await read();
  if (first && (first.x !== 0 || first.y !== 0)) return first;

  // GTK may report a temporary zero before the first map settles. One mapped
  // sample is enough: zero is also a legitimate decoration offset and must not
  // stall every frameless-window restore for another 1.5 seconds.
  await new Promise((resolve) => window.setTimeout(resolve, 150));
  return (await read()) || first || { x: 0, y: 0 };
}

async function floatingPlacement(api, mode, logicalSize, contentScale) {
  if (isLinuxPlatform() && !(await supportsGlobalWindowCoordinates())) {
    return { position: null, monitor: null };
  }
  const stored = readStoredPosition(mode);
  const requested = lastPositions[mode] || stored;
  if (!requested) return { position: null, monitor: null };
  const monitors = await api.availableMonitors().catch(() => []);
  const monitor = monitorForWindowPosition(
    monitors,
    requested,
    logicalSize,
    contentScale,
  );
  if (!monitor) return { position: null, monitor: null };
  const offset = isLinuxPlatform()
    ? await positionRestoreOffset(api, api.getCurrentWindow())
    : { x: 0, y: 0 };
  return {
    position: new api.PhysicalPosition(
      Math.round(requested.x + offset.x),
      Math.round(requested.y + offset.y),
    ),
    monitor,
  };
}

async function settleWebviewLayout() {
  if (typeof window === "undefined" || typeof window.requestAnimationFrame !== "function") {
    return;
  }
  await new Promise((resolve) => {
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      window.clearTimeout(timeout);
      resolve();
    };
    const timeout = window.setTimeout(finish, 120);
    window.requestAnimationFrame(() => {
      window.requestAnimationFrame(finish);
    });
  });
}

/// Windows 偶尔丢弃隐藏窗口的 setPosition，或在 show 后才完成 DPI 切换。
/// 最终以 CSS 视口为准：若 WebView2 还残留系统/浏览器 zoom，就用当前物理尺寸
/// 与实测视口反算窗口尺寸。只重复两次，避免异常瞬时值形成无限 resize 循环。
async function reconcileFloatingSizeAfterShow(
  api,
  appWindow,
  width,
  height,
  contentScale,
  previousPhysical,
) {
  await settleWebviewLayout();
  const factor = await appWindow.scaleFactor().catch(() => null);
  if (!Number.isFinite(factor) || factor <= 0) return previousPhysical;
  const formulaPhysical = await scaledPhysicalSize(
    api,
    appWindow,
    width,
    height,
    contentScale,
    factor,
  );
  let current = await appWindow.innerSize().catch(() => null);
  let appliedPhysical = current || previousPhysical || formulaPhysical;

  const formulaMatches =
    current &&
    Math.abs(current.width - formulaPhysical.width) <= 1 &&
    Math.abs(current.height - formulaPhysical.height) <= 1;
  if (!formulaMatches) {
    appliedPhysical = formulaPhysical;
    await appWindow.setSize(formulaPhysical).catch((error) => {
      console.warn("Unable to apply the calculated floating window size.", error);
    });
    await settleWebviewLayout();
    current = await appWindow.innerSize().catch(() => null);
  }

  const initialViewportWidth = window.innerWidth;
  const initialViewportHeight = window.innerHeight;
  if (
    Math.abs(initialViewportWidth - width) <= 1 &&
    Math.abs(initialViewportHeight - height) <= 1
  ) {
    await clampIntoWorkArea(api, appWindow, current || appliedPhysical);
    return current || appliedPhysical;
  }

  // 外框已经是设计尺寸 × 应用比例 × DPI，但视口仍偏小时，优先抵消 WebView2
  // 额外保留的 zoom。这样不会为了显示完整内容而把整个卡片无端放大。
  const correctedZoom = viewportCorrectedZoom({
    contentScale,
    viewportWidth: initialViewportWidth,
    viewportHeight: initialViewportHeight,
    expectedWidth: width,
    expectedHeight: height,
  });
  if (correctedZoom) {
    await applyWebviewZoom(correctedZoom);
    await settleWebviewLayout();
    current = await appWindow.innerSize().catch(() => current);
    if (
      Math.abs(window.innerWidth - width) <= 1 &&
      Math.abs(window.innerHeight - height) <= 1
    ) {
      await clampIntoWorkArea(api, appWindow, current || appliedPhysical);
      return current || appliedPhysical;
    }
  }

  for (let pass = 0; pass < 2; pass += 1) {
    const viewportWidth = window.innerWidth;
    const viewportHeight = window.innerHeight;
    if (
      Math.abs(viewportWidth - width) <= 1 &&
      Math.abs(viewportHeight - height) <= 1
    ) {
      await clampIntoWorkArea(api, appWindow, appliedPhysical);
      return appliedPhysical;
    }

    const corrected = current
      ? viewportCorrectedPhysicalSize({
          currentPhysicalWidth: current.width,
          currentPhysicalHeight: current.height,
          viewportWidth,
          viewportHeight,
          expectedWidth: width,
          expectedHeight: height,
        })
      : null;
    appliedPhysical = corrected
      ? new api.PhysicalSize(corrected.width, corrected.height)
      : formulaPhysical;
    await appWindow.setSize(appliedPhysical).catch((error) => {
      console.warn("Unable to reconcile the floating window size.", error);
    });
    await settleWebviewLayout();
    current = await appWindow.innerSize().catch(() => null);
  }

  if (
    Math.abs(window.innerWidth - width) <= 1 &&
    Math.abs(window.innerHeight - height) <= 1
  ) {
    await clampIntoWorkArea(api, appWindow, current || appliedPhysical);
    return current || appliedPhysical;
  }

  console.warn("Floating window viewport remains desynchronized.", {
    expectedWidth: width,
    expectedHeight: height,
    viewportWidth: window.innerWidth,
    viewportHeight: window.innerHeight,
    contentScale,
    scaleFactor: factor,
    targetPhysicalWidth: appliedPhysical.width,
    targetPhysicalHeight: appliedPhysical.height,
  });
  await clampIntoWorkArea(api, appWindow, appliedPhysical);
  return appliedPhysical;
}

// compact 与横/竖胶囊条各自记位，互不覆盖；expanded 不记位。
const POSITION_KEYS = {
  compact: "metrik:widgetPosition",
  "strip-horizontal": "metrik:stripHorizontalPosition",
  "strip-vertical": "metrik:stripVerticalPosition",
};

const lastPositions = {
  compact: null,
  "strip-horizontal": null,
  "strip-vertical": null,
};

function isDesktop() {
  return typeof window !== "undefined" && Boolean(window.__TAURI_INTERNALS__);
}

function readStoredPosition(mode) {
  const key = POSITION_KEYS[mode];
  if (!key) return null;
  try {
    // 将已有横向胶囊条的位置作为一次性兼容回退，避免升级后丢失原有位置。
    const raw = JSON.parse(localStorage.getItem(key)
      || (mode === "strip-horizontal" ? localStorage.getItem("metrik:stripPosition") : "null"));
    if (!raw || !Number.isFinite(raw.x) || !Number.isFinite(raw.y)) return null;
    return raw;
  } catch {
    return null;
  }
}

/// 记住窗口的物理坐标（按形态分开记）；边缘挂靠把窗口滑出屏幕时不记，
/// 避免下次开机在屏外。
async function rememberWindowPosition(api, appWindow, mode) {
  if (isLinuxPlatform() && !(await supportsGlobalWindowCoordinates())) return;
  const key = POSITION_KEYS[mode];
  if (!key) return;
  const [pos, monitor, monitors] = await Promise.all([
    appWindow.outerPosition().catch(() => null),
    api.currentMonitor().catch(() => null),
    api.availableMonitors().catch(() => []),
  ]);
  if (!pos) return;
  // Windows 把最小化窗口报在 (-32000, -32000)：不是用户摆放，不记。
  if (pos.x <= -32000 || pos.y <= -32000) return;
  // 完全掉出所有屏幕的坐标不记（钳位/锚定失效的残留、拔了扩展屏），
  // 否则坏坐标会被持久化，以后每次进入该形态都恢复到屏外。
  const outer = await appWindow.outerSize().catch(() => null);
  if (outer && (monitors || []).length) {
    const onAnyScreen = monitors.some((screen) => {
      const left = screen.position.x;
      const top = screen.position.y;
      return (
        pos.x + outer.width > left &&
        pos.x < left + screen.size.width &&
        pos.y + outer.height > top &&
        pos.y < top + screen.size.height
      );
    });
    if (!onAnyScreen) return;
  }
  if (monitor) {
    const workArea = monitor.workArea || {
      position: monitor.position,
      size: monitor.size,
    };
    const left = workArea.position.x;
    const top = workArea.position.y;
    const right = left + workArea.size.width;
    const bottom = top + workArea.size.height;
    // 边缘挂靠收起后只剩一条细边：四个方向都不能覆盖正常位置记忆。
    if (pos.x < left || pos.x > right - 24 || pos.y < top || pos.y > bottom - 24) return;
  }
  lastPositions[mode] = pos;
  localStorage.setItem(key, JSON.stringify({ x: pos.x, y: pos.y }));
  if (isLinuxPlatform()) {
    const inner = await appWindow.innerPosition().catch(() => null);
    const offsetX = inner ? Math.round(inner.x - pos.x) : 0;
    const offsetY = inner ? Math.round(inner.y - pos.y) : 0;
    await invoke("persist_linux_startup_position", {
      x: Math.round(pos.x),
      y: Math.round(pos.y),
      offsetX,
      offsetY,
    }).catch((error) => {
      console.warn("Unable to persist the Linux startup position.", error);
    });
  }
}

/// 启动时把窗口放回该形态上次的位置；坐标已不在任何显示器上（拔了扩展屏等）时居中。
async function restoreWindowPosition(mode = "compact") {
  // macOS 面板永远贴着菜单栏图标，没有"上次的位置"这回事。Linux 在 X11
  // 会话恢复坐标；Wayland 协议不暴露全局坐标，由能力探测自动跳过。
  if (isMacPlatform() || (isLinuxPlatform() && !(await supportsGlobalWindowCoordinates()))) return;
  const api = await windowApi();
  if (!api) return;
  const stored = readStoredPosition(mode);
  if (!stored) return;

  const appWindow = api.getCurrentWindow();
  const [size, monitors] = await Promise.all([
    appWindow.outerSize().catch(() => null),
    api.availableMonitors().catch(() => []),
  ]);
  const fallback = WINDOW_SIZES[mode] || WINDOW_SIZES.compact;
  const width = size?.width || fallback.width;
  const height = size?.height || fallback.height;
  // 至少有一部分窗口落在某块屏幕的可见区域内才算有效坐标。
  const onScreen = (monitors || []).some((monitor) => {
    const left = monitor.position.x;
    const top = monitor.position.y;
    return (
      stored.x + width > left &&
      stored.x < left + monitor.size.width &&
      stored.y + height > top &&
      stored.y < top + monitor.size.height
    );
  });
  if (!onScreen) return;

  lastPositions[mode] = new api.PhysicalPosition(stored.x, stored.y);
  const offset = isLinuxPlatform()
    ? await positionRestoreOffset(api, appWindow)
    : { x: 0, y: 0 };
  await appWindow
    .setPosition(new api.PhysicalPosition(
      Math.round(stored.x + offset.x),
      Math.round(stored.y + offset.y),
    ))
    .catch(() => {});
}

/// 托盘右键"显示完整视图"：让用户从胶囊/卡片直达完整视图，不必先弹出卡片
/// 再点展开。变形仍由前端做（Windows 单窗口变形），托盘只发意图。
/// macOS 的完整视图是独立窗口，由 macos.rs 的菜单栏负责，不发这个事件。
async function onTrayShowExpanded(handler) {
  if (!isDesktop()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen("tray://show-expanded", () => handler());
}

/// mac 外观改动（玻璃浓度）的跨窗口广播：设置页开在独立的完整视图窗口，
/// 面板是另一个 webview 实例。WKWebView 的 storage 事件不跨窗口触发
/// （每个窗口是独立 process pool），所以走 Tauri 事件总线；emit 也会回到
/// 发送方自己，监听方要能幂等处理。其它平台单窗口，不用广播。
async function broadcastMacAppearance(payload) {
  if (!isDesktop() || !isMacPlatform()) return;
  const { emit } = await import("@tauri-apps/api/event");
  await emit("metrik://mac-appearance", payload).catch(() => {});
}

async function onMacAppearance(handler) {
  if (!isDesktop() || !isMacPlatform()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen("metrik://mac-appearance", (event) => handler(event.payload || {}));
}

/// macOS 的设置页与菜单栏面板是两个独立 WebView。Agent 选择必须走事件总线，
/// 不能依赖 localStorage 的 storage 事件（WKWebView 不会跨这两个窗口派发它）。
async function broadcastMacAgentSelection(agents) {
  if (!isDesktop() || !isMacPlatform()) return;
  const { emit } = await import("@tauri-apps/api/event");
  // 先落库为后端权威选择，再通知其它窗口；接收窗口收到事件后会立刻刷新
  // 菜单栏状态项，顺序反过来会让它读到落库前的旧选择。
  await invoke("set_macos_agent_selection", { agents });
  await emit("metrik://mac-agent-selection", { agents }).catch(() => {});
}

/// 后端持久化的 macOS Agent 权威选择。各窗口 localStorage 互不同步（独立
/// WebView），本地值只是缓存；空数组表示后端还没保存过（首次启动）。
async function getMacAgentSelection() {
  if (!isDesktop() || !isMacPlatform()) return [];
  return invoke("get_macos_agent_selection").catch(() => []);
}

async function onMacAgentSelection(handler) {
  if (!isDesktop() || !isMacPlatform()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen("metrik://mac-agent-selection", (event) => handler(event.payload || {}));
}

/// 拖动结束后持久化窗口位置（compact 与 strip 各记各的；expanded 不记）。
async function startPositionMemory(getMode) {
  if (isMacPlatform() || (isLinuxPlatform() && !(await supportsGlobalWindowCoordinates()))) {
    return () => {};
  }
  const api = await windowApi();
  if (!api) return () => {};
  const appWindow = api.getCurrentWindow();
  let timer = null;
  const unlistenPromise = appWindow.onMoved(() => {
    const mode = getMode();
    if (!POSITION_KEYS[mode]) return;
    window.clearTimeout(timer);
    timer = window.setTimeout(() => {
      rememberWindowPosition(api, appWindow, mode).catch(() => {});
    }, 400);
  });
  return async () => {
    window.clearTimeout(timer);
    const unlisten = await unlistenPromise.catch(() => null);
    unlisten?.();
  };
}

function isWindowsPlatform() {
  return runtimePlatform() === "windows";
}

/// Ubuntu 24.04 的受支持 Linux 壳层以 Wayland 为基线。不能把 Linux 当成
/// Windows 来承诺全局坐标、DWM 材质或任务栏原生样式。
function isLinuxPlatform() {
  return runtimePlatform() === "linux";
}

let globalWindowCoordinatesPromise = null;

/// Windows 始终支持全局窗口坐标；Linux 只在 X11 启用。结果按 WebView 生命周期
/// 缓存，避免拖动与内容尺寸观察器反复跨 IPC 查询环境。
async function supportsGlobalWindowCoordinates() {
  if (isMacPlatform()) return false;
  if (!isLinuxPlatform()) return true;
  if (!isDesktop()) return false;
  if (!globalWindowCoordinatesPromise) {
    globalWindowCoordinatesPromise = invoke("linux_supports_global_window_coordinates")
      .then(Boolean)
      .catch(() => false);
  }
  return globalWindowCoordinatesPromise;
}

/// macOS 上小插件是菜单栏面板（NSPanel）：位置由托盘图标决定；零占地摘要直接
/// 画进菜单栏状态图标，不使用 strip 悬浮窗。窗口按钮/挂靠/位置记忆/置顶由平台语义取代。
function isMacPlatform() {
  return runtimePlatform() === "macos";
}

/// 桌面包优先使用 Tauri 编译期写入的平台值，避免 WebView user-agent 变化让
/// macOS 误入 Windows 的 strip 分支。纯网页预览才使用 UA 兜底。
function runtimePlatform() {
  let nativePlatform = null;
  if (isDesktop()) {
    try {
      nativePlatform = tauriPlatform();
    } catch {
      // 开发预览或插件尚未初始化时继续走 UA 兜底。
    }
  }
  const userAgent = typeof navigator === "undefined" ? "" : navigator.userAgent;
  return detectRuntimePlatform(nativePlatform, userAgent);
}

async function windowApi() {
  if (!isDesktop()) return null;
  return import("@tauri-apps/api/window");
}

async function makeWebviewTransparent() {
  const api = await import("@tauri-apps/api/webview");
  await api.getCurrentWebview().setBackgroundColor([0, 0, 0, 0]);
}

/// macOS 的完整视图是一个独立的标准窗口（原生红绿灯、可缩放、进 Dock），
/// 由后端创建；面板保持原样，不变形。
async function openExpandedWindow(nav) {
  if (!isDesktop()) return;
  await invoke("open_expanded_window", { nav: nav || null });
}

/// 按用户选择更新 macOS 菜单栏 Agent 状态项；null 表示该 Agent 没有可靠的
/// 官方额度，后端会显示 "--"，不会填零或伪造数字。
async function updateMacStatusItems(items) {
  if (!isDesktop() || !isMacPlatform()) return;
  await invoke("update_macos_status_items", {
    agents: items.map((item) => item.agent),
    remaining: items.map((item) =>
      Number.isFinite(item.remaining) ? item.remaining : null,
    ),
    stale: items.map((item) => Boolean(item.stale)),
  });
}

/// 把列表最上方 Agent 的余量画进 Windows 任务栏托盘图标。spec 为 null 时
/// 恢复应用默认图标。同一状态只下发一次：快照每次刷新都会重跑，但数字没变
/// 就不该再碰托盘。下发失败不记账，下一次快照会自动重试。
let appliedTrayBadgeKey = null;

async function updateTrayQuotaBadge(spec) {
  if (!isDesktop() || !isWindowsPlatform()) return;
  const nextKey = spec ? trayBadgeKey(spec) : null;
  if (nextKey === appliedTrayBadgeKey) return;
  if (!spec) {
    await invoke("set_tray_quota_icon", { icon: null, tooltip: null });
    appliedTrayBadgeKey = null;
    return;
  }
  const rendered = renderTrayQuotaBadge(spec.percent, spec.stale);
  await invoke("set_tray_quota_icon", {
    icon: {
      rgba: Array.from(rendered.rgba),
      width: rendered.width,
      height: rendered.height,
    },
    tooltip: spec.tooltip,
  });
  appliedTrayBadgeKey = nextKey;
}

async function applyWindowMode(mode, options = {}) {
  // macOS 的完整视图是独立窗口；菜单栏 NSPanel 只保留 compact 卡片。
  if (isMacPlatform()) {
    if (!isDesktop()) return;
    // 首帧直接用上次内容测量的高度（同 Windows 的尺寸缓存），避免两段式跳变。
    await resizeMacosPanel({ height: compactContentHeight(WINDOW_SIZES.compact.height) });
    return;
  }

  const api = await windowApi();
  if (!api) return;

  const appWindow = api.getCurrentWindow();
  const size = WINDOW_SIZES[mode] || WINDOW_SIZES.compact;

  // 变形前记下离开的悬浮形态的坐标：compact/横条/竖条各记各的，互不污染
  // （以前无条件写 lastPositions.compact，从胶囊条进大视图会把小插件的
  // 记忆污染成胶囊条坐标，回来时就"复位"了）。同形态重入（启动恢复、
  // 自检重断言）不是变形，不记。
  if (
    POSITION_KEYS[options.fromPositionMode]
    && options.fromPositionMode !== options.positionMode
  ) {
    await rememberWindowPosition(api, appWindow, options.fromPositionMode);
  }

  if (mode === "expanded") {
    // 完整视图是常规窗口：一律解除置顶。固定（置顶 + 锁位置）只属于
    // compact/strip 悬浮形态，否则 1120x760 的大窗口盖住所有应用切不走。
    await appWindow.setAlwaysOnTop(false).catch(() => {});
    if (isLinuxPlatform()) await setPinnedHoverBehavior(false);
    // 小插件不占任务栏；完整视图是常规窗口，要出现在任务栏里。
    // 无边框窗口的任务栏按钮由 WS_EX_APPWINDOW 决定，setSkipTaskbar 补不上它，
    // 所以走后端改窗口样式；样式必须在隐藏状态下改，重新显示后 shell 才重读。
    await appWindow.hide().catch(() => {});
    // 卡片/胶囊的缩放系数不进完整视图；完整视图窗口本身可自由拖拽，
    // 不需要内容缩放（zoom 恒为 1）。藏起来再改，避免闪缩放跳变。
    await applyWebviewZoom(1);
    await appWindow.setSkipTaskbar(false).catch(() => {});
    await invoke("set_taskbar_button", { visible: true }).catch(() => {});
    const monitor = await api.currentMonitor().catch(() => null);
    const workArea = monitor?.workArea?.size?.toLogical(monitor.scaleFactor);
    const targetWidth = Math.min(size.width, Math.max(WINDOW_SIZES.compact.width, (workArea?.width || size.width) - 32));
    const targetHeight = Math.min(size.height, Math.max(WINDOW_SIZES.compact.height, (workArea?.height || size.height) - 32));
    const minWidth = Math.min(size.minWidth, targetWidth);
    const minHeight = Math.min(size.minHeight, targetHeight);
    await appWindow.setMinSize(null);
    await appWindow.setResizable(true);
    await appWindow.setMaximizable(true);
    await appWindow.setMinSize(new api.LogicalSize(minWidth, minHeight));
    await appWindow.setSize(new api.LogicalSize(targetWidth, targetHeight));
    if (isLinuxPlatform()) await appWindow.center().catch(() => {});
    else await appWindow.center();
    await appWindow.show().catch(() => {});
    await appWindow.setFocus().catch(() => {});
    return;
  }

  if (await appWindow.isMaximized().catch(() => false)) {
    await appWindow.unmaximize();
  }
  await appWindow.hide().catch(() => {});
  // 卡片与胶囊各有独立缩放系数，互不干扰；藏起来再改，避免闪缩放跳变。
  await applyWebviewZoom(mode === "strip" ? stripScale : uiScale);
  await appWindow.setSkipTaskbar(true).catch(() => {});
  await invoke("set_taskbar_button", { visible: false }).catch(() => {});
  await appWindow.setMinSize(null);
  await appWindow.setMaximizable(false);

  if (mode === "strip") {
    const width = Math.max(size.minWidth, Math.round(options.width || size.width));
    const height = Math.max(size.minHeight, Math.round(options.height || size.height));
    const placement = await floatingPlacement(
      api,
      options.positionMode || "strip-horizontal",
      { width, height },
      stripScale,
    );
    // 先恢复目标位置；窗口 DPI 变化后再按目标显示器 factor 设尺寸。
    if (placement.position) {
      await appWindow.setPosition(placement.position).catch(() => {});
    }
    const physical = await scaledPhysicalSize(
      api,
      appWindow,
      width,
      height,
      stripScale,
      placement.monitor?.scaleFactor,
    );
    // tauri.conf 的初始 resizable:false 会让 GTK 把启动尺寸写成固定约束，必须
    // 先解除才能收成短边 40px 的胶囊。Linux 下此后保持 resizable:true：Tao
    // 文档明确指出再次切回 false 会附带至少 100px 的限制，实机会被 Mutter
    // 撑成 200×200 大框。内容观察器会把任何意外的手动尺寸变化立即校正回
    // 设计值，页面也不提供缩放控件。
    if (isLinuxPlatform()) await appWindow.setResizable(true).catch(() => {});
    await appWindow.setSize(physical).catch((error) => {
      console.warn("Unable to apply the strip window size.", error);
    });
    // 缩放只走设置页滑杆：原生无边框窗口的 resize 命中区覆盖所有边，
    // 限制不到四角（0.11.0–0.11.1 的拖拽缩放因此移除）。
    if (!isLinuxPlatform()) await appWindow.setResizable(false);
    // 伸出屏幕的部分钳回工作区（挂靠残留、方向切换变宽等）；
    // 完全不在任何屏幕上（拔了扩展屏等）时居中，胶囊条永远看得见、够得着。
    const clamped = await clampIntoWorkArea(api, appWindow, physical);
    if (!clamped) {
      if (isLinuxPlatform()) await appWindow.center().catch(() => {});
      else await appWindow.center();
    }
    // 钳位/居中后的最终坐标是显示后校验的基准。
    const stripFinal = await appWindow.outerPosition().catch(() => null);
    await appWindow.show().catch(() => {});
    await ensurePositionAfterShow(api, appWindow, stripFinal);
    await reconcileFloatingSizeAfterShow(
      api,
      appWindow,
      width,
      height,
      stripScale,
      physical,
    );
    await appWindow.setFocus().catch(() => {});
    return;
  }

  const compactHeight = compactContentHeight(size.height);
  const placement = await floatingPlacement(
    api,
    "compact",
    { width: size.width, height: compactHeight },
    uiScale,
  );
  // compact 与 strip 一样先到目标屏，再按目标 DPI 算尺寸。
  if (placement.position) {
    await appWindow.setPosition(placement.position).catch(() => {});
  }
  const compactPhysical = await scaledPhysicalSize(
    api,
    appWindow,
    size.width,
    compactHeight,
    uiScale,
    placement.monitor?.scaleFactor,
  );
  // setSize 失败不能中断 apply（窗口会停在 hidden 状态）：告警后继续，
  // 尺寸偏差由内容自愈观察器收敛。
  try {
    await appWindow.setSize(compactPhysical);
  } catch (error) {
    console.warn("Unable to apply the compact window size.", error);
  }
  // 缩放只走设置页滑杆（同胶囊条，拖拽缩放在 0.11.0–0.11.1 后移除）。
  if (!isLinuxPlatform()) {
    await appWindow.setResizable(false);
  }

  if (placement.position) {
    // 缩放系数调大后，记忆位置 + 新尺寸可能伸出屏幕，钳回工作区。
    await clampIntoWorkArea(api, appWindow, compactPhysical);
  } else {
    // Wayland 可以拒绝客户端指定位置；尺寸仍然有效，摆放交给合成器即可。
    if (isLinuxPlatform()) await appWindow.center().catch(() => {});
    else await appWindow.center();
  }
  // 钳位/居中后的最终坐标是显示后校验的基准。
  const compactFinal = await appWindow.outerPosition().catch(() => null);
  await appWindow.show().catch(() => {});
  await ensurePositionAfterShow(api, appWindow, compactFinal);
  await reconcileFloatingSizeAfterShow(
    api,
    appWindow,
    size.width,
    compactHeight,
    uiScale,
    compactPhysical,
  );
  if (isLinuxPlatform()) await appWindow.setResizable(false).catch(() => {});
  await appWindow.setFocus().catch(() => {});
}

/// 胶囊条格数或方向变化时只调尺寸，不走 hide/show，避免闪烁。
async function resizeStripWindow({ width, height }) {
  if (isMacPlatform()) {
    return;
  }
  const api = await windowApi();
  if (!api) return;
  const appWindow = api.getCurrentWindow();
  const size = WINDOW_SIZES.strip;
  const targetWidth = Math.max(size.minWidth, Math.round(width || size.width));
  const targetHeight = Math.max(size.minHeight, Math.round(height || size.height));
  // 变形前记下贴边状态：用户把条贴在屏幕右/下缘时，方向切换或格数变化
  // 只改尺寸会把它从边缘"撕"下来（只保左上角），必须按原贴边重新锚定。
  const coordinateAware = !isLinuxPlatform() || await supportsGlobalWindowCoordinates();
  const [pos, outer, monitor] = await Promise.all([
    appWindow.outerPosition().catch(() => null),
    appWindow.outerSize().catch(() => null),
    api.currentMonitor().catch(() => null),
  ]);
  const workArea = monitor?.workArea;
  const anchor = { right: false, bottom: false };
  if (coordinateAware && pos && outer && workArea) {
    const workRight = workArea.position.x + workArea.size.width;
    const workBottom = workArea.position.y + workArea.size.height;
    anchor.right = Math.abs(pos.x + outer.width - workRight) <= 8;
    anchor.bottom = Math.abs(pos.y + outer.height - workBottom) <= 8;
  }
  let physical = await scaledPhysicalSize(
    api,
    appWindow,
    targetWidth,
    targetHeight,
    stripScale,
    monitor?.scaleFactor,
  );
  await appWindow.setSize(physical).catch((error) => {
    console.warn("Unable to resize the strip window.", error);
  });
  physical = await reconcileFloatingSizeAfterShow(
    api,
    appWindow,
    targetWidth,
    targetHeight,
    stripScale,
    physical,
  );
  // 测量收敛出的尺寸记作下次变形的首帧（否则首帧永远是常量估计）。
  rememberStripSize(targetWidth, targetHeight);
  if (!coordinateAware) return;
  if ((anchor.right || anchor.bottom) && pos && workArea) {
    // 新尺寸必须用自己算出的 physical：setSize 是异步生效的，紧接着读
    // outerSize 会拿到旧值（Windows 实测拿到过 ~0），把窗口锚出屏幕。
    const workRight = workArea.position.x + workArea.size.width;
    const workBottom = workArea.position.y + workArea.size.height;
    const nextX = anchor.right ? workRight - physical.width : pos.x;
    const nextY = anchor.bottom ? workBottom - physical.height : pos.y;
    await appWindow
      .setPosition(new api.PhysicalPosition(Math.round(nextX), Math.round(nextY)))
      .catch(() => {});
  }
  // 变宽/变高可能把窗口顶出屏幕（固定态没有拖拽区，一出去就够不着了）；
  // 钳位用刚算出的 physical（setSize 异步生效，此刻读 outerSize 是旧值）；
  // 完全掉出所有屏幕时居中找回，胶囊条永远看得见、够得着。
  const clamped = await clampIntoWorkArea(api, appWindow, physical);
  if (!clamped) await appWindow.center().catch(() => {});
}

/// 小组件内容（Agent 行数）变化时只调高度，宽度恒为 320，不走 hide/show。
/// 上限取工作区高度留 48px 呼吸位（CSS px），超出部分由列表内部滚动承担。
async function resizeCompactWindow({ height }) {
  if (isMacPlatform()) return;
  const api = await windowApi();
  if (!api) return;
  const appWindow = api.getCurrentWindow();
  const size = WINDOW_SIZES.compact;
  let targetHeight = Math.max(size.minHeight, Math.round(height));
  const [factor, monitor] = await Promise.all([
    appWindow.scaleFactor().catch(() => 1),
    api.currentMonitor().catch(() => null),
  ]);
  if (monitor?.workArea?.size?.height) {
    const capCss = monitor.workArea.size.height / factor / uiScale - 48;
    targetHeight = Math.min(targetHeight, Math.max(size.minHeight, Math.floor(capCss)));
  }
  let physical = await scaledPhysicalSize(
    api,
    appWindow,
    size.width,
    targetHeight,
    uiScale,
    factor,
  );
  await appWindow.setSize(physical).catch((error) => {
    console.warn("Unable to resize the compact window.", error);
  });
  physical = await reconcileFloatingSizeAfterShow(
    api,
    appWindow,
    size.width,
    targetHeight,
    uiScale,
    physical,
  );
  // 高度收敛值记作下次变形的首帧。
  rememberCompactHeight(targetHeight);
  if (isLinuxPlatform() && !(await supportsGlobalWindowCoordinates())) return;
  // 变高可能把窗口底边顶出屏幕；钳位用刚算出的 physical（setSize 异步生效，
  // 此刻读 outerSize 是旧值）；完全掉出所有屏幕时居中找回。
  const clamped = await clampIntoWorkArea(api, appWindow, physical);
  if (!clamped) await appWindow.center().catch(() => {});
}

/// DPI 变化（拖到另一台显示器、系统改缩放）后按当前缩放系数重算 compact
/// 物理尺寸：zoom 不变、不 hide/show，只把视口校正回 320 CSS px。
/// 否则 zoom 与物理尺寸失配时视口缩成 ~256px，320 的最小内容宽度被裁。
async function reassertCompactSize(scaleFactor = null) {
  if (isMacPlatform()) return;
  const api = await windowApi();
  if (!api) return;
  const appWindow = api.getCurrentWindow();
  const size = WINDOW_SIZES.compact;
  await applyWebviewZoom(uiScale);
  const physical = await scaledPhysicalSize(
    api,
    appWindow,
    size.width,
    compactContentHeight(size.height),
    uiScale,
    scaleFactor,
  );
  await appWindow.setSize(physical).catch((error) => {
    console.warn("Unable to reassert the compact window size.", error);
  });
  await reconcileFloatingSizeAfterShow(
    api,
    appWindow,
    size.width,
    compactContentHeight(size.height),
    uiScale,
    physical,
  );
}

/// strip 不能只靠 DOM ResizeObserver：跨屏时 WebView 的 CSS 视口可能仍认为自己
/// 完整，真正被裁的是外层 HWND。DPI 事件必须像 compact 一样直接重断言。
async function reassertStripSize({ width, height, scaleFactor = null }) {
  if (isMacPlatform()) return;
  const api = await windowApi();
  if (!api) return;
  const appWindow = api.getCurrentWindow();
  const size = WINDOW_SIZES.strip;
  const targetWidth = Math.max(size.minWidth, Math.round(width || size.width));
  const targetHeight = Math.max(size.minHeight, Math.round(height || size.height));
  await applyWebviewZoom(stripScale);
  const physical = await scaledPhysicalSize(
    api,
    appWindow,
    targetWidth,
    targetHeight,
    stripScale,
    scaleFactor,
  );
  await appWindow.setSize(physical).catch((error) => {
    console.warn("Unable to reassert the strip window size.", error);
  });
  await reconcileFloatingSizeAfterShow(
    api,
    appWindow,
    targetWidth,
    targetHeight,
    stripScale,
    physical,
  );
}

/// macOS 菜单栏面板的高度跟随内容（宽度恒为 compact 设计宽，不做缩放——
/// 面板是系统 UI 的一部分，尺寸固定；缩放系数只属于 Windows 小插件）。
/// 面板顶部锚定菜单栏图标，长高向下延伸——macos.rs 的 resize_panel 会
/// 在尺寸变化后重算锚点，不会漂移。高度按屏幕可用高钳一次，
/// 面板不能长出屏幕底。
async function resizeMacosPanel({ width = WINDOW_SIZES.compact.width, height }) {
  if (!isDesktop() || !isMacPlatform()) return;
  const maxHeight = Math.max(40, Math.floor((window.screen?.availHeight || 900) - 80));
  await invoke("resize_macos_panel", {
    width: Math.round(width),
    height: Math.min(Math.round(height), maxHeight),
  }).catch(() => {});
}

/// 显示器 DPI 变化时回调；调用方据此重算悬浮形态的物理尺寸。
async function onScaleFactorChanged(handler) {
  if (!isDesktop() || isMacPlatform()) return () => {};
  const api = await windowApi();
  if (!api) return () => {};
  const unlistenPromise = api
    .getCurrentWindow()
    .onScaleChanged(({ payload }) => handler(payload));
  return async () => {
    const unlisten = await unlistenPromise.catch(() => null);
    unlisten?.();
  };
}

/// 返回实际生效的材质："native"（系统模糊已启用）、"alpha"（真实窗口
/// Alpha）、"css"（原生不可用，由 CSS 近实心玻璃承担外观）或 "off"。
async function setWindowGlass(enabled, radius = 12, tintStyle = "dark") {
  if (!isDesktop()) {
    return resolveGlassMode({
      enabled,
      tintStyle,
      nativeAvailable: false,
      trueAlphaAvailable: false,
    });
  }
  if (isWindowsPlatform()) {
    // Keep one immutable composition strategy for the entire lifetime of the
    // Windows widget. Switching the HWND between HostBackdrop/Acrylic and clear
    // alpha leaves WebView2 on a white redirection surface on current Win11.
    // Coffee CLI avoids that failure by relying on the creation-time
    // `transparent: true` WebView and drawing every material as a single CSS
    // surface. Do not reset WebView2's background at runtime: that also turns
    // external backdrop-filter sampling into an opaque white capture surface.
    return resolveWindowsGlassComposition({ enabled, tintStyle }).mode;
  }
  if (isLinuxPlatform()) {
    // WebKitGTK/Wayland 没有跨 GNOME、KDE 和 X11 都一致的原生 blur 协议。
    // 明确使用 CSS 表面，避免 setEffects 看似成功却渲染成不透明/黑底。
    return resolveGlassMode({
      enabled,
      tintStyle,
      nativeAvailable: false,
      trueAlphaAvailable: false,
    });
  }
  const api = await windowApi();
  if (!api) return enabled ? "css" : "off";
  const appWindow = api.getCurrentWindow();
  if (!enabled) {
    await appWindow.clearEffects();
    return "off";
  }
  if (isMacPlatform()) {
    // WKWebView 的不透明底色会盖住窗口的 vibrancy 层，先让它透明。
    await makeWebviewTransparent();
  }
  try {
    // macOS 的 vibrancy 是单选的：hudWindow 是系统 HUD 浮层一族的材质，
    // 比 menu 更薄更清透，跟随系统外观。面板是 nonactivating（永远不会成为
    // key window），state 必须锁 active，否则材质一直是失焦的发灰态。
    // 浓度不在这里调——CSS 按滑杆在 vibrancy 之上叠深浅可调的罩层。
    await appWindow.setEffects(
      isMacPlatform()
        ? { effects: ["hudWindow"], state: "active", radius }
        : { effects: ["blur"] },
    );
    return "native";
  } catch (error) {
    console.warn("Native window effects unavailable, using CSS glass.", error);
    return "css";
  }
}

const DOCK_TRIGGER_PX = 8;
const DOCK_PEEK_PX = 6;
const DOCK_HIDE_DELAY_MS = 900;
const DOCK_POLL_MS = 250;

let pinnedHoverBehaviorQueue = Promise.resolve();
let pinnedHoverEnabled = false;
let pinnedHoverNative = false;
let pinnedHoverInputsInstalled = false;
let pinnedHoverTargetOpacity = 0;

function applyPinnedHoverAppearance(inside) {
  const root = document.documentElement;
  root.dataset.pinnedHoverActive = pinnedHoverEnabled && inside ? "true" : "false";
}

/// Wayland/浏览器预览没有全局坐标，只能在窗口仍能收到的本地边界事件上回落。
/// X11 的主通道直接改 GTK 顶层窗口 opacity，不经过这里或 WebKit 事件。
function ensurePinnedHoverInputs() {
  if (pinnedHoverInputsInstalled) return;
  pinnedHoverInputsInstalled = true;
  const onPointerOver = () => {
    if (pinnedHoverEnabled && !pinnedHoverNative) applyPinnedHoverAppearance(true);
  };
  const onPointerOut = (event) => {
    if (pinnedHoverEnabled && !pinnedHoverNative && !event.relatedTarget) {
      applyPinnedHoverAppearance(false);
    }
  };
  window.addEventListener("pointerover", onPointerOver, true);
  window.addEventListener("pointerout", onPointerOut, true);
}

/// 串行同步置顶悬停监视器。窗口变形和 React StrictMode 都可能连续重断言
/// 置顶状态；若让这些 IPC 并发，迟到的旧请求会覆盖最终配置。
function setPinnedHoverBehavior(enabled) {
  // 置顶悬停（含原生 X11 与 Wayland 本地回落）是 Linux shell 专属能力。
  // Windows 继续保留原有的置顶窗口交互，不加载状态、事件或 CSS 回落。
  if (!isLinuxPlatform()) return Promise.resolve();
  const requested = Boolean(enabled);
  const requestedOpacity = pinnedHoverTargetOpacity;
  pinnedHoverBehaviorQueue = pinnedHoverBehaviorQueue
    .catch(() => {})
    .then(async () => {
      ensurePinnedHoverInputs();
      pinnedHoverEnabled = requested;
      // Linux 先按原生通道处理，避免 IPC 返回前的 pointerover 把 CSS opacity
      // 置零；命令若确认是 Wayland/连接失败，再切到本地回落。
      pinnedHoverNative = requested && isDesktop() && isLinuxPlatform();
      applyPinnedHoverAppearance(false);

      let native = false;
      if (isDesktop()) {
        native = await invoke("configure_pinned_hover", {
          enabled: requested,
          targetOpacity: requestedOpacity,
        })
          .then(Boolean)
          .catch((error) => {
            console.warn("Unable to configure pinned hover behavior.", error);
            return false;
          });
      }
      pinnedHoverNative = requested && native;
      document.documentElement.dataset.pinnedHoverDriver = pinnedHoverNative
        ? "native-x11"
        : "local";
      if (!pinnedHoverNative && requested) {
        const hovered = document.querySelector(".widget-shell:hover, .strip-shell:hover");
        applyPinnedHoverAppearance(Boolean(hovered));
      }
    });
  return pinnedHoverBehaviorQueue;
}

function setPinnedHoverTargetOpacity(opacity) {
  if (!isLinuxPlatform()) return;
  const parsed = Number(opacity);
  pinnedHoverTargetOpacity = Number.isFinite(parsed)
    ? Math.min(1, Math.max(0, parsed))
    : 0;
  if (pinnedHoverEnabled) setPinnedHoverBehavior(true);
}

/// 边缘挂靠：未固定的卡片和胶囊条可贴四边自动收起，只留一条细边。
/// 细边落在窗口的非客户区，webview 收不到 hover，因此以全局光标位置判断显示。
async function startEdgeDock({ getMode, getPinned }) {
  // Wayland 不提供全局指针与窗口坐标；Linux 的 X11 会话可以正常启用。
  if (isMacPlatform() || (isLinuxPlatform() && !(await supportsGlobalWindowCoordinates()))) {
    return () => {};
  }
  const api = await windowApi();
  if (!api) return () => {};
  const win = api.getCurrentWindow();
  let dock = null; // { edge, x, y, width, height, left, top, right, bottom, scale }
  let hidden = false;
  let disposed = false;
  let outsideSinceMs = null;
  let checkTimer;
  let pollTimer;

  const canDock = () => getMode() === "compact" || getMode() === "strip";
  const clamp = (value, min, max) => Math.max(min, Math.min(max, value));
  const peek = () => Math.round(DOCK_PEEK_PX * dock.scale);
  const exposedPosition = () => ({ x: dock.x, y: dock.y });
  const hiddenPosition = () => {
    const visible = peek();
    switch (dock.edge) {
      case "bottom": return { x: dock.x, y: dock.bottom - visible };
      case "left": return { x: dock.left - dock.width + visible, y: dock.y };
      case "right": return { x: dock.right - visible, y: dock.y };
      default: return { x: dock.x, y: dock.top - dock.height + visible };
    }
  };
  const slideTo = async (position) => {
    if (!dock) return;
    await win.setPosition(new api.PhysicalPosition(position.x, position.y)).catch(() => {});
  };

  const stopPoll = () => {
    window.clearInterval(pollTimer);
    pollTimer = undefined;
  };

  const undock = async () => {
    if (dock && hidden) await slideTo(exposedPosition());
    dock = null;
    hidden = false;
    outsideSinceMs = null;
    stopPoll();
    await win.setAlwaysOnTop(Boolean(getPinned())).catch(() => {});
  };

  const poll = async () => {
    if (disposed || !dock) return;
    if (getPinned() || !canDock()) {
      await undock();
      return;
    }
    const cursor = await api.cursorPosition().catch(() => null);
    if (!cursor) return;
    if (hidden) {
      const visible = peek();
      const onStrip = dock.edge === "bottom"
        ? cursor.x >= dock.x && cursor.x <= dock.x + dock.width && cursor.y >= dock.bottom - visible && cursor.y <= dock.bottom + visible
        : dock.edge === "left"
          ? cursor.y >= dock.y && cursor.y <= dock.y + dock.height && cursor.x >= dock.left - visible && cursor.x <= dock.left + visible
          : dock.edge === "right"
            ? cursor.y >= dock.y && cursor.y <= dock.y + dock.height && cursor.x >= dock.right - visible && cursor.x <= dock.right + visible
            : cursor.x >= dock.x && cursor.x <= dock.x + dock.width && cursor.y >= dock.top - visible && cursor.y <= dock.top + visible;
      if (onStrip) {
        hidden = false;
        outsideSinceMs = null;
        await slideTo(exposedPosition());
      }
      return;
    }
    const insideWindow =
      cursor.x >= dock.x && cursor.x <= dock.x + dock.width
      && cursor.y >= dock.y && cursor.y <= dock.y + dock.height;
    if (insideWindow) {
      outsideSinceMs = null;
      return;
    }
    outsideSinceMs = outsideSinceMs ?? Date.now();
    if (Date.now() - outsideSinceMs >= DOCK_HIDE_DELAY_MS) {
      hidden = true;
      outsideSinceMs = null;
      await slideTo(hiddenPosition());
    }
  };

  const check = async () => {
    if (disposed) return;
    if (!canDock() || getPinned()) {
      if (dock) await undock();
      return;
    }
    let pos;
    let size;
    let monitor;
    try {
      [pos, size, monitor] = await Promise.all([
        win.outerPosition(),
        win.outerSize(),
        api.currentMonitor(),
      ]);
    } catch {
      return;
    }
    if (!pos || !size || !monitor) return;
    const workArea = monitor.workArea || { position: monitor.position, size: monitor.size };
    const left = workArea.position.x;
    const top = workArea.position.y;
    const right = left + workArea.size.width;
    const bottom = top + workArea.size.height;
    const scale = monitor.scaleFactor || 1;
    if (hidden && dock) {
      const parked = hiddenPosition();
      if (Math.abs(pos.x - parked.x) <= 2 && Math.abs(pos.y - parked.y) <= 2) return;
    }
    const nearest = [
      { edge: "left", distance: Math.abs(pos.x - left) },
      { edge: "right", distance: Math.abs(pos.x + size.width - right) },
      { edge: "top", distance: Math.abs(pos.y - top) },
      { edge: "bottom", distance: Math.abs(pos.y + size.height - bottom) },
    ].sort((a, b) => a.distance - b.distance)[0];
    if (!nearest || nearest.distance > Math.round(DOCK_TRIGGER_PX * scale)) {
      if (dock) await undock();
      return;
    }
    const edge = nearest.edge;
    const x = edge === "left" ? left : edge === "right" ? right - size.width
      : clamp(pos.x, left, Math.max(left, right - size.width));
    const y = edge === "top" ? top : edge === "bottom" ? bottom - size.height
      : clamp(pos.y, top, Math.max(top, bottom - size.height));
    dock = { edge, x, y, width: size.width, height: size.height, left, top, right, bottom, scale };
    hidden = false;
    outsideSinceMs = null;
    await slideTo(exposedPosition());
    await win.setAlwaysOnTop(true).catch(() => {});
    if (!pollTimer) pollTimer = window.setInterval(poll, DOCK_POLL_MS);
  };

  const onMove = () => {
    window.clearTimeout(checkTimer);
    checkTimer = window.setTimeout(check, 220);
  };
  const unlistenPromise = win.onMoved(onMove);
  check();

  return async () => {
    disposed = true;
    stopPoll();
    window.clearTimeout(checkTimer);
    const unlisten = await unlistenPromise.catch(() => null);
    unlisten?.();
    if (dock) await undock();
  };
}

/// 让完整视图的原生窗口主题跟随用户选择（macOS 标题栏）；"自动"传 null 交回系统。
/// 后端只在 macOS 生效，其它平台与非桌面环境安静跳过。
async function setNativeTheme(theme) {
  if (!isDesktop()) return;
  await invoke("set_native_theme", { theme: theme ?? null }).catch(() => {});
}

async function setWindowPinned(pinned) {
  const api = await windowApi();
  if (!api) return;
  await api.getCurrentWindow().setAlwaysOnTop(pinned);
  // 悬停监视器与原生置顶状态共用同一条已串行化的窗口动作链，避免 React
  // effect 的严格模式清理晚到一步、把已经置顶的监视器再次关掉。
  if (isLinuxPlatform()) await setPinnedHoverBehavior(pinned);
}

/// Linux 托盘菜单的“置顶/取消置顶”请求。其 payload 是后端已经切换过的目标值，
/// 前端只负责把窗口与持久化状态同步到该值。
async function onTrayPinnedChange(handler) {
  if (!isDesktop() || !isLinuxPlatform()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen("tray://set-pinned", (event) => handler(Boolean(event.payload)));
}

/// 从设置页变更置顶后刷新 Linux 托盘项的文字；其它平台没有该菜单。
async function syncLinuxTrayPinned(pinned) {
  if (!isDesktop() || !isLinuxPlatform()) return;
  await invoke("sync_linux_tray_pinned", { pinned: Boolean(pinned) }).catch(() => {});
}

async function autostartApi() {
  if (!isDesktop()) return null;
  return import("@tauri-apps/plugin-autostart");
}

/// 检查更新（设置页手动点击，或启动后及持续运行时每天自动检查；后者可在设置关闭）。
/// 返回 null 表示已是最新（或非桌面环境）。
async function checkForUpdate() {
  if (!isDesktop()) return null;
  const { check } = await import("@tauri-apps/plugin-updater");
  const update = await check();
  if (!update) return null;
  return { version: update.version, notes: update.body || "", update };
}

/// 下载并安装；安装包的 minisign 签名由更新器校验，签名不符会直接失败。
async function installUpdate(update, onProgress) {
  let downloaded = 0;
  let total = 0;
  await update.downloadAndInstall((event) => {
    if (event.event === "Started") {
      total = event.data.contentLength || 0;
    } else if (event.event === "Progress") {
      downloaded += event.data.chunkLength || 0;
      onProgress?.(total ? Math.min(100, Math.round((downloaded / total) * 100)) : null);
    }
  });
  const { relaunch } = await import("@tauri-apps/plugin-process");
  await relaunch();
}

/// 开机自启状态；浏览器演示模式返回 null（设置页据此隐藏该项）。
async function getAutostart() {
  const api = await autostartApi();
  if (!api) return null;
  return api.isEnabled().catch(() => null);
}

async function setAutostart(enabled) {
  const api = await autostartApi();
  if (!api) throw new Error("浏览器演示模式不能配置开机启动");
  if (enabled) await api.enable();
  else await api.disable();
  return api.isEnabled().catch(() => enabled);
}

async function minimizeWindow() {
  const api = await windowApi();
  if (!api) return;
  await api.getCurrentWindow().minimize();
}

async function closeWindow() {
  const api = await windowApi();
  if (!api) return;
  await api.getCurrentWindow().close();
}

async function toggleMaximizeWindow() {
  const api = await windowApi();
  if (!api) return;
  await api.getCurrentWindow().toggleMaximize();
}

export {
  UI_SCALE_RANGE,
  WINDOW_SIZES,
  applyStartupUiScale,
  applyWindowMode,
  broadcastMacAgentSelection,
  broadcastMacAppearance,
  checkForUpdate,
  closeWindow,
  getAutostart,
  getMacAgentSelection,
  installUpdate,
  isDesktop,
  isLinuxPlatform,
  isMacPlatform,
  isWindowsPlatform,
  minimizeWindow,
  normalizeUiScale,
  onMacAgentSelection,
  onMacAppearance,
  onScaleFactorChanged,
  onTrayPinnedChange,
  onTrayShowExpanded,
  openExpandedWindow,
  readStripScale,
  readUiScale,
  reassertCompactSize,
  reassertStripSize,
  resizeCompactWindow,
  resizeMacosPanel,
  resizeStripWindow,
  restoreWindowPosition,
  setAutostart,
  setNativeTheme,
  updateMacStatusItems,
  updateTrayQuotaBadge,
  setStripScale,
  setWindowGlass,
  setPinnedHoverBehavior,
  setPinnedHoverTargetOpacity,
  setWindowPinned,
  setWindowUiScale,
  startEdgeDock,
  startPositionMemory,
  syncLinuxTrayPinned,
  stripContentSize,
  toggleMaximizeWindow,
};
