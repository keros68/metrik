use super::{discover_jsonl, AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{ParsedSource, TokenVector, UsageEvent};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Kimi CLI / Kimi Code 的 wire 日志（JSONL）。两代格式并存：
///
/// - 新版 `~/.kimi-code/sessions/<workspace>/<session>/agents/<agent>/wire.jsonl`：
///   顶层 `{"type":"usage.record","model":…,"usage":{camelCase},"usageScope":"turn|session","time":<ms>}`。
///   **只计 `usageScope == "turn"`（单轮增量）**；`session` 作用域是会话累计总量，
///   计入会重复计数。
/// - 旧版 `~/.kimi/sessions/<group>/<session>/wire.jsonl`：
///   `{"timestamp":<秒·浮点>,"message":{"type":"StatusUpdate","payload":{"token_usage":{snake_case},"message_id":…}}}`。
///   旧版无 scope 字段且未确认是否对同一 `message_id` 渐进更新，因此按 message_id
///   合并、分量取最大值——真增量时每个 id 只出现一次（取 max 无害），渐进更新时
///   正好避免重复计数。
///
/// 会话 ID 来自目录路径（记录里没有）。旧版 StatusUpdate 不带模型名，
/// 保持 `None`（诚实标注"未标注模型"，不猜测）。
pub struct KimiAdapter {
    roots: Vec<PathBuf>,
}

#[derive(Deserialize, Default)]
struct KimiRecord {
    // 新版
    #[serde(rename = "type")]
    record_type: Option<String>,
    model: Option<String>,
    usage: Option<NewUsage>,
    #[serde(rename = "usageScope")]
    usage_scope: Option<String>,
    time: Option<i64>,
    // 旧版
    timestamp: Option<f64>,
    message: Option<LegacyMessage>,
}

#[derive(Deserialize, Default)]
struct NewUsage {
    #[serde(rename = "inputOther", default)]
    input_other: i64,
    #[serde(rename = "inputCacheRead", default)]
    input_cache_read: i64,
    #[serde(rename = "inputCacheCreation", default)]
    input_cache_creation: i64,
    #[serde(default)]
    output: i64,
}

#[derive(Deserialize, Default)]
struct LegacyMessage {
    #[serde(rename = "type")]
    message_type: Option<String>,
    payload: Option<LegacyPayload>,
}

#[derive(Deserialize, Default)]
struct LegacyPayload {
    token_usage: Option<LegacyUsage>,
    message_id: Option<String>,
}

#[derive(Deserialize, Default)]
struct LegacyUsage {
    #[serde(default)]
    input_other: i64,
    #[serde(default)]
    input_cache_read: i64,
    #[serde(default)]
    input_cache_creation: i64,
    #[serde(default)]
    output: i64,
}

impl KimiAdapter {
    pub fn detected() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        // 新版数据根可被 KIMI_CODE_HOME 覆盖。
        let kimi_code = std::env::var_os("KIMI_CODE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".kimi-code"));
        Self {
            roots: vec![
                kimi_code.join("sessions"),
                home.join(".kimi").join("sessions"),
            ],
        }
    }

    #[cfg(test)]
    fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }
}

/// 会话 ID 取自路径：新版 `sessions/<workspace>/<session>/agents/<agent>/wire.jsonl`
/// 取 `<session>/<agent>`（子 agent 各自成流，合并会丢失粒度）；旧版
/// `sessions/<group>/<session>/wire.jsonl` 取 `<session>`。
fn session_id_from_path(path: &Path) -> String {
    let parts: Vec<&str> = path
        .iter()
        .filter_map(|part| part.to_str())
        .map(|part| part.trim_end_matches('/'))
        .collect();
    let agents_at = parts.iter().rposition(|part| *part == "agents");
    if let Some(index) = agents_at {
        if index >= 1 && index + 1 < parts.len() {
            return format!("{}/{}", parts[index - 1], parts[index + 1]);
        }
    }
    path.parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("unknown-session")
        .to_owned()
}

impl AgentAdapter for KimiAdapter {
    fn id(&self) -> &'static str {
        "kimi"
    }

    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate> {
        discover_jsonl(&self.roots, self.id(), cutoff_ms)
            .into_iter()
            .filter(|candidate| {
                candidate.path.file_name().and_then(|name| name.to_str()) == Some("wire.jsonl")
            })
            .collect()
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        let file = File::open(&candidate.path)
            .with_context(|| format!("failed to open {}", candidate.path.display()))?;
        let reader = BufReader::with_capacity(256 * 1024, file);

        let session_id = session_id_from_path(&candidate.path);
        let mut events: Vec<UsageEvent> = Vec::new();
        // 旧版按 message_id 合并（分量取最大值），值同时记录首见时间。
        let mut legacy: BTreeMap<String, (i64, TokenVector)> = BTreeMap::new();
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
            let Ok(record) = serde_json::from_str::<KimiRecord>(&line) else {
                // 活跃文件末尾可能是半行，下次扫描会重新读取。
                if track_skipped_lines {
                    diagnostics.malformed_lines += 1;
                }
                continue;
            };

            // 新版：只认单轮增量记录。
            if record.record_type.as_deref() == Some("usage.record") {
                // scope 缺失时不猜（可能是未来新增的累计口径），跳过并标记部分覆盖。
                if record.usage_scope.as_deref() != Some("turn") {
                    if track_skipped_lines && record.usage_scope.is_none() {
                        diagnostics.malformed_lines += 1;
                    }
                    continue;
                }
                let (Some(usage), Some(timestamp)) = (record.usage, record.time) else {
                    continue;
                };
                let tokens = TokenVector {
                    input_uncached: usage.input_other.max(0),
                    cache_read: usage.input_cache_read.max(0),
                    cache_write: usage.input_cache_creation.max(0),
                    output: usage.output.max(0),
                    reasoning_output: 0,
                };
                if tokens.processed() == 0 || timestamp < cutoff_ms {
                    continue;
                }
                let fingerprint = format!(
                    "{timestamp}:{}:{}:{}:{}",
                    tokens.input_uncached, tokens.cache_read, tokens.cache_write, tokens.output
                );
                events.push(UsageEvent::new(
                    self.id(),
                    format!("{session_id}:{fingerprint}"),
                    timestamp,
                    session_id.clone(),
                    non_empty(record.model),
                    tokens,
                    "turn_delta",
                ));
                continue;
            }

            // 旧版 StatusUpdate。
            let Some(message) = record.message else {
                continue;
            };
            if message.message_type.as_deref() != Some("StatusUpdate") {
                continue;
            }
            let Some(payload) = message.payload else {
                continue;
            };
            let Some(usage) = payload.token_usage else {
                continue;
            };
            // 旧版时间戳是 Unix 秒（浮点），新版是毫秒——别搞混。
            let Some(timestamp) = record.timestamp.map(|value| (value * 1000.0) as i64) else {
                continue;
            };
            let tokens = TokenVector {
                input_uncached: usage.input_other.max(0),
                cache_read: usage.input_cache_read.max(0),
                cache_write: usage.input_cache_creation.max(0),
                output: usage.output.max(0),
                reasoning_output: 0,
            };
            if tokens.processed() == 0 || timestamp < cutoff_ms {
                continue;
            }
            let key = payload
                .message_id
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| format!("ts:{timestamp}"));
            let entry = legacy
                .entry(key)
                .or_insert((timestamp, TokenVector::default()));
            entry.1 = TokenVector {
                input_uncached: entry.1.input_uncached.max(tokens.input_uncached),
                cache_read: entry.1.cache_read.max(tokens.cache_read),
                cache_write: entry.1.cache_write.max(tokens.cache_write),
                output: entry.1.output.max(tokens.output),
                reasoning_output: 0,
            };
        }

        events.extend(legacy.into_iter().map(|(message_id, (timestamp, tokens))| {
            UsageEvent::new(
                self.id(),
                format!("{session_id}:{message_id}"),
                timestamp,
                session_id.clone(),
                None,
                tokens,
                "message_merge",
            )
        }));

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

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|model| !model.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn candidate_for(path: &Path) -> SourceCandidate {
        let metadata = path.metadata().unwrap();
        SourceCandidate {
            source_id: "source".into(),
            path: path.to_path_buf(),
            size: metadata.len(),
            mtime_ns: 1,
        }
    }

    fn wire_file(label: &str, relative: &[&str], body: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "metrik-kimi-{label}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let mut directory = root.clone();
        for part in relative {
            directory = directory.join(part);
        }
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("wire.jsonl");
        let mut file = File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn new_format_counts_turn_scope_only_and_never_session_totals() {
        // session 作用域是会话累计总量，计入会重复计数。
        let path = wire_file(
            "turn-scope",
            &["sessions", "ws-1", "session-a", "agents", "main"],
            concat!(
                r#"{"type":"usage.record","model":"kimi-code/kimi-for-coding","usage":{"inputOther":3064,"output":76,"inputCacheRead":14848,"inputCacheCreation":0},"usageScope":"turn","time":1782113184943}"#,
                "\n",
                r#"{"type":"usage.record","model":"kimi-code/kimi-for-coding","usage":{"inputOther":120,"output":40,"inputCacheRead":0,"inputCacheCreation":512},"usageScope":"turn","time":1782113200000}"#,
                "\n",
                r#"{"type":"usage.record","model":"kimi-code/kimi-for-coding","usage":{"inputOther":3184,"output":116,"inputCacheRead":14848,"inputCacheCreation":512},"usageScope":"session","time":1782113200001}"#,
                "\n",
            ),
        );

        let parsed = KimiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 2);
        let total: i64 = parsed
            .source
            .events
            .iter()
            .map(|event| event.tokens.processed())
            .sum();
        // 只有两条 turn：(3064+14848+0+76) + (120+0+512+40) = 18660
        assert_eq!(total, 18_660);
        assert_eq!(
            parsed.source.events[0].model.as_deref(),
            Some("kimi-code/kimi-for-coding")
        );
        // 会话 ID 取自路径（含子 agent 粒度）。
        assert_eq!(parsed.source.events[0].session_id, "session-a/main");
        assert_eq!(parsed.source.events[0].tokens.cache_read, 14_848);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn legacy_status_updates_merge_by_message_id_taking_component_maxima() {
        // 同一 message_id 若被渐进更新，取分量最大值即为该消息的最终用量，
        // 不会把中间态叠加成重复计数；真增量时每个 id 只出现一次，取 max 无害。
        let path = wire_file(
            "legacy",
            &["sessions", "group-1", "session-b"],
            concat!(
                r#"{"type":"metadata","protocol_version":"1.3"}"#,
                "\n",
                r#"{"timestamp":1770983426.420942,"message":{"type":"StatusUpdate","payload":{"token_usage":{"input_other":1000,"output":500,"input_cache_read":0,"input_cache_creation":0},"message_id":"chatcmpl-a"}}}"#,
                "\n",
                r#"{"timestamp":1770983427.100000,"message":{"type":"StatusUpdate","payload":{"token_usage":{"input_other":1562,"output":2463,"input_cache_read":0,"input_cache_creation":0},"message_id":"chatcmpl-a"}}}"#,
                "\n",
                r#"{"timestamp":1770983500.000000,"message":{"type":"StatusUpdate","payload":{"token_usage":{"input_other":10,"output":20,"input_cache_read":30,"input_cache_creation":40},"message_id":"chatcmpl-b"}}}"#,
                "\n",
            ),
        );

        let parsed = KimiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 2);
        let total: i64 = parsed
            .source
            .events
            .iter()
            .map(|event| event.tokens.processed())
            .sum();
        // chatcmpl-a 取最大值 1562+2463 = 4025（不是 1500+4025），chatcmpl-b = 100
        assert_eq!(total, 4_125);
        // 旧版 StatusUpdate 不带模型名：诚实留空，不猜。
        assert!(parsed
            .source
            .events
            .iter()
            .all(|event| event.model.is_none()));
        // 时间戳是 Unix 秒（浮点）→ 毫秒。
        assert_eq!(parsed.source.events[0].occurred_at_ms, 1_770_983_426_420);
        assert_eq!(parsed.source.events[0].session_id, "session-b");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn malformed_lines_downgrade_the_scan_without_losing_valid_events() {
        let path = wire_file(
            "diagnostics",
            &["sessions", "ws-1", "session-c", "agents", "main"],
            concat!(
                r#"{"type":"usage.record","usage":{"inputOther":100,"output":10},"usageScope":"turn","time":1782113184943}"#,
                "\n",
                "not-json\n",
            ),
        );

        let parsed = KimiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].tokens.processed(), 110);
        assert_eq!(parsed.diagnostics.malformed_lines, 1);
        assert!(parsed.diagnostics.is_partial());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
