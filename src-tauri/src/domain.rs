use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 所有已启用 adapter 的 ID，前端 series 与汇总按此顺序输出。
pub const AGENT_IDS: [&str; 4] = ["codex", "claude", "zcode", "opencode"];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TokenVector {
    pub input_uncached: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub output: i64,
    pub reasoning_output: i64,
}

impl TokenVector {
    pub fn processed(&self) -> i64 {
        self.input_uncached + self.cache_read + self.cache_write + self.output
    }

    pub fn positive_delta(&self, previous: Option<&Self>) -> Self {
        let Some(previous) = previous else {
            return self.clone();
        };

        let source_reset = self.input_uncached < previous.input_uncached
            || self.cache_read < previous.cache_read
            || self.cache_write < previous.cache_write
            || self.output < previous.output;
        if source_reset {
            return self.clone();
        }

        Self {
            input_uncached: (self.input_uncached - previous.input_uncached).max(0),
            cache_read: (self.cache_read - previous.cache_read).max(0),
            cache_write: (self.cache_write - previous.cache_write).max(0),
            output: (self.output - previous.output).max(0),
            reasoning_output: (self.reasoning_output - previous.reasoning_output).max(0),
        }
    }

    pub fn component_max(&mut self, other: &Self) {
        self.input_uncached = self.input_uncached.max(other.input_uncached);
        self.cache_read = self.cache_read.max(other.cache_read);
        self.cache_write = self.cache_write.max(other.cache_write);
        self.output = self.output.max(other.output);
        self.reasoning_output = self.reasoning_output.max(other.reasoning_output);
    }
}

#[derive(Clone, Debug)]
pub struct UsageEvent {
    pub event_id: String,
    pub adapter_id: &'static str,
    pub event_key: String,
    pub occurred_at_ms: i64,
    pub session_id: String,
    pub model: Option<String>,
    pub tokens: TokenVector,
    pub quality: &'static str,
    pub payload_hash: String,
}

impl UsageEvent {
    pub fn new(
        adapter_id: &'static str,
        event_key: String,
        occurred_at_ms: i64,
        session_id: String,
        model: Option<String>,
        tokens: TokenVector,
        quality: &'static str,
    ) -> Self {
        let payload = format!(
            "{adapter_id}|{event_key}|{occurred_at_ms}|{}|{}|{}|{}|{}|{}",
            session_id,
            tokens.input_uncached,
            tokens.cache_read,
            tokens.cache_write,
            tokens.output,
            tokens.reasoning_output
        );
        let event_id = stable_hash(&format!("{adapter_id}|{event_key}"));
        let payload_hash = stable_hash(&payload);
        Self {
            event_id,
            adapter_id,
            event_key,
            occurred_at_ms,
            session_id,
            model,
            tokens,
            quality,
            payload_hash,
        }
    }
}

#[derive(Clone, Debug)]
pub struct QuotaSample {
    pub adapter_id: &'static str,
    pub window_key: &'static str,
    pub remaining_percent: f64,
    pub resets_at_ms: Option<i64>,
    pub collected_at_ms: i64,
    pub source_label: String,
    pub quality: &'static str,
}

#[derive(Debug)]
pub struct ParsedSource {
    pub source_id: String,
    pub adapter_id: &'static str,
    pub locator: PathBuf,
    pub logical_key: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub events: Vec<UsageEvent>,
    pub quotas: Vec<QuotaSample>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub generated_at: String,
    pub period: String,
    pub is_demo: bool,
    pub total_tokens: i64,
    pub comparison_percent: f64,
    pub comparison_available: bool,
    pub series: Vec<SeriesPoint>,
    pub quota: QuotaView,
    pub secondary_quota: QuotaView,
    pub agents: Vec<AgentSummary>,
    pub sources: Vec<SourceView>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesPoint {
    pub label: String,
    pub tokens: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaView {
    pub available: bool,
    pub remaining_percent: f64,
    pub resets_in_minutes: Option<f64>,
    pub age_minutes: Option<f64>,
    pub stale: bool,
    pub reset_expired: bool,
    pub source_label: String,
    pub quality: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSummary {
    pub id: String,
    pub tokens: i64,
    pub share: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceView {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub detail: String,
    pub quality: String,
    pub quality_label: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncView {
    pub enabled: bool,
    pub directory: Option<String>,
    pub device_id: String,
    pub device_label: String,
    pub last_export_ms: Option<i64>,
    pub last_error: Option<String>,
    pub devices: Vec<SyncDeviceView>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncDeviceView {
    pub id: String,
    pub label: String,
    pub exported_at_ms: i64,
    pub last_import_ms: i64,
    pub events: i64,
}

pub fn stable_hash(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}
