import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowDown,
  ArrowUp,
  ArrowsInSimple,
  ArrowsOutSimple,
  CaretRight,
  ChartBar,
  ChartLineUp,
  CircleHalfTilt,
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
import chatgptAppIcon from "./assets/chatgpt-app-icon.png";
import claudeAppIcon from "./assets/claude-app-icon.jpg";
import zcodeAppIcon from "./assets/zcode-app-icon.png";
import {
  configureSync,
  getClaudeHookStatus,
  getSyncSettings,
  getUsageSnapshot,
  rebuildLocalLedger,
  setClaudeHook,
} from "./usageClient";
import {
  applyWindowMode,
  closeWindow,
  minimizeWindow,
  setWindowGlass,
  setWindowPinned,
  startEdgeDock,
} from "./windowClient";

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
    label: "Claude Code",
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
    monogram: "OC",
    iconClass: "agent-icon--opencode",
  },
};

const AGENT_ORDER = Object.keys(AGENT_META);

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

function quotaUsedPercent(view) {
  return Math.min(100, Math.max(0, 100 - view.remainingPercent));
}

function QuotaBarRow({ label, view }) {
  const isSnapshot = view.stale || view.quality === "official_snapshot";
  return (
    <div className={`quota-bar-row ${isSnapshot ? "quota-bar-row--stale" : ""}`}>
      <small>{label}</small>
      <div className="quota-bar-track" aria-hidden="true">
        <i style={{ transform: `scaleX(${view.available ? quotaUsedPercent(view) / 100 : 0})` }} />
      </div>
      <em>{view.available ? `已用 ${Math.round(quotaUsedPercent(view))}%` : "--"}</em>
      <span>
        {view.resetExpired
          ? "已重置，等待刷新"
          : view.available
            ? `${formatReset(view.resetsInMinutes)}后重置`
            : "暂不可用"}
      </span>
    </div>
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

function PeriodControl({ period, onChange, compact = false }) {
  return (
    <div className={`period-control ${compact ? "period-control--compact" : ""}`} role="group" aria-label="统计周期">
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

function UsageChart({ snapshot, selectedAgent }) {
  const visibleAgents = selectedAgent === "all" ? AGENT_ORDER : [selectedAgent];
  const activeLabel = selectedAgent === "all"
    ? PERIODS.find((item) => item.id === snapshot.period)?.label || "今日"
    : AGENT_META[selectedAgent].label;
  const legendClass = selectedAgent === "all"
    ? "legend-line--codex"
    : `legend-line--${selectedAgent}`;

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
            formatTokens={exactTokens}
          />
        </Suspense>
      </div>
      <div className="chart-legend" aria-label="图例">
        <span><i className={`legend-line ${legendClass}`} />{activeLabel}</span>
      </div>
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
                  <QuotaBarRow key={window.key} label={window.label} view={window.view} />
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
        aria-label={pinned ? "取消固定在最前" : "固定在最前"}
        aria-pressed={pinned}
        title={pinned ? "取消固定在最前" : "固定在最前"}
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

function CompactWidget({
  snapshot,
  period,
  selectedAgent,
  visibleTokens,
  loading,
  pinned,
  transparent,
  onPeriodChange,
  onSelectAgent,
  onOpenSources,
  onTogglePinned,
  onToggleTransparent,
  onExpand,
  quotaAgent,
  onCycleQuotaAgent,
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
    <main className={`widget-shell ${transparent ? "widget-shell--transparent" : ""} ${loading ? "is-loading" : ""}`}>
      <h1 className="sr-only">Metrik Agent 用量桌面小插件</h1>
      <header className="widget-titlebar" data-tauri-drag-region>
        <div className="widget-brand" data-tauri-drag-region>
          <span>Metrik</span>
          <i className={`status-dot ${loading ? "status-dot--loading" : ""} ${snapshot.loadError ? "status-dot--error" : ""}`} aria-hidden="true" />
        </div>
        <WindowActions
          mode="compact"
          pinned={pinned}
          transparent={transparent}
          onToggleMode={onExpand}
          onTogglePinned={onTogglePinned}
          onToggleTransparent={onToggleTransparent}
        />
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
            type="button"
            onClick={onCycleQuotaAgent}
            aria-label={`${AGENT_META[quotaAgent].label} 配额，点击切换 Agent`}
            title="点击切换配额 Agent"
          >
            <span>{AGENT_META[quotaAgent].label} 已用</span>
            {quotaWindows.map((window) => (
              <div className="widget-quota-window" key={window.key}>
                <small>{shortWindowLabel(window.key)}</small>
                <div className="widget-quota-track" aria-hidden="true">
                  <i style={{ transform: `scaleX(${window.view.available ? quotaUsedPercent(window.view) / 100 : 0})` }} />
                </div>
                <em>{window.view.available ? `${Math.round(quotaUsedPercent(window.view))}%` : "--"}</em>
              </div>
            ))}
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
            // 小插件空间有限：可选 Agent 没有用量时不占一行，完整视图仍会显示。
            if (!["codex", "claude"].includes(agent.id) && !agent.tokens && selectedAgent !== agent.id) {
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
        额度数字的 statusLine 钩子（不读取对话内容、不接触登录凭据），并顺带在 Claude Code
        里显示一行简洁的额度状态栏。已有自定义 statusLine 时不会覆盖。
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
                  ? status.lastDataAtMs
                    ? `已安装 · 最近数据 ${formatSyncTime(status.lastDataAtMs)}`
                    : "已安装 · 等待 Claude Code 下次刷新状态栏"
                  : status.conflict
                    ? "未安装 · 检测到已有其他 statusLine，为避免覆盖已禁用安装"
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
    </div>
  );
}

function SettingsSection({ onSnapshotRefresh }) {
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
  return new URLSearchParams(window.location.search).get("view") === "expanded"
    ? "expanded"
    : "compact";
}

export function App() {
  const [viewMode, setViewMode] = useState(initialWindowMode);
  const [period, setPeriod] = useState("today");
  const [selectedAgent, setSelectedAgent] = useState("all");
  const [activeNav, setActiveNav] = useState("overview");
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [pinned, setPinned] = useState(() => localStorage.getItem("metrik:pinned") === "true");
  // 玻璃材质默认开启；用户关闭后记住选择。
  const [transparent, setTransparent] = useState(
    () => (localStorage.getItem("metrik:transparent") ?? "true") === "true",
  );
  const [quotaAgent, setQuotaAgent] = useState(
    () => localStorage.getItem("metrik:quotaAgent") || "codex",
  );
  const [loading, setLoading] = useState(true);
  const [rebuildState, setRebuildState] = useState({ status: "idle", message: "" });
  const [snapshot, setSnapshot] = useState(() => getUsageSnapshot.initial("today"));
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
    const refreshEvery = viewMode === "compact" ? 300_000 : 60_000;
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
  }, [loadSnapshot, period, viewMode]);

  useEffect(() => {
    if (pinned) runWindowAction(() => setWindowPinned(true));
  }, []);

  const pinnedRef = useRef(pinned);
  pinnedRef.current = pinned;
  const viewModeRef = useRef(viewMode);
  viewModeRef.current = viewMode;

  // 玻璃只作用于小插件形态；系统明暗主题切换时重发对应 tint。
  useEffect(() => {
    const apply = () => runWindowAction(() => setWindowGlass(transparent && viewMode === "compact"));
    apply();
    const media = window.matchMedia?.("(prefers-color-scheme: dark)");
    media?.addEventListener?.("change", apply);
    return () => media?.removeEventListener?.("change", apply);
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

  const handleWindowMode = useCallback((nextMode) => {
    setViewMode(nextMode);
    if (nextMode === "compact") setActiveNav("overview");
    runWindowAction(() => applyWindowMode(nextMode));
  }, []);

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
          onPeriodChange={setPeriod}
          onSelectAgent={setSelectedAgent}
          onOpenSources={() => setDrawerOpen(true)}
          onTogglePinned={handleTogglePinned}
          onToggleTransparent={handleToggleTransparent}
          onExpand={handleWindowMode}
          quotaAgent={activeQuotaAgent}
          onCycleQuotaAgent={handleCycleQuotaAgent}
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
        <div className="expanded-drag-region" data-tauri-drag-region aria-hidden="true" />
        <WindowActions
          mode="expanded"
          pinned={pinned}
          onToggleMode={handleWindowMode}
          onTogglePinned={handleTogglePinned}
        />
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
                <UsageChart snapshot={snapshot} selectedAgent={selectedAgent} />
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
          <SettingsSection onSnapshotRefresh={() => loadSnapshot(currentPeriod.current)} />
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
