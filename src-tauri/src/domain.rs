use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 所有已启用 adapter 的 ID，前端 series 与汇总按此顺序输出。
/// qoder 是配额-only：Qoder、QoderWork 与 Qoder CLI 共用同一账户级 Credits
/// 配额来源。Qoder CLI 的本地遥测 token 字段实测为 0，不能作为用量账本来源。
/// kimiwork 只保留为内部配额来源，窗口合并到 kimi，不作为独立可见 Agent。
/// qwen 只有本地用量：pi 的 qwen-token-plan key 用量归属到这张卡；百炼 Token
/// Plan 没有可编程的官方额度接口，不拉官方配额。
/// hermes 只有本地用量：Hermes 是 harness，走别家 coding plan 的用量按路由
/// 归属到对应卡片（见 hermes_providers），其余直连 API 留在这张卡。
pub const AGENT_IDS: [&str; 12] = [
    "codex",
    "claude",
    "zcode",
    "opencode",
    "kimi",
    "antigravity",
    "workbuddy",
    "qoder",
    "grok",
    "pi",
    "qwen",
    "hermes",
];

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

    /// 口径自检：把我们拆出来的分量和来源**自己报的总量**比一次。
    ///
    /// 读日志算 token 最危险的错法不是崩溃，而是把字段语义理解反了——
    /// reasoning 是否已含在 output 里、缓存读是否已含在输入里、拿到的是累计
    /// 还是增量、百分比是已用还是剩余。这类错**不会报错，只会显示一个看着
    /// 合理的错数字**，用户无从察觉。（真事：广泛使用的 tokscale 假设
    /// output 不含 reasoning 而分开相加，本机 11 万条 Codex 读数证明它是错的。）
    ///
    /// 凡是来源自带总量的，就用它当判据。对不上即记一次诊断，该来源标为
    /// 「数据不完整」并说明原因——把安静的错变成响亮的错。
    ///
    /// `reported_total <= 0` 视为"来源没报"，不参与判定：缺字段不是错。
    pub fn disagrees_with_reported_total(&self, reported_total: i64) -> bool {
        reported_total > 0 && self.processed() != reported_total
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
    /// 事件所属项目的工作目录（各 Agent 的 cwd / session directory）。
    /// 拿不到就是 None——不从会话名或日志路径反推。
    /// 刻意不参与 `payload_hash`：它是事件的附带归属，不是计量事实。若参与，
    /// 现有账本在解析器升级后重扫时会对同一 event_id 算出不同的 hash，而
    /// 非合并型 adapter（Codex/Kimi 等）遇到 hash 不一致就报身份冲突。
    pub project_path: Option<String>,
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
            project_path: None,
        }
    }

    /// 附加项目归属；`None` 与空白路径都保持"未归属"。
    pub fn with_project(mut self, project_path: Option<String>) -> Self {
        self.project_path = project_path.and_then(|value| normalize_project_path(&value));
        self
    }
}

/// 项目路径的存储形态：反斜杠转正斜杠、去掉尾部分隔符、Windows 盘符统一大写。
/// 盘符大小写是同一台机器上不同 Agent 之间唯一实测会分叉的地方（`D:\work` 与
/// `d:/work`）；路径其余部分按各 Agent 报出的原样保留，不做大小写折叠。
pub fn normalize_project_path(value: &str) -> Option<String> {
    let mut path = value.trim().replace('\\', "/");
    while path.len() > 1 && path.ends_with('/') && !path.ends_with(":/") {
        path.pop();
    }
    if path.is_empty() {
        return None;
    }
    let mut chars = path.chars();
    if let (Some(drive), Some(':')) = (chars.next(), chars.next()) {
        if drive.is_ascii_alphabetic() {
            path = format!("{}{}", drive.to_ascii_uppercase(), &path[1..]);
        }
    }
    Some(path)
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

/// 重置时间的合理性校验：实测 Claude Code 在重置时间未知时会下发哨兵值
/// （1900000000 秒 ≈ 2030 年），直接展示就是"1331 天后重置"。重置时间必然
/// 落在窗口语义内（5h 窗 ~6h、7d 窗 ~8d、月级窗口 ~35 天），越界一律丢弃
/// ——宁可不显示倒计时，也不显示错的。
pub fn sane_resets_at_ms(window_key: &str, resets_at_ms: i64, collected_at_ms: i64) -> Option<i64> {
    const HOUR_MS: i64 = 3_600_000;
    const DAY_MS: i64 = 86_400_000;
    let max_span_ms = if window_key == "five_hour" {
        6 * HOUR_MS
    } else if window_key.starts_with("seven_day") {
        8 * DAY_MS
    } else {
        35 * DAY_MS
    };
    // 下界放宽 10 分钟：采集时刻与负载内时间戳之间有合理的时钟/排程差。
    let plausible = resets_at_ms >= collected_at_ms - 10 * 60_000
        && resets_at_ms <= collected_at_ms + max_span_ms;
    plausible.then_some(resets_at_ms)
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
    pub indexing: IndexingView,
}

/// 历史索引的补齐进度。日志按固定的保留期视界解析，与 UI 选的周期无关；
/// 首次索引和解析器升级后剩下的文件分批补齐，每次快照只花掉一小段时间预算。
/// `pending > 0` 时账本尚未覆盖完整历史，数字必须显式标注为补齐中，
/// 不得当作精确结果呈现。
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexingView {
    pub pending: usize,
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
    /// 没有窗口时的原因（目前只有 Claude 直连查询失败会填）。让"没有数字"
    /// 这件事可自查，而不是笼统地叫用户去开另一个来源。
    pub note: Option<String>,
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
    /// 本机装了这个 Agent：安装痕迹命中，或本周期确有用量（后者兜住没有可靠
    /// 安装探针的 Agent）。只用于设置里的排序分组，不用于过滤——见 `detect`。
    pub detected: bool,
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
    /// 182 天窗口内按项目（分组规则归并后）的走势，token 降序取前若干个。
    pub projects: Vec<ProjectReportRow>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectReportRow {
    pub path: String,
    pub label: String,
    pub tokens: i64,
    /// 26 个 7 天桶，最旧在前、以今日结尾，供 sparkline 使用。
    pub weekly: Vec<i64>,
    /// 近 7 天相对再前 7 天的变化率（百分比）；前一段为 0 时为 None。
    pub recent_delta_percent: Option<f64>,
    pub active_days: i64,
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
    /// 该会话内 token 最多的项目（按分组规则归并后的根路径）；
    /// 事件都没有归属或全部被隐藏时为 None。
    pub project: Option<String>,
    /// 项目根的目录名，供列表直接显示。
    pub project_label: Option<String>,
}

/// 只读项目明细：按分组规则归并后的项目聚合 `usage_event`，只查询本地账本
/// 已有数据，绝不触发日志扫描。归属不到的事件不并进任何项目：读不到目录的
/// 计入 `unattributed_tokens`，命中隐藏规则的计入 `hidden_tokens`，都如实
/// 单列，不塞进"其他"。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageProjects {
    pub period: String,
    pub projects: Vec<ProjectSummary>,
    pub total_projects: i64,
    pub truncated: bool,
    pub unattributed_tokens: i64,
    /// 本周期内有用量、但当前版本读不到项目归属的 Agent（如 Antigravity）。
    pub unattributed_agents: Vec<String>,
    /// 命中隐藏规则（用户或内置）的用量。
    pub hidden_tokens: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    /// 项目根目录（归一化后的完整路径；手动登记或 .git 归并的结果）。
    pub path: String,
    /// 目录名，用于列表主标题。
    pub label: String,
    pub tokens: i64,
    pub input_uncached: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub output: i64,
    /// 可计价部分的估算成本；项目内模型全部未定价时为 None。
    pub usd: Option<f64>,
    pub session_count: i64,
    pub event_count: i64,
    pub last_ms: i64,
    /// 该项目下用过的 Agent，按 token 降序。
    pub agents: Vec<String>,
    /// 该项目下 token 最多的模型。
    pub model: Option<String>,
    /// 是否命中手动登记的项目根。
    pub pinned: bool,
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
