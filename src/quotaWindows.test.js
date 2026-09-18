import assert from "node:assert/strict";
import test from "node:test";

import { QUOTA_LOW_REMAINING, bindingWindow, isBalanceWindow } from "./quotaWindows.js";

/// 造一个窗口；顺序按后端的 quota_window_rank（five_hour → seven_day → 月度）。
function windowOf(key, remainingPercent, { resetExpired = false, available = true } = {}) {
  return { key, view: { available, remainingPercent, resetExpired } };
}

test("Claude：5h 还满着但每周快见底时，显示每周", () => {
  const picked = bindingWindow([
    windowOf("five_hour", 95),
    windowOf("seven_day", 13),
  ]);
  assert.equal(picked.key, "seven_day");
});

test("GLM：两个窗口都还满着时显示 5h，不因几个百分点的高低来回跳", () => {
  const picked = bindingWindow([
    windowOf("five_hour", 99),
    windowOf("seven_day", 97),
  ]);
  assert.equal(picked.key, "five_hour");
});

test("较长窗口只是略低时不接管——差值属于噪声", () => {
  const picked = bindingWindow([
    windowOf("five_hour", 80),
    windowOf("seven_day", 40),
  ]);
  assert.equal(picked.key, "five_hour");
});

test("越过告急线才接管", () => {
  const picked = bindingWindow([
    windowOf("five_hour", 80),
    windowOf("seven_day", QUOTA_LOW_REMAINING),
  ]);
  assert.equal(picked.key, "seven_day");
});

test("两个窗口都告急时取更少的那个", () => {
  const picked = bindingWindow([
    windowOf("five_hour", 12),
    windowOf("seven_day", 4),
  ]);
  assert.equal(picked.key, "seven_day");
});

test("Codex 没有 5h 窗口，落到每周", () => {
  const picked = bindingWindow([windowOf("secondary", 53)]);
  assert.equal(picked.key, "secondary");
});

test("任一窗口归零就显示它——那时这个 Agent 已经用不了了", () => {
  const picked = bindingWindow([
    windowOf("five_hour", 100),
    windowOf("seven_day", 0),
  ]);
  assert.equal(picked.key, "seven_day");
  assert.equal(picked.view.remainingPercent, 0);
});

test("都告急且并列时取周期更短的（列表顺序即周期顺序）", () => {
  const picked = bindingWindow([
    windowOf("five_hour", 5),
    windowOf("seven_day", 5),
  ]);
  assert.equal(picked.key, "five_hour");
});

test("已过重置点的读数属于上一个周期，不参与比较", () => {
  // Kimi 实测形状：5h/7d 的重置时刻都已过去（读数是 29 小时前的），月度是新的。
  const picked = bindingWindow([
    windowOf("five_hour", 100, { resetExpired: true }),
    windowOf("seven_day", 0, { resetExpired: true }),
    windowOf("monthly_cycle", 57.24),
  ]);
  assert.equal(picked.key, "monthly_cycle");
});

test("没有来源的窗口不参与比较", () => {
  const picked = bindingWindow([
    windowOf("five_hour", 0, { available: false }),
    windowOf("seven_day", 80),
  ]);
  assert.equal(picked.key, "seven_day");
});

test("isBalanceWindow 只认 balance_ 前缀", () => {
  assert.equal(isBalanceWindow({ key: "balance_cny" }), true);
  assert.equal(isBalanceWindow({ key: "balance_usd" }), true);
  assert.equal(isBalanceWindow({ key: "five_hour" }), false);
  assert.equal(isBalanceWindow({ key: "credits" }), false);
  assert.equal(isBalanceWindow(null), false);
});

test("DeepSeek：余额是金额不是百分比，¥8.4 不该被告急规则顶到行首", () => {
  const picked = bindingWindow([
    windowOf("seven_day", 80),
    windowOf("balance_cny", 8.4),
  ]);
  assert.equal(picked.key, "seven_day");
});

test("余额窗口是唯一的有效窗口时照常返回（live[0]）", () => {
  const picked = bindingWindow([windowOf("balance_cny", 8.4)]);
  assert.equal(picked.key, "balance_cny");
  assert.equal(picked.view.remainingPercent, 8.4);
});

test("全部失效时返回 null，由调用方显示「已重置，等待刷新」", () => {
  assert.equal(bindingWindow([windowOf("five_hour", 0, { resetExpired: true })]), null);
  assert.equal(bindingWindow([]), null);
  assert.equal(bindingWindow(undefined), null);
});
