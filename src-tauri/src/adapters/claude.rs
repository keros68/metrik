use super::{
    discover_jsonl, timestamp_str_ms, AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate,
};
use crate::domain::{ParsedSource, TokenVector, UsageEvent};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

pub struct ClaudeAdapter {
    roots: Vec<PathBuf>,
}

#[derive(Clone)]
struct MessageUsage {
    timestamp: i64,
    session_id: String,
    event_key: String,
    request_id: Option<String>,
    model: Option<String>,
    tokens: TokenVector,
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
        }
    }

    #[cfg(test)]
    fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }
}

impl AgentAdapter for ClaudeAdapter {
    fn id(&self) -> &'static str {
        "claude"
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
        let mut messages: HashMap<String, MessageUsage> = HashMap::new();
        let mut rejected_messages = HashSet::new();
        let mut diagnostics = ScanDiagnostics::default();
        let track_skipped_lines = candidate.mtime_ns / 1_000_000 >= cutoff_ms;

        for (line_index, line) in reader.lines().enumerate() {
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
            let Ok(record) = serde_json::from_str::<ClaudeRecord>(&line) else {
                if track_skipped_lines {
                    diagnostics.malformed_lines += 1;
                }
                continue;
            };
            if record.record_type.as_deref() != Some("assistant") {
                continue;
            }
            let Some(timestamp) = timestamp_str_ms(record.timestamp.as_deref()) else {
                continue;
            };
            if timestamp < cutoff_ms {
                continue;
            }
            let message = record.message.unwrap_or_default();
            let Some(usage) = message.usage else { continue };
            let session_id = record
                .session_id
                .unwrap_or_else(|| fallback_session.clone());
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

            if rejected_messages.contains(&key) {
                continue;
            }

            if let Some(stored) = messages.get_mut(&key) {
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
                    messages.remove(&key);
                    rejected_messages.insert(key);
                    diagnostics.rejected_events += 1;
                    continue;
                }

                stored.tokens.component_max(&candidate_usage);
                stored.request_id = stored.request_id.clone().or(record.request_id);
                stored.model = stored.model.clone().or(model);
                if timestamp >= stored.timestamp {
                    stored.timestamp = timestamp;
                }
            } else {
                messages.insert(
                    key.clone(),
                    MessageUsage {
                        timestamp,
                        session_id,
                        event_key: key,
                        request_id: record.request_id,
                        model,
                        tokens: candidate_usage,
                    },
                );
            }
        }

        let mut events: Vec<UsageEvent> = messages
            .into_values()
            .filter(|message| message.tokens.processed() > 0)
            .map(|message| {
                UsageEvent::new(
                    self.id(),
                    message.event_key,
                    message.timestamp,
                    message.session_id,
                    message.model,
                    message.tokens,
                    "exact",
                )
            })
            .collect();
        events.sort_by_key(|event| event.occurred_at_ms);

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
}
