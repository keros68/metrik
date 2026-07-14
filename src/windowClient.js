import { invoke } from "@tauri-apps/api/core";

const WINDOW_SIZES = {
  compact: { width: 320, height: 320, minWidth: 320, minHeight: 320 },
  expanded: { width: 1120, height: 760, minWidth: 960, minHeight: 700 },
};

let compactPosition = null;

const POSITION_KEY = "metrik:widgetPosition";

function isDesktop() {
  return typeof window !== "undefined" && Boolean(window.__TAURI_INTERNALS__);
}

function readStoredPosition() {
  try {
    const raw = JSON.parse(localStorage.getItem(POSITION_KEY) || "null");
    if (!raw || !Number.isFinite(raw.x) || !Number.isFinite(raw.y)) return null;
    return raw;
  } catch {
    return null;
  }
}

/// 记住小组件的物理坐标；边缘挂靠把窗口滑出屏幕时不记，避免下次开机在屏外。
async function rememberCompactPosition(api, appWindow) {
  const [pos, monitor] = await Promise.all([
    appWindow.outerPosition().catch(() => null),
    api.currentMonitor().catch(() => null),
  ]);
  if (!pos) return;
  if (monitor && pos.y < monitor.position.y) return;
  compactPosition = pos;
  localStorage.setItem(POSITION_KEY, JSON.stringify({ x: pos.x, y: pos.y }));
}

/// 启动时把小组件放回上次的位置；坐标已不在任何显示器上（拔了扩展屏等）时居中。
async function restoreWindowPosition() {
  const api = await windowApi();
  if (!api) return;
  const stored = readStoredPosition();
  if (!stored) return;

  const appWindow = api.getCurrentWindow();
  const [size, monitors] = await Promise.all([
    appWindow.outerSize().catch(() => null),
    api.availableMonitors().catch(() => []),
  ]);
  const width = size?.width || 320;
  const height = size?.height || 320;
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

  compactPosition = new api.PhysicalPosition(stored.x, stored.y);
  await appWindow.setPosition(compactPosition).catch(() => {});
}

/// 拖动结束后持久化小组件位置（expanded 形态不记）。
async function startPositionMemory(getMode) {
  const api = await windowApi();
  if (!api) return () => {};
  const appWindow = api.getCurrentWindow();
  let timer = null;
  const unlistenPromise = appWindow.onMoved(() => {
    if (getMode() !== "compact") return;
    window.clearTimeout(timer);
    timer = window.setTimeout(() => {
      rememberCompactPosition(api, appWindow).catch(() => {});
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

async function windowApi() {
  if (!isDesktop()) return null;
  return import("@tauri-apps/api/window");
}

async function makeWebviewTransparent() {
  const api = await import("@tauri-apps/api/webview");
  await api.getCurrentWebview().setBackgroundColor([0, 0, 0, 0]);
}

async function applyWindowMode(mode) {
  const api = await windowApi();
  if (!api) return;

  const appWindow = api.getCurrentWindow();
  const size = WINDOW_SIZES[mode] || WINDOW_SIZES.compact;

  if (mode === "expanded") {
    // 小插件不占任务栏；完整视图是常规窗口，要出现在任务栏里。
    // Windows 任务栏只在窗口重新显示时重读该样式，必须先藏后显才生效。
    await appWindow.hide().catch(() => {});
    await appWindow.setSkipTaskbar(false).catch(() => {});
    compactPosition = await appWindow.outerPosition().catch(() => null);
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
  await appWindow.setMinSize(null);
  await appWindow.setMaximizable(false);
  await appWindow.setSize(new api.LogicalSize(size.width, size.height));
  await appWindow.setResizable(false);
  await appWindow.setMinSize(new api.LogicalSize(size.minWidth, size.minHeight));

  const stored = readStoredPosition();
  const target =
    compactPosition || (stored ? new api.PhysicalPosition(stored.x, stored.y) : null);
  if (target) {
    await appWindow.setPosition(target).catch(() => appWindow.center());
  } else {
    await appWindow.center();
  }
  await appWindow.show().catch(() => {});
  await appWindow.setFocus().catch(() => {});
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
  try {
    await appWindow.setEffects({ effects: ["popover", "hudWindow", "blur"] });
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
    if (getPinned()) {
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
  minimizeWindow,
  restoreWindowPosition,
  setAutostart,
  setWindowGlass,
  setWindowPinned,
  startEdgeDock,
  startPositionMemory,
};
