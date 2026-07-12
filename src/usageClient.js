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
    tokens: { codex: 0, claude: 0, opencode: 0 },
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
        claude: Math.round(total * 0.285),
        opencode: Math.round(total * 0.06),
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
        opencode: Math.round(76_400 * config.factor * (0.9 + (index % 3) / 10) * weekend / config.points),
      },
    };
  });
}

function demoSnapshot(period = "today") {
  const scale = PERIOD_SCALE[period]?.factor || 1;
  const codexTokens = Math.round(892_400 * scale);
  const claudeTokens = Math.round(392_160 * scale);
  const opencodeTokens = Math.round(76_400 * scale);
  const totalTokens = codexTokens + claudeTokens + opencodeTokens;
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
    quota: {
      available: true,
      remainingPercent: 72,
      resetsInMinutes: 198,
      ageMinutes: 0,
      stale: false,
      resetExpired: false,
      sourceLabel: "演示配额",
      quality: "demo",
    },
    secondaryQuota: {
      available: true,
      remainingPercent: 86,
      resetsInMinutes: 8_940,
      ageMinutes: 0,
      stale: false,
      resetExpired: false,
      sourceLabel: "演示配额",
      quality: "demo",
    },
    agents: [
      { id: "codex", tokens: codexTokens, share: (codexTokens / totalTokens) * 100 },
      { id: "claude", tokens: claudeTokens, share: (claudeTokens / totalTokens) * 100 },
      { id: "opencode", tokens: opencodeTokens, share: (opencodeTokens / totalTokens) * 100 },
    ],
    sources: [
      { id: "codex-quota", kind: "official", label: "ChatGPT / Codex 官方配额", detail: "通过本机 ChatGPT / Codex 服务读取滚动窗口；不接触登录凭据。", quality: "official", qualityLabel: "官方" },
      { id: "codex-local", kind: "local", label: "ChatGPT / Codex 本地 Token", detail: "由 Codex Agent 会话日志中的累计快照计算正增量，并排除重复记录。", quality: "exact", qualityLabel: "精确解析" },
      { id: "claude-local", kind: "local", label: "Claude Code 本地 Token", detail: "读取消息 usage 字段并以消息标识去重；配额无可靠来源时不推算。", quality: "exact", qualityLabel: "精确解析" },
      { id: "opencode-local", kind: "local", label: "OpenCode 本地 Token", detail: "读取消息 usage 字段并以消息标识去重；未安装 OpenCode 时保持为 0。", quality: "exact", qualityLabel: "精确解析" },
    ],
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
    quota: {
      available: false,
      remainingPercent: 0,
      resetsInMinutes: null,
      ageMinutes: null,
      stale: false,
      resetExpired: false,
      sourceLabel: "正在读取",
      quality: "unavailable",
    },
    secondaryQuota: {
      available: false,
      remainingPercent: 0,
      resetsInMinutes: null,
      ageMinutes: null,
      stale: false,
      resetExpired: false,
      sourceLabel: "正在读取",
      quality: "unavailable",
    },
    agents: [
      { id: "codex", tokens: 0, share: 0 },
      { id: "claude", tokens: 0, share: 0 },
      { id: "opencode", tokens: 0, share: 0 },
    ],
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
    quota: {
      available: false,
      remainingPercent: 0,
      resetsInMinutes: null,
      ageMinutes: null,
      stale: false,
      resetExpired: false,
      sourceLabel: "本地服务暂不可用",
      quality: "unavailable",
    },
    secondaryQuota: {
      available: false,
      remainingPercent: 0,
      resetsInMinutes: null,
      ageMinutes: null,
      stale: false,
      resetExpired: false,
      sourceLabel: "本地服务暂不可用",
      quality: "unavailable",
    },
    agents: [
      { id: "codex", tokens: 0, share: 0 },
      { id: "claude", tokens: 0, share: 0 },
      { id: "opencode", tokens: 0, share: 0 },
    ],
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
  };
}

async function loadUsageSnapshot(period = "today") {
  if (!isTauriRuntime()) {
    await new Promise((resolve) => setTimeout(resolve, 180));
    return demoSnapshot(period);
  }

  try {
    return await invoke("usage_snapshot", { period });
  } catch (error) {
    console.warn("Unable to load live usage.", error);
    return unavailableSnapshot(period);
  }
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

loadUsageSnapshot.demo = demoSnapshot;
loadUsageSnapshot.initial = (period = "today") => (
  isTauriRuntime() ? pendingSnapshot(period) : demoSnapshot(period)
);

export { loadUsageSnapshot as getUsageSnapshot, rebuildLocalLedger, getSyncSettings, configureSync };
