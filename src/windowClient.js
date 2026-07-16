import { invoke } from "@tauri-apps/api/core";

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
  const [pos, monitor] = await Promise.all([
    appWindow.outerPosition().catch(() => null),
    api.currentMonitor().catch(() => null),
  ]);
  if (!pos) return;
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
  return typeof navigator !== "undefined" && navigator.userAgent.includes("Windows");
}

/// macOS 上小插件是菜单栏面板（NSPanel）：位置由托盘图标决定，形态不变，
/// 窗口按钮/边缘挂靠/位置记忆/置顶全部由平台语义取代，见 docs/superpowers/specs。
function isMacPlatform() {
  return typeof navigator !== "undefined" && navigator.userAgent.includes("Mac");
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

async function applyWindowMode(mode, options = {}) {
  // macOS 上两种形态是两个窗口，各自固定；没有"变形"这件事。
  if (isMacPlatform()) return;

  const api = await windowApi();
  if (!api) return;

  const appWindow = api.getCurrentWindow();
  const size = WINDOW_SIZES[mode] || WINDOW_SIZES.compact;

  if (mode === "expanded") {
    // 小插件不占任务栏；完整视图是常规窗口，要出现在任务栏里。
    // 无边框窗口的任务栏按钮由 WS_EX_APPWINDOW 决定，setSkipTaskbar 补不上它，
    // 所以走后端改窗口样式；样式必须在隐藏状态下改，重新显示后 shell 才重读。
    await appWindow.hide().catch(() => {});
    await appWindow.setSkipTaskbar(false).catch(() => {});
    await invoke("set_taskbar_button", { visible: true }).catch(() => {});
    lastPositions.compact = await appWindow.outerPosition().catch(() => null);
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
  await appWindow.setSkipTaskbar(true).catch(() => {});
  await invoke("set_taskbar_button", { visible: false }).catch(() => {});
  await appWindow.setMinSize(null);
  await appWindow.setMaximizable(false);

  if (mode === "strip") {
    const width = Math.max(size.minWidth, Math.round(options.width || size.width));
    const height = Math.max(size.minHeight, Math.round(options.height || size.height));
    await appWindow.setSize(new api.LogicalSize(width, height));
    await appWindow.setResizable(false);
    // 有记忆位置回记忆位置；首次进入保持当前位置（出现在卡片原地）。
    const storedStrip = readStoredPosition("strip");
    const stripTarget =
      lastPositions.strip ||
      (storedStrip ? new api.PhysicalPosition(storedStrip.x, storedStrip.y) : null);
    if (stripTarget) await appWindow.setPosition(stripTarget).catch(() => {});
    // 最终坐标不在任何屏幕的可见区域内（挂靠残留、拔了扩展屏等）时居中，
    // 保证胶囊条永远出现在看得见的地方。
    const [pos, outer, monitors] = await Promise.all([
      appWindow.outerPosition().catch(() => null),
      appWindow.outerSize().catch(() => null),
      api.availableMonitors().catch(() => []),
    ]);
    const onScreen = Boolean(
      pos &&
        (monitors || []).some(
          (monitor) =>
            pos.x + (outer?.width || 0) > monitor.position.x &&
            pos.x < monitor.position.x + monitor.size.width &&
            pos.y + (outer?.height || 0) > monitor.position.y &&
            pos.y < monitor.position.y + monitor.size.height,
        ),
    );
    if (!onScreen) await appWindow.center().catch(() => {});
    await appWindow.show().catch(() => {});
    await appWindow.setFocus().catch(() => {});
    return;
  }

  await appWindow.setSize(new api.LogicalSize(size.width, size.height));
  await appWindow.setResizable(false);
  await appWindow.setMinSize(new api.LogicalSize(size.minWidth, size.minHeight));

  const stored = readStoredPosition("compact");
  const target =
    lastPositions.compact || (stored ? new api.PhysicalPosition(stored.x, stored.y) : null);
  if (target) {
    await appWindow.setPosition(target).catch(() => appWindow.center());
  } else {
    await appWindow.center();
  }
  await appWindow.show().catch(() => {});
  await appWindow.setFocus().catch(() => {});
}

/// 胶囊条格数或方向变化时只调尺寸，不走 hide/show，避免闪烁。
async function resizeStripWindow({ width, height }) {
  const api = await windowApi();
  if (!api) return;
  const size = WINDOW_SIZES.strip;
  const targetWidth = Math.max(size.minWidth, Math.round(width || size.width));
  const targetHeight = Math.max(size.minHeight, Math.round(height || size.height));
  await api
    .getCurrentWindow()
    .setSize(new api.LogicalSize(targetWidth, targetHeight))
    .catch(() => {});
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
async function setWindowGlass(enabled) {
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
    // macOS 的 vibrancy 是单选的：只给一个材质。hudWindow 是深色 HUD 材质，
    // 不随系统浅色模式变白，和这套固定深色的配色一致（浅色主题是另一件事）。
    await appWindow.setEffects(
      isMacPlatform()
        ? { effects: ["hudWindow"], state: "active", radius: 12 }
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

/// 手动检查更新：只有用户点击时才发出这一个网络请求，不后台轮询。
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
  applyWindowMode,
  checkForUpdate,
  closeWindow,
  getAutostart,
  installUpdate,
  isDesktop,
  isMacPlatform,
  minimizeWindow,
  openExpandedWindow,
  resizeStripWindow,
  restoreWindowPosition,
  setAutostart,
  setNativeTheme,
  setWindowGlass,
  setWindowPinned,
  startEdgeDock,
  startPositionMemory,
};
