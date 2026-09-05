import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

const desktop = () => Boolean(window.__TAURI_INTERNALS__);

export function QuotaAlertsCard({ onSnapshotRefresh }) {
  const [enabled, setEnabled] = useState(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    if (!desktop()) return;
    let cancelled = false;
    invoke("quota_alert_settings").then((value) => {
      if (!cancelled) setEnabled(value);
    }).catch(() => {
      if (!cancelled) setError("提醒设置读取失败，请重新打开设置页。");
    });
    return () => { cancelled = true; };
  }, []);

  const toggle = async () => {
    setBusy(true);
    setError("");
    try {
      setEnabled(await invoke("set_quota_alerts", { enabled: !enabled }));
      onSnapshotRefresh();
    } catch {
      setError("提醒设置保存失败，请重试。");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="settings-card">
      <h2>额度提醒</h2>
      <p className="settings-muted">额度刷新后，任一有效窗口剩余不超过 15% 时发送系统通知。持续低额度只提醒一次；恢复后再次降低，两次提醒至少间隔 6 小时。</p>
      <label className="settings-check">
        <input type="checkbox" checked={enabled === true} disabled={!desktop() || enabled === null || busy} onChange={toggle} />
        开启低额度提醒
      </label>
      <p className="settings-muted">随现有额度刷新检查。通知显示受系统通知设置影响。</p>
      {!desktop() && <p className="settings-muted">浏览器演示模式：仅桌面应用可配置。</p>}
      {error && <p className="settings-feedback settings-feedback--error" role="alert">{error}</p>}
    </div>
  );
}

export function CodexCreditsCard() {
  const [credits, setCredits] = useState(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const query = async () => {
    setBusy(true);
    setError("");
    setCredits(null);
    try {
      setCredits(await invoke("codex_reset_credits"));
    } catch {
      setError("查询失败，请确认 Codex 已登录后重试。");
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="settings-card">
      <h2>Codex 重置券</h2>
      <p className="settings-muted">查询当前 Codex 账号的可用重置券及已知到期时间。</p>
      <button type="button" className="ledger-button ledger-button--secondary" disabled={!desktop() || busy} onClick={query}>
        {busy ? "查询中…" : "查询重置券"}
      </button>
      {credits && <dl className="settings-status" aria-live="polite">
        <div><dt>可用数量</dt><dd>{credits.availableCount == null ? "未提供" : `${credits.availableCount} 张`}</dd></div>
        {credits.availableCount > 0 && <div><dt>已知最早到期</dt><dd>{credits.nextKnownExpiryMs == null ? "未提供" : new Date(credits.nextKnownExpiryMs).toLocaleString("zh-CN", { hour12: false })}</dd></div>}
      </dl>}
      {!desktop() && <p className="settings-muted">浏览器演示模式：仅桌面应用可查询。</p>}
      {error && <p className="settings-feedback settings-feedback--error" role="alert">{error}</p>}
    </div>
  );
}
