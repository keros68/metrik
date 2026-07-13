import { invoke } from "@tauri-apps/api/core";

const WINDOW_SIZES = {
  compact: { width: 320, height: 320, minWidth: 320, minHeight: 320 },
  expanded: { width: 1120, height: 760, minWidth: 960, minHeight: 700 },
};

let compactPosition = null;

function isDesktop() {
  return typeof window !== "undefined" && Boolean(window.__TAURI_INTERNALS__);
}

function isWindowsPlatform() {
  return typeof navigator !== "undefined" && navigator.userAgent.includes("Windows");
}

async function windowApi() {
  if (!isDesktop()) return null;
  return import("@tauri-apps/api/window");
}

async function applyWindowMode(mode) {
  const api = await windowApi();
  if (!api) return;

  const appWindow = api.getCurrentWindow();
  const size = WINDOW_SIZES[mode] || WINDOW_SIZES.compact;

  if (mode === "expanded") {
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
    return;
  }

  if (await appWindow.isMaximized().catch(() => false)) {
    await appWindow.unmaximize();
  }
  await appWindow.setMinSize(null);
  await appWindow.setMaximizable(false);
  await appWindow.setSize(new api.LogicalSize(size.width, size.height));
  await appWindow.setResizable(false);
  await appWindow.setMinSize(new api.LogicalSize(size.minWidth, size.minHeight));

  if (compactPosition) {
    await appWindow.setPosition(compactPosition).catch(() => appWindow.center());
  } else {
    await appWindow.center();
  }
}

function glassTint() {
  const dark = typeof window !== "undefined"
    && window.matchMedia?.("(prefers-color-scheme: dark)")?.matches;
  // SWCA Acrylic 的 tint：亮主题近白高透，暗主题深灰。
  return dark ? [24, 26, 32, 170] : [252, 251, 250, 150];
}

async function setWindowGlass(enabled) {
  if (!isDesktop()) return;
  if (isWindowsPlatform()) {
    // Win11 的 DWM Acrylic 忽略 tint 且偏灰；改走 SWCA Acrylic，
    // 用自定义 tint 得到 CodexBar 式的通透磨砂。
    await invoke("set_glass_backdrop", { enabled, tint: glassTint() });
    return;
  }
  const api = await windowApi();
  if (!api) return;
  const appWindow = api.getCurrentWindow();
  if (enabled) {
    await appWindow.setEffects({ effects: ["popover", "hudWindow", "blur"] });
  } else {
    await appWindow.clearEffects();
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
    dock = null;
    hidden = false;
    outsideSinceMs = null;
    stopPoll();
    await win.setAlwaysOnTop(Boolean(getPinned())).catch(() => {});
  };

  const poll = async () => {
    if (disposed || !dock) return;
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
    if (getMode() !== "compact") {
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
  closeWindow,
  isDesktop,
  minimizeWindow,
  setWindowGlass,
  setWindowPinned,
  startEdgeDock,
};
