//! `metrik --quota-json`：官方额度的稳定 JSON 出口。
//!
//! 给外部工具（脚本、状态栏、自动化闸门）一个不依赖 UI 的读取面。边界刻意收窄：
//! - 只读已落库的最新官方窗口，用 `SQLITE_OPEN_READ_ONLY` 打开、跳过 schema 检查，
//!   与报告/会话查询同一待遇。**不扫描日志、不发网络请求、不碰扫描锁**——桌面
//!   应用是唯一写方，CLI 只是旁观者，报告它上次刷新留下的状态。
//! - 只暴露派生额度元数据，绝不暴露凭据或原始 provider 响应。
//! - 契约带 `schemaVersion`，字段增删必须升版本，消费者的解析才不会静默漂移。
//! - 余额窗口（`balance_*`）的 `remainingPercent` 是金额不是百分比，以
//!   `kind: "balance"` 显式区分，消费者不得把它当比例渲染。

use crate::domain::{agent_label, AGENT_IDS};
use anyhow::{Context, Result};
use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;

pub const SCHEMA_VERSION: u8 = 1;

/// tauri.conf.json 的 identifier；CLI 在 Tauri 运行时之外解析同一路径。
const APP_IDENTIFIER: &str = "app.metrik.desktop";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuotaJsonDocument {
    schema_version: u8,
    generated_at: String,
    agents: Vec<QuotaJsonAgent>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuotaJsonAgent {
    id: &'static str,
    label: &'static str,
    windows: Vec<QuotaJsonWindow>,
    /// 该 Agent 确实没有可用窗口时的原因（目前只有 Claude 直连失败会填）。
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuotaJsonWindow {
    key: String,
    label: String,
    /// "percent"：剩余比例；"balance"：账户余额金额。
    kind: &'static str,
    available: bool,
    remaining_percent: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    resets_in_minutes: Option<f64>,
    stale: bool,
    reset_expired: bool,
    quality: String,
}

/// 默认账本路径，与桌面应用的 `app_local_data_dir` 解析一致：
/// Windows `%LOCALAPPDATA%`、macOS `~/Library/Application Support`、
/// Linux `$XDG_DATA_HOME`，均拼接 identifier。
pub fn default_ledger_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join(APP_IDENTIFIER).join(crate::DATABASE_FILE_NAME))
}

fn window_kind(key: &str) -> &'static str {
    if key.starts_with("balance") {
        "balance"
    } else {
        "percent"
    }
}

/// 从一个已打开的只读连接组装文档。测试直接喂内存库。
fn quota_json_document(connection: &rusqlite::Connection) -> Result<QuotaJsonDocument> {
    let agents = AGENT_IDS
        .iter()
        .map(|id| {
            // 单个 Agent 读不出来不该吞掉：契约上宁缺毋滥，整体失败并保留 stderr 说明。
            let windows = crate::engine::load_visible_agent_quota_windows(connection, id)?;
            // 只在确实没有可用窗口时才带原因，与桌面快照同语义。
            let note = if *id == "claude" && !windows.iter().any(|w| w.view.available) {
                crate::claude_oauth::last_failure(connection)?.map(|failure| failure.message)
            } else {
                None
            };
            Ok(QuotaJsonAgent {
                id,
                label: agent_label(id),
                windows: windows
                    .into_iter()
                    .map(|window| {
                        let view = window.view;
                        let kind = window_kind(&window.key);
                        QuotaJsonWindow {
                            key: window.key,
                            label: window.label,
                            kind,
                            available: view.available,
                            remaining_percent: view.remaining_percent,
                            resets_in_minutes: view.resets_in_minutes,
                            stale: view.stale,
                            reset_expired: view.reset_expired,
                            quality: view.quality,
                        }
                    })
                    .collect(),
                note,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(QuotaJsonDocument {
        schema_version: SCHEMA_VERSION,
        generated_at: chrono::Utc::now().to_rfc3339(),
        agents,
    })
}

pub fn run_quota_json(database_path: Option<&std::path::Path>) -> Result<()> {
    let path = match database_path {
        Some(path) => path.to_path_buf(),
        None => default_ledger_path().context(
            "cannot locate the default Metrik ledger; pass the database path as the second argument",
        )?,
    };
    let connection = crate::storage::open_database_read_only(&path)
        .with_context(|| format!("cannot open the Metrik ledger at {}", path.display()))?;
    let document = quota_json_document(&connection)?;
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer_pretty(&mut lock, &document)
        .context("failed to serialize the quota document")?;
    lock.write_all(b"\n")
        .context("failed to write the quota document")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::QuotaSample;

    fn memory_ledger() -> rusqlite::Connection {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        connection
    }

    fn sample(
        adapter: &'static str,
        key: &str,
        remaining: f64,
        collected_at_ms: i64,
    ) -> QuotaSample {
        QuotaSample {
            adapter_id: adapter,
            window_key: key.to_owned(),
            remaining_percent: remaining,
            resets_at_ms: Some(collected_at_ms + 60 * 60_000),
            collected_at_ms,
            source_label: "test".to_owned(),
            quality: "official_live",
        }
    }

    fn agent_windows<'a>(document: &'a QuotaJsonDocument, id: &str) -> &'a [QuotaJsonWindow] {
        &document
            .agents
            .iter()
            .find(|agent| agent.id == id)
            .unwrap()
            .windows
    }

    #[test]
    fn document_follows_the_public_agent_order_and_keeps_empty_agents() {
        let connection = memory_ledger();
        let document = quota_json_document(&connection).unwrap();
        let ids: Vec<_> = document.agents.iter().map(|agent| agent.id).collect();
        assert_eq!(ids, AGENT_IDS.to_vec());
        assert!(
            document.agents.iter().all(|agent| agent.windows.is_empty()),
            "空账本不得臆造窗口"
        );
        assert_eq!(document.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn windows_carry_kind_staleness_and_reset_semantics() {
        let now = chrono::Utc::now().timestamp_millis();
        let connection = memory_ledger();
        crate::storage::upsert_quota(&connection, &sample("codex", "primary", 42.0, now)).unwrap();
        // 2 小时前的 official_live 读数：超过 7 分钟新鲜线，必须标 stale。
        crate::storage::upsert_quota(
            &connection,
            &sample("kimi", "five_hour", 80.0, now - 2 * 60 * 60_000),
        )
        .unwrap();
        // 余额窗口是金额，不是比例。
        crate::storage::upsert_quota(&connection, &sample("deepseek", "balance_cny", 120.0, now))
            .unwrap();

        let document = quota_json_document(&connection).unwrap();
        let codex = agent_windows(&document, "codex");
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].kind, "percent");
        assert!(codex[0].available);
        assert!(!codex[0].stale);
        assert!(codex[0].resets_in_minutes.unwrap() > 59.0);

        let kimi = agent_windows(&document, "kimi");
        assert!(kimi[0].stale);
        assert!(kimi[0].reset_expired);

        let balance = agent_windows(&document, "deepseek");
        assert_eq!(balance[0].kind, "balance");
        assert_eq!(balance[0].remaining_percent, 120.0);
    }

    #[test]
    fn kimi_and_kimiwork_merge_into_one_visible_window() {
        let now = chrono::Utc::now().timestamp_millis();
        let connection = memory_ledger();
        crate::storage::upsert_quota(&connection, &sample("kimi", "five_hour", 40.0, now)).unwrap();
        // 同键更新鲜的一份 kimiwork 读数应当胜出，且不产生第二个 five_hour 窗口。
        crate::storage::upsert_quota(&connection, &sample("kimiwork", "five_hour", 80.0, now))
            .unwrap();

        let document = quota_json_document(&connection).unwrap();
        let kimi = agent_windows(&document, "kimi");
        let five_hour: Vec<_> = kimi
            .iter()
            .filter(|window| window.key == "five_hour")
            .collect();
        assert_eq!(five_hour.len(), 1, "kimiwork 的同键窗口必须合并");
        assert_eq!(five_hour[0].remaining_percent, 80.0);
    }

    #[test]
    fn claude_note_explains_absence_only_while_nothing_is_available() {
        let connection = memory_ledger();
        crate::claude_oauth::record_failure(&connection, "Claude 用量接口限流（429）").unwrap();

        let document = quota_json_document(&connection).unwrap();
        let claude = document
            .agents
            .iter()
            .find(|agent| agent.id == "claude")
            .unwrap();
        assert_eq!(claude.note.as_deref(), Some("Claude 用量接口限流（429）"));

        let now = chrono::Utc::now().timestamp_millis();
        crate::storage::upsert_quota(&connection, &sample("claude", "five_hour", 50.0, now))
            .unwrap();
        let document = quota_json_document(&connection).unwrap();
        let claude = document
            .agents
            .iter()
            .find(|agent| agent.id == "claude")
            .unwrap();
        assert!(claude.note.is_none(), "有可用窗口时不再解释缺席");
    }

    #[test]
    fn document_serializes_with_the_stable_camel_case_contract() {
        let now = chrono::Utc::now().timestamp_millis();
        let connection = memory_ledger();
        crate::storage::upsert_quota(&connection, &sample("codex", "primary", 42.0, now)).unwrap();
        let document = quota_json_document(&connection).unwrap();
        let value = serde_json::to_value(&document).unwrap();
        let agent = &value["agents"][0];
        assert!(agent["id"].is_string());
        assert!(agent["label"].is_string());
        let window = &agent["windows"][0];
        for key in [
            "key",
            "label",
            "kind",
            "available",
            "remainingPercent",
            "resetsInMinutes",
            "stale",
            "resetExpired",
            "quality",
        ] {
            assert!(window.get(key).is_some(), "契约缺少字段 {key}");
        }
        assert_eq!(value["schemaVersion"], SCHEMA_VERSION);
        assert!(value["generatedAt"].is_string());
    }

    #[test]
    fn default_ledger_path_matches_the_desktop_ledger_file() {
        let path = default_ledger_path().expect("test machines have a data dir");
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            crate::DATABASE_FILE_NAME
        );
        assert!(path.to_string_lossy().contains(APP_IDENTIFIER));
    }
}
