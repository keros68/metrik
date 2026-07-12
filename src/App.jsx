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
  Clock,
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
import { getUsageSnapshot, rebuildLocalLedger } from "./usageClient";
import {
  applyWindowMode,
  closeWindow,
  minimizeWindow,
  setWindowGlass,
  setWindowPinned,
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
};

function compactTokens(value) {
  const amount = Number(value || 0);
  if (amount >= 1_000_000_000) {
    return `${(amount / 1_000_000_000).toFixed(2).replace(/\.00$/, "")}B`;
  }
  if (amount >= 1_000_000) {
    return `${(amount / 1_000_000).toFixed(2).replace(/\.00$/, "")}M`;
  }
  if (amount >= 1_000) {
    return `${(amount / 1_000).toFixed(amount >= 100_000 ? 0 : 1)}K`;
  }
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
  const showCodex = selectedAgent === "all" || selectedAgent === "codex";
  const showClaude = selectedAgent === "all" || selectedAgent === "claude";
  const activeLabel = selectedAgent === "all"
    ? PERIODS.find((item) => item.id === snapshot.period)?.label || "今日"
    : AGENT_META[selectedAgent].label;
  const legendClass = selectedAgent === "claude" ? "legend-line--claude" : "legend-line--codex";

  return (
    <section className="chart-section" aria-labelledby="usage-chart-title">
      <h2 id="usage-chart-title" className="sr-only">用量趋势</h2>
      <span className="axis-caption">{snapshot.period === "today" ? "tokens · 当日累计" : "tokens · 每日增量"}</span>
      <div className="chart-frame">
        <Suspense fallback={<div className="chart-loading">正在准备趋势图</div>}>
          <UsagePlot
            series={snapshot.series}
            showCodex={showCodex}
            showClaude={showClaude}
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
      <img src={meta.iconSrc} alt="" draggable="false" />
    </span>
  );
}

function Inspector({ snapshot, selectedAgent, onSelectAgent, onOpenSources }) {
  const quota = snapshot.quota;
  const secondaryQuota = snapshot.secondaryQuota;
  const quotaIsSnapshot = quota.stale || quota.quality === "official_snapshot";
  const secondaryIsSnapshot = secondaryQuota.stale || secondaryQuota.quality === "official_snapshot";
  const dataUnavailable = snapshot.pending || snapshot.loadError;
  const partial = snapshotIsPartial(snapshot);
  return (
    <aside className="inspector" aria-label="配额与 Agent 明细">
      <div className="inspector-section inspector-quota">
        <span className="eyebrow">{quotaIsSnapshot ? "ChatGPT / Codex 短窗快照" : "ChatGPT / Codex 短窗剩余"} · {quotaProvenance(quota)}</span>
        <div className="quota-value">
          {quota.available ? Math.round(quota.remainingPercent) : "--"}<small>{quota.available ? "%" : ""}</small>
        </div>
        <div className={`quota-track ${quotaIsSnapshot ? "quota-track--stale" : ""}`} aria-hidden="true">
          <span style={{ transform: `scaleX(${quota.available ? quota.remainingPercent / 100 : 0})` }} />
        </div>
      </div>

      <div className={`secondary-quota ${secondaryIsSnapshot ? "secondary-quota--stale" : ""}`}>
        <div className="secondary-quota-head">
          <span className="eyebrow">{secondaryIsSnapshot ? "ChatGPT / Codex 长窗快照" : "ChatGPT / Codex 长窗剩余"}</span>
          <strong>{secondaryQuota.available ? `${Math.round(secondaryQuota.remainingPercent)}%` : "--"}</strong>
        </div>
        <div className="secondary-quota-track" aria-hidden="true">
          <span style={{ transform: `scaleX(${secondaryQuota.available ? secondaryQuota.remainingPercent / 100 : 0})` }} />
        </div>
        <small>
          {quotaProvenance(secondaryQuota)} · {secondaryQuota.resetExpired
            ? "已重置，等待刷新"
            : secondaryQuota.available
              ? `${formatReset(secondaryQuota.resetsInMinutes)}后重置`
              : "重置时间暂不可用"}
        </small>
      </div>

      <div className="reset-row">
        <div>
          <span className="eyebrow">短窗距离重置</span>
          <strong>{quota.resetExpired ? "等待刷新" : quota.available ? formatReset(quota.resetsInMinutes) : "暂不可用"}</strong>
        </div>
        <Clock size={30} weight="light" aria-hidden="true" />
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
        <small>{snapshot.pending ? "后台建立索引，窗口仍可操作" : snapshot.loadError ? "没有用演示数字替代失败结果" : partial ? "打开统计说明查看受影响来源" : snapshot.isDemo ? "当前为演示模式" : `${quota.sourceLabel} · ${quotaProvenance(quota)} · ${formatClock(snapshot.generatedAt)}`}</small>
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
          aria-label={transparent ? "关闭透明模式" : "使用透明模式"}
          aria-pressed={transparent}
          title={transparent ? "关闭透明模式" : "透明模式"}
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
        aria-label="关闭 Metrik"
        title="关闭"
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
}) {
  const comparisonIsFlat = Math.abs(snapshot.comparisonPercent) < 0.5;
  const comparisonIsLower = snapshot.comparisonPercent < -0.5;
  const ComparisonArrow = comparisonIsLower ? ArrowDown : ArrowUp;
  const comparisonLabel = period === "today" ? "较近 7 日同时段" : "较前一周期";
  const flatComparisonLabel = period === "today" ? "与近 7 日同时段持平" : "与前一周期持平";
  const quota = snapshot.quota;
  const quotaIsSnapshot = quota.stale || quota.quality === "official_snapshot";
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
            <span>{selectedAgent === "all" ? "总用量" : AGENT_META[selectedAgent].label}</span>
            <div aria-live="polite" aria-atomic="true">
              <strong>{snapshot.pending || snapshot.loadError ? "--" : compactTokens(visibleTokens)}</strong>
              <small>tokens</small>
            </div>
            <p className="widget-comparison">
              {snapshot.pending ? (
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

          <button className={`widget-quota ${quotaIsSnapshot ? "widget-quota--stale" : ""}`} type="button" onClick={onOpenSources}>
            <span>ChatGPT · Codex 短窗</span>
            <strong>{quota.available ? `${Math.round(quota.remainingPercent)}%` : "--"}</strong>
            <div className="widget-quota-track" aria-hidden="true">
              <i style={{ transform: `scaleX(${quota.available ? quota.remainingPercent / 100 : 0})` }} />
            </div>
            <small>{quota.quality === "demo" ? quotaProvenance(quota) : quota.resetExpired ? "窗口已重置，等待刷新" : quotaIsSnapshot ? quotaProvenance(quota) : quota.available ? `${formatReset(quota.resetsInMinutes)}后重置` : "官方配额不可用"}</small>
          </button>
        </section>

        <section className="widget-agent-list" aria-label="按 Agent 筛选">
          {snapshot.agents.map((agent) => {
            const meta = AGENT_META[agent.id];
            if (!meta) return null;
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
  const [transparent, setTransparent] = useState(() => localStorage.getItem("metrik:transparent") === "true");
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
    if (transparent) runWindowAction(() => setWindowGlass(true));
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
  const appBusy = loading || rebuildState.status === "busy";
  const comparisonIsFlat = Math.abs(snapshot.comparisonPercent) < 0.5;
  const comparisonIsLower = snapshot.comparisonPercent < -0.5;
  const ComparisonArrow = comparisonIsLower ? ArrowDown : ArrowUp;
  const comparisonLabel = period === "today" ? "比近 7 日同时段" : "比前一周期";
  const flatComparisonLabel = period === "today" ? "与近 7 日同时段持平" : "与前一周期持平";

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
      runWindowAction(() => setWindowGlass(next));
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
                <span className="section-kicker">{PERIODS.find((item) => item.id === period)?.label}</span>
                <div className="metric-line" aria-live="polite" aria-atomic="true">
                  <h1>{snapshot.pending || snapshot.loadError ? "--" : compactTokens(visibleTokens)}</h1>
                  <span>tokens</span>
                </div>
                <p className="comparison">
                  {snapshot.pending ? (
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
