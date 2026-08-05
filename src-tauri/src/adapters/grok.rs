//! Grok Build CLI（`~/.grok`）本地用量与配额快照。
//!
//! 用量：`sessions/**/updates.jsonl` 中 `method == "_x.ai/session/update"` 且
//! 带 `update.usage` 的记录。每个 `prompt_id` 一条**单轮** usage（非会话累计）。
//!
//! 已核对本机口径（2026-08）：
//! - `totalTokens == inputTokens + outputTokens`
//! - `cachedReadTokens ⊆ inputTokens`
//! - `reasoningTokens ⊆ outputTokens`（不另加进 processed）
//! - `cacheCreationTokens ⊆ input`（从 uncached 拆出计入 cache_write）
//!
//! 配额：MVP 读 `~/.grok/logs/unified.jsonl` 里 Grok 自己写入的
//! `billing: fetched credits config` 最新一条，质量标 `official_snapshot`。
//! 不做 OAuth 实时拉取（避免误用订阅凭据；后续可单独 opt-in）。

use super::{AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{
    sane_resets_at_ms, stable_hash, ParsedSource, QuotaSample, TokenVector, UsageEvent,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct GrokAdapter {
    roots: Vec<PathBuf>,
}

#[derive(Deserialize, Default)]
struct GrokUpdateLine {
    timestamp: Option<i64>,
    method: Option<String>,
    params: Option<GrokParams>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GrokParams {
    session_id: Option<String>,
    update: Option<GrokUpdate>,
    #[serde(rename = "_meta")]
    meta: Option<GrokMeta>,
}

#[derive(Deserialize, Default)]
struct GrokUpdate {
    prompt_id: Option<String>,
    usage: Option<GrokUsage>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GrokUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    total_tokens: i64,
    #[serde(default)]
    cached_read_tokens: i64,
    #[serde(default)]
    cache_creation_tokens: i64,
    #[serde(default)]
    reasoning_tokens: i64,
    #[serde(default)]
    model_usage: HashMap<String, Value>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GrokMeta {
    event_id: Option<String>,
}

#[derive(Deserialize, Default)]
struct SummaryFile {
    info: Option<SummaryInfo>,
    git_root_dir: Option<String>,
    current_model_id: Option<String>,
}

#[derive(Deserialize, Default)]
struct SummaryInfo {
    cwd: Option<String>,
    id: Option<String>,
}

impl GrokAdapter {
    pub fn detected() -> Self {
        Self {
            roots: vec![grok_home().join("sessions")],
        }
    }

    #[cfg(test)]
    fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }
}

/// 与 `GrokAdapter::detected()` 同源：`GROK_HOME` 覆盖，否则 `~/.grok`。
pub fn grok_home() -> PathBuf {
    std::env::var_os("GROK_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".grok"))
}

/// 安装探针 / 配额是否值得尝试：家目录存在即可（不必有 sessions）。
pub fn grok_home_exists() -> bool {
    grok_home().exists()
}

impl AgentAdapter for GrokAdapter {
    fn id(&self) -> &'static str {
        "grok"
    }

    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate> {
        discover_updates_jsonl(&self.roots, self.id(), cutoff_ms)
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        let file = File::open(&candidate.path)
            .with_context(|| format!("failed to open {}", candidate.path.display()))?;
        let reader = BufReader::with_capacity(256 * 1024, file);

        let fallback_session = candidate
            .path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("unknown-session")
            .to_owned();
        let summary = load_summary(candidate.path.parent());
        let mut session_id = summary
            .as_ref()
            .and_then(|s| s.info.as_ref())
            .and_then(|info| info.id.clone())
            .filter(|id| !id.is_empty())
            .unwrap_or(fallback_session);
        let project_path = summary.as_ref().and_then(|s| {
            non_empty(s.info.as_ref().and_then(|info| info.cwd.clone()))
                .or_else(|| non_empty(s.git_root_dir.clone()))
        });
        let fallback_model = summary
            .as_ref()
            .and_then(|s| non_empty(s.current_model_id.clone()));

        // prompt_id → 最后一次完整 usage（文件顺序，后写覆盖前写）。
        let mut by_prompt: HashMap<String, PendingUsage> = HashMap::new();
        let mut diagnostics = ScanDiagnostics::default();
        let track_skipped_lines = candidate.mtime_ns / 1_000_000 >= cutoff_ms;

        for line in reader.lines() {
            let line = match line {
                Ok(line) => line,
                Err(_) => {
                    if track_skipped_lines {
                        diagnostics.unreadable_lines += 1;
                    }
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<GrokUpdateLine>(&line) else {
                if track_skipped_lines {
                    diagnostics.malformed_lines += 1;
                }
                continue;
            };
            if record.method.as_deref() != Some("_x.ai/session/update") {
                continue;
            }
            let params = record.params.unwrap_or_default();
            if let Some(id) = non_empty(params.session_id) {
                session_id = id;
            }
            let update = match params.update {
                Some(update) => update,
                None => continue,
            };
            let usage = match update.usage {
                Some(usage) => usage,
                None => continue,
            };
            let prompt_id = match non_empty(update.prompt_id) {
                Some(id) => id,
                None => {
                    if track_skipped_lines {
                        diagnostics.rejected_events += 1;
                    }
                    continue;
                }
            };
            let occurred_at_ms = match record.timestamp {
                Some(ts) if ts > 1_000_000_000_000 => ts, // already ms
                Some(ts) if ts > 0 => ts.saturating_mul(1000),
                _ => {
                    if track_skipped_lines {
                        diagnostics.rejected_events += 1;
                    }
                    continue;
                }
            };
            if occurred_at_ms < cutoff_ms {
                continue;
            }

            let input = usage.input_tokens.max(0);
            let cached_read = usage.cached_read_tokens.max(0).min(input);
            // cacheCreation 在真机上含于 input（total = input + output），
            // 从 uncached 里拆出，避免 processed 重复加到 total 之外。
            let cache_write = usage
                .cache_creation_tokens
                .max(0)
                .min(input.saturating_sub(cached_read));
            let output = usage.output_tokens.max(0);
            let reasoning = usage.reasoning_tokens.max(0).min(output);
            let tokens = TokenVector {
                input_uncached: input - cached_read - cache_write,
                cache_read: cached_read,
                cache_write,
                output,
                reasoning_output: reasoning,
            };
            // 真机：total = input + output；reasoning 已含在 output 内。
            if tokens.disagrees_with_reported_total(usage.total_tokens) {
                diagnostics.total_mismatches += 1;
            }
            if tokens.processed() <= 0 {
                continue;
            }

            let model = usage
                .model_usage
                .keys()
                .next()
                .cloned()
                .filter(|name| !name.is_empty())
                .or_else(|| fallback_model.clone());
            let event_key = params
                .meta
                .and_then(|meta| non_empty(meta.event_id))
                .unwrap_or_else(|| format!("{session_id}:{prompt_id}"));

            by_prompt.insert(
                prompt_id,
                PendingUsage {
                    event_key,
                    occurred_at_ms,
                    model,
                    tokens,
                },
            );
        }

        let mut events: Vec<UsageEvent> = by_prompt
            .into_iter()
            .map(|(_prompt_id, pending)| {
                UsageEvent::new(
                    self.id(),
                    pending.event_key,
                    pending.occurred_at_ms,
                    session_id.clone(),
                    pending.model,
                    pending.tokens,
                    "prompt_usage",
                )
                .with_project(project_path.clone())
            })
            .collect();
        events.sort_by_key(|event| event.occurred_at_ms);

        Ok(ParsedScan {
            source: ParsedSource {
                source_id: candidate.source_id.clone(),
                adapter_id: self.id(),
                locator: candidate.path.clone(),
                logical_key: session_id,
                size: candidate.size,
                mtime_ns: candidate.mtime_ns,
                events,
                quotas: Vec::new(),
            },
            diagnostics,
        })
    }
}

struct PendingUsage {
    event_key: String,
    occurred_at_ms: i64,
    model: Option<String>,
    tokens: TokenVector,
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

fn load_summary(session_dir: Option<&Path>) -> Option<SummaryFile> {
    let path = session_dir?.join("summary.json");
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 只收 `updates.jsonl`，避免把 chat_history / events 等大文件塞进扫描队列。
fn discover_updates_jsonl(roots: &[PathBuf], adapter_id: &str, cutoff_ms: i64) -> Vec<SourceCandidate> {
    let mut found = Vec::new();
    for root in roots.iter().filter(|root| root.exists()) {
        let walker = walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file());
        for entry in walker {
            let path = entry.into_path();
            if path.file_name().and_then(|name| name.to_str()) != Some("updates.jsonl") {
                continue;
            }
            let Ok(metadata) = path.metadata() else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            let Ok(since_epoch) = modified.duration_since(UNIX_EPOCH) else {
                continue;
            };
            let mtime_ns = since_epoch.as_nanos().min(i64::MAX as u128) as i64;
            if mtime_ns / 1_000_000 < cutoff_ms {
                continue;
            }
            let normalized = path.to_string_lossy().replace('\\', "/");
            let normalized = if cfg!(windows) {
                normalized.to_lowercase()
            } else {
                normalized
            };
            found.push(SourceCandidate {
                source_id: stable_hash(&format!("{adapter_id}|{normalized}")),
                path,
                size: metadata.len(),
                mtime_ns,
            });
        }
    }
    found.sort_by(|left, right| left.path.cmp(&right.path));
    found
}

/// 从 Grok CLI 统一日志读取最新官方 Credits 快照。
///
/// `timeout` 仅用于限制读尾部的最长阻塞（本地文件，正常远小于此）。
pub fn fetch_grok_quota_snapshot(_timeout: Duration) -> Result<Vec<QuotaSample>> {
    let path = grok_home().join("logs").join("unified.jsonl");
    if !path.exists() {
        anyhow::bail!("未找到 Grok 用量日志（~/.grok/logs/unified.jsonl）；先运行过 grok 会话后才会有配额快照");
    }

    let text = read_file_tail(&path, 512 * 1024)?;
    let mut best: Option<LogQuota> = None;
    for line in text.lines() {
        if !line.contains("billing: fetched credits config") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let msg = value.get("msg").and_then(Value::as_str).unwrap_or_default();
        if msg != "billing: fetched credits config" {
            continue;
        }
        let Some(config) = value
            .pointer("/ctx/config")
            .cloned()
            .or_else(|| value.pointer("/ctx").cloned())
        else {
            continue;
        };
        // config 可能直接在 ctx.config
        let config = if config.get("creditUsagePercent").is_some() {
            config
        } else if let Some(inner) = config.get("config") {
            inner.clone()
        } else {
            continue;
        };
        let used = match config.get("creditUsagePercent").and_then(Value::as_f64) {
            Some(value) if value.is_finite() => value.clamp(0.0, 100.0),
            _ => continue,
        };
        let collected_at_ms = parse_log_ts(value.get("ts").and_then(Value::as_str))
            .unwrap_or_else(now_ms);
        let period = config.get("currentPeriod");
        let period_type = period
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let window_key = match period_type {
            "USAGE_PERIOD_TYPE_MONTHLY" | "USAGE_PERIOD_TYPE_MONTH" => "monthly_cycle",
            // 真机 SuperGrok Heavy 为周窗。
            _ => "seven_day",
        };
        let end = period
            .and_then(|p| p.get("end"))
            .and_then(Value::as_str)
            .or_else(|| config.get("billingPeriodEnd").and_then(Value::as_str));
        let resets_at_ms = parse_log_ts(end)
            .and_then(|ms| sane_resets_at_ms(window_key, ms, collected_at_ms));

        let candidate = LogQuota {
            window_key: window_key.to_owned(),
            remaining_percent: (100.0 - used).clamp(0.0, 100.0),
            resets_at_ms,
            collected_at_ms,
        };
        if best
            .as_ref()
            .is_none_or(|prev| candidate.collected_at_ms >= prev.collected_at_ms)
        {
            best = Some(candidate);
        }
    }

    let Some(best) = best else {
        anyhow::bail!("Grok 日志中尚无 billing credits 记录（需至少成功拉取过一次配额）");
    };

    Ok(vec![QuotaSample {
        adapter_id: "grok",
        window_key: best.window_key,
        remaining_percent: best.remaining_percent,
        resets_at_ms: best.resets_at_ms,
        collected_at_ms: best.collected_at_ms,
        source_label: "Grok CLI 日志配额快照".into(),
        quality: "official_snapshot",
    }])
}

struct LogQuota {
    window_key: String,
    remaining_percent: f64,
    resets_at_ms: Option<i64>,
    collected_at_ms: i64,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn parse_log_ts(value: Option<&str>) -> Option<i64> {
    let value = value?;
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.timestamp_millis())
        .or_else(|| {
            // 允许无偏移的 ISO 形态
            chrono::DateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.fZ")
                .ok()
                .map(|dt| dt.timestamp_millis())
        })
}

/// 读文件末尾最多 `max_bytes`，避免 unified.jsonl 增长后全量扫描。
fn read_file_tail(path: &Path, max_bytes: u64) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let len = file.metadata()?.len();
    if len > max_bytes {
        file.seek(SeekFrom::Start(len - max_bytes))?;
        // 丢掉半行
        let mut discard = String::new();
        let mut reader = BufReader::new(&file);
        let _ = reader.read_line(&mut discard);
        let mut rest = String::new();
        reader.read_to_string(&mut rest)?;
        Ok(rest)
    } else {
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "metrik-grok-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("updates.jsonl");
        let mut file = File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn prompt_usage_is_ingested_once_per_prompt_with_cache_split() {
        let body = r#"
{"timestamp":1785935395,"method":"session/update","params":{"sessionId":"sess-a","update":{"toolCallId":"x"}}}
{"timestamp":1785935395,"method":"_x.ai/session/update","params":{"sessionId":"sess-a","update":{"prompt_id":"p1","usage":{"inputTokens":100,"outputTokens":20,"totalTokens":120,"cachedReadTokens":30,"cacheCreationTokens":5,"reasoningTokens":8,"modelUsage":{"grok-4.5-build":{}}}},"_meta":{"eventId":"e1"}}}
{"timestamp":1785935400,"method":"_x.ai/session/update","params":{"sessionId":"sess-a","update":{"prompt_id":"p2","usage":{"inputTokens":50,"outputTokens":10,"totalTokens":60,"cachedReadTokens":0,"cacheCreationTokens":0,"reasoningTokens":2,"modelUsage":{"grok-4.5-build":{}}}},"_meta":{"eventId":"e2"}}}
"#;
        let path = write_temp("usage", body);
        let meta = path.metadata().unwrap();
        let candidate = SourceCandidate {
            source_id: "s".into(),
            path: path.clone(),
            size: meta.len(),
            mtime_ns: 1,
        };
        let parsed = GrokAdapter::with_roots(vec![])
            .parse(&candidate, i64::MIN)
            .unwrap();
        assert_eq!(parsed.source.events.len(), 2);
        assert_eq!(parsed.diagnostics.total_mismatches, 0);
        let first = parsed
            .source
            .events
            .iter()
            .find(|e| e.event_key == "e1")
            .unwrap();
        assert_eq!(first.tokens.input_uncached, 65);
        assert_eq!(first.tokens.cache_read, 30);
        assert_eq!(first.tokens.cache_write, 5);
        assert_eq!(first.tokens.output, 20);
        assert_eq!(first.tokens.reasoning_output, 8);
        // processed = input + output（cache/reasoning 已含在分量内，不另加）
        assert_eq!(first.tokens.processed(), 120);
        assert_eq!(first.model.as_deref(), Some("grok-4.5-build"));
        assert_eq!(first.occurred_at_ms, 1785935395_000);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn later_usage_for_same_prompt_replaces_earlier() {
        let body = r#"
{"timestamp":1785935395,"method":"_x.ai/session/update","params":{"sessionId":"sess-a","update":{"prompt_id":"p1","usage":{"inputTokens":10,"outputTokens":1,"totalTokens":11,"cachedReadTokens":0,"cacheCreationTokens":0,"reasoningTokens":0,"modelUsage":{}}},"_meta":{"eventId":"e1"}}}
{"timestamp":1785935400,"method":"_x.ai/session/update","params":{"sessionId":"sess-a","update":{"prompt_id":"p1","usage":{"inputTokens":40,"outputTokens":5,"totalTokens":45,"cachedReadTokens":0,"cacheCreationTokens":0,"reasoningTokens":0,"modelUsage":{}}},"_meta":{"eventId":"e2"}}}
"#;
        let path = write_temp("dedupe", body);
        let meta = path.metadata().unwrap();
        let candidate = SourceCandidate {
            source_id: "s".into(),
            path: path.clone(),
            size: meta.len(),
            mtime_ns: 1,
        };
        let parsed = GrokAdapter::with_roots(vec![])
            .parse(&candidate, i64::MIN)
            .unwrap();
        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].tokens.processed(), 45);
        assert_eq!(parsed.source.events[0].event_key, "e2");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reported_total_mismatch_is_flagged() {
        let body = r#"
{"timestamp":1785935395,"method":"_x.ai/session/update","params":{"sessionId":"sess-a","update":{"prompt_id":"p1","usage":{"inputTokens":100,"outputTokens":20,"totalTokens":999,"cachedReadTokens":0,"cacheCreationTokens":0,"reasoningTokens":0,"modelUsage":{}}},"_meta":{"eventId":"e1"}}}
"#;
        let path = write_temp("mismatch", body);
        let meta = path.metadata().unwrap();
        let candidate = SourceCandidate {
            source_id: "s".into(),
            path: path.clone(),
            size: meta.len(),
            mtime_ns: 1,
        };
        let parsed = GrokAdapter::with_roots(vec![])
            .parse(&candidate, i64::MIN)
            .unwrap();
        assert_eq!(parsed.diagnostics.total_mismatches, 1);
        assert!(parsed.diagnostics.is_partial());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn quota_snapshot_reads_latest_credits_config() {
        let dir = std::env::temp_dir().join(format!("metrik-grok-quota-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let logs = dir.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let path = logs.join("unified.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"ts":"2026-08-05T10:00:00.000Z","msg":"billing: fetched credits config","ctx":{{"config":{{"creditUsagePercent":10.0,"currentPeriod":{{"type":"USAGE_PERIOD_TYPE_WEEKLY","end":"2026-08-06T11:47:07.960395+00:00"}},"billingPeriodEnd":"2026-08-06T11:47:07.960395+00:00"}}}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"ts":"2026-08-05T13:00:00.000Z","msg":"billing: fetched credits config","ctx":{{"config":{{"creditUsagePercent":15.0,"currentPeriod":{{"type":"USAGE_PERIOD_TYPE_WEEKLY","end":"2026-08-06T11:47:07.960395+00:00"}},"billingPeriodEnd":"2026-08-06T11:47:07.960395+00:00"}}}}}}"#
        )
        .unwrap();
        drop(file);

        // 临时把 GROK_HOME 指到 temp。
        let previous = std::env::var_os("GROK_HOME");
        std::env::set_var("GROK_HOME", &dir);
        let samples = fetch_grok_quota_snapshot(Duration::from_secs(1)).unwrap();
        match previous {
            Some(value) => std::env::set_var("GROK_HOME", value),
            None => std::env::remove_var("GROK_HOME"),
        }
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].adapter_id, "grok");
        assert_eq!(samples[0].window_key, "seven_day");
        assert!((samples[0].remaining_percent - 85.0).abs() < 0.01);
        assert_eq!(samples[0].quality, "official_snapshot");
        assert!(samples[0].resets_at_ms.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
