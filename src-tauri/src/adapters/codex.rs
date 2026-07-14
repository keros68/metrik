use super::{
    discover_jsonl, timestamp_str_ms, AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate,
};
use crate::domain::{ParsedSource, QuotaSample, TokenVector, UsageEvent};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

pub struct CodexAdapter {
    roots: Vec<PathBuf>,
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
    info: Option<TokenInfo>,
    rate_limits: Option<RateLimits>,
}

#[derive(Deserialize, Default)]
struct TokenInfo {
    total_token_usage: Option<RawTokenUsage>,
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
        let file = File::open(&candidate.path)
            .with_context(|| format!("failed to open {}", candidate.path.display()))?;
        let reader = BufReader::with_capacity(256 * 1024, file);

        let fallback_session = candidate
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("unknown-session")
            .to_owned();
        let mut session_id = fallback_session;
        let mut current_model: Option<String> = None;
        let mut previous: Option<TokenVector> = None;
        // Codex Desktop fork/subagent files replay the parent thread's history
        // (including its cumulative token_count events) before the first live
        // turn. Those replayed counters are already ledgered under the parent
        // session, so counting them again would double-count. The replay never
        // contains turn_context lines, so the first turn_context marks live data.
        let mut in_fork_replay = false;
        let mut pending_events: Vec<(String, i64, Option<String>, TokenVector)> = Vec::new();
        let mut quotas = Vec::new();
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
                }
                "turn_context" => {
                    in_fork_replay = false;
                    if let Some(model) = non_empty(payload.model) {
                        current_model = Some(model);
                    }
                }
                "event_msg" if payload.payload_type.as_deref() == Some("token_count") => {
                    // An event that carries its own model wins over the running
                    // turn_context; otherwise fall back to the tracked context so
                    // events before the first turn_context stay honestly unknown.
                    let event_model = non_empty(payload.model);
                    let occurred_at_ms = timestamp_str_ms(record.timestamp.as_deref());
                    if let (Some(timestamp), Some(total)) = (
                        occurred_at_ms,
                        payload.info.and_then(|info| info.total_token_usage),
                    ) {
                        let input = total.input_tokens.max(0);
                        let cached = total.cached_input_tokens.max(0).min(input);
                        let current = TokenVector {
                            input_uncached: input - cached,
                            cache_read: cached,
                            cache_write: 0,
                            output: total.output_tokens.max(0),
                            reasoning_output: total.reasoning_output_tokens.max(0),
                        };
                        let delta = current.positive_delta(previous.as_ref());
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
                            pending_events.push((fingerprint, timestamp, model, delta));
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
            .map(|(fingerprint, timestamp, model, tokens)| {
                let event_key = format!("{session_id}:{fingerprint}");
                UsageEvent::new(
                    self.id(),
                    event_key,
                    timestamp,
                    session_id.clone(),
                    model,
                    tokens,
                    "cumulative_delta",
                )
            })
            .collect();

        Ok(ParsedScan {
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
        })
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
