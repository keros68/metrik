import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import test from "node:test";
import * as geometry from "./windowGeometry.js";

// Execute the window controller against a simulated native boundary. No real
// desktop window, persisted settings, or WebView zoom is changed by these tests.
const source = readFileSync(new URL("./windowClient.js", import.meta.url), "utf8")
  .replace(/^import[\s\S]*?;\r?\n/gm, "")
  .replace(/export \{[\s\S]*?\};\s*$/, "");

function controller(stored = {}) {
  const context = vm.createContext({
    ...geometry,
    console: { warn() {} },
    window: { innerWidth: 320, innerHeight: 320 },
    localStorage: { getItem: (key) => stored[key] ?? null, setItem() {} },
  });
  vm.runInContext(source, context);
  return context;
}

test("startup does not apply compact zoom to a strip or expanded window", async () => {
  const context = controller();
  let zoomCalls = 0;
  Object.assign(context, {
    isMacPlatform: () => false,
    windowApi: async () => ({}),
    applyWebviewZoom: async () => { zoomCalls += 1; },
  });
  await context.applyStartupUiScale("strip");
  await context.applyStartupUiScale("expanded");
  assert.equal(zoomCalls, 0);
});

test("cached 28px horizontal strip and short compact heights survive startup", () => {
  const context = controller({
    "metrik:stripContentSize": JSON.stringify({ horizontal: { width: 366, height: 28 } }),
    "metrik:compactContentHeight": JSON.stringify({ height: 280 }),
  });
  assert.equal(context.stripContentSize("horizontal", {}).height, 28);
  assert.equal(context.compactContentHeight(320), 280);
});

test("an oversized window cannot be clamped above the work-area origin", async () => {
  const context = controller();
  let position = { x: 0, y: 0 };
  context.isLinuxPlatform = () => false;
  await context.clampIntoWorkArea({
    availableMonitors: async () => [{
      position: { x: 0, y: 0 }, size: { width: 1280, height: 720 },
    }],
    PhysicalPosition: class { constructor(x, y) { this.x = x; this.y = y; } },
  }, {
    outerPosition: async () => position,
    setPosition: async (value) => { position = value; },
  }, { width: 1600, height: 1000 });
  assert.equal(position.x, 0);
  assert.equal(position.y, 0);
});

test("a stale resize viewport cannot change the user's chosen zoom", async () => {
  const context = controller();
  let physical = { width: 320, height: 320 };
  const zooms = [];
  Object.assign(context, {
    window: { innerWidth: 640, innerHeight: 640 },
    settleWebviewLayout: async () => {},
    scaledPhysicalSize: async () => ({ width: 320, height: 320 }),
    clampIntoWorkArea: async () => {},
    applyWebviewZoom: async (zoom) => {
      zooms.push(zoom);
      context.window.innerWidth = 320;
      context.window.innerHeight = 320;
    },
  });
  const appWindow = {
    scaleFactor: async () => 1,
    innerSize: async () => physical,
    setSize: async (size) => { physical = size; },
  };
  await context.reconcileFloatingSizeAfterShow({ currentMonitor: async () => null }, appWindow, 320, 320, 1, physical);
  assert.deepEqual(zooms, [1]);
});

test("hover expansion preserves zoom and collapse restores the chosen strip scale", async () => {
  const context = controller();
  let physical = { width: 42, height: 300 };
  let position = { x: 1878, y: 500 };
  let reconciliations = 0;
  const zooms = [];
  const appWindow = {
    outerPosition: async () => position,
    outerSize: async () => physical,
    innerSize: async () => physical,
    scaleFactor: async () => 1,
    setSize: async (size) => { physical = size; },
    setPosition: async (value) => { position = value; },
  };
  const api = {
    getCurrentWindow: () => appWindow,
    currentMonitor: async () => ({ workArea: {
      position: { x: 0, y: 0 }, size: { width: 1920, height: 1040 },
    } }),
    PhysicalPosition: class { constructor(x, y) { this.x = x; this.y = y; } },
  };
  Object.assign(context, {
    isWindowsPlatform: () => true,
    isLinuxPlatform: () => false,
    windowApi: async () => api,
    scaledPhysicalSize: async (_api, _win, width, height) => ({ width, height }),
    settleWebviewLayout: async () => {},
    reconcileFloatingSizeAfterShow: async () => { reconciliations += 1; return physical; },
    applyWebviewZoom: async (zoom) => { zooms.push(zoom); },
  });
  await context.expandVerticalStripHover({
    width: 312, height: 300, railWidth: 42, railHeight: 300,
    anchorY: 260, cardHeight: 180,
  });
  assert.equal(reconciliations, 0);
  assert.deepEqual(zooms, []);
  await context.collapseVerticalStripHover();
  assert.equal(physical.width, 42);
  assert.equal(physical.height, 300);
  assert.equal(position.x, 1878);
  assert.equal(position.y, 500);
  assert.deepEqual(zooms, [1]);
});

test("reassertCompactSize keeps retrying on the escalating cadence while the viewport stays desynced", async () => {
  const context = controller();
  const sizes = [];
  const waits = [];
  const appWindow = {
    scaleFactor: async () => 1,
    setSize: async (physical) => { sizes.push(physical); },
  };
  Object.assign(context, {
    isMacPlatform: () => false,
    isWindowsPlatform: () => true,
    isLinuxPlatform: () => false,
    windowApi: async () => ({
      getCurrentWindow: () => appWindow,
      currentMonitor: async () => null,
    }),
    applyWebviewZoom: async () => {},
    scaledPhysicalSize: async (_api, _win, width, height) => ({ width, height }),
    reconcileFloatingSizeAfterShow: async () => ({}),
    desyncHealRetryDelayMs: (attempt) => geometry.desyncHealRetryDelayMs(attempt),
    setTimeout: (fn, ms) => { waits.push(ms); fn(); },
  });
  // 视口宽 256：320 的设计宽被裁，三次收敛后仍未落位。
  context.window.innerWidth = 256;
  context.window.innerHeight = 320;
  await context.reassertCompactSize();
  assert.equal(sizes.length, 4);
  assert.deepEqual(waits, [250, 600, 1200]);
});

test("reassertCompactSize stops as soon as the viewport settles", async () => {
  const context = controller();
  const sizes = [];
  const appWindow = {
    scaleFactor: async () => 1,
    setSize: async (physical) => {
      sizes.push(physical);
      context.window.innerWidth = 320;
    },
  };
  Object.assign(context, {
    isMacPlatform: () => false,
    isWindowsPlatform: () => true,
    isLinuxPlatform: () => false,
    windowApi: async () => ({
      getCurrentWindow: () => appWindow,
      currentMonitor: async () => null,
    }),
    applyWebviewZoom: async () => {},
    scaledPhysicalSize: async (_api, _win, width, height) => ({ width, height }),
    reconcileFloatingSizeAfterShow: async () => ({}),
    setTimeout: () => { throw new Error("must not wait once settled"); },
  });
  context.window.innerWidth = 256;
  context.window.innerHeight = 320;
  await context.reassertCompactSize();
  assert.equal(sizes.length, 1);
});

test("reassertCompactSize yields immediately when a newer window correction supersedes it", async () => {
  const context = controller();
  const appWindow = {
    scaleFactor: async () => 1,
    setSize: async () => { throw new Error("superseded pass must not touch the window"); },
  };
  Object.assign(context, {
    isMacPlatform: () => false,
    isWindowsPlatform: () => true,
    isLinuxPlatform: () => false,
    windowApi: async () => ({
      getCurrentWindow: () => appWindow,
      currentMonitor: async () => null,
    }),
    applyWebviewZoom: async () => {},
    scaledPhysicalSize: async (_api, _win, width, height) => ({ width, height }),
    reconcileFloatingSizeAfterShow: async () => ({}),
  });
  context.window.innerWidth = 256;
  context.window.innerHeight = 320;
  await context.reassertCompactSize(null, () => false);
});
test("strip controls collapse restores the pre-open geometry in one batched transaction", async () => {
  const context = controller();
  const calls = [];
  let reconciles = 0;
  let current = {
    position: { x: 900, y: 300 },
    size: { width: 42, height: 96 },
  };
  const appWindow = {
    outerPosition: async () => current.position,
    outerSize: async () => current.size,
    scaleFactor: async () => 1.25,
    setSize: async (size) => { calls.push(["setSize", size]); current = { ...current, size }; },
    setPosition: async (position) => { calls.push(["setPosition", position]); current = { ...current, position }; },
  };
  Object.assign(context, {
    isMacPlatform: () => false,
    isWindowsPlatform: () => true,
    isLinuxPlatform: () => false,
    windowApi: async () => ({ getCurrentWindow: () => appWindow }),
    reconcileFloatingSizeAfterShow: async () => { reconciles += 1; throw new Error("collapse must not reconcile"); },
  });
  await context.beginStripControlsExpand();
  // fit 观察器把窗口临时加高（模拟展开态）。
  current = { position: { x: 900, y: 160 }, size: { width: 42, height: 226 } };
  await context.collapseStripControlsExpand();
  assert.deepEqual(calls, [
    ["setSize", { width: 42, height: 96 }],
    ["setPosition", { x: 900, y: 300 }],
  ]);
  assert.equal(reconciles, 0);
  // CSS 尺寸按 stripScale×DPI 反记进缓存：96/1.25 = 76.8 → 77。
  const cached = context.stripContentSize("vertical", {});
  assert.equal(cached.width, 34);
  assert.equal(cached.height, 77);
  // 收起后还原值已消费，再次收起是无害的空操作。
  await context.collapseStripControlsExpand();
  assert.equal(calls.length, 2);
});

test("beginStripControlsExpand keeps an existing restore instead of overwriting it", async () => {
  const context = controller();
  let size = { width: 42, height: 96 };
  const appWindow = {
    outerPosition: async () => ({ x: 900, y: 300 }),
    outerSize: async () => size,
    scaleFactor: async () => 1,
    setSize: async (value) => { size = value; },
    setPosition: async () => {},
  };
  Object.assign(context, {
    isMacPlatform: () => false,
    isWindowsPlatform: () => true,
    isLinuxPlatform: () => false,
    windowApi: async () => ({ getCurrentWindow: () => appWindow }),
  });
  await context.beginStripControlsExpand();
  size = { width: 42, height: 226 };
  await context.beginStripControlsExpand();
  await context.collapseStripControlsExpand();
  assert.deepEqual(size, { width: 42, height: 96 });
});

test("opening strip controls over a hover card restores the rail, not the hover canvas", async () => {
  const context = controller();
  let physical = { width: 42, height: 300 };
  let position = { x: 1600, y: 500 };
  const appWindow = {
    outerPosition: async () => position,
    outerSize: async () => physical,
    innerSize: async () => physical,
    scaleFactor: async () => 1,
    setSize: async (size) => { physical = size; },
    setPosition: async (value) => { position = value; },
  };
  const api = {
    getCurrentWindow: () => appWindow,
    currentMonitor: async () => ({ workArea: {
      position: { x: 0, y: 0 }, size: { width: 1920, height: 1040 },
    } }),
    PhysicalPosition: class { constructor(x, y) { this.x = x; this.y = y; } },
  };
  Object.assign(context, {
    isMacPlatform: () => false,
    isWindowsPlatform: () => true,
    isLinuxPlatform: () => false,
    windowApi: async () => api,
    scaledPhysicalSize: async (_api, _win, width, height) => ({ width, height }),
    settleWebviewLayout: async () => {},
    applyWebviewZoom: async () => {},
  });
  await context.expandVerticalStripHover({
    width: 312, height: 300, railWidth: 42, railHeight: 300,
    anchorY: 150, cardHeight: 120,
  });
  assert.equal(position.x, 1600 + 42 - 312);
  // 点 … 时详情卡还开着：还原几何捕获排在悬停收回之前。
  await context.beginStripControlsExpand();
  await context.collapseVerticalStripHover();
  physical = { width: 42, height: 430 };
  await context.collapseStripControlsExpand();
  assert.deepEqual(physical, { width: 42, height: 300 });
  assert.deepEqual(position, { x: 1600, y: 500 });
});

test("collapseStripControlsExpand without a pending restore is a no-op", async () => {
  const context = controller();
  let resized = 0;
  const appWindow = {
    outerPosition: async () => null,
    outerSize: async () => null,
    scaleFactor: async () => 1,
    setSize: async () => { resized += 1; },
    setPosition: async () => {},
  };
  Object.assign(context, {
    isMacPlatform: () => false,
    isWindowsPlatform: () => true,
    isLinuxPlatform: () => false,
    windowApi: async () => ({ getCurrentWindow: () => appWindow }),
  });
  await context.collapseStripControlsExpand();
  assert.equal(resized, 0);
});
