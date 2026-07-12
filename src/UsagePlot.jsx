import { useEffect, useMemo, useRef } from "react";
import uPlot from "uplot";

const AXIS_FONT = '12px "Geist Variable", "Segoe UI Variable", sans-serif';

function shortTokens(value) {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000) return `${Math.round(value / 1_000)}K`;
  return String(Math.round(value));
}

export function UsagePlot({ series, showCodex, showClaude, selectedAgent, formatTokens }) {
  const shellRef = useRef(null);
  const hostRef = useRef(null);
  const tooltipRef = useRef(null);
  const tooltipTimeRef = useRef(null);
  const tooltipValueRef = useRef(null);
  const lineColor = selectedAgent === "claude" ? "#e36b49" : "#246bdb";
  const lineFill = selectedAgent === "claude" ? "rgba(227, 107, 73, 0.11)" : "rgba(36, 107, 219, 0.11)";
  const accessibleRows = useMemo(() => series.map((point) => ({
    label: point.label,
    value: (showCodex ? Number(point.codex || 0) : 0) + (showClaude ? Number(point.claude || 0) : 0),
  })), [series, showClaude, showCodex]);

  useEffect(() => {
    const shell = shellRef.current;
    const host = hostRef.current;
    const tooltip = tooltipRef.current;
    if (!shell || !host || !tooltip || !series.length) return undefined;

    const data = [
      series.map((_, index) => index),
      series.map((point) =>
        (showCodex ? Number(point.codex || 0) : 0) +
        (showClaude ? Number(point.claude || 0) : 0)),
    ];

    const visibleTotal = (index) => data[1][index];
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
          {
            label: "当前周期",
            stroke: lineColor,
            width: 2,
            fill: lineFill,
            paths: uPlot.paths.spline(),
            points: { show: false },
          },
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
              tooltipValueRef.current.textContent = `${formatTokens(visibleTotal(index))} tokens`;

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
  }, [formatTokens, lineColor, lineFill, selectedAgent, series, showClaude, showCodex]);

  return (
    <div className="usage-plot-shell" ref={shellRef} style={{ "--series-color": lineColor }}>
      <div className="usage-plot" ref={hostRef} />
      <div className="chart-tooltip chart-tooltip--floating" ref={tooltipRef} hidden>
        <span ref={tooltipTimeRef} />
        <strong ref={tooltipValueRef} />
      </div>
      <table className="sr-only">
        <caption>当前筛选条件下的用量趋势数据</caption>
        <thead><tr><th scope="col">时间</th><th scope="col">tokens</th></tr></thead>
        <tbody>
          {accessibleRows.map((row, index) => (
            <tr key={`${row.label}-${index}`}><th scope="row">{row.label}</th><td>{formatTokens(row.value)}</td></tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
