import assert from "node:assert/strict";
import test from "node:test";

import {
  GLASS_MODES,
  glassShellAppearance,
  nextGlassTint,
  resolveGlassMode,
} from "./glassAppearance.js";

test("the user-facing component appearance cycles through exactly three tints", () => {
  assert.equal(nextGlassTint("dark"), "light");
  assert.equal(nextGlassTint("light"), "clear");
  assert.equal(nextGlassTint("clear"), "dark");
  assert.equal(nextGlassTint("off"), "dark");
});

test("glass mode is alpha where true alpha exists, CSS otherwise, and off when disabled", () => {
  assert.equal(resolveGlassMode({ enabled: true, trueAlphaAvailable: true }), GLASS_MODES.alpha);
  assert.equal(resolveGlassMode({ enabled: true }), GLASS_MODES.css);
  assert.equal(resolveGlassMode({ enabled: false, trueAlphaAvailable: true }), GLASS_MODES.off);
  assert.equal(resolveGlassMode({ enabled: false }), GLASS_MODES.off);
});

test("every Windows tint keeps alpha classes free of the CSS fallback", () => {
  for (const glassTint of ["dark", "light", "clear"]) {
    const appearance = glassShellAppearance("widget", {
      glassMode: GLASS_MODES.alpha,
      glassTint,
    });

    assert.doesNotMatch(appearance.className, /--glass-css/);
    assert.equal(appearance.style["--glass-alpha"], 0.82);
    assert.equal(appearance.trueAlpha, glassTint === "clear");
  }
});

test("compact and strip clear glass share one true-alpha appearance", () => {
  for (const kind of ["widget", "strip"]) {
    const appearance = glassShellAppearance(kind, {
      glassMode: GLASS_MODES.alpha,
      glassTint: "clear",
      glassAlpha: 0.82,
    });
    const prefix = kind === "widget" ? "widget-shell" : "strip-shell";

    assert.equal(appearance.trueAlpha, true);
    assert.equal(appearance.edgeInteractive, true);
    assert.match(appearance.className, new RegExp(`${prefix}--transparent`));
    assert.match(appearance.className, new RegExp(`${prefix}--glass-clear`));
    // 透明档共用浅色档的深色前景，只有材质层不同。
    assert.match(appearance.className, new RegExp(`${prefix}--glass-light`));
    assert.doesNotMatch(appearance.className, /--glass-css/);
    assert.deepEqual(appearance.style, {
      "--glass-alpha": 0.82,
    });
    assert.equal(
      Object.keys(appearance.style).some((key) => key.startsWith("--wall-")),
      false,
    );
  }
});

test("strip orientation is explicit so horizontal and vertical can share material without sharing layout", () => {
  const horizontal = glassShellAppearance("strip");
  const vertical = glassShellAppearance("strip", { vertical: true });

  assert.match(horizontal.className, /strip-shell--horizontal/);
  assert.doesNotMatch(horizontal.className, /strip-shell--vertical/);
  assert.match(vertical.className, /strip-shell--vertical/);
  assert.doesNotMatch(vertical.className, /strip-shell--horizontal/);
});

test("the clear tint pairs each ink colour with the backdrop that can carry it", () => {
  for (const kind of ["widget", "strip"]) {
    const prefix = kind === "widget" ? "widget-shell" : "strip-shell";
    const base = { glassMode: GLASS_MODES.alpha, glassTint: "clear" };

    // 深色字 → 白霜，沿用浅色档整套前景。
    const ink = glassShellAppearance(kind, { ...base, glassInk: "dark" });
    assert.match(ink.className, new RegExp(`${prefix}--glass-light`));
    assert.doesNotMatch(ink.className, new RegExp(`${prefix}--glass-ink-light`));

    // 白色字 → 深色薄罩，绝不能同时挂浅色档（白字压白霜读不出来）。
    const white = glassShellAppearance(kind, { ...base, glassInk: "light" });
    assert.match(white.className, new RegExp(`${prefix}--glass-ink-light`));
    assert.doesNotMatch(white.className, new RegExp(`${prefix}--glass-light`));
    assert.equal(white.trueAlpha, true);
  }
});

test("the ink choice only applies to the clear tint", () => {
  for (const glassTint of ["dark", "light"]) {
    const appearance = glassShellAppearance("widget", {
      glassMode: GLASS_MODES.alpha,
      glassTint,
      glassInk: "light",
    });
    assert.doesNotMatch(appearance.className, /--glass-ink-light/);
  }
});

test("browser clear fallback keeps the edge interaction without claiming true alpha", () => {
  const appearance = glassShellAppearance("widget", {
    glassMode: GLASS_MODES.css,
    glassTint: "clear",
  });

  assert.equal(appearance.edgeInteractive, true);
  assert.equal(appearance.trueAlpha, false);
  assert.match(appearance.className, /widget-shell--glass-css/);
  assert.match(appearance.className, /widget-shell--glass-light/);
});

test("macOS ignores a stored Windows clear tint", () => {
  const appearance = glassShellAppearance("widget", {
    glassMode: GLASS_MODES.native,
    glassTint: "clear",
    isMac: true,
  });

  assert.equal(appearance.edgeInteractive, false);
  assert.equal(appearance.trueAlpha, false);
  assert.match(appearance.className, /widget-shell--mac/);
  assert.doesNotMatch(appearance.className, /widget-shell--glass-(?:light|clear)/);
});
