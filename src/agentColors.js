// 图表专用降饱和配色：品牌色直接上图偏"纯"，柔和一档的同源色 + 低透明面积。
// 趋势图（UsagePlot）、报告页折线和两处图例共用这一张表。
const AGENT_LINE_COLORS = {
  codex: { stroke: "#5586d4", fill: "rgba(85, 134, 212, 0.09)" },
  claude: { stroke: "#d98663", fill: "rgba(217, 134, 99, 0.09)" },
  zcode: { stroke: "#8b80d9", fill: "rgba(139, 128, 217, 0.09)" },
  opencode: { stroke: "#4aa392", fill: "rgba(74, 163, 146, 0.09)" },
  kimi: { stroke: "#c4719f", fill: "rgba(196, 113, 159, 0.09)" },
  antigravity: { stroke: "#d1a34e", fill: "rgba(209, 163, 78, 0.09)" },
  workbuddy: { stroke: "#5fa671", fill: "rgba(95, 166, 113, 0.09)" },
  grok: { stroke: "#8a919a", fill: "rgba(138, 145, 154, 0.09)" },
  qoder: { stroke: "#4a7fa5", fill: "rgba(74, 127, 165, 0.09)" },
  deepseek: { stroke: "#7a8df8", fill: "rgba(122, 141, 248, 0.09)" },
  dsh: { stroke: "#5c669c", fill: "rgba(92, 102, 156, 0.09)" },
  qwen: { stroke: "#8f76e0", fill: "rgba(143, 118, 224, 0.09)" },
  pi: { stroke: "#9aa0a6", fill: "rgba(154, 160, 166, 0.09)" },
  hermes: { stroke: "#7d8085", fill: "rgba(125, 128, 133, 0.09)" },
  cursor: { stroke: "#a8977c", fill: "rgba(168, 151, 124, 0.09)" },
  minimax: { stroke: "#4fb0de", fill: "rgba(79, 176, 222, 0.09)" },
  default: { stroke: "#5586d4", fill: "rgba(85, 134, 212, 0.09)" },
};

export function agentPalette(agent) {
  return AGENT_LINE_COLORS[agent] || AGENT_LINE_COLORS.default;
}
