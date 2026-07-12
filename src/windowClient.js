const WINDOW_SIZES = {
  compact: { width: 320, height: 320, minWidth: 320, minHeight: 320 },
  expanded: { width: 1120, height: 760, minWidth: 960, minHeight: 700 },
};

let compactPosition = null;

function isDesktop() {
  return typeof window !== "undefined" && Boolean(window.__TAURI_INTERNALS__);
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

async function setWindowGlass(enabled) {
  const api = await windowApi();
  if (!api) return;
  const appWindow = api.getCurrentWindow();
  if (enabled) {
    // Windows 取 acrylic，macOS 取 hudWindow；平台不支持的条目会被忽略。
    // 深色 tint 配合前端的深色玻璃材质（参考 HUD 风格小组件的通行做法）。
    await appWindow.setEffects({
      effects: ["acrylic", "hudWindow", "blur"],
      color: [18, 21, 27, 175],
    });
  } else {
    await appWindow.clearEffects();
  }
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
};
