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
                }
                "turn_context" => {
                    current_model = payload.model.or(current_model);
                }
                "event_msg" if payload.payload_type.as_deref() == Some("token_count") => {
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
                        previous = Some(current.clone());
                        if delta.processed() > 0 && timestamp >= cutoff_ms {
                            let fingerprint = format!(
                                "{timestamp}:{}:{}:{}:{}:{}",
                                current.input_uncached,
                                current.cache_read,
                                current.cache_write,
                                current.output,
                                current.reasoning_output
                            );
                            pending_events.push((
                                fingerprint,
                                timestamp,
                                current_model.clone(),
                                delta,
                            ));
                        }
                    }

                    if let (Some(timestamp), Some(rate_limits)) =
                        (occurred_at_ms, payload.rate_limits)
                    {
                        if timestamp >= cutoff_ms {
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

fn parse_quota_windows(rate_limits: RateLimits, timestamp: i64, source: &str) -> Vec<QuotaSample> {
    [
        ("primary", rate_limits.primary),
        ("secondary", rate_limits.secondary),
    ]
    .into_iter()
    .filter_map(|(window_key, window)| {
        let window = window?;
        let used = window.used_percent?;
        Some(QuotaSample {
            adapter_id: "codex",
            window_key,
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
}
