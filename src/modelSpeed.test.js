import assert from "node:assert/strict";
import { test } from "node:test";
import {
  buildModelsCsvTable,
  defaultModelSortDirection,
  extractModelTimingCells,
  formatDurationParts,
  formatRateParts,
  hasAnyModelTiming,
  sortModelEntries,
} from "./modelSpeed.js";

test("extractModelTimingCells drops zero-sample timing placeholders and respects per-agent rules", () => {
  // Claude 开启了响应均速，但未采到首 token 样本（sampleCount = 0）：首 token 应视为不可用。
  const claude = {
    agent: "claude",
    model: "claude-sonnet-5-5",
    tokens: 30_600_000,
    share: 5.5,
    responseTokensPerSecond: 104.3,
    responseSampleCount: 50,
    requestTiming: {
      firstTokenLatencyMs: { value: null, sampleCount: 0 },
    },
  };
  const claudeCells = extractModelTimingCells(claude);
  assert.equal(claudeCells.firstToken.available, false);
  assert.equal(claudeCells.response.available, true);
  assert.equal(claudeCells.response.value, 104.3);
  assert.equal(claudeCells.response.sampleCount, 50);
  assert.equal(claudeCells.stream.available, false);
  assert.equal(claudeCells.duration.available, false);

  // Antigravity / Kimi 展示三阶段拆分，不重复暴露 responseTokensPerSecond。
  const antigravity = {
    agent: "antigravity",
    model: "gemini-4-hydrogen",
    tokens: 54_400_000,
    share: 9.7,
    responseTokensPerSecond: 180.0,
    responseSampleCount: 203,
    requestTiming: {
      firstTokenLatencyMs: { value: 7220, sampleCount: 203 },
      streamTokensPerSecond: { value: 281.3, sampleCount: 203 },
      requestDurationMs: { value: 10580, sampleCount: 203 },
    },
  };
  const agyCells = extractModelTimingCells(antigravity);
  assert.equal(agyCells.firstToken.available, true);
  assert.equal(agyCells.stream.available, true);
  assert.equal(agyCells.duration.available, true);
  assert.equal(agyCells.response.available, false);

  // OpenCode 同时存在 V2 流式与 Pi 响应均速样本时，两者都保留各自独立样本数。
  const opencode = {
    agent: "opencode",
    model: "glm-5",
    tokens: 10_000,
    share: 1.0,
    responseTokensPerSecond: 42.5,
    responseSampleCount: 12,
    requestTiming: {
      firstTokenLatencyMs: { value: 850, sampleCount: 8 },
      streamTokensPerSecond: { value: 96.4, sampleCount: 7 },
      requestDurationMs: { value: 2400, sampleCount: 8 },
    },
  };
  const ocCells = extractModelTimingCells(opencode);
  assert.equal(ocCells.response.available, true);
  assert.equal(ocCells.response.sampleCount, 12);
  assert.equal(ocCells.stream.sampleCount, 7);

  // ZCode 无 requestTiming，仅靠 responseTokensPerSecond 展示响应均速。
  const zcode = {
    agent: "zcode",
    model: "glm-5.3-flash",
    tokens: 13_400_000,
    share: 2.4,
    responseTokensPerSecond: 14.5,
    responseSampleCount: 20,
  };
  assert.equal(extractModelTimingCells(zcode).response.available, true);
  assert.equal(hasAnyModelTiming(zcode), true);
  assert.equal(hasAnyModelTiming({ agent: "hermes", model: "x", tokens: 100, share: 1 }), false);
});

test("sortModelEntries keeps untimed models at the bottom for both asc and desc", () => {
  const models = [
    {
      agent: "codex",
      model: "untimed-big",
      tokens: 900,
      share: 60,
      responseSampleCount: 0,
    },
    {
      agent: "antigravity",
      model: "slow-ttft",
      tokens: 200,
      share: 15,
      requestTiming: { firstTokenLatencyMs: { value: 9000, sampleCount: 10 } },
    },
    {
      agent: "kimi",
      model: "fast-ttft",
      tokens: 100,
      share: 10,
      requestTiming: { firstTokenLatencyMs: { value: 1200, sampleCount: 5 } },
    },
  ];

  assert.equal(defaultModelSortDirection("firstToken"), "asc");
  assert.equal(defaultModelSortDirection("stream"), "desc");

  const asc = sortModelEntries(models, "firstToken", "asc").map((item) => item.model);
  assert.deepEqual(asc, ["fast-ttft", "slow-ttft", "untimed-big"]);

  const desc = sortModelEntries(models, "firstToken", "desc").map((item) => item.model);
  assert.deepEqual(desc, ["slow-ttft", "fast-ttft", "untimed-big"]);
});

test("formatting and CSV export preserve precision and leave missing metrics blank", () => {
  assert.deepEqual(formatRateParts(0.04), { number: "<0.1", unit: "tok/s" });
  assert.deepEqual(formatRateParts(281.34), { number: "281.3", unit: "tok/s" });
  assert.deepEqual(formatDurationParts(390), { number: "390", unit: "ms" });
  assert.deepEqual(formatDurationParts(7220), { number: "7.22", unit: "s" });

  const { header, rows } = buildModelsCsvTable([
    {
      agent: "claude",
      model: "claude-sonnet-5.5",
      tokens: 30_600_000,
      share: 5.5,
      responseTokensPerSecond: 104.3,
      responseSampleCount: 50,
      requestTiming: { firstTokenLatencyMs: { value: null, sampleCount: 0 } },
    },
  ]);
  assert.equal(header.length, 12);
  assert.deepEqual(rows[0], [
    "claude",
    "claude-sonnet-5.5",
    30_600_000,
    "5.50",
    "",
    "",
    "",
    "",
    "104.30",
    50,
    "",
    "",
  ]);
});
