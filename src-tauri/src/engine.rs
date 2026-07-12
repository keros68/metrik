use crate::adapters::{AgentAdapter, ClaudeAdapter, CodexAdapter, OpencodeAdapter, ScanDiagnostics};
use crate::app_server;
use crate::domain::{
    AgentSummary, QuotaSample, QuotaView, SeriesPoint, SourceView, SyncView, UsageSnapshot,
    AGENT_IDS,
};
use crate::storage;
use crate::sync;
use anyhow::{Context, Result};
use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone, Timelike, Utc, Weekday};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration as StdDuration, Instant};

#[derive(Default)]
struct ScanReport {
    discovered: HashMap<String, usize>,
    refreshed: HashMap<String, usize>,
    errors: HashMap<String, usize>,
    diagnostics: HashMap<String, AdapterDiagnostics>,
}

#[derive(Clone, Debug, Default)]
struct AdapterDiagnostics {
    partial_sources: usize,
    malformed_lines: usize,
    unreadable_lines: usize,
    rejected_events: usize,
}

#[derive(Clone)]
struct StoredEvent {
    adapter: String,
    timestamp: i64,
    tokens: i64,
}

pub fn build_snapshot(
    database_path: &Path,
    period: &str,
    quota_cache: &Mutex<Option<(Instant, Vec<QuotaSample>)>>,
) -> Result<UsageSnapshot> {
    let mut connection = storage::open_database(database_path)?;
    let now = Utc::now().timestamp_millis();
    let discovery_cutoff = discovery_cutoff_ms(period, now);
    let retention_cutoff = now - Duration::days(65).num_milliseconds();
    let report = ingest_sources(&mut connection, discovery_cutoff, retention_cutoff)?;

    if let Ok(samples) = cached_live_quota(quota_cache) {
        for sample in &samples {
            storage::upsert_quota(&connection, sample)?;
        }
    }

    storage::prune_missing_sources(&mut connection)?;
    storage::prune_old_events(&connection, retention_cutoff)?;
    sync::run_sync(&mut connection, now);
    query_snapshot(&connection, period, report)
}

fn discovery_cutoff_ms(period: &str, now_ms: i64) -> i64 {
    let history_days = match period {
        "month" => 61,
        "week" => 15,
        // Today needs the current partial day plus seven matching historical
        // windows. The extra calendar-day margin also covers local-midnight
        // boundaries without making unchanged files parse on every refresh.
        _ => 8,
    };
    now_ms - Duration::days(history_days).num_milliseconds()
}

fn cached_live_quota(
    cache: &Mutex<Option<(Instant, Vec<QuotaSample>)>>,
) -> Result<Vec<QuotaSample>> {
    let mut guard = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("quota cache lock poisoned"))?;
    if let Some((captured, value)) = guard.as_ref() {
        let ttl = if value.is_empty() { 240 } else { 60 };
        if captured.elapsed() < StdDuration::from_secs(ttl) {
            return Ok(value.clone());
        }
    }
    match app_server::read_codex_quota(StdDuration::from_secs(4)) {
        Ok(value) => {
            *guard = Some((Instant::now(), value.clone()));
            Ok(value)
        }
        Err(error) => {
            // An unavailable CLI should not make every periodic widget refresh
            // pay the full process timeout. The empty sentinel uses a longer TTL.
            *guard = Some((Instant::now(), Vec::new()));
            Err(error)
        }
    }
}

fn ingest_sources(
    connection: &mut Connection,
    cutoff_ms: i64,
    retention_cutoff_ms: i64,
) -> Result<ScanReport> {
    let adapters: Vec<Box<dyn AgentAdapter>> = vec![
        Box::new(CodexAdapter::detected()),
        Box::new(ClaudeAdapter::detected()),
        Box::new(OpencodeAdapter::detected()),
    ];
    let mut report = ScanReport::default();

    for adapter in adapters {
        // Parser-version migrations must revisit unchanged files as well as
        // recently modified ones. Discover every source once, but only rebuild
        // the history that the database itself retains.
        let migration_pending =
            storage::adapter_has_stale_parser_sources(connection, adapter.id())?;
        let discovery_cutoff_ms = if migration_pending {
            i64::MIN
        } else {
            cutoff_ms
        };
        let candidates = adapter.discover(discovery_cutoff_ms);
        let stored_diagnostics = stored_adapter_scan_diagnostics(connection, adapter.id())?;
        report
            .discovered
            .insert(adapter.id().into(), candidates.len());
        for candidate in candidates {
            let source_cutoff_ms = if migration_pending
                || storage::source_needs_parser_rebuild(connection, &candidate.source_id)?
            {
                retention_cutoff_ms.min(cutoff_ms)
            } else {
                cutoff_ms
            };
            if storage::source_is_current(
                connection,
                &candidate.source_id,
                candidate.size,
                candidate.mtime_ns,
                source_cutoff_ms,
            )? {
                if let Some(diagnostics) = stored_diagnostics.get(&candidate.source_id) {
                    record_scan_diagnostics(&mut report, adapter.id(), diagnostics);
                }
                continue;
            }

            match adapter.parse(&candidate, source_cutoff_ms) {
                Ok(scan) => {
                    let mut diagnostics = scan.diagnostics;
                    if let Ok(outcome) =
                        storage::replace_source(connection, &scan.source, source_cutoff_ms)
                    {
                        diagnostics.rejected_events += outcome.rejected_events;
                        if let Some(marker) = diagnostics.storage_marker() {
                            connection
                                .execute(
                                    "UPDATE scan_source SET last_error = ?1 WHERE source_id = ?2",
                                    params![marker, candidate.source_id],
                                )
                                .context("failed to persist JSONL scan diagnostics")?;
                        }
                        record_scan_diagnostics(&mut report, adapter.id(), &diagnostics);
                        *report.refreshed.entry(adapter.id().into()).or_default() += 1;
                    } else {
                        *report.errors.entry(adapter.id().into()).or_default() += 1;
                    }
                }
                Err(_) => {
                    *report.errors.entry(adapter.id().into()).or_default() += 1;
                }
            }
        }
    }

    Ok(report)
}

fn stored_adapter_scan_diagnostics(
    connection: &Connection,
    adapter_id: &str,
) -> Result<HashMap<String, ScanDiagnostics>> {
    let mut statement = connection.prepare(
        "SELECT source_id, last_error FROM scan_source
         WHERE adapter_id = ?1 AND last_error IS NOT NULL",
    )?;
    let rows = statement.query_map([adapter_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut diagnostics = HashMap::new();
    for row in rows {
        let (source_id, marker) = row?;
        if let Some(parsed) = ScanDiagnostics::from_storage_marker(&marker) {
            diagnostics.insert(source_id, parsed);
        }
    }
    Ok(diagnostics)
}

fn record_scan_diagnostics(
    report: &mut ScanReport,
    adapter_id: &str,
    diagnostics: &ScanDiagnostics,
) {
    if !diagnostics.is_partial() {
        return;
    }
    let aggregate = report.diagnostics.entry(adapter_id.into()).or_default();
    aggregate.partial_sources += 1;
    aggregate.malformed_lines += diagnostics.malformed_lines;
    aggregate.unreadable_lines += diagnostics.unreadable_lines;
    aggregate.rejected_events += diagnostics.rejected_events;
}

fn query_snapshot(
    connection: &Connection,
    requested_period: &str,
    report: ScanReport,
) -> Result<UsageSnapshot> {
    query_snapshot_at(connection, requested_period, report, Local::now())
}

fn query_snapshot_at(
    connection: &Connection,
    requested_period: &str,
    report: ScanReport,
    local_now: chrono::DateTime<Local>,
) -> Result<UsageSnapshot> {
    let period = match requested_period {
        "week" | "month" => requested_period,
        _ => "today",
    };
    let today = local_now.date_naive();
    let (start_date, comparison_days) = match period {
        "week" => (today - Duration::days(6), 7_i64),
        "month" => (today - Duration::days(29), 30_i64),
        _ => (today, 7_i64),
    };
    let bucket_count = period_bucket_count(period, local_now.hour());
    let start_ms = local_midnight(start_date)?.timestamp_millis();
    let end_ms = local_now.timestamp_millis() + 1;
    let events = load_events(connection, start_ms, end_ms)?;

    let mut buckets: HashMap<&str, Vec<i64>> = AGENT_IDS
        .iter()
        .map(|agent| (*agent, vec![0_i64; bucket_count]))
        .collect();
    let mut totals: HashMap<&str, i64> = AGENT_IDS.iter().map(|agent| (*agent, 0)).collect();

    for event in events {
        let Some(agent) = AGENT_IDS.iter().find(|agent| **agent == event.adapter) else {
            continue;
        };
        let local = match Local.timestamp_millis_opt(event.timestamp).single() {
            Some(value) => value,
            None => continue,
        };
        let index = if period == "today" {
            local.hour() as usize
        } else {
            (local.date_naive() - start_date).num_days().max(0) as usize
        };
        if index >= bucket_count {
            continue;
        }
        buckets.get_mut(agent).expect("registered agent")[index] += event.tokens;
        *totals.entry(agent).or_default() += event.tokens;
    }

    if period == "today" {
        for bucket in buckets.values_mut() {
            for index in 1..bucket_count {
                bucket[index] += bucket[index - 1];
            }
        }
    }

    let series = (0..bucket_count)
        .map(|index| SeriesPoint {
            label: bucket_label(period, start_date, index),
            tokens: AGENT_IDS
                .iter()
                .map(|agent| ((*agent).to_owned(), buckets[agent][index]))
                .collect(),
        })
        .collect();

    let total_tokens: i64 = totals.values().sum();
    let denominator = if period == "today" {
        average_prior_elapsed_windows(connection, &local_now, comparison_days)?
    } else {
        let previous_start = start_date - Duration::days(comparison_days);
        let previous_final_date = start_date - Duration::days(1);
        let elapsed_today = local_now.signed_duration_since(local_midnight(today)?);
        let previous_final_start = local_midnight(previous_final_date)?;
        let next_period_start = local_midnight(start_date)?;
        let matching_end = (previous_final_start + elapsed_today).min(next_period_start);
        let previous_end_ms = matching_end
            .timestamp_millis()
            .saturating_add(1)
            .min(next_period_start.timestamp_millis());
        let previous_total = sum_tokens_between(
            connection,
            local_midnight(previous_start)?.timestamp_millis(),
            previous_end_ms,
        )?;
        previous_total as f64
    };
    let comparison_available = denominator > 0.0;
    let comparison_percent = if comparison_available {
        ((total_tokens as f64 - denominator) / denominator) * 100.0
    } else {
        0.0
    };
    let share = |tokens: i64| {
        if total_tokens > 0 {
            tokens as f64 * 100.0 / total_tokens as f64
        } else {
            0.0
        }
    };

    Ok(UsageSnapshot {
        generated_at: Utc::now().to_rfc3339(),
        period: period.into(),
        is_demo: false,
        total_tokens,
        comparison_percent,
        comparison_available,
        series,
        quota: load_quota(connection, "primary")?,
        secondary_quota: load_quota(connection, "secondary")?,
        agents: AGENT_IDS
            .iter()
            .map(|agent| AgentSummary {
                id: (*agent).to_owned(),
                tokens: totals[agent],
                share: share(totals[agent]),
            })
            .collect(),
        sources: source_views(report, sync::sync_view(connection).ok()),
    })
}

fn average_prior_elapsed_windows(
    connection: &Connection,
    local_now: &chrono::DateTime<Local>,
    days: i64,
) -> Result<f64> {
    if days <= 0 {
        return Ok(0.0);
    }

    let today = local_now.date_naive();
    let elapsed = local_now.signed_duration_since(local_midnight(today)?);
    let mut total = 0_i64;
    for offset in 1..=days {
        let date = today - Duration::days(offset);
        let start = local_midnight(date)?;
        let next_start = local_midnight(date + Duration::days(1))?;
        let matching_end = (start + elapsed).min(next_start);
        let end_ms = matching_end
            .timestamp_millis()
            .saturating_add(1)
            .min(next_start.timestamp_millis());
        total += sum_tokens_between(connection, start.timestamp_millis(), end_ms)?;
    }
    Ok(total as f64 / days as f64)
}

fn period_bucket_count(period: &str, current_hour: u32) -> usize {
    match period {
        "week" => 7,
        "month" => 30,
        _ => current_hour.min(23) as usize + 1,
    }
}

fn load_events(connection: &Connection, start_ms: i64, end_ms: i64) -> Result<Vec<StoredEvent>> {
    let mut statement = connection.prepare(
        "SELECT adapter_id, occurred_at_ms, processed_tokens
         FROM usage_event
         WHERE occurred_at_ms >= ?1 AND occurred_at_ms < ?2
         UNION ALL
         SELECT adapter_id, occurred_at_ms, processed_tokens
         FROM remote_usage_event
         WHERE occurred_at_ms >= ?1 AND occurred_at_ms < ?2
         ORDER BY occurred_at_ms",
    )?;
    let rows = statement.query_map(params![start_ms, end_ms], |row| {
        Ok(StoredEvent {
            adapter: row.get(0)?,
            timestamp: row.get(1)?,
            tokens: row.get(2)?,
        })
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

fn sum_tokens_between(connection: &Connection, start_ms: i64, end_ms: i64) -> Result<i64> {
    connection
        .query_row(
            "SELECT (SELECT COALESCE(SUM(processed_tokens), 0) FROM usage_event
                     WHERE occurred_at_ms >= ?1 AND occurred_at_ms < ?2)
                  + (SELECT COALESCE(SUM(processed_tokens), 0) FROM remote_usage_event
                     WHERE occurred_at_ms >= ?1 AND occurred_at_ms < ?2)",
            params![start_ms, end_ms],
            |row| row.get(0),
        )
        .context("failed to calculate comparison usage")
}

fn load_quota(connection: &Connection, window_key: &str) -> Result<QuotaView> {
    let row = connection.query_row(
        "SELECT remaining_percent, resets_at_ms, source_label, quality, collected_at_ms
         FROM quota_snapshot WHERE adapter_id = 'codex' AND window_key = ?1",
        [window_key],
        |row| {
            Ok((
                row.get::<_, f64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        },
    );

    match row {
        Ok((remaining, reset, source, quality, collected_at_ms)) => {
            let now = Utc::now().timestamp_millis();
            let age_minutes = ((now - collected_at_ms).max(0) as f64) / 60_000.0;
            let reset_expired = reset.is_some_and(|value| value <= now);
            let stale_after_minutes = if quality == "official_live" {
                7.0
            } else {
                15.0
            };
            Ok(QuotaView {
                available: true,
                remaining_percent: remaining,
                resets_in_minutes: reset
                    .filter(|value| *value > now)
                    .map(|value| (value - now) as f64 / 60_000.0),
                age_minutes: Some(age_minutes),
                stale: age_minutes > stale_after_minutes || reset_expired,
                reset_expired,
                source_label: source,
                quality,
            })
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(QuotaView {
            available: false,
            remaining_percent: 0.0,
            resets_in_minutes: None,
            age_minutes: None,
            stale: false,
            reset_expired: false,
            source_label: "暂无可靠来源".into(),
            quality: "unavailable".into(),
        }),
        Err(error) => Err(error.into()),
    }
}

fn source_views(report: ScanReport, sync_status: Option<SyncView>) -> Vec<SourceView> {
    let discovered = |id: &str| report.discovered.get(id).copied().unwrap_or(0);
    let refreshed = |id: &str| report.refreshed.get(id).copied().unwrap_or(0);
    let errors = |id: &str| report.errors.get(id).copied().unwrap_or(0);
    let diagnostics = |id: &str| report.diagnostics.get(id).cloned().unwrap_or_default();
    let codex_diagnostics = diagnostics("codex");
    let claude_diagnostics = diagnostics("claude");
    let opencode_diagnostics = diagnostics("opencode");
    let codex_partial = codex_diagnostics.partial_sources > 0 || errors("codex") > 0;
    let claude_partial = claude_diagnostics.partial_sources > 0 || errors("claude") > 0;
    let opencode_partial = opencode_diagnostics.partial_sources > 0 || errors("opencode") > 0;
    let mut views = vec![
        SourceView {
            id: "codex-quota".into(),
            kind: "official".into(),
            label: "ChatGPT / Codex 官方配额".into(),
            detail: "采集主、次官方滚动窗口；桌面小插件仅展示主短窗，完整视图同时展示两者。优先读取本机 ChatGPT / Codex app-server，失败时使用带时间标记的日志快照。".into(),
            quality: "official".into(),
            quality_label: "官方".into(),
        },
        SourceView {
            id: "codex-local".into(),
            kind: "local".into(),
            label: "ChatGPT / Codex 本地 Token".into(),
            detail: format!(
                "发现 {} 个近期会话，本次更新 {} 个。{}累计快照按正增量入账；总量包含未缓存输入、缓存读取与输出，子项不重复相加。",
                discovered("codex"),
                refreshed("codex"),
                coverage_detail(&codex_diagnostics, errors("codex"))
            ),
            quality: if codex_partial { "partial" } else { "exact" }.into(),
            quality_label: if codex_partial {
                "部分覆盖"
            } else {
                "精确解析"
            }
            .into(),
        },
        SourceView {
            id: "claude-local".into(),
            kind: "local".into(),
            label: "Claude Code 本地 Token".into(),
            detail: format!(
                "发现 {} 个近期会话，本次更新 {} 个。{}重复消息跨会话按消息标识合并；总量包含缓存读取，配额不推算。",
                discovered("claude"),
                refreshed("claude"),
                coverage_detail(&claude_diagnostics, errors("claude"))
            ),
            quality: if claude_partial { "partial" } else { "exact" }.into(),
            quality_label: if claude_partial {
                "部分覆盖"
            } else {
                "精确解析"
            }
            .into(),
        },
        SourceView {
            id: "opencode-local".into(),
            kind: "local".into(),
            label: "OpenCode 本地 Token".into(),
            detail: format!(
                "发现 {} 条近期消息，本次更新 {} 条。{}读取消息 usage 字段并以消息标识去重；未安装 OpenCode 时保持为 0，不做推算。",
                discovered("opencode"),
                refreshed("opencode"),
                coverage_detail(&opencode_diagnostics, errors("opencode"))
            ),
            quality: if opencode_partial { "partial" } else { "exact" }.into(),
            quality_label: if opencode_partial {
                "部分覆盖"
            } else {
                "精确解析"
            }
            .into(),
        },
    ];

    if let Some(sync_status) = sync_status.filter(|status| status.enabled) {
        let device_count = sync_status.devices.len();
        let remote_events: i64 = sync_status.devices.iter().map(|device| device.events).sum();
        let failed = sync_status.last_error.is_some();
        views.push(SourceView {
            id: "device-sync".into(),
            kind: "sync".into(),
            label: "多设备同步".into(),
            detail: match (&sync_status.last_error, device_count) {
                (Some(error), _) => format!("上次同步未完全成功：{error}。合并数字可能滞后，本机统计不受影响。"),
                (None, 0) => "已开启文件夹同步，尚未发现其他设备的导出。其他电脑指向同一文件夹后会自动合并。".into(),
                (None, count) => format!(
                    "已合并 {count} 台其他设备的 {remote_events} 条统计事件；导出只含事件标识、Agent、时间与 token 数，不含任何对话内容。"
                ),
            },
            quality: if failed { "partial" } else { "exact" }.into(),
            quality_label: if failed { "部分覆盖" } else { "精确解析" }.into(),
        });
    }

    views
}

fn coverage_detail(diagnostics: &AdapterDiagnostics, errors: usize) -> String {
    if diagnostics.partial_sources == 0 && errors == 0 {
        return String::new();
    }

    let skipped = (diagnostics.partial_sources > 0).then(|| {
        format!(
            "{} 个会话存在未计入的 JSONL 内容（格式异常 {} 行、文本读取失败 {} 行、身份冲突 {} 条）；",
            diagnostics.partial_sources,
            diagnostics.malformed_lines,
            diagnostics.unreadable_lines,
            diagnostics.rejected_events
        )
    });
    let failed = (errors > 0).then(|| format!("另有 {errors} 个会话未能完成更新；"));
    format!(
        "{}{}本周期总量仅覆盖成功解析的记录，可能不完整。",
        skipped.unwrap_or_default(),
        failed.unwrap_or_default()
    )
}

fn local_midnight(date: NaiveDate) -> Result<chrono::DateTime<Local>> {
    let naive = date
        .and_hms_opt(0, 0, 0)
        .context("invalid local midnight")?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .context("local midnight is unavailable")
}

fn bucket_label(period: &str, start: NaiveDate, index: usize) -> String {
    if period == "today" {
        return format!("{index:02}:00");
    }
    let date = start + Duration::days(index as i64);
    if period == "week" {
        return match date.weekday() {
            Weekday::Mon => "周一",
            Weekday::Tue => "周二",
            Weekday::Wed => "周三",
            Weekday::Thu => "周四",
            Weekday::Fri => "周五",
            Weekday::Sat => "周六",
            Weekday::Sun => "周日",
        }
        .into();
    }
    format!("{} 日", date.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_local_time(date: NaiveDate, hour: u32) -> chrono::DateTime<Local> {
        Local
            .from_local_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
            .single()
            .unwrap()
    }

    fn insert_test_usage(
        connection: &Connection,
        event_id: &str,
        occurred_at_ms: i64,
        tokens: i64,
    ) {
        connection
            .execute(
                "INSERT INTO usage_event (
                    event_id, adapter_id, event_key, occurred_at_ms, session_id,
                    model, input_uncached_tokens, cache_read_tokens,
                    cache_write_tokens, output_tokens, reasoning_tokens,
                    processed_tokens, quality, payload_hash
                 ) VALUES (?1, 'codex', ?1, ?2, 'session', NULL, ?3, 0, 0, 0, 0, ?3, 'exact', ?1)",
                params![event_id, occurred_at_ms, tokens],
            )
            .unwrap();
    }

    #[test]
    fn skipped_jsonl_lines_downgrade_only_the_affected_source_payload() {
        let mut report = ScanReport::default();
        report.discovered.insert("codex".into(), 2);
        report.refreshed.insert("codex".into(), 1);
        record_scan_diagnostics(
            &mut report,
            "codex",
            &ScanDiagnostics {
                malformed_lines: 2,
                unreadable_lines: 1,
                ..Default::default()
            },
        );

        let views = source_views(report, None);
        let codex = views
            .iter()
            .find(|source| source.id == "codex-local")
            .unwrap();
        let claude = views
            .iter()
            .find(|source| source.id == "claude-local")
            .unwrap();

        assert_eq!(codex.quality, "partial");
        assert_eq!(codex.quality_label, "部分覆盖");
        assert!(codex.detail.contains("格式异常 2 行"));
        assert!(codex.detail.contains("文本读取失败 1 行"));
        assert!(codex.detail.contains("可能不完整"));
        assert_eq!(claude.quality, "exact");
        assert_eq!(claude.quality_label, "精确解析");
    }

    #[test]
    fn source_refresh_error_is_not_presented_as_exact() {
        let mut report = ScanReport::default();
        report.errors.insert("claude".into(), 1);

        let views = source_views(report, None);
        let claude = views
            .iter()
            .find(|source| source.id == "claude-local")
            .unwrap();

        assert_eq!(claude.quality, "partial");
        assert!(claude.detail.contains("1 个会话未能完成更新"));
    }

    #[test]
    fn rejected_claude_event_is_presented_as_partial_coverage() {
        let mut report = ScanReport::default();
        report.discovered.insert("claude".into(), 1);
        report.refreshed.insert("claude".into(), 1);
        record_scan_diagnostics(
            &mut report,
            "claude",
            &ScanDiagnostics {
                rejected_events: 1,
                ..Default::default()
            },
        );

        let claude = source_views(report, None)
            .into_iter()
            .find(|source| source.id == "claude-local")
            .unwrap();

        assert_eq!(claude.quality, "partial");
        assert!(claude.detail.contains("身份冲突 1 条"));
        assert!(claude.detail.contains("可能不完整"));
    }

    #[test]
    fn persisted_diagnostics_survive_an_unchanged_source_scan() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE scan_source (
                    source_id TEXT PRIMARY KEY,
                    adapter_id TEXT NOT NULL,
                    last_error TEXT
                );",
            )
            .unwrap();
        let marker = ScanDiagnostics {
            malformed_lines: 3,
            unreadable_lines: 1,
            ..Default::default()
        }
        .storage_marker()
        .unwrap();
        connection
            .execute(
                "INSERT INTO scan_source (source_id, adapter_id, last_error)
                 VALUES ('source-a', 'codex', ?1)",
                [marker],
            )
            .unwrap();

        let stored = stored_adapter_scan_diagnostics(&connection, "codex").unwrap();

        assert_eq!(
            stored.get("source-a"),
            Some(&ScanDiagnostics {
                malformed_lines: 3,
                unreadable_lines: 1,
                ..Default::default()
            })
        );
    }

    #[test]
    fn today_series_stops_at_the_current_hour() {
        assert_eq!(period_bucket_count("today", 0), 1);
        assert_eq!(period_bucket_count("today", 11), 12);
        assert_eq!(period_bucket_count("today", 23), 24);
    }

    #[test]
    fn week_and_month_bucket_counts_remain_fixed() {
        assert_eq!(period_bucket_count("week", 0), 7);
        assert_eq!(period_bucket_count("week", 23), 7);
        assert_eq!(period_bucket_count("month", 0), 30);
        assert_eq!(period_bucket_count("month", 23), 30);
    }

    #[test]
    fn today_discovery_keeps_an_eight_day_history_horizon() {
        let now = 1_800_000_000_000_i64;
        assert_eq!(
            discovery_cutoff_ms("today", now),
            now - Duration::days(8).num_milliseconds()
        );
        assert_eq!(
            discovery_cutoff_ms("week", now),
            now - Duration::days(15).num_milliseconds()
        );
        assert_eq!(
            discovery_cutoff_ms("month", now),
            now - Duration::days(61).num_milliseconds()
        );
    }

    #[test]
    fn today_comparison_uses_the_same_elapsed_window_from_each_prior_day() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);

        insert_test_usage(
            &connection,
            "today-before-cutoff",
            test_local_time(today, 11).timestamp_millis(),
            200,
        );
        for offset in 1..=7 {
            let date = today - Duration::days(offset);
            insert_test_usage(
                &connection,
                &format!("prior-{offset}-before-cutoff"),
                test_local_time(date, 10).timestamp_millis(),
                100,
            );
            insert_test_usage(
                &connection,
                &format!("prior-{offset}-after-cutoff"),
                test_local_time(date, 18).timestamp_millis(),
                900,
            );
        }

        let snapshot =
            query_snapshot_at(&connection, "today", ScanReport::default(), local_now).unwrap();

        assert_eq!(snapshot.total_tokens, 200);
        assert!(snapshot.comparison_available);
        assert!((snapshot.comparison_percent - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn week_comparison_matches_elapsed_time_on_the_previous_final_day() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);
        insert_test_usage(
            &connection,
            "current-week-evening",
            test_local_time(today - Duration::days(1), 18).timestamp_millis(),
            200,
        );
        insert_test_usage(
            &connection,
            "previous-week-before-cutoff",
            test_local_time(today - Duration::days(7), 10).timestamp_millis(),
            100,
        );
        insert_test_usage(
            &connection,
            "previous-week-after-cutoff",
            test_local_time(today - Duration::days(7), 18).timestamp_millis(),
            900,
        );

        let snapshot =
            query_snapshot_at(&connection, "week", ScanReport::default(), local_now).unwrap();

        assert_eq!(snapshot.total_tokens, 200);
        assert!(snapshot.comparison_available);
        assert!((snapshot.comparison_percent - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn month_comparison_matches_elapsed_time_on_the_previous_final_day() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);
        insert_test_usage(
            &connection,
            "current-month-evening",
            test_local_time(today - Duration::days(1), 18).timestamp_millis(),
            200,
        );
        insert_test_usage(
            &connection,
            "previous-month-before-cutoff",
            test_local_time(today - Duration::days(30), 10).timestamp_millis(),
            100,
        );
        insert_test_usage(
            &connection,
            "previous-month-after-cutoff",
            test_local_time(today - Duration::days(30), 18).timestamp_millis(),
            900,
        );

        let snapshot =
            query_snapshot_at(&connection, "month", ScanReport::default(), local_now).unwrap();

        assert_eq!(snapshot.total_tokens, 200);
        assert!(snapshot.comparison_available);
        assert!((snapshot.comparison_percent - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn expired_quota_snapshot_is_marked_stale() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let now = Utc::now().timestamp_millis();
        connection
            .execute(
                "INSERT INTO quota_snapshot (
                    adapter_id, window_key, remaining_percent, resets_at_ms,
                    collected_at_ms, quality, source_label
                 ) VALUES ('codex', 'primary', 72, ?1, ?2, 'official_snapshot', 'log')",
                params![now - 1_000, now - Duration::minutes(30).num_milliseconds()],
            )
            .unwrap();

        let quota = load_quota(&connection, "primary").unwrap();
        assert!(quota.available);
        assert!(quota.stale);
        assert!(quota.reset_expired);
        assert!(quota.resets_in_minutes.is_none());
        assert!(quota.age_minutes.unwrap() >= 29.0);
    }

    #[test]
    fn recent_live_quota_is_fresh() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let now = Utc::now().timestamp_millis();
        connection
            .execute(
                "INSERT INTO quota_snapshot (
                    adapter_id, window_key, remaining_percent, resets_at_ms,
                    collected_at_ms, quality, source_label
                 ) VALUES ('codex', 'primary', 72, ?1, ?2, 'official_live', 'app-server')",
                params![now + Duration::hours(2).num_milliseconds(), now],
            )
            .unwrap();

        let quota = load_quota(&connection, "primary").unwrap();
        assert!(quota.available);
        assert!(!quota.stale);
        assert!(!quota.reset_expired);
        assert!(quota.resets_in_minutes.unwrap() > 119.0);
    }

    #[test]
    fn live_quota_stays_fresh_across_the_compact_refresh_interval() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let now = Utc::now().timestamp_millis();
        connection
            .execute(
                "INSERT INTO quota_snapshot (
                    adapter_id, window_key, remaining_percent, resets_at_ms,
                    collected_at_ms, quality, source_label
                 ) VALUES ('codex', 'primary', 72, ?1, ?2, 'official_live', 'app-server')",
                params![
                    now + Duration::hours(2).num_milliseconds(),
                    now - Duration::minutes(5).num_milliseconds()
                ],
            )
            .unwrap();

        let quota = load_quota(&connection, "primary").unwrap();
        assert!(!quota.stale);
    }

    #[test]
    fn secondary_quota_window_is_loaded_independently() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let now = Utc::now().timestamp_millis();
        connection
            .execute(
                "INSERT INTO quota_snapshot (
                    adapter_id, window_key, remaining_percent, resets_at_ms,
                    collected_at_ms, quality, source_label
                 ) VALUES ('codex', 'primary', 72, ?1, ?2, 'official_live', 'app-server'),
                          ('codex', 'secondary', 83, ?3, ?2, 'official_live', 'app-server')",
                params![
                    now + Duration::hours(2).num_milliseconds(),
                    now,
                    now + Duration::days(6).num_milliseconds()
                ],
            )
            .unwrap();

        let primary = load_quota(&connection, "primary").unwrap();
        let secondary = load_quota(&connection, "secondary").unwrap();
        assert_eq!(primary.remaining_percent, 72.0);
        assert_eq!(secondary.remaining_percent, 83.0);
        assert!(!secondary.stale);
        assert!(secondary.resets_in_minutes.unwrap() > 8_639.0);
        assert_eq!(secondary.source_label, "app-server");
    }

    #[test]
    fn missing_secondary_quota_is_not_filled_from_primary() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let now = Utc::now().timestamp_millis();
        connection
            .execute(
                "INSERT INTO quota_snapshot (
                    adapter_id, window_key, remaining_percent, resets_at_ms,
                    collected_at_ms, quality, source_label
                 ) VALUES ('codex', 'primary', 72, ?1, ?2, 'official_live', 'app-server')",
                params![now + Duration::hours(2).num_milliseconds(), now],
            )
            .unwrap();

        let secondary = load_quota(&connection, "secondary").unwrap();
        assert!(!secondary.available);
        assert_eq!(secondary.remaining_percent, 0.0);
        assert_eq!(secondary.quality, "unavailable");
    }

    #[test]
    #[ignore = "reads the current user's local Codex and Claude Code logs"]
    fn live_snapshot_smoke_test() {
        let database = std::env::temp_dir().join(format!(
            "metrik-live-smoke-{}-{}.sqlite3",
            std::process::id(),
            Utc::now().timestamp_millis()
        ));
        let quota_cache = Mutex::new(None);
        let snapshot = build_snapshot(&database, "today", &quota_cache).unwrap();
        println!(
            "live snapshot: total={}, codex={}, claude={}, quota_available={}, quota_remaining={:.1}, quota_source={}",
            snapshot.total_tokens,
            snapshot.agents[0].tokens,
            snapshot.agents[1].tokens,
            snapshot.quota.available,
            snapshot.quota.remaining_percent,
            snapshot.quota.source_label
        );
        assert!(!snapshot.is_demo);
        assert!((1..=24).contains(&snapshot.series.len()));
        let expected_last_label = format!("{:02}:00", snapshot.series.len() - 1);
        assert_eq!(
            snapshot.series.last().map(|point| point.label.as_str()),
            Some(expected_last_label.as_str())
        );
        std::fs::remove_file(database).ok();
    }
}
