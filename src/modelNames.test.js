import { test } from "node:test";
import assert from "node:assert/strict";
import { claudeVersionWithDots, modelDisplayName } from "./modelNames.js";

test("Claude version numbers are shown with dots", () => {
  assert.equal(claudeVersionWithDots("claude-fable-5-1"), "claude-fable-5.1");
  assert.equal(claudeVersionWithDots("claude-opus-4-8"), "claude-opus-4.8");
  assert.equal(claudeVersionWithDots("claude-haiku-4-5"), "claude-haiku-4.5");
  // 老式命名把版本写在系列名前面。
  assert.equal(claudeVersionWithDots("claude-3-7-sonnet-20250219"), "claude-3.7-sonnet-20250219");
});

test("date snapshots survive the rewrite", () => {
  // 日期快照是模型身份的一部分：同一个系列的两个快照可能不同价，省掉就分不出来。
  assert.equal(claudeVersionWithDots("claude-opus-4-1-20250805"), "claude-opus-4.1-20250805");
  assert.equal(claudeVersionWithDots("claude-haiku-4-5-20251001"), "claude-haiku-4.5-20251001");
});

test("names without a two-part version are left alone", () => {
  assert.equal(claudeVersionWithDots("claude-opus-5"), "claude-opus-5");
  assert.equal(claudeVersionWithDots("claude-3-opus-20240229"), "claude-3-opus-20240229");
  assert.equal(claudeVersionWithDots("claude-4-sonnet-20250514"), "claude-4-sonnet-20250514");
  assert.equal(claudeVersionWithDots("claude-mythos-preview"), "claude-mythos-preview");
});

test("other vendors keep their own official spelling", () => {
  // grok-4-1-fast 是 xAI 官方写法，不是待修的连字符。
  assert.equal(claudeVersionWithDots("grok-4-1-fast"), "grok-4-1-fast");
  assert.equal(claudeVersionWithDots("gpt-5.6-sol"), "gpt-5.6-sol");
  assert.equal(claudeVersionWithDots("glm-5.3"), "glm-5.3");
});

test("placeholder rows still read as sentences, not model names", () => {
  assert.equal(modelDisplayName("synced-remote"), "其他设备同步（无模型名）");
  assert.equal(modelDisplayName("unknown"), "未标注模型");
  assert.equal(modelDisplayName("claude-fable-5-1"), "claude-fable-5.1");
});
