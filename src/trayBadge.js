/// Windows 托盘数字徽标：把 Agent 列表最上方一行的官方余量画成任务栏图标。
/// 决策逻辑（取哪个数字、写什么提示）与画布渲染分开：决策部分可在 Node
/// 单测里直接跑，画布只在浏览器里被调用。

/// 渲染边长（像素）。托盘图标在 100%–200% DPI 下最大显示 32px，
/// 以 32 渲染、由系统按需缩小，三个档位都不会放大糊掉。
const TRAY_BADGE_EDGE = 32;

/// 数字可用的最大宽度：给圆角底留出边缘余量。
const TRAY_BADGE_TEXT_BOX = 26;

/// 隐藏窗口时的徽标刷新间隔：与可见小插件同档（5 分钟）。
/// 托盘数字常亮在任务栏上，不能随窗口隐藏而冻结。
export const TRAY_BADGE_HIDDEN_REFRESH_MS = 300_000;

/// 余量取值归一：非有限值（含 null）代表不可用，其余四舍五入并钳到 0–100。
/// 上游（macStatusItems）已经钳过，这里再挡一层，防止未来调用方绕过。
export function normalizeBadgePercent(value) {
  if (!Number.isFinite(value)) return null;
  return Math.max(0, Math.min(100, Math.round(value)));
}

/// 从菜单栏同源的状态列表里取排序最高的 Agent（列表第一项）。
/// 列表为空或首项无效时返回 null，调用方按「无徽标」处理。
export function trayBadgeSpec(items) {
  if (!Array.isArray(items) || items.length === 0) return null;
  const top = items[0];
  if (!top || typeof top.agent !== "string") return null;
  return {
    agent: top.agent,
    percent: normalizeBadgePercent(top.remaining),
    stale: Boolean(top.stale),
  };
}

/// 徽标文字：不可用显示 --，与菜单栏同一语法；正常只有数字，16px 的
/// 托盘图标塞不下百分号，精确值由悬停提示给出。
export function trayBadgeText(percent) {
  return percent == null ? "--" : String(percent);
}

/// 悬停提示，与 macOS 状态项同一套文案。
export function trayBadgeTooltip(agentLabel, percent, stale) {
  const body = percent == null
    ? `${agentLabel} 配额不可用`
    : `${agentLabel} 剩余 ${percent}%`;
  return stale ? `Metrik · ${body} · 数据可能已过期` : `Metrik · ${body}`;
}

/// 状态指纹：Agent、数字或过期标记任一变化才算新徽标，避免每次快照
/// 都把同一张图标重发一遍。
export function trayBadgeKey(spec) {
  if (!spec) return null;
  const percent = spec.percent == null ? "--" : String(spec.percent);
  return `${spec.agent}:${percent}:${spec.stale ? 1 : 0}`;
}

const BADGE_COLORS = {
  // 深色圆角底保证对比：任务栏无论明暗，白字/琥珀字都立得住。
  background: "rgba(23, 25, 30, 0.94)",
  border: "rgba(255, 255, 255, 0.16)",
  fresh: "#f4f5f7",
  // 过期用琥珀色区分，图标里塞不下 macOS 的 ~ 前缀，颜色就是过期标记。
  stale: "#e8b04b",
  unavailable: "#8f96a3",
};

const BADGE_FONT_STACK = '"Geist Variable", "Segoe UI", system-ui, sans-serif';

/// 把徽标画成 RGBA 位图。只被浏览器端调用（windowClient），Node 单测不经过这里。
export function renderTrayQuotaBadge(percent, stale) {
  const size = TRAY_BADGE_EDGE;
  const canvas = document.createElement("canvas");
  canvas.width = size;
  canvas.height = size;
  const context = canvas.getContext("2d", { willReadFrequently: true });

  // 圆角底：整幅收在 1px 边距内，四角透明，贴进托盘槽位不出硬边。
  context.beginPath();
  context.roundRect(1, 1, size - 2, size - 2, 9);
  context.fillStyle = BADGE_COLORS.background;
  context.fill();
  context.strokeStyle = BADGE_COLORS.border;
  context.lineWidth = 1;
  context.stroke();

  const text = trayBadgeText(percent);
  const color = percent == null
    ? BADGE_COLORS.unavailable
    : stale
      ? BADGE_COLORS.stale
      : BADGE_COLORS.fresh;

  // 从大到小收到放得下为止；两位数一般停在 19–20px，三位数（100/--）收得
  // 更小。数字宽度由真实测量决定，不按字符数硬编码档位。
  let fontSize = 20;
  for (;;) {
    context.font = `700 ${fontSize}px ${BADGE_FONT_STACK}`;
    if (context.measureText(text).width <= TRAY_BADGE_TEXT_BOX || fontSize <= 12) break;
    fontSize -= 1;
  }

  context.fillStyle = color;
  context.textAlign = "center";
  context.textBaseline = "middle";
  // middle 基线以字面框为准，纯数字会略偏高，下移 1px 到视觉中心。
  context.fillText(text, size / 2, size / 2 + 1);

  const { data } = context.getImageData(0, 0, size, size);
  return { rgba: data, width: size, height: size };
}
