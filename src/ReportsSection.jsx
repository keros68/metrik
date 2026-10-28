import { useLayoutEffect, useRef, useState } from "react";
import { ArrowDown, ArrowUp } from "@phosphor-icons/react";
import { modelDisplayName } from "./modelNames.js";
import { compactTokens } from "./tokenFormat.js";
import { formatDate, t } from "./i18n.js";
import { AGENT_META, AGENT_ORDER, chartColor } from "./agentMeta.jsx";
import { ModelResponseSpeed, ModelResponseSpeedHelp } from "./viewParts.jsx";
import { PROJECT_COLOR_COUNT, ShareDonut } from "./UsageSection.jsx";

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
    return <p className="settings-muted" ref={hostRef}>{t("所选时间段内暂无已索引的用量。")}</p>;
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
        aria-label={t("近 {count} 周每周 token 用量趋势，按 Agent 分色", { count: weeks.length })}
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
      <div className="chart-legend chart-legend--report" aria-label={t("图例")}>
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
    return <p className="settings-muted">{t("所选时间段内暂无已索引的用量。")}</p>;
  }
  const total = rows.reduce((sum, agent) => sum + agent.tokens, 0) || 1;
  return (
    <div className="report-donut">
      <ShareDonut
        ariaLabel={t("近 {count} 周内各 Agent 用量占比环形图", { count: weeksCount })}
        segments={rows.map((agent) => ({
          key: agent.id,
          tokens: agent.tokens,
          color: AGENT_META[agent.id]?.accent || "#74767a",
        }))}
        total={total}
        centerTokens={totalTokens}
        caption={t("tokens · 近 {count} 周", { count: weeksCount })}
      />
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

export function ReportsSection({ report }) {
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
          <h1>{t("报告")}</h1>
          <p>{t("正在读取本地账本。报告只统计已索引的数据，不触发新的日志扫描。")}</p>
        </header>
      </main>
    );
  }
  const data = report.data;
  if (!data || data.loadError) {
    return (
      <main className="reports-section">
        <header className="settings-header">
          <h1>{t("报告")}</h1>
          <p>{t("本地账本读取失败，报告暂不可用；未以演示数据替代。稍后重试。")}</p>
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
    return { index, label: t("{month}月", { month: firstCell.month + 1 }) };
  }).filter(Boolean);
  const activeDayCount = data.days.filter((day) => day.tokens > 0).length;
  const coverageStart = Number.isFinite(data.firstEventMs)
    ? formatDate(data.firstEventMs)
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
        <h1 id="reports-title">{t("报告")}</h1>
        <p>
          <strong>{t("近 26 周活动")}</strong> · {t("只统计本地账本中已索引的数据（已解析 token 口径，非账单）。")}
          {coverageStart ? t("账本数据自 {date} 起。", { date: coverageStart }) : ""}
          {data.isDemo ? ` ${t("当前为浏览器演示数据。")}` : ""}
        </p>
      </header>

      <div className="report-stats">
        <div><strong>{compactTokens(data.totalTokens)}</strong><span>{t("26 周总量")}</span></div>
        <div><strong>{activeDayCount}</strong><span>{t("活跃天数")}</span></div>
        <div><strong>{data.streakDays}</strong><span>{t("连续活跃天数")}</span></div>
      </div>

      <section className="report-card" aria-label={t("活动可视化")}>
        <div className="report-toolbar">
          <div className="report-view-toggle" role="group" aria-label={t("切换图表形式")}>
            {REPORT_VIEWS.map((item) => (
              <button
                type="button"
                key={item.id}
                className={view === item.id ? "is-selected" : ""}
                aria-pressed={view === item.id}
                onClick={() => setView(item.id)}
              >
                {t(item.label)}
              </button>
            ))}
          </div>
          {view !== "heatmap" && view !== "projects" && (
            <div className="report-view-toggle" role="group" aria-label={t("统计时间段")}>
              {REPORT_RANGE_WEEKS.map((num) => (
                <button
                  type="button"
                  key={num}
                  className={rangeWeeks === num ? "is-selected" : ""}
                  aria-pressed={rangeWeeks === num}
                  onClick={() => handleRangeWeeks(num)}
                >
                  {t("{num} 周", { num })}
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
                    <small>{t("{count} 天", { count: project.activeDays })}</small>
                  </span>
                  <Sparkline
                    points={project.weekly}
                    color={index < PROJECT_COLOR_COUNT ? `var(--viz-${index + 1})` : "var(--viz-other)"}
                  />
                  <em>{compactTokens(project.tokens)}</em>
                  {project.recentDeltaPercent != null ? (
                    <small
                      className={project.recentDeltaPercent >= 0 ? "trend-up" : "trend-down"}
                      title={t("近 7 天相对再前 7 天")}
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
            <p className="settings-muted">{t("该时间段内暂无可归属的项目用量。")}</p>
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
        <div className="heatmap" style={{ "--heatmap-weeks": weeks.length }} role="img" aria-label={t("近 26 周每日 token 用量热力图，颜色越深用量越大")}>
          {weeks.map((week, weekIndex) => (
            <div className="heatmap-week" key={weekIndex}>
              {week.map((cell, dayIndex) => (
                cell ? (
                  <i
                    key={cell.key}
                    className={`heat-${heatLevel(cell.tokens, thresholds)}`}
                    title={`${cell.key} · ${cell.tokens ? `${compactTokens(cell.tokens)} tokens` : t("暂无用量")}`}
                  />
                ) : (
                  <i key={`pad-${weekIndex}-${dayIndex}`} className="heat-pad" aria-hidden="true" />
                )
              ))}
            </div>
          ))}
        </div>
        <div className="heatmap-scale" aria-hidden="true">
          <span>{t("少")}</span>
          <i className="heat-0" /><i className="heat-1" /><i className="heat-2" /><i className="heat-3" /><i className="heat-4" />
          <span>{t("多")}</span>
        </div>
          </>
        )}
        </div>
      </section>

      <div className="report-grid">
        <section className="report-card" aria-label={t("Agent 排行")}>
          <h2>{t("Agent 排行")}</h2>
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
                  <em>{compactTokens(agent.tokens)} · {t("{count} 天", { count: agent.activeDays })}</em>
                </li>
              );
            })}
          </ul>
        </section>

        <section className="report-card" aria-label={t("模型排行")}>
          <h2>{t("模型排行")}</h2>
          <ModelResponseSpeedHelp />
          <ul className="model-list model-list--response-speed">
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
                  <ModelResponseSpeed entry={entry} />
                </li>
              );
            })}
          </ul>
        </section>
      </div>

    </main>
  );
}
