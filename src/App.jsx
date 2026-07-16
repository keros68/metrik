import { lazy, Suspense, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  ArrowDown,
  ArrowUp,
  ArrowsDownUp,
  ArrowsInLineVertical,
  ArrowsInSimple,
  ArrowsLeftRight,
  ArrowsOutSimple,
  CaretRight,
  ChartBar,
  ChartLineUp,
  Check,
  CircleHalfTilt,
  Copy,
  ClockCounterClockwise,
  Database,
  FileText,
  FunnelSimple,
  GearSix,
  HardDrives,
  Minus,
  PushPinSimple,
  ShieldCheck,
  X,
} from "@phosphor-icons/react";
import antigravityAppIcon from "./assets/antigravity-app-icon.png";
import chatgptAppIcon from "./assets/chatgpt-app-icon.png";
import claudeAppIcon from "./assets/claude-app-icon.jpg";
import kimiAppIcon from "./assets/kimi-app-icon.png";
import opencodeAppIcon from "./assets/opencode-app-icon.png";
import zcodeAppIcon from "./assets/zcode-app-icon.png";
import {
  configureSync,
  getClaudeHookStatus,
  getClaudeOauthStatus,
  setClaudeOauth,
  getSyncSettings,
  getUsageReport,
  exportCsvFile,
  getUsageSessions,
  getUsageSnapshot,
  rebuildLocalLedger,
  setClaudeHook,
} from "./usageClient";
import {
  applyStartupUiScale,
  applyWindowMode,
  checkForUpdate,
  closeWindow,
  getAutostart,
  installUpdate,
  isDesktop,
  isMacPlatform,
  minimizeWindow,
  openExpandedWindow,
  resizeStripWindow,
  restoreWindowPosition,
  setAutostart,
  setNativeTheme,
  setWindowGlass,
  setWindowPinned,
  setWindowUiScale,
  startEdgeDock,
  startPositionMemory,
} from "./windowClient";

// macOS 是菜单栏应用：小插件是贴着菜单栏图标的面板（没有窗口按钮、不可拖动、
// 材质由系统 vibrancy 承担），完整视图是独立的标准窗口（原生红绿灯）。
// Windows 仍是"单窗口变形 + 自绘按钮"，两条路径不互相影响。
const IS_MAC = isMacPlatform();

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
    label: "ZCode / GLM",
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
    accent: "#3f74f2",
    iconSrc: kimiAppIcon,
    iconClass: "agent-icon--kimi",
  },
  antigravity: {
    label: "Antigravity",
    accent: "#4d84f0",
    iconSrc: antigravityAppIcon,
    iconClass: "agent-icon--antigravity",
  },
};

const AGENT_ORDER = Object.keys(AGENT_META);

// 胶囊条尺寸：横条一格约 68px 宽；竖条是横条立起来的窄长条，
// 一格约 54px 高（图标/百分比/进度条纵向堆叠）。
// chrome 是留白 + 状态点 + 方向/还原两个按钮（竖条里竖排）。
const STRIP_CELL_WIDTH = 68;
const STRIP_CHROME_WIDTH = 102;
const STRIP_BAR_HEIGHT = 40;
const STRIP_VERTICAL_WIDTH = 52;
const STRIP_VCELL_HEIGHT = 54;
const STRIP_VCHROME_HEIGHT = 110;

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

const AGENT_LABELS = Object.fromEntries(
  AGENT_ORDER.map((id) => [id, AGENT_META[id].label]),
);

// 位数自适应：数值越大小数越少，保证任何量级都不超过 4 个有效字符
// （紧凑态 41px 大字的容器只有约 5 字符宽）。
function scaledUnit(amount, divisor, unit) {
  const value = amount / divisor;
  const decimals = value >= 100 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(decimals).replace(/\.0+$/, "")}${unit}`;
}

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

function shortWindowLabel(key) {
  if (key === "five_hour" || key === "primary") return "5h";
  if (key === "seven_day" || key === "secondary") return "7d";
  if (key === "extra_usage") return "超额";
  return key.replace(/^seven_day_/, "").slice(0, 4);
}

// 小插件配额卡固定两行：优先取来源的前两个窗口，缺则补占位。
function compactQuotaWindows(entry) {
  const placeholders = [
    { key: "five_hour", label: "Session", view: UNAVAILABLE_QUOTA },
    { key: "seven_day", label: "每周", view: UNAVAILABLE_QUOTA },
  ];
  return [0, 1].map((index) => entry.windows?.[index] || placeholders[index]);
}

// 胶囊条一格取"最紧张"窗口：可用窗口里已用百分比最高者。
function stripCellData(entry) {
  const windows = (entry.windows || []).filter(
    (window) => window.view.available && !window.view.resetExpired,
  );
  if (!windows.length) return null;
  const tightest = windows.reduce(
    (worst, window) =>
      quotaUsedPercent(window.view) > quotaUsedPercent(worst.view) ? window : worst,
    windows[0],
  );
  return { tightest, windows };
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
  if (used >= 85) return "warn";
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
  const isSnapshot = view.stale || view.quality === "official_snapshot";
  const severity = quotaSeverity(view);
  const pace = quotaPace(view, windowKey);
  return (
    <>
      <div
        className={`quota-bar-row ${isSnapshot ? "quota-bar-row--stale" : ""} ${severity ? `quota-bar-row--${severity}` : ""}`}
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
            ? `节奏从容 ${Math.abs(pace.delta).toFixed(0)}% · 按当前用量可撑到重置`
            : pace.tone === "close"
              ? `节奏略偏快 ${pace.delta.toFixed(0)}% · 接近临界节奏`
              : `节奏偏快 ${pace.delta.toFixed(0)}% · 按当前用量重置前可能耗尽`}
        </small>
      )}
    </>
  );
}

function Sidebar({ activeNav, onNavChange, snapshot, loading }) {
  const partial = snapshotIsPartial(snapshot);
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

      <button
        className="source-status"
        type="button"
        onClick={() => onNavChange("sources")}
      >
        <span className={`status-dot ${loading ? "status-dot--loading" : ""} ${snapshot.loadError ? "status-dot--error" : ""} ${partial ? "status-dot--warning" : ""}`} />
        <span>{snapshot.pending ? "正在读取本机数据" : snapshot.loadError ? "数据暂不可用" : partial ? "部分覆盖" : snapshot.isDemo ? "演示数据" : "数据可追溯"}</span>
        <small>{snapshot.pending ? "大型日志库可能需要几分钟" : loading ? "正在更新" : snapshot.loadError ? "未使用演示数字" : partial ? "部分记录未解析，点此查看" : `更新于 ${formatClock(snapshot.generatedAt)}`}</small>
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
            <strong>{formatUsd(scopedUsd)}</strong>
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
                <span className="model-name">{entry.model === "unknown" ? "未标注模型" : entry.model}</span>
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
          <p>{pending ? "索引完成后会显示真实曲线。" : "未用零值或演示曲线替代读取失败。"}</p>
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

function Inspector({ snapshot, selectedAgent, onSelectAgent, onOpenSources }) {
  const dataUnavailable = snapshot.pending || snapshot.loadError;
  const partial = snapshotIsPartial(snapshot);
  return (
    <aside className="inspector" aria-label="配额与 Agent 明细">
      <div className="quota-groups" aria-label="各 Agent 官方配额">
        {AGENT_ORDER.map((agentId) => {
          const entry = agentQuotaFor(snapshot, agentId);
          const hasData = quotaHasData(entry);
          // 没有配额来源的可选 Agent 不占版面；Codex 与 Claude 始终显示。
          if (!hasData && !["codex", "claude"].includes(agentId)) return null;
          const provenanceView = entry.windows?.find((window) => window.view.available)?.view;
          return (
            <section className="quota-group" key={agentId}>
              <header>
                <strong>{AGENT_META[agentId].label}</strong>
                <small>
                  {hasData
                    ? quotaProvenance(provenanceView)
                    : agentId === "claude"
                      ? "在设置中开启配额钩子后显示"
                      : "暂无可靠来源"}
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
        {snapshot.agents.map((agent) => {
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
        <span><ShieldCheck size={17} weight="fill" />{snapshot.pending ? "正在读取本机数据" : snapshot.loadError ? "数据暂不可用" : partial ? "部分数据可能不完整" : "数据可追溯"}</span>
        <small>{snapshot.pending ? "后台建立索引，窗口仍可操作" : snapshot.loadError ? "没有用演示数字替代失败结果" : partial ? "打开统计说明查看受影响来源" : snapshot.isDemo ? "当前为演示模式" : `本地统计 + 官方配额 · ${formatClock(snapshot.generatedAt)}`}</small>
      </button>
    </aside>
  );
}

function runWindowAction(action) {
  action().catch((error) => {
    console.warn("Unable to update the desktop window.", error);
  });
}

function WindowActions({ mode, pinned, transparent = false, onToggleMode, onTogglePinned, onToggleTransparent }) {
  return (
    <div className={`window-actions window-actions--${mode}`} aria-label="窗口操作">
      {mode === "expanded" && (
        <button
          type="button"
          className="window-action"
          onClick={() => onToggleMode("compact")}
          aria-label="收起为桌面小插件"
          title="收起为桌面小插件"
        >
          <ArrowsInSimple size={17} weight="light" aria-hidden="true" />
        </button>
      )}
      {mode === "compact" && (
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
      {mode === "compact" && (
        <button
          type="button"
          className={`window-action ${transparent ? "window-action--active" : ""}`}
          onClick={onToggleTransparent}
          aria-label={transparent ? "关闭玻璃材质" : "使用玻璃材质"}
          aria-pressed={transparent}
          title={transparent ? "关闭玻璃材质" : "玻璃材质"}
        >
          <CircleHalfTilt size={16} weight={transparent ? "fill" : "light"} aria-hidden="true" />
        </button>
      )}
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
      <button
        type="button"
        className="window-action"
        onClick={() => runWindowAction(minimizeWindow)}
        aria-label="最小化"
        title="最小化"
      >
        <Minus size={17} weight="light" aria-hidden="true" />
      </button>
      <button
        type="button"
        className="window-action window-action--close"
        onClick={() => runWindowAction(closeWindow)}
        aria-label="隐藏到托盘"
        title="隐藏到托盘"
      >
        <X size={17} weight="light" aria-hidden="true" />
      </button>
    </div>
  );
}

function StripBar({
  snapshot,
  agents,
  pinned,
  loading,
  transparent,
  glassAlpha = 0.82,
  glassMode = "css",
  orientation,
  onToggleOrientation,
  onTogglePinned,
  onRestore,
  availableUpdate,
  onOpenUpdate,
}) {
  // 用户自选的 agent 一律占格；没有官方配额数据的显示 "--"，不伪造数字。
  const cells = agents.map((agentId) => ({
    agentId,
    cell: stripCellData(agentQuotaFor(snapshot, agentId)),
  }));
  const dragProps = pinned ? {} : { "data-tauri-drag-region": true };
  const vertical = orientation === "vertical";
  const OrientationIcon = vertical ? ArrowsLeftRight : ArrowsDownUp;
  return (
    <main
      className={`strip-shell ${vertical ? "strip-shell--vertical" : ""} ${transparent ? "strip-shell--transparent" : ""} ${transparent && glassMode === "css" ? "strip-shell--glass-css" : ""} ${IS_MAC ? "strip-shell--mac" : ""}`}
      {...dragProps}
      style={{
        ...(transparent ? { "--glass-alpha": glassAlpha } : {}),
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
          const isSnapshot = view.stale || view.quality === "official_snapshot";
          return (
            <div
              key={agentId}
              className={`strip-cell ${isSnapshot ? "strip-cell--stale" : ""} ${severity ? `strip-cell--${severity}` : ""}`}
              style={{ "--quota-accent": meta.accent }}
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
                <span className="strip-cell-track" aria-hidden="true">
                  <i
                    style={{
                      transform: `scaleX(${Math.max(0, Math.min(1, view.remainingPercent / 100))})`,
                    }}
                  />
                </span>
              </span>
            </div>
          );
        })
      ) : (
        <span className="strip-empty" {...dragProps}>
          配额不可用
        </span>
      )}
      <div className="strip-controls">
        {availableUpdate && (
          <button
            type="button"
            className="update-dot"
            onClick={onOpenUpdate}
            aria-label={`有新版本 ${availableUpdate.version}，打开设置更新`}
            title={`有新版本 ${availableUpdate.version}，点击更新`}
          />
        )}
        <i
          className={`status-dot ${loading ? "status-dot--loading" : ""} ${snapshot.loadError ? "status-dot--error" : ""}`}
          aria-hidden="true"
        />
        <button
          type="button"
          className={`strip-button ${pinned ? "strip-button--active" : ""}`}
          onClick={onTogglePinned}
          aria-label={pinned ? "取消固定，恢复拖动" : "固定在当前位置并置顶"}
          aria-pressed={pinned}
          title={pinned ? "取消固定，恢复拖动" : "固定在当前位置并置顶"}
        >
          <PushPinSimple size={13} weight={pinned ? "fill" : "light"} aria-hidden="true" />
        </button>
        <button
          type="button"
          className="strip-button"
          onClick={onToggleOrientation}
          aria-label={vertical ? "切换为横条" : "切换为竖条"}
          title={vertical ? "切换为横条" : "切换为竖条"}
        >
          <OrientationIcon size={13} weight="light" aria-hidden="true" />
        </button>
        <button
          type="button"
          className="strip-button"
          onClick={onRestore}
          aria-label="展开为桌面小插件"
          title="展开为桌面小插件"
        >
          <ArrowsOutSimple size={13} weight="light" aria-hidden="true" />
        </button>
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
  onPeriodChange,
  onSelectAgent,
  onOpenSources,
  onTogglePinned,
  onToggleTransparent,
  onExpand,
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
  // 标签必须描述快照本身的周期；切换周期的扫描期间不给旧数据贴新标签。
  const comparisonLabel = snapshot.period === "today" ? "较近 7 日同时段" : "较前一周期";
  const flatComparisonLabel = snapshot.period === "today" ? "与近 7 日同时段持平" : "与前一周期持平";
  const switchingPeriod = !snapshot.pending && !snapshot.loadError && period !== snapshot.period;
  const quotaEntry = agentQuotaFor(snapshot, quotaAgent);
  const quotaWindows = compactQuotaWindows(quotaEntry);
  const quotaView = quotaWindows.find((window) => window.view.available)?.view || UNAVAILABLE_QUOTA;
  const quotaIsSnapshot = quotaView.stale || quotaView.quality === "official_snapshot";
  const dataUnavailable = snapshot.pending || snapshot.loadError;
  const partial = snapshotIsPartial(snapshot);

  return (
    <main
      className={`widget-shell ${transparent ? "widget-shell--transparent" : ""} ${transparent && glassMode === "css" ? "widget-shell--glass-css" : ""} ${IS_MAC ? "widget-shell--mac" : ""} ${loading ? "is-loading" : ""}`}
      style={transparent ? { "--glass-alpha": glassAlpha } : undefined}
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
          <i className={`status-dot ${loading ? "status-dot--loading" : ""} ${snapshot.loadError ? "status-dot--error" : ""}`} aria-hidden="true" />
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
        {!IS_MAC && (
          <WindowActions
            mode="compact"
            pinned={pinned}
            transparent={transparent}
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
            className={`widget-quota ${quotaIsSnapshot ? "widget-quota--stale" : ""}`}
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
                    : quotaAgent === "claude"
                      ? "设置中开启配额钩子"
                      : "官方配额不可用"}
            </small>
          </button>
        </section>

        <section className="widget-agent-list" aria-label="按 Agent 筛选">
          {snapshot.agents.map((agent) => {
            const meta = AGENT_META[agent.id];
            if (!meta) return null;
            // 只展示用户在设置里勾选的 Agent（正被筛选的除外）；完整视图不受影响。
            if (!widgetAgents.includes(agent.id) && selectedAgent !== agent.id) {
              return null;
            }
            const isSelected = selectedAgent === agent.id;
            return (
              <button
                type="button"
                className={`widget-agent ${isSelected ? "widget-agent--selected" : ""}`}
                key={agent.id}
                aria-pressed={isSelected}
                onClick={() => onSelectAgent(isSelected ? "all" : agent.id)}
              >
                <i className="widget-agent-accent" style={{ backgroundColor: meta.accent }} aria-hidden="true" />
                <AgentMark agentId={agent.id} />
                <span>
                  <strong>{meta.label}</strong>
                  <small>{snapshot.pending || snapshot.loadError ? "--" : compactTokens(agent.tokens)} tokens</small>
                </span>
                <em>{dataUnavailable ? "--" : `${agent.share.toFixed(1)}%`}</em>
              </button>
            );
          })}
        </section>

        <footer className="widget-footer">
          <button type="button" className={`widget-source ${snapshot.loadError ? "widget-source--error" : ""} ${partial ? "widget-source--warning" : ""}`} onClick={onOpenSources} aria-live="polite">
            <ShieldCheck size={15} weight="fill" aria-hidden="true" />
            <span>{snapshot.pending ? "正在读取" : snapshot.loadError ? "数据暂不可用" : partial ? "部分覆盖" : snapshot.isDemo ? "演示数据" : "数据可追溯"}</span>
            <small>{snapshot.pending ? "请稍候" : loading ? "更新中" : snapshot.loadError ? "未替换" : partial ? "查看说明" : formatClock(snapshot.generatedAt)}</small>
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
          <div>
            <span className="eyebrow">统计说明</span>
            <h2 id="source-title">每个数字都有出处</h2>
          </div>
          <button ref={closeButtonRef} type="button" className="icon-button" onClick={onClose} aria-label="关闭">
            <X size={21} weight="light" />
          </button>
        </header>

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
          ? "钩子已安装。下次 Claude Code 刷新状态栏后，这里就会出现官方 5 小时与 7 天额度。"
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
        Claude Code 本身会把官方 5 小时 / 7 天额度推送给状态栏脚本。开启后 Metrik 安装一个只提取
        额度数字的 statusLine 钩子（不读取对话内容、不接触登录凭据）。已有自定义 statusLine
        时会自动串联：原有显示原样保留，行尾追加 5h/7d 额度；卸载时原样恢复。
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
        备选来源：使用本机 Claude Code 已保存的登录凭据直接查询官方额度接口。凭据只在本机内存中读取，
        不存储、不上传、不写日志；额度是 Claude 全产品合并值（含网页版）。该接口为 Claude Code
        客户端自用接口，若失效将显示为不可用并自动回落到状态栏钩子。
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
                    : status.enabled
                      ? "已开启 · 凭据可用"
                      : "未开启 · 凭据可用"}
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
        小组件的摆放位置会被记住，下次启动回到原处（拖到屏幕外或拔掉扩展屏时自动居中）。
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
  useEffect(() => {
    if (!availableUpdate) return;
    setState((current) =>
      current.status === "idle" || current.status === "current"
        ? { status: "available", ...availableUpdate }
        : current,
    );
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
        当前版本 {__APP_VERSION__}。每天自动检查一次新版本（仅一次网络请求，可关闭），
        发现后在小组件上以小圆点提示；下载与安装始终由你点击确认。更新包经签名校验，
        签名不符会拒绝安装。
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

const THEME_OPTIONS = [
  { id: "auto", label: "自动" },
  { id: "light", label: "亮色" },
  { id: "dark", label: "暗色" },
];

function ThemeCard({ theme, onThemeChange }) {
  return (
    <div className="settings-card">
      <h2>完整视图外观</h2>
      <p className="settings-muted">
        选择大窗口的明暗主题。“自动”跟随系统外观。桌面小插件不受此设置影响。
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
    </div>
  );
}

function GlassAlphaCard({ glassAlpha, onGlassAlpha }) {
  const percent = Math.round(glassAlpha * 100);
  return (
    <div className="settings-card">
      <h2>小组件玻璃浓度</h2>
      <p className="settings-muted">
        越低越透（透明感依赖系统模糊），越高越实。系统模糊不可用时自动锁定近实心，不受此值影响。
      </p>
      <div className="glass-slider-row">
        <input
          type="range"
          min="60"
          max="96"
          step="2"
          value={percent}
          aria-label="玻璃浓度百分比"
          onChange={(event) => onGlassAlpha(Number(event.target.value) / 100)}
        />
        <em>{percent}%</em>
      </div>
    </div>
  );
}

const UI_SCALE_OPTIONS = [
  { value: 1, label: "100%" },
  { value: 1.25, label: "125%" },
  { value: 1.5, label: "150%" },
];

function UiScaleCard({ uiScale, onUiScale }) {
  return (
    <div className="settings-card">
      <h2>小组件缩放</h2>
      <p className="settings-muted">
        整体放大桌面小插件与胶囊条（窗口和内容等比缩放，不会变形）。完整视图不受此设置影响。
      </p>
      <div className="theme-toggle" role="group" aria-label="小组件缩放档位">
        {UI_SCALE_OPTIONS.map((option) => (
          <button
            key={option.value}
            type="button"
            className={uiScale === option.value ? "is-selected" : ""}
            aria-pressed={uiScale === option.value}
            onClick={() => onUiScale(option.value)}
          >
            {option.label}
          </button>
        ))}
      </div>
    </div>
  );
}

function WidgetAgentsCard({ widgetAgents, onToggleWidgetAgent }) {
  return (
    <div className="settings-card">
      <h2>小组件显示的 Agent</h2>
      <p className="settings-muted">
        选择桌面小插件里展示哪些 Agent，可单选也可多选（至少保留一个）。完整视图始终展示全部。
      </p>
      <ul className="settings-agent-toggle">
        {AGENT_ORDER.map((agentId) => {
          const checked = widgetAgents.includes(agentId);
          return (
            <li key={agentId}>
              <label>
                <input
                  type="checkbox"
                  checked={checked}
                  disabled={checked && widgetAgents.length === 1}
                  onChange={() => onToggleWidgetAgent(agentId)}
                />
                <AgentMark agentId={agentId} />
                <span>{AGENT_META[agentId].label}</span>
              </label>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

function StripAgentsCard({ stripAgents, onToggleStripAgent, onMoveStripAgent }) {
  // 已选的按显示顺序排前面，未选的按默认顺序垫后。
  const rows = [
    ...stripAgents,
    ...AGENT_ORDER.filter((agentId) => !stripAgents.includes(agentId)),
  ];
  return (
    <div className="settings-card">
      <h2>胶囊条显示的 Agent</h2>
      <p className="settings-muted">
        选择胶囊条里展示哪些 Agent（至少保留一个）；勾选顺序就是显示顺序，↑ 可上移。
        没有官方配额来源的 Agent 会以 "--" 占格显示。
      </p>
      <ul className="settings-agent-toggle">
        {rows.map((agentId) => {
          const index = stripAgents.indexOf(agentId);
          const checked = index >= 0;
          return (
            <li key={agentId}>
              <label>
                <input
                  type="checkbox"
                  checked={checked}
                  disabled={checked && stripAgents.length === 1}
                  onChange={() => onToggleStripAgent(agentId)}
                />
                <AgentMark agentId={agentId} />
                <span>{AGENT_META[agentId].label}</span>
              </label>
              {checked && (
                <button
                  type="button"
                  className="settings-agent-move"
                  onClick={() => onMoveStripAgent(agentId)}
                  disabled={index === 0}
                  aria-label={`将 ${AGENT_META[agentId].label} 上移`}
                  title="上移"
                >
                  ↑
                </button>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}

function SettingsSection({ onSnapshotRefresh, widgetAgents, onToggleWidgetAgent, stripAgents, onToggleStripAgent, onMoveStripAgent, glassAlpha, onGlassAlpha, uiScale, onUiScale, theme, onThemeChange, autoUpdateCheck, onAutoUpdateCheck, availableUpdate }) {
  const [settings, setSettings] = useState(null);
  const [directoryInput, setDirectoryInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState(null);

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

  return (
    <main className="settings-section" aria-labelledby="settings-title">
      <header className="settings-header">
        <span className="section-kicker">设置</span>
        <h1 id="settings-title">多设备同步</h1>
        <p>
          把多台电脑的 Metrik 指向同一个共享文件夹（坚果云、OneDrive、Syncthing 等同步盘均可）。
          每台设备导出近 30 天的统计事件并自动合并其他设备的导出；
          导出只含事件标识、Agent、时间与 token 数，不含对话内容、Prompt 或凭据。
        </p>
      </header>

      {settings?.demo && (
        <p className="settings-demo-note">浏览器演示模式：同步配置仅在桌面应用中可用。</p>
      )}

      <div className="settings-grid">
      <ThemeCard theme={theme} onThemeChange={onThemeChange} />

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
      </div>

      <StartupCard
        autoUpdateCheck={autoUpdateCheck}
        onAutoUpdateCheck={onAutoUpdateCheck}
        availableUpdate={availableUpdate}
      />

      <WidgetAgentsCard widgetAgents={widgetAgents} onToggleWidgetAgent={onToggleWidgetAgent} />

      <StripAgentsCard
        stripAgents={stripAgents}
        onToggleStripAgent={onToggleStripAgent}
        onMoveStripAgent={onMoveStripAgent}
      />

      <GlassAlphaCard glassAlpha={glassAlpha} onGlassAlpha={onGlassAlpha} />

      <UiScaleCard uiScale={uiScale} onUiScale={onUiScale} />

      <ClaudeHookCard onSnapshotRefresh={onSnapshotRefresh} />

      {settings?.enabled && (
        <div className="settings-card">
          <h2>已发现的设备</h2>
          {settings.devices.length === 0 ? (
            <p className="settings-muted">尚未发现其他设备的导出文件。另一台电脑指向同一文件夹后会出现在这里。</p>
          ) : (
            <ul className="settings-device-list">
              {settings.devices.map((device) => (
                <li key={device.id}>
                  <strong>{device.label}</strong>
                  <span>{device.id}</span>
                  <small>{device.events} 条事件 · 导出于 {formatSyncTime(device.exportedAtMs)}</small>
                </li>
              ))}
            </ul>
          )}
        </div>
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
  const header = ["date", "start", "end", "agent", "model", "tokens", "input_uncached", "cache_read", "cache_write", "output", "estimated_usd", "events", "session_id"];
  const rows = sessions.map((session) => [
    new Date(session.endMs).toLocaleDateString("sv-SE"),
    new Date(session.startMs).toLocaleTimeString("zh-CN", { hour12: false }),
    new Date(session.endMs).toLocaleTimeString("zh-CN", { hour12: false }),
    session.agent,
    session.model || "",
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

async function exportSessionsCsv(sessions) {
  const csv = buildSessionsCsv(sessions);
  const fileName = `metrik-sessions-${new Date().toLocaleDateString("sv-SE")}.csv`;
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

function UsageSection({ sessionsState, period }) {
  const [agentFilter, setAgentFilter] = useState("all");
  const [modelFilter, setModelFilter] = useState("all");
  const [copiedId, setCopiedId] = useState(null);
  const [exportNote, setExportNote] = useState(null);
  const handleExport = async (sessions) => {
    try {
      const savedPath = await exportSessionsCsv(sessions);
      setExportNote(savedPath ? `已导出到 ${savedPath}` : "已开始下载");
    } catch (error) {
      setExportNote(`导出失败：${error}`);
    }
  };
  const copySessionId = (sessionId) => {
    navigator.clipboard?.writeText(sessionId).then(() => {
      setCopiedId(sessionId);
      setTimeout(() => setCopiedId((current) => (current === sessionId ? null : current)), 1400);
    }).catch(() => {});
  };

  if (!sessionsState || sessionsState.status === "loading") {
    return (
      <main className="usage-section" aria-busy="true">
        <header className="settings-header">
          <span className="section-kicker">用量</span>
          <h1>正在读取会话明细</h1>
          <p>只读取已索引的账本，不触发新的日志扫描。</p>
        </header>
      </main>
    );
  }
  const data = sessionsState.data;
  if (!data || data.loadError) {
    return (
      <main className="usage-section">
        <header className="settings-header">
          <span className="section-kicker">用量</span>
          <h1>会话明细暂不可用</h1>
          <p>本地账本读取失败；没有用演示数字替代。请稍后重试。</p>
        </header>
      </main>
    );
  }

  const models = [...new Set(data.sessions.map((session) => session.model).filter(Boolean))];
  const filtered = data.sessions.filter((session) =>
    (agentFilter === "all" || session.agent === agentFilter)
    && (modelFilter === "all" || session.model === modelFilter));
  const groups = [];
  filtered.forEach((session) => {
    const label = sessionDayLabel(session.endMs);
    const group = groups[groups.length - 1];
    if (group && group.label === label) group.sessions.push(session);
    else groups.push({ label, sessions: [session] });
  });
  const timeRange = (session) => {
    const fmt = (ms) => new Date(ms).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit", hour12: false });
    return `${fmt(session.startMs)}–${fmt(session.endMs)}`;
  };

  return (
    <main className="usage-section" aria-labelledby="usage-title">
      <header className="settings-header">
        <span className="section-kicker">用量</span>
        <h1 id="usage-title">会话明细</h1>
        <p>
          {PERIODS.find((item) => item.id === period)?.label}内 {data.totalSessions} 个会话
          {data.truncated ? "（仅显示最近 300 个）" : ""}。成本为按公开 API 价格的估算，非账单。
          {data.isDemo ? " 当前为浏览器演示数据。" : ""}
        </p>
      </header>

      <div className="usage-toolbar">
        <select value={agentFilter} onChange={(event) => setAgentFilter(event.target.value)} aria-label="按 Agent 筛选">
          <option value="all">全部 Agent</option>
          {AGENT_ORDER.map((id) => <option key={id} value={id}>{AGENT_META[id].label}</option>)}
        </select>
        <select value={modelFilter} onChange={(event) => setModelFilter(event.target.value)} aria-label="按模型筛选">
          <option value="all">全部模型</option>
          {models.map((model) => <option key={model} value={model}>{model}</option>)}
        </select>
        <button type="button" className="ledger-button" disabled={!filtered.length} onClick={() => handleExport(filtered)}>
          导出 CSV（{filtered.length}）
        </button>
      </div>

      {exportNote && <p className="settings-muted" role="status">{exportNote}</p>}

      {groups.length === 0 && (
        <p className="settings-muted">当前筛选条件下没有会话。</p>
      )}

      {groups.map((group) => (
        <section className="session-group" key={group.label} aria-label={group.label}>
          <h2>{group.label}</h2>
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
                  title={`复制会话 ID（可用于 resume 等操作）\n${session.sessionId}`}
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
      ))}
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

// 按周（周一起始）汇总每 Agent 的 token，用于趋势折线。
function weeklySeries(days) {
  const weeks = new Map();
  days.forEach((day) => {
    const date = new Date(`${day.date}T00:00:00`);
    const monday = new Date(date);
    monday.setDate(date.getDate() - ((date.getDay() + 6) % 7));
    const key = dateKey(monday);
    const bucket = weeks.get(key) || { label: key, byAgent: {} };
    Object.entries(day.byAgent || {}).forEach(([id, value]) => {
      bucket.byAgent[id] = (bucket.byAgent[id] || 0) + Number(value || 0);
    });
    weeks.set(key, bucket);
  });
  return [...weeks.values()].sort((a, b) => (a.label < b.label ? -1 : 1));
}

// 图表专用降饱和配色：品牌色直接上图会显得"纯"，
// 苹果式做法是柔和一档的同源色 + 平滑曲线 + 低透明面积。
const CHART_LINE_COLORS = {
  codex: "#5586d4",
  claude: "#d98663",
  zcode: "#8b80d9",
  opencode: "#4aa392",
  kimi: "#6f8fd6",
  antigravity: "#6b8fe4",
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

function ReportTrendChart({ days }) {
  const weeks = weeklySeries(days);
  const agents = AGENT_ORDER.filter((id) => weeks.some((week) => (week.byAgent[id] || 0) > 0));
  const max = Math.max(1, ...weeks.flatMap((week) => agents.map((id) => week.byAgent[id] || 0)));
  const width = 620;
  const height = 210;
  const pad = { top: 12, right: 8, bottom: 22, left: 8 };
  const x = (index) => pad.left + (index / Math.max(1, weeks.length - 1)) * (width - pad.left - pad.right);
  const y = (value) => height - pad.bottom - (value / max) * (height - pad.top - pad.bottom);
  const linePoints = (id) => weeks.map((week, index) => [x(index), y(week.byAgent[id] || 0)]);
  const baseline = height - pad.bottom;

  return (
    <div>
      <svg
        className="report-trend"
        viewBox={`0 0 ${width} ${height}`}
        role="img"
        aria-label="近 26 周每周 token 用量趋势，按 Agent 分色"
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
        <text x={pad.left} y={pad.top + 2} className="trend-label">{compactTokens(max)}</text>
        <text x={pad.left} y={height - 6} className="trend-label">{weeks[0]?.label}</text>
        <text x={width - pad.right} y={height - 6} className="trend-label" textAnchor="end">{weeks[weeks.length - 1]?.label}</text>
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
      </svg>
      <div className="chart-legend chart-legend--report" aria-label="图例">
        {agents.map((id) => (
          <span key={id}><i className="legend-line" style={{ background: chartColor(id) }} />{AGENT_META[id]?.label || id}</span>
        ))}
      </div>
    </div>
  );
}

function ReportShareDonut({ agents, totalTokens }) {
  const rows = agents.filter((agent) => agent.tokens > 0);
  const total = rows.reduce((sum, agent) => sum + agent.tokens, 0) || 1;
  const radius = 74;
  const circumference = 2 * Math.PI * radius;
  let offset = 0;
  return (
    <div className="report-donut">
      <svg viewBox="0 0 200 200" role="img" aria-label="26 周内各 Agent 用量占比环形图">
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
        <text x="100" y="96" textAnchor="middle" className="donut-total">{compactTokens(totalTokens)}</text>
        <text x="100" y="114" textAnchor="middle" className="donut-caption">tokens · 26 周</text>
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
];

function ReportsSection({ report }) {
  const [view, setView] = useState("heatmap");
  if (!report || report.status === "loading") {
    return (
      <main className="reports-section" aria-busy="true">
        <header className="settings-header">
          <span className="section-kicker">报告</span>
          <h1>正在读取本地账本</h1>
          <p>报告只统计已索引的数据，不触发新的日志扫描。</p>
        </header>
      </main>
    );
  }
  const data = report.data;
  if (!data || data.loadError) {
    return (
      <main className="reports-section">
        <header className="settings-header">
          <span className="section-kicker">报告</span>
          <h1>报告暂不可用</h1>
          <p>本地账本读取失败；没有用演示数字替代。请稍后重试。</p>
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

  return (
    <main className="reports-section" aria-labelledby="reports-title">
      <header className="settings-header">
        <span className="section-kicker">报告</span>
        <h1 id="reports-title">近 26 周活动</h1>
        <p>
          只统计本地账本中已索引的数据（processed token 口径，非账单）。
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
        {/* 固定高度：三种视图内容高度不同，卡片会随切换忽大忽小。 */}
        <div className="report-view-body">
        {view === "trend" ? (
          <ReportTrendChart days={data.days} />
        ) : view === "share" ? (
          <ReportShareDonut agents={data.agents} totalTokens={data.totalTokens} />
        ) : (
          <>
        <div className="heatmap-months" aria-hidden="true">
          {monthLabels.map((month) => (
            <span key={month.index} style={{ gridColumnStart: month.index + 1 }}>{month.label}</span>
          ))}
        </div>
        <div className="heatmap" role="img" aria-label="近 26 周每日 token 用量热力图，颜色越深用量越大">
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
            {data.agents.filter((agent) => agent.tokens > 0).map((agent) => {
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
                  <span className="model-name">{entry.model === "unknown" ? "未标注模型" : entry.model}</span>
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
      <p>这部分会在统计内核稳定后展开，首版先把概览和数据可信度做好。</p>
      <button type="button" onClick={onReturn}>返回概览</button>
    </main>
  );
}

function initialWindowMode() {
  if (typeof window === "undefined") return "compact";
  if (new URLSearchParams(window.location.search).get("view") === "expanded") return "expanded";
  // 上次收成胶囊条则恢复；expanded 不恢复。
  return localStorage.getItem("metrik:viewMode") === "strip" ? "strip" : "compact";
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
  const [activeNav, setActiveNav] = useState(initialNav);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [pinned, setPinned] = useState(() => localStorage.getItem("metrik:pinned") === "true");
  // 玻璃材质默认开启；用户关闭后记住选择。
  // macOS 上材质由系统 vibrancy 承担，恒开，没有开关按钮。
  const [transparent, setTransparent] = useState(
    () => IS_MAC || (localStorage.getItem("metrik:transparent") ?? "true") === "true",
  );
  const [quotaAgent, setQuotaAgent] = useState(
    () => localStorage.getItem("metrik:quotaAgent") || "codex",
  );
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
        const valid = stored.filter((id) => AGENT_ORDER.includes(id));
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
    return Number.isFinite(stored) && stored >= 0.6 && stored <= 0.96 ? stored : 0.82;
  });
  const handleGlassAlpha = useCallback((next) => {
    setGlassAlpha(next);
    localStorage.setItem("metrik:glassAlpha", String(next));
  }, []);
  // 卡片/胶囊的整体缩放档位：窗口尺寸与 WebView 原生 zoom 同乘一个系数，
  // 等比放大不会变形；expanded 不参与。生效在 windowClient 的形态切换里，
  // 设置页改档后下次回到卡片/胶囊时应用。
  const [uiScale, setUiScale] = useState(() => {
    const stored = Number(localStorage.getItem("metrik:uiScale"));
    return UI_SCALE_OPTIONS.some((option) => option.value === stored) ? stored : 1;
  });
  const handleUiScale = useCallback((next) => {
    setUiScale(next);
    localStorage.setItem("metrik:uiScale", String(next));
    setWindowUiScale(next);
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
    // 错开启动扫描的高峰再查；之后每天一次。
    const startTimer = window.setTimeout(check, 15000);
    const interval = window.setInterval(check, 24 * 60 * 60 * 1000);
    return () => {
      cancelled = true;
      window.clearTimeout(startTimer);
      window.clearInterval(interval);
    };
  }, [autoUpdateCheck]);
  const handleToggleWidgetAgent = useCallback((agentId) => {
    setWidgetAgents((current) => {
      const next = current.includes(agentId)
        ? current.filter((id) => id !== agentId)
        : AGENT_ORDER.filter((id) => current.includes(id) || id === agentId);
      if (!next.length) return current; // 至少保留一个
      localStorage.setItem("metrik:widgetAgents", JSON.stringify(next));
      return next;
    });
  }, []);
  const [loading, setLoading] = useState(true);
  const [rebuildState, setRebuildState] = useState({ status: "idle", message: "" });
  const [report, setReport] = useState(null);
  const [sessionsState, setSessionsState] = useState(null);
  const [snapshot, setSnapshot] = useState(() => getUsageSnapshot.initial("today"));
  // 历史索引还没补齐：账本尚未覆盖完整周期，数字必须显式标注为不完整。
  const indexingPending = snapshot.indexing?.pending || 0;
  const indexing = indexingPending > 0;
  const requestSequence = useRef(0);
  const loadInFlight = useRef(false);
  const activeLoadPeriod = useRef(null);
  const queuedLoadPeriod = useRef(null);
  const currentPeriod = useRef(period);
  const rebuildInFlight = useRef(false);
  currentPeriod.current = period;

  const loadSnapshot = useCallback(async (nextPeriod) => {
    if (loadInFlight.current) {
      queuedLoadPeriod.current = activeLoadPeriod.current === nextPeriod ? null : nextPeriod;
      return;
    }

    loadInFlight.current = true;
    let periodToLoad = nextPeriod;
    try {
      while (periodToLoad) {
        activeLoadPeriod.current = periodToLoad;
        queuedLoadPeriod.current = null;
        const requestId = ++requestSequence.current;
        setLoading(true);
        const next = await getUsageSnapshot(periodToLoad);
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
    loadSnapshot(period);
  }, [period, loadSnapshot]);

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
  }, [loadSnapshot, period, viewMode, indexing]);

  useEffect(() => {
    // macOS 面板由系统管层级和位置：不置顶、不恢复坐标。
    if (IS_MAC) return;
    if (pinned) runWindowAction(() => setWindowPinned(true));
    // 小组件回到上次摆放的位置（含固定位置），坐标已不在任何屏幕上时居中。
    // strip 形态的启动定位在 strip 专属 effect 里做。
    if (viewMode === "compact") {
      runWindowAction(() => restoreWindowPosition("compact"));
      // 启动时窗口尺寸来自 tauri.conf.json（未缩放），就地应用缩放档位。
      runWindowAction(() => applyStartupUiScale("compact"));
    }
  }, []);

  const pinnedRef = useRef(pinned);
  pinnedRef.current = pinned;
  const viewModeRef = useRef(viewMode);
  viewModeRef.current = viewMode;

  // 拖动后记住小组件位置，供下次启动恢复。
  useEffect(() => {
    const stopPromise = startPositionMemory(() => viewModeRef.current);
    return () => {
      stopPromise.then((stop) => stop?.());
    };
  }, []);

  // 玻璃只作用于小插件形态；系统明暗主题切换时重发对应 tint。
  // 原生材质不可用（或非桌面环境）时回落到 CSS 玻璃拟态承担外观。
  const [glassMode, setGlassMode] = useState("css");
  useEffect(() => {
    let cancelled = false;
    const apply = () => {
      setWindowGlass(transparent && viewMode !== "expanded")
        .then((mode) => {
          if (!cancelled) setGlassMode(mode);
        })
        .catch((error) => {
          console.warn("Unable to update the desktop window.", error);
          if (!cancelled) setGlassMode(transparent ? "css" : "off");
        });
    };
    apply();
    const media = window.matchMedia?.("(prefers-color-scheme: dark)");
    media?.addEventListener?.("change", apply);
    return () => {
      cancelled = true;
      media?.removeEventListener?.("change", apply);
    };
  }, [transparent, viewMode]);

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

  // 配额卡可在有官方数据的 Agent 间循环；没有任何数据时保底显示 Codex。
  const quotaAgents = useMemo(() => {
    const withData = (snapshot.agentQuotas || [])
      .filter(quotaHasData)
      .map((entry) => entry.agent)
      .filter((agent) => AGENT_META[agent]);
    return withData.length ? withData : ["codex"];
  }, [snapshot]);
  const activeQuotaAgent = quotaAgents.includes(quotaAgent) ? quotaAgent : quotaAgents[0];
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
        const valid = stored.filter((id) => AGENT_ORDER.includes(id));
        if (valid.length) return valid;
      }
    } catch {
      // 本地设置损坏时回到自动模式。
    }
    return null;
  });
  const stripAgents = stripAgentsSetting ?? autoStripAgents;
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
  // 进入 strip 时整窗变形一次（含启动恢复）；之后 agent 格数变化只调宽度。
  const stripApplied = useRef(false);
  useEffect(() => {
    if (IS_MAC) return;
    if (viewMode !== "strip") {
      stripApplied.current = false;
      return;
    }
    const size = stripWindowSize(stripOrientation, stripAgents.length);
    if (stripApplied.current) {
      runWindowAction(() => resizeStripWindow(size));
    } else {
      stripApplied.current = true;
      runWindowAction(() => applyWindowMode("strip", size));
    }
  }, [viewMode, stripAgents.length, stripOrientation]);
  const handleCycleQuotaAgent = useCallback(() => {
    const index = quotaAgents.indexOf(activeQuotaAgent);
    const next = quotaAgents[(index + 1) % quotaAgents.length];
    setQuotaAgent(next);
    localStorage.setItem("metrik:quotaAgent", next);
  }, [activeQuotaAgent, quotaAgents]);
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

  // 会话明细同样只读账本；随周期切换重载。
  useEffect(() => {
    if (activeNav !== "usage" || viewMode !== "expanded") return;
    let cancelled = false;
    setSessionsState({ status: "loading", data: null });
    getUsageSessions(period).then((data) => {
      if (!cancelled) setSessionsState({ status: "ready", data });
    });
    return () => {
      cancelled = true;
    };
  }, [activeNav, viewMode, period]);

  const handleWindowMode = useCallback((nextMode) => {
    // macOS：完整视图是另一个窗口，面板只负责把它开出来，自己保持原样。
    if (IS_MAC) {
      if (nextMode === "expanded") runWindowAction(() => openExpandedWindow());
      return;
    }
    setViewMode(nextMode);
    if (nextMode === "compact") setActiveNav("overview");
    if (nextMode !== "expanded") localStorage.setItem("metrik:viewMode", nextMode);
    // strip 的变形由 strip 专属 effect 统一处理（含启动恢复与格数变宽）。
    if (nextMode !== "strip") runWindowAction(() => applyWindowMode(nextMode));
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

  const handleTogglePinned = useCallback(() => {
    setPinned((current) => {
      const next = !current;
      localStorage.setItem("metrik:pinned", String(next));
      runWindowAction(() => setWindowPinned(next));
      return next;
    });
  }, []);

  const handleToggleTransparent = useCallback(() => {
    setTransparent((current) => {
      const next = !current;
      localStorage.setItem("metrik:transparent", String(next));
      return next;
    });
  }, []);

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

  if (viewMode === "strip") {
    return (
      <StripBar
        snapshot={snapshot}
        agents={stripAgents}
        pinned={pinned}
        loading={appBusy}
        transparent={transparent}
        glassAlpha={glassAlpha}
        glassMode={glassMode}
        orientation={stripOrientation}
        onToggleOrientation={handleToggleStripOrientation}
        onTogglePinned={handleTogglePinned}
        onRestore={() => handleWindowMode("compact")}
        availableUpdate={availableUpdate}
        onOpenUpdate={handleOpenUpdate}
      />
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
          onPeriodChange={setPeriod}
          onSelectAgent={setSelectedAgent}
          onOpenSources={() => setDrawerOpen(true)}
          onTogglePinned={handleTogglePinned}
          onToggleTransparent={handleToggleTransparent}
          onExpand={handleWindowMode}
          quotaAgent={activeQuotaAgent}
          onCycleQuotaAgent={handleCycleQuotaAgent}
          widgetAgents={widgetAgents}
          glassAlpha={glassAlpha}
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
              onToggleMode={handleWindowMode}
              onTogglePinned={handleTogglePinned}
            />
          </>
        )}
        <Sidebar activeNav={activeNav} onNavChange={handleNavChange} snapshot={snapshot} loading={appBusy} />

        {indexingPending > 0 ? (
          <div className="indexing-banner" role="status">
            <ClockCounterClockwise size={18} weight="light" aria-hidden="true" />
            正在补齐历史索引，还剩 <strong>{indexingPending}</strong> 个日志文件。历史周期的数字尚不完整，会随补齐自动更新。
          </div>
        ) : null}

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
              />
            </div>
          </>
        ) : activeNav === "settings" ? (
          <SettingsSection
            onSnapshotRefresh={() => loadSnapshot(currentPeriod.current)}
            widgetAgents={widgetAgents}
            onToggleWidgetAgent={handleToggleWidgetAgent}
            stripAgents={stripAgents}
            onToggleStripAgent={handleToggleStripAgent}
            onMoveStripAgent={handleMoveStripAgent}
            glassAlpha={glassAlpha}
            onGlassAlpha={handleGlassAlpha}
            uiScale={uiScale}
            onUiScale={handleUiScale}
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
            <UsageSection sessionsState={sessionsState} period={period} />
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
