import antigravityAppIcon from "./assets/antigravity-app-icon.png";
import chatgptAppIcon from "./assets/chatgpt-app-icon.png";
import claudeAppIcon from "./assets/claude-app-icon.jpg";
import cursorAppIcon from "./assets/cursor-app-icon.png";
import deepseekAppIcon from "./assets/deepseek-app-icon.png";
import dshAppIcon from "./assets/dsh-app-icon.png";
import hermesAppIcon from "./assets/hermes-app-icon.png";
import kimiAppIcon from "./assets/kimi-app-icon.png";
import minimaxAppIcon from "./assets/minimax-app-icon.png";
import opencodeAppIcon from "./assets/opencode-app-icon.png";
import qoderAppIcon from "./assets/qoder-app-icon.png";
import grokAppIcon from "./assets/grok-app-icon.png";
import grokbotAppIcon from "./assets/grokbot-app-icon.png";
import piAppIcon from "./assets/pi-app-icon.png";
import qwenAppIcon from "./assets/qwen-app-icon.png";
import workbuddyAppIcon from "./assets/workbuddy-app-icon.png";
import zcodeAppIcon from "./assets/zcode-app-icon.png";
import { agentPalette } from "./agentColors.js";

export const AGENT_META = {
  codex: {
    label: "ChatGPT",
    accent: "#246bdb",
    iconSrc: chatgptAppIcon,
    iconClass: "agent-icon--codex",
  },
  claude: {
    // 额度是 Claude 全产品合并的，展示名不限定 Code；数据源仍是 Claude Code 日志。
    label: "Claude",
    accent: "#e36b49",
    iconSrc: claudeAppIcon,
    iconClass: "agent-icon--claude",
  },
  zcode: {
    label: "GLM",
    accent: "#6a5ae0",
    iconSrc: zcodeAppIcon,
    iconClass: "agent-icon--zcode",
  },
  opencode: {
    label: "OpenCode",
    accent: "#1f9d8b",
    iconSrc: opencodeAppIcon,
    iconClass: "agent-icon--opencode",
  },
  kimi: {
    label: "Kimi",
    // 品红：与 codex 蓝彻底拉开（原来 #3f74f2 和 ChatGPT 的蓝在图里分不清）。
    accent: "#c6538c",
    iconSrc: kimiAppIcon,
    iconClass: "agent-icon--kimi",
  },
  antigravity: {
    label: "Antigravity",
    // 琥珀金：六个 Agent 各占一个色相，不与 Claude 的珊瑚橙混淆。
    accent: "#cf9526",
    iconSrc: antigravityAppIcon,
    iconClass: "agent-icon--antigravity",
  },
  workbuddy: {
    // 覆盖腾讯 CodeBuddy Code 与 WorkBuddy 两个同格式来源，展示名从用户口径。
    label: "WorkBuddy",
    // 与品牌同色系的绿；也正好避开 GLM 的 #6a5ae0。
    accent: "#3d9c50",
    iconSrc: workbuddyAppIcon,
    iconClass: "agent-icon--workbuddy",
  },
  qoder: {
    // 配额-only：Qoder/QoderWork/Qoder CLI 共用官网账户级 Credits。
    label: "Qoder",
    // 深青蓝：与 codex 的宝蓝、opencode 的青绿保持距离。
    accent: "#3a7ca5",
    iconSrc: qoderAppIcon,
    iconClass: "agent-icon--qoder",
  },
  deepseek: {
    // 配额-only：DeepSeek 官方 API 账户余额，balance_ 窗口装的是金额不是百分比。
    label: "DeepSeek",
    // 品牌蓝 #4d6bfe：比 codex 的 #246bdb 亮一度且偏紫，不会与 zcode 的
    // #6a5ae0（明显更紫）混淆；卡片相邻时明度差足以区分。
    accent: "#4d6bfe",
    iconSrc: deepseekAppIcon,
    iconClass: "agent-icon--deepseek",
  },
  dsh: {
    // DeepSeek Harness：只有本地用量，走 coding plan 的部分记在对应卡片。
    label: "DeepSeek Harness",
    // 墨蓝：贴合 DSH 图标的深色鲸鱼，比 deepseek 的 #4d6bfe 暗得多、
    // 比 qoder 的 #3a7ca5 偏紫，相邻时不混。
    accent: "#3f4a85",
    iconSrc: dshAppIcon,
    iconClass: "agent-icon--dsh",
  },
  grok: {
    // xAI Grok Build：本地单轮 usage + CLI 日志里的周 Credits 快照。
    label: "Grok",
    // 中性灰蓝：xAI 品牌本身是黑白，中性色既贴合品牌又与八家彩色拉开。
    accent: "#6e7681",
    iconSrc: grokAppIcon,
    iconClass: "agent-icon--grok",
  },
  grokbot: {
    // 配额-only：Grok Bot 桌面应用的账号级统一额度池（SuperGrok / Grok Bot /
    // 新版 Cursor 共用），需在设置的数据来源页开启。
    label: "Grok Bot",
    // 深一档的石墨：与 grok 同属 xAI 黑白系，明度差足以区分相邻两卡。
    accent: "#43484f",
    iconSrc: grokbotAppIcon,
    iconClass: "agent-icon--grokbot",
  },
  pi: {
    // pi（badlogic/pi-mono）是 harness：本地会话用量按 provider 归属到
    // 对应计量卡片（GLM / Qwen / 其余留 Pi）；pi 自身没有独立套餐，不显示配额。
    label: "Pi",
    // 官方 logo 是黑白单色几何 π（pi.dev/logo-auto.svg），非红色；
    // 强调色取中性银灰，暗/亮主题都可见。
    accent: "#9aa0a6",
    iconSrc: piAppIcon,
    iconClass: "agent-icon--pi",
  },
  qwen: {
    // 百炼个人 Token Plan 是账户级套餐，由 pi 等客户端的 qwen-token-plan key
    // 消耗；额度没有可编程的官方接口，这张卡只记本地归属用量。
    label: "Qwen",
    // 千问 App 官方图标；紫与 GLM 的 #6a5ae0 同系不同值，亮度更高。
    accent: "#7b5cd6",
    iconSrc: qwenAppIcon,
    iconClass: "agent-icon--qwen",
  },
  hermes: {
    // Hermes（Nous Research）是 harness：这张卡统计 Hermes 的全部本地消耗，
    // 不按上游套餐拆走；它自身没有套餐，官方套餐余量由对应来源独立展示。
    label: "Hermes",
    // 官方应用图标是黑白 nous-girl 圆角瓦片（戴耳机的少女，四角透明）；
    // 强调色取与 pi 银灰相邻但更深的暖灰，两张中性卡在图里不互相混淆。
    accent: "#8a8d92",
    iconSrc: hermesAppIcon,
    iconClass: "agent-icon--hermes",
  },
  cursor: {
    // 用量取 cursor.com 仪表盘的账号级逐次事件，配额取同一仪表盘的套餐余量
    // （账单周期窗口）；两者都需在设置的数据来源页开启。
    label: "Cursor",
    // 官方图标是米白瓦片上的黑色立方体；强调色取暖灰褐，与 pi / hermes 的冷灰区分。
    accent: "#a08c6e",
    iconSrc: cursorAppIcon,
    iconClass: "agent-icon--cursor",
  },
  minimax: {
    // MiniMax Code（mcode）的本地逐请求用量；Token Plan 额度暂无可编程的官方接口。
    label: "MiniMax",
    // 官方图标是蓝色渐变团块；强调色取偏青的天蓝，与 codex 宝蓝、deepseek 蓝紫拉开。
    accent: "#1f9bd7",
    iconSrc: minimaxAppIcon,
    iconClass: "agent-icon--minimax",
  },
};

export const AGENT_ORDER = Object.keys(AGENT_META);

export function visibleAgentId(agentId) {
  return agentId === "kimiwork" ? "kimi" : agentId;
}

export function normalizeVisibleAgentList(agentIds) {
  return [...new Set(agentIds.map(visibleAgentId))].filter((id) =>
    AGENT_ORDER.includes(id),
  );
}

/// 用户在设置里排出的顺序优先，其余按注册表顺序垫后。设置面板、小组件、
/// 完整视图侧栏共用这一份顺序，改一处四处同步。
export function agentIdsInDisplayOrder(preferred) {
  return [...preferred, ...AGENT_ORDER.filter((agentId) => !preferred.includes(agentId))];
}

export const AGENT_LABELS = Object.fromEntries(
  AGENT_ORDER.map((id) => [id, AGENT_META[id].label]),
);

export function AgentMark({ agentId }) {
  const meta = AGENT_META[agentId];
  return (
    <span className={`agent-icon ${meta.iconClass}`} aria-hidden="true">
      <img src={meta.iconSrc} alt="" draggable="false" />
    </span>
  );
}

// 配色表见 agentColors.js。
export function chartColor(id) {
  return agentPalette(id).stroke;
}
