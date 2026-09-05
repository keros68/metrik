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
