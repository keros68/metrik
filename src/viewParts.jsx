import { useEffect, useRef, useState } from "react";
import { Info } from "@phosphor-icons/react";
import { formatNumber, t } from "./i18n.js";
import { formatClock } from "./quotaView.jsx";

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

export function ModelResponseSpeedHelp() {
  const anchor = useRef(null);
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
      className="model-speed-help"
      data-dismissed={dismissed || undefined}
      onMouseEnter={() => setDismissed(false)}
      onFocusCapture={() => setDismissed(false)}
    >
      <button type="button" className="model-speed-help-button" aria-describedby="model-speed-explanation">
        {t("响应均速（含等待）")}
        <Info size={13} weight="regular" aria-hidden="true" />
      </button>
      <span id="model-speed-explanation" role="tooltip" className="model-speed-tooltip">
        {t("历史响应均速：输出 token 总数 ÷ 总耗时（含等待和思考）。")}
      </span>
    </div>
  );
}

export function ModelResponseSpeed({ entry }) {
  const rate = entry.responseTokensPerSecond;
  const count = entry.responseSampleCount;
  const available = Number.isFinite(rate) && rate > 0 && Number.isInteger(count) && count > 0;
  return (
    <div className={`model-response-speed ${available ? "" : "model-response-speed--unavailable"}`}>
      {available ? (
        <>
          <strong>{rate > 0 && rate < 0.1 ? "<0.1" : formatNumber(rate, { maximumFractionDigits: 1 })} <span>tokens/s</span></strong>
          <small>{t("{count} 个有效样本", { count: formatNumber(count) })}</small>
        </>
      ) : <span>{t("暂无计时")}</span>}
    </div>
  );
}
