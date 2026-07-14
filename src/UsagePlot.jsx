import { useEffect, useMemo, useRef } from "react";
import uPlot from "uplot";

const AXIS_FONT = '12px "Geist Variable", "Segoe UI Variable", sans-serif';

function shortTokens(value) {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000) return `${Math.round(value / 1_000)}K`;
  return String(Math.round(value));
}

// 图表专用降饱和配色（与报告页一致）：品牌色直接上图偏"纯"，柔和一档。
const AGENT_LINE_COLORS = {
  codex: { stroke: "#5586d4", fill: "rgba(85, 134, 212, 0.09)" },
  claude: { stroke: "#d98663", fill: "rgba(217, 134, 99, 0.09)" },
  zcode: { stroke: "#8b80d9", fill: "rgba(139, 128, 217, 0.09)" },
  opencode: { stroke: "#4aa392", fill: "rgba(74, 163, 146, 0.09)" },
  kimi: { stroke: "#6f8fd6", fill: "rgba(111, 143, 214, 0.09)" },
  antigravity: { stroke: "#6b8fe4", fill: "rgba(107, 143, 228, 0.09)" },
  default: { stroke: "#5586d4", fill: "rgba(85, 134, 212, 0.09)" },
};

function agentPalette(agent) {
  return AGENT_LINE_COLORS[agent] || AGENT_LINE_COLORS.default;
}

export function UsagePlot({ series, visibleAgents, selectedAgent, agentLabels = {}, formatTokens }) {
  const shellRef = useRef(null);
  const hostRef = useRef(null);
  const tooltipRef = useRef(null);
  const tooltipTimeRef = useRef(null);
  const tooltipValueRef = useRef(null);

  // "全部"时按 Agent 分色画多条线，只画周期内有数据的；全为零时保底一条，避免空图。
  const activeAgents = useMemo(() => {
    if (selectedAgent !== "all") return [selectedAgent];
    const withData = visibleAgents.filter((agent) =>
      series.some((point) => Number(point.tokens?.[agent] || 0) > 0),
    );
    return withData.length ? withData : visibleAgents.slice(0, 1);
  }, [selectedAgent, series, visibleAgents]);
  const multiLine = activeAgents.length > 1;
  const primaryColor = agentPalette(activeAgents[0]).stroke;

  const accessibleRows = useMemo(() => series.map((point) => ({
    label: point.label,
    values: activeAgents.map((agent) => Number(point.tokens?.[agent] || 0)),
  })), [series, activeAgents]);

  useEffect(() => {
    const shell = shellRef.current;
    const host = hostRef.current;
    const tooltip = tooltipRef.current;
    if (!shell || !host || !tooltip || !series.length) return undefined;

    const data = [
      series.map((_, index) => index),
      ...activeAgents.map((agent) => series.map((point) => Number(point.tokens?.[agent] || 0))),
    ];

    const xStride = series.length <= 7 ? 1 : series.length <= 25 ? 4 : 5;

    const plot = new uPlot(
      {
        width: Math.max(320, host.clientWidth),
        height: Math.max(260, host.clientHeight),
        padding: [16, 28, 0, 8],
        legend: { show: false },
        cursor: {
          drag: { x: false, y: false },
          points: { size: 8, width: 3 },
        },
        scales: {
          x: { time: false },
          y: {
            range: (_u, min, max) => [Math.min(0, min), Math.max(1, max * 1.16)],
          },
        },
        axes: [
          {
            stroke: "#77797c",
            font: AXIS_FONT,
            size: 43,
            gap: 13,
            grid: { show: false },
            ticks: { show: false },
            splits: () => series.map((_, index) => index).filter((index) => index % xStride === 0),
            values: (_u, values) => values.map((value) => series[Math.round(value)]?.label || ""),
          },
          {
            stroke: "#77797c",
            font: AXIS_FONT,
            size: 49,
            gap: 8,
            ticks: { show: false },
            grid: { stroke: "rgba(80, 83, 88, 0.16)", width: 1, dash: [3, 6] },
            splits: (_u, _axis, _min, max) => {
              const step = Math.max(1, Math.ceil(max / 5 / 10_000) * 10_000);
              return Array.from({ length: Math.ceil(max / step) + 1 }, (_, index) => index * step);
            },
            values: (_u, values) => values.map(shortTokens),
          },
        ],
        series: [
          {},
          ...activeAgents.map((agent) => ({
            label: agentLabels[agent] || agent,
            stroke: agentPalette(agent).stroke,
            width: 2,
            // 低透明面积填充：多线时也保留，柔和不压盖。
            fill: agentPalette(agent).fill,
            paths: uPlot.paths.spline(),
            points: { show: false },
          })),
        ],
        hooks: {
          setCursor: [
            (u) => {
              const index = u.cursor.idx;
              if (index == null || index < 0 || !series[index]) {
                tooltip.hidden = true;
                return;
              }

              tooltip.hidden = false;
              tooltipTimeRef.current.textContent = series[index].label;
              const valueHost = tooltipValueRef.current;
              valueHost.replaceChildren();
              activeAgents.forEach((agent, agentIndex) => {
                const row = document.createElement("strong");
                row.style.setProperty("--series-color", agentPalette(agent).stroke);
                row.textContent = multiLine
                  ? `${agentLabels[agent] || agent} ${formatTokens(data[agentIndex + 1][index])}`
                  : `${formatTokens(data[agentIndex + 1][index])} tokens`;
                valueHost.appendChild(row);
              });

              const desiredLeft = u.cursor.left + 20;
              const maxLeft = Math.max(8, shell.clientWidth - tooltip.offsetWidth - 8);
              const left = Math.min(Math.max(8, desiredLeft), maxLeft);
              const top = Math.max(8, u.cursor.top - 62);
              tooltip.style.transform = `translate3d(${left}px, ${top}px, 0)`;
            },
          ],
          ready: [
            () => {
              tooltip.hidden = true;
            },
          ],
        },
      },
      data,
      host,
    );

    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      const { width, height } = entry.contentRect;
      plot.setSize({ width: Math.max(320, Math.floor(width)), height: Math.max(260, Math.floor(height)) });
    });
    observer.observe(host);

    return () => {
      observer.disconnect();
      plot.destroy();
      host.replaceChildren();
    };
  }, [activeAgents, agentLabels, formatTokens, multiLine, series]);

  return (
    <div className="usage-plot-shell" ref={shellRef} style={{ "--series-color": primaryColor }}>
      <div className="usage-plot" ref={hostRef} />
      <div className="chart-tooltip chart-tooltip--floating" ref={tooltipRef} hidden>
        <span ref={tooltipTimeRef} />
        <div className="chart-tooltip-values" ref={tooltipValueRef} />
      </div>
      <table className="sr-only">
        <caption>当前筛选条件下的用量趋势数据</caption>
        <thead>
          <tr>
            <th scope="col">时间</th>
            {activeAgents.map((agent) => (
              <th scope="col" key={agent}>{agentLabels[agent] || agent}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {accessibleRows.map((row, index) => (
            <tr key={`${row.label}-${index}`}>
              <th scope="row">{row.label}</th>
              {row.values.map((value, valueIndex) => (
                <td key={valueIndex}>{formatTokens(value)}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
