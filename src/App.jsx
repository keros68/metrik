import { lazy, Suspense, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  ArrowDown,
  ArrowUp,
  ArrowsClockwise,
  ArrowsDownUp,
  ArrowsInLineVertical,
  ArrowsInSimple,
  ArrowsLeftRight,
  ArrowsOutSimple,
  CaretLeft,
  CaretRight,
  ChartBar,
  ChartLineUp,
  Check,
  CircleHalfTilt,
  Moon,
  Sun,
  Copy,
  CornersOut,
  ClockCounterClockwise,
  Database,
  DotsThree,
  EyeSlash,
  FileText,
  FolderSimple,
  FunnelSimple,
  GearSix,
  HardDrives,
  Minus,
  PushPinSimple,
  ShieldCheck,
  Trash,
  X,
} from "@phosphor-icons/react";
import antigravityAppIcon from "./assets/antigravity-app-icon.png";
import chatgptAppIcon from "./assets/chatgpt-app-icon.png";
import claudeAppIcon from "./assets/claude-app-icon.jpg";
import hermesAppIcon from "./assets/hermes-app-icon.png";
import kimiAppIcon from "./assets/kimi-app-icon.png";
import opencodeAppIcon from "./assets/opencode-app-icon.png";
import qoderAppIcon from "./assets/qoder-app-icon.png";
import grokAppIcon from "./assets/grok-app-icon.png";
import piAppIcon from "./assets/pi-app-icon.png";
import qwenAppIcon from "./assets/qwen-app-icon.png";
import workbuddyAppIcon from "./assets/workbuddy-app-icon.png";
import zcodeAppIcon from "./assets/zcode-app-icon.png";
import { glassShellAppearance, nextGlassTint, resolveGlassMode } from "./glassAppearance.js";
import { QUOTA_LOW_REMAINING, bindingWindow } from "./quotaWindows.js";
import { horizontalStripTargetWidth } from "./windowGeometry";
import {
  configureQoderCookie,
  configureSync,
  getClaudeHookStatus,
  getClaudeOauthStatus,
  getQoderCookieStatus,
  setClaudeOauth,
  getSyncSettings,
  getUsageReport,
  exportCsvFile,
  getUsageSessions,
  getUsageProjects,
  getProjectRules,
  setProjectRules,
  getUsageSnapshot,
  rebuildLocalLedger,
  removeSyncDevice,
  setClaudeHook,
} from "./usageClient";
import {
  UI_SCALE_RANGE,
  applyStartupUiScale,
  applyWindowMode,
  broadcastMacAgentSelection,
  broadcastMacAppearance,
  checkForUpdate,
  closeWindow,
  getMacAgentSelection,
  getAutostart,
  installUpdate,
  isDesktop,
  isLinuxPlatform,
  isMacPlatform,
  isWindowsPlatform,
  minimizeWindow,
  onMacAgentSelection,
  onMacAppearance,
  onScaleFactorChanged,
  onTrayPinnedChange,
  onTrayShowExpanded,
  openExpandedWindow,
  readStripScale,
  readUiScale,
  reassertCompactSize,
  reassertStripSize,
  resizeCompactWindow,
  resizeMacosPanel,
  resizeStripWindow,
  restoreWindowPosition,
  setAutostart,
  setNativeTheme,
  setStripScale,
  updateMacStatusItems,
  updateTrayQuotaBadge,
  setWindowGlass,
  setPinnedHoverTargetOpacity,
  setWindowPinned,
  setWindowUiScale,
  startEdgeDock,
  startPositionMemory,
  syncLinuxTrayPinned,
  stripContentSize,
  toggleMaximizeWindow,
} from "./windowClient";
import {
  TRAY_BADGE_HIDDEN_REFRESH_MS,
  trayBadgeSpec,
  trayBadgeTooltip,
} from "./trayBadge.js";

// macOS 是菜单栏应用：小插件是贴着菜单栏图标的面板（没有窗口按钮、不可拖动、
// 材质由系统 vibrancy 承担），完整视图是独立的标准窗口（原生红绿灯）。
// Windows 仍是"单窗口变形 + 自绘按钮"，两条路径不互相影响。
const IS_MAC = isMacPlatform();
// Windows 任务栏托盘可换成余量数字徽标；其它平台没有这条路径。
const IS_WINDOWS = isWindowsPlatform();
const IS_LINUX = isLinuxPlatform();

const UsagePlot = lazy(() =>
  import("./UsagePlot").then((module) => ({ default: module.UsagePlot })),
);

const PERIODS = [
  { id: "today", label: "今日" },
  { id: "week", label: "7 天" },
  { id: "month", label: "30 天" },
];

const NAV_ITEMS = [
  { id: "overview", label: "概览", icon: ChartLineUp },
  { id: "usage", label: "用量", icon: ChartBar },
  { id: "reports", label: "报告", icon: FileText },
  { id: "settings", label: "设置", icon: GearSix },
];

const AGENT_META = {
  codex: {
    label: "ChatGPT",
    accent: "#246bdb",
    iconSrc: chatgptAppIcon,
    iconClass: "agent-icon--codex",
  },
  claude: {
    // 额度是 Claude 全产品合并的，展示名不限定 Code；数据源仍是 Claude Code 日志。
    label: "Claude",
    accent: "#e36b49",
    iconSrc: claudeAppIcon,
    iconClass: "agent-icon--claude",
  },
  zcode: {
    label: "GLM",
    accent: "#6a5ae0",
    iconSrc: zcodeAppIcon,
    iconClass: "agent-icon--zcode",
  },
  opencode: {
    label: "OpenCode",
    accent: "#1f9d8b",
    iconSrc: opencodeAppIcon,
    iconClass: "agent-icon--opencode",
  },
  kimi: {
    label: "Kimi",
    // 品红：与 codex 蓝彻底拉开（原来 #3f74f2 和 ChatGPT 的蓝在图里分不清）。
    accent: "#c6538c",
    iconSrc: kimiAppIcon,
    iconClass: "agent-icon--kimi",
  },
  antigravity: {
    label: "Antigravity",
    // 琥珀金：六个 Agent 各占一个色相，不与 Claude 的珊瑚橙混淆。
    accent: "#cf9526",
    iconSrc: antigravityAppIcon,
    iconClass: "agent-icon--antigravity",
  },
  workbuddy: {
    // 覆盖腾讯 CodeBuddy Code 与 WorkBuddy 两个同格式来源，展示名从用户口径。
    label: "WorkBuddy",
    // 与品牌同色系的绿；也正好避开 GLM 的 #6a5ae0。
    accent: "#3d9c50",
    iconSrc: workbuddyAppIcon,
    iconClass: "agent-icon--workbuddy",
  },
  qoder: {
    // 配额-only：Qoder/QoderWork/Qoder CLI 共用官网账户级 Credits。
    label: "Qoder",
    // 深青蓝：与 codex 的宝蓝、opencode 的青绿保持距离。
    accent: "#3a7ca5",
    iconSrc: qoderAppIcon,
    iconClass: "agent-icon--qoder",
  },
  grok: {
    // xAI Grok Build：本地单轮 usage + CLI 日志里的周 Credits 快照。
    label: "Grok",
    // 中性灰蓝：xAI 品牌本身是黑白，中性色既贴合品牌又与八家彩色拉开。
    accent: "#6e7681",
    iconSrc: grokAppIcon,
    iconClass: "agent-icon--grok",
  },
  pi: {
    // pi（badlogic/pi-mono）是 harness：本地会话用量按 provider 归属到
    // 对应计量卡片（GLM / Qwen / 其余留 Pi）；pi 自身没有独立套餐，不显示配额。
    label: "Pi",
    // 官方 logo 是黑白单色几何 π（pi.dev/logo-auto.svg），非红色；
    // 强调色取中性银灰，暗/亮主题都可见。
    accent: "#9aa0a6",
    iconSrc: piAppIcon,
    iconClass: "agent-icon--pi",
  },
  qwen: {
    // 百炼个人 Token Plan 是账户级套餐，由 pi 等客户端的 qwen-token-plan key
    // 消耗；额度没有可编程的官方接口，这张卡只记本地归属用量。
    label: "Qwen",
    // 千问 App 官方图标；紫与 GLM 的 #6a5ae0 同系不同值，亮度更高。
    accent: "#7b5cd6",
    iconSrc: qwenAppIcon,
    iconClass: "agent-icon--qwen",
  },
  hermes: {
    // Hermes（Nous Research）是 harness：与 pi 同样没有自己的套餐，走别家
    // coding plan 的用量按路由归属到对应卡片（GLM / Kimi / ChatGPT），其余
    // 直连 API 留在这张卡；不显示配额。
    label: "Hermes",
    // 官方应用图标是黑白 nous-girl 圆角瓦片（戴耳机的少女，四角透明）；
    // 强调色取与 pi 银灰相邻但更深的暖灰，两张中性卡在图里不互相混淆。
    accent: "#8a8d92",
    iconSrc: hermesAppIcon,
    iconClass: "agent-icon--hermes",
  },
};

const AGENT_ORDER = Object.keys(AGENT_META);

function visibleAgentId(agentId) {
  return agentId === "kimiwork" ? "kimi" : agentId;
}

function normalizeVisibleAgentList(agentIds) {
  return [...new Set(agentIds.map(visibleAgentId))].filter((id) =>
    AGENT_ORDER.includes(id),
  );
}

/// 用户在设置里排出的顺序优先，其余按注册表顺序垫后。设置面板、小组件、
/// 完整视图侧栏共用这一份顺序，改一处四处同步。
function agentIdsInDisplayOrder(preferred) {
  return [...preferred, ...AGENT_ORDER.filter((agentId) => !preferred.includes(agentId))];
}

// 胶囊条首帧尺寸估计：一格只有图标 + 百分比两层，横条约 54px 宽、竖条约
// 46px 高。
// 这些常量只用于进入 strip 的第一帧，之后的窗口尺寸由 StripBar 里的
// 内容测量观察器按真实渲染结果收敛——不同字体/DPI/缩放比例、有无更新点
// 都不会再裁掉内容（曾因常量与 CSS 脱钩裁掉竖条最后一个按钮）。
const STRIP_CELL_WIDTH = 54;
// 控件区只剩状态灯/菜单入口那一个 26px 槽，两个平台一样：横条是外壳
// padding 11 + 槽 26 + 格间距，竖条是外壳 padding 10 + 控件区 padding 4 + 槽 26。
const STRIP_CHROME_WIDTH = 40;
// 条高的下限是 26px 的控件槽，不是格子内容（图标 16 + 上下 padding = 24）。
const STRIP_BAR_HEIGHT = 28;
// 竖条宽度由 26px 控件槽 + 外壳 padding 定死下限（32px）；42 留 10px 呼吸，
// 再宽图标和百分比周围就空得发肥。
const STRIP_VERTICAL_WIDTH = 42;
const STRIP_VCELL_HEIGHT = 46;
// 横条宽度的收缩迟滞。一格 54px，所以 6px 远低于「真的少了一个 Agent」，
// 又高于 DPI/zoom 取整带来的亚像素噪声。
const STRIP_WIDTH_SHRINK_SLACK = 6;
const STRIP_VCHROME_HEIGHT = 40;

function stripWindowSize(orientation, count) {
  const cells = Math.max(1, count);
  if (orientation === "vertical") {
    return {
      width: STRIP_VERTICAL_WIDTH,
      height: STRIP_VCHROME_HEIGHT + STRIP_VCELL_HEIGHT * cells,
    };
  }
  return { width: STRIP_CHROME_WIDTH + STRIP_CELL_WIDTH * cells, height: STRIP_BAR_HEIGHT };
}

/// 竖条内容高度：第一格到控件区 + 外壳 padding（CSS px）。竖条的格子是
/// flex:none，高度由真实内容决定，测量值双向可信（过裁则长、过高则收）。
function measureStripVerticalContent(shell) {
  const first = shell.querySelector(".strip-cell, .strip-empty");
  const controls = shell.querySelector(".strip-controls");
  if (!first || !controls) return null;
  const style = window.getComputedStyle(shell);
  const firstRect = first.getBoundingClientRect();
  const controlsRect = controls.getBoundingClientRect();
  // 1px 余量：分数 DPI 下物理像素取整最多吃掉不到 1 个 CSS px。
  return (
    controlsRect.bottom - firstRect.top +
    parseFloat(style.paddingTop) + parseFloat(style.paddingBottom) + 1
  );
}

/// 横条目标宽度：格子按设计宽 54/格（图标 + 三位百分比是字体无关的有界
/// 内容），控件区取实测自然宽（flex:none 永不被压缩；有无更新点、macOS
/// 有无固定键都会变）。横条格子 flex:1 会拉伸填满窗口，布局测量推不出
/// "窗口过宽"，所以格数部分必须用设计宽计算，窗口才能随格数增减伸缩。
function measureStripHorizontalTarget(shell) {
  const controls = shell.querySelector(".strip-controls");
  if (!controls) return null;
  const style = window.getComputedStyle(shell);
  const cellCount = Math.max(1, shell.querySelectorAll(".strip-cell").length);
  const controlsWidth = controls.getBoundingClientRect().width;
  return horizontalStripTargetWidth({
    cellCount,
    cellWidth: STRIP_CELL_WIDTH,
    controlsWidth,
    paddingLeft: parseFloat(style.paddingLeft),
    paddingRight: parseFloat(style.paddingRight),
    gap: parseFloat(style.columnGap || style.gap) || 0,
  });
}

const AGENT_LABELS = Object.fromEntries(
  AGENT_ORDER.map((id) => [id, AGENT_META[id].label]),
);

/// 状态灯的含义：绿 = 数据正常，黄（呼吸）= 正在更新，红 = 读取失败。
/// 灯本身只是装饰，含义必须悬浮可见，否则用户永远猜不到。
function statusDotTitle(loading, loadError) {
  if (loadError) return "数据读取失败，仍显示上次成功的数据";
  return loading ? "正在更新数据…" : "数据正常";
}

// 位数自适应：数值越大小数越少，保证任何量级都不超过 4 个有效字符
// （紧凑态 41px 大字的容器只有约 5 字符宽）。
function scaledUnit(amount, divisor, unit) {
  const value = amount / divisor;
  const decimals = value >= 100 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(decimals).replace(/\.0+$/, "")}${unit}`;
}

// 小组件窗口高度跟随 Agent 行数。内容自然高 = 各行 getBoundingClientRect
// 实测之和（行高固定 52px，见 styles.css 的 .widget-agent-list grid-auto-rows）；
// 非列表部分（标题栏/双块/页脚/间距）也是实测（shell.clientHeight - list.clientHeight）。
// 不写布局常量——写死的常量差 4px 就会逼出一条本不该出现的滚动条。
// 列表至少一行高：只选一个 Agent 时卡片随之收短，不再固定 320。
const COMPACT_LIST_MIN_HEIGHT = 52;
// 卡片窗口高度下限：非列表实测部分约 208px + 单行 52px，留少量余量取 260，
// 与 windowClient 的 WINDOW_SIZES.compact.minHeight 一致。
const COMPACT_MIN_WINDOW_HEIGHT = 260;

function compactTokens(value) {
  const amount = Number(value || 0);
  // 阈值取 999.5 个单位，避免四舍五入出现 "1000M" 这类五位结果。
  if (amount >= 999_500_000) return scaledUnit(amount, 1_000_000_000, "B");
  if (amount >= 999_500) return scaledUnit(amount, 1_000_000, "M");
  if (amount >= 1_000) return scaledUnit(amount, 1_000, "K");
  return amount.toLocaleString("zh-CN");
}

function exactTokens(value) {
  return Number(value || 0).toLocaleString("zh-CN");
}

function formatClock(isoString) {
  if (!isoString) return "--:--";
  const value = new Date(isoString);
  if (Number.isNaN(value.getTime())) return "--:--";
  return value.toLocaleTimeString("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
}

function formatReset(minutes) {
  if (!Number.isFinite(minutes)) return "暂不可用";
  if (minutes >= 1440) {
    const days = Math.floor(minutes / 1440);
    const hours = Math.floor((minutes % 1440) / 60);
    return `${days} 天 ${hours} 小时`;
  }
  const hours = Math.floor(minutes / 60);
  const rest = Math.max(0, Math.round(minutes % 60));
  return `${hours} 小时 ${rest} 分`;
}

function formatQuotaAge(minutes) {
  if (!Number.isFinite(minutes) || minutes < 1) return "刚刚";
  if (minutes < 60) return `${Math.round(minutes)} 分钟前`;
  if (minutes < 1440) return `${Math.floor(minutes / 60)} 小时前`;
  return `${Math.floor(minutes / 1440)} 天前`;
}

function quotaProvenance(quota) {
  if (!quota.available) return "暂无可靠来源";
  if (quota.quality === "demo") return "演示数据";
  if (quota.resetExpired) return "窗口已重置 · 等待刷新";
  if (quota.stale || quota.quality === "official_snapshot") {
    return `官方快照 · ${formatQuotaAge(quota.ageMinutes)}`;
  }
  return "官方 · 实时";
}

function snapshotIsPartial(snapshot) {
  return snapshot.sources?.some((source) => source.quality === "partial") || false;
}

const UNAVAILABLE_QUOTA = {
  available: false,
  remainingPercent: 0,
  resetsInMinutes: null,
  ageMinutes: null,
  stale: false,
  resetExpired: false,
  sourceLabel: "暂无可靠来源",
  quality: "unavailable",
};

function agentQuotaFor(snapshot, agentId) {
  return (
    snapshot.agentQuotas?.find((entry) => entry.agent === agentId) || {
      agent: agentId,
      windows: [],
    }
  );
}

function quotaHasData(entry) {
  return Boolean(entry?.windows?.some((window) => window.view.available));
}

/// 没有额度数字时该说什么。后端在 note 里带上直连失败的真实原因（凭据过期、
/// 缺 scope、限流……）；有原因就别再叫用户去开状态栏钩子——他多半已经开了
/// 直连，而且不用 Claude Code 的人开了钩子也不会有数据。
function quotaEmptyCopy(entry, agentId, short = false) {
  if (entry?.note) return short ? "直连查询失败" : "直连查询失败 · 见设置";
  if (agentId !== "claude") return short ? "官方配额不可用" : "暂无可靠来源";
  return short ? "设置中开启配额钩子" : "在设置中开启配额钩子后显示";
}

function shortWindowLabel(key) {
  if (key === "five_hour" || key === "primary") return "5h";
  if (key === "seven_day" || key === "secondary") return "7d";
  if (key === "extra_usage") return "超额";
  if (key === "credits") return "额度";
  if (key === "monthly_cycle") return "月度";
  return key.replace(/^seven_day_/, "").slice(0, 4);
}

// 小插件配额卡固定两行：优先取来源的前两个窗口，缺则补占位。
// 占位只补真实窗口没占用的标签——prolite 套餐唯一的真实窗口就是周窗（7d），
// 再补一个 seven_day 占位会出现两行 "7d"（实机踩过）。
function compactQuotaWindows(entry) {
  const rows = (entry.windows || []).slice(0, 2);
  const usedLabels = new Set(rows.map((window) => shortWindowLabel(window.key)));
  const placeholders = [
    { key: "five_hour", label: "Session", view: UNAVAILABLE_QUOTA },
    { key: "seven_day", label: "每周", view: UNAVAILABLE_QUOTA },
  ].filter((placeholder) => !usedLabels.has(shortWindowLabel(placeholder.key)));
  while (rows.length < 2 && placeholders.length) {
    rows.push(placeholders.shift());
  }
  return rows;
}

// 模型名展示：本地确实缺模型名的记 "unknown"（未标注模型）；
// "synced-remote" 是同步事件（导出本就不含模型名，见 sync 架构约束），
// 不是某个叫这个名字的模型，必须说人话。
function modelDisplayName(model) {
  if (model === "synced-remote") return "其他设备同步（无模型名）";
  if (model === "unknown") return "未标注模型";
  return model;
}

// 每行 Agent 的 tooltip 列出全部有来源的窗口（已过期窗口也算有来源，单独走
// "已重置，等待刷新"文案）；一个都没有时由调用方渲染 "-- / 暂无可靠来源"，
// 绝不编造数字。不能截断：行上显示的是 bindingWindow 挑出来的那个，它可能排在
// 第三位（Kimi 的月度窗口就是），截断会出现"行上写着月度、tooltip 里没有月度"。
function compactDisplayWindows(entry) {
  return (entry.windows || []).filter((window) => window.view.available);
}

// 原生 title tooltip：逐窗口列出剩余与重置倒计时，并标注官方/快照/演示来源，
// 让官方配额与本地解析用量始终可区分。
function compactQuotaTooltip(agentId, windows) {
  if (!windows.length) return `${AGENT_META[agentId].label}：暂无可靠来源`;
  const lines = windows.map((window) => {
    const view = window.view;
    const label = window.label || shortWindowLabel(window.key);
    if (view.resetExpired) return `${label}：已重置，等待刷新`;
    const reset = Number.isFinite(view.resetsInMinutes)
      ? ` · ${formatReset(view.resetsInMinutes)}后重置`
      : "";
    return `${label}：剩余 ${Math.round(view.remainingPercent)}%${reset} · ${quotaProvenance(view)}`;
  });
  return [AGENT_META[agentId].label, ...lines].join("\n");
}

// 胶囊条一格展示当前真正约束用量的窗口，规则见 quotaWindows.js。
function stripCellData(entry) {
  const windows = (entry.windows || []).filter(
    (window) => window.view.available && !window.view.resetExpired,
  );
  if (!windows.length) return null;
  return { tightest: bindingWindow(windows) || windows[0], windows };
}

// 原生 title tooltip：列出全部窗口的剩余与重置倒计时；快照数据标注更新时间。
function stripTooltip(agentId, windows) {
  const lines = windows.map((window) => {
    const view = window.view;
    const reset = Number.isFinite(view.resetsInMinutes)
      ? ` · ${formatReset(view.resetsInMinutes)}后重置`
      : "";
    return `${window.label || shortWindowLabel(window.key)}：剩余 ${Math.round(view.remainingPercent)}%${reset}`;
  });
  const first = windows[0].view;
  const head =
    first.stale || first.quality === "official_snapshot"
      ? `${AGENT_META[agentId].label}（官方快照 · ${formatQuotaAge(first.ageMinutes)}）`
      : AGENT_META[agentId].label;
  return [head, ...lines].join("\n");
}

function quotaUsedPercent(view) {
  return Math.min(100, Math.max(0, 100 - view.remainingPercent));
}

function windowLengthMinutes(key) {
  if (key === "five_hour" || key === "primary") return 300;
  if (key === "seven_day" || key === "secondary" || key?.startsWith?.("seven_day")) return 10080;
  return null;
}

// 接近耗尽的分级警示：85% 起提醒、95% 起告急（四个竞品一致的做法）。
function quotaSeverity(view) {
  if (!view.available || view.resetExpired) return "";
  const used = quotaUsedPercent(view);
  if (used >= 95) return "critical";
  // 与 bindingWindow 的"告急"同一条线：越过它，这个窗口才会顶到行首。
  if (used >= 100 - QUOTA_LOW_REMAINING) return "warn";
  return "";
}

// 消耗节奏（仅长窗口有意义）：已用占比对比窗口已经过时间占比，
// 由官方百分比与重置倒计时推得，属于本地推算而非官方指标。
function quotaPace(view, key) {
  const length = windowLengthMinutes(key);
  if (!length || length < 10080 || !view.available) return null;
  if (!Number.isFinite(view.resetsInMinutes) || view.resetExpired) return null;
  const elapsed = Math.min(length, Math.max(0, length - view.resetsInMinutes));
  if (elapsed < length * 0.05) return null;
  const delta = quotaUsedPercent(view) - (elapsed / length) * 100;
  // 三档表述：从容（不超节奏）、略偏快（10 个百分点内）、偏快（大概率撑不到重置）。
  const tone = delta <= 0 ? "ahead" : delta <= 10 ? "close" : "behind";
  return { delta, tone };
}

function QuotaBarRow({ label, view, windowKey, accent }) {
  const severity = quotaSeverity(view);
  const pace = quotaPace(view, windowKey);
  return (
    <>
      <div
        className={`quota-bar-row ${severity ? `quota-bar-row--${severity}` : ""}`}
        style={accent ? { "--quota-accent": accent } : undefined}
      >
        <small>{label}</small>
        <div className="quota-bar-track" aria-hidden="true">
          {/* 窗口已过期的快照不再显示旧百分比，避免把陈旧值当现状。 */}
          <i style={{ transform: `scaleX(${view.available && !view.resetExpired ? quotaUsedPercent(view) / 100 : 0})` }} />
        </div>
        <em>{view.available && !view.resetExpired ? `已用 ${Math.round(quotaUsedPercent(view))}%` : "--"}</em>
        <span>
          {view.resetExpired
            ? "已重置，等待刷新"
            : view.available
              ? `${formatReset(view.resetsInMinutes)}后重置`
              : "暂不可用"}
        </span>
      </div>
      {pace && (
        <small className={`quota-pace ${pace.tone === "behind" ? "quota-pace--behind" : ""}`}>
          {pace.tone === "ahead"
            ? `节奏从容 ${Math.abs(pace.delta).toFixed(0)}% · 按当前用量可维持至重置`
            : pace.tone === "close"
              ? `节奏略偏快 ${pace.delta.toFixed(0)}% · 接近临界节奏`
              : `节奏偏快 ${pace.delta.toFixed(0)}% · 按当前用量重置前可能耗尽`}
        </small>
      )}
    </>
  );
}

/// 数据来源状态的文案。完整说明一律进 title。
/// title + hint 给紧凑卡片底栏；state 给侧栏——那里主标题恒为「数据统计」，
/// 状态由灯和 state 这一行承担，不把"数据不完整"顶成入口的名字。
/// "部分覆盖" 这种自造词读者看不懂，直接说清楚哪里不全。
function sourceStatusCopy(snapshot, loading, partial) {
  const indexingPending = snapshot.indexing?.pending || 0;
  if (snapshot.pending) {
    return {
      title: "正在读取",
      hint: "首次扫描",
      state: "正在读取",
      detail: "正在读取本机日志，日志库较大时首次扫描需要几分钟。",
    };
  }
  if (snapshot.loadError) {
    return {
      title: "读取失败",
      hint: "点此排查",
      state: "读取失败",
      detail: "本机日志读取失败。界面不会以演示数据替代，点此查看数据来源。",
    };
  }
  if (indexingPending > 0) {
    return {
      title: "补齐历史",
      hint: `还剩 ${indexingPending}`,
      // 侧栏那行不能换行，只放数字；完整说明走 title 与统计说明抽屉。
      state: `补齐中 ${indexingPending}`,
      detail: `正在补齐历史索引，还剩 ${indexingPending} 个日志文件。历史周期的数字尚不完整，会随补齐自动更新。`,
    };
  }
  if (partial) {
    return {
      title: "数据不完整",
      hint: "点此查看",
      state: "数据不完整",
      detail: "部分日志未能解析，统计数字低于真实用量。点此查看受影响的来源。",
    };
  }
  if (snapshot.isDemo) {
    return {
      title: "演示数据",
      hint: "非真实用量",
      state: "演示数据",
      detail: "当前显示的是演示数据，不是本机真实用量。",
    };
  }
  return {
    title: "数据源正常",
    hint: loading ? "更新中" : formatClock(snapshot.generatedAt),
    state: loading ? "更新中" : `更新于 ${formatClock(snapshot.generatedAt)}`,
    detail: `各项数据均可溯源。更新于 ${formatClock(snapshot.generatedAt)}。`,
  };
}

function Sidebar({ activeNav, onNavChange, snapshot, loading }) {
  const partial = snapshotIsPartial(snapshot);
  const indexing = (snapshot.indexing?.pending || 0) > 0;
  const sourceStatus = sourceStatusCopy(snapshot, loading, partial);
  return (
    <aside className="sidebar" aria-label="主导航">
      <div className="wordmark">Metrik</div>

      <nav className="nav-stack">
        {NAV_ITEMS.map(({ id, label, icon: Icon }) => (
          <button
            className={`nav-button ${activeNav === id ? "nav-button--active" : ""}`}
            key={id}
            type="button"
            aria-label={label}
            aria-current={activeNav === id ? "page" : undefined}
            onClick={() => onNavChange(id)}
          >
            <Icon size={27} weight="light" aria-hidden="true" />
            <span className="nav-dot" aria-hidden="true" />
            <span className="tooltip-label">{label}</span>
          </button>
        ))}
      </nav>

      {/* 这块是数据来源页的入口，主标题恒为「数据统计」；状态由上方的灯和下面
          那行承担。两行都不换行、高度固定：文案随状态变长会把整条导航顶得上下
          跳。完整说明放 title。 */}
      <button
        className="source-status"
        type="button"
        onClick={() => onNavChange("sources")}
        title={sourceStatus.detail}
      >
        <span className={`status-dot ${loading || indexing ? "status-dot--loading" : ""} ${snapshot.loadError ? "status-dot--error" : ""} ${partial && !indexing ? "status-dot--warning" : ""}`} />
        <span>数据统计</span>
        <small>{sourceStatus.state}</small>
      </button>
    </aside>
  );
}

function PeriodControl({ period, onChange, compact = false, fullWidthArea = false }) {
  return (
    <div
      className={`period-control ${compact ? "period-control--compact" : ""} ${fullWidthArea ? "period-control--full" : ""}`}
      role="group"
      aria-label="统计周期"
    >
      {PERIODS.map((item) => (
        <button
          type="button"
          key={item.id}
          className={period === item.id ? "is-selected" : ""}
          aria-pressed={period === item.id}
          onClick={() => onChange(item.id)}
        >
          {item.label}
        </button>
      ))}
    </div>
  );
}

function UsageChart({ snapshot, selectedAgent, dark = false }) {
  const visibleAgents = selectedAgent === "all" ? AGENT_ORDER : [selectedAgent];
  // 图例与图中的线一致：只列周期内有数据的 Agent。
  const legendAgents = selectedAgent === "all"
    ? AGENT_ORDER.filter((agent) =>
        snapshot.series.some((point) => Number(point.tokens?.[agent] || 0) > 0))
    : [selectedAgent];

  return (
    <section className="chart-section" aria-labelledby="usage-chart-title">
      <h2 id="usage-chart-title" className="sr-only">用量趋势</h2>
      <span className="axis-caption">{snapshot.period === "today" ? "tokens · 当日累计" : "tokens · 每日增量"}</span>
      <div className="chart-frame">
        <Suspense fallback={<div className="chart-loading">正在准备趋势图</div>}>
          <UsagePlot
            series={snapshot.series}
            visibleAgents={visibleAgents}
            selectedAgent={selectedAgent}
            agentLabels={AGENT_LABELS}
            formatTokens={exactTokens}
            dark={dark}
          />
        </Suspense>
      </div>
      <div className="chart-legend" aria-label="图例">
        {(legendAgents.length ? legendAgents : visibleAgents.slice(0, 1)).map((agent) => (
          <span key={agent}>
            <i className={`legend-line legend-line--${agent}`} />
            {AGENT_META[agent]?.label || agent}
          </span>
        ))}
      </div>
    </section>
  );
}

function formatUsd(value) {
  const amount = Number(value || 0);
  const decimals = amount >= 100 ? 0 : amount >= 10 ? 1 : 2;
  return `$${amount.toFixed(decimals)}`;
}

const TOKEN_COMPONENTS = [
  { key: "inputUncached", label: "未缓存输入", color: "#246bdb" },
  { key: "cacheRead", label: "缓存读取", color: "#9dbdf0" },
  { key: "cacheWrite", label: "缓存写入", color: "#6a5ae0" },
  { key: "output", label: "输出", color: "#e36b49" },
];

// Token 构成 + 模型分布：都来自本地账本的精确解析（processed 口径，非账单）。
function BreakdownSection({ snapshot, selectedAgent }) {
  const scopedAgents = selectedAgent === "all"
    ? snapshot.agents
    : snapshot.agents.filter((agent) => agent.id === selectedAgent);
  const components = TOKEN_COMPONENTS.map((component) => ({
    ...component,
    value: scopedAgents.reduce((sum, agent) => sum + Number(agent[component.key] || 0), 0),
  }));
  const componentTotal = components.reduce((sum, component) => sum + component.value, 0);
  const models = (snapshot.models || [])
    .filter((entry) => selectedAgent === "all" || entry.agent === selectedAgent)
    .slice(0, 6);
  const modelMax = models[0]?.tokens || 1;

  const cost = snapshot.cost;
  const costRows = cost?.available
    ? cost.byAgent.filter((row) =>
        (selectedAgent === "all" || row.agent === selectedAgent) && (row.usd > 0 || row.unpricedTokens > 0))
      .sort((left, right) => right.usd - left.usd)
    : [];
  const scopedUsd = costRows.reduce((sum, row) => sum + row.usd, 0);
  const scopedUnpriced = costRows.reduce((sum, row) => sum + row.unpricedTokens, 0);

  if (!componentTotal && !models.length && !costRows.length) return null;

  return (
    <section className="breakdown-grid" aria-label="Token 构成、模型分布与成本估算">
      {componentTotal > 0 && (
        <article className="breakdown-card">
          <h2>Token 构成</h2>
          <div className="comp-bar" role="img" aria-label="按处理类型的 token 构成比例">
            {components.filter((component) => component.value > 0).map((component) => (
              <i
                key={component.key}
                style={{
                  width: `${(component.value / componentTotal) * 100}%`,
                  backgroundColor: component.color,
                }}
              />
            ))}
          </div>
          <ul className="comp-legend">
            {components.map((component) => (
              <li key={component.key}>
                <i style={{ backgroundColor: component.color }} aria-hidden="true" />
                <span>{component.label}</span>
                <em>{compactTokens(component.value)} · {((component.value / componentTotal) * 100).toFixed(1)}%</em>
              </li>
            ))}
          </ul>
        </article>
      )}
      {costRows.length > 0 && (
        <article className="breakdown-card">
          <h2>成本估算</h2>
          <p className="cost-total">
            <strong>
              <span className="cost-currency">$</span>
              {formatUsd(scopedUsd).slice(1)}
            </strong>
            <span>本周期 · API 等价</span>
          </p>
          <ul className="comp-legend">
            {costRows.map((row) => (
              <li key={row.agent}>
                <i style={{ backgroundColor: AGENT_META[row.agent]?.accent || "#74767a", borderRadius: "50%" }} aria-hidden="true" />
                <span>{AGENT_META[row.agent]?.label || row.agent}</span>
                <em>{row.usd > 0 ? formatUsd(row.usd) : "未计价"}</em>
              </li>
            ))}
          </ul>
          <p className="cost-note">
            按公开 API 价格（{cost.pricingAsOf}）折算，非官方账单。
            {scopedUnpriced > 0 ? `另有 ${compactTokens(scopedUnpriced)} tokens 因无可靠定价未计入。` : ""}
          </p>
        </article>
      )}
      {models.length > 0 && (
        <article className="breakdown-card">
          <h2>模型分布</h2>
          <ul className="model-list">
            {models.map((entry) => (
              <li key={`${entry.agent}-${entry.model}`}>
                <i
                  className="model-dot"
                  style={{ backgroundColor: AGENT_META[entry.agent]?.accent || "#74767a" }}
                  aria-hidden="true"
                  title={AGENT_META[entry.agent]?.label || entry.agent}
                />
                <span className="model-name">{modelDisplayName(entry.model)}</span>
                <span className="model-track" aria-hidden="true">
                  <i style={{ transform: `scaleX(${entry.tokens / modelMax})`, backgroundColor: AGENT_META[entry.agent]?.accent || "#74767a" }} />
                </span>
                <em>{compactTokens(entry.tokens)} · {entry.share.toFixed(1)}%</em>
              </li>
            ))}
          </ul>
        </article>
      )}
    </section>
  );
}

function ChartState({ pending }) {
  return (
    <section className="chart-section" aria-labelledby="usage-chart-state-title">
      <div className="chart-state" role="status">
        <HardDrives size={28} weight="light" aria-hidden="true" />
        <div>
          <h2 id="usage-chart-state-title">{pending ? "正在读取本机趋势" : "趋势暂不可用"}</h2>
          <p>{pending ? "索引完成后将显示真实曲线。" : "读取失败时不会以零值或演示曲线替代。"}</p>
        </div>
      </div>
    </section>
  );
}

function AgentMark({ agentId }) {
  const meta = AGENT_META[agentId];
  return (
    <span className={`agent-icon ${meta.iconClass}`} aria-hidden="true">
      {meta.iconSrc ? (
        <img src={meta.iconSrc} alt="" draggable="false" />
      ) : (
        <i className="agent-monogram" style={{ backgroundColor: meta.accent }}>{meta.monogram}</i>
      )}
    </span>
  );
}

function Inspector({ snapshot, selectedAgent, onSelectAgent, onOpenSources, widgetAgents }) {
  const dataUnavailable = snapshot.pending || snapshot.loadError;
  const partial = snapshotIsPartial(snapshot);
  const chosen = new Set(widgetAgents);
  // 侧栏两块都只留用户勾选的 Agent，外加本周期确有用量的。勾选的含义是「即使
  // 为 0 也留一行」，有用量则一律不藏——否则装了七个 Agent 的机器上半屏都是
  // "0 tokens 0.0%" 的死行，而单靠勾选又会把真实用量从统计软件里抹掉。
  // 图表、Token 构成、成本估算不受此影响，顶部总量始终等于全部 Agent 之和。
  const keepsRow = (agent) =>
    // 数据还没到位时不按用量过滤：那会儿人人都是 0，滤完只剩勾选的两行，
    // 快照一到又跳回七行。
    dataUnavailable ||
    chosen.has(agent.id) ||
    agent.tokens > 0 ||
    // 当前用作筛选的那个始终留一行：否则切到它没有用量的周期时行会消失，
    // 图表还筛着它，却再没有可点的地方把筛选取消掉。
    agent.id === selectedAgent;
  const rankedAgents = [...snapshot.agents]
    .filter(keepsRow)
    // 用得最多的排最上面。后端按注册表顺序返回，那个顺序对读者没有意义。
    // Array.sort 是稳定的：并列（尤其一堆 0）时保持注册表顺序，不会每次刷新乱跳。
    .sort((left, right) => right.tokens - left.tokens);
  return (
    <aside className="inspector" aria-label="配额与 Agent 明细">
      <div className="quota-groups" aria-label="各 Agent 官方配额">
        {/* 严格按勾选，顺序即勾选顺序。配额卡是实时状态而非历史用量，没勾就
            不该冒出来——哪怕它确有官方额度来源。勾了但没有来源的仍占一行，
            "暂无可靠来源"本身是有效信息。 */}
        {widgetAgents.filter((agentId) => AGENT_META[agentId]).map((agentId) => {
          const entry = agentQuotaFor(snapshot, agentId);
          const hasData = quotaHasData(entry);
          const provenanceView = entry.windows?.find((window) => window.view.available)?.view;
          return (
            <section className="quota-group" key={agentId}>
              <header>
                <strong>{AGENT_META[agentId].label}</strong>
                <small title={hasData ? undefined : entry.note || undefined}>
                  {hasData ? quotaProvenance(provenanceView) : quotaEmptyCopy(entry, agentId)}
                </small>
              </header>
              {hasData &&
                entry.windows.map((window) => (
                  <QuotaBarRow
                    key={window.key}
                    label={window.label}
                    view={window.view}
                    windowKey={window.key}
                    accent={AGENT_META[agentId].accent}
                  />
                ))}
            </section>
          );
        })}
      </div>

      <div className="agent-list" aria-label="按 Agent 筛选">
        {rankedAgents.map((agent) => {
          const meta = AGENT_META[agent.id];
          if (!meta) return null;
          const isSelected = selectedAgent === agent.id;
          return (
            <button
              type="button"
              className={`agent-row ${isSelected ? "agent-row--selected" : ""}`}
              key={agent.id}
              aria-pressed={isSelected}
              onClick={() => onSelectAgent(isSelected ? "all" : agent.id)}
            >
              <i className="agent-accent" style={{ backgroundColor: meta.accent }} />
              <AgentMark agentId={agent.id} />
              <span className="agent-copy">
                <strong>{meta.label}</strong>
                <small>{snapshot.pending || snapshot.loadError ? "--" : compactTokens(agent.tokens)} tokens</small>
              </span>
              <span className="agent-share">{dataUnavailable ? "--" : `${agent.share.toFixed(1)}%`}</span>
              <CaretRight size={19} weight="light" aria-hidden="true" />
            </button>
          );
        })}
      </div>

      <button className={`traceability ${snapshot.loadError ? "traceability--error" : ""} ${partial ? "traceability--warning" : ""}`} type="button" onClick={onOpenSources}>
        <span><ShieldCheck size={17} weight="fill" />{snapshot.pending ? "正在读取本机数据" : snapshot.loadError ? "数据暂不可用" : partial ? "部分数据可能不完整" : "数据源正常"}</span>
        <small>{snapshot.pending ? "后台建立索引，窗口仍可操作" : snapshot.loadError ? "失败结果未以演示数据替代" : partial ? "打开统计说明查看受影响来源" : snapshot.isDemo ? "当前为演示模式" : `本地统计 + 官方配额 · ${formatClock(snapshot.generatedAt)}`}</small>
      </button>
    </aside>
  );
}

let windowActionQueue = Promise.resolve();
let latestWindowCorrection = 0;

function runWindowAction(action) {
  // hide/move/zoom/resize/show 是一条事务；并发执行时旧屏幕 factor 算出的迟到
  // resize 会覆盖新屏幕上的修正。统一排队，失败后也让后续操作继续。
  windowActionQueue = windowActionQueue.then(action).catch((error) => {
    console.warn("Unable to update the desktop window.", error);
  });
  return windowActionQueue;
}

function runLatestWindowCorrection(action) {
  const correction = ++latestWindowCorrection;
  return runWindowAction(() => {
    if (correction !== latestWindowCorrection) return undefined;
    return action();
  });
}

/// 标题栏的主题快捷键：单击在亮/暗之间切换，右键（或长按）弹出含「自动」的
/// 三选菜单——单击不进「自动」是刻意的：一次点击只该有一个确定结果，
/// 而「自动」的结果取决于系统当前是什么。
function ThemeQuickToggle({ theme, darkTheme, onThemeChange }) {
  const [menuOpen, setMenuOpen] = useState(false);
  const wrapRef = useRef(null);

  useEffect(() => {
    if (!menuOpen) return undefined;
    const dismiss = (event) => {
      if (!wrapRef.current?.contains(event.target)) setMenuOpen(false);
    };
    const onEscape = (event) => {
      if (event.key === "Escape") setMenuOpen(false);
    };
    document.addEventListener("mousedown", dismiss);
    document.addEventListener("keydown", onEscape);
    return () => {
      document.removeEventListener("mousedown", dismiss);
      document.removeEventListener("keydown", onEscape);
    };
  }, [menuOpen]);

  const label = theme === "auto" ? "自动（跟随系统）" : theme === "dark" ? "暗色" : "亮色";
  return (
    <div className="theme-quick" ref={wrapRef}>
      <button
        type="button"
        className={`window-action ${menuOpen ? "window-action--active" : ""}`}
        onClick={() => onThemeChange(darkTheme ? "light" : "dark")}
        onContextMenu={(event) => {
          event.preventDefault();
          setMenuOpen((open) => !open);
        }}
        aria-label={`切换明暗，当前${label}`}
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        title={`当前：${label}\n单击切换明暗 · 右键选择模式`}
      >
        {darkTheme ? (
          <Moon size={16} weight="light" aria-hidden="true" />
        ) : (
          <Sun size={16} weight="light" aria-hidden="true" />
        )}
      </button>
      {menuOpen && (
        <div className="theme-quick-menu" role="menu">
          {THEME_OPTIONS.map((option) => (
            <button
              key={option.id}
              type="button"
              role="menuitemradio"
              aria-checked={theme === option.id}
              className={theme === option.id ? "is-selected" : ""}
              onClick={() => {
                onThemeChange(option.id);
                setMenuOpen(false);
              }}
            >
              {option.label}
              {theme === option.id && <Check size={13} weight="bold" aria-hidden="true" />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function WindowActions({ mode, pinned, transparent = false, glassTint = "dark", macMinimal = false, theme, darkTheme, onThemeChange, onToggleMode, onTogglePinned, onToggleTransparent }) {
  const glassName = (id) =>
    GLASS_TINT_OPTIONS.find((option) => option.id === id)?.label || "深色";
  const glassCurrent = normalizeGlassTint(glassTint);
  const glassNext = nextGlassTint(glassCurrent);
  const glassLabel = `${glassName(glassCurrent)} · 点击切为${glassName(glassNext)}`;
  return (
    <div className={`window-actions window-actions--${mode}`} aria-label="窗口操作">
      {mode === "expanded" && (
        <>
          <button
            type="button"
            className="window-action"
            onClick={() => onToggleMode("compact")}
            aria-label="收起为桌面小插件"
            title="收起为桌面小插件"
          >
            <ArrowsInSimple size={17} weight="light" aria-hidden="true" />
          </button>
          <button
            type="button"
            className="window-action"
            onClick={() => onToggleMode("strip")}
            aria-label="折叠为胶囊条"
            title="折叠为胶囊条"
          >
            <ArrowsInLineVertical size={16} weight="light" aria-hidden="true" />
          </button>
          <ThemeQuickToggle theme={theme} darkTheme={darkTheme} onThemeChange={onThemeChange} />
        </>
      )}
      {mode === "compact" && !macMinimal && (
        <button
          type="button"
          className="window-action"
          onClick={() => onToggleMode("strip")}
          aria-label="折叠为胶囊条"
          title="折叠为胶囊条"
        >
          <ArrowsInLineVertical size={16} weight="light" aria-hidden="true" />
        </button>
      )}
      {mode === "compact" && !macMinimal && (
        <button
          type="button"
          className={`window-action ${transparent ? "window-action--active" : ""}`}
          onClick={onToggleTransparent}
          aria-label={`外观：${glassLabel}`}
          title={`外观：${glassLabel}`}
        >
          <CircleHalfTilt size={16} weight={transparent ? "fill" : "light"} aria-hidden="true" />
        </button>
      )}
      {!macMinimal && mode !== "expanded" && (
        <button
          type="button"
          className={`window-action ${pinned ? "window-action--active" : ""}`}
          onClick={onTogglePinned}
          aria-label={pinned ? "取消固定，恢复拖动" : "固定在当前位置并置顶"}
          aria-pressed={pinned}
          title={pinned ? "取消固定，恢复拖动" : "固定在当前位置并置顶"}
        >
          <PushPinSimple size={17} weight={pinned ? "fill" : "light"} aria-hidden="true" />
        </button>
      )}
      {!macMinimal && (
        <>
          <button
            type="button"
            className="window-action"
            onClick={() => runWindowAction(minimizeWindow)}
            aria-label="最小化"
            title="最小化"
          >
            <Minus size={17} weight="light" aria-hidden="true" />
          </button>
          {mode === "expanded" && (
            <button
              type="button"
              className="window-action"
              onClick={() => runWindowAction(toggleMaximizeWindow)}
              aria-label="最大化或还原"
              title="最大化 / 还原"
            >
              <CornersOut size={16} weight="light" aria-hidden="true" />
            </button>
          )}
          <button
            type="button"
            className="window-action window-action--close"
            onClick={() => runWindowAction(closeWindow)}
            aria-label="隐藏到托盘"
            title="隐藏到托盘"
          >
            <X size={17} weight="light" aria-hidden="true" />
          </button>
        </>
      )}
    </div>
  );
}

function handleGlassPointerMove(event) {
  const shell = event.currentTarget;
  const bounds = shell.getBoundingClientRect();
  if (!bounds.width || !bounds.height) return;
  const frame = shell.parentElement?.id === "root" ? shell.parentElement : shell;
  const x = Math.max(0, Math.min(100, ((event.clientX - bounds.left) / bounds.width) * 100));
  const y = Math.max(0, Math.min(100, ((event.clientY - bounds.top) / bounds.height) * 100));
  frame.style.setProperty("--glass-pointer-x", `${x}%`);
  frame.style.setProperty("--glass-pointer-y", `${y}%`);
  frame.style.setProperty("--glass-edge-opacity", "1");
}

function handleGlassPointerLeave(event) {
  const shell = event.currentTarget;
  const frame = shell.parentElement?.id === "root" ? shell.parentElement : shell;
  frame.style.setProperty("--glass-edge-opacity", "0");
}

function glassPointerProps(enabled) {
  return enabled
    ? {
        onPointerMove: handleGlassPointerMove,
        onPointerLeave: handleGlassPointerLeave,
      }
    : {};
}

function StripBar({
  snapshot,
  agents,
  pinned,
  loading,
  transparent,
  glassAlpha = 0.82,
  glassMode = "css",
  glassTint = "dark",
  glassInk = "dark",
  orientation,
  onToggleOrientation,
  onTogglePinned,
  onRestore,
  onExpand,
  availableUpdate,
  onOpenUpdate,
}) {
  // 用户自选的 agent 一律占格；没有官方配额数据的显示 "--"，不伪造数字。
  const cells = agents.map((agentId) => ({
    agentId,
    cell: stripCellData(agentQuotaFor(snapshot, agentId)),
  }));
  const dragProps = pinned || IS_MAC ? {} : { "data-tauri-drag-region": true };
  const vertical = orientation === "vertical";
  // 透明档的真实桌面背景变化很大，控制图标加粗以稳定识别。
  const buttonWeight = transparent && glassTint === "clear" ? "bold" : "regular";
  const shellRef = useRef(null);
  const OrientationIcon = vertical ? ArrowsLeftRight : ArrowsDownUp;
  // 控制按钮不再常驻：四个 26px 的槽在竖条上要吃掉 130px，比三个格子还高。
  // 改成按需就地展开——点状态灯位上浮出的 … 把它们放出来，条身随之变长，
  // 指针离开自动收回。窗口尺寸本来就跟着内容测量走，这里不用另外调窗。
  const [controlsOpen, setControlsOpen] = useState(false);
  const toggleControls = useCallback(() => setControlsOpen((open) => !open), []);
  // 一旦进入置顶只读态，立即丢弃胶囊的临时操作面板状态，只保留数据展示。
  useEffect(() => {
    if (pinned && IS_LINUX) setControlsOpen(false);
  }, [pinned]);
  const shellAppearance = glassShellAppearance("strip", {
    transparent,
    glassMode,
    glassTint,
    glassInk,
    glassAlpha,
    isMac: IS_MAC,
    vertical,
  });
  // 窗口尺寸跟随真实内容（通用方案，替代手写常量）：每次渲染后与视口变化时
  // 复核目标尺寸，差 ≥1px 才调窗；量的是 CSS px，resizeStripWindow 内部统一
  // 乘缩放系数与 DPI。任何字体/DPI/缩放/更新点组合都收敛，不再裁按钮。
  useLayoutEffect(() => {
    if (IS_MAC || !isDesktop()) return undefined;
    const shell = shellRef.current;
    if (!shell) return undefined;
    let timer = null;
    const fit = () => {
      timer = null;
      const isVertical = shell.classList.contains("strip-shell--vertical");
      if (isVertical) {
        const targetHeight = measureStripVerticalContent(shell);
        if (!targetHeight) return;
        // 交叉轴是设计常量：竖条恒为 52 宽（方向切换后窗口可能还停在横条宽度）。
        if (
          Math.abs(targetHeight - shell.clientHeight) < 1
          && Math.abs(shell.clientWidth - STRIP_VERTICAL_WIDTH) <= 1
        ) {
          return;
        }
        runLatestWindowCorrection(() =>
          resizeStripWindow({ width: STRIP_VERTICAL_WIDTH, height: Math.ceil(targetHeight) }),
        );
        return;
      }
      const targetWidth = measureStripHorizontalTarget(shell);
      if (!targetWidth) return;
      // 交叉轴是设计常量：横条恒为 40 高。
      // 宽度方向做不对称迟滞：变宽立即跟进（否则格子被压到内容以下，图标和
      // 百分比会叠在一起），变窄要超过一格的余量才动。测量噪声只会让目标略微
      // 变小，对称的 1px 阈值会让它和调窗形成震荡（双屏用户实拍到持续闪烁）。
      const widthDelta = targetWidth - shell.clientWidth;
      if (
        widthDelta < 1
        && widthDelta > -STRIP_WIDTH_SHRINK_SLACK
        && shell.clientHeight === STRIP_BAR_HEIGHT
      ) {
        return;
      }
      runLatestWindowCorrection(() =>
        resizeStripWindow({ width: Math.ceil(targetWidth), height: STRIP_BAR_HEIGHT }),
      );
    };
    const schedule = () => {
      window.clearTimeout(timer);
      // 60ms：缓存未命中时的尺寸修正也要尽量落在同一帧动画里，不让人看见。
      timer = window.setTimeout(fit, 60);
    };
    schedule();
    // webfont 换字可能发生在 60ms 首次 fit 之后（没有触发器补测就会留下
    // 测量空窗，竖条底部按钮被裁）：字体就绪补测一次，再兜底一个 450ms
    // 的二次测量。
    let disposed = false;
    document.fonts?.ready?.then(() => {
      if (!disposed) schedule();
    });
    const lateTimer = window.setTimeout(schedule, 450);
    if (typeof ResizeObserver === "undefined") {
      return () => {
        disposed = true;
        window.clearTimeout(timer);
        window.clearTimeout(lateTimer);
      };
    }
    const observer = new ResizeObserver(schedule);
    observer.observe(shell);
    return () => {
      disposed = true;
      window.clearTimeout(timer);
      window.clearTimeout(lateTimer);
      observer.disconnect();
    };
  });
  const glassProps = glassPointerProps(shellAppearance.edgeInteractive);
  // 展开态跟着指针走：移出条身就收回，省得用户忘了收，条一直长着。
  const handlePointerLeave = (event) => {
    glassProps.onPointerLeave?.(event);
    setControlsOpen(false);
  };
  return (
    <main
      ref={shellRef}
      className={shellAppearance.className}
      data-glass-surface={shellAppearance.trueAlpha ? "true-alpha" : undefined}
      data-pinned={pinned && IS_LINUX ? "true" : undefined}
      inert={pinned && IS_LINUX ? true : undefined}
      {...dragProps}
      {...glassProps}
      onPointerLeave={handlePointerLeave}
      style={{
        ...shellAppearance.style,
        ...(pinned ? { cursor: "default" } : {}),
      }}
    >
      <h1 className="sr-only">Metrik 官方配额胶囊条</h1>
      {cells.length ? (
        cells.map(({ agentId, cell }) => {
          const meta = AGENT_META[agentId];
          if (!cell) {
            return (
              <div
                key={agentId}
                className="strip-cell strip-cell--unavailable"
                title={`${meta.label}：官方配额不可用`}
                {...dragProps}
              >
                <img
                  className={`strip-cell-icon ${meta.iconClass || ""}`}
                  src={meta.iconSrc}
                  alt={meta.label}
                  draggable={false}
                />
                <span className="strip-cell-body">
                  <em>--</em>
                </span>
              </div>
            );
          }
          const view = cell.tightest.view;
          const severity = quotaSeverity(view);
          return (
            <div
              key={agentId}
              className={`strip-cell ${severity ? `strip-cell--${severity}` : ""}`}
              title={stripTooltip(agentId, cell.windows)}
              {...dragProps}
            >
              <img
                className={`strip-cell-icon ${meta.iconClass || ""}`}
                src={meta.iconSrc}
                alt={meta.label}
                draggable={false}
              />
              <span className="strip-cell-body">
                <em>{Math.round(view.remainingPercent)}%</em>
              </span>
            </div>
          );
        })
      ) : (
        <span className="strip-empty" {...dragProps}>
          配额不可用
        </span>
      )}
      <div className={`strip-controls ${controlsOpen && !(pinned && IS_LINUX) ? "strip-controls--open" : ""}`}>
        {availableUpdate && (
          <span className="strip-control-slot">
            <button
              type="button"
              className="update-dot"
              onClick={onOpenUpdate}
              aria-label={`有新版本 ${availableUpdate.version}，打开设置更新`}
              title={`有新版本 ${availableUpdate.version}，点击更新`}
            />
          </span>
        )}
        {/* 状态灯与展开入口共用一个 26px 槽：… 绝对定位压在灯上，
            悬停或展开时互换，收起态因此一个像素都不多占。 */}
        <span
          className="strip-control-slot strip-control-slot--menu"
          title={statusDotTitle(loading, snapshot.loadError)}
        >
          <i
            className={`status-dot ${loading ? "status-dot--loading" : ""} ${snapshot.loadError ? "status-dot--error" : ""}`}
            aria-hidden="true"
          />
          {!(pinned && IS_LINUX) && (
            <button
              type="button"
              className={`strip-button strip-button--menu ${controlsOpen ? "strip-button--active" : ""}`}
              onClick={toggleControls}
              aria-label={controlsOpen ? "收起控制按钮" : "展开控制按钮"}
              aria-expanded={controlsOpen}
              title={controlsOpen ? "收起控制按钮" : "展开控制按钮"}
            >
              <DotsThree size={16} weight={buttonWeight} aria-hidden="true" />
            </button>
          )}
        </span>
        {controlsOpen && !(pinned && IS_LINUX) && (
          <>
            {!IS_MAC && (
              <button
                type="button"
                className={`strip-button ${pinned ? "strip-button--active" : ""}`}
                onClick={onTogglePinned}
                aria-label={pinned ? "取消固定，恢复拖动" : "固定在当前位置并置顶"}
                aria-pressed={pinned}
                title={pinned ? "取消固定，恢复拖动" : "固定在当前位置并置顶"}
              >
                <PushPinSimple size={15} weight={pinned ? "fill" : buttonWeight} aria-hidden="true" />
              </button>
            )}
            <button
              type="button"
              className="strip-button"
              onClick={onToggleOrientation}
              aria-label={vertical ? "切换为横条" : "切换为竖条"}
              title={vertical ? "切换为横条" : "切换为竖条"}
            >
              <OrientationIcon size={15} weight={buttonWeight} aria-hidden="true" />
            </button>
            <button
              type="button"
              className="strip-button"
              onClick={onRestore}
              aria-label="展开为桌面小插件"
              title="展开为桌面小插件"
            >
              <ArrowsOutSimple size={15} weight={buttonWeight} aria-hidden="true" />
            </button>
            <button
              type="button"
              className="strip-button"
              onClick={onExpand}
              aria-label="打开完整视图"
              title="完整视图"
            >
              <CornersOut size={15} weight={buttonWeight} aria-hidden="true" />
            </button>
          </>
        )}
      </div>
    </main>
  );
}

function CompactWidget({
  snapshot,
  period,
  selectedAgent,
  visibleTokens,
  loading,
  pinned,
  transparent,
  glassMode = "css",
  glassTint = "dark",
  glassInk = "dark",
  onPeriodChange,
  onOpenSources,
  onTogglePinned,
  onToggleTransparent,
  onExpand,
  onRefresh,
  quotaAgent,
  onCycleQuotaAgent,
  widgetAgents,
  glassAlpha = 0.82,
  availableUpdate,
  onOpenUpdate,
}) {
  const comparisonIsFlat = Math.abs(snapshot.comparisonPercent) < 0.5;
  const comparisonIsLower = snapshot.comparisonPercent < -0.5;
  const ComparisonArrow = comparisonIsLower ? ArrowDown : ArrowUp;
  const shellRef = useRef(null);
  // 宽度失配自愈的节流时间戳：失配不消失时观察器会一直触发，没有节流会
  // 每 120ms 重断言一次尺寸；距上次不足 2s 不再重复自愈（失配消失即止），
  // 替代旧的 3 次终身上限——上限烧完后失配就永远留着了。跨屏/DPI 变化时
  // 清零，让新显示器上的自愈立即恢复资格。自愈只重断言当前尺寸，不再完整
  // hide/show 窗口或恢复记忆位置，避免旧显示器状态覆盖新 DPI。
  const lastDesyncHealRef = useRef(0);
  useEffect(() => {
    if (!isDesktop()) return undefined;
    let cancel = null;
    onScaleFactorChanged(() => {
      lastDesyncHealRef.current = 0;
    }).then((fn) => {
      cancel = fn;
    });
    return () => {
      cancel?.();
    };
  }, []);
  // 一个观察器承担两条自愈：
  // 1) 宽度失配（zoom×物理尺寸失配，视口 < 320，右侧被裁）→ 按当前 DPI
  //    重断言尺寸（节流到 2s 一次，失配消失即止）；
  // 2) Agent 行数变化 → 窗口高度跟随内容（1-2 行回 320，更多行加高，
  //    工作区装不下的部分由列表内部滚动承担）。行数变化不改外壳尺寸，
  //    ResizeObserver 感知不到，所以每次渲染后再主动复核一次。
  useEffect(() => {
    if (!isDesktop()) return undefined;
    const shell = shellRef.current;
    if (!shell || typeof ResizeObserver === "undefined") return undefined;
    let timer = null;
    const check = () => {
      timer = null;
      const rect = shell.getBoundingClientRect();
      // 宽度失配（zoom×物理尺寸失配，视口 < 320，右侧被裁）是 Windows 单窗口
      // 变形独有的问题；macOS 面板没有 zoom，不会失配。
      const widthDesynced =
        shell.scrollWidth > shell.clientWidth + 1 || rect.width > window.innerWidth + 1;
      if (!IS_MAC && widthDesynced) {
        const now = Date.now();
        if (now - lastDesyncHealRef.current < 2000) return;
        lastDesyncHealRef.current = now;
        runLatestWindowCorrection(() => reassertCompactSize());
        return;
      }
      const list = shell.querySelector(".widget-agent-list");
      if (!list) return;
      // 内容自然高 = 实际行高之和（getBoundingClientRect，与窗口大小无关）。
      // 绝不能用 list.scrollHeight：它不小于 clientHeight，窗口一大于内容
      // 就等于窗口高，目标高度会每轮 +4px 无限延伸（实测踩过）。
      const rowEls = list.querySelectorAll(".widget-agent");
      let natural = COMPACT_LIST_MIN_HEIGHT;
      if (rowEls.length) {
        const firstRow = rowEls[0].getBoundingClientRect();
        const lastRow = rowEls[rowEls.length - 1].getBoundingClientRect();
        natural = Math.max(COMPACT_LIST_MIN_HEIGHT, Math.ceil(lastRow.bottom - firstRow.top));
      }
      // 非列表部分实测 + 列表自然高 + 4px 余量（1fr 分配与取整的抖动）。
      // 用 scrollHeight 兜底：只有一个 Agent 时窗口会收到比内容还矮，底栏被
      // 裁在可视区外，clientHeight 就量不到它，于是"目标高度"一直等于当前
      // 高度，卡在底栏永远不显示的稳定错误态。
      const chrome = Math.max(shell.scrollHeight, shell.clientHeight) - list.clientHeight;
      const target = Math.max(COMPACT_MIN_WINDOW_HEIGHT, Math.round(chrome + natural + 4));
      if (IS_MAC) {
        // macOS 面板顶部锚定菜单栏图标：高度跟随内容（屏幕可用高 - 80 封顶），
        // 宽度恒为设计宽；resize 后面板会自己重算锚点。
        const cap = Math.max(
          COMPACT_MIN_WINDOW_HEIGHT,
          Math.floor((window.screen?.availHeight || 900) - 80),
        );
        const macTarget = Math.min(target, cap);
        if (Math.abs(macTarget - shell.clientHeight) >= 2) {
          runWindowAction(() => resizeMacosPanel({ height: macTarget }));
        }
        return;
      }
      if (Math.abs(target - shell.clientHeight) >= 2) {
        runLatestWindowCorrection(() => resizeCompactWindow({ height: target }));
      }
    };
    const schedule = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(check, 120);
    };
    const observer = new ResizeObserver(schedule);
    observer.observe(shell);
    schedule();
    return () => {
      window.clearTimeout(timer);
      observer.disconnect();
    };
  });
  // 标签必须描述快照本身的周期；切换周期的扫描期间不给旧数据贴新标签。
  const comparisonLabel = snapshot.period === "today" ? "较近 7 日同时段" : "较前一周期";
  const flatComparisonLabel = snapshot.period === "today" ? "与近 7 日同时段持平" : "与前一周期持平";
  const switchingPeriod = !snapshot.pending && !snapshot.loadError && period !== snapshot.period;
  const quotaEntry = agentQuotaFor(snapshot, quotaAgent);
  const quotaWindows = compactQuotaWindows(quotaEntry);
  const quotaView = quotaWindows.find((window) => window.view.available)?.view || UNAVAILABLE_QUOTA;
  const partial = snapshotIsPartial(snapshot);
  const sourceStatus = sourceStatusCopy(snapshot, loading, partial);
  const shellAppearance = glassShellAppearance("widget", {
    transparent,
    glassMode,
    glassTint,
    glassInk,
    glassAlpha,
    isMac: IS_MAC,
    loading,
  });

  return (
    <main
      ref={shellRef}
      className={shellAppearance.className}
      data-glass-surface={shellAppearance.trueAlpha ? "true-alpha" : undefined}
      data-pinned={pinned && IS_LINUX ? "true" : undefined}
      inert={pinned && IS_LINUX ? true : undefined}
      {...glassPointerProps(shellAppearance.edgeInteractive)}
      style={shellAppearance.style}
    >
      <h1 className="sr-only">Metrik Agent 用量桌面小插件</h1>
      <header
        className="widget-titlebar"
        // 固定 = 置顶 + 锁定位置：去掉拖动区，窗口停在用户选定的位置。
        // macOS 面板贴着菜单栏图标，拖动无意义，也没有窗口按钮。
        {...(pinned || IS_MAC ? {} : { "data-tauri-drag-region": true })}
        style={pinned || IS_MAC ? { cursor: "default" } : undefined}
      >
        <div
          className="widget-brand"
          {...(pinned || IS_MAC ? {} : { "data-tauri-drag-region": true })}
        >
          <span>Metrik</span>
          <i
            className={`status-dot ${loading ? "status-dot--loading" : ""} ${snapshot.loadError ? "status-dot--error" : ""}`}
            title={statusDotTitle(loading, snapshot.loadError)}
            aria-hidden="true"
          />
          {availableUpdate && (
            <button
              type="button"
              className="update-dot"
              onClick={onOpenUpdate}
              aria-label={`有新版本 ${availableUpdate.version}，打开设置更新`}
              title={`有新版本 ${availableUpdate.version}，点击更新`}
            />
          )}
        </div>
        {!(pinned && IS_LINUX) && (
          <WindowActions
            mode="compact"
            pinned={pinned}
            transparent={transparent}
            glassTint={glassTint}
            macMinimal={IS_MAC}
            onToggleMode={onExpand}
            onTogglePinned={onTogglePinned}
            onToggleTransparent={onToggleTransparent}
          />
        )}
      </header>

      <div className="widget-content">
        <PeriodControl period={period} onChange={onPeriodChange} compact />

        <section className="widget-primary" aria-label="用量摘要">
          <div className="widget-metric">
            <span>
              {selectedAgent === "all" ? "总用量" : AGENT_META[selectedAgent].label}
              {switchingPeriod ? `（${PERIODS.find((item) => item.id === snapshot.period)?.label}）` : ""}
            </span>
            <div aria-live="polite" aria-atomic="true">
              <strong>{snapshot.pending || snapshot.loadError ? "--" : compactTokens(visibleTokens)}</strong>
              <small>tokens</small>
            </div>
            <p className="widget-comparison">
              {switchingPeriod ? (
                <>正在统计{PERIODS.find((item) => item.id === period)?.label}数据…</>
              ) : snapshot.pending ? (
                <>正在建立本地索引</>
              ) : snapshot.loadError ? (
                <>本地数据读取失败</>
              ) : selectedAgent !== "all" ? (
                <>
                  <FunnelSimple size={14} weight="light" aria-hidden="true" />
                  已按 Agent 筛选
                </>
              ) : snapshot.comparisonAvailable ? (
                <>
                  {comparisonIsFlat ? (
                    flatComparisonLabel
                  ) : (
                    <>
                      <ComparisonArrow size={14} weight="bold" aria-hidden="true" />
                      {comparisonLabel}{comparisonIsLower ? "低" : "高"} {Math.abs(snapshot.comparisonPercent).toFixed(0)}%
                    </>
                  )}
                </>
              ) : (
                <>{period === "today" ? "同时段基线建立中" : "基线建立中"}</>
              )}
            </p>
          </div>

          <button
            className="widget-quota"
            style={{ "--quota-accent": AGENT_META[quotaAgent].accent }}
            type="button"
            onClick={onCycleQuotaAgent}
            aria-label={`${AGENT_META[quotaAgent].label} 配额，点击切换 Agent`}
            title="点击切换配额 Agent"
          >
            <span>{AGENT_META[quotaAgent].label} 已用</span>
            {quotaWindows.map((window) => {
              const severity = quotaSeverity(window.view);
              const current = window.view.available && !window.view.resetExpired;
              return (
                <div
                  className={`widget-quota-window ${severity ? `widget-quota-window--${severity}` : ""}`}
                  key={window.key}
                >
                  <small>{shortWindowLabel(window.key)}</small>
                  <div className="widget-quota-track" aria-hidden="true">
                    <i style={{ transform: `scaleX(${current ? quotaUsedPercent(window.view) / 100 : 0})` }} />
                  </div>
                  <em>{current ? `${Math.round(quotaUsedPercent(window.view))}%` : "--"}</em>
                </div>
              );
            })}
            <small>
              {quotaView.quality === "demo"
                ? quotaProvenance(quotaView)
                : quotaView.resetExpired
                  ? "窗口已重置，等待刷新"
                  : quotaView.available
                    ? `${formatReset(quotaView.resetsInMinutes)}后重置`
                    : quotaEmptyCopy(quotaEntry, quotaAgent, true)}
            </small>
          </button>
        </section>

        <section className="widget-agent-list" aria-label="各 Agent 官方剩余额度">
          {(() => {
            // 行集合只由用户的 Agent 选择决定（与胶囊条同一哲学：自选一律占格），
            // 没有可靠配额来源的 Agent 显示 "-- / 暂无可靠来源" 而不是消失——
            // 否则来源一抖动行数就变，窗口高度跟着内容跳来跳去（用户称之为"乱飘"）。
            return widgetAgents.filter((id) => AGENT_META[id]).map((agentId) => {
              const meta = AGENT_META[agentId];
              if (!meta) return null;
              const entry = agentQuotaFor(snapshot, agentId);
              const windows = compactDisplayWindows(entry);
              // 行内头条窗口与胶囊条同规则，见 quotaWindows.js；
              // 完整窗口明细在该行的原生 tooltip 与焦点卡中。
              const headline = bindingWindow(entry.windows) || windows[0] || null;
              const headlineView = headline?.view || null;
              const current = Boolean(headlineView && headlineView.available && !headlineView.resetExpired);
              const severity = headlineView ? quotaSeverity(headlineView) : "";
              const remaining = headlineView ? Math.min(100, Math.max(0, headlineView.remainingPercent)) : 0;
              return (
                <div
                  className={`widget-agent ${severity ? `widget-agent--${severity}` : ""}`}
                  key={agentId}
                  style={{ "--quota-accent": meta.accent }}
                  title={compactQuotaTooltip(agentId, windows)}
                >
                  <i className="widget-agent-accent" style={{ backgroundColor: meta.accent }} aria-hidden="true" />
                  <AgentMark agentId={agentId} />
                  <span>
                    <strong>{meta.label}</strong>
                    {/* 窗口标签与名称同一行：5h/7d 剩余；无来源或已重置时明示状态。 */}
                    <small>
                      {current
                        ? `· ${shortWindowLabel(headline.key)}`
                        : headlineView
                          ? "· 已重置，等待刷新"
                          : snapshot.pending
                            ? "· 正在读取…"
                            : snapshot.loadError
                              ? "· 读取失败"
                              : "· 暂无可靠来源"}
                    </small>
                  </span>
                  {/* 展示剩余额度（用户关心的是还能用多少）。快照新鲜度不再
                      影响数字的写法与颜色：那一格已经写着窗口或"已重置，等待
                      刷新"，再加 ~ 前缀和灰化是重复信息，只会让人以为数字是
                      估算出来的。 */}
                  <em>{current ? `${Math.round(remaining)}%` : "--"}</em>
                </div>
              );
            });
          })()}
        </section>

        <footer className="widget-footer">
          <button
            type="button"
            className={`widget-source ${snapshot.loadError ? "widget-source--error" : ""} ${partial ? "widget-source--warning" : ""}`}
            onClick={onOpenSources}
            title={sourceStatus.detail}
          >
            <ShieldCheck size={15} weight="fill" aria-hidden="true" />
            <span>{sourceStatus.title}</span>
            <small>{sourceStatus.hint}</small>
          </button>
          <button
            type="button"
            className="widget-refresh"
            onClick={onRefresh}
            disabled={loading}
            aria-label="强制刷新官方额度与本地统计"
            title="强制刷新官方额度与本地统计"
          >
            <ArrowsClockwise size={13} weight="light" aria-hidden="true" />
          </button>
          <button type="button" className="widget-expand" onClick={() => onExpand("expanded")}>
            <span>完整视图</span>
            <ArrowsOutSimple size={16} weight="light" aria-hidden="true" />
          </button>
        </footer>
      </div>
    </main>
  );
}

function SourceDrawer({ snapshot, onClose, onRebuildLedger, rebuildState }) {
  const drawerRef = useRef(null);
  const closeButtonRef = useRef(null);
  const cancelRebuildRef = useRef(null);
  const [confirmingRebuild, setConfirmingRebuild] = useState(false);

  useEffect(() => {
    const previouslyFocused = document.activeElement;
    closeButtonRef.current?.focus();

    const keepFocusInside = (event) => {
      if (event.key !== "Tab" || !drawerRef.current) return;
      const focusable = Array.from(
        drawerRef.current.querySelectorAll("button:not([disabled]), [href], [tabindex]:not([tabindex='-1'])"),
      );
      if (!focusable.length) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };

    document.addEventListener("keydown", keepFocusInside);
    return () => {
      document.removeEventListener("keydown", keepFocusInside);
      previouslyFocused?.focus?.();
    };
  }, []);

  useEffect(() => {
    if (confirmingRebuild) cancelRebuildRef.current?.focus();
  }, [confirmingRebuild]);

  const rebuildBusy = rebuildState.status === "busy";
  const rebuildStatusRole = rebuildState.status === "error" ? "alert" : "status";

  const confirmRebuild = () => {
    setConfirmingRebuild(false);
    onRebuildLedger();
  };

  return (
    <div className="drawer-backdrop" role="presentation" onMouseDown={onClose}>
      <section
        ref={drawerRef}
        className="source-drawer"
        role="dialog"
        aria-modal="true"
        aria-labelledby="source-title"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header>
          <h2 id="source-title">统计说明</h2>
          <button ref={closeButtonRef} type="button" className="icon-button" onClick={onClose} aria-label="关闭">
            <X size={21} weight="light" />
          </button>
        </header>

        {(snapshot.indexing?.pending || 0) > 0 && (
          <div className="indexing-note" role="status">
            <ClockCounterClockwise size={20} weight="light" aria-hidden="true" />
            <p>
              正在补齐历史索引，还剩 <strong>{snapshot.indexing.pending}</strong> 个日志文件。
              历史周期的数字尚不完整，会随补齐自动更新。
            </p>
          </div>
        )}

        <div className="source-list">
          {snapshot.sources.map((source) => (
            <article className="source-item" key={source.id}>
              <span className="source-item-icon">
                {source.kind === "official" ? (
                  <ShieldCheck size={22} weight="light" />
                ) : source.kind === "local" ? (
                  <Database size={22} weight="light" />
                ) : (
                  <HardDrives size={22} weight="light" />
                )}
              </span>
              <div>
                <strong>{source.label}</strong>
                <p>{source.detail}</p>
              </div>
              <span className={`quality-badge quality-badge--${source.quality}`}>{source.qualityLabel}</span>
            </article>
          ))}
        </div>

        <div className="privacy-note">
          <ShieldCheck size={20} weight="light" />
          <p>本机会顺序扫描日志，但只解析并保存统计字段；不会提取、保存或上传正文、提示词、工具输出或凭据。SQLite 会保留用量时间、Agent、模型、会话标识与本机源路径。</p>
        </div>

        <section className="ledger-recovery" aria-labelledby="ledger-recovery-title">
          <div className="ledger-recovery-heading">
            <span className="ledger-recovery-icon" aria-hidden="true">
              <ClockCounterClockwise size={21} weight="light" />
            </span>
            <div>
              <h3 id="ledger-recovery-title">重建本地账本</h3>
              <p id="ledger-recovery-description">只清理 Metrik 的派生统计索引，再从本机 Agent 日志重建当前周期。</p>
            </div>
          </div>

          {snapshot.isDemo && (
            <p className="ledger-demo-note">
              浏览器演示：这里仅模拟重建流程，不会访问或删除任何本机文件。
            </p>
          )}

          {confirmingRebuild ? (
            <div className="ledger-confirmation" role="group" aria-labelledby="ledger-confirm-title">
              <strong id="ledger-confirm-title">确认只重建统计索引？</strong>
              <p>原始 Agent 日志、提示词、工具输出与登录凭据都不会被删除或改写。重建可能需要几分钟。</p>
              <div className="ledger-confirm-actions">
                <button
                  ref={cancelRebuildRef}
                  type="button"
                  className="ledger-button ledger-button--secondary"
                  onClick={() => setConfirmingRebuild(false)}
                >
                  取消
                </button>
                <button
                  type="button"
                  className="ledger-button ledger-button--primary"
                  onClick={confirmRebuild}
                >
                  确认重建
                </button>
              </div>
            </div>
          ) : (
            <button
              type="button"
              className={`ledger-button ledger-button--rebuild ${rebuildBusy ? "ledger-button--busy" : ""}`}
              aria-describedby="ledger-recovery-description"
              aria-busy={rebuildBusy}
              disabled={rebuildBusy}
              onClick={() => setConfirmingRebuild(true)}
            >
              <ClockCounterClockwise size={17} weight="light" aria-hidden="true" />
              {rebuildBusy ? "正在重建…" : "重建本地账本"}
            </button>
          )}

          {rebuildState.status !== "idle" && (
            <p
              className={`ledger-rebuild-status ledger-rebuild-status--${rebuildState.status}`}
              role={rebuildStatusRole}
              aria-live={rebuildState.status === "error" ? "assertive" : "polite"}
            >
              {rebuildState.message}
            </p>
          )}
        </section>
      </section>
    </div>
  );
}

function formatSyncTime(ms) {
  if (!Number.isFinite(ms)) return "尚未同步";
  const value = new Date(ms);
  if (Number.isNaN(value.getTime())) return "尚未同步";
  return value.toLocaleString("zh-CN", { hour12: false });
}

function ClaudeHookCard({ onSnapshotRefresh }) {
  const [status, setStatus] = useState(null);
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState(null);

  useEffect(() => {
    let cancelled = false;
    getClaudeHookStatus()
      .then((value) => {
        if (!cancelled) setStatus(value);
      })
      .catch(() => {
        if (!cancelled) setFeedback({ tone: "error", message: "钩子状态读取失败。" });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const toggle = async (enabled) => {
    setBusy(true);
    setFeedback(null);
    try {
      const next = await setClaudeHook(enabled);
      setStatus(next);
      setFeedback({
        tone: "success",
        message: enabled
          ? "钩子已安装。下次 Claude Code 刷新状态栏后，此处即显示官方 5 小时与 7 天额度。"
          : "钩子已卸载，statusLine 设置已恢复。",
      });
      onSnapshotRefresh();
    } catch (error) {
      setFeedback({ tone: "error", message: `${error}` });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="settings-card">
      <h2>Claude Code 官方配额</h2>
      <p className="settings-muted">
        安装一个只提取 5h/7d 额度数字的 statusLine 钩子（不读对话内容、不碰登录凭据）。
        已有自定义 statusLine 会自动串联、原样保留；卸载时恢复原状。
      </p>
      {status?.demo ? (
        <p className="settings-muted">浏览器演示模式：仅桌面应用可配置。</p>
      ) : status && (
        <>
          <div className="settings-directory-row">
            <button
              type="button"
              className={`ledger-button ${status.installed ? "ledger-button--secondary" : "ledger-button--primary"}`}
              disabled={busy || (!status.installed && status.conflict)}
              onClick={() => toggle(!status.installed)}
            >
              {status.installed ? "卸载钩子" : "安装钩子"}
            </button>
          </div>
          <dl className="settings-status">
            <div>
              <dt>状态</dt>
              <dd>
                {status.installed
                  ? `已安装${status.chained ? " · 已串联你原有的状态栏" : ""} · ${
                      status.lastDataAtMs
                        ? `最近数据 ${formatSyncTime(status.lastDataAtMs)}`
                        : "等待 Claude Code 下次刷新状态栏"
                    }`
                  : status.conflict
                    ? "未安装 · 现有 statusLine 缺少 command 字段，无法串联"
                    : "未安装"}
              </dd>
            </div>
          </dl>
        </>
      )}
      {feedback && (
        <p
          className={`settings-feedback settings-feedback--${feedback.tone}`}
          role={feedback.tone === "error" ? "alert" : "status"}
        >
          {feedback.message}
        </p>
      )}
      <ClaudeOauthBlock onSnapshotRefresh={onSnapshotRefresh} />
    </div>
  );
}

// OAuth 官方额度：读取 Claude Code 自己保存的登录凭据（显式 opt-in），
// 直接查询账户级合并额度（含网页版消耗），不依赖终端状态栏。
function ClaudeOauthBlock({ onSnapshotRefresh }) {
  const [status, setStatus] = useState(null);
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState(null);

  useEffect(() => {
    let cancelled = false;
    getClaudeOauthStatus()
      .then((value) => {
        if (!cancelled) setStatus(value);
      })
      .catch(() => {
        if (!cancelled) setFeedback({ tone: "error", message: "官方额度来源状态读取失败。" });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const toggle = async (enabled) => {
    setBusy(true);
    setFeedback(null);
    try {
      const next = await setClaudeOauth(enabled);
      setStatus(next);
      setFeedback({
        tone: "success",
        message: enabled
          ? "已开启。下次刷新起直接查询官方额度（约每 2 分钟一次）；查询失败时自动回落到状态栏钩子。"
          : "已关闭。恢复只用状态栏钩子提供额度。",
      });
      onSnapshotRefresh();
    } catch (error) {
      setFeedback({ tone: "error", message: `${error}` });
    } finally {
      setBusy(false);
    }
  };

  if (status?.demo) return null;

  return (
    <div className="settings-subsection">
      <h3>官方额度直连（OAuth）</h3>
      <p className="settings-muted">
        备选来源：用本机 Claude Code 已保存的凭据直接查询官方额度（账户级合并值，约两分钟一刷新），
        网页版与桌面客户端的消耗同样计入。凭据只在内存中读取，不存储、不上传。
        前提是最近使用过 Claude Code：凭据有效期仅数小时，且仅在 Claude Code 运行时刷新；
        过期后回落到状态栏钩子。
      </p>
      <p className="settings-muted">
        ⚠️ 条款风险须知：Anthropic 2026 年 2 月更新的消费者条款禁止在第三方工具中使用 Claude 订阅的
        OAuth 凭据。目前公开的封禁与拦截集中在借订阅做推理的第三方工具，未见只读用量查询被封号的案例，
        但按条款字面本功能同样属于违规范围。若不愿承担此风险，请保持关闭，使用零凭据的状态栏钩子。
      </p>
      {status && (
        <>
          <div className="settings-directory-row">
            <button
              type="button"
              className={`ledger-button ${status.enabled ? "ledger-button--secondary" : "ledger-button--primary"}`}
              disabled={busy || (!status.enabled && !status.credentialsPresent)}
              onClick={() => toggle(!status.enabled)}
            >
              {status.enabled ? "关闭直连" : "开启直连"}
            </button>
          </div>
          <dl className="settings-status">
            <div>
              <dt>状态</dt>
              <dd>
                {!status.credentialsPresent
                  ? "本机未找到 Claude Code 登录凭据（请先在终端运行 claude 登录）"
                  : !status.scopeOk
                    ? "凭据缺少 user:profile 权限，开启后可能查询失败（可运行 claude login 重新登录）"
                    : status.expired
                      ? `${status.enabled ? "已开启" : "未开启"} · 凭据已过期，运行一次 Claude Code 即可自动刷新`
                      : status.enabled
                        ? "已开启 · 凭据可用"
                        : "未开启 · 凭据可用"}
              </dd>
            </div>
            {/* 开关一切正常、却始终没有额度数字时，唯一能解释原因的就是这一行。 */}
            {status.lastFailure && (
              <div>
                <dt>最近失败</dt>
                <dd>
                  {status.lastFailure.message} · {formatSyncTime(status.lastFailure.atMs)}
                </dd>
              </div>
            )}
          </dl>
        </>
      )}
      {feedback && (
        <p
          className={`settings-feedback settings-feedback--${feedback.tone}`}
          role={feedback.tone === "error" ? "alert" : "status"}
        >
          {feedback.message}
        </p>
      )}
    </div>
  );
}

function StartupCard({ autoUpdateCheck, onAutoUpdateCheck, availableUpdate }) {
  const [enabled, setEnabled] = useState(null);
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState(null);

  useEffect(() => {
    let cancelled = false;
    getAutostart()
      .then((value) => {
        if (!cancelled) setEnabled(value);
      })
      .catch(() => {
        if (!cancelled) setFeedback({ tone: "error", message: "开机启动状态读取失败。" });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const toggle = async (next) => {
    setBusy(true);
    setFeedback(null);
    try {
      setEnabled(await setAutostart(next));
    } catch (error) {
      setFeedback({ tone: "error", message: `${error}` });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="settings-card">
      <h2>启动与位置</h2>
      <p className="settings-muted">
        位置会被记住，下次启动时恢复；超出屏幕范围时自动居中。
      </p>
      {enabled === null ? (
        <p className="settings-muted">浏览器演示模式：仅桌面应用可配置开机启动。</p>
      ) : (
        <div className="settings-directory-row">
          <button
            type="button"
            className={`ledger-button ${enabled ? "ledger-button--secondary" : "ledger-button--primary"}`}
            disabled={busy}
            onClick={() => toggle(!enabled)}
          >
            {enabled ? "关闭开机启动" : "开机时自动启动"}
          </button>
        </div>
      )}
      {feedback && (
        <p className={`settings-feedback settings-feedback--${feedback.tone}`} role="alert">
          {feedback.message}
        </p>
      )}
      <UpdateBlock
        autoCheck={autoUpdateCheck}
        onAutoCheckChange={onAutoUpdateCheck}
        availableUpdate={availableUpdate}
      />
    </div>
  );
}

// 检查每天自动做一次（可关，关掉后回到纯手动）；下载安装永远由用户点击触发。
function UpdateBlock({ autoCheck, onAutoCheckChange, availableUpdate }) {
  const [state, setState] = useState(() =>
    availableUpdate ? { status: "available", ...availableUpdate } : { status: "idle" },
  );
  // 自动检查在小组件形态就可能发现新版；进设置页时直接呈现，不用再点一次。
  // 后来的自动检查发现更新的版本时要顶掉手上这一份：否则一直提示第一次发现
  // 的那个版本，中间发布的都要等装完它才看得见。
  useEffect(() => {
    if (!availableUpdate) return;
    setState((current) => {
      // 正在检查或下载时不换手上这一份，免得按钮和进度对不上。
      if (current.status === "checking" || current.status === "installing") return current;
      if (current.status === "available" && current.version === availableUpdate.version) {
        return current;
      }
      return { status: "available", ...availableUpdate };
    });
  }, [availableUpdate]);

  const check = async () => {
    setState({ status: "checking" });
    try {
      const found = await checkForUpdate();
      setState(found ? { status: "available", ...found } : { status: "current" });
    } catch (error) {
      setState({ status: "error", message: `${error}` });
    }
  };

  const install = async () => {
    setState((current) => ({ ...current, status: "installing", percent: null }));
    try {
      await installUpdate(state.update, (percent) =>
        setState((current) => ({ ...current, percent })));
    } catch (error) {
      setState({ status: "error", message: `${error}` });
    }
  };

  if (!isDesktop()) return null;

  return (
    <div className="settings-subsection">
      <h3>更新</h3>
      <p className="settings-muted">
        当前版本 {__APP_VERSION__}。自动检查开启后，启动时检查一次，持续运行时每天检查一次；
        下载与安装由你确认，更新包经签名校验。
      </p>
      <label className="update-autocheck">
        <input
          type="checkbox"
          checked={autoCheck}
          onChange={(event) => onAutoCheckChange(event.target.checked)}
        />
        <span>自动检查更新（每天一次）</span>
      </label>
      <div className="settings-directory-row">
        <button
          type="button"
          className="ledger-button"
          disabled={state.status === "checking" || state.status === "installing"}
          onClick={state.status === "available" ? install : check}
        >
          {state.status === "checking"
            ? "检查中…"
            : state.status === "installing"
              ? `下载中${state.percent == null ? "" : ` ${state.percent}%`}…`
              : state.status === "available"
                ? `更新到 ${state.version}`
                : "检查更新"}
        </button>
        {/* 有新版待装时主按钮变成"更新到 X"，没有这一个就再也没法重新检查：
            关掉自动检查的人只能先把旧版装掉，才看得到后面发布的版本。 */}
        {state.status === "available" && (
          <button type="button" className="ledger-button ledger-button--secondary" onClick={check}>
            重新检查
          </button>
        )}
      </div>
      {state.status === "current" && (
        <p className="settings-feedback settings-feedback--success" role="status">
          已是最新版本。
        </p>
      )}
      {state.status === "available" && state.notes && (
        <p className="settings-muted">{state.notes}</p>
      )}
      {state.status === "error" && (
        <p className="settings-feedback settings-feedback--error" role="alert">
          {state.message}
        </p>
      )}
    </div>
  );
}

const REPO_URL = "https://github.com/keros68/metrik";
const AUTHOR_EMAIL = "keros68@gmail.com";

// 桌面端交给系统浏览器/邮件客户端（opener 插件，capability 限定了这两个地址）；
// 浏览器演示模式退化为普通链接。
function AboutCard() {
  const openExternal = async (event, url) => {
    if (!isDesktop()) return;
    event.preventDefault();
    const { openUrl } = await import("@tauri-apps/plugin-opener");
    await openUrl(url).catch((error) => console.warn("open external link failed:", error));
  };
  return (
    <div className="settings-card settings-about">
      <h2>关于</h2>
      <p className="settings-muted">Metrik {__APP_VERSION__}</p>
      <p className="settings-muted">
        作者：keros68（
        <a href={`mailto:${AUTHOR_EMAIL}`} onClick={(event) => openExternal(event, `mailto:${AUTHOR_EMAIL}`)}>
          {AUTHOR_EMAIL}
        </a>
        ）
      </p>
      <p className="settings-muted">
        项目仓库：
        <a href={REPO_URL} onClick={(event) => openExternal(event, REPO_URL)}>
          github.com/keros68/metrik
        </a>
      </p>
      <p className="settings-muted">许可证：AGPL-3.0-or-later</p>
    </div>
  );
}

const THEME_OPTIONS = [
  { id: "auto", label: "自动" },
  { id: "light", label: "亮色" },
  { id: "dark", label: "暗色" },
];

// 悬浮形态的窗口圆角，物理像素。10 收到 8：贴着壁纸看，10 的弧偏圆，靠近
// macOS 大部件而不是任务栏那一排小图标；8 更贴合胶囊的高度。
const GLASS_RADIUS_PX = 8;

const GLASS_TINT_OPTIONS = [
  { id: "dark", label: "深色" },
  { id: "light", label: "浅色" },
  { id: "clear", label: "透明" },
];

function normalizeGlassTint(value) {
  return GLASS_TINT_OPTIONS.some((option) => option.id === value) ? value : "dark";
}

// 透明档的文字颜色。桌面挂件常见的两种风格：深色字配白霜（Pogget 一路），
// 或白色字直接压在壁纸上（Rainmeter 一路）。两者需要的底完全相反，所以
// 选文字颜色实际上也在选罩层，见 glassAppearance.js。
const GLASS_INK_OPTIONS = [
  { id: "dark", label: "深色字" },
  { id: "light", label: "白色字" },
];

const PINNED_HOVER_OPTIONS = [
  { id: "fade", label: "降低透明度" },
  { id: "hide", label: "完全隐藏" },
];
// X11 主通道不依赖透明窗口继续接收 DOM 事件，因此隐藏可以真正降到 0；
// 无全局坐标的本地回落会在 CSS 中保留 0.1% alpha 以维持命中区域。
const PINNED_HIDDEN_OPACITY = 0;

function normalizeGlassInk(value) {
  return GLASS_INK_OPTIONS.some((option) => option.id === value) ? value : "dark";
}

function normalizePinnedHoverMode(value) {
  return PINNED_HOVER_OPTIONS.some((option) => option.id === value) ? value : "fade";
}

function SliderRow({ label, hint, min, max, step, percent, ariaLabel, onChange }) {
  return (
    <div className="settings-subsection">
      <h3>{label}</h3>
      <p className="settings-muted">{hint}</p>
      <div className="glass-slider-row">
        <input
          type="range"
          min={min}
          max={max}
          step={step}
          value={percent}
          aria-label={ariaLabel}
          onChange={(event) => onChange(Number(event.target.value) / 100)}
        />
        <em>{percent}%</em>
      </div>
    </div>
  );
}

function AppearanceCard({ theme, onThemeChange, glassAlpha, onGlassAlpha, glassTint, onGlassTint, glassInk, onGlassInk, uiScale, onUiScale, stripScale, onStripScale, pinned, onPinnedChange, pinnedHoverMode, onPinnedHoverMode, pinnedHoverOpacity, onPinnedHoverOpacity }) {
  return (
    <div className="settings-card">
      <h2>外观与缩放</h2>
      <p className="settings-muted">
        大窗口的明暗主题，“自动”跟随系统；小插件不受影响。
      </p>
      <div className="theme-toggle" role="group" aria-label="完整视图主题">
        {THEME_OPTIONS.map((option) => (
          <button
            key={option.id}
            type="button"
            className={theme === option.id ? "is-selected" : ""}
            aria-pressed={theme === option.id}
            onClick={() => onThemeChange(option.id)}
          >
            {option.label}
          </button>
        ))}
      </div>
      {/* macOS 面板材质跟随系统 vibrancy，不提供组件外观选项；
          深/浅配色用于 Windows 与 Linux 的卡片和胶囊。 */}
      {!IS_MAC && (
        <div className="settings-subsection">
          <h3>组件外观</h3>
          <p className="settings-muted">
            深色是 HUD 玻璃；浅色是透亮白磨砂；透明会直接透出桌面与后方窗口，并叠加轻霜和边缘高光。
          </p>
          <div className="theme-toggle" role="group" aria-label="组件外观">
            {GLASS_TINT_OPTIONS.map((option) => (
              <button
                key={option.id}
                type="button"
                className={glassTint === option.id ? "is-selected" : ""}
                aria-pressed={glassTint === option.id}
                onClick={() => onGlassTint(option.id)}
              >
                {option.label}
              </button>
            ))}
          </div>
        </div>
      )}
      {!IS_MAC && glassTint === "clear" && (
        <div className="settings-subsection">
          <h3>透明档文字</h3>
          <p className="settings-muted">
            深色字配白霜，在浅色壁纸上更清晰；白色字配一层薄暗罩，接近桌面挂件常见的风格。
          </p>
          <div className="theme-toggle" role="group" aria-label="透明档文字">
            {GLASS_INK_OPTIONS.map((option) => (
              <button
                key={option.id}
                type="button"
                className={glassInk === option.id ? "is-selected" : ""}
                aria-pressed={glassInk === option.id}
                onClick={() => onGlassInk(option.id)}
              >
                {option.label}
              </button>
            ))}
          </div>
        </div>
      )}
      <SliderRow
        label="玻璃浓度"
        hint="同时作用于卡片和胶囊；越低越通透，越高越厚实。"
        min={5}
        max={96}
        step={2}
        percent={Math.round(glassAlpha * 100)}
        ariaLabel="玻璃浓度百分比"
        onChange={onGlassAlpha}
      />
      {/* mac 的菜单栏面板是系统 UI 的一部分，尺寸固定不提供缩放；
          缩放只针对 Windows 的桌面小插件与胶囊条。 */}
      {!IS_MAC && (
        <SliderRow
          label="卡片缩放"
          hint="仅调整卡片尺寸；滑杆改动下次进入时生效。"
          min={UI_SCALE_RANGE.min * 100}
          max={UI_SCALE_RANGE.max * 100}
          step={5}
          percent={Math.round(uiScale * 100)}
          ariaLabel="卡片缩放百分比"
          onChange={onUiScale}
        />
      )}
      {!IS_MAC && (
        <SliderRow
          label="胶囊缩放"
          hint="仅调整胶囊尺寸，与卡片互不影响；下次进入时生效。"
          min={UI_SCALE_RANGE.min * 100}
          max={UI_SCALE_RANGE.max * 100}
          step={5}
          percent={Math.round(stripScale * 100)}
          ariaLabel="胶囊缩放百分比"
          onChange={onStripScale}
        />
      )}
      {IS_LINUX && (
        <div className="settings-subsection">
          <h3>置顶展示</h3>
          <p className="settings-muted">
            置顶后卡片和胶囊的全部控件、拖动与点击都会失活；只能回到此设置取消。
          </p>
          <div className="theme-toggle" role="group" aria-label="置顶展示模式">
            <button
              type="button"
              className={!pinned ? "is-selected" : ""}
              aria-pressed={!pinned}
              onClick={() => onPinnedChange(false)}
            >
              正常交互
            </button>
            <button
              type="button"
              className={pinned ? "is-selected" : ""}
              aria-pressed={pinned}
              onClick={() => onPinnedChange(true)}
            >
              置顶只读
            </button>
          </div>
        </div>
      )}
      {IS_LINUX && (
        <div className="settings-subsection">
          <h3>置顶悬停行为</h3>
          <p className="settings-muted">
            鼠标进入置顶卡片或胶囊时立即生效，移开后立即恢复。
          </p>
          <div className="theme-toggle" role="group" aria-label="置顶悬停行为">
            {PINNED_HOVER_OPTIONS.map((option) => (
              <button
                key={option.id}
                type="button"
                className={pinnedHoverMode === option.id ? "is-selected" : ""}
                aria-pressed={pinnedHoverMode === option.id}
                onClick={() => onPinnedHoverMode(option.id)}
              >
                {option.label}
              </button>
            ))}
          </div>
        </div>
      )}
      {IS_LINUX && pinnedHoverMode === "fade" && (
        <SliderRow
          label="悬停不透明度"
          hint="数值越低，鼠标经过时越接近完全透明。"
          min={5}
          max={90}
          step={5}
          percent={Math.round(pinnedHoverOpacity * 100)}
          ariaLabel="置顶悬停不透明度百分比"
          onChange={onPinnedHoverOpacity}
        />
      )}
    </div>
  );
}

function NativeMacWidgetCard() {
  if (!IS_MAC) return null;
  return (
    <div className="settings-card desktop-widget-setting">
      <div>
        <span className="desktop-widget-setting-kicker">macOS</span>
        <h2>桌面小组件</h2>
        <p className="settings-muted">
          在桌面空白处右键“编辑小组件”，搜索 Metrik 后添加。透明材质、圆角和摆放由 macOS 管理。
        </p>
      </div>
      <span className="desktop-widget-native-badge">系统原生</span>
    </div>
  );
}

function AgentListColumn({ title, hint, agents, detected, onToggle, onMove }) {
  // 已选的按显示顺序排前面，未选的按默认顺序垫后。
  const rows = agentIdsInDisplayOrder(agents);
  // 勾选过的一律留在上面：检测只用于分组，不能把用户自己选的挪进折叠区。
  // detected 为 null 表示这次拿不到检测结果（加载中、演示数据、旧后端），
  // 此时不折叠——宁可多列几行，也不能因为不知道就把 Agent 藏起来。
  const present = detected
    ? rows.filter((agentId) => agents.includes(agentId) || detected.has(agentId))
    : rows;
  const missing = detected ? rows.filter((agentId) => !present.includes(agentId)) : [];
  const row = (agentId) => {
    const index = agents.indexOf(agentId);
    const checked = index >= 0;
    return (
      <li key={agentId}>
        <label>
          <input
            type="checkbox"
            checked={checked}
            disabled={checked && agents.length === 1}
            onChange={() => onToggle(agentId)}
          />
          <AgentMark agentId={agentId} />
          <span>{AGENT_META[agentId].label}</span>
        </label>
        {checked && (
          <button
            type="button"
            className="settings-agent-move"
            onClick={() => onMove(agentId)}
            disabled={index === 0}
            aria-label={`将 ${AGENT_META[agentId].label} 上移`}
            title="上移"
          >
            ↑
          </button>
        )}
      </li>
    );
  };
  return (
    <div className="settings-agent-column">
      <h3>{title}</h3>
      <p className="settings-muted">{hint}</p>
      <ul className="settings-agent-toggle">{present.map(row)}</ul>
      {missing.length > 0 && (
        <details className="settings-agent-missing">
          <summary>本机未检测到（{missing.length}）</summary>
          <ul className="settings-agent-toggle">{missing.map(row)}</ul>
        </details>
      )}
    </div>
  );
}

function AgentsDisplayCard({ widgetAgents, onToggleWidgetAgent, onMoveWidgetAgent, stripAgents, onToggleStripAgent, onMoveStripAgent, detectedAgents, trayBadgeEnabled, onToggleTrayBadge }) {
  return (
    <div className="settings-card">
      <h2>显示的 Agent</h2>
      <p className="settings-muted">
        勾选即展示（至少保留一个），顺序即显示顺序，↑ 上移。
      </p>
      <div className="settings-agent-columns">
        <AgentListColumn
          title={IS_MAC ? "菜单栏、小组件与侧栏" : "小组件与侧栏"}
          hint="完整视图侧栏还会自动加入本周期内有用量的 Agent。"
          agents={widgetAgents}
          detected={detectedAgents}
          onToggle={onToggleWidgetAgent}
          onMove={onMoveWidgetAgent}
        />
        {!IS_MAC && (
          <AgentListColumn
            title="胶囊条"
            hint={'无配额来源的以 "--" 占格。'}
            agents={stripAgents}
            detected={detectedAgents}
            onToggle={onToggleStripAgent}
            onMove={onMoveStripAgent}
          />
        )}
      </div>
      {/* Windows 专属：托盘图标换成数字。取的正是上面列表最上方的 Agent，
          与菜单栏状态项同一套数据，只是托盘只放得下一个数字。 */}
      {IS_WINDOWS && (
        <div className="settings-subsection">
          <h3>任务栏图标</h3>
          <p className="settings-muted">
            开启后，任务栏右下角的 Metrik 图标改为显示列表最上方 Agent 的剩余百分比；
            无可靠额度时显示 --，窗口隐藏后数字仍每 5 分钟更新一次。
          </p>
          <label className="settings-check">
            <input
              type="checkbox"
              checked={trayBadgeEnabled}
              onChange={(event) => onToggleTrayBadge(event.target.checked)}
            />
            <span>图标改为显示余量数字</span>
          </label>
        </div>
      )}
    </div>
  );
}

function QoderQuotaCard({ onSnapshotRefresh }) {
  const [status, setStatus] = useState(null);
  const [cookieInput, setCookieInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState(null);

  useEffect(() => {
    let cancelled = false;
    getQoderCookieStatus()
      .then((value) => {
        if (!cancelled) setStatus(value);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  const apply = async (cookie) => {
    setBusy(true);
    setFeedback(null);
    try {
      const next = await configureQoderCookie(cookie);
      setStatus(next);
      setCookieInput("");
      // 验证失败也已保存：如实转述后端的结果，不粉饰。
      setFeedback({
        tone: next.message?.includes("失败") ? "error" : "success",
        message: next.message || "已更新。",
      });
      if (cookie && !next.message?.includes("失败")) onSnapshotRefresh();
    } catch (error) {
      setFeedback({ tone: "error", message: `操作失败：${error}` });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="settings-card">
      <h2>Qoder 官方额度</h2>
      <p className="settings-muted">
        Qoder、QoderWork 与 Qoder CLI 共用账户级 Credits；本地客户端不提供可被可靠解析的 token 用量，只能读取官网 Credits 额度，需要提供一次
        登录 cookie。cookie 仅明文保存在本机（不入账本、不进同步导出），可随时清除。
      </p>
      <details className="settings-guide">
        <summary>如何获取 cookie</summary>
        <ol>
          <li>浏览器登录 qoder.com.cn（国际版 qoder.com），进入「用量明细」页；</li>
          <li>按 F12 打开开发者工具 → 网络（Network）标签，点击过滤器中的 Fetch/XHR；</li>
          <li>右键列表中任意 qoder 域名的请求（如 big_model_credits）→ 复制 →
            「复制请求标头」或「以 cURL 格式复制」，将整段粘贴到下方，系统会自动提取其中的 Cookie。</li>
        </ol>
      </details>
      {status?.demo ? (
        <p className="settings-muted">浏览器演示模式：仅桌面应用可配置。</p>
      ) : (
        <>
          <div className="settings-directory-row">
            <input
              type="password"
              value={cookieInput}
              placeholder="粘贴 Cookie 值 / 整段请求标头 / cURL 命令"
              spellCheck={false}
              disabled={busy}
              aria-label="Qoder cookie"
              onChange={(event) => setCookieInput(event.target.value)}
            />
            <button
              type="button"
              className="ledger-button ledger-button--primary"
              disabled={busy || !cookieInput.trim()}
              onClick={() => apply(cookieInput.trim())}
            >
              保存并验证
            </button>
          </div>
          {status?.source === "file" && (
            <button
              type="button"
              className="ledger-button ledger-button--secondary"
              disabled={busy}
              onClick={() => apply(null)}
            >
              清除已保存的 cookie
            </button>
          )}
          {feedback && (
            <p
              className={`settings-feedback settings-feedback--${feedback.tone}`}
              role={feedback.tone === "error" ? "alert" : "status"}
            >
              {feedback.message}
            </p>
          )}
          <dl className="settings-status">
            <div>
              <dt>状态</dt>
              <dd>
                {status == null
                  ? "读取中…"
                  : status.configured
                    ? status.source === "env"
                      ? "已配置（环境变量）"
                      : "已配置（本机保存）"
                    : "未配置 · 配额卡将显示不可用"}
              </dd>
            </div>
          </dl>
        </>
      )}
    </div>
  );
}

// 设置分三类子页：每页 2 张卡，高度相近。Agent 列表会随支持的 Agent 变多而
// 变高，与短卡混排时会把整排撑出大片空白——分页正是为了让它只影响自己那一页。
const SETTINGS_TABS = [
  {
    id: "appearance",
    label: "外观与缩放",
    title: "外观与缩放",
    blurb: "完整视图的明暗主题，卡片与胶囊的组件外观、玻璃浓度及独立缩放。",
  },
  {
    id: "display",
    label: "Agent 选择",
    title: "小组件展示的 Agent",
    blurb: "选择小组件与胶囊条展示哪些 Agent、以什么顺序。",
  },
  {
    id: "sources",
    label: "数据来源",
    title: "官方额度来源",
    blurb: "配置各 Agent 的官方配额读取方式。官方额度、本地解析用量与估算成本是三类不同事实，界面上始终分开呈现。",
  },
  {
    id: "sync",
    label: "同步与更新",
    title: "多设备同步与更新",
    blurb: "多台电脑指向同一个共享文件夹即可互相合并统计；导出只含事件标识、Agent、时间与 token 数，不含对话内容或凭据。",
  },
];

function SettingsSection({ onSnapshotRefresh, widgetAgents, onToggleWidgetAgent, onMoveWidgetAgent, stripAgents, onToggleStripAgent, onMoveStripAgent, detectedAgents, trayBadgeEnabled, onToggleTrayBadge, glassAlpha, onGlassAlpha, glassTint, onGlassTint, glassInk, onGlassInk, uiScale, onUiScale, stripScale, onStripScale, pinned, onPinnedChange, pinnedHoverMode, onPinnedHoverMode, pinnedHoverOpacity, onPinnedHoverOpacity, theme, onThemeChange, autoUpdateCheck, onAutoUpdateCheck, availableUpdate }) {
  const [settings, setSettings] = useState(null);
  const [directoryInput, setDirectoryInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState(null);
  const [removingDeviceId, setRemovingDeviceId] = useState(null);

  useEffect(() => {
    let cancelled = false;
    getSyncSettings()
      .then((value) => {
        if (cancelled) return;
        setSettings(value);
        setDirectoryInput(value.directory || "");
      })
      .catch(() => {
        if (!cancelled) setFeedback({ tone: "error", message: "同步设置读取失败，请稍后重试。" });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const applySync = async (directory) => {
    setBusy(true);
    setFeedback(null);
    try {
      const next = await configureSync(directory);
      setSettings(next);
      setDirectoryInput(next.directory || "");
      setFeedback({
        tone: "success",
        message: directory ? "同步已开启，本机统计事件已导出。" : "同步已关闭，已清除合并的远端统计。",
      });
      onSnapshotRefresh();
    } catch (error) {
      setFeedback({ tone: "error", message: `未能更新同步设置：${error}` });
    } finally {
      setBusy(false);
    }
  };

  const removeDevice = async (deviceId) => {
    setBusy(true);
    setFeedback(null);
    try {
      const next = await removeSyncDevice(deviceId);
      setSettings(next);
      setFeedback({ tone: "success", message: "设备已删除，已清除它的同步事件与导出文件。" });
      onSnapshotRefresh();
    } catch (error) {
      setFeedback({ tone: "error", message: `未能删除设备：${error}` });
    } finally {
      setBusy(false);
      setRemovingDeviceId(null);
    }
  };

  const [tab, setTab] = useState("appearance");
  const activeTab = SETTINGS_TABS.find((item) => item.id === tab) || SETTINGS_TABS[0];

  return (
    <main className="settings-section" aria-labelledby="settings-title">
      <header className="settings-header">
        <h1 id="settings-title">设置</h1>
        <p><strong>{activeTab.title}</strong> · {activeTab.blurb}</p>
      </header>

      <div className="settings-tabs" role="tablist" aria-label="设置分类">
        {SETTINGS_TABS.map((item) => (
          <button
            key={item.id}
            type="button"
            role="tab"
            aria-selected={item.id === activeTab.id}
            className={item.id === activeTab.id ? "is-selected" : ""}
            onClick={() => setTab(item.id)}
          >
            {item.label}
          </button>
        ))}
      </div>

      {settings?.demo && activeTab.id === "sync" && (
        <p className="settings-demo-note">浏览器演示模式：同步配置仅在桌面应用中可用。</p>
      )}

      <div className="settings-grid">
        {activeTab.id === "appearance" && (
          <>
            <AppearanceCard
              theme={theme}
              onThemeChange={onThemeChange}
              glassAlpha={glassAlpha}
              onGlassAlpha={onGlassAlpha}
              glassTint={glassTint}
              onGlassTint={onGlassTint}
              glassInk={glassInk}
              onGlassInk={onGlassInk}
              uiScale={uiScale}
              onUiScale={onUiScale}
              stripScale={stripScale}
              onStripScale={onStripScale}
              pinned={pinned}
              onPinnedChange={onPinnedChange}
              pinnedHoverMode={pinnedHoverMode}
              onPinnedHoverMode={onPinnedHoverMode}
              pinnedHoverOpacity={pinnedHoverOpacity}
              onPinnedHoverOpacity={onPinnedHoverOpacity}
            />
            <NativeMacWidgetCard />
          </>
        )}

        {activeTab.id === "display" && (
          <AgentsDisplayCard
            widgetAgents={widgetAgents}
            onToggleWidgetAgent={onToggleWidgetAgent}
            onMoveWidgetAgent={onMoveWidgetAgent}
            stripAgents={stripAgents}
            onToggleStripAgent={onToggleStripAgent}
            onMoveStripAgent={onMoveStripAgent}
            detectedAgents={detectedAgents}
            trayBadgeEnabled={trayBadgeEnabled}
            onToggleTrayBadge={onToggleTrayBadge}
          />
        )}
        {activeTab.id === "sources" && (
          <>
            <ClaudeHookCard onSnapshotRefresh={onSnapshotRefresh} />
            <QoderQuotaCard onSnapshotRefresh={onSnapshotRefresh} />
          </>
        )}

        {activeTab.id === "sync" && (
          <>
            <div className="settings-card">
              <label htmlFor="sync-directory">同步文件夹（绝对路径）</label>
              <div className="settings-directory-row">
                <input
                  id="sync-directory"
                  type="text"
                  value={directoryInput}
                  placeholder="例如 D:\Nutstore\metrik-sync"
                  spellCheck={false}
                  disabled={busy || settings?.demo}
                  onChange={(event) => setDirectoryInput(event.target.value)}
                />
                <button
                  type="button"
                  className="ledger-button ledger-button--primary"
                  disabled={busy || settings?.demo || !directoryInput.trim()}
                  onClick={() => applySync(directoryInput.trim())}
                >
                  {settings?.enabled ? "更新目录" : "开启同步"}
                </button>
                {settings?.enabled && (
                  <button
                    type="button"
                    className="ledger-button ledger-button--secondary"
                    disabled={busy || settings?.demo}
                    onClick={() => applySync(null)}
                  >
                    关闭同步
                  </button>
                )}
              </div>

              {feedback && (
                <p
                  className={`settings-feedback settings-feedback--${feedback.tone}`}
                  role={feedback.tone === "error" ? "alert" : "status"}
                >
                  {feedback.message}
                </p>
              )}

              {/* 读取未回来时这张卡片是空的，数据一到就整块长出来，看着像界面
                  抖了一下。先占住位置并说明在读什么。 */}
              {!settings && !feedback && (
                <p className="settings-muted" role="status">读取同步设置…</p>
              )}

              {settings && !settings.demo && (
                <dl className="settings-status">
                  <div>
                    <dt>本机设备</dt>
                    <dd>{settings.deviceLabel} · {settings.deviceId}</dd>
                  </div>
                  <div>
                    <dt>上次同步</dt>
                    <dd>{settings.enabled ? formatSyncTime(settings.lastExportMs) : "同步未开启"}</dd>
                  </div>
                  {settings.lastError && (
                    <div>
                      <dt>同步告警</dt>
                      <dd className="settings-error-text">{settings.lastError}</dd>
                    </div>
                  )}
                </dl>
              )}

              {settings?.enabled && (
                <div className="settings-subsection">
                  <h3>已发现的设备</h3>
                  {settings.devices.length === 0 ? (
                    <p className="settings-muted">尚未发现其他设备的导出文件。其他电脑指向同一文件夹后即会显示。</p>
                  ) : (
                    <ul className="settings-device-list">
                      {settings.devices.map((device) => (
                        <li key={device.id}>
                          <div className="settings-device-head">
                            <div className="settings-device-info">
                              <strong>{device.label}</strong>
                              <span>{device.id}</span>
                              <small>{device.events} 条事件 · 导出于 {formatSyncTime(device.exportedAtMs)}</small>
                            </div>
                            {removingDeviceId !== device.id && (
                              <button
                                type="button"
                                className="ledger-button ledger-button--secondary settings-device-remove"
                                disabled={busy || settings?.demo}
                                onClick={() => setRemovingDeviceId(device.id)}
                              >
                                <Trash size={15} weight="light" aria-hidden="true" />
                                删除
                              </button>
                            )}
                          </div>
                          {removingDeviceId === device.id && (
                            <div className="ledger-confirmation" role="group" aria-labelledby={`device-confirm-title-${device.id}`}>
                              <strong id={`device-confirm-title-${device.id}`}>删除该设备？</strong>
                              <p>将移除它的同步事件与共享文件夹中的导出文件。若该设备仍在线，会在下次同步后重新出现。</p>
                              <div className="ledger-confirm-actions">
                                <button
                                  type="button"
                                  className="ledger-button ledger-button--secondary"
                                  disabled={busy}
                                  onClick={() => setRemovingDeviceId(null)}
                                >
                                  取消
                                </button>
                                <button
                                  type="button"
                                  className="ledger-button ledger-button--primary"
                                  disabled={busy}
                                  onClick={() => removeDevice(device.id)}
                                >
                                  {busy ? "正在删除…" : "确认删除"}
                                </button>
                              </div>
                            </div>
                          )}
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
              )}
            </div>
            <StartupCard
              autoUpdateCheck={autoUpdateCheck}
              onAutoUpdateCheck={onAutoUpdateCheck}
              availableUpdate={availableUpdate}
            />
            <AboutCard />
          </>
        )}
      </div>
    </main>
  );
}

function sessionDayLabel(ms) {
  const date = new Date(ms);
  const today = new Date();
  const startOfDay = (d) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const diffDays = Math.round((startOfDay(today) - startOfDay(date)) / 86_400_000);
  if (diffDays === 0) return "今日";
  if (diffDays === 1) return "昨日";
  return date.toLocaleDateString("zh-CN", { month: "long", day: "numeric" });
}

function csvEscape(value) {
  const text = String(value ?? "");
  return /[",\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

// 导出只含账本本就存储的统计字段，与隐私边界一致。
function buildSessionsCsv(sessions) {
  const header = ["date", "start", "end", "agent", "model", "project", "project_path", "tokens", "input_uncached", "cache_read", "cache_write", "output", "estimated_usd", "events", "session_id"];
  const rows = sessions.map((session) => [
    new Date(session.endMs).toLocaleDateString("sv-SE"),
    new Date(session.startMs).toLocaleTimeString("zh-CN", { hour12: false }),
    new Date(session.endMs).toLocaleTimeString("zh-CN", { hour12: false }),
    session.agent,
    session.model || "",
    session.projectLabel || "",
    session.project || "",
    session.tokens,
    session.inputUncached,
    session.cacheRead,
    session.cacheWrite,
    session.output,
    session.usd == null ? "" : session.usd.toFixed(4),
    session.eventCount,
    session.sessionId,
  ]);
  // 带 BOM，Excel 才能正确识别 UTF-8。
  return `﻿${[header, ...rows].map((row) => row.map(csvEscape).join(",")).join("\r\n")}`;
}

async function saveCsv(fileName, csv) {
  // 桌面端：blob 下载在 Tauri WebView 里不生效，改走后端写入下载目录。
  const savedPath = await exportCsvFile(fileName, csv);
  if (savedPath) return savedPath;
  // 浏览器演示模式退回常规下载。
  const url = URL.createObjectURL(new Blob([csv], { type: "text/csv;charset=utf-8" }));
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = fileName;
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
  return null;
}

async function exportSessionsCsv(sessions) {
  return saveCsv(
    `metrik-sessions-${new Date().toLocaleDateString("sv-SE")}.csv`,
    buildSessionsCsv(sessions),
  );
}

// 会话行与项目行的项目名只显示目录名，完整路径放 title。
function projectLabel(path) {
  const parts = String(path).split("/").filter(Boolean);
  return parts[parts.length - 1] || path;
}

// 添加项目：登记项目根、隐藏目录，列出已有规则并可移除。
function ProjectRulesCard({ rules, busy, onAddRoot, onRemoveRoot, onRemoveHidden, onClose }) {
  const [draft, setDraft] = useState("");
  const submit = () => {
    const value = draft.trim();
    if (!value) return;
    onAddRoot(value);
    setDraft("");
  };
  return (
    <section className="rules-card" aria-label="添加项目">
      <header className="rules-card-head">
        <h2>添加项目</h2>
        <button type="button" className="rules-close" onClick={onClose} aria-label="收起添加项目">
          <X size={14} weight="bold" aria-hidden="true" />
        </button>
      </header>
      <p>
        决定哪些目录算作一个项目。默认按 git 仓库根归并，家目录、下载与系统临时目录不列为项目。
        账本始终记录事件发生时的原始目录，这里只改变展示时的归类，移除后即恢复。
      </p>
      <div className="rules-add">
        <input
          value={draft}
          placeholder="登记项目根目录，其下用量归并为一个项目"
          spellCheck={false}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => { if (event.key === "Enter") submit(); }}
          aria-label="项目根目录路径"
        />
        <button type="button" className="ledger-button" disabled={!draft.trim() || busy} onClick={submit}>
          登记
        </button>
      </div>
      {!rules ? (
        <p className="settings-muted">正在读取规则…</p>
      ) : (
        <>
          {rules.roots.length > 0 && (
            <div className="rules-group">
              <h3>项目根 · 子目录归并到这里</h3>
              <ul>
                {rules.roots.map((path) => (
                  <li key={path}>
                    <PushPinSimple size={12} weight="fill" aria-hidden="true" />
                    <span title={path}>{path}</span>
                    <button type="button" disabled={busy} onClick={() => onRemoveRoot(path)} aria-label={`移除项目根 ${path}`}>
                      <X size={12} weight="bold" aria-hidden="true" />
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}
          {rules.hidden.length > 0 && (
            <div className="rules-group">
              <h3>已隐藏 · 不作为项目展示</h3>
              <ul>
                {rules.hidden.map((path) => (
                  <li key={path}>
                    <EyeSlash size={12} weight="regular" aria-hidden="true" />
                    <span title={path}>{path}</span>
                    <button type="button" disabled={busy} onClick={() => onRemoveHidden(path)} aria-label={`取消隐藏 ${path}`}>
                      <X size={12} weight="bold" aria-hidden="true" />
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}
          {rules.roots.length === 0 && rules.hidden.length === 0 && (
            <p className="settings-muted">尚无手动归类规则。可在项目行上点击图钉或眼睛图标，或在上方登记目录。</p>
          )}
        </>
      )}
    </section>
  );
}

const PROJECT_PREVIEW_COUNT = 8;

// 项目分类色：经 CVD/对比度校验的固定顺序（styles.css 里的 --viz-1..6），
// 按周期内 token 排名前 6 依序分配，之后统一入"其他"灰。颜色跟随项目路径，
// Agent 筛选不重新分配，避免筛选后颜色跳变。
const PROJECT_COLOR_COUNT = 6;

function projectColorMap(projects) {
  const map = new Map();
  projects.slice(0, PROJECT_COLOR_COUNT).forEach((project, index) => {
    map.set(project.path, `var(--viz-${index + 1})`);
  });
  return map;
}

// 项目总表导出：与账本口径一致的统计字段。
function buildProjectsCsv(projects) {
  const header = ["project", "path", "agents", "model", "tokens", "input_uncached", "cache_read", "cache_write", "output", "estimated_usd", "sessions", "events", "last_used"];
  const rows = projects.map((project) => [
    project.label,
    project.path,
    project.agents.join(" "),
    project.model || "",
    project.tokens,
    project.inputUncached,
    project.cacheRead,
    project.cacheWrite,
    project.output,
    project.usd == null ? "" : project.usd.toFixed(4),
    project.sessionCount,
    project.eventCount,
    new Date(project.lastMs).toLocaleString("sv-SE"),
  ]);
  return `﻿${[header, ...rows].map((row) => row.map(csvEscape).join(",")).join("\r\n")}`;
}

async function exportProjectsCsv(projects) {
  return saveCsv(
    `metrik-projects-${new Date().toLocaleDateString("sv-SE")}.csv`,
    buildProjectsCsv(projects),
  );
}

// 项目占比环形图：与报告页 Agent 占比同一视觉语言；段间留白 2.5，
// 点击分段进入对应项目详情。"其他"聚合段不可点。
function ProjectShareDonut({ projects, colorByPath, onOpen }) {
  const total = projects.reduce((sum, project) => sum + project.tokens, 0) || 1;
  const top = projects.slice(0, PROJECT_COLOR_COUNT);
  const otherTokens = projects.slice(PROJECT_COLOR_COUNT).reduce((sum, project) => sum + project.tokens, 0);
  const segments = [
    ...top.map((project) => ({
      key: project.path,
      label: project.label,
      tokens: project.tokens,
      color: colorByPath.get(project.path),
      selectable: true,
    })),
    ...(otherTokens > 0
      ? [{ key: "__other", label: "其他", tokens: otherTokens, color: "var(--viz-other)", selectable: false }]
      : []),
  ];
  const radius = 74;
  const circumference = 2 * Math.PI * radius;
  let offset = 0;
  return (
    <svg className="project-donut" viewBox="0 0 200 200" role="img" aria-label="项目用量占比环形图">
      {segments.map((segment) => {
        const dash = (segment.tokens / total) * circumference;
        const rendered = (
          <circle
            key={segment.key}
            cx="100"
            cy="100"
            r={radius}
            fill="none"
            stroke={segment.color}
            strokeWidth="21"
            strokeDasharray={`${Math.max(0, dash - 2.5)} ${circumference - Math.max(0, dash - 2.5)}`}
            strokeDashoffset={-offset}
            transform="rotate(-90 100 100)"
            className={segment.selectable ? "project-donut-segment" : undefined}
            onClick={segment.selectable ? () => onOpen(segment.key) : undefined}
          >
            <title>{`${segment.label} · ${compactTokens(segment.tokens)} · ${((segment.tokens / total) * 100).toFixed(1)}%`}</title>
          </circle>
        );
        offset += dash;
        return rendered;
      })}
      {/* 两行合起来在环心居中：基线放 96/114 时墨迹只到 77.4~114（数字与
          "tokens" 都没有下伸部），视觉中心落在 95.7，整体偏高约 4px。 */}
      <text x="100" y="100" textAnchor="middle" className="donut-total">{compactTokens(total)}</text>
      <text x="100" y="118" textAnchor="middle" className="donut-caption">tokens</text>
    </svg>
  );
}

// 用量页：项目列表 → 点击进入单个项目的会话明细（层级下钻，不再同屏堆两个列表）。
// detail 为 null 时是项目列表；{ type:"project", path } 是项目详情；
// { type:"unattributed" } 是读不到目录的会话。
function UsageSection({ projectsState, sessionsState, period, onRulesChanged }) {
  const [agentFilter, setAgentFilter] = useState("all");
  const [modelFilter, setModelFilter] = useState("all");
  const [detail, setDetail] = useState(null);
  const [showAllProjects, setShowAllProjects] = useState(false);
  const [copiedId, setCopiedId] = useState(null);
  const [note, setNote] = useState(null);
  const [rulesOpen, setRulesOpen] = useState(false);
  const [rules, setRules] = useState(null);
  const [rulesBusy, setRulesBusy] = useState(false);

  useEffect(() => {
    if (rulesOpen && rules == null) {
      getProjectRules().then((loaded) => setRules(loaded.loadError ? { roots: [], hidden: [] } : loaded));
    }
  }, [rulesOpen, rules]);

  const projects = projectsState?.data;
  const sessionsData = sessionsState?.data;
  const colorByPath = useMemo(
    () => projectColorMap(projects?.projects || []),
    [projects],
  );

  // 正在看的项目因规则变更或换周期消失时，退回列表。
  useEffect(() => {
    if (detail?.type !== "project" || !projects || projects.loadError) return;
    if (!projects.projects.some((project) => project.path === detail.path)) {
      setDetail(null);
    }
  }, [detail, projects]);

  const openDetail = (next) => {
    setModelFilter("all");
    setNote(null);
    setDetail(next);
  };

  const applyRules = async (next) => {
    setRulesBusy(true);
    try {
      const saved = await setProjectRules(next);
      setRules(saved);
      onRulesChanged();
    } catch (error) {
      setNote({ text: `规则保存失败：${error}` });
    } finally {
      setRulesBusy(false);
    }
  };
  const currentRules = async () => {
    if (rules) return rules;
    const loaded = await getProjectRules();
    return loaded.loadError ? { roots: [], hidden: [] } : loaded;
  };
  const pinProject = async (path) => {
    const current = await currentRules();
    await applyRules({ ...current, roots: [...current.roots, path] });
  };
  const removeRoot = async (path) => {
    const current = await currentRules();
    await applyRules({ ...current, roots: current.roots.filter((item) => item !== path) });
  };
  const removeHidden = async (path) => {
    const current = await currentRules();
    await applyRules({ ...current, hidden: current.hidden.filter((item) => item !== path) });
  };
  // 登记与取消登记同一颗按钮：图钉状态可逆，按钮也不会中途从 DOM 消失。
  const togglePin = (project) => (
    project.pinned ? removeRoot(project.path) : pinProject(project.path)
  );
  // 隐藏会让这一行消失，就地留一个撤销入口；规则面板仍是长期的恢复位置。
  const hideProject = async (project) => {
    const current = await currentRules();
    await applyRules({ ...current, hidden: [...current.hidden, project.path] });
    setNote({
      text: `已隐藏 ${project.label}`,
      undo: () => removeHidden(project.path),
    });
  };

  const noteExport = async (task) => {
    try {
      const savedPath = await task();
      setNote({ text: savedPath ? `已导出到 ${savedPath}` : "已开始下载" });
    } catch (error) {
      setNote({ text: `导出失败：${error}` });
    }
  };
  const copySessionId = (sessionId) => {
    navigator.clipboard?.writeText(sessionId).then(() => {
      setCopiedId(sessionId);
      setTimeout(() => setCopiedId((current) => (current === sessionId ? null : current)), 1400);
    }).catch(() => {});
  };

  const loading = !projectsState || projectsState.status === "loading"
    || !sessionsState || sessionsState.status === "loading";
  if (loading) {
    return (
      <main className="usage-section" aria-busy="true">
        <header className="settings-header">
          <h1>用量</h1>
          <p>正在读取用量明细。只读取已索引的账本，不触发新的日志扫描。</p>
        </header>
      </main>
    );
  }
  if (!projects || projects.loadError || !sessionsData || sessionsData.loadError) {
    return (
      <main className="usage-section">
        <header className="settings-header">
          <h1>用量</h1>
          <p>本地账本读取失败，明细暂不可用；未以演示数据替代。请稍后重试。</p>
        </header>
      </main>
    );
  }

  const timeRange = (session) => {
    const fmt = (ms) => new Date(ms).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit", hour12: false });
    return `${fmt(session.startMs)}–${fmt(session.endMs)}`;
  };

  const sessionGroups = (sessions) => {
    const groups = [];
    sessions.forEach((session) => {
      const label = sessionDayLabel(session.endMs);
      const group = groups[groups.length - 1];
      if (group && group.label === label) group.sessions.push(session);
      else groups.push({ label, sessions: [session] });
    });
    return groups;
  };

  const renderSessionRows = (groups) => groups.map((group) => (
    <section className="session-group" key={group.label} aria-label={group.label}>
      <h3>{group.label}</h3>
      {group.sessions.map((session) => {
        const meta = AGENT_META[session.agent];
        return (
          <article className="session-row" key={`${session.agent}-${session.sessionId}`}>
            <i className="model-dot" style={{ backgroundColor: meta?.accent || "#74767a" }} aria-hidden="true" />
            <div className="session-copy">
              <strong>
                {timeRange(session)} · {meta?.label || session.agent}
                {session.model ? ` · ${session.model}` : ""}
              </strong>
              <small>
                {compactTokens(session.tokens)} tokens
                {session.usd != null ? ` · ≈${formatUsd(session.usd)}` : " · 未计价"}
                {` · ${session.eventCount} 次记录`}
                {` · 缓存读 ${session.tokens ? Math.round((session.cacheRead / session.tokens) * 100) : 0}%`}
              </small>
            </div>
            <button
              type="button"
              className={`session-id-chip ${copiedId === session.sessionId ? "session-id-chip--copied" : ""}`}
              onClick={() => copySessionId(session.sessionId)}
              title={`复制会话 ID（可用于 resume 等操作）
${session.sessionId}`}
            >
              {copiedId === session.sessionId
                ? <Check size={12} weight="bold" aria-hidden="true" />
                : <Copy size={12} weight="light" aria-hidden="true" />}
              <span>{session.sessionId.length > 14 ? `${session.sessionId.slice(0, 12)}…` : session.sessionId}</span>
            </button>
            <em>{compactTokens(session.tokens)}</em>
          </article>
        );
      })}
    </section>
  ));

  // ── 项目详情视图 ──
  if (detail) {
    const detailMeta = detail.type === "project"
      ? projects.projects.find((project) => project.path === detail.path)
      : null;
    const scoped = sessionsData.sessions.filter((session) => (
      detail.type === "project" ? session.project === detail.path : !session.project
    ));
    const models = [...new Set(scoped.map((session) => session.model).filter(Boolean))];
    const filtered = scoped.filter((session) => modelFilter === "all" || session.model === modelFilter);
    const groups = sessionGroups(filtered);
    const title = detail.type === "project" ? (detailMeta?.label || projectLabel(detail.path)) : "未归类会话";

    return (
      <main className="usage-section" aria-labelledby="usage-title">
        <header className="settings-header">
          <button type="button" className="detail-back" onClick={() => openDetail(null)}>
            <CaretLeft size={12} weight="bold" aria-hidden="true" />
            项目列表
          </button>
          <h1 id="usage-title">
            {detail.type === "project" && (
              <i
                className="project-title-dot"
                style={{ backgroundColor: colorByPath.get(detail.path) || "var(--viz-other)" }}
                aria-hidden="true"
              />
            )}
            {title}
          </h1>
          <p>
            {detail.type === "project" ? (
              <>
                {detail.path}
                {detailMeta ? ` · ${detailMeta.agents.map((id) => AGENT_META[id]?.label || id).join("、")}` : ""}
                {` · ${scoped.length} 个会话`}
                {detailMeta?.usd != null ? ` · 估算 ≈${formatUsd(detailMeta.usd)}` : ""}
              </>
            ) : (
              <>这些会话的来源不带工作目录（如 Antigravity），无法归入项目。{` ${scoped.length} 个会话。`}</>
            )}
          </p>
        </header>

        <div className="usage-toolbar">
          <select value={modelFilter} onChange={(event) => setModelFilter(event.target.value)} aria-label="按模型筛选">
            <option value="all">全部模型</option>
            {models.map((model) => <option key={model} value={model}>{model}</option>)}
          </select>
          <button
            type="button"
            className="ledger-button usage-toolbar-end"
            disabled={!filtered.length}
            onClick={() => noteExport(() => exportSessionsCsv(filtered))}
          >
            导出会话 CSV（{filtered.length}）
          </button>
        </div>

        {note && (
        <p className="usage-note" role="status">
          {note.text}
          {note.undo && (
            <button
              type="button"
              disabled={rulesBusy}
              onClick={() => { setNote(null); note.undo(); }}
            >
              撤销
            </button>
          )}
        </p>
      )}
        {groups.length === 0 && <p className="settings-muted">本周期内没有可显示的会话。</p>}
        {groups.length > 0 && <div className="report-card session-board">{renderSessionRows(groups)}</div>}
      </main>
    );
  }

  // ── 项目列表视图 ──
  const filteredProjects = projects.projects.filter((project) =>
    agentFilter === "all" || project.agents.includes(agentFilter));
  const visibleProjects = showAllProjects
    ? filteredProjects
    : filteredProjects.slice(0, PROJECT_PREVIEW_COUNT);
  const maxTokens = filteredProjects.reduce((max, project) => Math.max(max, project.tokens), 0);
  const unattributedLabel = (projects.unattributedAgents || [])
    .map((id) => AGENT_META[id]?.label || id)
    .join("、");

  return (
    <main className="usage-section" aria-labelledby="usage-title">
      <header className="settings-header">
        <h1 id="usage-title">用量</h1>
        <p>
          <strong>项目</strong> · {PERIODS.find((item) => item.id === period)?.label}内 {projects.totalProjects} 个项目、{sessionsData.totalSessions} 个会话，点击项目查看会话明细。成本为按公开 API 价格的估算，非账单。
          {sessionsData.isDemo ? " 当前为浏览器演示数据。" : ""}
        </p>
      </header>

      <div className="usage-toolbar">
        <select value={agentFilter} onChange={(event) => setAgentFilter(event.target.value)} aria-label="按 Agent 筛选">
          <option value="all">全部 Agent</option>
          {AGENT_ORDER.map((id) => <option key={id} value={id}>{AGENT_META[id].label}</option>)}
        </select>
        <button
          type="button"
          className={`ledger-button rules-toggle ${rulesOpen ? "rules-toggle--open" : ""}`}
          aria-expanded={rulesOpen}
          onClick={() => setRulesOpen((open) => !open)}
        >
          <FolderSimple size={13} weight="bold" aria-hidden="true" />
          添加项目
        </button>
        <button
          type="button"
          className="ledger-button"
          disabled={!filteredProjects.length}
          onClick={() => noteExport(() => exportProjectsCsv(filteredProjects))}
        >
          导出 CSV（{filteredProjects.length}）
        </button>
      </div>

      {note && (
        <p className="usage-note" role="status">
          {note.text}
          {note.undo && (
            <button
              type="button"
              disabled={rulesBusy}
              onClick={() => { setNote(null); note.undo(); }}
            >
              撤销
            </button>
          )}
        </p>
      )}

      {rulesOpen && (
        <ProjectRulesCard
          rules={rules}
          busy={rulesBusy}
          onAddRoot={pinProject}
          onRemoveRoot={removeRoot}
          onRemoveHidden={removeHidden}
          onClose={() => setRulesOpen(false)}
        />
      )}

      <section className="report-card project-board" aria-label="项目汇总">
        {projects.projects.length > 0 && (
          <div className="project-overview">
            <ProjectShareDonut
              projects={projects.projects}
              colorByPath={colorByPath}
              onOpen={(path) => openDetail({ type: "project", path })}
            />
            <div className="project-overview-figures">
              {/* token 总量在环心；这里放成本这另一维度，同一个数不出现两次。 */}
              {(() => {
                const priced = projects.projects.filter((project) => project.usd != null);
                if (!priced.length) {
                  return (
                    <>
                      <strong>{projects.totalProjects}</strong>
                      <span>个项目 · 未计价</span>
                    </>
                  );
                }
                return (
                  <>
                    <strong>≈{formatUsd(priced.reduce((sum, project) => sum + project.usd, 0))}</strong>
                    <span>{projects.totalProjects} 个项目 · 估算成本</span>
                  </>
                );
              })()}
              {(projects.unattributedTokens > 0 || projects.hiddenTokens > 0) && (
                <small>
                  {projects.unattributedTokens > 0 && (
                    <button type="button" onClick={() => openDetail({ type: "unattributed" })}>
                      读不到目录 {compactTokens(projects.unattributedTokens)}{unattributedLabel ? `（${unattributedLabel}）` : ""}
                    </button>
                  )}
                  {projects.hiddenTokens > 0 && (
                    // 这个数把用户隐藏的与内置排除（家目录、下载、系统临时目录）
                    // 算在一起，所以不能说"已隐藏"——用户把自己的规则删干净了，
                    // 剩下的内置部分仍会让它显示，读起来像没删掉。
                    <button type="button" onClick={() => setRulesOpen(true)}>
                      未计入项目 {compactTokens(projects.hiddenTokens)}
                    </button>
                  )}
                </small>
              )}
            </div>
          </div>
        )}
        {visibleProjects.length === 0 && (
          <p className="settings-muted">
            {projects.totalProjects === 0
              ? "本周期内没有带项目归属的用量。"
              : "当前筛选条件下没有项目。"}
          </p>
        )}
        {visibleProjects.map((project) => (
          <article
            className="project-row"
            key={project.path}
            onClick={(event) => {
              if (event.target.closest?.(".project-actions")) return;
              openDetail({ type: "project", path: project.path });
            }}
            role="button"
            tabIndex={0}
            onKeyDown={(event) => {
              if (event.key === "Enter" || event.key === " ") {
                event.preventDefault();
                openDetail({ type: "project", path: project.path });
              }
            }}
          >
            <div className="session-copy">
              <strong title={project.path}>{project.label}</strong>
              <small>
                {project.agents.map((id) => AGENT_META[id]?.label || id).join(" · ")}
                {project.model ? ` · ${modelDisplayName(project.model)}` : ""}
                {` · ${project.sessionCount} 个会话`}
                {project.usd != null ? ` · ≈${formatUsd(project.usd)}` : " · 未计价"}
              </small>
              <span className="project-bar" aria-hidden="true">
                <i
                  style={{
                    width: `${maxTokens ? Math.max(2, (project.tokens / maxTokens) * 100) : 0}%`,
                    background: colorByPath.get(project.path),
                  }}
                />
              </span>
            </div>
            {/* 操作区整体阻断冒泡：按钮若在点击途中卸载，mouseup 会落到行上误触发进入详情。 */}
            <div
              className={`project-actions ${project.pinned ? "project-actions--pinned" : ""}`}
              onClick={(event) => event.stopPropagation()}
              onMouseDown={(event) => event.stopPropagation()}
            >
              <button
                type="button"
                className={project.pinned ? "is-active" : ""}
                disabled={rulesBusy}
                aria-pressed={project.pinned}
                title={project.pinned
                  ? `取消登记：${project.path} 下的子目录恢复各自成行`
                  : `登记为项目根：${project.path} 下的子目录都归并到这一行`}
                aria-label={project.pinned
                  ? `取消登记项目根 ${project.label}`
                  : `把 ${project.label} 登记为项目根`}
                onClick={() => togglePin(project)}
              >
                <PushPinSimple size={13} weight={project.pinned ? "fill" : "regular"} aria-hidden="true" />
              </button>
              <button
                type="button"
                disabled={rulesBusy}
                title={`隐藏 ${project.path}：其用量不再作为项目展示，可在项目归类里恢复`}
                aria-label={`隐藏项目 ${project.label}`}
                onClick={() => hideProject(project)}
              >
                <EyeSlash size={13} weight="regular" aria-hidden="true" />
              </button>
            </div>
            <span className="project-open" aria-hidden="true">
              <CaretRight size={13} weight="bold" />
            </span>
            <em>{compactTokens(project.tokens)}</em>
          </article>
        ))}
        {filteredProjects.length > PROJECT_PREVIEW_COUNT && (
          <button type="button" className="project-expand" onClick={() => setShowAllProjects((value) => !value)}>
            {showAllProjects ? "收起" : `显示全部 ${filteredProjects.length} 个项目`}
          </button>
        )}
      </section>
    </main>
  );
}

function dateKey(date) {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}

// 26 周活动热力图的格子矩阵：列 = 周（周一起始），行 = 星期。
function buildHeatmapWeeks(days) {
  const tokensByDate = new Map(days.map((day) => [day.date, day.tokens]));
  const today = new Date();
  const end = new Date(today.getFullYear(), today.getMonth(), today.getDate());
  const start = new Date(end);
  start.setDate(start.getDate() - 181);
  // 对齐到周一，首列可能带上窗口外的占位格。
  const lead = (start.getDay() + 6) % 7;
  start.setDate(start.getDate() - lead);

  const weeks = [];
  const cursor = new Date(start);
  while (cursor <= end) {
    const week = [];
    for (let i = 0; i < 7; i += 1) {
      const inWindow = cursor <= end;
      week.push(
        inWindow
          ? { key: dateKey(cursor), tokens: tokensByDate.get(dateKey(cursor)) || 0, month: cursor.getMonth(), day: cursor.getDate() }
          : null,
      );
      cursor.setDate(cursor.getDate() + 1);
    }
    weeks.push(week);
  }
  return weeks;
}

// 非零值的分位数阈值 → 0–4 五档（序列色由浅到深）。
function heatLevel(tokens, thresholds) {
  if (!tokens) return 0;
  if (tokens <= thresholds[0]) return 1;
  if (tokens <= thresholds[1]) return 2;
  if (tokens <= thresholds[2]) return 3;
  return 4;
}

// 按周（周一起始）汇总每 Agent 的 token，用于趋势折线与构成。
// 以当前周为终点补零生成连续 rangeWeeks 个周：无数据的周保留为零，
// 周与周在 X 轴上等距，日期刻度才对得上。
function weeklySeries(days, rangeWeeks) {
  const byWeek = new Map();
  days.forEach((day) => {
    const date = new Date(`${day.date}T00:00:00`);
    const monday = new Date(date);
    monday.setDate(date.getDate() - ((date.getDay() + 6) % 7));
    const key = dateKey(monday);
    const bucket = byWeek.get(key) || { label: key, byAgent: {} };
    Object.entries(day.byAgent || {}).forEach(([id, value]) => {
      bucket.byAgent[id] = (bucket.byAgent[id] || 0) + Number(value || 0);
    });
    byWeek.set(key, bucket);
  });
  const currentMonday = new Date();
  currentMonday.setHours(0, 0, 0, 0);
  currentMonday.setDate(currentMonday.getDate() - ((currentMonday.getDay() + 6) % 7));
  return Array.from({ length: rangeWeeks }, (_, index) => {
    const monday = new Date(currentMonday);
    monday.setDate(monday.getDate() - (rangeWeeks - 1 - index) * 7);
    const key = dateKey(monday);
    return byWeek.get(key) || { label: key, byAgent: {} };
  });
}

// 周一日期 → "M/D" 短刻度。
function weekTickLabel(key) {
  const date = new Date(`${key}T00:00:00`);
  return `${date.getMonth() + 1}/${date.getDate()}`;
}

// 图表专用降饱和配色：品牌色直接上图会显得"纯"，
// 苹果式做法是柔和一档的同源色 + 平滑曲线 + 低透明面积。
// 六个 Agent 各占一个色相（蓝/珊瑚/紫罗兰/青/品红/琥珀），
// 任何叠加组合都可分辨——曾经 codex/kimi/antigravity 三个蓝挤在一起。
const CHART_LINE_COLORS = {
  codex: "#5586d4",
  claude: "#d98663",
  zcode: "#8b80d9",
  opencode: "#4aa392",
  kimi: "#c4719f",
  antigravity: "#d1a34e",
};

function chartColor(id) {
  return CHART_LINE_COLORS[id] || "#8a8c90";
}

// Catmull-Rom 平滑成三次贝塞尔路径。
function smoothPath(points) {
  if (points.length < 2) return "";
  let d = `M ${points[0][0].toFixed(1)},${points[0][1].toFixed(1)}`;
  for (let i = 0; i < points.length - 1; i += 1) {
    const p0 = points[Math.max(0, i - 1)];
    const p1 = points[i];
    const p2 = points[i + 1];
    const p3 = points[Math.min(points.length - 1, i + 2)];
    const c1 = [p1[0] + (p2[0] - p0[0]) / 6, p1[1] + (p2[1] - p0[1]) / 6];
    const c2 = [p2[0] - (p3[0] - p1[0]) / 6, p2[1] - (p3[1] - p1[1]) / 6];
    d += ` C ${c1[0].toFixed(1)},${c1[1].toFixed(1)} ${c2[0].toFixed(1)},${c2[1].toFixed(1)} ${p2[0].toFixed(1)},${p2[1].toFixed(1)}`;
  }
  return d;
}

function ReportTrendChart({ weeks }) {
  // viewBox 宽度跟随容器实测宽度，缩放系数恒为 1：图表始终占满整行，
  // 刻度字号也不再随窗口忽大忽小。固定 620 时，宽窗口下按高度约束缩放，
  // 只画得出 720px，左右各空几百像素。
  const hostRef = useRef(null);
  const [width, setWidth] = useState(620);
  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!host) return undefined;
    const apply = (value) => setWidth(Math.max(420, Math.round(value)));
    apply(host.clientWidth);
    const observer = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect;
      if (rect) apply(rect.width);
    });
    observer.observe(host);
    return () => observer.disconnect();
  }, []);

  const agents = AGENT_ORDER.filter((id) => weeks.some((week) => (week.byAgent[id] || 0) > 0));
  if (!agents.length) {
    return <p className="settings-muted" ref={hostRef}>所选时间段内没有已索引的用量。</p>;
  }
  const max = Math.max(1, ...weeks.flatMap((week) => agents.map((id) => week.byAgent[id] || 0)));
  const height = 210;
  const pad = { top: 12, right: 8, bottom: 22, left: 8 };
  const x = (index) => pad.left + (index / Math.max(1, weeks.length - 1)) * (width - pad.left - pad.right);
  const y = (value) => height - pad.bottom - (value / max) * (height - pad.top - pad.bottom);
  const linePoints = (id) => weeks.map((week, index) => [x(index), y(week.byAgent[id] || 0)]);
  const baseline = height - pad.bottom;
  // X 轴最多 ~6 个周刻度；Y 轴给半程和峰值两条虚线参考。
  const tickStride = Math.max(1, Math.ceil(weeks.length / 6));
  const gridValues = [max / 2, max];

  return (
    <div ref={hostRef}>
      <svg
        className="report-trend"
        viewBox={`0 0 ${width} ${height}`}
        role="img"
        aria-label={`近 ${weeks.length} 周每周 token 用量趋势，按 Agent 分色`}
      >
        <defs>
          {agents.map((id) => (
            <linearGradient key={id} id={`trend-fill-${id}`} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor={chartColor(id)} stopOpacity="0.16" />
              <stop offset="100%" stopColor={chartColor(id)} stopOpacity="0" />
            </linearGradient>
          ))}
        </defs>
        <line x1={pad.left} y1={baseline} x2={width - pad.right} y2={baseline} className="trend-axis" />
        {gridValues.map((value) => (
          <line key={value} x1={pad.left} y1={y(value)} x2={width - pad.right} y2={y(value)} className="trend-grid" />
        ))}
        {agents.map((id) => {
          const pts = linePoints(id);
          const line = smoothPath(pts);
          const area = `${line} L ${pts[pts.length - 1][0].toFixed(1)},${baseline} L ${pts[0][0].toFixed(1)},${baseline} Z`;
          return (
            <g key={id}>
              <path d={area} fill={`url(#trend-fill-${id})`} stroke="none" />
              <path d={line} fill="none" stroke={chartColor(id)} strokeWidth="2" strokeLinejoin="round" strokeLinecap="round" />
            </g>
          );
        })}
        {/* 刻度文字画在曲线之后，避免被面积填充和线盖住。 */}
        {gridValues.map((value) => (
          <text key={value} x={pad.left} y={y(value) - 4} className="trend-label">{compactTokens(value)}</text>
        ))}
        {weeks.map((week, index) =>
          index % tickStride === 0 ? (
            <text
              key={week.label}
              x={x(index)}
              y={height - 6}
              className="trend-label"
              textAnchor={index === 0 ? "start" : x(index) > width - 30 ? "end" : "middle"}
            >
              {weekTickLabel(week.label)}
            </text>
          ) : null,
        )}
      </svg>
      <div className="chart-legend chart-legend--report" aria-label="图例">
        {agents.map((id) => (
          <span key={id}><i className="legend-line" style={{ background: chartColor(id) }} />{AGENT_META[id]?.label || id}</span>
        ))}
      </div>
    </div>
  );
}

function ReportShareDonut({ agents, totalTokens, weeksCount }) {
  const rows = agents.filter((agent) => agent.tokens > 0);
  if (!rows.length) {
    return <p className="settings-muted">所选时间段内没有已索引的用量。</p>;
  }
  const total = rows.reduce((sum, agent) => sum + agent.tokens, 0) || 1;
  const radius = 74;
  const circumference = 2 * Math.PI * radius;
  let offset = 0;
  return (
    <div className="report-donut">
      <svg viewBox="0 0 200 200" role="img" aria-label={`近 ${weeksCount} 周内各 Agent 用量占比环形图`}>
        {rows.map((agent) => {
          const fraction = agent.tokens / total;
          const dash = fraction * circumference;
          const segment = (
            <circle
              key={agent.id}
              cx="100"
              cy="100"
              r={radius}
              fill="none"
              stroke={AGENT_META[agent.id]?.accent || "#74767a"}
              strokeWidth="21"
              strokeDasharray={`${Math.max(0, dash - 2.5)} ${circumference - Math.max(0, dash - 2.5)}`}
              strokeDashoffset={-offset}
              transform="rotate(-90 100 100)"
            />
          );
          offset += dash;
          return segment;
        })}
        {/* 基线与项目环形保持一致，见 ProjectShareDonut 的说明。 */}
        <text x="100" y="100" textAnchor="middle" className="donut-total">{compactTokens(totalTokens)}</text>
        <text x="100" y="118" textAnchor="middle" className="donut-caption">{`tokens · 近 ${weeksCount} 周`}</text>
      </svg>
      <ul className="comp-legend">
        {rows.map((agent) => (
          <li key={agent.id}>
            <i style={{ backgroundColor: AGENT_META[agent.id]?.accent || "#74767a", borderRadius: "50%" }} aria-hidden="true" />
            <span>{AGENT_META[agent.id]?.label || agent.id}</span>
            <em>{compactTokens(agent.tokens)} · {((agent.tokens / total) * 100).toFixed(1)}%</em>
          </li>
        ))}
      </ul>
    </div>
  );
}

const REPORT_VIEWS = [
  { id: "heatmap", label: "热力图" },
  { id: "trend", label: "周趋势" },
  { id: "share", label: "构成" },
  { id: "projects", label: "项目" },
];

// 周趋势/构成的统计时间段档位；热力图固定 26 周日历不参与。
const REPORT_RANGE_WEEKS = [4, 8, 13, 26];

// 26 周走势的迷你条图：每根竖条一个周桶，与热力图同属离散语言；
// 最近一周实色，其余降透明度。零用量周留一根底线，不假装没有那一周。
function Sparkline({ points, color }) {
  const max = Math.max(...points, 1);
  const width = 100;
  const height = 26;
  const step = width / points.length;
  return (
    <svg
      className="project-sparkline"
      style={color ? { color } : undefined}
      viewBox={`0 0 ${width} ${height}`}
      preserveAspectRatio="none"
      aria-hidden="true"
    >
      {points.map((value, index) => {
        const barHeight = value > 0 ? Math.max(1.6, (value / max) * (height - 2)) : 0.8;
        return (
          <rect
            key={index}
            x={(index * step + 0.35).toFixed(2)}
            y={(height - barHeight).toFixed(2)}
            width={(step - 0.7).toFixed(2)}
            height={barHeight.toFixed(2)}
          />
        );
      })}
    </svg>
  );
}

function ReportsSection({ report }) {
  const [view, setView] = useState("heatmap");
  const [rangeWeeks, setRangeWeeks] = useState(() => {
    const stored = Number(localStorage.getItem("metrik:reportWeeks"));
    return REPORT_RANGE_WEEKS.includes(stored) ? stored : 8;
  });
  const handleRangeWeeks = (next) => {
    setRangeWeeks(next);
    localStorage.setItem("metrik:reportWeeks", String(next));
  };
  if (!report || report.status === "loading") {
    return (
      <main className="reports-section" aria-busy="true">
        <header className="settings-header">
          <h1>报告</h1>
          <p>正在读取本地账本。报告只统计已索引的数据，不触发新的日志扫描。</p>
        </header>
      </main>
    );
  }
  const data = report.data;
  if (!data || data.loadError) {
    return (
      <main className="reports-section">
        <header className="settings-header">
          <h1>报告</h1>
          <p>本地账本读取失败，报告暂不可用；未以演示数据替代。请稍后重试。</p>
        </header>
      </main>
    );
  }

  const weeks = buildHeatmapWeeks(data.days);
  const nonZero = data.days.map((day) => day.tokens).filter(Boolean).sort((a, b) => a - b);
  const q = (p) => nonZero[Math.min(nonZero.length - 1, Math.floor(nonZero.length * p))] || 1;
  const thresholds = [q(0.25), q(0.5), q(0.75)];
  const monthLabels = weeks.map((week, index) => {
    const firstCell = week.find(Boolean);
    if (!firstCell || firstCell.day > 7) return null;
    const prev = weeks[index - 1]?.find(Boolean);
    if (prev && prev.month === firstCell.month) return null;
    return { index, label: `${firstCell.month + 1}月` };
  }).filter(Boolean);
  const activeDayCount = data.days.filter((day) => day.tokens > 0).length;
  const coverageStart = Number.isFinite(data.firstEventMs)
    ? new Date(data.firstEventMs).toLocaleDateString("zh-CN")
    : null;
  // 周趋势与构成共用同一份按档位截取的周序列；热力图仍是固定 26 周日历。
  const trendWeeks = weeklySeries(data.days, rangeWeeks);
  const rangeTotals = {};
  trendWeeks.forEach((week) => {
    Object.entries(week.byAgent).forEach(([id, value]) => {
      rangeTotals[id] = (rangeTotals[id] || 0) + value;
    });
  });
  const rangeAgents = AGENT_ORDER.filter((id) => rangeTotals[id] > 0).map((id) => ({
    id,
    tokens: rangeTotals[id],
  }));
  const rangeTotal = rangeAgents.reduce((sum, agent) => sum + agent.tokens, 0);

  return (
    <main className="reports-section" aria-labelledby="reports-title">
      <header className="settings-header">
        <h1 id="reports-title">报告</h1>
        <p>
          <strong>近 26 周活动</strong> · 只统计本地账本中已索引的数据（processed token 口径，非账单）。
          {coverageStart ? `账本数据自 ${coverageStart} 起。` : ""}
          {data.isDemo ? " 当前为浏览器演示数据。" : ""}
        </p>
      </header>

      <div className="report-stats">
        <div><strong>{compactTokens(data.totalTokens)}</strong><span>26 周总量</span></div>
        <div><strong>{activeDayCount}</strong><span>活跃天数</span></div>
        <div><strong>{data.streakDays}</strong><span>连续活跃天</span></div>
      </div>

      <section className="report-card" aria-label="活动可视化">
        <div className="report-toolbar">
          <div className="report-view-toggle" role="group" aria-label="切换图表形式">
            {REPORT_VIEWS.map((item) => (
              <button
                type="button"
                key={item.id}
                className={view === item.id ? "is-selected" : ""}
                aria-pressed={view === item.id}
                onClick={() => setView(item.id)}
              >
                {item.label}
              </button>
            ))}
          </div>
          {view !== "heatmap" && view !== "projects" && (
            <div className="report-view-toggle" role="group" aria-label="统计时间段">
              {REPORT_RANGE_WEEKS.map((num) => (
                <button
                  type="button"
                  key={num}
                  className={rangeWeeks === num ? "is-selected" : ""}
                  aria-pressed={rangeWeeks === num}
                  onClick={() => handleRangeWeeks(num)}
                >
                  {num} 周
                </button>
              ))}
            </div>
          )}
        </div>
        {/* 固定高度：三种视图内容高度不同，卡片会随切换忽大忽小。 */}
        <div className="report-view-body">
        {view === "projects" ? (
          (data.projects || []).length > 0 ? (
            <ul className="project-trend-list">
              {data.projects.map((project, index) => (
                <li key={project.path}>
                  <span className="project-trend-name" title={project.path}>
                    {project.label}
                    <small>{project.activeDays} 天</small>
                  </span>
                  <Sparkline
                    points={project.weekly}
                    color={index < PROJECT_COLOR_COUNT ? `var(--viz-${index + 1})` : "var(--viz-other)"}
                  />
                  <em>{compactTokens(project.tokens)}</em>
                  {project.recentDeltaPercent != null ? (
                    <small
                      className={project.recentDeltaPercent >= 0 ? "trend-up" : "trend-down"}
                      title="近 7 天相对再前 7 天"
                    >
                      {project.recentDeltaPercent >= 0
                        ? <ArrowUp size={10} weight="bold" aria-hidden="true" />
                        : <ArrowDown size={10} weight="bold" aria-hidden="true" />}
                      {Math.abs(Math.round(project.recentDeltaPercent))}%
                    </small>
                  ) : (
                    <small className="trend-flat">—</small>
                  )}
                </li>
              ))}
            </ul>
          ) : (
            <p className="settings-muted">该时间段内暂无可归属的项目用量。</p>
          )
        ) : view === "trend" ? (
          <ReportTrendChart weeks={trendWeeks} />
        ) : view === "share" ? (
          <ReportShareDonut agents={rangeAgents} totalTokens={rangeTotal} weeksCount={trendWeeks.length} />
        ) : (
          <>
        <div className="heatmap-months" style={{ "--heatmap-weeks": weeks.length }} aria-hidden="true">
          {monthLabels.map((month) => (
            <span key={month.index} style={{ gridColumnStart: month.index + 1 }}>{month.label}</span>
          ))}
        </div>
        <div className="heatmap" style={{ "--heatmap-weeks": weeks.length }} role="img" aria-label="近 26 周每日 token 用量热力图，颜色越深用量越大">
          {weeks.map((week, weekIndex) => (
            <div className="heatmap-week" key={weekIndex}>
              {week.map((cell, dayIndex) => (
                cell ? (
                  <i
                    key={cell.key}
                    className={`heat-${heatLevel(cell.tokens, thresholds)}`}
                    title={`${cell.key} · ${cell.tokens ? `${compactTokens(cell.tokens)} tokens` : "无用量"}`}
                  />
                ) : (
                  <i key={`pad-${weekIndex}-${dayIndex}`} className="heat-pad" aria-hidden="true" />
                )
              ))}
            </div>
          ))}
        </div>
        <div className="heatmap-scale" aria-hidden="true">
          <span>少</span>
          <i className="heat-0" /><i className="heat-1" /><i className="heat-2" /><i className="heat-3" /><i className="heat-4" />
          <span>多</span>
        </div>
          </>
        )}
        </div>
      </section>

      <div className="report-grid">
        <section className="report-card" aria-label="Agent 排行">
          <h2>Agent 排行</h2>
          <ul className="model-list">
            {/* 后端按注册表顺序返回，之前直接渲染——一个叫"排行"的列表其实没排过序。 */}
            {data.agents
              .filter((agent) => agent.tokens > 0)
              .sort((left, right) => right.tokens - left.tokens)
              .map((agent) => {
              const meta = AGENT_META[agent.id];
              const max = Math.max(...data.agents.map((entry) => entry.tokens), 1);
              return (
                <li key={agent.id}>
                  <i className="model-dot" style={{ backgroundColor: meta?.accent || "#74767a" }} aria-hidden="true" />
                  <span className="model-name">{meta?.label || agent.id}</span>
                  <span className="model-track" aria-hidden="true">
                    <i style={{ transform: `scaleX(${agent.tokens / max})`, backgroundColor: meta?.accent || "#74767a" }} />
                  </span>
                  <em>{compactTokens(agent.tokens)} · {agent.activeDays} 天</em>
                </li>
              );
            })}
          </ul>
        </section>

        <section className="report-card" aria-label="模型排行">
          <h2>模型排行</h2>
          <ul className="model-list">
            {(data.topModels || []).slice(0, 8).map((entry) => {
              const max = data.topModels[0]?.tokens || 1;
              return (
                <li key={`${entry.agent}-${entry.model}`}>
                  <i className="model-dot" style={{ backgroundColor: AGENT_META[entry.agent]?.accent || "#74767a" }} aria-hidden="true" />
                  <span className="model-name">{modelDisplayName(entry.model)}</span>
                  <span className="model-track" aria-hidden="true">
                    <i style={{ transform: `scaleX(${entry.tokens / max})`, backgroundColor: AGENT_META[entry.agent]?.accent || "#74767a" }} />
                  </span>
                  <em>{compactTokens(entry.tokens)}</em>
                </li>
              );
            })}
          </ul>
        </section>
      </div>

    </main>
  );
}

function EmptySection({ section, onReturn }) {
  const item = NAV_ITEMS.find((entry) => entry.id === section);
  const Icon = item?.icon || ChartLineUp;
  return (
    <main className="empty-section">
      <span><Icon size={30} weight="light" /></span>
      <h1>{item?.label || "功能"}</h1>
      <p>该功能将在统计内核稳定后提供，首版优先实现概览与数据可信度。</p>
      <button type="button" onClick={onReturn}>返回概览</button>
    </main>
  );
}

function initialWindowMode() {
  if (typeof window === "undefined") return "compact";
  if (new URLSearchParams(window.location.search).get("view") === "expanded") return "expanded";
  // macOS 的零占地摘要属于菜单栏状态图标，不再把面板压成一条悬浮胶囊。
  if (IS_MAC) return "compact";
  // 上次收成胶囊条则恢复；expanded 不恢复。
  return localStorage.getItem("metrik:viewMode") === "strip" ? "strip" : "compact";
}

function stripPositionMode(orientation) {
  return `strip-${orientation}`;
}

/// 托盘菜单的"设置"直接开在设置页；其余情况从概览进。
function initialNav() {
  if (typeof window === "undefined") return "overview";
  return new URLSearchParams(window.location.search).get("nav") === "settings"
    ? "settings"
    : "overview";
}

export function App() {
  const [viewMode, setViewMode] = useState(initialWindowMode);
  const [period, setPeriod] = useState("today");
  const [selectedAgent, setSelectedAgent] = useState("all");
  const [quotaAgent, setQuotaAgent] = useState(
    () => visibleAgentId(localStorage.getItem("metrik:quotaAgent") || "codex"),
  );
  const [activeNav, setActiveNav] = useState(initialNav);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [pinned, setPinned] = useState(() => localStorage.getItem("metrik:pinned") === "true");
  const [pinnedHoverMode, setPinnedHoverMode] = useState(() =>
    normalizePinnedHoverMode(localStorage.getItem("metrik:pinnedHoverMode")),
  );
  const [pinnedHoverOpacity, setPinnedHoverOpacity] = useState(() => {
    const stored = Number(localStorage.getItem("metrik:pinnedHoverOpacity"));
    return Number.isFinite(stored) && stored >= 0.05 && stored <= 0.9 ? stored : 0.14;
  });
  const handlePinnedHoverMode = useCallback((next) => {
    const value = normalizePinnedHoverMode(next);
    setPinnedHoverMode(value);
    localStorage.setItem("metrik:pinnedHoverMode", value);
  }, []);
  const handlePinnedHoverOpacity = useCallback((next) => {
    const value = Math.min(0.9, Math.max(0.05, next));
    setPinnedHoverOpacity(value);
    localStorage.setItem("metrik:pinnedHoverOpacity", String(value));
  }, []);
  // 卡片与胶囊固定使用玻璃材质，用户只在深色、浅色和透明三种外观间选择。
  // expanded 仍通过 viewMode 单独关闭玻璃绘制。
  const transparent = true;
  // 胶囊条方向：横条 / 竖条，用户手动选，记住选择。
  const [stripOrientation, setStripOrientation] = useState(() =>
    localStorage.getItem("metrik:stripOrientation") === "vertical" ? "vertical" : "horizontal",
  );
  const handleToggleStripOrientation = useCallback(() => {
    setStripOrientation((current) => {
      const next = current === "vertical" ? "horizontal" : "vertical";
      localStorage.setItem("metrik:stripOrientation", next);
      return next;
    });
  }, []);
  // 大窗口（展开视图）暗色主题：自动 / 亮 / 暗三态，默认跟随系统。
  // 仅作用于展开视图；紧凑小插件的玻璃/浅色外观不受此设置影响。
  const [theme, setTheme] = useState(() => {
    const stored = localStorage.getItem("metrik:theme");
    return stored === "light" || stored === "dark" ? stored : "auto";
  });
  const handleThemeChange = useCallback((next) => {
    setTheme(next);
    localStorage.setItem("metrik:theme", next);
  }, []);
  const [systemDark, setSystemDark] = useState(
    () => window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false,
  );
  useEffect(() => {
    const media = window.matchMedia?.("(prefers-color-scheme: dark)");
    if (!media) return undefined;
    const update = () => setSystemDark(media.matches);
    media.addEventListener?.("change", update);
    return () => media.removeEventListener?.("change", update);
  }, []);
  const darkTheme = theme === "auto" ? systemDark : theme === "dark";
  // data-theme 只挂在展开窗口上：紧凑态永不带该属性，暗色 CSS 因此绝不会
  // 泄漏到小插件或它弹出的来源抽屉（Windows 下两态共用同一文档）。
  useLayoutEffect(() => {
    const root = document.documentElement;
    if (viewMode === "expanded") {
      root.dataset.theme = darkTheme ? "dark" : "light";
    } else {
      delete root.dataset.theme;
    }
  }, [viewMode, darkTheme]);
  // macOS 完整视图是独立原生窗口：手动明暗时让原生标题栏跟随内容；"自动"传 null
  // 交回系统（内容也跟随系统，两者一致）。只作用于展开窗口，不碰紧凑面板；
  // 其它平台后端 no-op。
  useEffect(() => {
    if (viewMode !== "expanded") return;
    setNativeTheme(theme === "auto" ? null : theme);
  }, [viewMode, theme]);
  // 小插件展示哪些 Agent 由用户在设置里勾选；默认 Codex + Claude。
  const [widgetAgents, setWidgetAgents] = useState(() => {
    try {
      const stored = JSON.parse(localStorage.getItem("metrik:widgetAgents") || "null");
      if (Array.isArray(stored)) {
        const valid = normalizeVisibleAgentList(stored);
        if (valid.length) return valid;
      }
    } catch {
      // 本地设置损坏时回到默认值。
    }
    return ["codex", "claude"];
  });
  // 玻璃浓度用户可调（ModernFlyouts 的做法）；仅影响玻璃模式的 CSS tint。
  const [glassAlpha, setGlassAlpha] = useState(() => {
    const stored = Number(localStorage.getItem("metrik:glassAlpha"));
    return Number.isFinite(stored) && stored >= 0.05 && stored <= 0.96 ? stored : 0.82;
  });
  const handleGlassAlpha = useCallback((next) => {
    setGlassAlpha(next);
    localStorage.setItem("metrik:glassAlpha", String(next));
    if (IS_MAC) runWindowAction(() => broadcastMacAppearance({ glassAlpha: next }));
  }, []);
  // 组件外观：深色 HUD / 透亮白（苹果式白 tint + 深色文字）/ 透明（不铺材质，
  // 直接透出桌面）。用户可选，记住选择。作用于 Windows 与 Linux 的卡片和胶囊；
  // macOS 面板材质跟随系统 vibrancy，不提供此项。
  const [glassTint, setGlassTint] = useState(() => {
    const legacyDisabled = localStorage.getItem("metrik:transparent") === "false";
    const value = legacyDisabled
      ? "dark"
      : normalizeGlassTint(localStorage.getItem("metrik:glassTint"));
    // 旧版本把“不透明”作为第四种状态；新版迁移到默认深色，并移除旧开关。
    if (legacyDisabled) localStorage.setItem("metrik:glassTint", value);
    localStorage.removeItem("metrik:transparent");
    return value;
  });
  const handleGlassTint = useCallback((next) => {
    const value = normalizeGlassTint(next);
    setGlassTint(value);
    localStorage.setItem("metrik:glassTint", value);
  }, []);
  // 透明档的文字颜色，只在透明档生效；其它两档的前景由配色本身决定。
  const [glassInk, setGlassInk] = useState(() =>
    normalizeGlassInk(localStorage.getItem("metrik:glassInk")),
  );
  const handleGlassInk = useCallback((next) => {
    const value = normalizeGlassInk(next);
    setGlassInk(value);
    localStorage.setItem("metrik:glassInk", value);
  }, []);
  // 轮转按钮要读当前配色但不该因它重建回调，用 ref 取值。
  const glassTintRef = useRef(glassTint);
  glassTintRef.current = glassTint;
  // 卡片/胶囊的整体缩放系数（连续值）：窗口尺寸与 WebView 原生 zoom 同乘一个系数，
  // 等比放大不会变形；expanded 有独立系数。生效在 windowClient 的形态切换里，
  // 设置页调整后下次回到卡片/胶囊时应用。
  const [uiScale, setUiScale] = useState(() => readUiScale());
  const handleUiScale = useCallback((next) => {
    // setWindowUiScale 负责钳位与持久化，返回实际生效值回填 UI。
    setUiScale(setWindowUiScale(next));
  }, []);
  // 自动检查更新：默认开、设置里可关。只检查和提醒（小组件上的小圆点），
  // 下载安装始终由用户在设置页点击触发。
  const [autoUpdateCheck, setAutoUpdateCheck] = useState(
    () => (localStorage.getItem("metrik:autoUpdateCheck") ?? "true") === "true",
  );
  const handleAutoUpdateCheck = useCallback((next) => {
    setAutoUpdateCheck(next);
    localStorage.setItem("metrik:autoUpdateCheck", String(next));
  }, []);
  // Windows 专属：任务栏托盘图标改为显示 Agent 列表最上方的余量数字。默认关，
  // 关着的人看到的一直是应用图标。
  const [trayBadgeEnabled, setTrayBadgeEnabled] = useState(
    () => localStorage.getItem("metrik:trayQuotaBadge") === "true",
  );
  const handleToggleTrayBadge = useCallback((next) => {
    setTrayBadgeEnabled(next);
    localStorage.setItem("metrik:trayQuotaBadge", String(next));
  }, []);
  const [availableUpdate, setAvailableUpdate] = useState(null);
  useEffect(() => {
    if (!isDesktop() || !autoUpdateCheck) return undefined;
    let cancelled = false;
    const check = () => {
      checkForUpdate()
        .then((found) => {
          if (!cancelled && found) setAvailableUpdate(found);
        })
        .catch(() => {}); // 静默失败：提醒是尽力而为，不打扰
    };
    // 错开启动扫描的高峰再查；之后每天一次。间隔只在应用连续运行时计时。
    const startTimer = window.setTimeout(check, 15000);
    const interval = window.setInterval(check, 24 * 60 * 60 * 1000);
    return () => {
      cancelled = true;
      window.clearTimeout(startTimer);
      window.clearInterval(interval);
    };
  }, [autoUpdateCheck]);
  const commitWidgetAgents = useCallback((next) => {
    setWidgetAgents(next);
    localStorage.setItem("metrik:widgetAgents", JSON.stringify(next));
    // macOS 的设置页和菜单栏面板不共享 React 状态，也收不到彼此的 storage
    // 事件。立即广播选择，避免隐藏面板下一轮轮询把旧列表写回菜单栏/WidgetKit。
    if (IS_MAC) runWindowAction(() => broadcastMacAgentSelection(next));
  }, []);
  const handleToggleWidgetAgent = useCallback((agentId) => {
    // 新勾选的排到末尾（数组顺序就是显示顺序，与胶囊条同一套语义）。
    const next = widgetAgents.includes(agentId)
      ? widgetAgents.filter((id) => id !== agentId)
      : [...widgetAgents, agentId];
    if (!next.length) return; // 至少保留一个
    commitWidgetAgents(next);
  }, [commitWidgetAgents, widgetAgents]);
  const handleMoveWidgetAgent = useCallback((agentId) => {
    const next = [...widgetAgents];
    const index = next.indexOf(agentId);
    if (index <= 0) return;
    [next[index - 1], next[index]] = [next[index], next[index - 1]];
    commitWidgetAgents(next);
  }, [commitWidgetAgents, widgetAgents]);
  const [loading, setLoading] = useState(true);
  const [rebuildState, setRebuildState] = useState({ status: "idle", message: "" });
  const [report, setReport] = useState(null);
  const [sessionsState, setSessionsState] = useState(null);
  const [projectsState, setProjectsState] = useState(null);
  // 规则变更后项目与会话一起重载。
  const [usageReloadNonce, setUsageReloadNonce] = useState(0);
  const [snapshot, setSnapshot] = useState(() => getUsageSnapshot.initial("today"));
  // 历史索引还没补齐：账本尚未覆盖完整周期，数字必须显式标注为不完整。
  const indexingPending = snapshot.indexing?.pending || 0;
  const indexing = indexingPending > 0;
  const requestSequence = useRef(0);
  const loadInFlight = useRef(false);
  const activeLoadPeriod = useRef(null);
  const queuedLoadPeriod = useRef(null);
  const currentPeriod = useRef(period);
  const widgetAgentsRef = useRef(widgetAgents);
  const rebuildInFlight = useRef(false);
  currentPeriod.current = period;
  widgetAgentsRef.current = widgetAgents;

  const loadSnapshot = useCallback(async (nextPeriod, options) => {
    if (loadInFlight.current) {
      // 普通的同周期轮询仍合并；Agent 勾选/排序变化必须排一次同周期请求，
      // 否则恰逢已有请求进行中时，WidgetKit 会一直保留旧选择。
      // 已排队的选择刷新也不能被随后到达的普通轮询清掉。
      if (options?.ensureLatestWidgetAgents || activeLoadPeriod.current !== nextPeriod) {
        queuedLoadPeriod.current = nextPeriod;
      }
      return;
    }

    loadInFlight.current = true;
    let periodToLoad = nextPeriod;
    // force（手动强制刷新）只作用于本次请求；排队的周期切换仍按常规加载。
    let forceLoad = options?.force === true;
    try {
      while (periodToLoad) {
        activeLoadPeriod.current = periodToLoad;
        queuedLoadPeriod.current = null;
        const requestId = ++requestSequence.current;
        setLoading(true);
        const next = await getUsageSnapshot(periodToLoad, {
          force: forceLoad,
          // 每轮都读 ref：排队请求必须拿到最新勾选，不能沿用创建回调时的闭包。
          widgetAgents: widgetAgentsRef.current,
        });
        forceLoad = false;
        if (requestId === requestSequence.current && !queuedLoadPeriod.current) {
          setSnapshot(next);
        }
        periodToLoad = queuedLoadPeriod.current;
      }
    } finally {
      activeLoadPeriod.current = null;
      loadInFlight.current = false;
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    // 周期或 Agent 选择变化都重发快照；选择变化即便撞上在途请求也不能被合并。
    loadSnapshot(period, { ensureLatestWidgetAgents: true });
  }, [period, widgetAgents, loadSnapshot]);

  useEffect(() => {
    // 历史索引未补齐时快速迭代：每次快照只花掉一小段补齐预算，靠连续刷新把
    // 剩余文件啃完，界面全程可用。补齐结束后回到常规节奏。
    // strip 与 compact 同档：都是常驻小组件，不需要展开视图的高频刷新。
    const refreshEvery = indexing ? 400 : viewMode === "expanded" ? 60_000 : 300_000;
    let timer;

    const schedule = () => {
      window.clearInterval(timer);
      timer = undefined;
      if (document.visibilityState === "visible") {
        timer = window.setInterval(() => loadSnapshot(period), refreshEvery);
      } else if (trayBadgeEnabled) {
        // 托盘数字还亮在任务栏上：窗口隐藏时保留慢速刷新，数字不会冻在旧值。
        timer = window.setInterval(
          () => loadSnapshot(period),
          TRAY_BADGE_HIDDEN_REFRESH_MS,
        );
      }
    };

    const refreshWhenVisible = () => {
      schedule();
      if (document.visibilityState === "visible") loadSnapshot(period);
    };

    schedule();
    document.addEventListener("visibilitychange", refreshWhenVisible);
    window.addEventListener("focus", refreshWhenVisible);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", refreshWhenVisible);
      window.removeEventListener("focus", refreshWhenVisible);
    };
  }, [loadSnapshot, period, viewMode, indexing, trayBadgeEnabled]);

  useEffect(() => {
    // macOS 面板由系统管层级和位置：不置顶、不恢复坐标。
    if (IS_MAC) return;
    if (pinned) runWindowAction(() => setWindowPinned(true));
    runWindowAction(() => syncLinuxTrayPinned(pinned));
    // 小组件回到上次摆放的位置（含固定位置），坐标已不在任何屏幕上时居中。
    // strip 形态的启动定位在 strip 专属 effect 里做。
    if (viewMode === "compact") {
      // 先到位、再按最终所在显示器的 factor 算尺寸：并发执行可能在错误的
      // factor 下算物理尺寸，视口缩水后 320 的内容被裁。
      runWindowAction(async () => {
        await restoreWindowPosition("compact");
        await applyStartupUiScale("compact");
      });
    }
  }, []);

  const pinnedRef = useRef(pinned);
  pinnedRef.current = pinned;
  const viewModeRef = useRef(viewMode);
  viewModeRef.current = viewMode;
  const stripOrientationRef = useRef(stripOrientation);
  stripOrientationRef.current = stripOrientation;
  // 变形前的形态：applyWindowMode 的 fromMode 用它按形态分别记位，互不污染。
  const previousViewModeRef = useRef(viewMode);

  useLayoutEffect(() => {
    if (!IS_LINUX) return undefined;
    const opacity = pinnedHoverMode === "hide"
      ? PINNED_HIDDEN_OPACITY
      : pinnedHoverOpacity;
    setPinnedHoverTargetOpacity(opacity);
    document.documentElement.style.setProperty("--pinned-hover-target-opacity", String(opacity));
    document.documentElement.dataset.pinnedHoverMode = pinnedHoverMode;
  }, [pinnedHoverMode, pinnedHoverOpacity]);

  useEffect(() => () => {
    if (!IS_LINUX) return;
    document.documentElement.style.removeProperty("--pinned-hover-target-opacity");
    delete document.documentElement.dataset.pinnedHoverMode;
  }, []);

  // 拖动后记住小组件位置，供下次启动恢复。
  useEffect(() => {
    const stopPromise = startPositionMemory(() => (
      viewModeRef.current === "strip"
        ? stripPositionMode(stripOrientationRef.current)
        : viewModeRef.current
    ));
    return () => {
      stopPromise.then((stop) => stop?.());
    };
  }, []);

  // Windows 小组件终身保持创建期透明窗口，不在运行时切换 DWM backdrop。
  // 桌面端初始直接选 alpha，避免 WebView 背景确认前闪一帧 CSS fallback。
  const [glassMode, setGlassMode] = useState(() =>
    resolveGlassMode({
      enabled: transparent && viewMode !== "expanded",
      tintStyle: glassTint,
      nativeAvailable: false,
      trueAlphaAvailable: isDesktop() && isWindowsPlatform(),
    }),
  );
  useEffect(() => {
    let cancelled = false;
    const apply = () => {
      setWindowGlass(
        transparent && viewMode !== "expanded",
        viewMode === "strip" ? 20 : 14,
        glassTint,
      )
        .then((mode) => {
          if (!cancelled) setGlassMode(mode);
        })
        .catch((error) => {
          console.warn("Unable to update the desktop window.", error);
          if (!cancelled) {
            setGlassMode(resolveGlassMode({
              enabled: transparent && viewMode !== "expanded",
              tintStyle: glassTint,
              nativeAvailable: false,
              trueAlphaAvailable: isDesktop() && isWindowsPlatform(),
            }));
          }
        });
    };
    apply();
    const media = window.matchMedia?.("(prefers-color-scheme: dark)");
    media?.addEventListener?.("change", apply);
    return () => {
      cancelled = true;
      media?.removeEventListener?.("change", apply);
    };
  }, [transparent, viewMode, glassTint]);

  // 深色档的 0.55 下限是白色文字的可读下限，不是历史补偿：白字压在 0.55 深底
  // 上、透出亮壁纸时对比度 4.2:1，降到 0.22 就只剩 1.9:1。浅色档同理。
  // clear 走深色文字，下限交给 CSS 里那条白霜曲线（0.38 起）。
  const shellGlassAlpha = useMemo(() => {
    if (glassTint === "clear") return glassAlpha;
    const t = (glassAlpha - 0.05) / (0.96 - 0.05);
    return 0.55 + Math.min(1, Math.max(0, t)) * (0.96 - 0.55);
  }, [glassAlpha, glassTint]);
  useLayoutEffect(() => {
    document.documentElement.style.setProperty(
      "--shell-glass-alpha",
      String(shellGlassAlpha),
    );
    return () => {
      document.documentElement.style.removeProperty("--shell-glass-alpha");
    };
  }, [shellGlassAlpha]);

  // 圆角写成 CSS px 会被卡片和胶囊各自的 WebView 原生 zoom 放大：同样 8px，
  // 卡片(zoom 0.95)画出 9.5 物理像素，竖胶囊(zoom 1.75)画出 17.5，成了大圆头。
  // 折成物理像素让两种形态、任何缩放档都是同一个视觉半径。
  useLayoutEffect(() => {
    const apply = () => {
      const dpr = window.devicePixelRatio || 1;
      document.documentElement.style.setProperty(
        "--glass-radius",
        `${(GLASS_RADIUS_PX / dpr).toFixed(2)}px`,
      );
    };
    apply();
    window.addEventListener("resize", apply);
    return () => {
      window.removeEventListener("resize", apply);
      document.documentElement.style.removeProperty("--glass-radius");
    };
  }, []);

  // mac 的设置页开在独立的完整视图窗口里，面板是另一个 webview 实例，
  // 拖滑杆时面板自己的 React 状态不会变。WKWebView 的 storage 事件不跨
  // 窗口触发（每窗口独立 process pool），所以经 Tauri 事件总线把玻璃浓度
  // 实时推进面板（改 CSS 变量当场可见）。广播也会回到发送方，处理是幂等
  // 的。其它平台单窗口无此问题。
  useEffect(() => {
    if (!IS_MAC || !isDesktop()) return undefined;
    const unlistenPromise = onMacAppearance((payload) => {
      const alpha = Number(payload.glassAlpha);
      if (Number.isFinite(alpha) && alpha >= 0.05 && alpha <= 0.96) setGlassAlpha(alpha);
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    if (!IS_MAC || !isDesktop()) return undefined;
    const unlistenPromise = onMacAgentSelection((payload) => {
      if (!Array.isArray(payload.agents)) return;
      const next = normalizeVisibleAgentList(payload.agents);
      if (!next.length) return;
      // emit 会回到发送窗口；内容相同时保留原数组，避免无意义快照刷新。
      setWidgetAgents((current) => {
        if (current.length === next.length && current.every((id, index) => id === next[index])) {
          return current;
        }
        localStorage.setItem("metrik:widgetAgents", JSON.stringify(next));
        return next;
      });
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    if (!IS_MAC || !isDesktop()) return undefined;
    let cancelled = false;
    // 权威 Agent 选择在后端数据库：设置窗口、菜单栏面板、桌面组件各有独立的
    // localStorage（WKWebView 不共享），本地值只是缓存。启动时以后端为准，
    // 任何窗口的旧缓存都不能把已勾选的 Agent 从菜单栏和 WidgetKit 抹掉；
    // 后端还没保存过（首次启动）时保留本地缓存，由首轮轮询播种。
    getMacAgentSelection()
      .then((agents) => {
        const next = normalizeVisibleAgentList(agents || []);
        if (cancelled || !next.length) return;
        setWidgetAgents((current) => {
          if (current.length === next.length && current.every((id, index) => id === next[index])) {
            return current;
          }
          localStorage.setItem("metrik:widgetAgents", JSON.stringify(next));
          return next;
        });
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  // 边缘挂靠：拖到屏幕上缘自动收起，鼠标碰边弹出。
  useEffect(() => {
    const stopPromise = startEdgeDock({
      getMode: () => viewModeRef.current,
      getPinned: () => pinnedRef.current,
    });
    return () => {
      stopPromise.then((stop) => stop?.());
    };
  }, []);

  useEffect(() => {
    if (!drawerOpen) return undefined;
    const closeOnEscape = (event) => {
      if (event.key === "Escape") setDrawerOpen(false);
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [drawerOpen]);

  const visibleTokens = useMemo(() => {
    if (selectedAgent === "all") return snapshot.totalTokens;
    return snapshot.agents.find((agent) => agent.id === selectedAgent)?.tokens || 0;
  }, [selectedAgent, snapshot]);

  // 本机装了哪些 Agent，供设置里的分组用。拿不到检测结果时返回 null，
  // 由列表决定不折叠——加载中、演示数据、旧后端都属于"不知道"，
  // 而不知道时把 Agent 收进折叠区会让用户以为我们不支持它。
  const detectedAgents = useMemo(() => {
    if (snapshot.pending || snapshot.loadError) return null;
    const known = snapshot.agents.filter((agent) => typeof agent.detected === "boolean");
    if (!known.length) return null;
    return new Set(known.filter((agent) => agent.detected).map((agent) => agent.id));
  }, [snapshot]);

  // 配额卡只在小组件已勾选展示的 Agent 里轮换（用户明确不想看的不进循环）；
  // 勾选了但配额来源未启用的也保留——"官方配额不可用/设置中开启钩子"的
  // 提示本身是有效信息。widgetAgents 由设置保证非空。
  const quotaAgents = useMemo(
    () => widgetAgents.filter((id) => AGENT_META[id]),
    [widgetAgents],
  );
  const activeQuotaAgent = quotaAgents.includes(quotaAgent) ? quotaAgent : quotaAgents[0];
  const handleCycleQuotaAgent = useCallback(() => {
    const index = quotaAgents.indexOf(activeQuotaAgent);
    const next = quotaAgents[(index + 1) % quotaAgents.length];
    setQuotaAgent(next);
    localStorage.setItem("metrik:quotaAgent", next);
  }, [activeQuotaAgent, quotaAgents]);

  // 自动模式：胶囊条显示全部有官方配额数据的 agent（快照顺序）。
  const autoStripAgents = useMemo(
    () =>
      (snapshot.agentQuotas || [])
        .filter(quotaHasData)
        .map((entry) => entry.agent)
        .filter((agent) => AGENT_META[agent]),
    [snapshot],
  );
  const autoStripAgentsRef = useRef(autoStripAgents);
  autoStripAgentsRef.current = autoStripAgents;
  // 用户自选模式：内容与顺序都由用户在设置里定；null = 自动。
  const [stripAgentsSetting, setStripAgentsSetting] = useState(() => {
    try {
      const stored = JSON.parse(localStorage.getItem("metrik:stripAgents") || "null");
      if (Array.isArray(stored)) {
        const valid = normalizeVisibleAgentList(stored);
        if (valid.length) return valid;
      }
    } catch {
      // 本地设置损坏时回到自动模式。
    }
    return null;
  });
  const stripAgents = stripAgentsSetting ?? autoStripAgents;

  // 胶囊条的独立缩放系数（与小组件互不影响）：设置页滑杆调整，下次进入时生效。
  const [stripScale, setStripScaleState] = useState(() => readStripScale());
  const handleStripScale = useCallback((next) => {
    setStripScaleState(setStripScale(next));
  }, []);
  const stripLayoutRef = useRef({
    orientation: stripOrientation,
    count: stripAgents.length,
  });
  stripLayoutRef.current = {
    orientation: stripOrientation,
    count: stripAgents.length,
  };

  // DPI 事件的 payload 是 Windows 已确认的新 factor，直接用它重算；compact
  // 与 strip 都不能只靠 DOM 自愈，因为被裁的可能是 WebView 外层 HWND。
  useEffect(() => {
    const stopPromise = onScaleFactorChanged(({ scaleFactor } = {}) => {
      if (viewModeRef.current === "compact") {
        runLatestWindowCorrection(() => reassertCompactSize(scaleFactor));
        return;
      }
      if (viewModeRef.current === "strip") {
        const layout = stripLayoutRef.current;
        const estimate = stripWindowSize(layout.orientation, layout.count);
        const content = stripContentSize(layout.orientation, estimate);
        runLatestWindowCorrection(() =>
          reassertStripSize({ ...content, scaleFactor }),
        );
      }
    });
    return () => {
      stopPromise.then((stop) => stop?.());
    };
  }, []);

  // macOS 菜单栏与紧凑小组件共用 widgetAgents：用户勾选哪些 Agent，状态栏就
  // 显示哪些品牌图标与官方额度。无可靠额度时传 null，原生状态项明确显示 "--"。
  const macStatusItems = useMemo(() => {
    const statusFor = (agentId) => {
      const cell = stripCellData(agentQuotaFor(snapshot, agentId));
      const view = cell?.tightest?.view;
      if (!view?.available) return { remaining: null, stale: false };
      return {
        remaining: Math.max(0, Math.min(100, Math.round(view.remainingPercent))),
        stale: Boolean(view.stale || view.quality === "official_snapshot"),
      };
    };
    return widgetAgents.map((agent) => {
      const status = statusFor(agent);
      return {
        agent,
        remaining: status.remaining,
        stale: Boolean(snapshot.pending || snapshot.loadError || status.stale),
      };
    });
  }, [snapshot, widgetAgents]);

  useEffect(() => {
    if (!IS_MAC) return;
    runWindowAction(() => updateMacStatusItems(macStatusItems));
  }, [macStatusItems]);

  // Windows 托盘徽标与 macOS 菜单栏同源：都从 widgetAgents 的状态列表取数，
  // 只是托盘只放得下一个数字，取列表最上方（排序最高）的那个 Agent。
  const trayBadge = useMemo(
    () => trayBadgeSpec(macStatusItems),
    [macStatusItems],
  );
  useEffect(() => {
    if (!IS_WINDOWS) return;
    const spec = trayBadgeEnabled
      ? trayBadge && {
          ...trayBadge,
          tooltip: trayBadgeTooltip(
            AGENT_META[trayBadge.agent]?.label || trayBadge.agent,
            trayBadge.percent,
            trayBadge.stale,
          ),
        }
      : null;
    runWindowAction(() => updateTrayQuotaBadge(spec));
  }, [trayBadge, trayBadgeEnabled]);

  // 勾选即追加到末尾（勾选顺序 = 显示顺序）；首次改动时以当前自动列表为基准。
  const handleToggleStripAgent = useCallback((agentId) => {
    setStripAgentsSetting((current) => {
      const base = current ?? autoStripAgentsRef.current;
      const next = base.includes(agentId)
        ? base.filter((id) => id !== agentId)
        : [...base, agentId];
      if (!next.length) return current; // 至少保留一个
      localStorage.setItem("metrik:stripAgents", JSON.stringify(next));
      return next;
    });
  }, []);
  const handleMoveStripAgent = useCallback((agentId) => {
    setStripAgentsSetting((current) => {
      const base = [...(current ?? autoStripAgentsRef.current)];
      const index = base.indexOf(agentId);
      if (index <= 0) return current;
      [base[index - 1], base[index]] = [base[index], base[index - 1]];
      localStorage.setItem("metrik:stripAgents", JSON.stringify(base));
      return base;
    });
  }, []);
  // 进入 strip 时整窗变形一次（含启动恢复）；之后窗口尺寸由 StripBar 的
  // 内容测量观察器按真实渲染收敛，不再用手写常量重算。
  const stripApplied = useRef(null);
  useEffect(() => {
    if (IS_MAC) return;
    if (viewMode !== "strip") {
      stripApplied.current = null;
      return;
    }
    if (stripApplied.current === stripOrientation) return;
    const previousOrientation = stripApplied.current;
    stripApplied.current = stripOrientation;
    const fromMode = previousViewModeRef.current;
    const positionMode = stripPositionMode(stripOrientation);
    const fromPositionMode = previousOrientation
      ? stripPositionMode(previousOrientation)
      : fromMode === "strip"
        ? positionMode
        : fromMode;
    runWindowAction(async () => {
      // 首帧直接用上次测量收敛的尺寸（没有才回退常量估计），
      // 避免变形后 240ms 再跳一次的两段式卡顿。
      const estimate = stripWindowSize(stripOrientation, stripAgents.length);
      await applyWindowMode("strip", {
        ...stripContentSize(stripOrientation, estimate),
        positionMode,
        ...(fromPositionMode !== positionMode ? { fromPositionMode } : {}),
      });
      // expanded 期间置顶被强制解除；回到悬浮形态按用户选择重新断言。
      await setWindowPinned(pinnedRef.current);
    });
  }, [viewMode, stripAgents.length, stripOrientation]);
  const appBusy = loading || rebuildState.status === "busy";
  const comparisonIsFlat = Math.abs(snapshot.comparisonPercent) < 0.5;
  const comparisonIsLower = snapshot.comparisonPercent < -0.5;
  const ComparisonArrow = comparisonIsLower ? ArrowDown : ArrowUp;
  // 标签跟随快照的实际周期；切换周期扫描期间显式提示，不给旧数据贴新标签。
  const comparisonLabel = snapshot.period === "today" ? "比近 7 日同时段" : "比前一周期";
  const flatComparisonLabel = snapshot.period === "today" ? "与近 7 日同时段持平" : "与前一周期持平";
  const switchingPeriod = !snapshot.pending && !snapshot.loadError && period !== snapshot.period;

  const handleNavChange = (next) => {
    if (next === "sources") {
      setDrawerOpen(true);
      return;
    }
    setActiveNav(next);
  };

  // 报告只读账本、不触发扫描；进入报告页时（重新）加载。
  useEffect(() => {
    if (activeNav !== "reports" || viewMode !== "expanded") return;
    let cancelled = false;
    setReport({ status: "loading", data: null });
    getUsageReport().then((data) => {
      if (!cancelled) setReport({ status: "ready", data });
    });
    return () => {
      cancelled = true;
    };
  }, [activeNav, viewMode]);

  // 用量页只读账本；项目与会话同批加载，规则变更后整体重载。
  useEffect(() => {
    if (activeNav !== "usage" || viewMode !== "expanded") return;
    let cancelled = false;
    setProjectsState({ status: "loading", data: null });
    setSessionsState({ status: "loading", data: null });
    Promise.all([getUsageProjects(period), getUsageSessions(period)]).then(([projectsData, sessionsData]) => {
      if (cancelled) return;
      setProjectsState({ status: "ready", data: projectsData });
      setSessionsState({ status: "ready", data: sessionsData });
    });
    return () => {
      cancelled = true;
    };
  }, [activeNav, viewMode, period, usageReloadNonce]);

  const handleWindowMode = useCallback((nextMode) => {
    // macOS：菜单栏状态图标承担零占地摘要；面板只保留紧凑卡片，完整视图
    // 另开标准窗口，不再把 NSPanel 变形成一条悬浮胶囊。
    if (IS_MAC) {
      if (nextMode === "expanded") {
        runWindowAction(() => openExpandedWindow());
        return;
      }
      setViewMode("compact");
      setActiveNav("overview");
      localStorage.setItem("metrik:viewMode", "compact");
      runWindowAction(() => applyWindowMode("compact"));
      return;
    }
    const fromMode = viewModeRef.current;
    previousViewModeRef.current = fromMode;
    const fromPositionMode = fromMode === "strip"
      ? stripPositionMode(stripOrientationRef.current)
      : fromMode;
    setViewMode(nextMode);
    if (nextMode === "compact") setActiveNav("overview");
    if (nextMode !== "expanded") localStorage.setItem("metrik:viewMode", nextMode);
    // strip 的变形由 strip 专属 effect 统一处理（含启动恢复与置顶断言）。
    // expanded 在 applyWindowMode 里强制解除置顶；回到 compact 后按用户选择
    // 重新断言，固定只属于悬浮形态。
    if (nextMode === "strip") return;
    runWindowAction(async () => {
      await applyWindowMode(nextMode, { fromPositionMode });
      if (nextMode === "compact") await setWindowPinned(pinnedRef.current);
    });
  }, []);

  // 小组件上的更新提示点：点击直达设置页。macOS 的设置在独立展开窗口里。
  const handleOpenUpdate = useCallback(() => {
    if (IS_MAC) {
      runWindowAction(() => openExpandedWindow("settings"));
      return;
    }
    setActiveNav("settings");
    handleWindowMode("expanded");
  }, [handleWindowMode]);

  // 托盘右键"显示完整视图"：胶囊/卡片直达完整视图，跳过中间那一步。
  // macOS 的完整视图是独立窗口，由菜单栏自己开，不发这个事件。
  useEffect(() => {
    if (IS_MAC) return undefined;
    const stopPromise = onTrayShowExpanded(() => {
      // 置顶悬浮层已经完全只读；托盘打开完整视图时直达设置，让用户有一条
      // 明确且唯一的解除路径。未置顶仍按原行为进入概览。
      setActiveNav(IS_LINUX && pinnedRef.current ? "settings" : "overview");
      handleWindowMode("expanded");
    });
    return () => {
      stopPromise.then((stop) => stop?.());
    };
  }, [handleWindowMode]);

  const handlePinnedChange = useCallback((next) => {
    const value = Boolean(next);
    setPinned(value);
    localStorage.setItem("metrik:pinned", String(value));
    // 完整视图始终是普通窗口；在设置中选择“置顶只读”只为下次回到悬浮形态
    // 预设状态，不能让 1120×760 的设置窗口本身盖住其它应用。
    runWindowAction(async () => {
      await setWindowPinned(value && viewModeRef.current !== "expanded");
      await syncLinuxTrayPinned(value);
    });
  }, []);

  const handleTogglePinned = useCallback(() => {
    handlePinnedChange(!pinnedRef.current);
  }, [handlePinnedChange]);

  // Linux 托盘菜单已在原生层先更新了菜单文案；这里把其目标值写入 UI、
  // localStorage 与窗口层级。Windows/macOS 不订阅该 Linux 专用事件。
  useEffect(() => {
    const stopPromise = onTrayPinnedChange((next) => handlePinnedChange(next));
    return () => {
      stopPromise.then((stop) => stop?.());
    };
  }, [handlePinnedChange]);

  // 标题栏的 ◐ 按钮与设置页保持同一模型，只循环深色、浅色、透明三种组件外观。
  // 技术层的 off 只属于 expanded/回落状态，不作为第四种配色暴露。
  const handleToggleTransparent = useCallback(() => {
    handleGlassTint(nextGlassTint(normalizeGlassTint(glassTintRef.current)));
  }, [handleGlassTint]);

  const handleRebuildLedger = useCallback(async () => {
    if (rebuildInFlight.current) return;

    const rebuildPeriod = currentPeriod.current;
    rebuildInFlight.current = true;
    requestSequence.current += 1;
    setRebuildState({
      status: "busy",
      message: "正在清理派生统计索引并重建当前周期…",
    });

    try {
      const next = await rebuildLocalLedger(rebuildPeriod);
      if (currentPeriod.current === rebuildPeriod) {
        setSnapshot(next);
      } else {
        loadSnapshot(currentPeriod.current);
      }
      setRebuildState({
        status: "success",
        message: next.isDemo
          ? "演示流程已完成；没有访问或删除任何本机文件。"
          : `重建完成 · 更新于 ${formatClock(next.generatedAt)}`,
      });
    } catch (error) {
      console.warn("Unable to rebuild the local ledger.", error);
      setRebuildState({
        status: "error",
        message: "重建未完成。原始 Agent 日志与凭据未受影响，请稍后重试。",
      });
    } finally {
      rebuildInFlight.current = false;
    }
  }, [loadSnapshot]);

  // 小插件/完整视图的手动刷新：强制后端重取官方额度与本地统计（绕过缓存）。
  // 注意：这是 Hook，必须放在 strip/compact 的条件 return 之前，
  // 否则切形态时 hooks 数量变化会直接把 React 树崩成白屏。
  const handleForceRefresh = useCallback(() => {
    loadSnapshot(currentPeriod.current, { force: true });
  }, [loadSnapshot]);

  if (viewMode === "strip" && !IS_MAC) {
    return (
      <>
        <StripBar
          snapshot={snapshot}
          agents={stripAgents}
          pinned={pinned}
          loading={appBusy}
          transparent={transparent}
          glassAlpha={shellGlassAlpha}
          glassMode={glassMode}
          glassTint={glassTint}
          glassInk={glassInk}
          orientation={stripOrientation}
          onToggleOrientation={handleToggleStripOrientation}
          onTogglePinned={handleTogglePinned}
          onRestore={() => handleWindowMode("compact")}
          onExpand={() => handleWindowMode("expanded")}
          availableUpdate={availableUpdate}
          onOpenUpdate={handleOpenUpdate}
        />
      </>
    );
  }

  if (viewMode === "compact") {
    return (
      <>
        <CompactWidget
          snapshot={snapshot}
          period={period}
          selectedAgent={selectedAgent}
          visibleTokens={visibleTokens}
          loading={appBusy}
          pinned={pinned}
          transparent={transparent}
          glassMode={glassMode}
          glassTint={glassTint}
          glassInk={glassInk}
          onPeriodChange={setPeriod}
          onOpenSources={() => setDrawerOpen(true)}
          onTogglePinned={handleTogglePinned}
          onToggleTransparent={handleToggleTransparent}
          onExpand={handleWindowMode}
          onRefresh={handleForceRefresh}
          quotaAgent={activeQuotaAgent}
          onCycleQuotaAgent={handleCycleQuotaAgent}
          widgetAgents={widgetAgents}
          glassAlpha={shellGlassAlpha}
          availableUpdate={availableUpdate}
          onOpenUpdate={handleOpenUpdate}
        />
        {drawerOpen && (
          <SourceDrawer
            snapshot={snapshot}
            rebuildState={rebuildState}
            onRebuildLedger={handleRebuildLedger}
            onClose={() => setDrawerOpen(false)}
          />
        )}
      </>
    );
  }

  return (
    <>
      <div className={`app-shell app-shell--expanded ${appBusy ? "is-loading" : ""}`}>
        {/* macOS 的完整视图是标准窗口：拖动和窗口按钮都归原生标题栏，不自绘。 */}
        {!IS_MAC && (
          <>
            <div className="expanded-drag-region" data-tauri-drag-region aria-hidden="true" />
            <WindowActions
              mode="expanded"
              pinned={pinned}
              theme={theme}
              darkTheme={darkTheme}
              onThemeChange={handleThemeChange}
              onToggleMode={handleWindowMode}
              onTogglePinned={handleTogglePinned}
            />
          </>
        )}
        {/* 手动强制刷新：强制后端重取官方额度与本地统计（绕过缓存），加载期间禁用并旋转。 */}
        <button
          type="button"
          className={`expanded-refresh ${IS_MAC ? "expanded-refresh--mac" : ""}`}
          onClick={handleForceRefresh}
          disabled={appBusy}
          aria-label="强制刷新官方额度与本地统计"
          title="强制刷新官方额度与本地统计"
        >
          <ArrowsClockwise size={15} weight="light" aria-hidden="true" />
        </button>
        <Sidebar activeNav={activeNav} onNavChange={handleNavChange} snapshot={snapshot} loading={appBusy} />

        {activeNav === "overview" ? (
          <>
            <PeriodControl period={period} onChange={setPeriod} />
            <main className="dashboard">
              <header className="hero-copy">
                <span className="section-kicker">{PERIODS.find((item) => item.id === snapshot.period)?.label}</span>
                <div className="metric-line" aria-live="polite" aria-atomic="true">
                  <h1>{snapshot.pending || snapshot.loadError ? "--" : compactTokens(visibleTokens)}</h1>
                  <span>tokens</span>
                </div>
                <p className="comparison">
                  {switchingPeriod ? (
                    <>
                      <ClockCounterClockwise size={22} weight="light" aria-hidden="true" />
                      正在统计{PERIODS.find((item) => item.id === period)?.label}数据，暂显示{PERIODS.find((item) => item.id === snapshot.period)?.label}
                    </>
                  ) : snapshot.pending ? (
                    <>
                      <ClockCounterClockwise size={22} weight="light" aria-hidden="true" />
                      正在建立本地索引，窗口仍可操作
                    </>
                  ) : snapshot.loadError ? (
                    <>
                      <ClockCounterClockwise size={22} weight="light" aria-hidden="true" />
                      本地数据读取失败，未显示演示数字
                    </>
                  ) : selectedAgent !== "all" ? (
                    <>
                      <FunnelSimple size={22} weight="light" aria-hidden="true" />
                      仅显示 {AGENT_META[selectedAgent].label} 用量
                    </>
                  ) : snapshot.comparisonAvailable ? (
                    <>
                      {comparisonIsFlat ? (
                        flatComparisonLabel
                      ) : (
                        <>
                          <ComparisonArrow size={22} weight="bold" aria-hidden="true" />
                          {comparisonLabel}{comparisonIsLower ? "低" : "高"}{" "}
                          <strong>{Math.abs(snapshot.comparisonPercent).toFixed(0)}%</strong>
                        </>
                      )}
                    </>
                  ) : (
                    <>
                      <ClockCounterClockwise size={22} weight="light" aria-hidden="true" />
                      {period === "today" ? "近 7 日同时段基线尚未建立" : "前一周期基线尚未建立"}
                    </>
                  )}
                </p>
              </header>

              {snapshot.pending || snapshot.loadError ? (
                <ChartState pending={snapshot.pending} />
              ) : (
                <>
                  <UsageChart snapshot={snapshot} selectedAgent={selectedAgent} dark={darkTheme} />
                  <BreakdownSection snapshot={snapshot} selectedAgent={selectedAgent} />
                </>
              )}
            </main>

            <div className="inspector-zone">
              <Inspector
                snapshot={snapshot}
                selectedAgent={selectedAgent}
                onSelectAgent={setSelectedAgent}
                onOpenSources={() => setDrawerOpen(true)}
                widgetAgents={widgetAgents}
              />
            </div>
          </>
        ) : activeNav === "settings" ? (
          <SettingsSection
            onSnapshotRefresh={() => loadSnapshot(currentPeriod.current)}
            widgetAgents={widgetAgents}
            onToggleWidgetAgent={handleToggleWidgetAgent}
            onMoveWidgetAgent={handleMoveWidgetAgent}
            stripAgents={stripAgents}
            onToggleStripAgent={handleToggleStripAgent}
            onMoveStripAgent={handleMoveStripAgent}
            detectedAgents={detectedAgents}
            trayBadgeEnabled={trayBadgeEnabled}
            onToggleTrayBadge={handleToggleTrayBadge}
            glassAlpha={glassAlpha}
            onGlassAlpha={handleGlassAlpha}
            glassTint={glassTint}
            onGlassTint={handleGlassTint}
            glassInk={glassInk}
            onGlassInk={handleGlassInk}
            uiScale={uiScale}
            onUiScale={handleUiScale}
            stripScale={stripScale}
            onStripScale={handleStripScale}
            pinned={pinned}
            onPinnedChange={handlePinnedChange}
            pinnedHoverMode={pinnedHoverMode}
            onPinnedHoverMode={handlePinnedHoverMode}
            pinnedHoverOpacity={pinnedHoverOpacity}
            onPinnedHoverOpacity={handlePinnedHoverOpacity}
            theme={theme}
            onThemeChange={handleThemeChange}
            autoUpdateCheck={autoUpdateCheck}
            onAutoUpdateCheck={handleAutoUpdateCheck}
            availableUpdate={availableUpdate}
          />
        ) : activeNav === "reports" ? (
          <ReportsSection report={report} />
        ) : activeNav === "usage" ? (
          <>
            <PeriodControl period={period} onChange={setPeriod} fullWidthArea />
            <UsageSection
              projectsState={projectsState}
              sessionsState={sessionsState}
              period={period}
              onRulesChanged={() => setUsageReloadNonce((value) => value + 1)}
            />
          </>
        ) : (
          <EmptySection section={activeNav} onReturn={() => setActiveNav("overview")} />
        )}
      </div>

      {drawerOpen && (
        <SourceDrawer
          snapshot={snapshot}
          rebuildState={rebuildState}
          onRebuildLedger={handleRebuildLedger}
          onClose={() => setDrawerOpen(false)}
        />
      )}
    </>
  );
}
