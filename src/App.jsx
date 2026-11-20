import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  ArrowDown,
  ArrowsClockwise,
  ArrowUp,
  CaretRight,
  ChartBar,
  ChartLineUp,
  ClockCounterClockwise,
  Database,
  FileText,
  FunnelSimple,
  GearSix,
  HardDrives,
  ShieldCheck,
  X,
} from "@phosphor-icons/react";
import { nextGlassTint, resolveGlassMode } from "./glassAppearance.js";
import { modelDisplayName } from "./modelNames.js";
import { compactTokens } from "./tokenFormat.js";
import { isBalanceWindow } from "./quotaWindows.js";
import { t, useLanguage } from "./i18n.js";
import {
  getQuotaSnapshot,
  getUsageProjects,
  getUsageReport,
  getUsageSessions,
  getUsageSnapshot,
  rebuildLocalLedger,
} from "./usageClient";
import {
  applyLiveCompactScale,
  applyLiveStripScale,
  applyStartupUiScale,
  applyWindowMode,
  broadcastMacAgentSelection,
  broadcastMacAppearance,
  broadcastStateSync,
  checkForUpdate,
  collapseVerticalStripHover,
  getMacAgentSelection,
  getSurfaceVisibility,
  isDesktop,
  isWindowsPlatform,
  onMacAgentSelection,
  onMacAppearance,
  onPanelNav,
  onScaleFactorChanged,
  onStateSync,
  onSurfaceVisibilityChanged,
  onTrayPinnedChange,
  onTrayShowExpanded,
  openExpandedWindow,
  readStripScale,
  readUiScale,
  reassertCompactSize,
  reassertStripSize,
  restoreWindowPosition,
  setNativeTheme,
  setPinnedHoverTargetOpacity,
  setStripScale,
  setSurfaceVisible,
  setWindowGlass,
  setWindowPinned,
  setWindowUiScale,
  startEdgeDock,
  startPositionMemory,
  stripContentSize,
  syncEdgeDock,
  syncLinuxTrayPinned,
  takePendingPanelNav,
  updateMacStatusItems,
  updateTrayQuotaBadge,
} from "./windowClient";
import { TRAY_BADGE_HIDDEN_REFRESH_MS, trayBadgeSpec, trayBadgeTooltip } from "./trayBadge.js";
import { IS_LINUX, IS_MAC, IS_WINDOWS } from "./platform.js";
import {
  AGENT_LABELS,
  AGENT_META,
  AGENT_ORDER,
  AgentMark,
  chartColor,
  normalizeVisibleAgentList,
  visibleAgentId,
} from "./agentMeta.jsx";
import {
  GLASS_RADIUS_PX,
  normalizeGlassInk,
  normalizeGlassTint,
  normalizePinnedHoverMode,
  PINNED_HIDDEN_OPACITY,
} from "./appearanceOptions.js";
import {
  agentQuotaFor,
  conciseQualityLabel,
  conciseSourceDetail,
  exactTokens,
  formatClock,
  QuotaBarRow,
  quotaEmptyCopy,
  quotaHasData,
  quotaProvenance,
  snapshotIsPartial,
  stripCellData,
} from "./quotaView.jsx";
import {
  formatUsd,
  PeriodControl,
  PERIODS,
  sourceStatusCopy,
} from "./viewParts.jsx";
import {
  CompactWidget,
  invalidateWindowCorrections,
  runLatestWindowCorrection,
  runWindowAction,
  StripBar,
  stripWindowSize,
  WindowActions,
} from "./widgets.jsx";
import { feedbackText, SettingsSection } from "./SettingsSection.jsx";
import { UsageSection } from "./UsageSection.jsx";
import { ReportsSection } from "./ReportsSection.jsx";

const UsagePlot = lazy(() =>
  import("./UsagePlot").then((module) => ({ default: module.UsagePlot })),
);

const NAV_ITEMS = [
  { id: "overview", label: "概览", icon: ChartLineUp },
  { id: "usage", label: "用量", icon: ChartBar },
  { id: "reports", label: "报告", icon: FileText },
  { id: "settings", label: "设置", icon: GearSix },
];

function Sidebar({ activeNav, onNavChange, snapshot, loading, nextRefreshAt }) {
  const partial = snapshotIsPartial(snapshot);
  const indexing = (snapshot.indexing?.pending || 0) > 0;
  const sourceStatus = sourceStatusCopy(snapshot, loading, partial);
  const statusAnchor = useRef(null);
  const [tooltipDismissed, setTooltipDismissed] = useState(false);
  useEffect(() => {
    const dismissTooltip = (event) => {
      if (event.key === "Escape" && statusAnchor.current?.matches(":hover, :focus-within")) {
        setTooltipDismissed(true);
      }
    };
    document.addEventListener("keydown", dismissTooltip);
    return () => document.removeEventListener("keydown", dismissTooltip);
  }, []);
  // 状态行保持定高；完整说明同时支持悬停和键盘焦点。更新中不显示旧计划时间。
  const statusDescription = nextRefreshAt && !loading && !snapshot.pending && !indexing
    ? `${sourceStatus.detail}\n${t("下次自动刷新约 {time}", {
        time: formatClock(new Date(nextRefreshAt).toISOString()),
      })}`
    : sourceStatus.detail;
  return (
    <aside className="sidebar" aria-label={t("主导航")}>
      <div className="wordmark">Metrik</div>

      <nav className="nav-stack">
        {NAV_ITEMS.map(({ id, label, icon: Icon }) => (
          <button
            className={`nav-button ${activeNav === id ? "nav-button--active" : ""}`}
            key={id}
            type="button"
            aria-label={t(label)}
            aria-current={activeNav === id ? "page" : undefined}
            onClick={() => onNavChange(id)}
          >
            <Icon size={27} weight="light" aria-hidden="true" />
            <span className="nav-dot" aria-hidden="true" />
            <span className="tooltip-label">{t(label)}</span>
          </button>
        ))}
      </nav>

      {/* 状态入口保持两行定高，说明浮在侧栏旁边，不挤动导航。 */}
      <div
        ref={statusAnchor}
        className="source-status-anchor"
        data-tooltip-dismissed={tooltipDismissed || undefined}
        onMouseEnter={() => setTooltipDismissed(false)}
        onFocusCapture={() => setTooltipDismissed(false)}
      >
        <button
          className="source-status"
          type="button"
          onClick={() => onNavChange("sources")}
          aria-describedby="source-status-tooltip"
        >
          <span className={`status-dot ${loading || indexing ? "status-dot--loading" : ""} ${snapshot.loadError ? "status-dot--error" : ""} ${partial && !indexing ? "status-dot--warning" : ""}`} />
          <span>{t("数据统计")}</span>
          <small>{sourceStatus.state}</small>
        </button>
        <span id="source-status-tooltip" className="tooltip-label source-status-tooltip" role="tooltip">
          {statusDescription}
        </span>
      </div>
    </aside>
  );
}

function UsageChart({ snapshot, selectedAgent, dark = false }) {
  // 引用要稳定：UsagePlot 以它为依赖，每次渲染新建数组会重建整张图。
  const visibleAgents = useMemo(
    () => (selectedAgent === "all" ? AGENT_ORDER : [selectedAgent]),
    [selectedAgent],
  );
  // 图例与图中的线一致：只列周期内有数据的 Agent。
  const legendAgents = selectedAgent === "all"
    ? AGENT_ORDER.filter((agent) =>
        snapshot.series.some((point) => Number(point.tokens?.[agent] || 0) > 0))
    : [selectedAgent];

  return (
    <section className="chart-section" aria-labelledby="usage-chart-title">
      <h2 id="usage-chart-title" className="sr-only">{t("用量趋势")}</h2>
      <span className="axis-caption">{snapshot.period === "today" ? t("tokens · 当日累计") : t("tokens · 每日增量")}</span>
      <div className="chart-frame">
        <Suspense fallback={<div className="chart-loading">{t("正在准备趋势图")}</div>}>
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
      <div className="chart-legend" aria-label={t("图例")}>
        {(legendAgents.length ? legendAgents : visibleAgents.slice(0, 1)).map((agent) => (
          <span key={agent}>
            <i className="legend-line" style={{ background: chartColor(agent) }} />
            {AGENT_META[agent]?.label || agent}
          </span>
        ))}
      </div>
    </section>
  );
}

const TOKEN_COMPONENTS = [
  { key: "inputUncached", label: "未缓存输入", color: "#246bdb" },
  { key: "cacheRead", label: "缓存读取", color: "#9dbdf0" },
  { key: "cacheWrite", label: "缓存写入", color: "#6a5ae0" },
  { key: "output", label: "输出", color: "#e36b49" },
];

const MODEL_PREVIEW_COUNT = 6;

// Token 构成 + 模型分布：都来自本地账本的精确解析（processed 口径，非账单）。
function BreakdownSection({ snapshot, selectedAgent, onOpenModelSpeed }) {
  const scopedAgents = selectedAgent === "all"
    ? snapshot.agents
    : snapshot.agents.filter((agent) => agent.id === selectedAgent);
  const components = TOKEN_COMPONENTS.map((component) => ({
    ...component,
    value: scopedAgents.reduce((sum, agent) => sum + Number(agent[component.key] || 0), 0),
  }));
  const componentTotal = components.reduce((sum, component) => sum + component.value, 0);
  const [showAllModels, setShowAllModels] = useState(false);
  const scopedModels = (snapshot.models || [])
    .filter((entry) => selectedAgent === "all" || entry.agent === selectedAgent);
  const volumeModels = [...scopedModels]
    .filter((entry) => entry.tokens > 0)
    .sort((left, right) => right.tokens - left.tokens);
  const models = showAllModels ? volumeModels : volumeModels.slice(0, MODEL_PREVIEW_COUNT);
  const modelMax = models[0]?.tokens || 1;

  const cost = snapshot.cost;
  const costRows = cost?.available
    ? cost.byAgent.filter((row) =>
        (selectedAgent === "all" || row.agent === selectedAgent) && (row.usd > 0 || row.unpricedTokens > 0))
      .sort((left, right) => right.usd - left.usd)
    : [];
  const scopedUsd = costRows.reduce((sum, row) => sum + row.usd, 0);
  const scopedUnpriced = costRows.reduce((sum, row) => sum + row.unpricedTokens, 0);

  if (!componentTotal && !scopedModels.length && !costRows.length) return null;

  return (
    <section className="breakdown-stack" aria-label={t("Token 构成、模型分布与成本估算")}>
      {(componentTotal > 0 || costRows.length > 0) && (
        <div className="breakdown-grid">
          {componentTotal > 0 && (
            <article className="breakdown-card">
              <h2>{t("Token 构成")}</h2>
              <div className="comp-bar" role="img" aria-label={t("按处理类型的 token 构成比例")}>
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
                    <span>{t(component.label)}</span>
                    <em>{compactTokens(component.value)} · {((component.value / componentTotal) * 100).toFixed(1)}%</em>
                  </li>
                ))}
              </ul>
            </article>
          )}
          {costRows.length > 0 && (
            <article className="breakdown-card">
              <h2>{t("成本估算")}</h2>
              <p className="cost-total">
                <strong>
                  <span className="cost-currency">$</span>
                  {formatUsd(scopedUsd).slice(1)}
                </strong>
                <span>{t("本周期 · API 等价")}</span>
              </p>
              <ul className="comp-legend">
                {costRows.map((row) => (
                  <li key={row.agent}>
                    <i style={{ backgroundColor: AGENT_META[row.agent]?.accent || "#74767a", borderRadius: "50%" }} aria-hidden="true" />
                    <span>{AGENT_META[row.agent]?.label || row.agent}</span>
                    <em>{row.usd > 0 ? formatUsd(row.usd) : t("未计价")}</em>
                  </li>
                ))}
              </ul>
              <p className="cost-note">
                {t("按公开 API 价格（{date}）折算，非账单。", { date: cost.pricingAsOf })}
                {scopedUnpriced > 0 ? t("另有 {tokens} tokens 因无可靠定价未计入。", { tokens: compactTokens(scopedUnpriced) }) : ""}
              </p>
            </article>
          )}
        </div>
      )}
      {scopedModels.length > 0 && (
        <article className="breakdown-card breakdown-card--models">
          <div className="breakdown-card-head">
            <h2>{t("模型分布")}</h2>
            <button type="button" className="breakdown-link" onClick={onOpenModelSpeed}>
              <span>{t("速率明细")}</span>
              <CaretRight size={12} weight="bold" aria-hidden="true" />
            </button>
          </div>
          {models.length > 0 ? (
            <ul className="model-list model-list--overview">
              {models.map((entry) => {
                const agentLabel = AGENT_META[entry.agent]?.label || entry.agent;
                const displayName = modelDisplayName(entry.model);
                return (
                  <li key={`${entry.agent}-${entry.model}`}>
                    <i
                      className="model-dot"
                      style={{ backgroundColor: AGENT_META[entry.agent]?.accent || "#74767a" }}
                      aria-hidden="true"
                    />
                    <span className="model-name" title={`${displayName} · ${agentLabel}`}>
                      <span>{displayName}</span>
                      <small className="model-agent-tag">{agentLabel}</small>
                    </span>
                    <span className="model-track" aria-hidden="true">
                      <i style={{ transform: `scaleX(${entry.tokens / modelMax})`, backgroundColor: AGENT_META[entry.agent]?.accent || "#74767a" }} />
                    </span>
                    <em>{compactTokens(entry.tokens)} · {entry.share.toFixed(1)}%</em>
                  </li>
                );
              })}
            </ul>
          ) : (
            <p className="cost-note">{t("当前周期仅有响应速度采样记录，点击右上角查看速率明细。")}</p>
          )}
          {volumeModels.length > MODEL_PREVIEW_COUNT && (
            <button type="button" className="project-expand" onClick={() => setShowAllModels((value) => !value)}>
              {showAllModels ? t("收起") : t("显示全部 {count} 个模型", { count: volumeModels.length })}
            </button>
          )}
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
          <h2 id="usage-chart-state-title">{pending ? t("正在读取本机趋势") : t("趋势暂不可用")}</h2>
          <p>{pending ? t("索引完成后将显示真实曲线。") : t("读取失败时不会以零值或演示曲线替代。")}</p>
        </div>
      </div>
    </section>
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
    <aside className="inspector" aria-label={t("配额与 Agent 明细")}>
      <div className="quota-groups" aria-label={t("各 Agent 官方配额")}>
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

      <div className="agent-list" aria-label={t("按 Agent 筛选")}>
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
        <span><ShieldCheck size={17} weight="fill" />{snapshot.pending ? t("正在读取本机数据") : snapshot.loadError ? t("数据暂不可用") : partial ? t("部分数据可能不完整") : t("数据源正常")}</span>
        <small>{snapshot.pending ? t("后台建立索引，窗口仍可操作") : snapshot.loadError ? t("失败结果未以演示数据替代") : partial ? t("打开数据来源查看受影响来源") : snapshot.isDemo ? t("当前为演示数据") : t("本地统计 + 官方配额 · {time}", { time: formatClock(snapshot.generatedAt) })}</small>
      </button>
    </aside>
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
  const partial = snapshotIsPartial(snapshot);

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
          <h2 id="source-title">{t("数据来源", { context: "drawer" })}</h2>
          <div className="source-header-actions">
            {!partial && !confirmingRebuild && rebuildState.status === "idle" && (
              <button
                type="button"
                className="source-rescan-button"
                disabled={rebuildBusy}
                onClick={() => setConfirmingRebuild(true)}
              >
                <ClockCounterClockwise size={15} weight="light" aria-hidden="true" />
                {rebuildBusy ? t("扫描中…") : t("重新扫描")}
              </button>
            )}
            <button ref={closeButtonRef} type="button" className="icon-button" onClick={onClose} aria-label={t("关闭")}>
              <X size={21} weight="light" />
            </button>
          </div>
        </header>

        {(snapshot.indexing?.pending || 0) > 0 && (
          <div className="indexing-note" role="status">
            <ClockCounterClockwise size={20} weight="light" aria-hidden="true" />
            <p>
              {(() => {
                const [before, after] = t("正在补齐历史，还剩 {count} 个文件。完成后会自动更新。").split("{count}");
                return <>{before}<strong>{snapshot.indexing.pending}</strong>{after}</>;
              })()}
            </p>
          </div>
        )}

        {(partial || confirmingRebuild || rebuildState.status !== "idle") && (
          <section className={`source-repair ${partial ? "source-repair--warning" : ""}`} aria-labelledby="source-repair-title">
            {confirmingRebuild ? (
              <div className="source-repair-confirmation" role="group" aria-labelledby="source-repair-title">
                <div>
                  <strong id="source-repair-title">{t("重新扫描本机数据？")}</strong>
                  <p>{t("只重建 Metrik 的统计索引，不会修改 Agent 日志。")}</p>
                </div>
                <div className="source-repair-actions">
                  <button
                    ref={cancelRebuildRef}
                    type="button"
                    className="ledger-button ledger-button--secondary"
                    onClick={() => setConfirmingRebuild(false)}
                  >
                    {t("取消")}
                  </button>
                  <button
                    type="button"
                    className="ledger-button ledger-button--primary"
                    onClick={confirmRebuild}
                  >
                    {t("重新扫描")}
                  </button>
                </div>
              </div>
            ) : (
              <div className="source-repair-row">
                <span className="source-repair-icon" aria-hidden="true">
                  <ClockCounterClockwise size={18} weight="light" />
                </span>
                <div>
                  <strong id="source-repair-title">
                    {rebuildBusy
                      ? t("正在重新扫描")
                      : partial && rebuildState.status === "success"
                        ? t("扫描完成，仍不完整")
                        : partial
                          ? t("部分数据未计入")
                          : t("扫描完成")}
                  </strong>
                  <p>
                    {rebuildBusy
                      ? t("正在重建统计索引，可能需要几分钟。")
                      : rebuildState.status === "success"
                        ? feedbackText(rebuildState)
                        : partial
                          ? t("可重新扫描本机日志；如果仍不完整，更新 Metrik 后再试。")
                          : feedbackText(rebuildState)}
                  </p>
                </div>
                {partial && !rebuildBusy && (
                  <button type="button" className="source-rescan-button" onClick={() => setConfirmingRebuild(true)}>
                    {t("重新扫描")}
                  </button>
                )}
              </div>
            )}
            {!confirmingRebuild && rebuildState.status === "error" && (
              <p className="source-repair-error" role={rebuildStatusRole}>{feedbackText(rebuildState)}</p>
            )}
          </section>
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
                <p>{conciseSourceDetail(source)}</p>
              </div>
              <span className={`quality-badge quality-badge--${source.quality}`}>{conciseQualityLabel(source)}</span>
            </article>
          ))}
        </div>

        <div className="privacy-note">
          <ShieldCheck size={20} weight="light" />
          <p>{t("只保存用量和来源信息，不读取或上传对话正文与凭据。")}</p>
        </div>
      </section>
    </div>
  );
}

function initialWindowMode() {
  if (typeof window === "undefined") return "compact";
  const view = new URLSearchParams(window.location.search).get("view");
  if (view === "expanded") return "expanded";
  if (view === "strip" && !IS_MAC) return "strip";
  if (IS_MAC) return "compact";
  // Windows / Linux 桌面端三形态各自独立窗口：main 固定承担 compact 卡片，
  // strip 与 expanded 由独立窗口承担；浏览器演示模式仍支持单页切换。
  if (isDesktop()) return "compact";
  return localStorage.getItem("metrik:viewMode") === "strip" ? "strip" : "compact";
}

function readStartupSurfacePrefs() {
  const storedCompact = localStorage.getItem("metrik:surfaceCompactVisible");
  const storedStrip = localStorage.getItem("metrik:surfaceStripVisible");
  if (storedCompact !== null || storedStrip !== null) {
    const compact = storedCompact !== "false";
    const strip = storedStrip === "true";
    // 防止上次全部关掉导致冷启动无任何可见窗口
    return compact || strip ? { compact, strip } : { compact: true, strip: false };
  }
  const legacyStrip = localStorage.getItem("metrik:viewMode") === "strip";
  return { compact: !legacyStrip, strip: legacyStrip };
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
  const [usageView, setUsageView] = useState("projects");
  const [usageJumpRequest, setUsageJumpRequest] = useState(null);
  const handleOpenModelSpeed = useCallback(() => {
    setUsageView("models");
    setUsageJumpRequest((current) => ({
      agent: selectedAgent,
      nonce: (current?.nonce || 0) + 1,
    }));
    setActiveNav("usage");
  }, [selectedAgent]);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [pinned, setPinned] = useState(() => localStorage.getItem("metrik:pinned") === "true");
  const [edgeAutoHide, setEdgeAutoHide] = useState(
    () => localStorage.getItem("metrik:edgeAutoHide") === "true",
  );
  const [surfaceVisibility, setSurfaceVisibility] = useState(() => ({
    compact: viewMode === "compact",
    strip: viewMode === "strip",
    expanded: viewMode === "expanded",
  }));
  const surfaceVisibilityRef = useRef(surfaceVisibility);
  surfaceVisibilityRef.current = surfaceVisibility;

  useEffect(() => {
    if (IS_MAC || !isDesktop()) return undefined;
    let cancelled = false;
    getSurfaceVisibility().then((vis) => {
      if (!cancelled && vis) setSurfaceVisibility(vis);
    });
    const stopPromise = onSurfaceVisibilityChanged((vis) => {
      if (!vis) return;
      setSurfaceVisibility(vis);
      if (typeof vis.compact === "boolean") {
        localStorage.setItem("metrik:surfaceCompactVisible", String(vis.compact));
      }
      if (typeof vis.strip === "boolean") {
        localStorage.setItem("metrik:surfaceStripVisible", String(vis.strip));
      }
    });
    return () => {
      cancelled = true;
      stopPromise.then((stop) => stop?.());
    };
  }, []);

  useEffect(() => {
    if (!isDesktop() || viewMode !== "expanded") return undefined;
    const applyNav = (nav) => {
      if (nav === "settings" || nav === "overview" || nav === "usage" || nav === "reports") {
        setActiveNav(nav);
      }
    };
    const stopPromise = onPanelNav(applyNav);
    takePendingPanelNav().then((pending) => {
      if (pending) applyNav(pending);
    });
    return () => {
      stopPromise.then((stop) => stop?.());
    };
  }, [viewMode]);

  const handleSetSurfaceVisible = useCallback((surface, visible, nav = null) => {
    if (IS_MAC || !isDesktop()) return;
    if (surface === "compact") {
      localStorage.setItem("metrik:surfaceCompactVisible", String(Boolean(visible)));
    } else if (surface === "strip") {
      localStorage.setItem("metrik:surfaceStripVisible", String(Boolean(visible)));
    }
    runWindowAction(() => setSurfaceVisible(surface, visible, nav));
  }, []);

  const handleToggleSurface = useCallback((surface) => {
    if (IS_MAC || !isDesktop()) return;
    const current = Boolean(surfaceVisibilityRef.current?.[surface]);
    handleSetSurfaceVisible(surface, !current);
  }, [handleSetSurfaceVisible]);

  const handleEdgeAutoHideChange = useCallback((next) => {
    const value = Boolean(next);
    setEdgeAutoHide(value);
    localStorage.setItem("metrik:edgeAutoHide", String(value));
    broadcastStateSync("edgeAutoHide", value);
  }, []);
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
    broadcastStateSync("pinnedHoverMode", value);
  }, []);
  const handlePinnedHoverOpacity = useCallback((next) => {
    const value = Math.min(0.9, Math.max(0.05, next));
    setPinnedHoverOpacity(value);
    localStorage.setItem("metrik:pinnedHoverOpacity", String(value));
    broadcastStateSync("pinnedHoverOpacity", value);
  }, []);
  // 胶囊条方向：横条 / 竖条，用户手动选，记住选择。
  const [stripOrientation, setStripOrientation] = useState(() =>
    localStorage.getItem("metrik:stripOrientation") === "vertical" ? "vertical" : "horizontal",
  );
  const handleToggleStripOrientation = useCallback(() => {
    setStripOrientation((current) => {
      const next = current === "vertical" ? "horizontal" : "vertical";
      localStorage.setItem("metrik:stripOrientation", next);
      broadcastStateSync("stripOrientation", next);
      return next;
    });
  }, []);
  // 大窗口（主面板）暗色主题：自动 / 亮 / 暗三态，默认跟随系统。
  // 仅作用于主面板；紧凑小插件的玻璃/浅色外观不受此设置影响。
  const [theme, setTheme] = useState(() => {
    const stored = localStorage.getItem("metrik:theme");
    return stored === "light" || stored === "dark" ? stored : "auto";
  });
  const handleThemeChange = useCallback((next) => {
    setTheme(next);
    localStorage.setItem("metrik:theme", next);
    broadcastStateSync("theme", next);
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
    else broadcastStateSync("glassAlpha", next);
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
    broadcastStateSync("glassTint", value);
  }, []);
  // 透明档的文字颜色，只在透明档生效；其它两档的前景由配色本身决定。
  const [glassInk, setGlassInk] = useState(() =>
    normalizeGlassInk(localStorage.getItem("metrik:glassInk")),
  );
  const handleGlassInk = useCallback((next) => {
    const value = normalizeGlassInk(next);
    setGlassInk(value);
    localStorage.setItem("metrik:glassInk", value);
    broadcastStateSync("glassInk", value);
  }, []);
  // 轮转按钮要读当前配色但不该因它重建回调，用 ref 取值。
  const glassTintRef = useRef(glassTint);
  glassTintRef.current = glassTint;
  // 卡片的整体缩放系数（连续值）：滑杆与卡片右下角拖拽共用，实时生效。
  const [uiScale, setUiScale] = useState(() => readUiScale());
  const handleUiScale = useCallback((next) => {
    const applied = setWindowUiScale(next);
    setUiScale(applied);
    broadcastStateSync("uiScale", applied);
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
    broadcastStateSync("trayBadgeEnabled", next);
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
    else broadcastStateSync("widgetAgents", next);
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
  // 下一次常规自动刷新的时刻，只用于状态提示；补齐索引的快速轮询与窗口隐藏时为空。
  const [nextRefreshAt, setNextRefreshAt] = useState(null);
  const requestSequence = useRef(0);
  const loadInFlight = useRef(false);
  const activeLoadPeriod = useRef(null);
  const queuedLoadPeriod = useRef(null);
  const currentPeriod = useRef(period);
  const widgetAgentsRef = useRef(widgetAgents);
  const rebuildInFlight = useRef(false);
  const lastQuotaCheckAt = useRef(0);
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
    let quotaOnly = options?.quotaOnly === true;
    try {
      while (periodToLoad) {
        activeLoadPeriod.current = periodToLoad;
        queuedLoadPeriod.current = null;
        const requestId = ++requestSequence.current;
        const quotaOnlyLoad = quotaOnly;
        if (!quotaOnlyLoad) {
          setLoading(true);
        }
        const next = quotaOnlyLoad
          ? await getQuotaSnapshot(periodToLoad)
          : await getUsageSnapshot(periodToLoad, {
              force: forceLoad,
              // 每轮都读 ref：排队请求必须拿到最新勾选，不能沿用创建回调时的闭包。
              widgetAgents: widgetAgentsRef.current,
            });
        forceLoad = false;
        quotaOnly = false;
        if (!next.loadError) {
          lastQuotaCheckAt.current = Date.now();
        }
        // 仅额度的刷新失败时保留现有快照：它只是聚焦时的加速通道，失败不该把
        // 已载入的本地统计和上一轮额度清成不可用。
        const keepCurrent = quotaOnlyLoad && next.loadError;
        if (requestId === requestSequence.current && !queuedLoadPeriod.current && !keepCurrent) {
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

  // 快照里的来源说明、窗口标签、趋势刻度是后端按当时语言写的；切换语言后
  // 重取一次，屏幕上已有的后端文字随之换成新语言。
  const language = useLanguage();
  const snapshotLanguageRef = useRef(language);
  useEffect(() => {
    if (snapshotLanguageRef.current === language) return;
    snapshotLanguageRef.current = language;
    loadSnapshot(currentPeriod.current, { ensureLatestWidgetAgents: true });
  }, [language, loadSnapshot]);

  useEffect(() => {
    // 历史索引未补齐时快速迭代：每次快照只花掉一小段补齐预算，靠连续刷新把
    // 剩余文件啃完，界面全程可用。补齐结束后回到常规节奏。
    // strip 与 compact 同档：都是常驻小组件，不需要展开视图的高频刷新。
    const refreshEvery = indexing ? 400 : viewMode === "expanded" ? 60_000 : 300_000;
    let timer;

    // 记的是定时器真正的触发时刻：手动刷新不重置定时器，焦点与可见性变化会。
    const markNextRefresh = () => setNextRefreshAt(indexing ? null : Date.now() + refreshEvery);

    const schedule = () => {
      window.clearInterval(timer);
      timer = undefined;
      setNextRefreshAt(null);
      if (document.visibilityState === "visible") {
        markNextRefresh();
        timer = window.setInterval(() => {
          markNextRefresh();
          loadSnapshot(period);
        }, refreshEvery);
      } else if (trayBadgeEnabled || IS_MAC) {
        // Windows 托盘数字和 macOS 菜单栏状态项都常亮在系统栏上：窗口隐藏时
        // 保留慢速刷新，数字不会冻在旧值——macOS 否则会一直 "--" 到用户点开面板。
        timer = window.setInterval(
          () => loadSnapshot(period),
          TRAY_BADGE_HIDDEN_REFRESH_MS,
        );
      }
    };

    // 聚焦/切回可见时带 15 秒最小冷却：合并同一刻触发的 visibilitychange + focus，
    // 且点击已在前台的小组件不重置 5 分钟全量日志扫描定时器，避免长期推迟本地扫描。
    const FOCUS_QUOTA_COOLDOWN_MS = 15_000;
    const maybeRefreshQuota = () => {
      if (
        document.visibilityState === "visible" &&
        Date.now() - lastQuotaCheckAt.current >= FOCUS_QUOTA_COOLDOWN_MS
      ) {
        loadSnapshot(period, { quotaOnly: true });
      }
    };

    const handleVisibilityChange = () => {
      schedule();
      maybeRefreshQuota();
    };

    schedule();
    document.addEventListener("visibilitychange", handleVisibilityChange);
    window.addEventListener("focus", maybeRefreshQuota);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", handleVisibilityChange);
      window.removeEventListener("focus", maybeRefreshQuota);
    };
  }, [loadSnapshot, period, viewMode, indexing, trayBadgeEnabled]);

  useEffect(() => {
    // macOS 面板由系统管层级和位置：不置顶、不恢复坐标。
    if (IS_MAC) return;
    if (viewMode === "expanded") return;
    if (pinned) runWindowAction(() => setWindowPinned(true));
    runWindowAction(() => syncLinuxTrayPinned(pinned));
    // 小组件回到上次摆放的位置（含固定位置），坐标已不在任何屏幕上时居中。
    // strip 形态的启动定位在 strip 专属 effect 里做。
    if (viewMode === "compact") {
      const prefs = isDesktop() ? readStartupSurfacePrefs() : { compact: true, strip: false };
      runWindowAction(async () => {
        await restoreWindowPosition("compact");
        await applyStartupUiScale("compact", { keepHidden: !prefs.compact });
        if (prefs.strip && isDesktop()) {
          await setSurfaceVisible("strip", true);
        }
      });
    }
  }, []);

  const pinnedRef = useRef(pinned);
  pinnedRef.current = pinned;
  const edgeAutoHideRef = useRef(edgeAutoHide);
  edgeAutoHideRef.current = edgeAutoHide;
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

  // 卡片与胶囊固定使用玻璃材质，用户只在深色、浅色和透明三种外观间选择；
  // 只有 expanded 关闭玻璃绘制。
  // Windows 小组件终身保持创建期透明窗口，不在运行时切换 DWM backdrop。
  // 桌面端初始直接选 alpha，避免 WebView 背景确认前闪一帧 CSS fallback。
  const [glassMode, setGlassMode] = useState(() =>
    resolveGlassMode({
      enabled: viewMode !== "expanded",
      trueAlphaAvailable: isDesktop() && isWindowsPlatform(),
    }),
  );
  useEffect(() => {
    let cancelled = false;
    const apply = () => {
      setWindowGlass(viewMode !== "expanded", viewMode === "strip" ? 20 : 14)
        .then((mode) => {
          if (!cancelled) setGlassMode(mode);
        })
        .catch((error) => {
          console.warn("Unable to update the desktop window.", error);
          if (!cancelled) {
            setGlassMode(resolveGlassMode({
              enabled: viewMode !== "expanded",
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
  }, [viewMode]);

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

  // 边缘挂靠（可选，默认关闭）：开启后未固定卡片/胶囊靠边自动收起，鼠标贴边弹出。
  useEffect(() => {
    const stopPromise = startEdgeDock({
      getMode: () => viewModeRef.current,
      getPinned: () => pinnedRef.current,
      getEnabled: () => edgeAutoHideRef.current,
    });
    return () => {
      stopPromise.then((stop) => stop?.());
    };
  }, []);

  useEffect(() => {
    runWindowAction(() => syncEdgeDock());
  }, [edgeAutoHide, pinned, viewMode]);

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

  // 胶囊条的独立缩放系数（与小组件互不影响）：实时生效。
  const [stripScale, setStripScaleState] = useState(() => readStripScale());
  const handleStripScale = useCallback((next) => {
    const applied = setStripScale(next);
    setStripScaleState(applied);
    broadcastStateSync("stripScale", applied);
  }, []);
  const stripLayoutRef = useRef({
    orientation: stripOrientation,
    count: stripAgents.length,
  });
  stripLayoutRef.current = {
    orientation: stripOrientation,
    count: stripAgents.length,
  };

  const lastSyncSeqRef = useRef({});
  const emitSyncedState = useCallback((key, value) => {
    lastSyncSeqRef.current[key] = Date.now();
    broadcastStateSync(key, value);
  }, []);

  // Windows / Linux 跨窗口实时同步（卡片、胶囊、主面板共享外观、缩放与列表设置）
  useEffect(() => {
    if (IS_MAC || !isDesktop()) return undefined;
    const stopPromise = onStateSync(({ key, value, seq }) => {
      if (typeof seq === "number" && (lastSyncSeqRef.current[key] || 0) > seq) return;
      if (typeof seq === "number") lastSyncSeqRef.current[key] = seq;
      switch (key) {
        case "glassAlpha": {
          const alpha = Number(value);
          if (Number.isFinite(alpha) && alpha >= 0.05 && alpha <= 0.96) setGlassAlpha(alpha);
          break;
        }
        case "glassTint":
          setGlassTint(normalizeGlassTint(value));
          break;
        case "glassInk":
          setGlassInk(normalizeGlassInk(value));
          break;
        case "theme":
          if (value === "auto" || value === "light" || value === "dark") setTheme(value);
          break;
        case "uiScale": {
          const applied = Number(value);
          if (Number.isFinite(applied)) {
            setUiScale(applied);
            if (viewModeRef.current === "compact") {
              runLatestWindowCorrection(() => applyLiveCompactScale(applied, { persist: true }));
            }
          }
          break;
        }
        case "stripScale": {
          const applied = Number(value);
          if (Number.isFinite(applied)) {
            setStripScaleState(applied);
            if (viewModeRef.current === "strip") {
              const layout = stripLayoutRef.current;
              const estimate = stripWindowSize(layout.orientation, layout.count);
              const content = stripContentSize(layout.orientation, estimate);
              runLatestWindowCorrection(() => applyLiveStripScale(applied, content, { persist: true }));
            }
          }
          break;
        }
        case "snapshotRefresh":
          loadSnapshot(currentPeriod.current);
          break;
        case "widgetAgents": {
          if (Array.isArray(value)) {
            const next = normalizeVisibleAgentList(value);
            if (next.length) setWidgetAgents(next);
          }
          break;
        }
        case "stripAgents": {
          if (Array.isArray(value)) {
            const next = normalizeVisibleAgentList(value);
            if (next.length) setStripAgentsSetting(next);
          }
          break;
        }
        case "pinned": {
          const next = Boolean(value);
          setPinned(next);
          if (viewModeRef.current !== "expanded") {
            runWindowAction(async () => {
              invalidateWindowCorrections();
              await collapseVerticalStripHover();
              await setWindowPinned(next);
            });
          }
          break;
        }
        case "edgeAutoHide":
          setEdgeAutoHide(Boolean(value));
          break;
        case "stripOrientation":
          if (value === "horizontal" || value === "vertical") setStripOrientation(value);
          break;
        case "trayBadgeEnabled":
          setTrayBadgeEnabled(Boolean(value));
          break;
        case "pinnedHoverMode":
          setPinnedHoverMode(normalizePinnedHoverMode(value));
          break;
        case "pinnedHoverOpacity": {
          const opacity = Number(value);
          if (Number.isFinite(opacity)) setPinnedHoverOpacity(Math.min(0.9, Math.max(0.05, opacity)));
          break;
        }
        default:
          break;
      }
    });
    return () => {
      stopPromise.then((stop) => stop?.());
    };
  }, []);

  const handleCardLiveDragScale = useCallback((nextScale) => {
    setUiScale(nextScale);
    runLatestWindowCorrection(() => applyLiveCompactScale(nextScale, { persist: false }));
  }, []);

  const handleCardCommitDragScale = useCallback((nextScale) => {
    const applied = setWindowUiScale(nextScale);
    setUiScale(applied);
    broadcastStateSync("uiScale", applied);
    runLatestWindowCorrection(() => applyLiveCompactScale(applied, { persist: true }));
  }, []);

  const handleStripLiveDragScale = useCallback((nextScale, base) => {
    setStripScaleState(nextScale);
    runLatestWindowCorrection(() => applyLiveStripScale(nextScale, base, { persist: false }));
  }, []);

  const handleStripCommitDragScale = useCallback((nextScale, base) => {
    const applied = setStripScale(nextScale);
    setStripScaleState(applied);
    broadcastStateSync("stripScale", applied);
    runLatestWindowCorrection(() => applyLiveStripScale(applied, base, { persist: true }));
  }, []);

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
      // 余额窗口装的是金额：菜单栏/托盘只有一个裸数字位，写 ¥68.5 会被读成
      // "剩余 68%"。与其误导，宁可显示 "--"；金额在卡片、胶囊与 tooltip 里完整呈现。
      if (isBalanceWindow(cell.tightest)) return { remaining: null, stale: false };
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
    // 只有常驻的 main（compact）窗口负责更新 Windows 托盘数字，避免多窗口互相覆盖。
    if (!IS_WINDOWS || viewMode !== "compact") return;
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
    // 悬停提示是送到原生托盘的文字，切换语言时要重发。
  }, [trayBadge, trayBadgeEnabled, language, viewMode]);

  // 勾选即追加到末尾（勾选顺序 = 显示顺序）；首次改动时以当前自动列表为基准。
  const handleToggleStripAgent = useCallback((agentId) => {
    setStripAgentsSetting((current) => {
      const base = current ?? autoStripAgentsRef.current;
      const next = base.includes(agentId)
        ? base.filter((id) => id !== agentId)
        : [...base, agentId];
      if (!next.length) return current; // 至少保留一个
      localStorage.setItem("metrik:stripAgents", JSON.stringify(next));
      broadcastStateSync("stripAgents", next);
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
      broadcastStateSync("stripAgents", base);
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
  // 百分比要加粗，按占位符拆成前后两段，中间插入 <strong>。
  const comparisonParts = (snapshot.period === "today"
    ? (comparisonIsLower ? t("比近 7 日同时段低 {percent}") : t("比近 7 日同时段高 {percent}"))
    : (comparisonIsLower ? t("比前一周期低 {percent}") : t("比前一周期高 {percent}"))).split("{percent}");
  const flatComparisonLabel = snapshot.period === "today" ? t("与近 7 日同时段持平") : t("与前一周期持平");
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
    // macOS：菜单栏状态图标承担零占地摘要；面板只保留紧凑卡片，主面板另开标准窗口。
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
    // Windows / Linux 桌面端：卡片、胶囊、主面板各自独立窗口，互不隐藏。
    if (isDesktop()) {
      if (nextMode === "expanded") {
        runWindowAction(() => openExpandedWindow());
        return;
      }
      if (nextMode === "compact" || nextMode === "strip") {
        handleSetSurfaceVisible(nextMode, true);
        return;
      }
    }
    const fromMode = viewModeRef.current;
    invalidateWindowCorrections();
    previousViewModeRef.current = fromMode;
    const fromPositionMode = fromMode === "strip"
      ? stripPositionMode(stripOrientationRef.current)
      : fromMode;
    setViewMode(nextMode);
    if (nextMode === "compact") setActiveNav("overview");
    if (nextMode !== "expanded") localStorage.setItem("metrik:viewMode", nextMode);
    if (nextMode === "strip") return;
    runWindowAction(async () => {
      await applyWindowMode(nextMode, { fromPositionMode });
      if (nextMode === "compact") await setWindowPinned(pinnedRef.current);
    });
  }, [handleSetSurfaceVisible]);

  // 小组件上的更新提示点：点击直达主面板设置页。
  const handleOpenUpdate = useCallback(() => {
    if (isDesktop()) {
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
    emitSyncedState("pinned", value);
    // 主面板始终是普通窗口；只在悬浮窗口上应用置顶，避免在 Linux 上误停悬浮悬停监听。
    runWindowAction(async () => {
      if (viewModeRef.current !== "expanded") {
        invalidateWindowCorrections();
        await collapseVerticalStripHover();
        await setWindowPinned(value);
      }
      await syncLinuxTrayPinned(value);
    });
  }, [emitSyncedState]);

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
      text: "正在清理派生统计索引并重建当前周期…",
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
        ...(next.isDemo
          ? { text: "演示扫描完成。" }
          : snapshotIsPartial(next)
            ? { text: "扫描完成，仍有记录无法解析。更新 Metrik 后再试。" }
            : { text: "扫描完成 · {time}", params: { time: formatClock(next.generatedAt) } }),
      });
    } catch (error) {
      console.warn("Unable to rebuild the local ledger.", error);
      setRebuildState({
        status: "error",
        text: "扫描失败，稍后重试。Agent 日志未受影响。",
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
    broadcastStateSync("snapshotRefresh", Date.now());
  }, [loadSnapshot]);

  if (viewMode === "strip" && !IS_MAC) {
    return (
      <>
        <StripBar
          snapshot={snapshot}
          agents={stripAgents}
          pinned={pinned}
          loading={appBusy}
          glassAlpha={shellGlassAlpha}
          rawGlassAlpha={glassAlpha}
          glassMode={glassMode}
          glassTint={glassTint}
          glassInk={glassInk}
          orientation={stripOrientation}
          onToggleOrientation={handleToggleStripOrientation}
          onTogglePinned={handleTogglePinned}
          onDragScale={handleStripLiveDragScale}
          onCommitDragScale={handleStripCommitDragScale}
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
          glassMode={glassMode}
          glassTint={glassTint}
          glassInk={glassInk}
          onPeriodChange={setPeriod}
          onOpenSources={() => setDrawerOpen(true)}
          onTogglePinned={handleTogglePinned}
          onToggleTransparent={handleToggleTransparent}
          onExpand={handleWindowMode}
          onToggleSurface={isDesktop() && !IS_MAC ? handleToggleSurface : undefined}
          surfaceVisibility={surfaceVisibility}
          onDragScale={handleCardLiveDragScale}
          onCommitDragScale={handleCardCommitDragScale}
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
      <div className={`app-shell app-shell--expanded ${appBusy ? "is-loading" : ""} ${!IS_MAC ? "app-shell--native-resize" : ""}`}>
        {/* macOS 的完整视图是标准窗口：拖动和窗口按钮都归原生标题栏，不自绘。 */}
        {!IS_MAC && (
          <>
            <div className="expanded-drag-region" data-tauri-drag-region aria-hidden="true" />
            <WindowActions
              mode="expanded"
              pinned={pinned}
              theme={theme}
              darkTheme={darkTheme}
              surfaceVisibility={surfaceVisibility}
              onThemeChange={handleThemeChange}
              onToggleMode={handleWindowMode}
              onToggleSurface={isDesktop() && !IS_MAC ? handleToggleSurface : undefined}
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
          aria-label={t("强制刷新官方配额与本地统计")}
          title={t("强制刷新官方配额与本地统计")}
        >
          <ArrowsClockwise size={15} weight="light" aria-hidden="true" />
        </button>
        <Sidebar
          activeNav={activeNav}
          onNavChange={handleNavChange}
          snapshot={snapshot}
          loading={appBusy}
          nextRefreshAt={nextRefreshAt}
        />

        {activeNav === "overview" ? (
          <>
            <PeriodControl period={period} onChange={setPeriod} />
            <main className="dashboard">
              <header className="hero-copy">
                <span className="section-kicker">{t(PERIODS.find((item) => item.id === snapshot.period)?.label)}</span>
                <div className="metric-line" aria-live="polite" aria-atomic="true">
                  <h1>{snapshot.pending || snapshot.loadError ? "--" : compactTokens(visibleTokens)}</h1>
                  <span>tokens</span>
                </div>
                <p className="comparison">
                  {switchingPeriod ? (
                    <>
                      <ClockCounterClockwise size={22} weight="light" aria-hidden="true" />
                      {t("正在统计{period}数据，暂显示{shown}", {
                        period: t(PERIODS.find((item) => item.id === period)?.label),
                        shown: t(PERIODS.find((item) => item.id === snapshot.period)?.label),
                      })}
                    </>
                  ) : snapshot.pending ? (
                    <>
                      <ClockCounterClockwise size={22} weight="light" aria-hidden="true" />
                      {t("正在建立本地索引，窗口仍可操作")}
                    </>
                  ) : snapshot.loadError ? (
                    <>
                      <ClockCounterClockwise size={22} weight="light" aria-hidden="true" />
                      {t("本地数据读取失败，未显示演示数字")}
                    </>
                  ) : selectedAgent !== "all" ? (
                    <>
                      <FunnelSimple size={22} weight="light" aria-hidden="true" />
                      {t("仅显示 {agent} 用量", { agent: AGENT_META[selectedAgent].label })}
                    </>
                  ) : snapshot.comparisonAvailable ? (
                    <>
                      {comparisonIsFlat ? (
                        flatComparisonLabel
                      ) : (
                        <>
                          <ComparisonArrow size={22} weight="bold" aria-hidden="true" />
                          {comparisonParts[0]}
                          <strong>{Math.abs(snapshot.comparisonPercent).toFixed(0)}%</strong>
                          {comparisonParts[1]}
                        </>
                      )}
                    </>
                  ) : (
                    <>
                      <ClockCounterClockwise size={22} weight="light" aria-hidden="true" />
                      {period === "today" ? t("近 7 日同时段基线尚未建立") : t("前一周期基线尚未建立")}
                    </>
                  )}
                </p>
              </header>

              {snapshot.pending || snapshot.loadError ? (
                <ChartState pending={snapshot.pending} />
              ) : (
                <>
                  <UsageChart snapshot={snapshot} selectedAgent={selectedAgent} dark={darkTheme} />
                  <BreakdownSection
                    snapshot={snapshot}
                    selectedAgent={selectedAgent}
                    onOpenModelSpeed={handleOpenModelSpeed}
                  />
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
            onSnapshotRefresh={() => {
              loadSnapshot(currentPeriod.current);
              broadcastStateSync("snapshotRefresh", Date.now());
            }}
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
            edgeAutoHide={edgeAutoHide}
            onEdgeAutoHideChange={handleEdgeAutoHideChange}
            surfaceVisibility={surfaceVisibility}
            onSetSurfaceVisible={handleSetSurfaceVisible}
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
        ) : (
          <>
            <PeriodControl period={period} onChange={setPeriod} fullWidthArea />
            <UsageSection
              snapshot={snapshot}
              projectsState={projectsState}
              sessionsState={sessionsState}
              period={period}
              activeView={usageView}
              onViewChange={setUsageView}
              jumpRequest={usageJumpRequest}
              onRulesChanged={() => setUsageReloadNonce((value) => value + 1)}
            />
          </>
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
