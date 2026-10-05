// English UI text. Each key is the exact Chinese source string passed to t().
// Values are plain strings with the same {placeholders} as the key, or a
// function of the params when plural or word order needs it.
export default {
  // Language settings (LanguageSettings.jsx)
  "语言": "Language",
  "界面语言": "Interface language",
  "跟随系统": "Follow system",
  "跟随系统时，系统语言为中文则显示中文，否则显示英文。":
    "When following the system, Metrik shows Chinese if the system language is Chinese and English otherwise.",
  "语言设置保存失败，请重试。": "Couldn't save the language setting. Try again.",

  // Quota alerts and Codex reset credits (QuotaSettings.jsx)
  "额度提醒": "Quota alerts",
  "额度刷新后，任一有效窗口剩余不超过 15% 时发送系统通知。持续低额度只提醒一次；恢复后再次降低，两次提醒至少间隔 6 小时。":
    "After a quota refresh, Metrik sends a system notification when any valid window has 15% or less remaining. A continuous low-quota period notifies once; if it recovers and drops again, alerts stay at least 6 hours apart.",
  "开启低额度提醒": "Enable low-quota alerts",
  "随现有额度刷新检查。通知显示受系统通知设置影响。":
    "Checked with the existing quota refreshes. Whether notifications appear depends on your system notification settings.",
  "浏览器演示模式：仅桌面应用可配置。": "Browser demo: available in the desktop app only.",
  "提醒设置读取失败，请重新打开设置页。": "Couldn't read the alert setting. Reopen Settings.",
  "提醒设置保存失败，请重试。": "Couldn't save the alert setting. Try again.",
  "Codex 重置券": "Codex reset credits",
  "查询当前 Codex 账号的可用重置券及已知到期时间。":
    "Check the reset credits available to the current Codex account and their known expiry.",
  "查询中…": "Checking…",
  "查询重置券": "Check reset credits",
  "可用数量": "Available",
  "未提供": "Not reported",
  "{count} 张": ({ count }) => (count === 1 ? "1 credit" : `${count} credits`),
  "已知最早到期": "Earliest known expiry",
  "浏览器演示模式：仅桌面应用可查询。": "Browser demo: available in the desktop app only.",
  "查询失败，请确认 Codex 已登录后重试。": "Check failed. Make sure Codex is signed in, then try again.",
};
