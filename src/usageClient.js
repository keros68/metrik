import { invoke } from "@tauri-apps/api/core";

const PERIOD_SCALE = {
  today: { factor: 1, points: 24, label: (index) => `${String(index).padStart(2, "0")}:00` },
  week: {
    factor: 5.8,
    points: 7,
    label: (index) => ["周一", "周二", "周三", "周四", "周五", "周六", "今日"][index],
  },
  month: { factor: 23.4, points: 30, label: (index) => `${index + 1} 日` },
};

function isTauriRuntime() {
  return typeof window !== "undefined" && Boolean(window.__TAURI_INTERNALS__);
}

function emptySeries(period) {
  const config = PERIOD_SCALE[period] || PERIOD_SCALE.today;
  return Array.from({ length: config.points }, (_, index) => ({
    label: config.label(index),
    tokens: { codex: 0, claude: 0, zcode: 0, opencode: 0, kimi: 0, antigravity: 0, workbuddy: 0 },
  }));
}

function demoSeries(period) {
  const config = PERIOD_SCALE[period] || PERIOD_SCALE.today;
  const totalBase = [0, 0, 1200, 3400, 6200, 14000, 33000, 51000, 62000, 74000, 84000, 92000, 101000, 108000, 111000, 115000, 127000, 132000, 141000, 151000, 165000, 158000, 143000, 129000, 147000]
    .map((value) => Math.round(value * 0.78));
  if (period === "today") {
    return totalBase.map((total, index) => ({
      label: config.label(index),
      tokens: {
        codex: Math.round(total * 0.655),
        claude: Math.round(total * 0.245),
        zcode: Math.round(total * 0.04),
        opencode: Math.round(total * 0.06),
        kimi: Math.round(total * 0.03),
        antigravity: Math.round(total * 0.025),
        workbuddy: Math.round(total * 0.02),
      },
    }));
  }

  return Array.from({ length: config.points }, (_, index) => {
    const wave = 0.84 + ((index * 7) % 9) / 20;
    const weekend = period === "week" && (index === 5 || index === 6) ? 0.63 : 1;
    return {
      label: config.label(index),
      tokens: {
        codex: Math.round(892_400 * config.factor * wave * weekend / config.points),
        claude: Math.round(392_160 * config.factor * (1.14 - (index % 4) / 18) * weekend / config.points),
        zcode: Math.round(52_800 * config.factor * (0.8 + (index % 5) / 12) * weekend / config.points),
        opencode: Math.round(76_400 * config.factor * (0.9 + (index % 3) / 10) * weekend / config.points),
        kimi: Math.round(38_200 * config.factor * (0.85 + (index % 4) / 14) * weekend / config.points),
        antigravity: Math.round(31_500 * config.factor * (0.82 + (index % 5) / 15) * weekend / config.points),
        workbuddy: Math.round(24_800 * config.factor * (0.8 + (index % 3) / 13) * weekend / config.points),
      },
    };
  });
}

function demoQuotaView(remainingPercent, resetsInMinutes, overrides = {}) {
  return {
    available: true,
    remainingPercent,
    resetsInMinutes,
    ageMinutes: 0,
    stale: false,
    resetExpired: false,
    sourceLabel: "演示配额",
    quality: "demo",
    ...overrides,
  };
}

// 演示分量比例取自真实日志的典型形态：缓存读远大于其余分量。
function demoAgentSummary(id, tokens, totalTokens) {
  return {
    id,
    tokens,
    inputUncached: Math.round(tokens * 0.07),
    cacheRead: Math.round(tokens * 0.82),
    cacheWrite: Math.round(tokens * 0.06),
    output: tokens - Math.round(tokens * 0.07) - Math.round(tokens * 0.82) - Math.round(tokens * 0.06),
    share: (tokens / totalTokens) * 100,
  };
}

function demoSnapshot(period = "today") {
  const scale = PERIOD_SCALE[period]?.factor || 1;
  const codexTokens = Math.round(892_400 * scale);
  const claudeTokens = Math.round(392_160 * scale);
  const zcodeTokens = Math.round(52_800 * scale);
  const opencodeTokens = Math.round(76_400 * scale);
  const kimiTokens = Math.round(38_200 * scale);
  const antigravityTokens = Math.round(31_500 * scale);
  const workbuddyTokens = Math.round(24_800 * scale);
  const totalTokens =
    codexTokens + claudeTokens + zcodeTokens + opencodeTokens + kimiTokens + antigravityTokens + workbuddyTokens;
  return {
    generatedAt: new Date().toISOString(),
    period,
    isDemo: true,
    loadError: false,
    pending: false,
    totalTokens,
    comparisonPercent: period === "today" ? -12 : period === "week" ? -8 : -5,
    comparisonAvailable: true,
    series: demoSeries(period),
    agentQuotas: [
      {
        agent: "codex",
        windows: [
          { key: "five_hour", label: "Session", view: demoQuotaView(72, 198) },
          { key: "seven_day", label: "每周", view: demoQuotaView(86, 8_940) },
        ],
      },
      {
        agent: "claude",
        windows: [
          { key: "five_hour", label: "Session", view: demoQuotaView(94, 102) },
          // 故意做一扇陈旧窗口，让浏览器预览能走到 stale 标注路径。
          { key: "seven_day", label: "每周 · 全模型", view: demoQuotaView(67, 6_180, { ageMinutes: 23, stale: true }) },
        ],
      },
      {
        agent: "zcode",
        windows: [
          { key: "five_hour", label: "Session", view: demoQuotaView(58, 241) },
          { key: "seven_day", label: "每周", view: demoQuotaView(81, 5_760) },
        ],
      },
      {
        agent: "kimi",
        windows: [
          { key: "five_hour", label: "Session", view: demoQuotaView(90, 130) },
          { key: "seven_day", label: "每周", view: demoQuotaView(76, 4_320) },
        ],
      },
      // OpenCode 与 WorkBuddy 现实中没有官方配额来源：窗口列表保持为空。
      { agent: "opencode", windows: [] },
      {
        agent: "workbuddy",
        windows: [{ key: "credits", label: "Credits", view: demoQuotaView(78, 15_120) }],
      },
      {
        agent: "qoder",
        windows: [{ key: "credits", label: "Credits", view: demoQuotaView(64, 12_960) }],
      },
    ],
    agents: [
      demoAgentSummary("codex", codexTokens, totalTokens),
      demoAgentSummary("claude", claudeTokens, totalTokens),
      demoAgentSummary("zcode", zcodeTokens, totalTokens),
      demoAgentSummary("opencode", opencodeTokens, totalTokens),
      demoAgentSummary("kimi", kimiTokens, totalTokens),
      demoAgentSummary("antigravity", antigravityTokens, totalTokens),
      demoAgentSummary("workbuddy", workbuddyTokens, totalTokens),
    ],
    cost: {
      available: true,
      // 演示值按 gpt-5.2 / claude 价目的量级粗算。
      totalUsd: 5.62 * scale,
      // ZCode / OpenCode / Kimi / Antigravity / WorkBuddy 未计价：演示数据也如实反映这一点。
      unpricedTokens: zcodeTokens + opencodeTokens + kimiTokens + antigravityTokens + workbuddyTokens,
      pricingAsOf: "2026-07-13",
      byAgent: [
        { agent: "codex", usd: 2.31 * scale, unpricedTokens: 0 },
        { agent: "claude", usd: 3.31 * scale, unpricedTokens: 0 },
        { agent: "zcode", usd: 0, unpricedTokens: zcodeTokens },
        { agent: "opencode", usd: 0, unpricedTokens: opencodeTokens },
        { agent: "kimi", usd: 0, unpricedTokens: kimiTokens },
        { agent: "antigravity", usd: 0, unpricedTokens: antigravityTokens },
        { agent: "workbuddy", usd: 0, unpricedTokens: workbuddyTokens },
      ],
    },
    models: [
      { model: "gpt-5.2-codex", agent: "codex", tokens: Math.round(codexTokens * 0.9), share: (codexTokens * 0.9 / totalTokens) * 100 },
      { model: "claude-fable-5", agent: "claude", tokens: Math.round(claudeTokens * 0.72), share: (claudeTokens * 0.72 / totalTokens) * 100 },
      { model: "claude-sonnet-5", agent: "claude", tokens: Math.round(claudeTokens * 0.28), share: (claudeTokens * 0.28 / totalTokens) * 100 },
      { model: "gpt-5.2", agent: "codex", tokens: Math.round(codexTokens * 0.1), share: (codexTokens * 0.1 / totalTokens) * 100 },
      { model: "glm-5", agent: "zcode", tokens: zcodeTokens, share: (zcodeTokens / totalTokens) * 100 },
      { model: "unknown", agent: "opencode", tokens: opencodeTokens, share: (opencodeTokens / totalTokens) * 100 },
      { model: "kimi-for-coding", agent: "kimi", tokens: kimiTokens, share: (kimiTokens / totalTokens) * 100 },
      { model: "gemini-3.1-pro", agent: "antigravity", tokens: antigravityTokens, share: (antigravityTokens / totalTokens) * 100 },
      { model: "glm-5.2", agent: "workbuddy", tokens: workbuddyTokens, share: (workbuddyTokens / totalTokens) * 100 },
    ],
    sources: [
      { id: "codex-quota", kind: "official", label: "ChatGPT / Codex 官方配额", detail: "通过本机 ChatGPT / Codex 服务读取滚动窗口；不接触登录凭据。", quality: "official", qualityLabel: "官方" },
      { id: "codex-local", kind: "local", label: "ChatGPT / Codex 本地 Token", detail: "由 Codex Agent 会话日志中的累计快照计算正增量，并排除重复记录。", quality: "exact", qualityLabel: "精确解析" },
      { id: "claude-local", kind: "local", label: "Claude Code 本地 Token", detail: "读取消息 usage 字段并以消息标识去重；配额无可靠来源时不推算。", quality: "exact", qualityLabel: "精确解析" },
      { id: "zcode-local", kind: "local", label: "GLM 本地 Token", detail: "读取 model_usage 统计表的逐请求计数；不读取消息内容。", quality: "exact", qualityLabel: "精确解析" },
      { id: "opencode-local", kind: "local", label: "OpenCode 本地 Token", detail: "读取消息 usage 字段并以消息标识去重；未安装 OpenCode 时保持为 0。", quality: "exact", qualityLabel: "精确解析" },
      { id: "kimi-local", kind: "local", label: "Kimi 本地 Token", detail: "只计单轮增量记录（会话累计记录会重复计数）；未安装 Kimi 时保持为 0。", quality: "exact", qualityLabel: "精确解析" },
      { id: "antigravity-live", kind: "local", label: "Antigravity 用量", detail: "来自本机 language server 实时 RPC；IDE 未运行时为 0，不估算。尚未实机验收。", quality: "exact", qualityLabel: "精确解析" },
      { id: "workbuddy-local", kind: "local", label: "WorkBuddy 本地 Token", detail: "读取 CodeBuddy/WorkBuddy 会话转录的 usage 字段并以消息标识去重；未安装时保持为 0。", quality: "exact", qualityLabel: "精确解析" },
      { id: "qoder-quota", kind: "official", label: "Qoder 官方 Credits", detail: "设置 QODER_COOKIE 环境变量后读取官网额度；本地不落 token 用量，无本地统计。", quality: "official", qualityLabel: "官方" },
    ],
    indexing: { pending: 0 },
  };
}

function pendingSnapshot(period = "today") {
  return {
    generatedAt: null,
    period,
    isDemo: false,
    loadError: false,
    pending: true,
    totalTokens: 0,
    comparisonPercent: 0,
    comparisonAvailable: false,
    series: emptySeries(period),
    agentQuotas: [],
    agents: [
      { id: "codex", tokens: 0, share: 0 },
      { id: "claude", tokens: 0, share: 0 },
      { id: "zcode", tokens: 0, share: 0 },
      { id: "opencode", tokens: 0, share: 0 },
    ],
    models: [],
    sources: [
      {
        id: "pending",
        kind: "local",
        label: "正在建立本地统计索引",
        detail: "首次升级或大型日志库可能需要几分钟；窗口操作不会被阻塞，也不会显示演示数字。",
        quality: "unavailable",
        qualityLabel: "读取中",
      },
    ],
    indexing: { pending: 0 },
  };
}

function unavailableSnapshot(period = "today") {
  return {
    generatedAt: new Date().toISOString(),
    period,
    isDemo: false,
    loadError: true,
    pending: false,
    totalTokens: 0,
    comparisonPercent: 0,
    comparisonAvailable: false,
    series: emptySeries(period),
    agentQuotas: [],
    agents: [
      { id: "codex", tokens: 0, share: 0 },
      { id: "claude", tokens: 0, share: 0 },
      { id: "zcode", tokens: 0, share: 0 },
      { id: "opencode", tokens: 0, share: 0 },
    ],
    models: [],
    sources: [
      {
        id: "load-error",
        kind: "local",
        label: "本地统计暂不可用",
        detail: "Metrik 没有用演示数字替代失败结果。请稍后重试；原始 Agent 日志不会因此被修改。",
        quality: "unavailable",
        qualityLabel: "未载入",
      },
    ],
    indexing: { pending: 0 },
  };
}

async function loadUsageSnapshot(period = "today", options = {}) {
  if (!isTauriRuntime()) {
    await new Promise((resolve) => setTimeout(resolve, 180));
    return demoSnapshot(period);
  }

  try {
    return await invoke("usage_snapshot", { period, force: !!options.force });
  } catch (error) {
    console.warn("Unable to load live usage.", error);
    return unavailableSnapshot(period);
  }
}

function demoReport() {
  const days = [];
  const now = new Date();
  const byAgentTotals = { codex: 0, claude: 0, zcode: 0, opencode: 0 };
  const activeDays = { codex: 0, claude: 0, zcode: 0, opencode: 0 };
  let total = 0;
  for (let offset = 181; offset >= 0; offset -= 1) {
    const date = new Date(now.getFullYear(), now.getMonth(), now.getDate() - offset);
    const weekday = date.getDay();
    const wave = 0.55 + ((offset * 13) % 17) / 17;
    const weekend = weekday === 0 || weekday === 6 ? 0.3 : 1;
    // 演示形态：约 8% 的天完全没有用量。
    if ((offset * 7) % 12 === 0) continue;
    const codex = Math.round(9_800_000 * wave * weekend);
    const claude = Math.round(6_400_000 * (1.4 - wave / 2) * weekend);
    const zcode = (offset % 5 === 0) ? Math.round(900_000 * wave) : 0;
    const opencode = (offset % 9 === 0) ? Math.round(600_000 * wave) : 0;
    const key = `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
    const byAgent = { codex, claude, zcode, opencode };
    const dayTotal = codex + claude + zcode + opencode;
    Object.entries(byAgent).forEach(([id, value]) => {
      byAgentTotals[id] += value;
      if (value > 0) activeDays[id] += 1;
    });
    total += dayTotal;
    days.push({ date: key, tokens: dayTotal, byAgent });
  }
  return {
    isDemo: true,
    loadError: false,
    days,
    firstEventMs: now.getTime() - 181 * 86_400_000,
    lastEventMs: now.getTime(),
    totalTokens: total,
    topModels: [
      { model: "gpt-5.2-codex", agent: "codex", tokens: Math.round(byAgentTotals.codex * 0.9), share: (byAgentTotals.codex * 0.9 / total) * 100 },
      { model: "claude-fable-5", agent: "claude", tokens: Math.round(byAgentTotals.claude * 0.72), share: (byAgentTotals.claude * 0.72 / total) * 100 },
      { model: "claude-sonnet-5", agent: "claude", tokens: Math.round(byAgentTotals.claude * 0.28), share: (byAgentTotals.claude * 0.28 / total) * 100 },
      { model: "gpt-5.2", agent: "codex", tokens: Math.round(byAgentTotals.codex * 0.1), share: (byAgentTotals.codex * 0.1 / total) * 100 },
      { model: "glm-5", agent: "zcode", tokens: byAgentTotals.zcode, share: (byAgentTotals.zcode / total) * 100 },
    ],
    agents: ["codex", "claude", "zcode", "opencode"].map((id) => ({
      id,
      tokens: byAgentTotals[id],
      activeDays: activeDays[id],
    })),
    streakDays: 5,
  };
}

function demoSessions(period = "today") {
  const now = Date.now();
  const dayCount = period === "today" ? 1 : period === "week" ? 7 : 30;
  const sessions = [];
  const specs = [
    { agent: "codex", model: "gpt-5.2-codex", base: 12_400_000, usd: 0.42 },
    { agent: "claude", model: "claude-fable-5", base: 38_200_000, usd: 1.92 },
    { agent: "claude", model: "claude-sonnet-5", base: 6_800_000, usd: 0.21 },
    { agent: "codex", model: "gpt-5.2", base: 3_100_000, usd: 0.11 },
  ];
  for (let day = 0; day < Math.min(dayCount, 5); day += 1) {
    specs.forEach((spec, index) => {
      if ((day + index) % 3 === 2) return;
      const end = now - day * 86_400_000 - (index * 2 + 1) * 3_600_000;
      const tokens = Math.round(spec.base * (0.6 + ((day + index) % 4) / 5));
      sessions.push({
        agent: spec.agent,
        sessionId: `demo-${day}-${index}-${spec.agent}`,
        startMs: end - (28 + index * 17) * 60_000,
        endMs: end,
        tokens,
        inputUncached: Math.round(tokens * 0.07),
        cacheRead: Math.round(tokens * 0.82),
        cacheWrite: Math.round(tokens * 0.06),
        output: Math.round(tokens * 0.05),
        model: spec.model,
        models: [spec.model],
        usd: spec.usd * (0.6 + ((day + index) % 4) / 5),
        eventCount: 40 + index * 13,
      });
    });
  }
  sessions.sort((a, b) => b.endMs - a.endMs);
  return { period, sessions, totalSessions: sessions.length, truncated: false, isDemo: true, loadError: false };
}

async function getUsageSessions(period = "today") {
  if (!isTauriRuntime()) {
    await new Promise((resolve) => setTimeout(resolve, 200));
    return demoSessions(period);
  }
  try {
    return await invoke("usage_sessions", { period });
  } catch (error) {
    console.warn("Unable to load usage sessions.", error);
    return { period, sessions: [], totalSessions: 0, truncated: false, isDemo: false, loadError: true };
  }
}

async function getUsageReport() {
  if (!isTauriRuntime()) {
    await new Promise((resolve) => setTimeout(resolve, 220));
    return demoReport();
  }
  try {
    return await invoke("usage_report");
  } catch (error) {
    console.warn("Unable to load the usage report.", error);
    return { loadError: true, isDemo: false, days: [], topModels: [], agents: [], totalTokens: 0, streakDays: 0, firstEventMs: null, lastEventMs: null };
  }
}

// 桌面端 WebView 不响应 blob 下载，导出走后端写入下载目录；返回完整路径。
async function exportCsvFile(fileName, content) {
  if (!isTauriRuntime()) {
    return null;
  }
  return invoke("export_csv", { fileName, content });
}

async function rebuildLocalLedger(period = "today") {
  if (!isTauriRuntime()) {
    await new Promise((resolve) => setTimeout(resolve, 700));
    return demoSnapshot(period);
  }

  return invoke("rebuild_local_ledger", { period });
}

async function getSyncSettings() {
  if (!isTauriRuntime()) {
    await new Promise((resolve) => setTimeout(resolve, 120));
    return {
      demo: true,
      enabled: false,
      directory: null,
      deviceId: "demo-device",
      deviceLabel: "演示设备",
      lastExportMs: null,
      lastError: null,
      devices: [],
    };
  }
  return invoke("sync_settings");
}

async function configureSync(directory) {
  if (!isTauriRuntime()) {
    throw new Error("浏览器演示模式不能配置同步");
  }
  return invoke("configure_sync", { directory });
}

async function getClaudeHookStatus() {
  if (!isTauriRuntime()) {
    return { demo: true, installed: false, conflict: false, lastDataAtMs: null };
  }
  return invoke("claude_hook_status");
}

async function setClaudeHook(enabled) {
  if (!isTauriRuntime()) {
    throw new Error("浏览器演示模式不能配置钩子");
  }
  return invoke("set_claude_hook", { enabled });
}

async function getQoderCookieStatus() {
  if (!isTauriRuntime()) {
    return { demo: true, configured: false, source: null, message: null };
  }
  return invoke("qoder_cookie_status");
}

async function configureQoderCookie(cookie) {
  if (!isTauriRuntime()) {
    throw new Error("浏览器演示模式不能配置 cookie");
  }
  return invoke("configure_qoder_cookie", { cookie });
}

async function getClaudeOauthStatus() {
  if (!isTauriRuntime()) {
    return { demo: true, enabled: false, credentialsPresent: false, scopeOk: false };
  }
  return invoke("claude_oauth_status");
}

async function setClaudeOauth(enabled) {
  if (!isTauriRuntime()) {
    throw new Error("浏览器演示模式不能配置官方额度来源");
  }
  return invoke("set_claude_oauth", { enabled });
}

loadUsageSnapshot.demo = demoSnapshot;
loadUsageSnapshot.initial = (period = "today") => (
  isTauriRuntime() ? pendingSnapshot(period) : demoSnapshot(period)
);

export {
  loadUsageSnapshot as getUsageSnapshot,
  getUsageReport,
  getUsageSessions,
  exportCsvFile,
  rebuildLocalLedger,
  getSyncSettings,
  configureSync,
  getClaudeHookStatus,
  setClaudeHook,
  getClaudeOauthStatus,
  setClaudeOauth,
  getQoderCookieStatus,
  configureQoderCookie,
};
