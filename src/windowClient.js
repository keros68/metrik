import { invoke } from "@tauri-apps/api/core";
import { platform as tauriPlatform } from "@tauri-apps/plugin-os";
import { detectRuntimePlatform } from "./platformDetection";

const WINDOW_SIZES = {
  compact: { width: 320, height: 320, minWidth: 320, minHeight: 320 },
  expanded: { width: 1120, height: 760, minWidth: 960, minHeight: 700 },
  strip: { width: 240, height: 40, minWidth: 48, minHeight: 40 },
};

// 卡片/胶囊的整体缩放档位（1 / 1.25 / 1.5）。窗口尺寸与页面 zoom 同乘一个系数，
// 比例不变所以不会变形；expanded 不参与（窗口本身可自由缩放）。
const UI_SCALE_KEY = "metrik:uiScale";
const UI_SCALE_STEPS = [1, 1.25, 1.5];

function readStoredUiScale() {
  try {
    const stored = Number(localStorage.getItem(UI_SCALE_KEY));
    return UI_SCALE_STEPS.includes(stored) ? stored : 1;
  } catch {
    return 1;
  }
}

let uiScale = readStoredUiScale();

function setWindowUiScale(scale) {
  uiScale = UI_SCALE_STEPS.includes(scale) ? scale : 1;
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

/// compact/strip 的窗口尺寸：乘缩放档位后取整到物理像素再下发，
/// 避免分数 DPI（125%/150%）下逻辑尺寸取整产生的半像素裁切。
async function scaledPhysicalSize(api, appWindow, width, height) {
  const factor = await appWindow.scaleFactor().catch(() => 1);
  return new api.PhysicalSize(
    Math.round(width * uiScale * factor),
    Math.round(height * uiScale * factor),
  );
}

/// 内容缩放用 WebView 原生 zoom（等同浏览器 Ctrl+缩放）：视口单位、媒体查询
/// 全部自洽。CSS zoom 做不到——100vw 元素在 zoom 下会溢出视口（实测）。
async function applyWebviewZoom(factor) {
  if (!isDesktop()) return;
  const api = await import("@tauri-apps/api/webview");
  await api
    .getCurrentWebview()
    .setZoom(factor)
    .catch(() => {});
}

/// 启动时就地应用缩放档位（不走 applyWindowMode 的 hide/show，避免闪烁）。
/// strip 的启动尺寸由 strip 专属 effect 走 applyWindowMode，这里只管 compact。
async function applyStartupUiScale(mode) {
  if (isMacPlatform()) return;
  const api = await windowApi();
  if (!api) return;
  await applyWebviewZoom(uiScale);
  if (mode !== "compact" || uiScale === 1) return;
  const appWindow = api.getCurrentWindow();
  const size = WINDOW_SIZES.compact;
  await appWindow.setSize(
    await scaledPhysicalSize(api, appWindow, size.width, compactContentHeight(size.height)),
  );
  await clampIntoWorkArea(api, appWindow);
}

/// 窗口重设尺寸后可能伸出屏幕（固定状态下竖条切横条最典型：位置不动、
/// 宽度暴涨，控制按钮全在屏幕外，固定态又没有拖拽区，用户就被锁死了）。
/// 把窗口钳回重叠面积最大的显示器工作区内；完全不与任何屏幕重叠时
/// 返回 false，由调用方居中。
async function clampIntoWorkArea(api, appWindow) {
  const [pos, outer, monitors] = await Promise.all([
    appWindow.outerPosition().catch(() => null),
    appWindow.outerSize().catch(() => null),
    api.availableMonitors().catch(() => []),
  ]);
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
  if (!target) return;
  const current = await appWindow.outerPosition().catch(() => null);
  if (!current) return;
  if (Math.abs(current.x - target.x) <= 2 && Math.abs(current.y - target.y) <= 2) return;
  await appWindow
    .setPosition(new api.PhysicalPosition(Math.round(target.x), Math.round(target.y)))
    .catch(() => {});
  await clampIntoWorkArea(api, appWindow);
}

// compact 与 strip 各自记位，互不覆盖；expanded 不记位。
const POSITION_KEYS = {
  compact: "metrik:widgetPosition",
  strip: "metrik:stripPosition",
};

const lastPositions = { compact: null, strip: null };

function isDesktop() {
  return typeof window !== "undefined" && Boolean(window.__TAURI_INTERNALS__);
}

function readStoredPosition(mode) {
  const key = POSITION_KEYS[mode];
  if (!key) return null;
  try {
    const raw = JSON.parse(localStorage.getItem(key) || "null");
    if (!raw || !Number.isFinite(raw.x) || !Number.isFinite(raw.y)) return null;
    return raw;
  } catch {
    return null;
  }
}

/// 记住窗口的物理坐标（按形态分开记）；边缘挂靠把窗口滑出屏幕时不记，
/// 避免下次开机在屏外。
async function rememberWindowPosition(api, appWindow, mode) {
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
    const top = monitor.position.y;
    const workBottom = monitor.workArea
      ? monitor.workArea.position.y + monitor.workArea.size.height
      : top + monitor.size.height;
    // 滑出上缘（挂靠）或压进任务栏下面的坐标都不记，
    // 避免下次开机窗口停在够不着的地方。
    if (pos.y < top || pos.y > workBottom - 24) return;
  }
  lastPositions[mode] = pos;
  localStorage.setItem(key, JSON.stringify({ x: pos.x, y: pos.y }));
}

/// 启动时把窗口放回该形态上次的位置；坐标已不在任何显示器上（拔了扩展屏等）时居中。
async function restoreWindowPosition(mode = "compact") {
  // macOS 面板永远贴着菜单栏图标，没有"上次的位置"这回事。
  if (isMacPlatform()) return;
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
  await appWindow.setPosition(lastPositions[mode]).catch(() => {});
}

/// 托盘右键"显示完整视图"：让用户从胶囊/卡片直达完整视图，不必先弹出卡片
/// 再点展开。变形仍由前端做（Windows 单窗口变形），托盘只发意图。
/// macOS 的完整视图是独立窗口，由 macos.rs 的菜单栏负责，不发这个事件。
async function onTrayShowExpanded(handler) {
  if (!isDesktop()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen("tray://show-expanded", () => handler());
}

/// 拖动结束后持久化窗口位置（compact 与 strip 各记各的；expanded 不记）。
async function startPositionMemory(getMode) {
  if (isMacPlatform()) return () => {};
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

async function applyWindowMode(mode, options = {}) {
  // macOS 的完整视图是独立窗口；菜单栏 NSPanel 只保留 compact 卡片。
  if (isMacPlatform()) {
    if (!isDesktop()) return;
    const size = WINDOW_SIZES.compact;
    const width = size.width;
    // 首帧直接用上次内容测量的高度（同 Windows 的尺寸缓存），避免两段式跳变。
    const height = compactContentHeight(size.height);
    await invoke("resize_macos_panel", { width, height });
    return;
  }

  const api = await windowApi();
  if (!api) return;

  const appWindow = api.getCurrentWindow();
  const size = WINDOW_SIZES[mode] || WINDOW_SIZES.compact;

  // 变形前记下离开的悬浮形态的坐标：compact/strip 各记各的，互不污染
  // （以前无条件写 lastPositions.compact，从胶囊条进大视图会把小插件的
  // 记忆污染成胶囊条坐标，回来时就"复位"了）。同形态重入（启动恢复、
  // 自检重断言）不是变形，不记。
  if (POSITION_KEYS[options.fromMode] && options.fromMode !== mode) {
    lastPositions[options.fromMode] = await appWindow.outerPosition().catch(() => null);
  }

  if (mode === "expanded") {
    // 完整视图是常规窗口：一律解除置顶。固定（置顶 + 锁位置）只属于
    // compact/strip 悬浮形态，否则 1120x760 的大窗口盖住所有应用切不走。
    await appWindow.setAlwaysOnTop(false).catch(() => {});
    // 小插件不占任务栏；完整视图是常规窗口，要出现在任务栏里。
    // 无边框窗口的任务栏按钮由 WS_EX_APPWINDOW 决定，setSkipTaskbar 补不上它，
    // 所以走后端改窗口样式；样式必须在隐藏状态下改，重新显示后 shell 才重读。
    await appWindow.hide().catch(() => {});
    // 缩放档位只作用卡片/胶囊，完整视图恢复 1:1；藏起来再改，避免闪缩放跳变。
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
    await appWindow.center();
    await appWindow.show().catch(() => {});
    await appWindow.setFocus().catch(() => {});
    return;
  }

  if (await appWindow.isMaximized().catch(() => false)) {
    await appWindow.unmaximize();
  }
  await appWindow.hide().catch(() => {});
  // 卡片/胶囊按用户的缩放档位显示；藏起来再改，避免闪缩放跳变。
  await applyWebviewZoom(uiScale);
  await appWindow.setSkipTaskbar(true).catch(() => {});
  await invoke("set_taskbar_button", { visible: false }).catch(() => {});
  await appWindow.setMinSize(null);
  await appWindow.setMaximizable(false);

  if (mode === "strip") {
    const width = Math.max(size.minWidth, Math.round(options.width || size.width));
    const height = Math.max(size.minHeight, Math.round(options.height || size.height));
    await appWindow.setSize(await scaledPhysicalSize(api, appWindow, width, height));
    await appWindow.setResizable(false);
    // 有记忆位置回记忆位置；首次进入保持当前位置（出现在卡片原地）。
    const storedStrip = readStoredPosition("strip");
    const stripTarget =
      lastPositions.strip ||
      (storedStrip ? new api.PhysicalPosition(storedStrip.x, storedStrip.y) : null);
    if (stripTarget) await appWindow.setPosition(stripTarget).catch(() => {});
    // 伸出屏幕的部分钳回工作区（挂靠残留、方向切换变宽等）；
    // 完全不在任何屏幕上（拔了扩展屏等）时居中，胶囊条永远看得见、够得着。
    const clamped = await clampIntoWorkArea(api, appWindow);
    if (!clamped) await appWindow.center().catch(() => {});
    // 钳位/居中后的最终坐标是显示后校验的基准。
    const stripFinal = await appWindow.outerPosition().catch(() => null);
    await appWindow.show().catch(() => {});
    await ensurePositionAfterShow(api, appWindow, stripFinal);
    await appWindow.setFocus().catch(() => {});
    return;
  }

  await appWindow.setSize(
    await scaledPhysicalSize(api, appWindow, size.width, compactContentHeight(size.height)),
  );
  await appWindow.setResizable(false);
  await appWindow.setMinSize(new api.LogicalSize(size.minWidth * uiScale, size.minHeight * uiScale));

  const stored = readStoredPosition("compact");
  const target =
    lastPositions.compact || (stored ? new api.PhysicalPosition(stored.x, stored.y) : null);
  if (target) {
    await appWindow.setPosition(target).catch(() => appWindow.center());
    // 缩放档位调大后，记忆位置 + 新尺寸可能伸出屏幕，钳回工作区。
    await clampIntoWorkArea(api, appWindow);
  } else {
    await appWindow.center();
  }
  // 钳位/居中后的最终坐标是显示后校验的基准。
  const compactFinal = await appWindow.outerPosition().catch(() => null);
  await appWindow.show().catch(() => {});
  await ensurePositionAfterShow(api, appWindow, compactFinal);
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
  const [pos, outer, monitor] = await Promise.all([
    appWindow.outerPosition().catch(() => null),
    appWindow.outerSize().catch(() => null),
    api.currentMonitor().catch(() => null),
  ]);
  const workArea = monitor?.workArea;
  const anchor = { right: false, bottom: false };
  if (pos && outer && workArea) {
    const workRight = workArea.position.x + workArea.size.width;
    const workBottom = workArea.position.y + workArea.size.height;
    anchor.right = Math.abs(pos.x + outer.width - workRight) <= 8;
    anchor.bottom = Math.abs(pos.y + outer.height - workBottom) <= 8;
  }
  const physical = await scaledPhysicalSize(api, appWindow, targetWidth, targetHeight);
  await appWindow.setSize(physical).catch(() => {});
  // 测量收敛出的尺寸记作下次变形的首帧（否则首帧永远是常量估计）。
  rememberStripSize(targetWidth, targetHeight);
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
  // 完全掉出所有屏幕时居中找回，胶囊条永远看得见、够得着。
  const clamped = await clampIntoWorkArea(api, appWindow);
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
  await appWindow
    .setSize(await scaledPhysicalSize(api, appWindow, size.width, targetHeight))
    .catch(() => {});
  // 高度收敛值记作下次变形的首帧。
  rememberCompactHeight(targetHeight);
  // 变高可能把窗口底边顶出屏幕；完全掉出所有屏幕时居中找回。
  const clamped = await clampIntoWorkArea(api, appWindow);
  if (!clamped) await appWindow.center().catch(() => {});
}

/// DPI 变化（拖到另一台显示器、系统改缩放）后按当前缩放档位重算 compact
/// 物理尺寸：zoom 不变、不 hide/show，只把视口校正回 320 CSS px。
/// 否则 zoom 与物理尺寸失配时视口缩成 ~256px，320 的最小内容宽度被裁。
async function reassertCompactSize() {
  if (isMacPlatform()) return;
  const api = await windowApi();
  if (!api) return;
  const appWindow = api.getCurrentWindow();
  const size = WINDOW_SIZES.compact;
  await appWindow
    .setSize(
      await scaledPhysicalSize(api, appWindow, size.width, compactContentHeight(size.height)),
    )
    .catch(() => {});
  await clampIntoWorkArea(api, appWindow);
}

/// macOS 菜单栏面板的高度跟随内容（宽度恒为 compact 设计宽）。
/// 面板顶部锚定菜单栏图标，长高向下延伸——macos.rs 的 resize_panel 会
/// 在尺寸变化后重算锚点，不会漂移。
async function resizeMacosPanel({ width, height }) {
  if (!isDesktop() || !isMacPlatform()) return;
  await invoke("resize_macos_panel", { width, height }).catch(() => {});
}

/// 显示器 DPI 变化时回调；调用方据此重算悬浮形态的物理尺寸。
async function onScaleFactorChanged(handler) {
  if (!isDesktop() || isMacPlatform()) return () => {};
  const api = await windowApi();
  if (!api) return () => {};
  const unlistenPromise = api.getCurrentWindow().onScaleChanged(() => handler());
  return async () => {
    const unlisten = await unlistenPromise.catch(() => null);
    unlisten?.();
  };
}

function glassOptions() {
  // 玻璃固定为深色 HUD，不随系统主题；tint 只供旧系统的 SWCA 回退使用。
  return {
    dark: true,
    tint: [18, 20, 25, 96],
  };
}

/// 返回实际生效的材质："native"（系统模糊已启用）、"css"（原生不可用，
/// 由 CSS 近实心玻璃承担外观）或 "off"。调用方据此切换样式层。
async function setWindowGlass(enabled, radius = 12) {
  if (!isDesktop()) return enabled ? "css" : "off";
  if (isWindowsPlatform()) {
    // WebView2 has its own composition surface. Make that surface transparent
    // before applying the HWND backdrop, otherwise it masks the native material.
    await makeWebviewTransparent();
    if (!enabled) {
      await invoke("set_glass_backdrop", { enabled, ...glassOptions() });
      return "off";
    }
    try {
      await invoke("set_glass_backdrop", { enabled, ...glassOptions() });
      return "native";
    } catch (error) {
      console.warn("Native glass backdrop unavailable, using CSS glass.", error);
      return "css";
    }
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
    // macOS 的 vibrancy 是单选的：menu 是原生菜单同款材质。面板不再锁死
    // dark，而是像 CodexBar 的 NSMenu 一样跟随应用当前的系统外观。
    await appWindow.setEffects(
      isMacPlatform()
        ? { effects: ["menu"], state: "active", radius }
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

/// 边缘挂靠：小插件拖到屏幕上缘后自动收起，只留一条细边；
/// 鼠标碰到细边滑出，移开后再收回。拖离上缘即恢复普通窗口。
/// 细边落在窗口的非客户区，webview 收不到 hover 事件，
/// 所以挂靠期间用全局光标位置轮询判断进出。
async function startEdgeDock({ getMode, getPinned }) {
  // 边缘挂靠是桌面浮窗的交互；菜单栏面板不需要（它本来就贴在菜单栏上）。
  if (isMacPlatform()) return () => {};
  const api = await windowApi();
  if (!api) return () => {};
  const win = api.getCurrentWindow();
  let dock = null; // { x, width, height, exposedY, hiddenY, scale, top }
  let hidden = false;
  let disposed = false;
  let outsideSinceMs = null;
  let checkTimer;
  let pollTimer;

  const slideTo = async (y) => {
    if (!dock) return;
    await win.setPosition(new api.PhysicalPosition(dock.x, y)).catch(() => {});
  };

  const stopPoll = () => {
    window.clearInterval(pollTimer);
    pollTimer = undefined;
  };

  const undock = async () => {
    // 已收起时先滑回可见位置，再解除挂靠，避免窗口留在屏幕外。
    if (dock && hidden) await slideTo(dock.exposedY);
    dock = null;
    hidden = false;
    outsideSinceMs = null;
    stopPoll();
    await win.setAlwaysOnTop(Boolean(getPinned())).catch(() => {});
  };

  const poll = async () => {
    if (disposed || !dock) return;
    // 固定 = 锁定位置：立即解除挂靠，不再自动收起。
    // 离开 compact（折叠成胶囊条等）同样立即解除：挂靠计时器记的是旧形态的
    // 高度，继续收起会把新形态的窗口整个滑出屏幕。
    if (getPinned() || getMode() !== "compact") {
      await undock();
      return;
    }
    let cursor;
    try {
      cursor = await api.cursorPosition();
    } catch {
      return;
    }
    const stripHeight = Math.round(DOCK_PEEK_PX * dock.scale);
    const insideX = cursor.x >= dock.x && cursor.x <= dock.x + dock.width;
    if (hidden) {
      const onStrip = insideX && cursor.y <= dock.top + stripHeight;
      if (onStrip) {
        hidden = false;
        outsideSinceMs = null;
        await slideTo(dock.exposedY);
      }
      return;
    }
    const insideWindow =
      insideX && cursor.y >= dock.top && cursor.y <= dock.top + dock.height;
    if (insideWindow) {
      outsideSinceMs = null;
      return;
    }
    outsideSinceMs = outsideSinceMs ?? Date.now();
    if (Date.now() - outsideSinceMs >= DOCK_HIDE_DELAY_MS) {
      hidden = true;
      outsideSinceMs = null;
      await slideTo(dock.hiddenY);
    }
  };

  const check = async () => {
    if (disposed) return;
    if (getMode() !== "compact" || getPinned()) {
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
    const top = monitor.position.y;
    const scale = monitor.scaleFactor || 1;
    if (hidden && dock && pos.y <= dock.hiddenY + 2) {
      // 自己滑出屏幕触发的 move 事件，保持隐藏态。
      return;
    }
    if (pos.y <= top + Math.round(DOCK_TRIGGER_PX * scale)) {
      dock = {
        x: pos.x,
        width: size.width,
        height: size.height,
        top,
        scale,
        exposedY: top,
        hiddenY: top - size.height + Math.round(DOCK_PEEK_PX * scale),
      };
      hidden = false;
      outsideSinceMs = null;
      // 隐藏后只留细边，必须置顶才碰得到。
      await win.setAlwaysOnTop(true).catch(() => {});
      if (!pollTimer) pollTimer = window.setInterval(poll, DOCK_POLL_MS);
    } else if (dock) {
      await undock();
    }
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
}

async function autostartApi() {
  if (!isDesktop()) return null;
  return import("@tauri-apps/plugin-autostart");
}

/// 检查更新（设置页手动点击，或自动检查每天一次；后者可在设置关闭）。
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

export {
  WINDOW_SIZES,
  applyStartupUiScale,
  applyWindowMode,
  checkForUpdate,
  closeWindow,
  getAutostart,
  installUpdate,
  isDesktop,
  isMacPlatform,
  minimizeWindow,
  onScaleFactorChanged,
  onTrayShowExpanded,
  openExpandedWindow,
  reassertCompactSize,
  resizeCompactWindow,
  resizeMacosPanel,
  resizeStripWindow,
  restoreWindowPosition,
  setAutostart,
  setNativeTheme,
  updateMacStatusItems,
  setWindowGlass,
  setWindowPinned,
  setWindowUiScale,
  startEdgeDock,
  startPositionMemory,
  stripContentSize,
};
