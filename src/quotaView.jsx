import {
  bindingWindow,
  formatReset,
  isBalanceWindow,
  QUOTA_LOW_REMAINING,
} from "./quotaWindows.js";
import { formatNumber, formatTime, t } from "./i18n.js";
import { AGENT_META } from "./agentMeta.jsx";

/// 状态灯的含义：绿 = 数据正常，黄（呼吸）= 正在更新，红 = 读取失败。
/// 灯本身只是装饰，含义必须悬浮可见，否则用户永远猜不到。
export function statusDotTitle(loading, loadError) {
  if (loadError) return t("数据读取失败，仍显示上次成功的数据");
  return loading ? t("正在更新数据…") : t("数据正常");
}

export function exactTokens(value) {
  return formatNumber(value || 0);
}

export function formatClock(isoString) {
  if (!isoString) return "--:--";
  const value = new Date(isoString);
  if (Number.isNaN(value.getTime())) return "--:--";
  return formatTime(value, {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
}

function formatQuotaAge(minutes) {
  if (!Number.isFinite(minutes) || minutes < 1) return t("刚刚");
  if (minutes < 60) return t("{count} 分钟前", { count: Math.round(minutes) });
  if (minutes < 1440) return t("{count} 小时前", { count: Math.floor(minutes / 60) });
  return t("{count} 天前", { count: Math.floor(minutes / 1440) });
}

export function quotaProvenance(quota) {
  if (!quota.available) return t("暂无可靠来源");
  if (quota.quality === "demo") return t("演示数据");
    if (quota.resetExpired) return t("已重置 · 等待刷新");
  if (quota.stale || quota.quality === "official_snapshot") {
    return t("官方快照 · {age}", { age: formatQuotaAge(quota.ageMinutes) });
  }
  return t("官方 · 实时");
}

export function snapshotIsPartial(snapshot) {
  return snapshot.sources?.some((source) => source.quality === "partial") || false;
}

export const UNAVAILABLE_QUOTA = {
  available: false,
  remainingPercent: 0,
  resetsInMinutes: null,
  ageMinutes: null,
  stale: false,
  resetExpired: false,
  sourceLabel: "暂无可靠来源",
  quality: "unavailable",
};

export function agentQuotaFor(snapshot, agentId) {
  return (
    snapshot.agentQuotas?.find((entry) => entry.agent === agentId) || {
      agent: agentId,
      windows: [],
    }
  );
}

export function quotaHasData(entry) {
  return Boolean(entry?.windows?.some((window) => window.view.available));
}

/// 没有额度数字时该说什么。后端在 note 里带上直连失败的真实原因（凭据过期、
/// 缺 scope、限流……）；有原因就别再叫用户去开状态栏钩子——他多半已经开了
/// 直连，而且不用 Claude Code 的人开了钩子也不会有数据。
export function quotaEmptyCopy(entry, agentId, short = false) {
  if (entry?.note) return short ? t("直连查询失败") : t("直连查询失败 · 详见设置");
  if (agentId !== "claude") return short ? t("官方配额不可用") : t("暂无可靠来源");
  return short ? t("设置中开启状态栏钩子") : t("在设置中开启状态栏钩子后显示");
}

export function shortWindowLabel(key) {
  if (key === "five_hour" || key === "primary") return "5h";
  if (key === "seven_day" || key === "secondary") return "7d";
  if (key === "extra_usage") return t("超额");
  if (key === "credits") return t("额度");
  if (key === "monthly_cycle") return t("月度");
  if (key.startsWith("balance")) return t("余额", { context: "short" });
  if (key.endsWith("_5h")) return key.replace(/_5h$/, "").slice(0, 3) + " 5h";
  if (key.endsWith("_weekly")) return key.replace(/_weekly$/, "").slice(0, 3) + " 7d";
  if (key.endsWith("_7d")) return key.replace(/_7d$/, "").slice(0, 3) + " 7d";
  return key.replace(/^seven_day_/, "").slice(0, 4);
}

// 小插件配额卡固定两行：优先取来源的前两个窗口，缺则补占位。
// 占位只补真实窗口没占用的标签——prolite 套餐唯一的真实窗口就是周窗（7d），
// 再补一个 seven_day 占位会出现两行 "7d"（实机踩过）。
export function compactQuotaWindows(entry) {
  const rows = (entry.windows || []).slice(0, 2);
  const usedLabels = new Set(rows.map((window) => shortWindowLabel(window.key)));
  const placeholders = [
    { key: "five_hour", label: "Session", view: UNAVAILABLE_QUOTA },
    { key: "seven_day", label: t("每周"), view: UNAVAILABLE_QUOTA },
  ].filter((placeholder) => !usedLabels.has(shortWindowLabel(placeholder.key)));
  while (rows.length < 2 && placeholders.length) {
    rows.push(placeholders.shift());
  }
  return rows;
}

// 每行 Agent 的 tooltip 列出全部有来源的窗口（已过期窗口也算有来源，单独走
// "已重置，等待刷新"文案）；一个都没有时由调用方渲染 "-- / 暂无可靠来源"，
// 绝不编造数字。不能截断：行上显示的是 bindingWindow 挑出来的那个，它可能排在
// 第三位（Kimi 的月度窗口就是），截断会出现"行上写着月度、tooltip 里没有月度"。
export function compactDisplayWindows(entry) {
  return (entry.windows || []).filter((window) => window.view.available);
}

// 原生 title tooltip：逐窗口列出剩余与重置倒计时，并标注官方/快照/演示来源，
// 让官方配额与本地解析用量始终可区分。
export function compactQuotaTooltip(agentId, windows) {
  if (!windows.length) return t("{agent}：暂无可靠来源", { agent: AGENT_META[agentId].label });
  const lines = windows.map((window) => {
    const view = window.view;
    const label = window.label || shortWindowLabel(window.key);
    if (view.resetExpired) return t("{label}：已重置，等待刷新", { label });
    // 余额窗口：金额不拼"剩余 X%"，也没有重置倒计时。
    if (isBalanceWindow(window)) {
      return t("{label}：余额 {amount} · {provenance}", {
        label,
        amount: formatBalance(window.key, view.remainingPercent),
        provenance: quotaProvenance(view),
      });
    }
    const reset = Number.isFinite(view.resetsInMinutes)
      ? ` · ${t("{time}后重置", { time: formatReset(view.resetsInMinutes) })}`
      : "";
    return t("{label}：剩余 {percent}%{reset} · {provenance}", {
      label,
      percent: Math.round(view.remainingPercent),
      reset,
      provenance: quotaProvenance(view),
    });
  });
  return [AGENT_META[agentId].label, ...lines].join("\n");
}

// 胶囊条一格展示当前真正约束用量的窗口，规则见 quotaWindows.js。
export function stripCellData(entry) {
  const windows = (entry.windows || []).filter(
    (window) => window.view.available && !window.view.resetExpired,
  );
  if (!windows.length) return null;
  return { tightest: bindingWindow(windows) || windows[0], windows };
}

export function conciseSourceDetail(source) {
  if (source.kind === "official") {
    return t("显示官方额度和重置时间。");
  }
  if (source.kind === "sync") {
    return source.quality === "partial"
      ? t("同步未完成，本机统计不受影响。")
      : t("已合并其他设备的统计数据。");
  }

  // scanSummary 是后端给的扫描摘要（已是当前语言），没有时为空。
  const scanSummary = source.scanSummary || "";
  if (source.quality === "partial") {
    return t("{summary}部分记录未计入。", { summary: scanSummary });
  }
  return scanSummary || t("本机数据已更新。");
}

export function conciseQualityLabel(source) {
  if (source.quality === "exact") return t("正常");
  if (source.quality === "partial") return t("不完整");
  return source.qualityLabel;
}

export function quotaUsedPercent(view) {
  return Math.min(100, Math.max(0, 100 - view.remainingPercent));
}

// 余额型窗口（balance_<币种>）的 remainingPercent 装的是金额，不是百分比：
// "剩余/已用 X%"的换算对它一律不成立，展示统一走这个货币格式——金额 ≥100
// 时不显示小数，否则保留两位。compact 供胶囊条格子使用：格子只有 32px，
// 两位小数必然溢出裁切，100 以下降到 1 位并去掉孤立的 .0。
export function formatBalance(key, value, compact = false) {
  const amount = Number(value);
  if (!Number.isFinite(amount)) return "--";
  const symbol = key === "balance_cny" ? "¥" : key === "balance_usd" ? "$" : "";
  const text = amount.toFixed(amount >= 100 ? 0 : compact ? 1 : 2);
  return symbol + (compact ? text.replace(/\.0$/, "") : text);
}

function windowLengthMinutes(key) {
  if (key === "five_hour" || key === "primary") return 300;
  if (key === "seven_day" || key === "secondary" || key?.startsWith?.("seven_day")) return 10080;
  return null;
}

// 接近耗尽的分级警示：85% 起提醒、95% 起告急（四个竞品一致的做法）。
export function quotaSeverity(view, key) {
  // 余额窗口装的是金额：¥10 不是"剩余 10%"，永远走中性档，不进 warn/critical。
  if (isBalanceWindow({ key })) return "";
  if (!view.available || view.resetExpired) return "";
  const used = quotaUsedPercent(view);
  if (used >= 95) return "critical";
  // 与 bindingWindow 的"告急"同一条线：越过它，这个窗口才会顶到行首。
  if (used >= 100 - QUOTA_LOW_REMAINING) return "warn";
  return "";
}

export function StripDetailCard({ agentId, cell, cardRef }) {
  const meta = AGENT_META[agentId];
  const firstView = cell?.windows?.[0]?.view;
  const stale = firstView && (firstView.stale || firstView.quality === "official_snapshot");
  return (
    <aside ref={cardRef} className="strip-detail-card" role="tooltip" aria-label={t("{agent} 用量详情", { agent: meta.label })}>
      <header className="strip-detail-header">
        <img src={meta.iconSrc} className={meta.iconClass || ""} alt="" draggable={false} />
        <strong>{t("{agent} 用量", { agent: meta.label })}</strong>
      </header>
      {cell?.windows?.length ? (
        <div className="strip-detail-metrics">
          {cell.windows.map((window) => {
            const view = window.view;
            // 余额窗口：金额没有"已用占比"可画，轨道画满（中性表达），数值用货币格式。
            const balance = isBalanceWindow(window);
            const used = Math.round(quotaUsedPercent(view));
            const severity = quotaSeverity(view, window.key);
            const reset = Number.isFinite(view.resetsInMinutes)
              ? t("{time}后重置", { time: formatReset(view.resetsInMinutes) })
              : t("重置时间不可用");
            return (
              <section className="strip-detail-metric" key={window.key}>
                <span>{window.label || shortWindowLabel(window.key)}</span>
                <div className="strip-detail-track" aria-hidden="true">
                  <i
                    className={severity ? `strip-detail-fill--${severity}` : ""}
                    style={{ width: balance ? "100%" : `${used}%`, backgroundColor: severity ? undefined : meta.accent }}
                  />
                </div>
                <p>
                  <strong>{balance ? t("余额 {amount}", { amount: formatBalance(window.key, view.remainingPercent) }) : t("已用 {percent}%", { percent: used })}</strong>
                  <small>{balance ? t("账户余额 · 不重置") : reset}</small>
                </p>
              </section>
            );
          })}
        </div>
      ) : (
        <p className="strip-detail-empty">{t("官方配额暂不可用")}</p>
      )}
      {stale && <footer>{t("官方快照 · {age}", { age: formatQuotaAge(firstView.ageMinutes) })}</footer>}
    </aside>
  );
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

export function QuotaBarRow({ label, view, windowKey, accent }) {
  // 余额窗口：金额没有"已用占比"，轨道画满作为中性表达，数值用货币格式。
  const balance = isBalanceWindow({ key: windowKey });
  const severity = quotaSeverity(view, windowKey);
  const pace = balance ? null : quotaPace(view, windowKey);
  return (
    <>
      <div
        className={`quota-bar-row ${severity ? `quota-bar-row--${severity}` : ""}`}
        style={accent ? { "--quota-accent": accent } : undefined}
      >
        <small>{label}</small>
        <div className="quota-bar-track" aria-hidden="true">
          {/* 窗口已过期的快照不再显示旧百分比，避免把陈旧值当现状。 */}
          <i style={{ transform: `scaleX(${view.available && !view.resetExpired ? (balance ? 1 : quotaUsedPercent(view) / 100) : 0})` }} />
        </div>
        <em>{view.available && !view.resetExpired
          ? balance
            ? t("余额 {amount}", { amount: formatBalance(windowKey, view.remainingPercent) })
            : t("已用 {percent}%", { percent: Math.round(quotaUsedPercent(view)) })
          : "--"}</em>
        <span>
          {view.resetExpired
            ? t("已重置，等待刷新")
            : view.available
              ? balance
                ? t("账户余额 · 不重置")
                : t("{time}后重置", { time: formatReset(view.resetsInMinutes) })
              : t("暂不可用")}
        </span>
      </div>
      {pace && (
        <small className={`quota-pace ${pace.tone === "behind" ? "quota-pace--behind" : ""}`}>
          {pace.tone === "ahead"
            ? t("节奏从容 {delta}% · 按当前用量可维持至重置", { delta: Math.abs(pace.delta).toFixed(0) })
            : pace.tone === "close"
              ? t("节奏略偏快 {delta}% · 接近临界节奏", { delta: pace.delta.toFixed(0) })
              : t("节奏偏快 {delta}% · 按当前用量重置前可能耗尽", { delta: pace.delta.toFixed(0) })}
        </small>
      )}
    </>
  );
}
