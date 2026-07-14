use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 所有已启用 adapter 的 ID，前端 series 与汇总按此顺序输出。
pub const AGENT_IDS: [&str; 5] = ["codex", "claude", "zcode", "opencode", "kimi"];

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
    pub window_key: String,
    pub remaining_percent: f64,
    pub resets_at_ms: Option<i64>,
    pub collected_at_ms: i64,
    pub source_label: String,
    pub quality: &'static str,
}

/// Codex 的 primary/secondary 只是槽位，不是窗口语义：套餐不同，同一个槽位
/// 可能是 5 小时窗也可能是周窗（prolite 的 primary 就是 10080 分钟的周窗）。
/// 按窗口时长归类，槽位只作为缺时长时的回退，避免把周额度标成"5 小时"。
pub fn codex_window_key(window_minutes: Option<i64>, slot: &str) -> String {
    match window_minutes {
        Some(minutes) if minutes <= 1440 => "primary".into(),
        Some(_) => "secondary".into(),
        None => slot.to_owned(),
    }
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
    pub agent_quotas: Vec<AgentQuotaView>,
    pub agents: Vec<AgentSummary>,
    pub models: Vec<ModelSummary>,
    pub sources: Vec<SourceView>,
    pub cost: CostSummary,
}

/// 周期内的估算成本，与官方账单和本地解析用量是三类不同事实，永远分开呈现；
/// 没有可靠定价的模型（见 `pricing.rs`）不猜价格，其 token 计入
/// `unpriced_tokens` 而不是被折算进 `total_usd`。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostSummary {
    pub available: bool,
    pub total_usd: f64,
    pub unpriced_tokens: i64,
    pub pricing_as_of: String,
    pub by_agent: Vec<AgentCost>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCost {
    pub agent: String,
    pub usd: f64,
    pub unpriced_tokens: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesPoint {
    pub label: String,
    pub tokens: BTreeMap<String, i64>,
}

/// 一个 Agent 的全部官方滚动窗口（Session、每周、模型专属周限等），
/// 按短窗→长窗→其余的顺序排列；来源没有的窗口不臆造。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentQuotaView {
    pub agent: String,
    pub windows: Vec<AgentQuotaWindow>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentQuotaWindow {
    pub key: String,
    pub label: String,
    pub view: QuotaView,
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
    /// 未缓存输入、缓存读取、缓存写入、输出——processed 口径的分量拆解，
    /// 四项相加等于 `tokens`（同步导入的远端事件不带分量，按 0 计入）。
    pub input_uncached: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub output: i64,
    pub share: f64,
}

/// 周期内按模型聚合的 processed token 用量，按 tokens 降序排列。
/// 缺失或空白的模型名归入 "unknown"，不丢弃事件。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSummary {
    pub model: String,
    pub agent: String,
    pub tokens: i64,
    pub share: f64,
}

/// 只读的历史报告：182 天窗口内每日、按 Agent、按模型的 processed token 聚合，
/// 只查询本地账本已有数据，绝不触发日志扫描。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    pub generated_at: String,
    pub days: Vec<DayUsage>,
    pub first_event_ms: Option<i64>,
    pub last_event_ms: Option<i64>,
    pub total_tokens: i64,
    pub top_models: Vec<ModelSummary>,
    pub agents: Vec<AgentReportRow>,
    pub streak_days: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayUsage {
    pub date: String,
    pub tokens: i64,
    pub by_agent: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentReportRow {
    pub id: String,
    pub tokens: i64,
    pub active_days: i64,
}

/// 只读会话明细：按 (adapter, session_id) 聚合 `usage_event`，只查询本地账本
/// 已有数据，绝不触发日志扫描。`remote_usage_event` 没有会话维度，不计入。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSessions {
    pub period: String,
    pub sessions: Vec<SessionSummary>,
    pub total_sessions: i64,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub agent: String,
    pub session_id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    /// processed 总量：四项分量之和。
    pub tokens: i64,
    pub input_uncached: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub output: i64,
    /// 该会话内 token 最多的模型；会话内所有事件都没有模型名时为 None。
    pub model: Option<String>,
    /// 会话内出现过的全部模型，去重、按 token 降序。
    pub models: Vec<String>,
    /// 按 `pricing` 模块可计价部分求和的估算成本；会话内模型全部未定价时为 None。
    pub usd: Option<f64>,
    pub event_count: i64,
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
