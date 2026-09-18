/// 一个 Agent 可以同时受几个额度窗口约束（5 小时 / 每周 / 月度）。卡片行和
/// 胶囊格只有一个数字的位置，这个文件决定该显示哪一个。
///
/// 规则：平时显示周期最短的窗口；有窗口告急时，改显示告急窗口里剩余最少的。
///
/// 这个数字要回答"现在还能不能用、还能用多少"。日常受限的是短窗口——它掉得
/// 最快，所以正常显示 5h（Codex 取消了 5 小时限额，没有这个窗口，就落到每周）。
/// 但只要任何一个窗口见底，这个 Agent 就用不了了，别的窗口还剩多少都不算数：
/// 5h 满着而每周归零时，用不了就是用不了，那时必须把每周顶到台面上。
///
/// 不直接取"剩余最少"，是因为两个窗口都还很满时谁高谁低是噪声：实测 GLM 的
/// 5h 99% / 每周 97% 会让标签停在每周，用两下又跳回 5h，读者只会觉得它在乱跳。
///
/// 已过重置点的窗口跳过。`reset_expired` 只在"记录里有重置时刻、且该时刻已经
/// 过去"时为真，也就是这条读数所属的周期已经结束、额度应当已经回满，那个数字
/// 描述的是上一个周期。拿它当现状会凭空报一个早就不成立的余量。

/// "告急"的界线，与 App.jsx 里 quotaSeverity 的 warn 档同一个数：已用 85%。
export const QUOTA_LOW_REMAINING = 15;

/// 余额型窗口（key 形如 balance_cny / balance_usd）：view.remainingPercent 里
/// 装的是金额数值而不是百分比——可能大于 100，且越小越穷但语义不是"告急"。
export function isBalanceWindow(window) {
  return Boolean(window?.key?.startsWith("balance"));
}

/// 从窗口列表里挑出当前真正约束用量的那个；没有有效窗口时返回 null。
/// 传入顺序须为后端的 quota_window_rank 顺序（five_hour → seven_day → 月度 →
/// 超额付费）：平时取第一个，并列告急时也靠它保证短周期优先。
export function bindingWindow(windows) {
  const live = (windows || []).filter(
    (window) => window.view.available && !window.view.resetExpired,
  );
  if (!live.length) return null;
  // 余额窗口的 remainingPercent 是金额：余额 ¥10 不是"剩余 10%"，不能被告急
  // 过滤顶到行首、压过真正见底的周期窗口；它仍可作为 live[0] 正常返回。
  const low = live.filter(
    (window) => !isBalanceWindow(window) && window.view.remainingPercent <= QUOTA_LOW_REMAINING,
  );
  if (!low.length) return live[0];
  return low.reduce((tightest, window) =>
    window.view.remainingPercent < tightest.view.remainingPercent ? window : tightest,
  );
}
