import { formatNumber } from "./i18n.js";

/// 各列的自然初始排序方向：用量与吞吐按从大到小，延迟与耗时按从快到慢（从小到大）。
export function defaultModelSortDirection(column) {
  return column === "firstToken" || column === "duration" ? "asc" : "desc";
}

function normalizeDurationMetric(metric) {
  const sampleCount = Number(metric?.sampleCount ?? 0);
  const value = metric?.value;
  const available = Number.isInteger(sampleCount)
    && sampleCount > 0
    && typeof value === "number"
    && Number.isFinite(value)
    && value >= 0;
  return available
    ? { available: true, value, sampleCount }
    : { available: false, value: null, sampleCount: 0 };
}

function normalizeRateMetric(value, sampleCountRaw) {
  const sampleCount = Number(sampleCountRaw ?? 0);
  const available = Number.isInteger(sampleCount)
    && sampleCount > 0
    && typeof value === "number"
    && Number.isFinite(value)
    && value > 0;
  return available
    ? { available: true, value, sampleCount }
    : { available: false, value: null, sampleCount: 0 };
}

/// 将单条 ModelSummary 归一为四个固定速率/耗时槽位。
/// 口径与后端及 PRODUCT-CONSTRAINTS 保持一致：
/// - Kimi / Antigravity 已完整给出「首字 + 流式 + 总耗时」，不重复显示含等待均速；
/// - OpenCode V2 给三阶段，但经由 Pi 的 OpenCode Go 另有含等待均速时一并保留；
/// - sampleCount 为 0 的空指标对象一律归为 unavailable，避免渲染「暂无计时 / 0 样本」。
export function extractModelTimingCells(entry) {
  const timing = entry?.requestTiming;
  const showResponseAverage = !timing
    || entry?.agent === "codex"
    || entry?.agent === "claude"
    || (entry?.agent === "opencode" && Number(entry?.responseSampleCount) > 0);

  return {
    firstToken: normalizeDurationMetric(timing?.firstTokenLatencyMs),
    stream: normalizeRateMetric(
      timing?.streamTokensPerSecond?.value,
      timing?.streamTokensPerSecond?.sampleCount,
    ),
    response: showResponseAverage
      ? normalizeRateMetric(entry?.responseTokensPerSecond, entry?.responseSampleCount)
      : { available: false, value: null, sampleCount: 0 },
    duration: normalizeDurationMetric(timing?.requestDurationMs),
  };
}

export function hasAnyModelTiming(entry) {
  const cells = extractModelTimingCells(entry);
  return (
    cells.firstToken.available
    || cells.stream.available
    || cells.response.available
    || cells.duration.available
  );
}

/// 排序模型列表：按速率或耗时列排序时，缺失样本（—）始终沉底，绝不当作 0 排到最前；
/// 同值或同为无样本时按 token 降序稳定兜底。
export function sortModelEntries(entries, sortKey = "tokens", sortDir = defaultModelSortDirection(sortKey)) {
  const dir = sortDir === "asc" ? 1 : -1;
  return (entries || [])
    .map((entry, index) => ({ entry, index, cells: extractModelTimingCells(entry) }))
    .sort((left, right) => {
      if (sortKey === "tokens") {
        const diff = (Number(left.entry.tokens) || 0) - (Number(right.entry.tokens) || 0);
        if (diff !== 0) return diff * dir;
        return left.index - right.index;
      }
      const leftCell = left.cells[sortKey];
      const rightCell = right.cells[sortKey];
      const leftAvailable = Boolean(leftCell?.available);
      const rightAvailable = Boolean(rightCell?.available);
      if (leftAvailable !== rightAvailable) {
        return leftAvailable ? -1 : 1;
      }
      if (leftAvailable && rightAvailable && leftCell.value !== rightCell.value) {
        return (leftCell.value - rightCell.value) * dir;
      }
      const tokenDiff = (Number(right.entry.tokens) || 0) - (Number(left.entry.tokens) || 0);
      if (tokenDiff !== 0) return tokenDiff;
      return left.index - right.index;
    })
    .map((item) => item.entry);
}

export function formatRateParts(rate) {
  const number = rate < 0.1
    ? "<0.1"
    : formatNumber(rate, { maximumFractionDigits: 1 });
  return { number, unit: "tok/s" };
}

export function formatDurationParts(ms) {
  if (ms < 1000) {
    return {
      number: formatNumber(ms, { maximumFractionDigits: 1 }),
      unit: "ms",
    };
  }
  return {
    number: formatNumber(ms / 1000, { maximumFractionDigits: 2 }),
    unit: "s",
  };
}

/// 模型速率导出行：缺失的计时保持空字符串，不写成 0。
export function buildModelsCsvTable(entries) {
  const header = [
    "agent",
    "model",
    "tokens",
    "share_percent",
    "first_token_latency_ms",
    "first_token_samples",
    "stream_tokens_per_sec",
    "stream_samples",
    "response_tokens_per_sec",
    "response_samples",
    "request_duration_ms",
    "request_duration_samples",
  ];
  const rows = (entries || []).map((entry) => {
    const cells = extractModelTimingCells(entry);
    return [
      entry.agent || "",
      entry.model || "",
      Number(entry.tokens || 0),
      Number(entry.share || 0).toFixed(2),
      cells.firstToken.available ? cells.firstToken.value.toFixed(1) : "",
      cells.firstToken.available ? cells.firstToken.sampleCount : "",
      cells.stream.available ? cells.stream.value.toFixed(2) : "",
      cells.stream.available ? cells.stream.sampleCount : "",
      cells.response.available ? cells.response.value.toFixed(2) : "",
      cells.response.available ? cells.response.sampleCount : "",
      cells.duration.available ? cells.duration.value.toFixed(1) : "",
      cells.duration.available ? cells.duration.sampleCount : "",
    ];
  });
  return { header, rows };
}
