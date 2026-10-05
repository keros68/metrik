import { formatNumber } from "./i18n.js";

// 位数自适应：数值越大小数越少，保证任何量级都不超过 4 个有效字符
// （紧凑态 41px 大字的容器只有约 5 字符宽）。
function scaledUnit(amount, divisor, unit) {
  const value = amount / divisor;
  const decimals = value >= 100 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(decimals).replace(/\.0+$/, "")}${unit}`;
}

export function compactTokens(value) {
  const amount = Number(value || 0);
  // 阈值取 999.5 个单位，避免四舍五入出现 "1000M" 这类五位结果。
  if (amount >= 999_500_000) return scaledUnit(amount, 1_000_000_000, "B");
  if (amount >= 999_500) return scaledUnit(amount, 1_000_000, "M");
  if (amount >= 1_000) return scaledUnit(amount, 1_000, "K");
  return formatNumber(amount);
}
