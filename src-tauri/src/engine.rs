use crate::adapters::{
    AgentAdapter, AntigravityAdapter, ClaudeAdapter, CodexAdapter, KimiAdapter, OpencodeAdapter,
    ScanDiagnostics, SourceCandidate, ZcodeAdapter,
};
use crate::app_server;
use crate::claude_hook::ClaudeHook;
use crate::claude_oauth::{self, ClaudeOauth};
use crate::coding_quota;
use crate::domain::{
    AgentCost, AgentQuotaView, AgentReportRow, AgentSummary, CostSummary, DayUsage, IndexingView,
    ModelSummary, QuotaSample, QuotaView, SeriesPoint, SessionSummary, SourceView, SyncView,
    UsageReport, UsageSessions, UsageSnapshot, AGENT_IDS,
};
use crate::pricing;
use crate::storage;
use crate::sync;
use anyhow::{Context, Result};
use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone, Timelike, Utc, Weekday};
use rusqlite::{params, Connection};
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration as StdDuration, Instant};

/// 报告窗口固定为 182 天（26 周），与 `usage_snapshot` 的扫描周期无关。
const REPORT_WINDOW_DAYS: i64 = 182;

/// 账本保留期，同时是唯一的解析视界。解析窗口**不跟随 UI 周期**：跟随会让
/// 切到「30 天」时把此前只按 8 天窗口解析过的日志全部整份重扫，代价整个压在
/// 一次前台请求里（实测本机 691 个文件、约 2GB JSONL）。固定视界的代价是每个
/// 文件只在新增或变化时解析一次，切周期退化为纯 SQL 查询。
const RETENTION_DAYS: i64 = 65;

/// 每次快照分给日志解析的时间预算。待解析的源按 mtime 倒序排队（最近改动的先做，
/// 当前周期的数字最先准确），超预算的留给下一次刷新，剩余量记进 `backfill_pending`。
/// 预算只在文件之间检查，所以单个大文件可能超出——这是可接受的，
/// 代价上限是一个文件的解析时间，而不是整个日志库。
const PARSE_BUDGET: StdDuration = StdDuration::from_millis(1500);

#[derive(Default)]
struct ScanReport {
    discovered: HashMap<String, usize>,
    refreshed: HashMap<String, usize>,
    errors: HashMap<String, usize>,
    diagnostics: HashMap<String, AdapterDiagnostics>,
    /// adapter 自报的"存在但读不了"的存储形态（见 AgentAdapter::coverage_gaps）。
    /// 非空 → 该 Agent 标"部分覆盖"，原因原样展示。
    coverage_gaps: HashMap<String, Vec<String>>,
    backfill_pending: usize,
}

#[derive(Clone, Debug, Default)]
struct AdapterDiagnostics {
    partial_sources: usize,
    malformed_lines: usize,
    unreadable_lines: usize,
    rejected_events: usize,
}

#[derive(Clone)]
struct StoredEvent {
    adapter: String,
    timestamp: i64,
    tokens: i64,
    model: Option<String>,
    input_uncached: i64,
    cache_read: i64,
    cache_write: i64,
    output: i64,
}

/// 一个 Agent 在周期内的 processed token 分量拆解。远端同步事件不带分量，
/// 按 0 计入，不会伪造精度。
#[derive(Clone, Copy, Default)]
struct TokenComponents {
    input_uncached: i64,
    cache_read: i64,
    cache_write: i64,
    output: i64,
}

impl TokenComponents {
    fn processed(&self) -> i64 {
        self.input_uncached + self.cache_read + self.cache_write + self.output
    }

    fn add(&mut self, other: &Self) {
        self.input_uncached += other.input_uncached;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.output += other.output;
    }
}

struct StoredSessionEvent {
    adapter: String,
    session_id: String,
    timestamp: i64,
    model: Option<String>,
    input_uncached: i64,
    cache_read: i64,
    cache_write: i64,
    output: i64,
}

#[derive(Default)]
struct SessionAgg {
    start_ms: i64,
    end_ms: i64,
    totals: TokenComponents,
    event_count: i64,
    model_components: HashMap<String, TokenComponents>,
}

pub fn build_snapshot(
    database_path: &Path,
    period: &str,
    quota_cache: &Mutex<Option<(Instant, Vec<QuotaSample>)>>,
    claude_quota_cache: &Mutex<Option<(Instant, Vec<QuotaSample>)>>,
    http_quota_cache: &Mutex<HashMap<&'static str, (Instant, Vec<QuotaSample>)>>,
) -> Result<UsageSnapshot> {
    let mut connection = storage::open_database(database_path)?;
    let now = Utc::now().timestamp_millis();
    let retention_cutoff = now - Duration::days(RETENTION_DAYS).num_milliseconds();
    let report = ingest_sources(&mut connection, retention_cutoff)?;

    // app-server 是 Codex 额度的权威来源：拉到就整体替换，套餐变更后消失的
    // 窗口（如 prolite 没有 5 小时窗）不得留着旧日志快照冒充当前额度。
    if let Ok(samples) = cached_live_quota(quota_cache) {
        if !samples.is_empty() {
            connection.execute("DELETE FROM quota_snapshot WHERE adapter_id = 'codex'", [])?;
        }
        for sample in &samples {
            storage::upsert_quota(&connection, sample)?;
        }
    }
    // Claude 官方额度：用户显式开启 OAuth 来源时优先（账户级合并额度，
    // 不依赖终端状态栏）；拉取失败或未开启时回落到 statusLine 钩子文件。
    let oauth_enabled =
        storage::get_app_setting(&connection, claude_oauth::SETTING_KEY)?.as_deref() == Some("1");
    let mut claude_samples = Vec::new();
    if oauth_enabled {
        if let Ok(samples) = cached_claude_oauth_quota(claude_quota_cache) {
            claude_samples = samples;
        }
    }
    if claude_samples.is_empty() {
        claude_samples = ClaudeHook::detected().quota_samples();
    }
    if !claude_samples.is_empty() {
        // 当前来源是 Claude 配额的唯一事实：整体替换，来源里消失的窗口
        // （或旧版 primary/secondary 键）不得滞留在展示里。
        connection.execute("DELETE FROM quota_snapshot WHERE adapter_id = 'claude'", [])?;
    }
    for sample in claude_samples {
        storage::upsert_quota(&connection, &sample)?;
    }

    // GLM/Kimi 官方配额：与 codex/claude 同型，一次实时 GET，跨快照缓存限流。
    // 取到才整体替换该 Agent 的窗口；无凭据/失败时保留旧行（会随时效变陈旧），
    // 绝不写零值或估算冒充。
    for (adapter_id, fetch) in [
        (
            "zcode",
            coding_quota::fetch_zcode_quota as fn(StdDuration) -> Result<Vec<QuotaSample>>,
        ),
        ("kimi", coding_quota::fetch_kimi_quota),
    ] {
        if let Ok(samples) = cached_coding_quota(http_quota_cache, adapter_id, fetch) {
            if !samples.is_empty() {
                connection.execute(
                    "DELETE FROM quota_snapshot WHERE adapter_id = ?1",
                    [adapter_id],
                )?;
                for sample in &samples {
                    storage::upsert_quota(&connection, sample)?;
                }
            }
        }
    }

    storage::prune_missing_sources(&mut connection)?;
    storage::prune_old_events(&connection, retention_cutoff)?;
    sync::run_sync(&mut connection, now);
    query_snapshot(&connection, period, report)
}

/// 只读历史报告：仅查询本地账本已有数据，绝不触发日志扫描或写入，
/// 保证报告页秒开。
pub fn build_report(database_path: &Path) -> Result<UsageReport> {
    let connection = storage::open_database_read_only(database_path)?;
    report_at(&connection, Local::now())
}

fn report_at(connection: &Connection, local_now: chrono::DateTime<Local>) -> Result<UsageReport> {
    let today = local_now.date_naive();
    let start_date = today - Duration::days(REPORT_WINDOW_DAYS - 1);
    let start_ms = local_midnight(start_date)?.timestamp_millis();
    let end_ms = local_now.timestamp_millis() + 1;
    let events = load_events(connection, start_ms, end_ms)?;

    let (first_event_ms, last_event_ms) = global_event_bounds(connection)?;

    let mut day_totals: BTreeMap<NaiveDate, i64> = BTreeMap::new();
    let mut day_by_agent: HashMap<NaiveDate, BTreeMap<String, i64>> = HashMap::new();
    let mut agent_totals: HashMap<&str, i64> = AGENT_IDS.iter().map(|agent| (*agent, 0)).collect();
    let mut agent_active_days: HashMap<&str, HashSet<NaiveDate>> = AGENT_IDS
        .iter()
        .map(|agent| (*agent, HashSet::new()))
        .collect();
    let mut model_totals: HashMap<(String, String), i64> = HashMap::new();
    let mut total_tokens: i64 = 0;

    for event in events {
        let Some(agent) = AGENT_IDS.iter().find(|agent| **agent == event.adapter) else {
            continue;
        };
        let local = match Local.timestamp_millis_opt(event.timestamp).single() {
            Some(value) => value,
            None => continue,
        };
        let date = local.date_naive();

        *day_totals.entry(date).or_default() += event.tokens;
        *day_by_agent
            .entry(date)
            .or_default()
            .entry((*agent).to_owned())
            .or_default() += event.tokens;
        *agent_totals.get_mut(agent).expect("registered agent") += event.tokens;
        agent_active_days
            .get_mut(agent)
            .expect("registered agent")
            .insert(date);
        total_tokens += event.tokens;

        let model_key = event
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown")
            .to_owned();
        *model_totals
            .entry(((*agent).to_owned(), model_key))
            .or_insert(0) += event.tokens;
    }

    let streak_days = compute_streak(&day_totals);

    let days: Vec<DayUsage> = day_totals
        .into_iter()
        .map(|(date, tokens)| DayUsage {
            date: date.format("%Y-%m-%d").to_string(),
            tokens,
            by_agent: day_by_agent.remove(&date).unwrap_or_default(),
        })
        .collect();

    let share = |tokens: i64| {
        if total_tokens > 0 {
            tokens as f64 * 100.0 / total_tokens as f64
        } else {
            0.0
        }
    };
    let mut top_models: Vec<ModelSummary> = model_totals
        .into_iter()
        .map(|((agent, model), tokens)| ModelSummary {
            model,
            agent,
            tokens,
            share: share(tokens),
        })
        .collect();
    top_models.sort_by_key(|model| Reverse(model.tokens));
    top_models.truncate(10);

    let agents = AGENT_IDS
        .iter()
        .map(|agent| AgentReportRow {
            id: (*agent).to_owned(),
            tokens: agent_totals[agent],
            active_days: agent_active_days[agent].len() as i64,
        })
        .collect();

    Ok(UsageReport {
        generated_at: Utc::now().to_rfc3339(),
        days,
        first_event_ms,
        last_event_ms,
        total_tokens,
        top_models,
        agents,
        streak_days,
    })
}

const MAX_SESSIONS: usize = 300;

/// 只读会话明细：只查询本地账本已有数据，绝不触发日志扫描，不占用扫描锁。
/// `remote_usage_event` 没有会话维度，不计入。
pub fn build_sessions(database_path: &Path, period: &str) -> Result<UsageSessions> {
    let connection = storage::open_database_read_only(database_path)?;
    sessions_at(&connection, period, Local::now())
}

fn sessions_at(
    connection: &Connection,
    requested_period: &str,
    local_now: chrono::DateTime<Local>,
) -> Result<UsageSessions> {
    let period = match requested_period {
        "week" | "month" => requested_period,
        _ => "today",
    };
    let today = local_now.date_naive();
    let start_date = match period {
        "week" => today - Duration::days(6),
        "month" => today - Duration::days(29),
        _ => today,
    };
    let start_ms = local_midnight(start_date)?.timestamp_millis();
    let end_ms = local_now.timestamp_millis() + 1;

    let events = load_session_events(connection, start_ms, end_ms)?;

    let mut aggregates: HashMap<(String, String), SessionAgg> = HashMap::new();
    for event in events {
        let Some(agent) = AGENT_IDS.iter().find(|agent| **agent == event.adapter) else {
            continue;
        };
        let session_id = {
            let trimmed = event.session_id.trim();
            if trimmed.is_empty() {
                "unknown".to_owned()
            } else {
                trimmed.to_owned()
            }
        };
        let key = ((*agent).to_owned(), session_id);
        let agg = aggregates.entry(key).or_insert_with(|| SessionAgg {
            start_ms: event.timestamp,
            end_ms: event.timestamp,
            ..Default::default()
        });
        agg.start_ms = agg.start_ms.min(event.timestamp);
        agg.end_ms = agg.end_ms.max(event.timestamp);
        agg.event_count += 1;
        let comps = TokenComponents {
            input_uncached: event.input_uncached,
            cache_read: event.cache_read,
            cache_write: event.cache_write,
            output: event.output,
        };
        agg.totals.add(&comps);
        if let Some(model) = event
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            agg.model_components
                .entry(model.to_owned())
                .or_default()
                .add(&comps);
        }
    }

    let mut sessions: Vec<SessionSummary> = aggregates
        .into_iter()
        .map(|((agent, session_id), agg)| {
            let mut model_totals: Vec<(String, i64)> = agg
                .model_components
                .iter()
                .map(|(model, comps)| (model.clone(), comps.processed()))
                .collect();
            model_totals.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let model = model_totals.first().map(|(model, _)| model.clone());
            let models = model_totals.into_iter().map(|(model, _)| model).collect();

            let mut usd_total = 0.0_f64;
            let mut priced_any = false;
            for (model, comps) in &agg.model_components {
                if let Some(price) = pricing::price_for(model) {
                    priced_any = true;
                    usd_total += comps.input_uncached as f64 * price.input / 1_000_000.0
                        + comps.cache_read as f64 * price.cache_read / 1_000_000.0
                        + comps.cache_write as f64 * price.cache_write / 1_000_000.0
                        + comps.output as f64 * price.output / 1_000_000.0;
                }
            }
            let usd = priced_any.then_some(usd_total);

            SessionSummary {
                agent,
                session_id,
                start_ms: agg.start_ms,
                end_ms: agg.end_ms,
                tokens: agg.totals.processed(),
                input_uncached: agg.totals.input_uncached,
                cache_read: agg.totals.cache_read,
                cache_write: agg.totals.cache_write,
                output: agg.totals.output,
                model,
                models,
                usd,
                event_count: agg.event_count,
            }
        })
        .collect();

    sessions.sort_by_key(|session| Reverse(session.end_ms));
    let total_sessions = sessions.len() as i64;
    let truncated = sessions.len() > MAX_SESSIONS;
    sessions.truncate(MAX_SESSIONS);

    Ok(UsageSessions {
        period: period.into(),
        sessions,
        total_sessions,
        truncated,
    })
}

fn load_session_events(
    connection: &Connection,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<StoredSessionEvent>> {
    let mut statement = connection.prepare(
        "SELECT adapter_id, session_id, occurred_at_ms, model,
                input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens
         FROM usage_event
         WHERE occurred_at_ms >= ?1 AND occurred_at_ms < ?2
         ORDER BY occurred_at_ms",
    )?;
    let rows = statement.query_map(params![start_ms, end_ms], |row| {
        Ok(StoredSessionEvent {
            adapter: row.get(0)?,
            session_id: row.get(1)?,
            timestamp: row.get(2)?,
            model: row.get(3)?,
            input_uncached: row.get(4)?,
            cache_read: row.get(5)?,
            cache_write: row.get(6)?,
            output: row.get(7)?,
        })
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

/// 截至最近一个有数据的日子，向前数连续活跃天数；没有数据返回 0。
fn compute_streak(day_totals: &BTreeMap<NaiveDate, i64>) -> i64 {
    let Some((&last_active_date, _)) = day_totals.iter().next_back() else {
        return 0;
    };
    let mut streak = 0_i64;
    let mut cursor = last_active_date;
    loop {
        if !day_totals.contains_key(&cursor) {
            break;
        }
        streak += 1;
        cursor -= Duration::days(1);
    }
    streak
}

/// 账本中最早/最晚事件时间（本地事件与同步导入事件均计入），不限定报告窗口，
/// 用于前端标注"数据自 X 起"。
fn global_event_bounds(connection: &Connection) -> Result<(Option<i64>, Option<i64>)> {
    connection
        .query_row(
            "SELECT MIN(min_ms), MAX(max_ms) FROM (
                 SELECT MIN(occurred_at_ms) AS min_ms, MAX(occurred_at_ms) AS max_ms FROM usage_event
                 UNION ALL
                 SELECT MIN(occurred_at_ms), MAX(occurred_at_ms) FROM remote_usage_event
             )",
            [],
            |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .context("failed to calculate ledger event bounds")
}

fn cached_live_quota(
    cache: &Mutex<Option<(Instant, Vec<QuotaSample>)>>,
) -> Result<Vec<QuotaSample>> {
    let mut guard = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("quota cache lock poisoned"))?;
    if let Some((captured, value)) = guard.as_ref() {
        let ttl = if value.is_empty() { 240 } else { 60 };
        if captured.elapsed() < StdDuration::from_secs(ttl) {
            return Ok(value.clone());
        }
    }
    match app_server::read_codex_quota(StdDuration::from_secs(4)) {
        Ok(value) => {
            *guard = Some((Instant::now(), value.clone()));
            Ok(value)
        }
        Err(error) => {
            // An unavailable CLI should not make every periodic widget refresh
            // pay the full process timeout. The empty sentinel uses a longer TTL.
            *guard = Some((Instant::now(), Vec::new()));
            Err(error)
        }
    }
}

/// Claude OAuth 官方额度的缓存拉取：成功 120s、失败 300s（限流友好）。
fn cached_claude_oauth_quota(
    cache: &Mutex<Option<(Instant, Vec<QuotaSample>)>>,
) -> Result<Vec<QuotaSample>> {
    let mut guard = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("claude quota cache lock poisoned"))?;
    if let Some((captured, value)) = guard.as_ref() {
        let ttl = if value.is_empty() { 300 } else { 120 };
        if captured.elapsed() < StdDuration::from_secs(ttl) {
            return Ok(value.clone());
        }
    }
    match ClaudeOauth::detected().fetch_quota_samples(StdDuration::from_secs(6)) {
        Ok(value) => {
            *guard = Some((Instant::now(), value.clone()));
            Ok(value)
        }
        Err(error) => {
            *guard = Some((Instant::now(), Vec::new()));
            Err(error)
        }
    }
}

/// GLM/Kimi 等走网络的官方配额缓存拉取：按 adapter 分桶，成功 120s、失败 300s。
/// adapter 每次快照都重建，故缓存不能放 adapter 里，必须由 engine 层跨快照持有。
fn cached_coding_quota(
    cache: &Mutex<HashMap<&'static str, (Instant, Vec<QuotaSample>)>>,
    adapter_id: &'static str,
    fetch: fn(StdDuration) -> Result<Vec<QuotaSample>>,
) -> Result<Vec<QuotaSample>> {
    let mut guard = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("coding quota cache lock poisoned"))?;
    if let Some((captured, value)) = guard.get(adapter_id) {
        let ttl = if value.is_empty() { 300 } else { 120 };
        if captured.elapsed() < StdDuration::from_secs(ttl) {
            return Ok(value.clone());
        }
    }
    match fetch(StdDuration::from_secs(6)) {
        Ok(value) => {
            guard.insert(adapter_id, (Instant::now(), value.clone()));
            Ok(value)
        }
        Err(error) => {
            guard.insert(adapter_id, (Instant::now(), Vec::new()));
            Err(error)
        }
    }
}

/// 按固定的保留期视界摄取日志。需要解析的源按 mtime 倒序排队，在时间预算内尽量
/// 解析；没轮到的记进 `report.backfill_pending`，由界面显式标注为「补齐中」。
fn ingest_sources(connection: &mut Connection, horizon_ms: i64) -> Result<ScanReport> {
    let adapters: Vec<Box<dyn AgentAdapter>> = vec![
        Box::new(CodexAdapter::detected()),
        Box::new(ClaudeAdapter::detected()),
        Box::new(ZcodeAdapter::detected()),
        Box::new(OpencodeAdapter::detected()),
        Box::new(KimiAdapter::detected()),
        Box::new(AntigravityAdapter::detected()),
    ];
    let mut report = ScanReport::default();
    let mut queue: Vec<(usize, SourceCandidate)> = Vec::new();

    for (index, adapter) in adapters.iter().enumerate() {
        let candidates = adapter.discover(horizon_ms);
        let stored_diagnostics = stored_adapter_scan_diagnostics(connection, adapter.id())?;
        let gaps = adapter.coverage_gaps();
        if !gaps.is_empty() {
            report.coverage_gaps.insert(adapter.id().into(), gaps);
        }
        report
            .discovered
            .insert(adapter.id().into(), candidates.len());
        for candidate in candidates {
            if storage::source_is_current(
                connection,
                &candidate.source_id,
                candidate.size,
                candidate.mtime_ns,
                horizon_ms,
            )? {
                if let Some(diagnostics) = stored_diagnostics.get(&candidate.source_id) {
                    record_scan_diagnostics(&mut report, adapter.id(), diagnostics);
                }
                continue;
            }
            queue.push((index, candidate));
        }
    }

    // 最近改动的先解析：当前周期的数字最先变准，历史从近端往回填。
    queue.sort_by_key(|(_, candidate)| Reverse(candidate.mtime_ns));
    let deadline = Instant::now() + PARSE_BUDGET;
    for (index, candidate) in &queue {
        if Instant::now() >= deadline {
            report.backfill_pending += 1;
            continue;
        }
        ingest_candidate(
            connection,
            adapters[*index].as_ref(),
            candidate,
            horizon_ms,
            &mut report,
        )?;
    }

    Ok(report)
}

fn ingest_candidate(
    connection: &mut Connection,
    adapter: &dyn AgentAdapter,
    candidate: &SourceCandidate,
    horizon_ms: i64,
    report: &mut ScanReport,
) -> Result<()> {
    match adapter.parse(candidate, horizon_ms) {
        Ok(scan) => {
            let mut diagnostics = scan.diagnostics;
            if let Ok(outcome) = storage::replace_source(connection, &scan.source, horizon_ms) {
                diagnostics.rejected_events += outcome.rejected_events;
                if let Some(marker) = diagnostics.storage_marker() {
                    connection
                        .execute(
                            "UPDATE scan_source SET last_error = ?1 WHERE source_id = ?2",
                            params![marker, candidate.source_id],
                        )
                        .context("failed to persist JSONL scan diagnostics")?;
                }
                record_scan_diagnostics(report, adapter.id(), &diagnostics);
                *report.refreshed.entry(adapter.id().into()).or_default() += 1;
            } else {
                *report.errors.entry(adapter.id().into()).or_default() += 1;
            }
        }
        Err(_) => {
            *report.errors.entry(adapter.id().into()).or_default() += 1;
        }
    }
    Ok(())
}

fn stored_adapter_scan_diagnostics(
    connection: &Connection,
    adapter_id: &str,
) -> Result<HashMap<String, ScanDiagnostics>> {
    let mut statement = connection.prepare(
        "SELECT source_id, last_error FROM scan_source
         WHERE adapter_id = ?1 AND last_error IS NOT NULL",
    )?;
    let rows = statement.query_map([adapter_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut diagnostics = HashMap::new();
    for row in rows {
        let (source_id, marker) = row?;
        if let Some(parsed) = ScanDiagnostics::from_storage_marker(&marker) {
            diagnostics.insert(source_id, parsed);
        }
    }
    Ok(diagnostics)
}

fn record_scan_diagnostics(
    report: &mut ScanReport,
    adapter_id: &str,
    diagnostics: &ScanDiagnostics,
) {
    if !diagnostics.is_partial() {
        return;
    }
    let aggregate = report.diagnostics.entry(adapter_id.into()).or_default();
    aggregate.partial_sources += 1;
    aggregate.malformed_lines += diagnostics.malformed_lines;
    aggregate.unreadable_lines += diagnostics.unreadable_lines;
    aggregate.rejected_events += diagnostics.rejected_events;
}

fn query_snapshot(
    connection: &Connection,
    requested_period: &str,
    report: ScanReport,
) -> Result<UsageSnapshot> {
    query_snapshot_at(connection, requested_period, report, Local::now())
}

fn query_snapshot_at(
    connection: &Connection,
    requested_period: &str,
    report: ScanReport,
    local_now: chrono::DateTime<Local>,
) -> Result<UsageSnapshot> {
    let period = match requested_period {
        "week" | "month" => requested_period,
        _ => "today",
    };
    let today = local_now.date_naive();
    let (start_date, comparison_days) = match period {
        "week" => (today - Duration::days(6), 7_i64),
        "month" => (today - Duration::days(29), 30_i64),
        _ => (today, 7_i64),
    };
    let bucket_count = period_bucket_count(period, local_now.hour());
    let start_ms = local_midnight(start_date)?.timestamp_millis();
    let end_ms = local_now.timestamp_millis() + 1;
    let events = load_events(connection, start_ms, end_ms)?;

    let mut buckets: HashMap<&str, Vec<i64>> = AGENT_IDS
        .iter()
        .map(|agent| (*agent, vec![0_i64; bucket_count]))
        .collect();
    let mut totals: HashMap<&str, i64> = AGENT_IDS.iter().map(|agent| (*agent, 0)).collect();
    let mut components: HashMap<&str, TokenComponents> = AGENT_IDS
        .iter()
        .map(|agent| (*agent, TokenComponents::default()))
        .collect();
    let mut model_totals: HashMap<(String, String), i64> = HashMap::new();
    let mut model_components: HashMap<(String, String), TokenComponents> = HashMap::new();

    for event in events {
        let Some(agent) = AGENT_IDS.iter().find(|agent| **agent == event.adapter) else {
            continue;
        };
        let local = match Local.timestamp_millis_opt(event.timestamp).single() {
            Some(value) => value,
            None => continue,
        };
        let index = if period == "today" {
            local.hour() as usize
        } else {
            (local.date_naive() - start_date).num_days().max(0) as usize
        };
        if index >= bucket_count {
            continue;
        }
        buckets.get_mut(agent).expect("registered agent")[index] += event.tokens;
        *totals.entry(agent).or_default() += event.tokens;
        let comps = components.get_mut(agent).expect("registered agent");
        comps.input_uncached += event.input_uncached;
        comps.cache_read += event.cache_read;
        comps.cache_write += event.cache_write;
        comps.output += event.output;
        let model_key = event
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown")
            .to_owned();
        let model_map_key = ((*agent).to_owned(), model_key);
        *model_totals.entry(model_map_key.clone()).or_insert(0) += event.tokens;
        let model_comp = model_components.entry(model_map_key).or_default();
        model_comp.input_uncached += event.input_uncached;
        model_comp.cache_read += event.cache_read;
        model_comp.cache_write += event.cache_write;
        model_comp.output += event.output;
    }

    if period == "today" {
        for bucket in buckets.values_mut() {
            for index in 1..bucket_count {
                bucket[index] += bucket[index - 1];
            }
        }
    }

    let series = (0..bucket_count)
        .map(|index| SeriesPoint {
            label: bucket_label(period, start_date, index),
            tokens: AGENT_IDS
                .iter()
                .map(|agent| ((*agent).to_owned(), buckets[agent][index]))
                .collect(),
        })
        .collect();

    let total_tokens: i64 = totals.values().sum();
    let denominator = if period == "today" {
        average_prior_elapsed_windows(connection, &local_now, comparison_days)?
    } else {
        let previous_start = start_date - Duration::days(comparison_days);
        let previous_final_date = start_date - Duration::days(1);
        let elapsed_today = local_now.signed_duration_since(local_midnight(today)?);
        let previous_final_start = local_midnight(previous_final_date)?;
        let next_period_start = local_midnight(start_date)?;
        let matching_end = (previous_final_start + elapsed_today).min(next_period_start);
        let previous_end_ms = matching_end
            .timestamp_millis()
            .saturating_add(1)
            .min(next_period_start.timestamp_millis());
        let previous_total = sum_tokens_between(
            connection,
            local_midnight(previous_start)?.timestamp_millis(),
            previous_end_ms,
        )?;
        previous_total as f64
    };
    let comparison_available = denominator > 0.0;
    let comparison_percent = if comparison_available {
        ((total_tokens as f64 - denominator) / denominator) * 100.0
    } else {
        0.0
    };
    let share = |tokens: i64| {
        if total_tokens > 0 {
            tokens as f64 * 100.0 / total_tokens as f64
        } else {
            0.0
        }
    };

    let mut agent_cost_usd: HashMap<&str, f64> =
        AGENT_IDS.iter().map(|agent| (*agent, 0.0)).collect();
    let mut agent_unpriced_tokens: HashMap<&str, i64> =
        AGENT_IDS.iter().map(|agent| (*agent, 0)).collect();
    let mut total_usd = 0.0_f64;
    let mut unpriced_tokens = 0_i64;
    for ((agent, model), comp) in &model_components {
        let processed = comp.input_uncached + comp.cache_read + comp.cache_write + comp.output;
        match pricing::price_for(model) {
            Some(price) => {
                let usd = comp.input_uncached as f64 * price.input / 1_000_000.0
                    + comp.cache_read as f64 * price.cache_read / 1_000_000.0
                    + comp.cache_write as f64 * price.cache_write / 1_000_000.0
                    + comp.output as f64 * price.output / 1_000_000.0;
                total_usd += usd;
                if let Some(entry) = agent_cost_usd.get_mut(agent.as_str()) {
                    *entry += usd;
                }
            }
            None => {
                unpriced_tokens += processed;
                if let Some(entry) = agent_unpriced_tokens.get_mut(agent.as_str()) {
                    *entry += processed;
                }
            }
        }
    }
    let cost = CostSummary {
        available: true,
        total_usd,
        unpriced_tokens,
        pricing_as_of: pricing::PRICING_AS_OF.to_owned(),
        by_agent: AGENT_IDS
            .iter()
            .map(|agent| AgentCost {
                agent: (*agent).to_owned(),
                usd: agent_cost_usd[agent],
                unpriced_tokens: agent_unpriced_tokens[agent],
            })
            .collect(),
    };

    let mut models: Vec<ModelSummary> = model_totals
        .into_iter()
        .map(|((agent, model), tokens)| ModelSummary {
            model,
            agent,
            tokens,
            share: share(tokens),
        })
        .collect();
    models.sort_by_key(|model| Reverse(model.tokens));

    Ok(UsageSnapshot {
        generated_at: Utc::now().to_rfc3339(),
        period: period.into(),
        is_demo: false,
        total_tokens,
        comparison_percent,
        comparison_available,
        series,
        agent_quotas: AGENT_IDS
            .iter()
            .map(|agent| {
                Ok(AgentQuotaView {
                    agent: (*agent).to_owned(),
                    windows: load_agent_quota_windows(connection, agent)?,
                })
            })
            .collect::<Result<Vec<_>>>()?,
        agents: AGENT_IDS
            .iter()
            .map(|agent| {
                let comps = components[agent];
                AgentSummary {
                    id: (*agent).to_owned(),
                    tokens: totals[agent],
                    input_uncached: comps.input_uncached,
                    cache_read: comps.cache_read,
                    cache_write: comps.cache_write,
                    output: comps.output,
                    share: share(totals[agent]),
                }
            })
            .collect(),
        models,
        indexing: IndexingView {
            pending: report.backfill_pending,
        },
        sources: source_views(report, sync::sync_view(connection).ok()),
        cost,
    })
}

fn average_prior_elapsed_windows(
    connection: &Connection,
    local_now: &chrono::DateTime<Local>,
    days: i64,
) -> Result<f64> {
    if days <= 0 {
        return Ok(0.0);
    }

    let today = local_now.date_naive();
    let elapsed = local_now.signed_duration_since(local_midnight(today)?);
    let mut total = 0_i64;
    for offset in 1..=days {
        let date = today - Duration::days(offset);
        let start = local_midnight(date)?;
        let next_start = local_midnight(date + Duration::days(1))?;
        let matching_end = (start + elapsed).min(next_start);
        let end_ms = matching_end
            .timestamp_millis()
            .saturating_add(1)
            .min(next_start.timestamp_millis());
        total += sum_tokens_between(connection, start.timestamp_millis(), end_ms)?;
    }
    Ok(total as f64 / days as f64)
}

fn period_bucket_count(period: &str, current_hour: u32) -> usize {
    match period {
        "week" => 7,
        "month" => 30,
        _ => current_hour.min(23) as usize + 1,
    }
}

fn load_events(connection: &Connection, start_ms: i64, end_ms: i64) -> Result<Vec<StoredEvent>> {
    // 远端同步事件只带处理量总数，不带模型或分量拆解（见架构约束：同步导出
    // 只含派生统计字段）；用 NULL/0 补齐列，让这些事件归入 "unknown" 模型、
    // 分量记 0，而不是被丢弃。
    let mut statement = connection.prepare(
        "SELECT adapter_id, occurred_at_ms, processed_tokens, model,
                input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens
         FROM usage_event
         WHERE occurred_at_ms >= ?1 AND occurred_at_ms < ?2
         UNION ALL
         SELECT adapter_id, occurred_at_ms, processed_tokens, NULL AS model,
                0, 0, 0, 0
         FROM remote_usage_event
         WHERE occurred_at_ms >= ?1 AND occurred_at_ms < ?2
         ORDER BY occurred_at_ms",
    )?;
    let rows = statement.query_map(params![start_ms, end_ms], |row| {
        Ok(StoredEvent {
            adapter: row.get(0)?,
            timestamp: row.get(1)?,
            tokens: row.get(2)?,
            model: row.get(3)?,
            input_uncached: row.get(4)?,
            cache_read: row.get(5)?,
            cache_write: row.get(6)?,
            output: row.get(7)?,
        })
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

fn sum_tokens_between(connection: &Connection, start_ms: i64, end_ms: i64) -> Result<i64> {
    connection
        .query_row(
            "SELECT (SELECT COALESCE(SUM(processed_tokens), 0) FROM usage_event
                     WHERE occurred_at_ms >= ?1 AND occurred_at_ms < ?2)
                  + (SELECT COALESCE(SUM(processed_tokens), 0) FROM remote_usage_event
                     WHERE occurred_at_ms >= ?1 AND occurred_at_ms < ?2)",
            params![start_ms, end_ms],
            |row| row.get(0),
        )
        .context("failed to calculate comparison usage")
}

fn quota_window_rank(key: &str) -> (u8, String) {
    match key {
        "five_hour" | "primary" => (0, String::new()),
        "seven_day" | "secondary" => (1, String::new()),
        // 超额付费是套餐外的补充预算，排在全部套餐窗口之后。
        "extra_usage" => (3, String::new()),
        other => (2, other.to_owned()),
    }
}

fn quota_window_label(adapter_id: &str, key: &str) -> String {
    // Antigravity 的窗口键是官方桶标识（如 gemini_weekly），原样展示。
    if adapter_id == "antigravity" {
        return key.replace('_', " ");
    }
    match key {
        "five_hour" | "primary" => "Session".into(),
        "seven_day" | "secondary" => {
            if adapter_id == "claude" {
                "每周 · 全模型".into()
            } else {
                "每周".into()
            }
        }
        // Claude 套餐外按量付费的已用比例（月度预算）。
        "extra_usage" => "超额用量".into(),
        other => {
            let model = other.strip_prefix("seven_day_").unwrap_or(other);
            let mut chars = model.chars();
            let pretty = chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_else(|| other.to_owned());
            format!("每周 · {pretty}")
        }
    }
}

/// 按短窗 → 长窗 → 其余（字母序）返回一个 Agent 的全部官方窗口。
fn load_agent_quota_windows(
    connection: &Connection,
    adapter_id: &str,
) -> Result<Vec<crate::domain::AgentQuotaWindow>> {
    let mut statement =
        connection.prepare("SELECT window_key FROM quota_snapshot WHERE adapter_id = ?1")?;
    let mut keys = statement
        .query_map([adapter_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    keys.sort_by_key(|key| quota_window_rank(key));

    keys.into_iter()
        .map(|key| {
            Ok(crate::domain::AgentQuotaWindow {
                label: quota_window_label(adapter_id, &key),
                view: load_quota(connection, adapter_id, &key)?,
                key,
            })
        })
        .collect()
}

fn load_quota(connection: &Connection, adapter_id: &str, window_key: &str) -> Result<QuotaView> {
    let row = connection.query_row(
        "SELECT remaining_percent, resets_at_ms, source_label, quality, collected_at_ms
         FROM quota_snapshot WHERE adapter_id = ?2 AND window_key = ?1",
        params![window_key, adapter_id],
        |row| {
            Ok((
                row.get::<_, f64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        },
    );

    match row {
        Ok((remaining, reset, source, quality, collected_at_ms)) => {
            let now = Utc::now().timestamp_millis();
            let age_minutes = ((now - collected_at_ms).max(0) as f64) / 60_000.0;
            let reset_expired = reset.is_some_and(|value| value <= now);
            let stale_after_minutes = if quality == "official_live" {
                7.0
            } else {
                15.0
            };
            Ok(QuotaView {
                available: true,
                remaining_percent: remaining,
                resets_in_minutes: reset
                    .filter(|value| *value > now)
                    .map(|value| (value - now) as f64 / 60_000.0),
                age_minutes: Some(age_minutes),
                stale: age_minutes > stale_after_minutes || reset_expired,
                reset_expired,
                source_label: source,
                quality,
            })
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(QuotaView {
            available: false,
            remaining_percent: 0.0,
            resets_in_minutes: None,
            age_minutes: None,
            stale: false,
            reset_expired: false,
            source_label: "暂无可靠来源".into(),
            quality: "unavailable".into(),
        }),
        Err(error) => Err(error.into()),
    }
}

fn source_views(report: ScanReport, sync_status: Option<SyncView>) -> Vec<SourceView> {
    let discovered = |id: &str| report.discovered.get(id).copied().unwrap_or(0);
    let refreshed = |id: &str| report.refreshed.get(id).copied().unwrap_or(0);
    let errors = |id: &str| report.errors.get(id).copied().unwrap_or(0);
    let diagnostics = |id: &str| report.diagnostics.get(id).cloned().unwrap_or_default();
    let codex_diagnostics = diagnostics("codex");
    let claude_diagnostics = diagnostics("claude");
    let opencode_diagnostics = diagnostics("opencode");
    let kimi_diagnostics = diagnostics("kimi");
    let codex_partial = codex_diagnostics.partial_sources > 0 || errors("codex") > 0;
    let claude_partial = claude_diagnostics.partial_sources > 0 || errors("claude") > 0;
    // 读不了的存储形态（如 OpenCode 1.2+ 的 SQLite）也是覆盖缺口：
    // 此时的 0 是"读不到"而非"没用过"，必须标部分覆盖。
    let opencode_gaps = report
        .coverage_gaps
        .get("opencode")
        .cloned()
        .unwrap_or_default();
    let opencode_partial = opencode_diagnostics.partial_sources > 0
        || errors("opencode") > 0
        || !opencode_gaps.is_empty();
    let kimi_partial = kimi_diagnostics.partial_sources > 0 || errors("kimi") > 0;
    let mut views = vec![
        SourceView {
            id: "codex-quota".into(),
            kind: "official".into(),
            label: "ChatGPT / Codex 官方配额".into(),
            detail: "采集主、次官方滚动窗口；桌面小插件仅展示主短窗，完整视图同时展示两者。优先读取本机 ChatGPT / Codex app-server，失败时使用带时间标记的日志快照。".into(),
            quality: "official".into(),
            quality_label: "官方".into(),
        },
        SourceView {
            id: "codex-local".into(),
            kind: "local".into(),
            label: "ChatGPT / Codex 本地 Token".into(),
            detail: format!(
                "发现 {} 个近期会话，本次更新 {} 个。{}累计快照按正增量入账；总量包含未缓存输入、缓存读取与输出，子项不重复相加。",
                discovered("codex"),
                refreshed("codex"),
                coverage_detail(&codex_diagnostics, errors("codex"))
            ),
            quality: if codex_partial { "partial" } else { "exact" }.into(),
            quality_label: if codex_partial {
                "部分覆盖"
            } else {
                "精确解析"
            }
            .into(),
        },
        SourceView {
            id: "claude-local".into(),
            kind: "local".into(),
            label: "Claude Code 本地 Token".into(),
            detail: format!(
                "发现 {} 个近期会话，本次更新 {} 个。{}重复消息跨会话按消息标识合并；总量包含缓存读取，配额不推算。",
                discovered("claude"),
                refreshed("claude"),
                coverage_detail(&claude_diagnostics, errors("claude"))
            ),
            quality: if claude_partial { "partial" } else { "exact" }.into(),
            quality_label: if claude_partial {
                "部分覆盖"
            } else {
                "精确解析"
            }
            .into(),
        },
        SourceView {
            id: "zcode-local".into(),
            kind: "local".into(),
            label: "ZCode / GLM 本地 Token".into(),
            detail: format!(
                "发现 {} 个用量库，本次更新 {} 个。{}只读取 model_usage 统计表的逐请求计数，主会话与子代理均覆盖；不读取消息内容表。",
                discovered("zcode"),
                refreshed("zcode"),
                coverage_detail(&diagnostics("zcode"), errors("zcode"))
            ),
            quality: if diagnostics("zcode").partial_sources > 0 || errors("zcode") > 0 {
                "partial"
            } else {
                "exact"
            }
            .into(),
            quality_label: if diagnostics("zcode").partial_sources > 0 || errors("zcode") > 0 {
                "部分覆盖"
            } else {
                "精确解析"
            }
            .into(),
        },
        SourceView {
            id: "opencode-local".into(),
            kind: "local".into(),
            label: "OpenCode 本地 Token".into(),
            detail: format!(
                "发现 {} 条近期消息，本次更新 {} 条。{}{}读取消息 usage 字段并以消息标识去重；未安装 OpenCode 时保持为 0，不做推算。",
                discovered("opencode"),
                refreshed("opencode"),
                coverage_detail(&opencode_diagnostics, errors("opencode")),
                if opencode_gaps.is_empty() {
                    String::new()
                } else {
                    format!("{}。", opencode_gaps.join("；"))
                },
            ),
            quality: if opencode_partial { "partial" } else { "exact" }.into(),
            quality_label: if opencode_partial {
                "部分覆盖"
            } else {
                "精确解析"
            }
            .into(),
        },
        SourceView {
            id: "kimi-local".into(),
            kind: "local".into(),
            label: "Kimi 本地 Token".into(),
            detail: format!(
                "发现 {} 个 wire.jsonl，本次更新 {} 个。{}只计单轮增量（usageScope=turn）与旧版 StatusUpdate（按 message_id 取分量最大值）；未安装 Kimi 时保持为 0，不做推算。尚未在装有 Kimi 的机器上实机验收。",
                discovered("kimi"),
                refreshed("kimi"),
                coverage_detail(&kimi_diagnostics, errors("kimi"))
            ),
            quality: if kimi_partial { "partial" } else { "exact" }.into(),
            quality_label: if kimi_partial { "部分覆盖" } else { "精确解析" }.into(),
        },
        SourceView {
            id: "antigravity-live".into(),
            kind: "local".into(),
            label: "Antigravity 用量".into(),
            detail: format!(
                "发现 {} 个活跃会话，本次更新 {} 个。{}用量来自本机 language server 的实时 RPC（IDE 未运行时为 0，不估算）；按 responseId 去重。尚未在装有 Antigravity 的机器上实机验收。",
                discovered("antigravity").saturating_sub(1),
                refreshed("antigravity"),
                coverage_detail(&diagnostics("antigravity"), errors("antigravity"))
            ),
            quality: "exact".into(),
            quality_label: "精确解析".into(),
        },
    ];

    if let Some(sync_status) = sync_status.filter(|status| status.enabled) {
        let device_count = sync_status.devices.len();
        let remote_events: i64 = sync_status.devices.iter().map(|device| device.events).sum();
        let failed = sync_status.last_error.is_some();
        views.push(SourceView {
            id: "device-sync".into(),
            kind: "sync".into(),
            label: "多设备同步".into(),
            detail: match (&sync_status.last_error, device_count) {
                (Some(error), _) => format!("上次同步未完全成功：{error}。合并数字可能滞后，本机统计不受影响。"),
                (None, 0) => "已开启文件夹同步，尚未发现其他设备的导出。其他电脑指向同一文件夹后会自动合并。".into(),
                (None, count) => format!(
                    "已合并 {count} 台其他设备的 {remote_events} 条统计事件；导出只含事件标识、Agent、时间与 token 数，不含任何对话内容。"
                ),
            },
            quality: if failed { "partial" } else { "exact" }.into(),
            quality_label: if failed { "部分覆盖" } else { "精确解析" }.into(),
        });
    }

    views
}

fn coverage_detail(diagnostics: &AdapterDiagnostics, errors: usize) -> String {
    if diagnostics.partial_sources == 0 && errors == 0 {
        return String::new();
    }

    let skipped = (diagnostics.partial_sources > 0).then(|| {
        format!(
            "{} 个会话存在未计入的 JSONL 内容（格式异常 {} 行、文本读取失败 {} 行、身份冲突 {} 条）；",
            diagnostics.partial_sources,
            diagnostics.malformed_lines,
            diagnostics.unreadable_lines,
            diagnostics.rejected_events
        )
    });
    let failed = (errors > 0).then(|| format!("另有 {errors} 个会话未能完成更新；"));
    format!(
        "{}{}本周期总量仅覆盖成功解析的记录，可能不完整。",
        skipped.unwrap_or_default(),
        failed.unwrap_or_default()
    )
}

fn local_midnight(date: NaiveDate) -> Result<chrono::DateTime<Local>> {
    let naive = date
        .and_hms_opt(0, 0, 0)
        .context("invalid local midnight")?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .context("local midnight is unavailable")
}

fn bucket_label(period: &str, start: NaiveDate, index: usize) -> String {
    if period == "today" {
        return format!("{index:02}:00");
    }
    let date = start + Duration::days(index as i64);
    if period == "week" {
        return match date.weekday() {
            Weekday::Mon => "周一",
            Weekday::Tue => "周二",
            Weekday::Wed => "周三",
            Weekday::Thu => "周四",
            Weekday::Fri => "周五",
            Weekday::Sat => "周六",
            Weekday::Sun => "周日",
        }
        .into();
    }
    format!("{} 日", date.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_local_time(date: NaiveDate, hour: u32) -> chrono::DateTime<Local> {
        Local
            .from_local_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
            .single()
            .unwrap()
    }

    fn insert_test_usage(
        connection: &Connection,
        event_id: &str,
        occurred_at_ms: i64,
        tokens: i64,
    ) {
        connection
            .execute(
                "INSERT INTO usage_event (
                    event_id, adapter_id, event_key, occurred_at_ms, session_id,
                    model, input_uncached_tokens, cache_read_tokens,
                    cache_write_tokens, output_tokens, reasoning_tokens,
                    processed_tokens, quality, payload_hash
                 ) VALUES (?1, 'codex', ?1, ?2, 'session', NULL, ?3, 0, 0, 0, 0, ?3, 'exact', ?1)",
                params![event_id, occurred_at_ms, tokens],
            )
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_test_usage_full(
        connection: &Connection,
        event_id: &str,
        adapter_id: &str,
        occurred_at_ms: i64,
        model: Option<&str>,
        input_uncached: i64,
        cache_read: i64,
        cache_write: i64,
        output: i64,
    ) {
        let processed = input_uncached + cache_read + cache_write + output;
        connection
            .execute(
                "INSERT INTO usage_event (
                    event_id, adapter_id, event_key, occurred_at_ms, session_id,
                    model, input_uncached_tokens, cache_read_tokens,
                    cache_write_tokens, output_tokens, reasoning_tokens,
                    processed_tokens, quality, payload_hash
                 ) VALUES (?1, ?2, ?1, ?3, 'session', ?4, ?5, ?6, ?7, ?8, 0, ?9, 'exact', ?1)",
                params![
                    event_id,
                    adapter_id,
                    occurred_at_ms,
                    model,
                    input_uncached,
                    cache_read,
                    cache_write,
                    output,
                    processed,
                ],
            )
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_test_session_event(
        connection: &Connection,
        event_id: &str,
        adapter_id: &str,
        session_id: &str,
        occurred_at_ms: i64,
        model: Option<&str>,
        input_uncached: i64,
        cache_read: i64,
        cache_write: i64,
        output: i64,
    ) {
        let processed = input_uncached + cache_read + cache_write + output;
        connection
            .execute(
                "INSERT INTO usage_event (
                    event_id, adapter_id, event_key, occurred_at_ms, session_id,
                    model, input_uncached_tokens, cache_read_tokens,
                    cache_write_tokens, output_tokens, reasoning_tokens,
                    processed_tokens, quality, payload_hash
                 ) VALUES (?1, ?2, ?1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, 'exact', ?1)",
                params![
                    event_id,
                    adapter_id,
                    occurred_at_ms,
                    session_id,
                    model,
                    input_uncached,
                    cache_read,
                    cache_write,
                    output,
                    processed,
                ],
            )
            .unwrap();
    }

    #[test]
    fn sessions_aggregate_boundaries_and_component_sums() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);

        insert_test_session_event(
            &connection,
            "codex-s1-a",
            "codex",
            "sess-1",
            test_local_time(today, 8).timestamp_millis(),
            Some("gpt-5"),
            10,
            2,
            0,
            3,
        );
        insert_test_session_event(
            &connection,
            "codex-s1-b",
            "codex",
            "sess-1",
            test_local_time(today, 9).timestamp_millis(),
            Some("gpt-5"),
            5,
            0,
            1,
            2,
        );
        insert_test_session_event(
            &connection,
            "codex-s2",
            "codex",
            "sess-2",
            test_local_time(today, 7).timestamp_millis(),
            Some("gpt-5"),
            1,
            0,
            0,
            1,
        );

        let result = sessions_at(&connection, "today", local_now).unwrap();

        assert_eq!(result.total_sessions, 2);
        assert!(!result.truncated);
        let sess1 = result
            .sessions
            .iter()
            .find(|s| s.session_id == "sess-1")
            .unwrap();
        assert_eq!(sess1.event_count, 2);
        assert_eq!(sess1.input_uncached, 15);
        assert_eq!(sess1.cache_read, 2);
        assert_eq!(sess1.cache_write, 1);
        assert_eq!(sess1.output, 5);
        assert_eq!(sess1.tokens, 23);
        assert_eq!(sess1.start_ms, test_local_time(today, 8).timestamp_millis());
        assert_eq!(sess1.end_ms, test_local_time(today, 9).timestamp_millis());

        // end_ms 降序：sess-1（09:00）排在 sess-2（07:00）之前。
        assert_eq!(result.sessions[0].session_id, "sess-1");
        assert_eq!(result.sessions[1].session_id, "sess-2");
    }

    #[test]
    fn dominant_model_is_the_one_with_the_most_session_tokens() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);

        insert_test_session_event(
            &connection,
            "sess-model-a",
            "claude",
            "sess-multi",
            test_local_time(today, 8).timestamp_millis(),
            Some("claude-sonnet"),
            100,
            0,
            0,
            0,
        );
        insert_test_session_event(
            &connection,
            "sess-model-b",
            "claude",
            "sess-multi",
            test_local_time(today, 9).timestamp_millis(),
            Some("claude-opus"),
            5,
            0,
            0,
            0,
        );

        let result = sessions_at(&connection, "today", local_now).unwrap();
        let session = result
            .sessions
            .iter()
            .find(|s| s.session_id == "sess-multi")
            .unwrap();

        assert_eq!(session.model.as_deref(), Some("claude-sonnet"));
        assert_eq!(session.models, vec!["claude-sonnet", "claude-opus"]);
    }

    #[test]
    fn session_with_no_model_has_none_model_and_no_models() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);

        insert_test_session_event(
            &connection,
            "sess-nomodel",
            "codex",
            "sess-blank",
            test_local_time(today, 8).timestamp_millis(),
            None,
            10,
            0,
            0,
            0,
        );

        let result = sessions_at(&connection, "today", local_now).unwrap();
        let session = result
            .sessions
            .iter()
            .find(|s| s.session_id == "sess-blank")
            .unwrap();

        assert_eq!(session.model, None);
        assert!(session.models.is_empty());
        assert_eq!(session.usd, None);
    }

    #[test]
    fn session_cost_matches_pricing_module_and_isolates_unpriced_models() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);

        // priced model.
        insert_test_session_event(
            &connection,
            "sess-priced",
            "codex",
            "sess-cost",
            test_local_time(today, 8).timestamp_millis(),
            Some("gpt-5"),
            2_000_000,
            0,
            0,
            1_000_000,
        );

        let result = sessions_at(&connection, "today", local_now).unwrap();
        let session = result
            .sessions
            .iter()
            .find(|s| s.session_id == "sess-cost")
            .unwrap();
        let expected_usd = 2.0 * 1.25 + 1.0 * 10.0;
        assert!((session.usd.unwrap() - expected_usd).abs() < 1e-9);

        // 混合已计价与未计价模型：usd 只求和已计价部分，仍为 Some。
        let connection2 = Connection::open_in_memory().unwrap();
        connection2
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        insert_test_session_event(
            &connection2,
            "sess-mixed-priced",
            "claude",
            "sess-mixed",
            test_local_time(today, 8).timestamp_millis(),
            Some("claude-sonnet-4-5"),
            10,
            0,
            0,
            10,
        );
        insert_test_session_event(
            &connection2,
            "sess-mixed-unpriced",
            "claude",
            "sess-mixed",
            test_local_time(today, 9).timestamp_millis(),
            // 订阅 coding-plan 专属 ID，没有官方按 token 价目（见 pricing.rs）。
            Some("GLM-5.2"),
            10,
            0,
            0,
            10,
        );
        let mixed_result = sessions_at(&connection2, "today", local_now).unwrap();
        let mixed = mixed_result
            .sessions
            .iter()
            .find(|s| s.session_id == "sess-mixed")
            .unwrap();
        assert!(mixed.usd.is_some());
        assert!(mixed.usd.unwrap() > 0.0);

        // 全部未计价：usd 必须是 None，不能伪造成 0。
        let connection3 = Connection::open_in_memory().unwrap();
        connection3
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        insert_test_session_event(
            &connection3,
            "sess-all-unpriced",
            "claude",
            "sess-none",
            test_local_time(today, 8).timestamp_millis(),
            // 同上：GLM-5.2 是订阅专属 ID，必须保持未计价。
            Some("GLM-5.2"),
            10,
            0,
            0,
            10,
        );
        let unpriced_result = sessions_at(&connection3, "today", local_now).unwrap();
        let unpriced = unpriced_result
            .sessions
            .iter()
            .find(|s| s.session_id == "sess-none")
            .unwrap();
        assert_eq!(unpriced.usd, None);
    }

    #[test]
    fn blank_session_id_merges_into_a_synthetic_unknown_session_per_agent() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);

        insert_test_session_event(
            &connection,
            "codex-blank-a",
            "codex",
            "",
            test_local_time(today, 8).timestamp_millis(),
            Some("gpt-5"),
            10,
            0,
            0,
            0,
        );
        insert_test_session_event(
            &connection,
            "codex-blank-b",
            "codex",
            "   ",
            test_local_time(today, 9).timestamp_millis(),
            Some("gpt-5"),
            5,
            0,
            0,
            0,
        );
        insert_test_session_event(
            &connection,
            "claude-blank",
            "claude",
            "",
            test_local_time(today, 8).timestamp_millis(),
            Some("claude-sonnet"),
            1,
            0,
            0,
            0,
        );

        let result = sessions_at(&connection, "today", local_now).unwrap();

        let codex_unknown = result
            .sessions
            .iter()
            .find(|s| s.agent == "codex" && s.session_id == "unknown")
            .unwrap();
        assert_eq!(codex_unknown.event_count, 2);
        assert_eq!(codex_unknown.input_uncached, 15);

        let claude_unknown = result
            .sessions
            .iter()
            .find(|s| s.agent == "claude" && s.session_id == "unknown")
            .unwrap();
        assert_eq!(claude_unknown.event_count, 1);

        // 两个 agent 的 unknown 会话是分开的合成会话，不合并。
        assert_eq!(result.total_sessions, 2);
    }

    #[test]
    fn sessions_are_truncated_at_300_with_accurate_total_count() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);

        for index in 0..305 {
            insert_test_session_event(
                &connection,
                &format!("sess-event-{index}"),
                "codex",
                &format!("sess-{index}"),
                test_local_time(today, 8).timestamp_millis() + index,
                Some("gpt-5"),
                1,
                0,
                0,
                0,
            );
        }

        let result = sessions_at(&connection, "today", local_now).unwrap();

        assert_eq!(result.total_sessions, 305);
        assert!(result.truncated);
        assert_eq!(result.sessions.len(), 300);
    }

    #[test]
    fn skipped_jsonl_lines_downgrade_only_the_affected_source_payload() {
        let mut report = ScanReport::default();
        report.discovered.insert("codex".into(), 2);
        report.refreshed.insert("codex".into(), 1);
        record_scan_diagnostics(
            &mut report,
            "codex",
            &ScanDiagnostics {
                malformed_lines: 2,
                unreadable_lines: 1,
                ..Default::default()
            },
        );

        let views = source_views(report, None);
        let codex = views
            .iter()
            .find(|source| source.id == "codex-local")
            .unwrap();
        let claude = views
            .iter()
            .find(|source| source.id == "claude-local")
            .unwrap();

        assert_eq!(codex.quality, "partial");
        assert_eq!(codex.quality_label, "部分覆盖");
        assert!(codex.detail.contains("格式异常 2 行"));
        assert!(codex.detail.contains("文本读取失败 1 行"));
        assert!(codex.detail.contains("可能不完整"));
        assert_eq!(claude.quality, "exact");
        assert_eq!(claude.quality_label, "精确解析");
    }

    #[test]
    fn source_refresh_error_is_not_presented_as_exact() {
        let mut report = ScanReport::default();
        report.errors.insert("claude".into(), 1);

        let views = source_views(report, None);
        let claude = views
            .iter()
            .find(|source| source.id == "claude-local")
            .unwrap();

        assert_eq!(claude.quality, "partial");
        assert!(claude.detail.contains("1 个会话未能完成更新"));
    }

    #[test]
    fn unreadable_store_marks_opencode_partial_with_the_reason() {
        // OpenCode 1.2+ 的 SQLite 库读不了：0 是"读不到"不是"没用过"，
        // 必须标部分覆盖并把原因展示出来，不许显示成精确的 0。
        let mut report = ScanReport::default();
        report.coverage_gaps.insert(
            "opencode".into(),
            vec!["检测到 OpenCode 1.2+ 的 SQLite 存储（opencode.db），当前版本尚不支持读取，其中的会话未计入统计".into()],
        );

        let views = source_views(report, None);
        let opencode = views
            .iter()
            .find(|source| source.id == "opencode-local")
            .unwrap();

        assert_eq!(opencode.quality, "partial");
        assert_eq!(opencode.quality_label, "部分覆盖");
        assert!(opencode.detail.contains("SQLite"));
        assert!(opencode.detail.contains("尚不支持读取"));
    }

    #[test]
    fn rejected_claude_event_is_presented_as_partial_coverage() {
        let mut report = ScanReport::default();
        report.discovered.insert("claude".into(), 1);
        report.refreshed.insert("claude".into(), 1);
        record_scan_diagnostics(
            &mut report,
            "claude",
            &ScanDiagnostics {
                rejected_events: 1,
                ..Default::default()
            },
        );

        let claude = source_views(report, None)
            .into_iter()
            .find(|source| source.id == "claude-local")
            .unwrap();

        assert_eq!(claude.quality, "partial");
        assert!(claude.detail.contains("身份冲突 1 条"));
        assert!(claude.detail.contains("可能不完整"));
    }

    #[test]
    fn persisted_diagnostics_survive_an_unchanged_source_scan() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE scan_source (
                    source_id TEXT PRIMARY KEY,
                    adapter_id TEXT NOT NULL,
                    last_error TEXT
                );",
            )
            .unwrap();
        let marker = ScanDiagnostics {
            malformed_lines: 3,
            unreadable_lines: 1,
            ..Default::default()
        }
        .storage_marker()
        .unwrap();
        connection
            .execute(
                "INSERT INTO scan_source (source_id, adapter_id, last_error)
                 VALUES ('source-a', 'codex', ?1)",
                [marker],
            )
            .unwrap();

        let stored = stored_adapter_scan_diagnostics(&connection, "codex").unwrap();

        assert_eq!(
            stored.get("source-a"),
            Some(&ScanDiagnostics {
                malformed_lines: 3,
                unreadable_lines: 1,
                ..Default::default()
            })
        );
    }

    #[test]
    fn today_series_stops_at_the_current_hour() {
        assert_eq!(period_bucket_count("today", 0), 1);
        assert_eq!(period_bucket_count("today", 11), 12);
        assert_eq!(period_bucket_count("today", 23), 24);
    }

    #[test]
    fn week_and_month_bucket_counts_remain_fixed() {
        assert_eq!(period_bucket_count("week", 0), 7);
        assert_eq!(period_bucket_count("week", 23), 7);
        assert_eq!(period_bucket_count("month", 0), 30);
        assert_eq!(period_bucket_count("month", 23), 30);
    }

    /// 解析视界固定为保留期，不跟随 UI 周期。按视界解析过的源在之后的任何时刻、
    /// 任何周期下都算 current——切到「30 天」不会再触发整份重扫。
    #[test]
    fn a_source_parsed_at_the_retention_horizon_is_never_reparsed_by_a_period_switch() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let parsed_at = 1_800_000_000_000_i64;
        let horizon_at_parse = parsed_at - Duration::days(RETENTION_DAYS).num_milliseconds();
        connection
            .execute(
                "INSERT INTO scan_source (
                     source_id, adapter_id, logical_key, locator, observed_size,
                     mtime_ns, coverage_start_ms, parser_version, last_success_ms, last_error
                 ) VALUES ('source', 'codex', 'source', 'rollout.jsonl', 10, 1, ?1, ?2, ?3, NULL)",
                params![horizon_at_parse, storage::PARSER_VERSION, parsed_at],
            )
            .unwrap();

        // 一周后再取快照：视界随 now 前移，旧覆盖仍然完全包住它。
        let later = parsed_at + Duration::days(7).num_milliseconds();
        let horizon_now = later - Duration::days(RETENTION_DAYS).num_milliseconds();
        assert!(storage::source_is_current(&connection, "source", 10, 1, horizon_now).unwrap());
    }

    #[test]
    fn today_comparison_uses_the_same_elapsed_window_from_each_prior_day() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);

        insert_test_usage(
            &connection,
            "today-before-cutoff",
            test_local_time(today, 11).timestamp_millis(),
            200,
        );
        for offset in 1..=7 {
            let date = today - Duration::days(offset);
            insert_test_usage(
                &connection,
                &format!("prior-{offset}-before-cutoff"),
                test_local_time(date, 10).timestamp_millis(),
                100,
            );
            insert_test_usage(
                &connection,
                &format!("prior-{offset}-after-cutoff"),
                test_local_time(date, 18).timestamp_millis(),
                900,
            );
        }

        let snapshot =
            query_snapshot_at(&connection, "today", ScanReport::default(), local_now).unwrap();

        assert_eq!(snapshot.total_tokens, 200);
        assert!(snapshot.comparison_available);
        assert!((snapshot.comparison_percent - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn model_and_component_aggregation_across_multiple_agents() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);
        let at = test_local_time(today, 10).timestamp_millis();

        insert_test_usage_full(
            &connection,
            "codex-gpt5-a",
            "codex",
            at,
            Some("gpt-5"),
            10,
            2,
            0,
            3,
        );
        insert_test_usage_full(
            &connection,
            "codex-gpt5-b",
            "codex",
            at,
            Some("gpt-5"),
            5,
            0,
            1,
            2,
        );
        insert_test_usage_full(
            &connection,
            "claude-sonnet",
            "claude",
            at,
            Some("claude-sonnet"),
            7,
            1,
            0,
            4,
        );
        insert_test_usage_full(
            &connection,
            "claude-unknown",
            "claude",
            at,
            None,
            1,
            0,
            0,
            1,
        );
        insert_test_usage_full(
            &connection,
            "claude-blank",
            "claude",
            at,
            Some("   "),
            0,
            0,
            0,
            1,
        );

        let snapshot =
            query_snapshot_at(&connection, "today", ScanReport::default(), local_now).unwrap();

        // 分量求和正确：codex 两条 gpt-5 事件的各分量分别相加。
        let codex_agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.id == "codex")
            .unwrap();
        assert_eq!(codex_agent.input_uncached, 15);
        assert_eq!(codex_agent.cache_read, 2);
        assert_eq!(codex_agent.cache_write, 1);
        assert_eq!(codex_agent.output, 5);
        assert_eq!(codex_agent.tokens, 23);
        assert_eq!(
            codex_agent.input_uncached
                + codex_agent.cache_read
                + codex_agent.cache_write
                + codex_agent.output,
            codex_agent.tokens
        );

        let claude_agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.id == "claude")
            .unwrap();
        assert_eq!(claude_agent.input_uncached, 8);
        assert_eq!(claude_agent.cache_read, 1);
        assert_eq!(claude_agent.output, 6);

        // 多模型多 agent 聚合正确：同名模型跨事件合并，不同 agent 分开列出。
        let codex_gpt5 = snapshot
            .models
            .iter()
            .find(|entry| entry.agent == "codex" && entry.model == "gpt-5")
            .unwrap();
        assert_eq!(codex_gpt5.tokens, 23);
        let claude_sonnet = snapshot
            .models
            .iter()
            .find(|entry| entry.agent == "claude" && entry.model == "claude-sonnet")
            .unwrap();
        assert_eq!(claude_sonnet.tokens, 12);

        // 空 model（NULL 或空白字符串）归入 unknown，不丢弃事件。
        let claude_unknown = snapshot
            .models
            .iter()
            .find(|entry| entry.agent == "claude" && entry.model == "unknown")
            .unwrap();
        assert_eq!(claude_unknown.tokens, 3);

        // 按 tokens 降序排列。
        for pair in snapshot.models.windows(2) {
            assert!(pair[0].tokens >= pair[1].tokens);
        }

        let total_model_tokens: i64 = snapshot.models.iter().map(|entry| entry.tokens).sum();
        assert_eq!(total_model_tokens, snapshot.total_tokens);
    }

    #[test]
    fn cost_summary_prices_known_models_and_isolates_unpriced_tokens() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);
        let at = test_local_time(today, 10).timestamp_millis();

        // codex: priced gpt-5 usage (1.25 / 0.125 / 0 / 10.0 per million).
        insert_test_usage_full(
            &connection,
            "codex-gpt5",
            "codex",
            at,
            Some("gpt-5"),
            2_000_000,
            0,
            0,
            1_000_000,
        );
        // codex: an unpriced model must not be silently priced.
        insert_test_usage_full(
            &connection,
            "codex-custom",
            "codex",
            at,
            Some("custom-internal-model"),
            30,
            0,
            0,
            10,
        );
        // codex: "gpt-5.2-codex" must win over the shorter "gpt-5" prefix.
        insert_test_usage_full(
            &connection,
            "codex-gpt52codex",
            "codex",
            at,
            Some("gpt-5.2-codex"),
            1_000_000,
            0,
            0,
            0,
        );
        // claude: dated snapshot resolves via longest-prefix match onto
        // "claude-sonnet-4-5".
        insert_test_usage_full(
            &connection,
            "claude-sonnet",
            "claude",
            at,
            Some("claude-sonnet-4-5-20250929"),
            1_000_000,
            1_000_000,
            1_000_000,
            1_000_000,
        );
        // claude: GLM-5.2 是订阅 coding-plan 专属 ID，没有官方按 token 价目，
        // 必须保持未计价（见 pricing.rs 的覆盖范围说明）。
        insert_test_usage_full(
            &connection,
            "claude-glm",
            "claude",
            at,
            Some("GLM-5.2"),
            100,
            0,
            0,
            50,
        );

        let snapshot =
            query_snapshot_at(&connection, "today", ScanReport::default(), local_now).unwrap();

        assert!(snapshot.cost.available);
        assert_eq!(snapshot.cost.pricing_as_of, pricing::PRICING_AS_OF);

        let expected_gpt5_usd = 2.0 * 1.25 + 1.0 * 10.0; // 12.5
        let expected_gpt52codex_usd = 1.0 * 1.75; // not gpt-5's 1.25
        let expected_claude_sonnet_usd = 1.0 * 3.0 + 1.0 * 0.3 + 1.0 * 3.75 + 1.0 * 15.0; // 22.05
        let expected_total_usd =
            expected_gpt5_usd + expected_gpt52codex_usd + expected_claude_sonnet_usd;
        let expected_unpriced_tokens = 40 + 150; // custom-internal-model + glm-4.7 processed totals

        assert!((snapshot.cost.total_usd - expected_total_usd).abs() < 1e-9);
        assert_eq!(snapshot.cost.unpriced_tokens, expected_unpriced_tokens);

        let codex_cost = snapshot
            .cost
            .by_agent
            .iter()
            .find(|entry| entry.agent == "codex")
            .unwrap();
        assert!((codex_cost.usd - (expected_gpt5_usd + expected_gpt52codex_usd)).abs() < 1e-9);
        assert_eq!(codex_cost.unpriced_tokens, 40);

        let claude_cost = snapshot
            .cost
            .by_agent
            .iter()
            .find(|entry| entry.agent == "claude")
            .unwrap();
        assert!((claude_cost.usd - expected_claude_sonnet_usd).abs() < 1e-9);
        assert_eq!(claude_cost.unpriced_tokens, 150);

        let by_agent_total_usd: f64 = snapshot.cost.by_agent.iter().map(|entry| entry.usd).sum();
        let by_agent_total_unpriced: i64 = snapshot
            .cost
            .by_agent
            .iter()
            .map(|entry| entry.unpriced_tokens)
            .sum();
        assert!((by_agent_total_usd - snapshot.cost.total_usd).abs() < 1e-9);
        assert_eq!(by_agent_total_unpriced, snapshot.cost.unpriced_tokens);
    }

    #[test]
    fn week_comparison_matches_elapsed_time_on_the_previous_final_day() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);
        insert_test_usage(
            &connection,
            "current-week-evening",
            test_local_time(today - Duration::days(1), 18).timestamp_millis(),
            200,
        );
        insert_test_usage(
            &connection,
            "previous-week-before-cutoff",
            test_local_time(today - Duration::days(7), 10).timestamp_millis(),
            100,
        );
        insert_test_usage(
            &connection,
            "previous-week-after-cutoff",
            test_local_time(today - Duration::days(7), 18).timestamp_millis(),
            900,
        );

        let snapshot =
            query_snapshot_at(&connection, "week", ScanReport::default(), local_now).unwrap();

        assert_eq!(snapshot.total_tokens, 200);
        assert!(snapshot.comparison_available);
        assert!((snapshot.comparison_percent - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn month_comparison_matches_elapsed_time_on_the_previous_final_day() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);
        insert_test_usage(
            &connection,
            "current-month-evening",
            test_local_time(today - Duration::days(1), 18).timestamp_millis(),
            200,
        );
        insert_test_usage(
            &connection,
            "previous-month-before-cutoff",
            test_local_time(today - Duration::days(30), 10).timestamp_millis(),
            100,
        );
        insert_test_usage(
            &connection,
            "previous-month-after-cutoff",
            test_local_time(today - Duration::days(30), 18).timestamp_millis(),
            900,
        );

        let snapshot =
            query_snapshot_at(&connection, "month", ScanReport::default(), local_now).unwrap();

        assert_eq!(snapshot.total_tokens, 200);
        assert!(snapshot.comparison_available);
        assert!((snapshot.comparison_percent - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn expired_quota_snapshot_is_marked_stale() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let now = Utc::now().timestamp_millis();
        connection
            .execute(
                "INSERT INTO quota_snapshot (
                    adapter_id, window_key, remaining_percent, resets_at_ms,
                    collected_at_ms, quality, source_label
                 ) VALUES ('codex', 'primary', 72, ?1, ?2, 'official_snapshot', 'log')",
                params![now - 1_000, now - Duration::minutes(30).num_milliseconds()],
            )
            .unwrap();

        let quota = load_quota(&connection, "codex", "primary").unwrap();
        assert!(quota.available);
        assert!(quota.stale);
        assert!(quota.reset_expired);
        assert!(quota.resets_in_minutes.is_none());
        assert!(quota.age_minutes.unwrap() >= 29.0);
    }

    #[test]
    fn recent_live_quota_is_fresh() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let now = Utc::now().timestamp_millis();
        connection
            .execute(
                "INSERT INTO quota_snapshot (
                    adapter_id, window_key, remaining_percent, resets_at_ms,
                    collected_at_ms, quality, source_label
                 ) VALUES ('codex', 'primary', 72, ?1, ?2, 'official_live', 'app-server')",
                params![now + Duration::hours(2).num_milliseconds(), now],
            )
            .unwrap();

        let quota = load_quota(&connection, "codex", "primary").unwrap();
        assert!(quota.available);
        assert!(!quota.stale);
        assert!(!quota.reset_expired);
        assert!(quota.resets_in_minutes.unwrap() > 119.0);
    }

    #[test]
    fn live_quota_stays_fresh_across_the_compact_refresh_interval() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let now = Utc::now().timestamp_millis();
        connection
            .execute(
                "INSERT INTO quota_snapshot (
                    adapter_id, window_key, remaining_percent, resets_at_ms,
                    collected_at_ms, quality, source_label
                 ) VALUES ('codex', 'primary', 72, ?1, ?2, 'official_live', 'app-server')",
                params![
                    now + Duration::hours(2).num_milliseconds(),
                    now - Duration::minutes(5).num_milliseconds()
                ],
            )
            .unwrap();

        let quota = load_quota(&connection, "codex", "primary").unwrap();
        assert!(!quota.stale);
    }

    #[test]
    fn secondary_quota_window_is_loaded_independently() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let now = Utc::now().timestamp_millis();
        connection
            .execute(
                "INSERT INTO quota_snapshot (
                    adapter_id, window_key, remaining_percent, resets_at_ms,
                    collected_at_ms, quality, source_label
                 ) VALUES ('codex', 'primary', 72, ?1, ?2, 'official_live', 'app-server'),
                          ('codex', 'secondary', 83, ?3, ?2, 'official_live', 'app-server')",
                params![
                    now + Duration::hours(2).num_milliseconds(),
                    now,
                    now + Duration::days(6).num_milliseconds()
                ],
            )
            .unwrap();

        let primary = load_quota(&connection, "codex", "primary").unwrap();
        let secondary = load_quota(&connection, "codex", "secondary").unwrap();
        assert_eq!(primary.remaining_percent, 72.0);
        assert_eq!(secondary.remaining_percent, 83.0);
        assert!(!secondary.stale);
        assert!(secondary.resets_in_minutes.unwrap() > 8_639.0);
        assert_eq!(secondary.source_label, "app-server");
    }

    #[test]
    fn missing_secondary_quota_is_not_filled_from_primary() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let now = Utc::now().timestamp_millis();
        connection
            .execute(
                "INSERT INTO quota_snapshot (
                    adapter_id, window_key, remaining_percent, resets_at_ms,
                    collected_at_ms, quality, source_label
                 ) VALUES ('codex', 'primary', 72, ?1, ?2, 'official_live', 'app-server')",
                params![now + Duration::hours(2).num_milliseconds(), now],
            )
            .unwrap();

        let secondary = load_quota(&connection, "codex", "secondary").unwrap();
        assert!(!secondary.available);
        assert_eq!(secondary.remaining_percent, 0.0);
        assert_eq!(secondary.quality, "unavailable");
    }

    #[test]
    #[ignore = "reads the current user's local Codex and Claude Code logs"]
    fn live_snapshot_smoke_test() {
        let database = std::env::temp_dir().join(format!(
            "metrik-live-smoke-{}-{}.sqlite3",
            std::process::id(),
            Utc::now().timestamp_millis()
        ));
        let quota_cache = Mutex::new(None);
        let claude_quota_cache = Mutex::new(None);
        let http_quota_cache = Mutex::new(HashMap::new());
        let snapshot = build_snapshot(
            &database,
            "today",
            &quota_cache,
            &claude_quota_cache,
            &http_quota_cache,
        )
        .unwrap();
        println!(
            "live snapshot: total={}, codex={}, claude={}, quota_available={}, quota_remaining={:.1}, quota_source={}",
            snapshot.total_tokens,
            snapshot.agents[0].tokens,
            snapshot.agents[1].tokens,
            snapshot.agent_quotas[0].windows.first().map(|w| w.view.available).unwrap_or(false),
            snapshot.agent_quotas[0].windows.first().map(|w| w.view.remaining_percent).unwrap_or(0.0),
            snapshot.agent_quotas[0].windows.first().map(|w| w.view.source_label.clone()).unwrap_or_default()
        );
        assert!(!snapshot.is_demo);
        assert!((1..=24).contains(&snapshot.series.len()));
        let expected_last_label = format!("{:02}:00", snapshot.series.len() - 1);
        assert_eq!(
            snapshot.series.last().map(|point| point.label.as_str()),
            Some(expected_last_label.as_str())
        );
        std::fs::remove_file(database).ok();
    }

    #[test]
    fn report_is_empty_for_an_empty_ledger() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);

        let report = report_at(&connection, local_now).unwrap();

        assert!(report.days.is_empty());
        assert_eq!(report.first_event_ms, None);
        assert_eq!(report.last_event_ms, None);
        assert_eq!(report.total_tokens, 0);
        assert!(report.top_models.is_empty());
        assert_eq!(report.streak_days, 0);
        for agent in &report.agents {
            assert_eq!(agent.tokens, 0);
            assert_eq!(agent.active_days, 0);
        }
    }

    #[test]
    fn report_aggregates_multiple_days_and_agents_across_the_local_day_boundary() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);

        // Two events land in the same local day just either side of midnight.
        insert_test_usage_full(
            &connection,
            "codex-late",
            "codex",
            test_local_time(today - Duration::days(1), 23).timestamp_millis(),
            Some("gpt-5"),
            10,
            0,
            0,
            5,
        );
        insert_test_usage_full(
            &connection,
            "codex-early",
            "codex",
            test_local_time(today - Duration::days(1), 1).timestamp_millis(),
            Some("gpt-5"),
            4,
            0,
            0,
            1,
        );
        insert_test_usage_full(
            &connection,
            "claude-today",
            "claude",
            test_local_time(today, 9).timestamp_millis(),
            Some("claude-sonnet"),
            7,
            1,
            0,
            2,
        );

        let report = report_at(&connection, local_now).unwrap();

        assert_eq!(report.days.len(), 2);
        let yesterday_key = (today - Duration::days(1)).format("%Y-%m-%d").to_string();
        let today_key = today.format("%Y-%m-%d").to_string();
        let yesterday_row = report
            .days
            .iter()
            .find(|day| day.date == yesterday_key)
            .unwrap();
        assert_eq!(yesterday_row.tokens, 20);
        assert_eq!(yesterday_row.by_agent.get("codex"), Some(&20));
        let today_row = report
            .days
            .iter()
            .find(|day| day.date == today_key)
            .unwrap();
        assert_eq!(today_row.tokens, 10);
        assert_eq!(today_row.by_agent.get("claude"), Some(&10));

        assert_eq!(report.total_tokens, 30);

        let codex_agent = report.agents.iter().find(|a| a.id == "codex").unwrap();
        assert_eq!(codex_agent.tokens, 20);
        assert_eq!(codex_agent.active_days, 1);
        let claude_agent = report.agents.iter().find(|a| a.id == "claude").unwrap();
        assert_eq!(claude_agent.tokens, 10);
        assert_eq!(claude_agent.active_days, 1);

        let codex_model = report
            .top_models
            .iter()
            .find(|m| m.agent == "codex" && m.model == "gpt-5")
            .unwrap();
        assert_eq!(codex_model.tokens, 20);

        // Both days are consecutive, so the streak covers both.
        assert_eq!(report.streak_days, 2);
    }

    #[test]
    fn report_streak_stops_at_a_gap_and_counts_from_the_latest_active_day() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);

        // Active today, yesterday, day-before-yesterday, then a gap, then one
        // more isolated active day further back.
        for offset in [0_i64, 1, 2] {
            insert_test_usage(
                &connection,
                &format!("recent-{offset}"),
                test_local_time(today - Duration::days(offset), 10).timestamp_millis(),
                50,
            );
        }
        insert_test_usage(
            &connection,
            "isolated",
            test_local_time(today - Duration::days(10), 10).timestamp_millis(),
            50,
        );

        let report = report_at(&connection, local_now).unwrap();

        assert_eq!(report.streak_days, 3);
        assert_eq!(report.days.len(), 4);
    }

    #[test]
    fn report_first_and_last_event_ms_span_the_full_ledger_not_just_the_window() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        let local_now = test_local_time(today, 12);
        let earliest_ms = test_local_time(today, 8).timestamp_millis();
        let latest_ms = test_local_time(today, 11).timestamp_millis();

        insert_test_usage(&connection, "first", earliest_ms, 10);
        insert_test_usage(&connection, "last", latest_ms, 20);

        let report = report_at(&connection, local_now).unwrap();

        assert_eq!(report.first_event_ms, Some(earliest_ms));
        assert_eq!(report.last_event_ms, Some(latest_ms));
    }

    #[test]
    fn report_never_touches_scan_source_bookkeeping() {
        // report_at only issues SELECT queries against usage_event and
        // remote_usage_event; it must never discover, parse, or write
        // scan_source rows the way build_snapshot does.
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let today = Local::now().date_naive();
        insert_test_usage(
            &connection,
            "codex-a",
            test_local_time(today, 10).timestamp_millis(),
            10,
        );

        let report = report_at(&connection, test_local_time(today, 12)).unwrap();

        let scan_source_rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM scan_source", [], |row| row.get(0))
            .unwrap();
        assert_eq!(scan_source_rows, 0);
        assert_eq!(report.total_tokens, 10);
    }
}
