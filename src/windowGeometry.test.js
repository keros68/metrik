import assert from "node:assert/strict";
import test from "node:test";

import {
  floatingViewportSize,
  horizontalStripTargetWidth,
  isDockAnchorPosition,
  isStableFloatingMode,
  monitorForWindowPosition,
  physicalWindowSize,
  verticalStripHoverLocalLayout,
  verticalStripHoverLayout,
  viewportCorrectedPhysicalSize,
} from "./windowGeometry.js";

test("floating content fits the physical work area at combined app and monitor scale", () => {
  assert.deepEqual(floatingViewportSize(42, 900, 2, 1.5, { width: 1920, height: 1040 }), {
    width: 42, height: 346,
  });
  assert.deepEqual(floatingViewportSize(1200, 28, 1.25, 1.5, { width: 1366, height: 728 }), {
    width: 728, height: 28,
  });
  assert.deepEqual(floatingViewportSize(320, 900, 1, 1, null), { width: 320, height: 900 });
});

test("moving a docked window away from its anchor is treated as a user drag", () => {
  const anchor = { x: 1878, y: 300 };
  assert.equal(isDockAnchorPosition({ x: 1878, y: 300 }, anchor), true);
  assert.equal(isDockAnchorPosition({ x: 1880, y: 299 }, anchor), true);
  assert.equal(isDockAnchorPosition({ x: 1600, y: 300 }, anchor), false);
});

test("transient strip geometry never participates in persistent floating state", () => {
  assert.equal(isStableFloatingMode("compact"), true);
  assert.equal(isStableFloatingMode("strip-vertical"), true);
  assert.equal(isStableFloatingMode("strip-vertical", true), false);
  assert.equal(isStableFloatingMode("expanded"), false);
});

function monitor(x, width, scaleFactor, workHeight = 1080) {
  return {
    position: { x, y: 0 },
    size: { width, height: workHeight },
    workArea: {
      position: { x, y: 0 },
      size: { width, height: workHeight - 40 },
    },
    scaleFactor,
  };
}

test("physical size combines app scale with destination monitor DPI", () => {
  assert.deepEqual(physicalWindowSize(320, 440, 1.25, 1.5), {
    width: 600,
    height: 825,
  });
  assert.deepEqual(physicalWindowSize(52, 272, 1, 1.75), {
    width: 91,
    height: 476,
  });
});

test("remembered position selects the destination monitor instead of the current one", () => {
  const primary = monitor(0, 1920, 1.25);
  const secondary = monitor(1920, 3840, 2);
  const selected = monitorForWindowPosition(
    [primary, secondary],
    { x: 2400, y: 120 },
    { width: 320, height: 440 },
    1,
  );
  assert.equal(selected, secondary);
  assert.deepEqual(physicalWindowSize(52, 272, 1, selected.scaleFactor), {
    width: 104,
    height: 544,
  });
});

test("overlap chooses the monitor that will contain most of the restored window", () => {
  const left = monitor(-1920, 1920, 1.5);
  const right = monitor(0, 1920, 2);
  const selected = monitorForWindowPosition(
    [left, right],
    { x: -120, y: 80 },
    { width: 320, height: 320 },
    1,
  );
  assert.equal(selected, right);
});

test("fully off-screen remembered positions do not select a monitor", () => {
  const selected = monitorForWindowPosition(
    [monitor(0, 1920, 1.5)],
    { x: 9000, y: 9000 },
    { width: 320, height: 440 },
    1,
  );
  assert.equal(selected, null);
});

test("horizontal strip width includes every outer flex gap", () => {
  assert.equal(
    horizontalStripTargetWidth({
      cellCount: 2,
      cellWidth: 68,
      controlsWidth: 146,
      paddingLeft: 6,
      paddingRight: 5,
      gap: 4,
    }),
    302,
  );
  assert.equal(
    horizontalStripTargetWidth({
      cellCount: 0,
      cellWidth: 68,
      controlsWidth: 146,
      paddingLeft: 6,
      paddingRight: 5,
      gap: 4,
    }),
    230,
  );
});

test("vertical strip hover expands away from the nearest screen edge", () => {
  assert.deepEqual(
    verticalStripHoverLayout({
      railPosition: { x: 1878, y: 300 },
      railSize: { width: 42, height: 260 },
      workArea: { x: 0, y: 0, width: 1920, height: 1040 },
      targetSize: { width: 392, height: 320 },
      anchorY: 69,
      cardHeight: 280,
    }),
    { side: "right", x: 1528, y: 240, cardCenter: 148, railOffsetY: 60 },
  );
});

test("vertical strip hover keeps the whole rail visible for a lower cell", () => {
  const layout = verticalStripHoverLayout({
    railPosition: { x: 1878, y: 300 },
    railSize: { width: 42, height: 360 },
    workArea: { x: 0, y: 0, width: 1920, height: 1040 },
    targetSize: { width: 312, height: 360 },
    anchorY: 299,
    cardHeight: 180,
  });

  assert.deepEqual(layout, {
    side: "right",
    x: 1608,
    y: 300,
    cardCenter: 262,
    railOffsetY: 0,
  });
  assert.ok(layout.railOffsetY >= 0);
  assert.ok(layout.railOffsetY + 360 <= 360);
});

test("vertical strip hover keeps the rail bottom visible for the first cell", () => {
  const layout = verticalStripHoverLayout({
    railPosition: { x: 2464, y: 357 },
    railSize: { width: 54, height: 314 },
    workArea: { x: 0, y: 0, width: 2560, height: 1400 },
    targetSize: { width: 510, height: 314 },
    anchorY: 23,
    cardHeight: 270,
  });

  assert.deepEqual(layout, {
    side: "right",
    x: 2008,
    y: 357,
    cardCenter: 143,
    railOffsetY: 0,
  });
});

test("vertical strip hover stays inside the work area near the top-left", () => {
  assert.deepEqual(
    verticalStripHoverLayout({
      railPosition: { x: 0, y: 0 },
      railSize: { width: 42, height: 90 },
      workArea: { x: 0, y: 0, width: 1920, height: 1040 },
      targetSize: { width: 392, height: 300 },
      anchorY: 23,
      cardHeight: 270,
    }),
    { side: "left", x: 0, y: 0, cardCenter: 143, railOffsetY: 0 },
  );
});

test("Wayland-local strip hover clamps the card and pointer without global coordinates", () => {
  assert.deepEqual(
    verticalStripHoverLocalLayout({
      targetHeight: 300,
      anchorY: 23,
      cardHeight: 270,
    }),
    { cardCenter: 143, pointerY: 22 },
  );
  assert.deepEqual(
    verticalStripHoverLocalLayout({
      targetHeight: 300,
      anchorY: 277,
      cardHeight: 270,
    }),
    { cardCenter: 157, pointerY: 248 },
  );
});

test("runtime viewport corrects a hidden WebView zoom layer", () => {
  assert.deepEqual(
    viewportCorrectedPhysicalSize({
      currentPhysicalWidth: 560,
      currentPhysicalHeight: 560,
      viewportWidth: 256,
      viewportHeight: 256,
      expectedWidth: 320,
      expectedHeight: 320,
    }),
    { width: 700, height: 700 },
  );
});

test("runtime viewport correction rejects transient invalid measurements", () => {
  assert.equal(
    viewportCorrectedPhysicalSize({
      currentPhysicalWidth: 560,
      currentPhysicalHeight: 560,
      viewportWidth: 0,
      viewportHeight: 0,
      expectedWidth: 320,
      expectedHeight: 320,
    }),
    null,
  );
});
