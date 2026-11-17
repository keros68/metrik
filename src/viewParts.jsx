import { useEffect, useId, useMemo, useRef, useState } from "react";
import { ArrowDown, ArrowUp, Info } from "@phosphor-icons/react";
import { AGENT_META } from "./agentMeta.jsx";
import { formatNumber, t } from "./i18n.js";
import { modelDisplayName } from "./modelNames.js";
import {
  defaultModelSortDirection,
  extractModelTimingCells,
  formatDurationParts,
  formatRateParts,
  sortModelEntries,
} from "./modelSpeed.js";
import { formatClock } from "./quotaView.jsx";
import { compactTokens } from "./tokenFormat.js";

export const PERIODS = [
  { id: "today", label: "今日" },
  { id: "week", label: "7 天" },
  { id: "month", label: "30 天" },
];

/// 数据来源状态的文案。完整说明一律进 title。
/// title + hint 给紧凑卡片底栏；state 给侧栏——那里主标题恒为「数据统计」，
/// 状态由灯和 state 这一行承担，不把"数据不完整"顶成入口的名字。
/// "部分覆盖" 这种自造词读者看不懂，直接说清楚哪里不全。
export function sourceStatusCopy(snapshot, loading, partial) {
  const indexingPending = snapshot.indexing?.pending || 0;
  if (snapshot.pending) {
    return {
      title: t("正在读取"),
      hint: t("首次扫描"),
      state: t("正在读取"),
      detail: t("正在读取本机日志，日志库较大时首次扫描需要几分钟。"),
    };
  }
  if (snapshot.loadError) {
    return {
      title: t("读取失败"),
      hint: t("点击排查"),
      state: t("读取失败"),
      detail: t("本机日志读取失败。界面不会以演示数据替代，点击查看数据来源。"),
    };
  }
  if (indexingPending > 0) {
    return {
      title: t("补齐历史"),
      hint: t("还剩 {count}", { count: indexingPending }),
      // 侧栏那行不能换行，只放数字；完整说明走 title 与统计说明抽屉。
      state: t("补齐中 {count}", { count: indexingPending }),
      detail: t("正在补齐历史索引，还剩 {count} 个日志文件。历史周期的数字尚不完整，会随补齐自动更新。", { count: indexingPending }),
    };
  }
  if (partial) {
    return {
      title: t("数据不完整"),
      hint: t("点击查看"),
      state: t("数据不完整"),
      detail: t("部分日志未能解析，统计数字低于真实用量。点击查看受影响的来源。"),
    };
  }
  if (snapshot.isDemo) {
    return {
      title: t("演示数据"),
      hint: t("非真实用量"),
      state: t("演示数据"),
      detail: t("当前显示的是演示数据，不是本机真实用量。"),
    };
  }
  return {
    title: t("数据源正常"),
    hint: loading ? t("更新中") : formatClock(snapshot.generatedAt),
    state: loading ? t("更新中") : t("更新于 {time}", { time: formatClock(snapshot.generatedAt) }),
    detail: t("各项数据均可溯源。"),
  };
}

export function PeriodControl({ period, onChange, compact = false, fullWidthArea = false }) {
  return (
    <div
      className={`period-control ${compact ? "period-control--compact" : ""} ${fullWidthArea ? "period-control--full" : ""}`}
      role="group"
      aria-label={t("统计周期")}
    >
      {PERIODS.map((item) => (
        <button
          type="button"
          key={item.id}
          className={period === item.id ? "is-selected" : ""}
          aria-pressed={period === item.id}
          onClick={() => onChange(item.id)}
        >
          {t(item.label)}
        </button>
      ))}
    </div>
  );
}

export function formatUsd(value) {
  const amount = Number(value || 0);
  const decimals = amount >= 100 ? 0 : amount >= 10 ? 1 : 2;
  return `$${amount.toFixed(decimals)}`;
}

export function ModelResponseSpeedHelp({ align = "start" }) {
  const anchor = useRef(null);
  const tooltipId = useId();
  const [dismissed, setDismissed] = useState(false);
  useEffect(() => {
    const dismiss = (event) => {
      if (event.key === "Escape" && anchor.current?.matches(":hover, :focus-within")) {
        setDismissed(true);
      }
    };
    document.addEventListener("keydown", dismiss);
    return () => document.removeEventListener("keydown", dismiss);
  }, []);
  return (
    <div
      ref={anchor}
      className={`model-speed-help ${align === "end" ? "model-speed-help--end" : ""}`}
      data-dismissed={dismissed || undefined}
      onMouseEnter={() => setDismissed(false)}
      onFocusCapture={() => setDismissed(false)}
    >
      <button type="button" className="model-speed-help-button" aria-describedby={tooltipId}>
        {t("响应计时")}
        <Info size={13} weight="regular" aria-hidden="true" />
      </button>
      <span id={tooltipId} role="tooltip" className="model-speed-tooltip">
        {t("响应均速按包含等待的总耗时计算。首 token 延迟和请求总耗时取平均值，流式速度按流式阶段的总耗时计算。")}
      </span>
    </div>
  );
}

const SPEED_TABLE_COLUMNS = [
  { key: "tokens", label: "用量占比", kind: "usage" },
  { key: "firstToken", label: "首 token 延迟", kind: "duration" },
  { key: "stream", label: "流式速度", kind: "rate" },
  { key: "response", label: "均速（含等待）", title: "响应均速（含等待）", kind: "rate" },
  { key: "duration", label: "请求总耗时", kind: "duration" },
];

function MetricCell({ cell, kind }) {
  if (!cell.available) {
    return (
      <td className="model-col-metric model-col-metric--empty" aria-label={t("暂无计时")}>
        <span aria-hidden="true">—</span>
      </td>
    );
  }
  const parts = kind === "duration" ? formatDurationParts(cell.value) : formatRateParts(cell.value);
  return (
    <td className="model-col-metric">
      <strong>
        {parts.number} <span className="model-metric-unit">{parts.unit}</span>
      </strong>
      <small>{t("{count} 样本", { count: formatNumber(cell.sampleCount) })}</small>
    </td>
  );
}

export function ModelSpeedTable({ entries, ariaLabel, emptyText }) {
  const [sort, setSort] = useState({ key: "tokens", dir: "desc" });
  const maxTokens = useMemo(
    () => (entries || []).reduce((max, item) => Math.max(max, Number(item.tokens) || 0), 0),
    [entries],
  );
  const sorted = useMemo(
    () => sortModelEntries(entries, sort.key, sort.dir),
    [entries, sort.key, sort.dir],
  );

  const toggleSort = (key) => {
    setSort((current) => (
      current.key === key
        ? { key, dir: current.dir === "asc" ? "desc" : "asc" }
        : { key, dir: defaultModelSortDirection(key) }
    ));
  };

  if (!sorted.length) {
    return <p className="settings-muted">{emptyText || t("当前筛选条件下暂无模型记录。")}</p>;
  }

  return (
    <div className="model-table-scroll">
      <table className="model-speed-table" aria-label={ariaLabel || t("模型用量与响应速率")}>
        <thead>
          <tr>
            <th scope="col" className="model-col-name">{t("模型")}</th>
            {SPEED_TABLE_COLUMNS.map((column) => {
              const active = sort.key === column.key;
              const SortIcon = sort.dir === "asc" ? ArrowUp : ArrowDown;
              return (
                <th
                  scope="col"
                  key={column.key}
                  className={column.kind === "usage" ? "model-col-usage" : "model-col-metric"}
                  aria-sort={active ? (sort.dir === "asc" ? "ascending" : "descending") : "none"}
                >
                  <button
                    type="button"
                    className={`model-sort-button ${active ? "is-active" : ""}`}
                    title={column.title ? t(column.title) : undefined}
                    onClick={() => toggleSort(column.key)}
                  >
                    <span>{t(column.label)}</span>
                    {active && <SortIcon size={11} weight="bold" aria-hidden="true" />}
                  </button>
                </th>
              );
            })}
          </tr>
        </thead>
        <tbody>
          {sorted.map((entry) => {
            const meta = AGENT_META[entry.agent];
            const accent = meta?.accent || "#74767a";
            const agentLabel = meta?.label || entry.agent;
            const displayName = modelDisplayName(entry.model);
            const cells = extractModelTimingCells(entry);
            const barRatio = maxTokens > 0 ? Math.max(0, Math.min(1, entry.tokens / maxTokens)) : 0;
            return (
              <tr key={`${entry.agent}-${entry.model}`}>
                <td className="model-col-name">
                  <div className="model-table-id">
                    <i className="model-dot" style={{ backgroundColor: accent }} aria-hidden="true" />
                    <div className="model-table-names">
                      <strong title={displayName}>{displayName}</strong>
                      <small>{agentLabel}</small>
                    </div>
                  </div>
                </td>
                <td className="model-col-usage">
                  <div className="model-table-usage">
                    <div className="model-table-usage-figures">
                      <strong>{compactTokens(entry.tokens)}</strong>
                      <small>{Number(entry.share || 0).toFixed(1)}%</small>
                    </div>
                    <span className="model-track" aria-hidden="true">
                      <i style={{ transform: `scaleX(${barRatio})`, backgroundColor: accent }} />
                    </span>
                  </div>
                </td>
                <MetricCell cell={cells.firstToken} kind="duration" />
                <MetricCell cell={cells.stream} kind="rate" />
                <MetricCell cell={cells.response} kind="rate" />
                <MetricCell cell={cells.duration} kind="duration" />
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
