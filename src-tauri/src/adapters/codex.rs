use super::{
    discover_jsonl, timestamp_str_ms, AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate,
};
use crate::domain::{ParsedSource, QuotaSample, TokenVector, UsageEvent};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Take};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

pub struct CodexAdapter {
    roots: Vec<PathBuf>,
}

static CHECKPOINT: Mutex<Option<Checkpoint>> = Mutex::new(None);

struct Checkpoint {
    candidate: SourceCandidate,
    cutoff_ms: i64,
    reader: BufReader<Take<File>>,
    session_id: String,
    current_model: Option<String>,
    current_cwd: Option<String>,
    previous: Option<TokenVector>,
    in_fork_replay: bool,
    pending_events: Vec<PendingEvent>,
    quotas: Vec<QuotaSample>,
    diagnostics: ScanDiagnostics,
}

#[derive(Deserialize, Default)]
struct CodexRecord {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    record_type: Option<String>,
    payload: Option<CodexPayload>,
}

#[derive(Deserialize, Default)]
struct CodexPayload {
    #[serde(rename = "type")]
    payload_type: Option<String>,
    id: Option<String>,
    forked_from_id: Option<String>,
    model: Option<String>,
    cwd: Option<String>,
    info: Option<TokenInfo>,
    rate_limits: Option<RateLimits>,
}

/// 一条待落账的增量：同一轮里模型和工作目录都可能变，所以随事件一起暂存。
struct PendingEvent {
    fingerprint: String,
    timestamp: i64,
    model: Option<String>,
    tokens: TokenVector,
    cwd: Option<String>,
    request_input_tokens: Option<i64>,
}

#[derive(Deserialize, Default)]
struct TokenInfo {
    total_token_usage: Option<RawTokenUsage>,
    last_token_usage: Option<RawTokenUsage>,
    #[serde(default)]
    model_context_window: Option<i64>,
}

#[derive(Deserialize, Default)]
struct RawTokenUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    cached_input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    reasoning_output_tokens: i64,
    /// Codex 自报的总量，用作口径自检的判据（见
    /// `TokenVector::disagrees_with_reported_total`）。本机 60011 条读数
    /// 恒等于 input + output——reasoning 与缓存读都已含在里面，不另加。
    #[serde(default)]
    total_tokens: i64,
}

impl RawTokenUsage {
    /// `codex exec` 一轮没拿到用量时，Codex 0.147 会写一条四个分量全 0、
    /// `total_tokens` 却等于 `model_context_window` 的读数（本机 2026-08-13
    /// 两个会话实拍：0/0/0/0/258400，上下文窗口正是 258400）。它不是用量，
    /// 是占位：没有任何分量可计，也不能当成口径不一致把整个来源标成
    /// 「不完整」——重新扫描只会再读到同一行，用户永远修不掉。
    ///
    /// 只认这一种签名。分量全 0 但 total 不等于窗口时仍按不一致报警：那
    /// 可能是字段改名让我们全读成了 0，正是自检要抓的情况。
    fn is_context_window_placeholder(&self, model_context_window: Option<i64>) -> bool {
        self.input_tokens == 0
            && self.cached_input_tokens == 0
            && self.output_tokens == 0
            && self.reasoning_output_tokens == 0
            && self.total_tokens > 0
            && model_context_window == Some(self.total_tokens)
    }
}

#[derive(Deserialize, Default)]
struct RateLimits {
    primary: Option<RateWindow>,
    secondary: Option<RateWindow>,
}

#[derive(Deserialize, Default)]
struct RateWindow {
    used_percent: Option<f64>,
    resets_at: Option<i64>,
    window_minutes: Option<i64>,
}

impl CodexAdapter {
    pub fn detected() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            roots: vec![
                home.join(".codex").join("sessions"),
                home.join(".codex").join("archived_sessions"),
            ],
        }
    }

    #[cfg(test)]
    fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }
}

impl AgentAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate> {
        discover_jsonl(&self.roots, self.id(), cutoff_ms)
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        self.parse_slice(candidate, cutoff_ms, None)
            .map(|scan| scan.expect("unbounded scan completes"))
    }

    fn has_pending(&self, candidate: &SourceCandidate) -> bool {
        CHECKPOINT.lock().ok().is_some_and(|state| {
            state
                .as_ref()
                .is_some_and(|state| state.candidate.source_id == candidate.source_id)
        })
    }

    fn parse_until(
        &self,
        candidate: &SourceCandidate,
        cutoff_ms: i64,
        deadline: Instant,
    ) -> Result<Option<ParsedScan>> {
        self.parse_slice(candidate, cutoff_ms, Some(deadline))
    }
}

impl CodexAdapter {
    fn parse_slice(
        &self,
        candidate: &SourceCandidate,
        cutoff_ms: i64,
        deadline: Option<Instant>,
    ) -> Result<Option<ParsedScan>> {
        let local = Mutex::new(None);
        let checkpoint_store = if deadline.is_some() {
            &CHECKPOINT
        } else {
            &local
        };
        let mut checkpoint = checkpoint_store
            .lock()
            .map_err(|_| anyhow::anyhow!("Codex scan lock poisoned"))?;
        // Keep one interrupted source in memory. Growing append-only rollouts
        // finish their captured prefix before a later refresh reads new bytes.
        let resumed = checkpoint.take().filter(|state| {
            state.candidate.source_id == candidate.source_id
                && state.cutoff_ms <= cutoff_ms
                && (candidate.size > state.candidate.size
                    || (candidate.size == state.candidate.size
                        && candidate.mtime_ns == state.candidate.mtime_ns))
        });
        let Checkpoint {
            candidate,
            cutoff_ms,
            mut reader,
            mut session_id,
            mut current_model,
            mut current_cwd,
            mut previous,
            mut in_fork_replay,
            mut pending_events,
            mut quotas,
            mut diagnostics,
        } = match resumed {
            Some(state) => state,
            None => {
                let file = File::open(&candidate.path)
                    .with_context(|| format!("failed to open {}", candidate.path.display()))?;
                Checkpoint {
                    candidate: candidate.clone(),
                    cutoff_ms,
                    reader: BufReader::with_capacity(256 * 1024, file.take(candidate.size)),
                    session_id: candidate
                        .path
                        .file_stem()
                        .and_then(|v| v.to_str())
                        .unwrap_or("unknown-session")
                        .to_owned(),
                    current_model: None,
                    current_cwd: None,
                    previous: None,
                    in_fork_replay: false,
                    pending_events: Vec::new(),
                    quotas: Vec::new(),
                    diagnostics: ScanDiagnostics::default(),
                }
            }
        };
        let track_skipped_lines = candidate.mtime_ns / 1_000_000 >= cutoff_ms;
        loop {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                *checkpoint = Some(Checkpoint {
                    candidate,
                    cutoff_ms,
                    reader,
                    session_id,
                    current_model,
                    current_cwd,
                    previous,
                    in_fork_replay,
                    pending_events,
                    quotas,
                    diagnostics,
                });
                return Ok(None);
            }
            let Some(line) = reader.by_ref().lines().next() else {
                break;
            };
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
            let Ok(record) = serde_json::from_str::<CodexRecord>(&line) else {
                // Active JSONL files can end in a partial line. The next full scan will ingest it.
                if track_skipped_lines {
                    diagnostics.malformed_lines += 1;
                }
                continue;
            };
            let record_type = record.record_type.as_deref().unwrap_or_default();
            let payload = record.payload.unwrap_or_default();

            match record_type {
                "session_meta" => {
                    if let Some(id) = payload.id {
                        session_id = id;
                    }
                    if non_empty(payload.forked_from_id).is_some() {
                        in_fork_replay = true;
                    }
                    if let Some(model) = non_empty(payload.model) {
                        current_model = Some(model);
                    }
                    if let Some(cwd) = non_empty(payload.cwd) {
                        current_cwd = Some(cwd);
                    }
                }
                "turn_context" => {
                    in_fork_replay = false;
                    if let Some(model) = non_empty(payload.model) {
                        current_model = Some(model);
                    }
                    if let Some(cwd) = non_empty(payload.cwd) {
                        current_cwd = Some(cwd);
                    }
                }
                "event_msg" if payload.payload_type.as_deref() == Some("token_count") => {
                    // An event that carries its own model wins over the running
                    // turn_context; otherwise fall back to the tracked context so
                    // events before the first turn_context stay honestly unknown.
                    let event_model = non_empty(payload.model);
                    let occurred_at_ms = timestamp_str_ms(record.timestamp.as_deref());
                    let info = payload.info.unwrap_or_default();
                    let context_window = info.model_context_window;
                    let usage = info
                        .total_token_usage
                        .filter(|total| !total.is_context_window_placeholder(context_window));
                    if let (Some(timestamp), Some(total)) = (occurred_at_ms, usage) {
                        let input = total.input_tokens.max(0);
                        let cached = total.cached_input_tokens.max(0).min(input);
                        let current = TokenVector {
                            input_uncached: input - cached,
                            cache_read: cached,
                            cache_write: 0,
                            output: total.output_tokens.max(0),
                            reasoning_output: total.reasoning_output_tokens.max(0),
                        };
                        // 口径自检对累计快照做，不对增量做：来源报的也是累计值。
                        if current.disagrees_with_reported_total(total.total_tokens) {
                            diagnostics.total_mismatches += 1;
                        }
                        let delta = current.positive_delta(previous.as_ref());
                        // A missed group of requests cannot select one pricing tier.
                        let request_input_tokens =
                            info.last_token_usage.as_ref().and_then(|last| {
                                (last.input_tokens == delta.input_uncached + delta.cache_read
                                    && last.cached_input_tokens == delta.cache_read
                                    && last.output_tokens == delta.output
                                    && last.input_tokens >= 0)
                                    .then_some(last.input_tokens)
                            });
                        // Replayed counters still advance the baseline so the first
                        // live delta only counts the fork's own increment.
                        previous = Some(current.clone());
                        if !in_fork_replay && delta.processed() > 0 && timestamp >= cutoff_ms {
                            let fingerprint = format!(
                                "{timestamp}:{}:{}:{}:{}:{}",
                                current.input_uncached,
                                current.cache_read,
                                current.cache_write,
                                current.output,
                                current.reasoning_output
                            );
                            let model = event_model.or_else(|| current_model.clone());
                            pending_events.push(PendingEvent {
                                fingerprint,
                                timestamp,
                                model,
                                tokens: delta,
                                cwd: current_cwd.clone(),
                                request_input_tokens,
                            });
                        }
                    }

                    if let (Some(timestamp), Some(rate_limits)) =
                        (occurred_at_ms, payload.rate_limits)
                    {
                        // Replayed rate limits are the parent's stale snapshots.
                        if !in_fork_replay && timestamp >= cutoff_ms {
                            quotas.extend(parse_quota_windows(
                                rate_limits,
                                timestamp,
                                "Codex 日志配额快照",
                            ));
                        }
                    }
                }
                _ => {}
            }
        }

        let events = pending_events
            .into_iter()
            .map(|pending| {
                let event_key = format!("{session_id}:{}", pending.fingerprint);
                let mut event = UsageEvent::new(
                    self.id(),
                    event_key,
                    pending.timestamp,
                    session_id.clone(),
                    pending.model,
                    pending.tokens,
                    "cumulative_delta",
                )
                .with_project(pending.cwd);
                event.request_input_tokens = pending.request_input_tokens;
                event
            })
            .collect();

        Ok(Some(ParsedScan {
            source: ParsedSource {
                source_id: candidate.source_id.clone(),
                adapter_id: self.id(),
                locator: candidate.path.clone(),
                logical_key: session_id,
                size: candidate.size,
                mtime_ns: candidate.mtime_ns,
                events,
                quotas,
            },
            diagnostics,
        }))
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|model| !model.is_empty())
}

fn parse_quota_windows(rate_limits: RateLimits, timestamp: i64, source: &str) -> Vec<QuotaSample> {
    [
        ("primary", rate_limits.primary),
        ("secondary", rate_limits.secondary),
    ]
    .into_iter()
    .filter_map(|(slot, window)| {
        let window = window?;
        let used = window.used_percent?;
        Some(QuotaSample {
            adapter_id: "codex",
            // 槽位不等于窗口语义，按时长归类（见 domain::codex_window_key）。
            window_key: crate::domain::codex_window_key(window.window_minutes, slot),
            remaining_percent: (100.0 - used).clamp(0.0, 100.0),
            resets_at_ms: window.resets_at.map(|value| value * 1000),
            collected_at_ms: timestamp,
            source_label: source.to_owned(),
            quality: "official_snapshot",
        })
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn bounded_scan_resumes_and_pricing_requires_matching_request_evidence() {
        let path = std::env::temp_dir().join(format!("metrik-resume-{}.jsonl", std::process::id()));
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"type":"session_meta","payload":{{"id":"resume"}}}}"#
        )
        .unwrap();
        for index in 1..=10_000 {
            writeln!(file, "").unwrap();
            let last = if index == 2 { 1 } else { 30 };
            writeln!(file, r#"{{"timestamp":"2026-09-05T00:00:00Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{},"total_tokens":{}}},"last_token_usage":{{"input_tokens":{last}}}}}}}}}"#, index * 30, index * 30).unwrap();
        }
        drop(file);
        let candidate = SourceCandidate {
            source_id: path.display().to_string(),
            path: path.clone(),
            size: path.metadata().unwrap().len(),
            mtime_ns: 1,
        };
        let adapter = CodexAdapter::with_roots(vec![]);
        let full = adapter.parse(&candidate, 0).unwrap();
        assert_eq!(full.source.events[0].request_input_tokens, Some(30));
        assert_eq!(full.source.events[1].request_input_tokens, None);
        assert!(adapter
            .parse_until(&candidate, 0, Instant::now())
            .unwrap()
            .is_none());
        assert!(adapter.has_pending(&candidate));
        let mut slices = 0;
        let resumed = loop {
            slices += 1;
            assert!(slices < 2000, "scan must make progress");
            if let Some(scan) = adapter
                .parse_until(
                    &candidate,
                    1,
                    Instant::now() + std::time::Duration::from_millis(1),
                )
                .unwrap()
            {
                break scan;
            }
        };
        assert!(slices > 1);
        assert_eq!(resumed.source.events.len(), full.source.events.len());
        for (a, b) in resumed.source.events.iter().zip(&full.source.events) {
            assert_eq!(a.event_id, b.event_id);
            assert_eq!(a.tokens, b.tokens);
        }
        assert!(!adapter.has_pending(&candidate));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cumulative_snapshots_become_positive_deltas_without_double_counting() {
        let temp = std::env::temp_dir().join(format!("metrik-codex-{}.jsonl", std::process::id()));
        let mut file = File::create(&temp).unwrap();
        writeln!(
            file,
            r#"{{"type":"session_meta","payload":{{"id":"session-a"}}}}"#
        )
        .unwrap();
        for (index, total) in [100, 140, 140, 190].iter().enumerate() {
            writeln!(
                file,
                r#"{{"timestamp":"2026-07-12T0{index}:00:00Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{total},"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0}}}}}}}}"#
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
        let parsed = CodexAdapter::with_roots(vec![])
            .parse(&candidate, i64::MIN)
            .unwrap();
        let deltas: Vec<i64> = parsed
            .source
            .events
            .iter()
            .map(|event| event.tokens.processed())
            .collect();
        assert_eq!(deltas, vec![100, 40, 50]);
        assert_eq!(deltas.iter().sum::<i64>(), 190);
        std::fs::remove_file(temp).ok();
    }

    /// 口径自检：来源自报总量与我们拆出的分量一致时不该报警，不一致时必须
    /// 记下来并把该来源标为数据不完整。不一致意味着我们对字段语义的理解错了
    /// （reasoning 是否含在 output、缓存是否含在输入……），这类错不会崩溃、
    /// 只会显示一个看着合理的错数字。
    #[test]
    fn a_reported_total_that_contradicts_our_components_is_flagged() {
        let write_log = |name: &str, line: &str| {
            let temp = std::env::temp_dir()
                .join(format!("metrik-codex-{name}-{}.jsonl", std::process::id()));
            let mut file = File::create(&temp).unwrap();
            writeln!(
                file,
                r#"{{"type":"session_meta","payload":{{"id":"session-a"}}}}"#
            )
            .unwrap();
            writeln!(file, "{line}").unwrap();
            drop(file);
            let metadata = temp.metadata().unwrap();
            let candidate = SourceCandidate {
                source_id: "source".into(),
                path: temp.clone(),
                size: metadata.len(),
                mtime_ns: 1,
            };
            let parsed = CodexAdapter::with_roots(vec![])
                .parse(&candidate, i64::MIN)
                .unwrap();
            std::fs::remove_file(temp).ok();
            parsed
        };

        // 真机口径：total = input + output，reasoning 与缓存读都已含在里面。
        let agreeing = write_log(
            "agree",
            r#"{"timestamp":"2026-07-12T01:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":30,"output_tokens":20,"reasoning_output_tokens":8,"total_tokens":120}}}}"#,
        );
        assert_eq!(agreeing.diagnostics.total_mismatches, 0);
        assert!(!agreeing.diagnostics.is_partial());
        assert_eq!(agreeing.source.events[0].tokens.processed(), 120);

        // 假设来源某天改成"reasoning 另计"：total 变 128 而我们仍按 120 拆。
        // 数字看着依旧合理，只有和自报总量一比才露馅。
        let disagreeing = write_log(
            "disagree",
            r#"{"timestamp":"2026-07-12T01:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":30,"output_tokens":20,"reasoning_output_tokens":8,"total_tokens":128}}}}"#,
        );
        assert_eq!(disagreeing.diagnostics.total_mismatches, 1);
        assert!(disagreeing.diagnostics.is_partial());
    }

    #[test]
    fn context_window_placeholder_is_neither_usage_nor_a_mismatch() {
        let parse = |name: &str, body: &str| {
            let temp = std::env::temp_dir().join(format!(
                "metrik-codex-placeholder-{name}-{}.jsonl",
                std::process::id()
            ));
            std::fs::write(&temp, body).unwrap();
            let metadata = temp.metadata().unwrap();
            let candidate = SourceCandidate {
                source_id: "source".into(),
                path: temp.clone(),
                size: metadata.len(),
                mtime_ns: 1,
            };
            let parsed = CodexAdapter::with_roots(vec![])
                .parse(&candidate, i64::MIN)
                .unwrap();
            std::fs::remove_file(temp).ok();
            parsed
        };

        // 本机实拍（codex exec，Codex 0.147）：分量全 0、total 等于上下文窗口。
        // 后面跟一条正常读数，确认占位不影响真实用量的入账，也不改基线。
        let placeholder = parse(
            "skip",
            concat!(
                r#"{"timestamp":"2026-08-13T14:32:27.986Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":0,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":258400},"last_token_usage":{"input_tokens":0,"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":258400},"model_context_window":258400},"rate_limits":{"primary":{"used_percent":12.0}}}}"#,
                "\n",
                r#"{"timestamp":"2026-08-13T14:33:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":30,"output_tokens":20,"reasoning_output_tokens":8,"total_tokens":120},"model_context_window":258400}}}"#,
                "\n",
            ),
        );
        assert_eq!(placeholder.diagnostics.total_mismatches, 0);
        assert!(!placeholder.diagnostics.is_partial());
        assert_eq!(placeholder.source.events.len(), 1);
        assert_eq!(placeholder.source.events[0].tokens.processed(), 120);
        // 占位行上的配额快照仍然要收：那是官方额度，与用量无关。
        assert_eq!(placeholder.source.quotas.len(), 1);

        // 分量全 0 但 total 不等于窗口：不是已知占位，仍按口径不一致报警——
        // 这可能是字段改名让我们全读成了 0。
        let renamed = parse(
            "warn",
            r#"{"timestamp":"2026-08-13T14:32:27.986Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":0,"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":5000},"model_context_window":258400}}}"#,
        );
        assert_eq!(renamed.diagnostics.total_mismatches, 1);
        assert!(renamed.diagnostics.is_partial());
    }

    #[test]
    fn events_carry_the_working_directory_in_effect_when_they_happened() {
        let temp =
            std::env::temp_dir().join(format!("metrik-codex-cwd-{}.jsonl", std::process::id()));
        let mut file = File::create(&temp).unwrap();
        writeln!(
            file,
            r#"{{"type":"session_meta","payload":{{"id":"session-a","cwd":"D:\\work\\usage"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"timestamp":"2026-07-12T01:00:00Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":100,"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0}}}}}}}}"#
        )
        .unwrap();
        // 中途换目录：之后的事件跟着 turn_context 走。
        writeln!(
            file,
            r#"{{"timestamp":"2026-07-12T02:00:00Z","type":"turn_context","payload":{{"cwd":"E:/other"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"timestamp":"2026-07-12T03:00:00Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":180,"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0}}}}}}}}"#
        )
        .unwrap();
        drop(file);
        let metadata = temp.metadata().unwrap();
        let candidate = SourceCandidate {
            source_id: "source".into(),
            path: temp.clone(),
            size: metadata.len(),
            mtime_ns: 1,
        };

        let parsed = CodexAdapter::with_roots(vec![])
            .parse(&candidate, i64::MIN)
            .unwrap();

        let projects: Vec<Option<&str>> = parsed
            .source
            .events
            .iter()
            .map(|event| event.project_path.as_deref())
            .collect();
        assert_eq!(projects, vec![Some("D:/work/usage"), Some("E:/other")]);
        std::fs::remove_file(temp).ok();
    }

    #[test]
    fn malformed_and_unreadable_lines_downgrade_scan_without_losing_valid_events() {
        let temp = std::env::temp_dir().join(format!(
            "metrik-codex-diagnostics-{}.jsonl",
            std::process::id()
        ));
        let valid = br#"{"type":"session_meta","payload":{"id":"session-a"}}
{"timestamp":"2026-07-12T01:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0}}}}
"#;
        let mut file = File::create(&temp).unwrap();
        file.write_all(valid).unwrap();
        file.write_all(b"not-json\n").unwrap();
        file.write_all(&[0xff, b'\n']).unwrap();
        drop(file);

        let metadata = temp.metadata().unwrap();
        let candidate = SourceCandidate {
            source_id: "source".into(),
            path: temp.clone(),
            size: metadata.len(),
            mtime_ns: 1,
        };
        let scan = CodexAdapter::with_roots(vec![])
            .parse(&candidate, i64::MIN)
            .unwrap();

        assert_eq!(scan.source.events.len(), 1);
        assert_eq!(scan.source.events[0].tokens.processed(), 100);
        assert_eq!(scan.diagnostics.malformed_lines, 1);
        assert_eq!(scan.diagnostics.unreadable_lines, 1);
        assert!(scan.diagnostics.is_partial());
        std::fs::remove_file(temp).ok();
    }

    #[test]
    fn fork_replay_token_counts_are_skipped_until_first_turn_context() {
        let temp = std::env::temp_dir().join(format!(
            "metrik-codex-fork-replay-{}.jsonl",
            std::process::id()
        ));
        let mut file = File::create(&temp).unwrap();
        // Fork file: session_meta carries forked_from_id, then the parent's
        // history is replayed (cumulative counters already ledgered under the
        // parent session) before the first live turn_context.
        writeln!(
            file,
            r#"{{"type":"session_meta","payload":{{"id":"fork-a","forked_from_id":"parent-a"}}}}"#
        )
        .unwrap();
        for (index, total) in [100_000, 250_000, 398_000].iter().enumerate() {
            writeln!(
                file,
                r#"{{"timestamp":"2026-07-12T00:00:0{index}Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{total},"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0}}}},"rate_limits":{{"primary":{{"used_percent":50.0}}}}}}}}"#
            )
            .unwrap();
        }
        writeln!(
            file,
            r#"{{"type":"turn_context","payload":{{"model":"gpt-5.6-sol"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"timestamp":"2026-07-12T01:00:00Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":410000,"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0}}}}}}}}"#
        )
        .unwrap();
        drop(file);

        let metadata = temp.metadata().unwrap();
        let candidate = SourceCandidate {
            source_id: "source".into(),
            path: temp.clone(),
            size: metadata.len(),
            mtime_ns: 1,
        };
        let parsed = CodexAdapter::with_roots(vec![])
            .parse(&candidate, i64::MIN)
            .unwrap();

        // Only the live increment past the replayed baseline is counted,
        // attributed to the live turn's model; replayed quotas are dropped.
        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].tokens.processed(), 12_000);
        assert_eq!(
            parsed.source.events[0].model.as_deref(),
            Some("gpt-5.6-sol")
        );
        assert!(parsed.source.quotas.is_empty());
        std::fs::remove_file(temp).ok();
    }

    #[test]
    fn token_count_model_tracks_turn_context_and_prefers_its_own_model() {
        let temp = std::env::temp_dir().join(format!(
            "metrik-codex-model-context-{}.jsonl",
            std::process::id()
        ));
        let mut file = File::create(&temp).unwrap();
        // Before any turn_context: model stays honestly unknown.
        writeln!(
            file,
            r#"{{"type":"session_meta","payload":{{"id":"session-a"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"timestamp":"2026-07-12T00:00:00Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":100,"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0}}}}}}}}"#
        )
        .unwrap();
        // turn_context sets the running model context.
        writeln!(
            file,
            r#"{{"type":"turn_context","payload":{{"model":"gpt-5.5"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"timestamp":"2026-07-12T01:00:00Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":140,"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0}}}}}}}}"#
        )
        .unwrap();
        // Event carrying its own model wins over the tracked context.
        writeln!(
            file,
            r#"{{"timestamp":"2026-07-12T02:00:00Z","type":"event_msg","payload":{{"type":"token_count","model":"gpt-5.5-override","info":{{"total_token_usage":{{"input_tokens":190,"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0}}}}}}}}"#
        )
        .unwrap();
        // A second turn_context switches the model for subsequent events.
        writeln!(
            file,
            r#"{{"type":"turn_context","payload":{{"model":"gpt-6"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"timestamp":"2026-07-12T03:00:00Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":240,"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0}}}}}}}}"#
        )
        .unwrap();
        drop(file);

        let metadata = temp.metadata().unwrap();
        let candidate = SourceCandidate {
            source_id: "source".into(),
            path: temp.clone(),
            size: metadata.len(),
            mtime_ns: 1,
        };
        let parsed = CodexAdapter::with_roots(vec![])
            .parse(&candidate, i64::MIN)
            .unwrap();

        let models: Vec<Option<String>> = parsed
            .source
            .events
            .iter()
            .map(|event| event.model.clone())
            .collect();
        assert_eq!(
            models,
            vec![
                None,
                Some("gpt-5.5".to_string()),
                Some("gpt-5.5-override".to_string()),
                Some("gpt-6".to_string()),
            ]
        );
        std::fs::remove_file(temp).ok();
    }
}
