import { test } from "node:test";
import assert from "node:assert/strict";
import {
  TRAY_BADGE_HIDDEN_REFRESH_MS,
  normalizeBadgePercent,
  trayBadgeKey,
  trayBadgeSpec,
  trayBadgeText,
  trayBadgeTooltip,
} from "./trayBadge.js";
import { setLanguageSetting } from "./i18n.js";

test("badge spec picks the first agent from the shared status list", () => {
  const items = [
    { agent: "claude", remaining: 87, stale: false },
    { agent: "codex", remaining: 42, stale: true },
  ];
  assert.deepEqual(trayBadgeSpec(items), {
    agent: "claude",
    percent: 87,
    stale: false,
  });
});

test("badge spec survives empty or malformed lists", () => {
  assert.equal(trayBadgeSpec([]), null);
  assert.equal(trayBadgeSpec(null), null);
  assert.equal(trayBadgeSpec([{ agent: 7 }]), null);
});

test("badge spec normalizes the percentage", () => {
  const spec = trayBadgeSpec([{ agent: "codex", remaining: 120.4, stale: true }]);
  assert.equal(spec.percent, 100);
  const missing = trayBadgeSpec([{ agent: "codex", remaining: null, stale: false }]);
  assert.equal(missing.percent, null);
});

test("percent normalization keeps only finite clamped integers", () => {
  assert.equal(normalizeBadgePercent(94.4), 94);
  assert.equal(normalizeBadgePercent(-3), 0);
  assert.equal(normalizeBadgePercent(250), 100);
  assert.equal(normalizeBadgePercent(null), null);
  assert.equal(normalizeBadgePercent(Number.NaN), null);
});

test("badge text shows -- only when the quota is unavailable", () => {
  assert.equal(trayBadgeText(87), "87");
  assert.equal(trayBadgeText(0), "0");
  assert.equal(trayBadgeText(100), "100");
  assert.equal(trayBadgeText(null), "--");
});

test("badge tooltip reuses the menu-bar wording", () => {
  assert.equal(trayBadgeTooltip("ChatGPT", 87, false), "Metrik · ChatGPT 剩余 87%");
  assert.equal(
    trayBadgeTooltip("Claude", 42, true),
    "Metrik · Claude 剩余 42% · 数据可能已过期",
  );
  assert.equal(trayBadgeTooltip("GLM", null, false), "Metrik · GLM 配额不可用");
});

test("badge tooltip follows the interface language", async () => {
  await setLanguageSetting("en");
  try {
    assert.equal(trayBadgeTooltip("ChatGPT", 87, false), "Metrik · ChatGPT 87% left");
    assert.equal(
      trayBadgeTooltip("Claude", 42, true),
      "Metrik · Claude 42% left · data may be stale",
    );
    assert.equal(trayBadgeTooltip("GLM", null, false), "Metrik · GLM quota unavailable");
  } finally {
    await setLanguageSetting("zh");
  }
});

test("badge key changes when the tooltip text changes", () => {
  const spec = { agent: "kimi", percent: 66, stale: false };
  assert.notEqual(
    trayBadgeKey({ ...spec, tooltip: "Metrik · Kimi 剩余 66%" }),
    trayBadgeKey({ ...spec, tooltip: "Metrik · Kimi 66% left" }),
  );
});

test("badge key changes only when agent, percent, or staleness changes", () => {
  const key = trayBadgeKey({ agent: "kimi", percent: 66, stale: false });
  assert.equal(trayBadgeKey({ agent: "kimi", percent: 66, stale: false }), key);
  assert.notEqual(trayBadgeKey({ agent: "kimi", percent: 65, stale: false }), key);
  assert.notEqual(trayBadgeKey({ agent: "kimi", percent: 66, stale: true }), key);
  assert.notEqual(trayBadgeKey({ agent: "codex", percent: 66, stale: false }), key);
  assert.notEqual(trayBadgeKey({ agent: "kimi", percent: null, stale: false }), key);
  assert.equal(trayBadgeKey(null), null);
});

test("hidden refresh cadence matches the visible compact widget", () => {
  assert.equal(TRAY_BADGE_HIDDEN_REFRESH_MS, 300_000);
});
