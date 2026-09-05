//! Optional notifications from fresh quota snapshots. State is local and shared by all windows.
use crate::{domain::AgentQuotaView, storage};
use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const ENABLED_KEY: &str = "quota_alerts_enabled";
const STATE_KEY: &str = "quota_alerts_state";
const LOW_REMAINING: f64 = 15.0;
const COOLDOWN_MS: i64 = 6 * 60 * 60 * 1000;

#[derive(Default, Deserialize, Serialize)]
struct AlertState {
    notified_low: bool,
    last_sent_ms: Option<i64>,
}

pub fn enabled(connection: &Connection) -> Result<bool> {
    Ok(storage::get_app_setting(connection, ENABLED_KEY)?.as_deref() == Some("1"))
}

pub fn check(
    connection: &Connection,
    quotas: &[AgentQuotaView],
    now: i64,
    mut send: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    if !enabled(connection)? {
        return Ok(());
    }
    let raw = storage::get_app_setting(connection, STATE_KEY)?.unwrap_or_default();
    let mut states: BTreeMap<String, AlertState> = serde_json::from_str(&raw).unwrap_or_default();
    for quota in quotas {
        let current: Vec<_> = quota
            .windows
            .iter()
            .filter(|window| {
                let view = &window.view;
                view.available
                    && !view.stale
                    && !view.reset_expired
                    && view.remaining_percent.is_finite()
                    && matches!(view.quality.as_str(), "official_live" | "official_snapshot")
            })
            .collect();
        if current.is_empty() {
            continue;
        }
        let scope = storage::get_app_setting(connection, &format!("quota_scope_{}", quota.agent))?
            .unwrap_or_else(|| "legacy".into());
        let state = states
            .entry(format!("{}:{scope}", quota.agent))
            .or_default();
        let low = current
            .iter()
            .filter(|window| window.view.remaining_percent <= LOW_REMAINING)
            .min_by(|a, b| {
                a.view
                    .remaining_percent
                    .total_cmp(&b.view.remaining_percent)
            });
        let Some(window) = low else {
            // A partial/stale set cannot prove that all constraints recovered.
            if current.len() == quota.windows.len() {
                state.notified_low = false;
            }
            continue;
        };
        if state.notified_low
            || state
                .last_sent_ms
                .is_some_and(|last| now.saturating_sub(last) < COOLDOWN_MS)
        {
            continue;
        }
        let name = match quota.agent.as_str() {
            "codex" => "Codex",
            "claude" => "Claude",
            "zcode" => "GLM",
            "kimi" => "Kimi",
            "qoder" => "Qoder",
            "workbuddy" => "WorkBuddy",
            "grok" => "Grok",
            "antigravity" => "Antigravity",
            other => other,
        };
        send(&format!(
            "{name} · {}：剩余 {:.1}%",
            window.label, window.view.remaining_percent
        ))?;
        state.notified_low = true;
        state.last_sent_ms = Some(now);
        storage::set_app_setting(connection, STATE_KEY, &serde_json::to_string(&states)?)?;
    }
    let encoded = serde_json::to_string(&states)?;
    if encoded != raw {
        storage::set_app_setting(connection, STATE_KEY, &encoded)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentQuotaWindow, QuotaView};

    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_setting (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        db
    }

    fn quota(remaining: f64) -> AgentQuotaView {
        AgentQuotaView {
            agent: "codex".into(),
            note: None,
            windows: vec![AgentQuotaWindow {
                key: "primary".into(),
                label: "Session".into(),
                view: QuotaView {
                    available: true,
                    remaining_percent: remaining,
                    resets_in_minutes: Some(60.0),
                    age_minutes: Some(0.0),
                    stale: false,
                    reset_expired: false,
                    source_label: "Codex app-server".into(),
                    quality: "official_live".into(),
                },
            }],
        }
    }

    #[test]
    fn switching_account_has_an_independent_notification_episode() {
        let db = database();
        storage::set_app_setting(&db, ENABLED_KEY, "1").unwrap();
        let mut sent = 0;
        for account in ["a", "a", "b", "b", "a"] {
            storage::set_app_setting(&db, "quota_scope_codex", account).unwrap();
            check(&db, &[quota(10.0)], 0, |_| {
                sent += 1;
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(sent, 2);
    }

    #[test]
    fn disabled_stale_expired_and_missing_windows_do_not_notify() {
        let db = database();
        check(&db, &[quota(0.0)], 0, |_| panic!("disabled")).unwrap();
        storage::set_app_setting(&db, ENABLED_KEY, "1").unwrap();
        for mode in 0..4 {
            let mut q = quota(0.0);
            match mode {
                0 => q.windows[0].view.stale = true,
                1 => q.windows[0].view.reset_expired = true,
                2 => q.windows[0].view.available = false,
                _ => q.windows[0].view.quality = "estimated".into(),
            }
            check(&db, &[q], 0, |_| panic!("unreliable quota")).unwrap();
        }
    }

    #[test]
    fn persisted_episode_and_cooldown_prevent_repeated_notifications() {
        let db = database();
        storage::set_app_setting(&db, ENABLED_KEY, "1").unwrap();
        let mut messages = Vec::new();
        for (time, remaining) in [
            (0, 15.0),
            (1, 0.0),
            (COOLDOWN_MS, 0.0),
            (COOLDOWN_MS + 1, 80.0),
            (COOLDOWN_MS + 2, 10.0),
            (COOLDOWN_MS + 3, 80.0),
            (COOLDOWN_MS + 4, 10.0),
        ] {
            check(&db, &[quota(remaining)], time, |body| {
                messages.push(body.to_owned());
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn failed_delivery_does_not_consume_notification() {
        let db = database();
        storage::set_app_setting(&db, ENABLED_KEY, "1").unwrap();
        assert!(check(&db, &[quota(5.0)], 0, |_| anyhow::bail!("delivery failed")).is_err());
        let mut count = 0;
        check(&db, &[quota(5.0)], 1, |_| {
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 1);
    }
}
