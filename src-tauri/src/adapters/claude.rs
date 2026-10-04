use super::{
    discover_jsonl, timestamp_str_ms, AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate,
};
use crate::domain::{ParsedSource, TokenVector, UsageEvent};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;

pub struct ClaudeAdapter {
    roots: Vec<PathBuf>,
    /// 达到这个大小的会话文件才保留续读状态，见 TAILS。
    tail_min_bytes: u64,
}

/// 大会话文件（本机实测单文件可达 200MB，release 构建整份重扫约 360ms）一直在
/// 追加写入，每次刷新都从头解析不划算。解析到文件末尾时把累积状态留在内存里，
/// 下次文件变长就只读新增的字节。
///
/// 只认「纯追加」：续读前核对上次读到位置之前的一段字节，文件被截断或改写就
/// 丢弃状态从头解析。逐行累积本身是幂等的（同一条消息按分量取最大值），所以
/// 末尾未写完的半行可以先试读、不推进位置，下次补全后再读一遍也不会重复计数。
///
/// 状态只在进程内：重启后第一次照常整份解析。上限 TAIL_CAPACITY 个文件，
/// 按最近使用淘汰；单文件状态与其消息数成正比（几千条消息约几百 KB）。
static TAILS: Mutex<Vec<(String, TailState)>> = Mutex::new(Vec::new());
const TAIL_CAPACITY: usize = 8;
const TAIL_MIN_BYTES: u64 = 8 * 1024 * 1024;
const ANCHOR_BYTES: u64 = 64;

#[derive(Clone)]
struct MessageUsage {
    timestamp: i64,
    /// 这条消息最早一条记录的时间。解析视界每次刷新都会前移，续读时要据此
    /// 判断这条消息是否整体留在视界内（见 `TailState::reusable_for`）。
    first_timestamp: i64,
    session_id: String,
    event_key: String,
    request_id: Option<String>,
    model: Option<String>,
    tokens: TokenVector,
    cwd: Option<String>,
}

/// 一个文件已解析部分的累积结果，外加续读位置。
struct TailState {
    cutoff_ms: i64,
    /// 已消费到的最后一个完整行的行尾。
    offset: u64,
    /// `offset` 之前最多 ANCHOR_BYTES 字节，用来认出文件是否只是追加。
    anchor: Vec<u8>,
    next_line_index: usize,
    messages: HashMap<String, MessageUsage>,
    /// 元数据自相矛盾而整条丢弃的消息，值是其记录的 (最早, 最晚) 时间。
    rejected: HashMap<String, (i64, i64)>,
    unreadable_lines: usize,
    malformed_lines: usize,
}

impl TailState {
    fn new(cutoff_ms: i64) -> Self {
        Self {
            cutoff_ms,
            offset: 0,
            anchor: Vec::new(),
            next_line_index: 0,
            messages: HashMap::new(),
            rejected: HashMap::new(),
            unreadable_lines: 0,
            malformed_lines: 0,
        }
    }

    /// 视界前移后，续读结果要与「用新视界从头解析」一致：完全落在新视界之前
    /// 的消息直接剔除；跨在视界上的消息（部分记录在视界前）从头解析才算得准，
    /// 这时放弃续读。实际中一条消息的记录相隔不过几秒，几乎碰不到。
    fn reusable_for(&mut self, cutoff_ms: i64) -> bool {
        if cutoff_ms < self.cutoff_ms {
            return false;
        }
        let straddles = |first: i64, last: i64| first < cutoff_ms && last >= cutoff_ms;
        if self
            .messages
            .values()
            .any(|message| straddles(message.first_timestamp, message.timestamp))
            || self
                .rejected
                .values()
                .any(|&(first, last)| straddles(first, last))
        {
            return false;
        }
        self.messages
            .retain(|_, message| message.first_timestamp >= cutoff_ms);
        self.rejected
            .retain(|_, &mut (first, _)| first >= cutoff_ms);
        self.cutoff_ms = cutoff_ms;
        true
    }

    fn ingest(&mut self, line_index: usize, record: ClaudeRecord, fallback_session: &str) {
        if record.record_type.as_deref() != Some("assistant") {
            return;
        }
        let Some(timestamp) = timestamp_str_ms(record.timestamp.as_deref()) else {
            return;
        };
        if timestamp < self.cutoff_ms {
            return;
        }
        let message = record.message.unwrap_or_default();
        let Some(usage) = message.usage else { return };
        let session_id = record
            .session_id
            .unwrap_or_else(|| fallback_session.to_owned());
        let has_provider_message_id = message.id.is_some();
        let message_id = message
            .id
            .unwrap_or_else(|| format!("{session_id}:{timestamp}:{line_index}"));
        // Claude message IDs are provider-generated and stable across copied or
        // branched session logs. Group on that ID, not the enclosing session.
        // Fallback IDs retain the session so malformed records cannot collide.
        let key = if has_provider_message_id {
            format!("message:{message_id}")
        } else {
            format!("fallback:{message_id}")
        };
        let candidate_usage = TokenVector {
            input_uncached: usage.input_tokens.max(0),
            cache_read: usage.cache_read_input_tokens.max(0),
            cache_write: usage.cache_creation_input_tokens.max(0),
            output: usage.output_tokens.max(0),
            reasoning_output: 0,
        };
        let model = message.model;

        if let Some((first, last)) = self.rejected.get_mut(&key) {
            *first = (*first).min(timestamp);
            *last = (*last).max(timestamp);
            return;
        }

        if let Some(stored) = self.messages.get_mut(&key) {
            let request_conflict = if let (Some(stored_request), Some(candidate_request)) =
                (stored.request_id.as_deref(), record.request_id.as_deref())
            {
                stored_request != candidate_request
            } else {
                false
            };
            let model_conflict = if let (Some(stored_model), Some(candidate_model)) =
                (stored.model.as_deref(), model.as_deref())
            {
                stored_model != candidate_model
            } else {
                false
            };
            if request_conflict || model_conflict {
                // A provider ID with contradictory metadata is ambiguous. Drop
                // only that grouped message and retain every other valid event
                // from the source; diagnostics make the partial coverage visible.
                let span = (
                    stored.first_timestamp.min(timestamp),
                    stored.timestamp.max(timestamp),
                );
                self.messages.remove(&key);
                self.rejected.insert(key, span);
                return;
            }

            stored.tokens.component_max(&candidate_usage);
            stored.request_id = stored.request_id.clone().or(record.request_id);
            stored.model = stored.model.clone().or(model);
            stored.cwd = stored.cwd.clone().or(record.cwd);
            stored.first_timestamp = stored.first_timestamp.min(timestamp);
            if timestamp >= stored.timestamp {
                stored.timestamp = timestamp;
            }
        } else {
            self.messages.insert(
                key.clone(),
                MessageUsage {
                    timestamp,
                    first_timestamp: timestamp,
                    session_id,
                    event_key: key,
                    request_id: record.request_id,
                    model,
                    tokens: candidate_usage,
                    cwd: record.cwd,
                },
            );
        }
    }
}

#[derive(Deserialize, Default)]
struct ClaudeRecord {
    #[serde(rename = "type")]
    record_type: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    /// 每条记录都带工作目录；目录名 `~/.claude/projects/<编码 cwd>` 是有损编码
    /// （路径分隔符和字面 `-` 都写成 `-`），只能用这个字段还原真实路径。
    cwd: Option<String>,
    message: Option<ClaudeMessage>,
}

#[derive(Deserialize, Default)]
struct ClaudeMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize, Default)]
struct ClaudeUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    cache_creation_input_tokens: i64,
    #[serde(default)]
    cache_read_input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
}

impl ClaudeAdapter {
    pub fn detected() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            roots: vec![home.join(".claude").join("projects")],
            tail_min_bytes: TAIL_MIN_BYTES,
        }
    }

    #[cfg(test)]
    fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            tail_min_bytes: TAIL_MIN_BYTES,
        }
    }

    /// 取出可续读的状态；没有、视界不兼容或文件不再是上次的前缀时从头开始。
    fn resume(&self, candidate: &SourceCandidate, cutoff_ms: i64, file: &mut File) -> TailState {
        let cached = TAILS.lock().ok().and_then(|mut tails| {
            let index = tails
                .iter()
                .position(|(source_id, _)| *source_id == candidate.source_id)?;
            Some(tails.remove(index).1)
        });
        cached
            .filter(|state| candidate.size >= state.offset && same_prefix(file, state))
            .and_then(|mut state| state.reusable_for(cutoff_ms).then_some(state))
            .unwrap_or_else(|| TailState::new(cutoff_ms))
    }

    fn retain(&self, candidate: &SourceCandidate, state: TailState) {
        if candidate.size < self.tail_min_bytes {
            return;
        }
        if let Ok(mut tails) = TAILS.lock() {
            tails.retain(|(source_id, _)| *source_id != candidate.source_id);
            if tails.len() >= TAIL_CAPACITY {
                tails.remove(0);
            }
            tails.push((candidate.source_id.clone(), state));
        }
    }
}

enum Line {
    Record(Box<ClaudeRecord>),
    Blank,
    Unreadable,
    Malformed,
}

/// 文件在 `state.offset` 之前的最后一段字节是否与上次读到的一致。
fn same_prefix(file: &mut File, state: &TailState) -> bool {
    let start = state.offset - state.anchor.len() as u64;
    let mut current = vec![0; state.anchor.len()];
    file.seek(SeekFrom::Start(start)).is_ok()
        && file.read_exact(&mut current).is_ok()
        && current == state.anchor
}

impl AgentAdapter for ClaudeAdapter {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate> {
        discover_jsonl(&self.roots, self.id(), cutoff_ms)
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        let mut file = File::open(&candidate.path)
            .with_context(|| format!("failed to open {}", candidate.path.display()))?;
        let fallback_session = candidate
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("unknown-session")
            .to_owned();
        let mut state = self.resume(candidate, cutoff_ms, &mut file);

        // 只读到发现时记下的大小：之后追加的字节留给下次刷新，记录的
        // size/mtime 才与已解析的内容对得上。
        file.seek(SeekFrom::Start(state.offset))?;
        let mut reader = BufReader::with_capacity(
            256 * 1024,
            file.take(candidate.size.saturating_sub(state.offset)),
        );
        let mut line = Vec::new();
        let mut trailing_malformed = 0;
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            let complete = line.last() == Some(&b'\n');
            let line_index = state.next_line_index;
            let parsed = match std::str::from_utf8(&line) {
                Err(_) => Line::Unreadable,
                Ok(text) if text.trim().is_empty() => Line::Blank,
                Ok(text) => serde_json::from_str::<ClaudeRecord>(text)
                    .map_or(Line::Malformed, |record| Line::Record(Box::new(record))),
            };
            if complete {
                state.offset += read as u64;
                state.next_line_index += 1;
                match parsed {
                    Line::Record(record) => state.ingest(line_index, *record, &fallback_session),
                    Line::Unreadable => state.unreadable_lines += 1,
                    Line::Malformed => state.malformed_lines += 1,
                    Line::Blank => {}
                }
            } else {
                // 活跃文件末尾可能是写到一半的行：读得通就先计入（幂等），读不通
                // 只计入本次诊断；都不推进位置，下次从行首重读。
                match parsed {
                    Line::Record(record) => state.ingest(line_index, *record, &fallback_session),
                    Line::Unreadable | Line::Malformed => trailing_malformed += 1,
                    Line::Blank => {}
                }
            }
        }
        let anchor_start = state.offset.saturating_sub(ANCHOR_BYTES);
        let mut file = reader.into_inner().into_inner();
        let mut anchor = vec![0; (state.offset - anchor_start) as usize];
        file.seek(SeekFrom::Start(anchor_start))?;
        file.read_exact(&mut anchor)?;
        state.anchor = anchor;

        let mut diagnostics = ScanDiagnostics {
            rejected_events: state.rejected.len(),
            ..Default::default()
        };
        // 文件在 cutoff 之前就没再变过时，跳过的行不计（与 JsonlRecords 一致）。
        if candidate.mtime_ns / 1_000_000 >= cutoff_ms {
            diagnostics.unreadable_lines = state.unreadable_lines;
            diagnostics.malformed_lines = state.malformed_lines + trailing_malformed;
        }

        let mut events: Vec<UsageEvent> = state
            .messages
            .values()
            .filter(|message| message.tokens.processed() > 0)
            .map(|message| {
                UsageEvent::new(
                    self.id(),
                    message.event_key.clone(),
                    message.timestamp,
                    message.session_id.clone(),
                    message.model.clone(),
                    message.tokens.clone(),
                    "exact",
                )
                .with_project(message.cwd.clone())
            })
            .collect();
        events.sort_by_key(|event| event.occurred_at_ms);
        self.retain(candidate, state);

        let logical_key = events
            .first()
            .map(|event| event.session_id.clone())
            .unwrap_or(fallback_session);
        Ok(ParsedScan {
            source: ParsedSource {
                source_id: candidate.source_id.clone(),
                adapter_id: self.id(),
                locator: candidate.path.clone(),
                logical_key,
                size: candidate.size,
                mtime_ns: candidate.mtime_ns,
                events,
                quotas: vec![],
            },
            diagnostics,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn repeated_message_usage_keeps_component_wise_maximum() {
        let temp = std::env::temp_dir().join(format!("metrik-claude-{}.jsonl", std::process::id()));
        let mut file = File::create(&temp).unwrap();
        for input in [100, 100, 140] {
            writeln!(
                file,
                r#"{{"type":"assistant","timestamp":"2026-07-12T01:00:00Z","sessionId":"session-a","message":{{"id":"message-a","model":"claude-sonnet","usage":{{"input_tokens":{input},"cache_creation_input_tokens":10,"cache_read_input_tokens":20,"output_tokens":5}}}}}}"#
            )
            .unwrap();
        }
        drop(file);
        let metadata = temp.metadata().unwrap();
        let candidate = SourceCandidate {
            source_id: "source".into(),
            path: temp.clone(),
            size: metadata.len(),
            mtime_ns: 1,
        };
        let parsed = ClaudeAdapter::with_roots(vec![])
            .parse(&candidate, i64::MIN)
            .unwrap();
        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].tokens.input_uncached, 140);
        assert_eq!(parsed.source.events[0].tokens.processed(), 175);
        assert_eq!(parsed.source.events[0].event_key, "message:message-a");
        std::fs::remove_file(temp).ok();
    }

    #[test]
    fn provider_message_is_deduplicated_across_sessions() {
        let temp = std::env::temp_dir().join(format!(
            "metrik-claude-cross-session-{}.jsonl",
            std::process::id()
        ));
        let mut file = File::create(&temp).unwrap();
        for (session, timestamp, output) in [
            ("session-a", "2026-07-12T01:00:00Z", 5),
            ("session-b", "2026-07-12T01:01:00Z", 9),
        ] {
            let record = serde_json::json!({
                "type": "assistant",
                "timestamp": timestamp,
                "sessionId": session,
                "requestId": "request-a",
                "message": {
                    "id": "message-a",
                    "model": "claude-sonnet",
                    "usage": { "input_tokens": 100, "output_tokens": output }
                }
            });
            writeln!(file, "{record}").unwrap();
        }
        drop(file);

        let metadata = temp.metadata().unwrap();
        let candidate = SourceCandidate {
            source_id: "source".into(),
            path: temp.clone(),
            size: metadata.len(),
            mtime_ns: 1,
        };
        let parsed = ClaudeAdapter::with_roots(vec![])
            .parse(&candidate, i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].event_key, "message:message-a");
        assert_eq!(parsed.source.events[0].tokens.output, 9);
        assert_eq!(parsed.source.events[0].session_id, "session-a");
        assert_eq!(
            parsed.source.events[0].occurred_at_ms,
            timestamp_str_ms(Some("2026-07-12T01:01:00Z")).unwrap()
        );
        std::fs::remove_file(temp).ok();
    }

    #[test]
    fn conflicting_request_ids_reject_only_that_message() {
        let temp = std::env::temp_dir().join(format!(
            "metrik-claude-request-collision-{}.jsonl",
            std::process::id()
        ));
        let mut file = File::create(&temp).unwrap();
        for request in ["request-a", "request-b"] {
            let record = serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-07-12T01:00:00Z",
                "sessionId": "session-a",
                "requestId": request,
                "message": {
                    "id": "message-a",
                    "model": "claude-sonnet",
                    "usage": { "input_tokens": 100, "output_tokens": 5 }
                }
            });
            writeln!(file, "{record}").unwrap();
        }
        let valid = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-07-12T01:01:00Z",
            "sessionId": "session-a",
            "requestId": "request-valid",
            "message": {
                "id": "message-valid",
                "model": "claude-sonnet",
                "usage": { "input_tokens": 40, "output_tokens": 2 }
            }
        });
        writeln!(file, "{valid}").unwrap();
        drop(file);

        let metadata = temp.metadata().unwrap();
        let candidate = SourceCandidate {
            source_id: "source".into(),
            path: temp.clone(),
            size: metadata.len(),
            mtime_ns: 1,
        };
        let parsed = ClaudeAdapter::with_roots(vec![])
            .parse(&candidate, i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].event_key, "message:message-valid");
        assert_eq!(parsed.source.events[0].tokens.processed(), 42);
        assert_eq!(parsed.diagnostics.rejected_events, 1);
        std::fs::remove_file(temp).ok();
    }

    #[test]
    fn the_record_cwd_becomes_the_project_path() {
        let temp = std::env::temp_dir().join(format!(
            "metrik-claude-cwd-{}-{}.jsonl",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let mut file = File::create(&temp).unwrap();
        let with_cwd = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-07-12T01:00:00Z",
            "sessionId": "session-a",
            "cwd": "D:\\work\\usage",
            "message": {
                "id": "message-a",
                "model": "claude-sonnet",
                "usage": { "input_tokens": 100, "output_tokens": 5 }
            }
        });
        let without_cwd = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-07-12T01:01:00Z",
            "sessionId": "session-a",
            "message": {
                "id": "message-b",
                "model": "claude-sonnet",
                "usage": { "input_tokens": 20, "output_tokens": 3 }
            }
        });
        writeln!(file, "{with_cwd}").unwrap();
        writeln!(file, "{without_cwd}").unwrap();
        drop(file);

        let metadata = temp.metadata().unwrap();
        let candidate = SourceCandidate {
            source_id: "source".into(),
            path: temp.clone(),
            size: metadata.len(),
            mtime_ns: 1,
        };
        let parsed = ClaudeAdapter::with_roots(vec![])
            .parse(&candidate, i64::MIN)
            .unwrap();

        let projects: Vec<Option<&str>> = parsed
            .source
            .events
            .iter()
            .map(|event| event.project_path.as_deref())
            .collect();
        assert_eq!(projects, vec![Some("D:/work/usage"), None]);
        std::fs::remove_file(temp).ok();
    }

    #[test]
    fn conflicting_models_reject_only_that_message() {
        let temp = std::env::temp_dir().join(format!(
            "metrik-claude-model-collision-{}.jsonl",
            std::process::id()
        ));
        let mut file = File::create(&temp).unwrap();
        for model in ["claude-sonnet", "claude-opus"] {
            let record = serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-07-12T01:00:00Z",
                "sessionId": "session-a",
                "requestId": "request-a",
                "message": {
                    "id": "message-a",
                    "model": model,
                    "usage": { "input_tokens": 100, "output_tokens": 5 }
                }
            });
            writeln!(file, "{record}").unwrap();
        }
        let valid = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-07-12T01:01:00Z",
            "sessionId": "session-a",
            "message": {
                "id": "message-valid",
                "model": "claude-sonnet",
                "usage": { "input_tokens": 20, "output_tokens": 3 }
            }
        });
        writeln!(file, "{valid}").unwrap();
        drop(file);

        let metadata = temp.metadata().unwrap();
        let candidate = SourceCandidate {
            source_id: "source".into(),
            path: temp.clone(),
            size: metadata.len(),
            mtime_ns: 1,
        };
        let parsed = ClaudeAdapter::with_roots(vec![])
            .parse(&candidate, i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].event_key, "message:message-valid");
        assert_eq!(parsed.diagnostics.rejected_events, 1);
        std::fs::remove_file(temp).ok();
    }

    #[test]
    fn request_id_presence_does_not_change_provider_identity() {
        let mut keys = Vec::new();
        for (suffix, request_id) in [("with", Some("request-a")), ("without", None)] {
            let temp = std::env::temp_dir().join(format!(
                "metrik-claude-request-{suffix}-{}.jsonl",
                std::process::id()
            ));
            let mut file = File::create(&temp).unwrap();
            let mut record = serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-07-12T01:00:00Z",
                "sessionId": "session-a",
                "message": {
                    "id": "message-a",
                    "model": "claude-sonnet",
                    "usage": { "input_tokens": 100, "output_tokens": 5 }
                }
            });
            if let Some(request_id) = request_id {
                record["requestId"] = request_id.into();
            }
            writeln!(file, "{record}").unwrap();
            drop(file);

            let metadata = temp.metadata().unwrap();
            let candidate = SourceCandidate {
                source_id: format!("source-{suffix}"),
                path: temp.clone(),
                size: metadata.len(),
                mtime_ns: 1,
            };
            let parsed = ClaudeAdapter::with_roots(vec![])
                .parse(&candidate, i64::MIN)
                .unwrap();
            keys.push(parsed.source.events[0].event_key.clone());
            std::fs::remove_file(temp).ok();
        }

        assert_eq!(keys, ["message:message-a", "message:message-a"]);
    }

    #[test]
    fn malformed_line_is_reported_while_valid_usage_is_retained() {
        let temp = std::env::temp_dir().join(format!(
            "metrik-claude-diagnostics-{}.jsonl",
            std::process::id()
        ));
        let mut file = File::create(&temp).unwrap();
        writeln!(file, "{{broken-json").unwrap();
        let record = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-07-12T01:00:00Z",
            "sessionId": "session-a",
            "message": {
                "id": "message-a",
                "model": "claude-sonnet",
                "usage": { "input_tokens": 100, "output_tokens": 5 }
            }
        });
        writeln!(file, "{record}").unwrap();
        drop(file);

        let metadata = temp.metadata().unwrap();
        let candidate = SourceCandidate {
            source_id: "source".into(),
            path: temp.clone(),
            size: metadata.len(),
            mtime_ns: 1,
        };
        let scan = ClaudeAdapter::with_roots(vec![])
            .parse(&candidate, i64::MIN)
            .unwrap();

        assert_eq!(scan.source.events.len(), 1);
        assert_eq!(scan.diagnostics.malformed_lines, 1);
        assert_eq!(scan.diagnostics.unreadable_lines, 0);
        assert_eq!(scan.diagnostics.rejected_events, 0);
        std::fs::remove_file(temp).ok();
    }

    fn tail_adapter() -> ClaudeAdapter {
        ClaudeAdapter {
            roots: vec![],
            tail_min_bytes: 0,
        }
    }

    fn candidate_for(source_id: &str, path: &std::path::Path) -> SourceCandidate {
        SourceCandidate {
            source_id: source_id.into(),
            path: path.to_path_buf(),
            size: path.metadata().unwrap().len(),
            mtime_ns: i64::MAX,
        }
    }

    /// 与顺序无关的事件摘要，用来比较续读与整份解析的结果。
    fn summary(scan: &ParsedScan) -> Vec<String> {
        let mut rows: Vec<String> = scan
            .source
            .events
            .iter()
            .map(|event| {
                format!(
                    "{}|{}|{}|{:?}|{}|{}|{}|{}",
                    event.event_key,
                    event.occurred_at_ms,
                    event.session_id,
                    event.model,
                    event.tokens.input_uncached,
                    event.tokens.cache_read,
                    event.tokens.cache_write,
                    event.tokens.output
                )
            })
            .collect();
        rows.sort();
        rows
    }

    fn record(message: &str, second: u32, output: i64, model: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "timestamp": format!("2026-07-12T01:00:{second:02}Z"),
            "sessionId": "session-a",
            "requestId": format!("request-{message}"),
            "message": {
                "id": message,
                "model": model,
                "usage": { "input_tokens": 100, "output_tokens": output }
            }
        })
        .to_string()
    }

    /// 任意位置切开（包括行中间）分段续读，结果必须与一次整份解析完全一致，
    /// 包括跨段的流式更新、元数据冲突导致的整条丢弃和无 ID 的回退键。
    #[test]
    fn appended_bytes_resume_to_the_same_result_as_a_full_parse() {
        let mut lines = Vec::new();
        for index in 0..40u32 {
            let message = format!("m{}", index % 13);
            let model = if index == 31 {
                "claude-other"
            } else {
                "claude-sonnet"
            };
            lines.push(record(&message, index, i64::from(index) + 1, model));
        }
        lines.push(
            r#"{"type":"assistant","timestamp":"2026-07-12T01:01:00Z","message":{"usage":{"input_tokens":7,"output_tokens":3}}}"#
                .into(),
        );
        lines.push("not json".into());
        let content = lines.join("\n") + "\n";
        let path =
            std::env::temp_dir().join(format!("metrik-claude-tail-{}.jsonl", std::process::id()));
        let adapter = tail_adapter();
        let source_id = format!("tail-resume-{}", std::process::id());
        let cuts = [
            0,
            1,
            150,
            151,
            900,
            content.len() / 2,
            content.len() - 5,
            content.len(),
        ];
        let mut last = None;
        for cut in cuts {
            std::fs::write(&path, &content.as_bytes()[..cut]).unwrap();
            last = Some(
                adapter
                    .parse(&candidate_for(&source_id, &path), i64::MIN)
                    .unwrap(),
            );
        }
        let resumed = last.unwrap();
        let expected = ClaudeAdapter::with_roots(vec![])
            .parse(&candidate_for("tail-full", &path), i64::MIN)
            .unwrap();
        assert!(expected.diagnostics.rejected_events > 0);
        assert_eq!(summary(&resumed), summary(&expected));
        assert_eq!(resumed.diagnostics, expected.diagnostics);
        std::fs::remove_file(path).ok();
    }

    /// 文件被改写（不再以上次读到的内容为前缀）时必须从头解析，不能沿用旧状态。
    #[test]
    fn a_rewritten_file_is_parsed_from_the_start() {
        let path = std::env::temp_dir().join(format!(
            "metrik-claude-tail-rewrite-{}.jsonl",
            std::process::id()
        ));
        let adapter = tail_adapter();
        let source_id = format!("tail-rewrite-{}", std::process::id());
        std::fs::write(&path, record("old", 1, 5, "claude-sonnet") + "\n").unwrap();
        adapter
            .parse(&candidate_for(&source_id, &path), i64::MIN)
            .unwrap();

        let rewritten = [
            record("new-a", 2, 7, "claude-sonnet"),
            record("new-b", 3, 9, "claude-sonnet"),
        ]
        .join("\n")
            + "\n";
        std::fs::write(&path, rewritten).unwrap();
        let scan = adapter
            .parse(&candidate_for(&source_id, &path), i64::MIN)
            .unwrap();
        let keys: Vec<_> = scan
            .source
            .events
            .iter()
            .map(|event| event.event_key.as_str())
            .collect();
        assert_eq!(keys, ["message:new-a", "message:new-b"]);
        std::fs::remove_file(path).ok();
    }

    /// 解析视界前移后，续读要剔除落到视界之前的消息，与按新视界整份解析一致。
    #[test]
    fn an_advancing_cutoff_drops_expired_messages_when_resuming() {
        let path = std::env::temp_dir().join(format!(
            "metrik-claude-tail-cutoff-{}.jsonl",
            std::process::id()
        ));
        let adapter = tail_adapter();
        let source_id = format!("tail-cutoff-{}", std::process::id());
        let first = [
            record("early", 1, 5, "claude-sonnet"),
            record("late", 30, 6, "claude-sonnet"),
        ]
        .join("\n")
            + "\n";
        std::fs::write(&path, &first).unwrap();
        adapter
            .parse(&candidate_for(&source_id, &path), i64::MIN)
            .unwrap();

        std::fs::write(
            &path,
            first + &record("later", 40, 7, "claude-sonnet") + "\n",
        )
        .unwrap();
        let cutoff = timestamp_str_ms(Some("2026-07-12T01:00:10Z")).unwrap();
        let resumed = adapter
            .parse(&candidate_for(&source_id, &path), cutoff)
            .unwrap();
        let full = ClaudeAdapter::with_roots(vec![])
            .parse(&candidate_for("tail-cutoff-full", &path), cutoff)
            .unwrap();
        assert_eq!(summary(&resumed), summary(&full));
        assert_eq!(resumed.source.events.len(), 2);
        std::fs::remove_file(path).ok();
    }
}
