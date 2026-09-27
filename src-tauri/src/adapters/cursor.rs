//! Cursor 的 Token 消耗。
//!
//! 2026-02 之后的 Cursor 把逐次 `tokenCount` 写成 0/0，本机 `state.vscdb` 的
//! bubble 不能当账本（对话正文也不读）。数字与仪表盘同一份逐次事件：
//! `POST https://cursor.com/api/dashboard/get-filtered-usage-events`。
//! 鉴权只用 Cursor 已经写在 `ItemTable.cursorAuth/accessToken` 里的明文会话，
//! 当次请求后即丢弃，不写入账本、日志或诊断。
//!
//! 口径与仪表盘列一致，也与本账本其他 Agent 的分量一致：
//! - `inputTokens` → `input_uncached`（不含缓存；缓存另列）
//! - `cacheReadTokens` / `cacheWriteTokens` 分开计入
//! - `outputTokens` → `output`
//! - 事件没有推理子项，`reasoning_output` 保持 0，不重复加
//! - `totalCents` 是金额不是 token 总量，没有可核对的自报总量，不做
//!   `disagrees_with_reported_total`
//!
//! 事件是增量而不是累计行。接口没有 request id，身份用时间、会话、模型与
//! 分量；同一毫秒里这份指纹重复出现时按出现次序加后缀，避免并行调用被并成一条。
//! 65 天窗口若超过单次分页上限，就按时间对半切开再拉，不因条数多就整段放弃。
//! 每一段都必须拉完整才拼进结果：残缺结果若走 `replace_source` 会删掉这次没看到的旧事件。
//! 网络请求之间看扫描截止时间。没拉完只把进度留在内存（不含会话），下次快照续跑；
//! 整段窗口完成后才一次性入账。
//! 接口不带工作目录，用量不归入项目；未收录价目的模型保持未计价。没有官方配额窗口。

use super::{AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{stable_hash, ParsedSource, TokenVector, UsageEvent};
use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FILTERED_USAGE_URL: &str = "https://cursor.com/api/dashboard/get-filtered-usage-events";
const AUTH_ME_URL: &str = "https://cursor.com/api/auth/me";
/// 仪表盘事件不在本机文件里，不能用 state.vscdb 的 mtime（它几乎每次按键都变）。
/// 五分钟一个桶：桶内快照只做元数据比对，不重复打接口。
const REFRESH_MS: i64 = 5 * 60 * 1000;
const PAGE_SIZE: i64 = 1000;
const MAX_PAGES: i64 = 15;
/// 同一段时间里总数被新请求改写时，先整段重拉，仍对不上再把时间切开。
const RANGE_ATTEMPTS: u32 = 3;
const MAX_QUEUED_RANGES: usize = 256;
/// 剩余预算不够发下一次请求时先挂起。单次请求也不超过这个上限，避免一次套接字把扫描锁占满。
const MIN_SLICE: Duration = Duration::from_millis(200);
const MAX_REQUEST: Duration = Duration::from_secs(2);

static FETCH: Mutex<Option<FetchCheckpoint>> = Mutex::new(None);

pub struct CursorAdapter {
    state_db: PathBuf,
}

pub fn cursor_state_db() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_default()
        .join("Cursor")
        .join("User")
        .join("globalStorage")
        .join("state.vscdb")
}

impl CursorAdapter {
    pub fn detected() -> Self {
        Self {
            state_db: cursor_state_db(),
        }
    }

    #[cfg(test)]
    fn with_state_db(state_db: PathBuf) -> Self {
        Self { state_db }
    }
}

impl AgentAdapter for CursorAdapter {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn discover(&self, _cutoff_ms: i64) -> Vec<SourceCandidate> {
        if !self.state_db.exists() {
            return Vec::new();
        }
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_millis().min(i64::MAX as u128) as i64)
            .unwrap_or(0);
        let normalized = normalize_locator(&self.state_db);
        vec![SourceCandidate {
            source_id: stable_hash(&format!("cursor|{normalized}")),
            path: self.state_db.clone(),
            // 大小固定：刷新只跟时间桶走，不跟状态库体积走。
            size: 1,
            mtime_ns: refresh_bucket_ns(now_ms),
        }]
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        self.parse_until(
            candidate,
            cutoff_ms,
            Instant::now() + Duration::from_secs(180),
        )?
        .context("Cursor 用量未在单次解析时限内完成")
    }

    fn has_pending(&self, candidate: &SourceCandidate) -> bool {
        fetch_is_pending(candidate)
    }

    fn parse_until(
        &self,
        candidate: &SourceCandidate,
        cutoff_ms: i64,
        deadline: Instant,
    ) -> Result<Option<ParsedScan>> {
        let Some(pages) = drive_fetch(self.state_db.as_path(), candidate, cutoff_ms, Some(deadline))?
        else {
            return Ok(None);
        };
        Ok(Some(scan_from_pages(candidate, cutoff_ms, pages)))
    }

    fn coverage_gaps(&self) -> Vec<String> {
        if !self.state_db.exists() {
            return Vec::new();
        }
        match read_access_token(&self.state_db) {
            Ok(None) => vec![
                "检测到 Cursor，但本机没有登录会话，无法读取用量。当前版本写在本地的 tokenCount 多为 0，不能当作消耗".into(),
            ],
            Ok(Some(_)) | Err(_) => Vec::new(),
        }
    }
}

fn refresh_bucket_ns(now_ms: i64) -> i64 {
    now_ms.div_euclid(REFRESH_MS) * REFRESH_MS * 1_000_000
}

fn normalize_locator(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

struct ParsedPages {
    events: Vec<UsageEvent>,
    malformed: usize,
}

fn events_from_pages(pages: &[Value], cutoff_ms: i64) -> ParsedPages {
    let mut events = Vec::new();
    let mut occurrences: HashMap<String, u32> = HashMap::new();
    let mut malformed = 0usize;
    for page in pages {
        let Some(rows) = page.as_array() else {
            malformed += 1;
            continue;
        };
        for row in rows {
            match event_from_row(row, cutoff_ms) {
                RowOutcome::Event(event) => {
                    events.push(with_occurrence(event, &mut occurrences));
                }
                RowOutcome::Skip => {}
                RowOutcome::Malformed => malformed += 1,
            }
        }
    }
    ParsedPages { events, malformed }
}

/// 第一条保持原键，方便唯一事件在重拉时对上已有账本。第二条起加出现次序。
fn with_occurrence(event: UsageEvent, occurrences: &mut HashMap<String, u32>) -> UsageEvent {
    let base = event.event_key.clone();
    let occurrence = {
        let seen = occurrences.entry(base.clone()).or_insert(0);
        *seen += 1;
        *seen
    };
    if occurrence == 1 {
        return event;
    }
    UsageEvent::new(
        "cursor",
        format!("{base}#{occurrence}"),
        event.occurred_at_ms,
        event.session_id,
        event.model,
        event.tokens,
        event.quality,
    )
}

enum RowOutcome {
    Event(UsageEvent),
    Skip,
    Malformed,
}

fn event_from_row(row: &Value, cutoff_ms: i64) -> RowOutcome {
    let Some(row) = row.as_object() else {
        return RowOutcome::Malformed;
    };
    let Some(occurred_at_ms) = json_i64(row.get("timestamp")) else {
        return RowOutcome::Malformed;
    };
    if occurred_at_ms < cutoff_ms {
        return RowOutcome::Skip;
    }
    let usage = row.get("tokenUsage");
    let input = json_i64(usage.and_then(|value| value.get("inputTokens"))).unwrap_or(0);
    let output = json_i64(usage.and_then(|value| value.get("outputTokens"))).unwrap_or(0);
    let cache_read = json_i64(usage.and_then(|value| value.get("cacheReadTokens"))).unwrap_or(0);
    let cache_write = json_i64(usage.and_then(|value| value.get("cacheWriteTokens"))).unwrap_or(0);
    let tokens = TokenVector {
        input_uncached: input.max(0),
        cache_read: cache_read.max(0),
        cache_write: cache_write.max(0),
        output: output.max(0),
        reasoning_output: 0,
    };
    if tokens.processed() == 0 {
        return RowOutcome::Skip;
    }
    let model = row
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let session_id = row
        .get("conversationId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("cursor")
        .to_owned();
    // 分量写进事件键：同一次请求重拉时键不变；仪表盘若改写读数，旧键在整源替换时退出，不会加计两次。
    // 同毫秒的另一次调用由 `with_occurrence` 加后缀，不在这里丢掉。
    let event_key = format!(
        "cursor:{occurred_at_ms}|{session_id}|{}|{}|{}|{}|{}",
        model.as_deref().unwrap_or(""),
        tokens.input_uncached,
        tokens.output,
        tokens.cache_read,
        tokens.cache_write,
    );
    RowOutcome::Event(UsageEvent::new(
        "cursor",
        event_key,
        occurred_at_ms,
        session_id,
        model,
        tokens,
        "exact",
    ))
}

fn usage_fetch_is_complete(
    fetched: i64,
    total: Option<i64>,
    last_page_full: bool,
    hit_cap: bool,
) -> bool {
    if hit_cap {
        return false;
    }
    match total {
        Some(total) => fetched >= total,
        None => !last_page_full,
    }
}

fn page_budget() -> i64 {
    MAX_PAGES * PAGE_SIZE
}

enum TotalDecision {
    Accept,
    RetryRange,
    SplitRange,
}

/// 超过单次分页上限就切开，不把重度用户锁在失败上。
/// 翻页过程中总数变了先重拉这一段：人越活跃，整段放弃的概率越高。
fn decide_total(previous: Option<i64>, total: i64) -> TotalDecision {
    if total > page_budget() {
        return TotalDecision::SplitRange;
    }
    if previous.is_some_and(|previous| previous != total) {
        return TotalDecision::RetryRange;
    }
    TotalDecision::Accept
}

/// 两段都含中点。接口没有写明端点开闭，重叠这一毫秒再按指纹扣掉重复，
/// 避免开闭理解反了时把边界上的调用丢掉或算两次。
fn split_millis(start: i64, end: i64) -> Option<((i64, i64), (i64, i64))> {
    if end <= start {
        return None;
    }
    // 只差 1 毫秒时无法重叠切开，否则右段会和原窗口一样大，递归不会结束。
    if end == start + 1 {
        return Some(((start, start), (end, end)));
    }
    let mid = start + (end - start) / 2;
    if mid <= start || mid >= end {
        return None;
    }
    Some(((start, mid), (mid, end)))
}

fn merge_overlapping_pages(mut left: Vec<Value>, right: Vec<Value>, boundary_ms: i64) -> Vec<Value> {
    let mut boundary_counts: HashMap<String, u32> = HashMap::new();
    for page in &left {
        let Some(rows) = page.as_array() else {
            continue;
        };
        for row in rows {
            if json_i64(row.get("timestamp")) != Some(boundary_ms) {
                continue;
            }
            let Some(fingerprint) = boundary_fingerprint(row) else {
                continue;
            };
            *boundary_counts.entry(fingerprint).or_insert(0) += 1;
        }
    }
    let right = right.into_iter().map(|page| {
        let Some(rows) = page.as_array() else {
            return page;
        };
        let kept = rows
            .iter()
            .filter(|row| {
                if json_i64(row.get("timestamp")) != Some(boundary_ms) {
                    return true;
                }
                let Some(fingerprint) = boundary_fingerprint(row) else {
                    return true;
                };
                let count = boundary_counts.entry(fingerprint).or_insert(0);
                if *count == 0 {
                    return true;
                }
                *count -= 1;
                false
            })
            .cloned()
            .collect();
        Value::Array(kept)
    });
    left.extend(right);
    left
}

fn boundary_fingerprint(row: &Value) -> Option<String> {
    match event_from_row(row, i64::MIN) {
        RowOutcome::Event(event) => Some(event.event_key),
        RowOutcome::Skip | RowOutcome::Malformed => None,
    }
}

struct FetchCheckpoint {
    source_id: String,
    cutoff_ms: i64,
    mtime_ns: i64,
    progress: FetchProgress,
}

struct FetchProgress {
    user_id: Option<i64>,
    ranges: VecDeque<RangeJob>,
    finished: Vec<FinishedRange>,
    transport_failures: u32,
}

struct RangeJob {
    start_ms: i64,
    end_ms: i64,
    attempts_used: u32,
    page_next: i64,
    fetched: i64,
    expected_total: Option<i64>,
    pages: Vec<Value>,
}

struct FinishedRange {
    start_ms: i64,
    end_ms: i64,
    pages: Vec<Value>,
}

impl RangeJob {
    fn new(start_ms: i64, end_ms: i64) -> Self {
        Self {
            start_ms,
            end_ms,
            attempts_used: 0,
            page_next: 1,
            fetched: 0,
            expected_total: None,
            pages: Vec::new(),
        }
    }

    fn reset_pages(&mut self) {
        self.page_next = 1;
        self.fetched = 0;
        // 清掉旧总数，下一页按服务端的新总数重新起页，而不是和过期总数死磕。
        self.expected_total = None;
        self.pages.clear();
    }
}

impl FetchProgress {
    fn new(cutoff_ms: i64, end_ms: i64) -> Self {
        let mut ranges = VecDeque::new();
        if cutoff_ms <= end_ms {
            ranges.push_back(RangeJob::new(cutoff_ms, end_ms));
        }
        Self {
            user_id: None,
            ranges,
            finished: Vec::new(),
            transport_failures: 0,
        }
    }

    fn is_complete(&self) -> bool {
        self.ranges.is_empty()
    }

    fn pages(&self) -> Vec<Value> {
        let mut acc = Vec::new();
        let mut acc_end = None;
        for range in &self.finished {
            if acc_end == Some(range.start_ms) {
                acc = merge_overlapping_pages(acc, range.pages.clone(), range.start_ms);
            } else {
                acc.extend(range.pages.clone());
            }
            acc_end = Some(range.end_ms);
        }
        acc
    }

    /// 把一页结果应用到当前时间段。调用方每次只发一页，发之前先看预算。
    fn apply_page(&mut self, total: i64, rows: Vec<Value>) -> Result<()> {
        let Some(front) = self.ranges.front() else {
            bail!("Cursor 用量没有待拉的时间段");
        };
        match decide_total(front.expected_total, total) {
            TotalDecision::SplitRange => self.split_front(),
            TotalDecision::RetryRange => {
                let attempts = {
                    let front = self.ranges.front_mut().expect("range still queued");
                    front.attempts_used += 1;
                    front.reset_pages();
                    front.attempts_used
                };
                if attempts >= RANGE_ATTEMPTS {
                    self.split_front()
                } else {
                    Ok(())
                }
            }
            TotalDecision::Accept => {
                let row_count = rows.len() as i64;
                let full = row_count >= PAGE_SIZE;
                let action = {
                    let front = self.ranges.front_mut().expect("range still queued");
                    front.expected_total = Some(total);
                    front.fetched += row_count;
                    front.pages.push(Value::Array(rows));
                    if full && front.fetched < total {
                        if front.page_next >= MAX_PAGES {
                            RangeAction::Split
                        } else {
                            front.page_next += 1;
                            RangeAction::Continue
                        }
                    } else if usage_fetch_is_complete(front.fetched, Some(total), full, false) {
                        RangeAction::Finish
                    } else {
                        RangeAction::Incomplete
                    }
                };
                match action {
                    RangeAction::Continue => Ok(()),
                    RangeAction::Split => self.split_front(),
                    RangeAction::Finish => {
                        self.finish_front();
                        Ok(())
                    }
                    RangeAction::Incomplete => {
                        bail!("Cursor 用量事件未完整拉完，本轮不写入，避免用残缺结果覆盖已有账本")
                    }
                }
            }
        }
    }

    fn split_front(&mut self) -> Result<()> {
        let Some(front) = self.ranges.pop_front() else {
            bail!("Cursor 用量没有待切开的时间段");
        };
        let Some((left, right)) = split_millis(front.start_ms, front.end_ms) else {
            bail!("Cursor 用量在同一毫秒内超过单次分页上限，本轮不写入");
        };
        self.ranges
            .push_front(RangeJob::new(right.0, right.1));
        self.ranges
            .push_front(RangeJob::new(left.0, left.1));
        if self.ranges.len() > MAX_QUEUED_RANGES {
            bail!("Cursor 用量时间窗口切得过碎，本轮不写入");
        }
        Ok(())
    }

    fn finish_front(&mut self) {
        let Some(front) = self.ranges.pop_front() else {
            return;
        };
        self.finished.push(FinishedRange {
            start_ms: front.start_ms,
            end_ms: front.end_ms,
            pages: front.pages,
        });
    }
}

enum RangeAction {
    Continue,
    Split,
    Finish,
    Incomplete,
}

fn budget_for(remaining: Option<Duration>) -> Option<Duration> {
    let Some(remaining) = remaining else {
        return Some(Duration::from_secs(8));
    };
    if remaining < MIN_SLICE {
        return None;
    }
    Some(remaining.min(MAX_REQUEST))
}

fn scan_from_pages(candidate: &SourceCandidate, cutoff_ms: i64, pages: Vec<Value>) -> ParsedScan {
    let parsed = events_from_pages(&pages, cutoff_ms);
    ParsedScan {
        source: ParsedSource {
            source_id: candidate.source_id.clone(),
            adapter_id: "cursor",
            locator: candidate.path.clone(),
            logical_key: candidate.source_id.clone(),
            size: candidate.size,
            mtime_ns: candidate.mtime_ns,
            events: parsed.events,
            quotas: Vec::new(),
        },
        diagnostics: ScanDiagnostics {
            malformed_lines: parsed.malformed,
            ..ScanDiagnostics::default()
        },
    }
}

fn fetch_is_pending(candidate: &SourceCandidate) -> bool {
    FETCH.lock().ok().is_some_and(|guard| {
        guard
            .as_ref()
            .is_some_and(|saved| saved.source_id == candidate.source_id)
    })
}

fn drive_fetch(
    state_db: &Path,
    candidate: &SourceCandidate,
    cutoff_ms: i64,
    deadline: Option<Instant>,
) -> Result<Option<Vec<Value>>> {
    let mut progress = take_progress(candidate, cutoff_ms);
    let token = match read_access_token(state_db)? {
        Some(token) => token,
        None => {
            clear_fetch();
            bail!("Cursor 没有登录会话，本轮不写入");
        }
    };
    let outcome = drive_with_token(&mut progress, &token, deadline);
    match &outcome {
        Ok(None) => store_progress(candidate, cutoff_ms, progress),
        Ok(Some(_)) | Err(_) => clear_fetch(),
    }
    outcome
}

fn drive_with_token(
    progress: &mut FetchProgress,
    access_token: &str,
    deadline: Option<Instant>,
) -> Result<Option<Vec<Value>>> {
    loop {
        if progress.is_complete() {
            return Ok(Some(progress.pages()));
        }
        let Some(timeout) = budget_for(deadline.map(|deadline| deadline.saturating_duration_since(Instant::now())))
        else {
            return Ok(None);
        };
        let step = if progress.user_id.is_none() {
            match fetch_user_id(access_token, timeout) {
                Ok(user_id) => {
                    progress.user_id = Some(user_id);
                    progress.transport_failures = 0;
                    continue;
                }
                Err(step) => step,
            }
        } else {
            let user_id = progress.user_id.expect("checked above");
            let (start_ms, end_ms, page) = {
                let front = progress.ranges.front().expect("incomplete fetch has a range");
                (front.start_ms, front.end_ms, front.page_next)
            };
            match fetch_usage_page(access_token, user_id, start_ms, end_ms, page, timeout) {
                Ok((total, rows)) => {
                    progress.transport_failures = 0;
                    progress.apply_page(total, rows)?;
                    continue;
                }
                Err(step) => step,
            }
        };
        match step {
            FetchStop::Pause => {
                progress.transport_failures += 1;
                if progress.transport_failures >= 3 {
                    bail!("Cursor 用量接口连续失败，本轮不写入");
                }
                return Ok(None);
            }
            FetchStop::Fatal(error) => return Err(error),
        }
    }
}

enum FetchStop {
    Pause,
    Fatal(anyhow::Error),
}

fn fetch_user_id(access_token: &str, timeout: Duration) -> Result<i64, FetchStop> {
    let json = cursor_request(access_token, AUTH_ME_URL, None, timeout)?;
    json_i64(json.get("id"))
        .filter(|id| *id > 0)
        .context("Cursor 登录会话缺少用户标识")
        .map_err(FetchStop::Fatal)
}

fn fetch_usage_page(
    access_token: &str,
    user_id: i64,
    start_ms: i64,
    end_ms: i64,
    page: i64,
    timeout: Duration,
) -> Result<(i64, Vec<Value>), FetchStop> {
    let body = serde_json::json!({
        "teamId": 0,
        "startDate": start_ms.to_string(),
        "endDate": end_ms.to_string(),
        "page": page,
        "pageSize": PAGE_SIZE,
        "userId": user_id,
    });
    let json = cursor_request(access_token, FILTERED_USAGE_URL, Some(body), timeout)?;
    usage_page(&json).map_err(FetchStop::Fatal)
}

fn cursor_request(
    access_token: &str,
    url: &str,
    body: Option<Value>,
    timeout: Duration,
) -> Result<Value, FetchStop> {
    let sub = jwt_sub(access_token).map_err(FetchStop::Fatal)?;
    let cookie = format!("WorkosCursorSessionToken={sub}%3A%3A{access_token}");
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let response = if let Some(body) = body {
        agent
            .post(url)
            .set("Cookie", &cookie)
            .set("Origin", "https://cursor.com")
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .send_string(&body.to_string())
    } else {
        agent
            .get(url)
            .set("Cookie", &cookie)
            .set("Accept", "application/json")
            .call()
    };
    let response = match response {
        Ok(response) => response,
        Err(ureq::Error::Status(401 | 403, _)) => {
            return Err(FetchStop::Fatal(anyhow::anyhow!(cursor_status_message(401))));
        }
        Err(ureq::Error::Status(429, _)) | Err(ureq::Error::Transport(_)) => {
            return Err(FetchStop::Pause);
        }
        Err(error) => return Err(FetchStop::Fatal(map_cursor_error(error))),
    };
    let text = response
        .into_string()
        .context("读取 Cursor 用量响应失败")
        .map_err(FetchStop::Fatal)?;
    serde_json::from_str(&text)
        .context("Cursor 用量响应不是预期的 JSON")
        .map_err(FetchStop::Fatal)
}

fn take_progress(candidate: &SourceCandidate, cutoff_ms: i64) -> FetchProgress {
    let saved = FETCH.lock().ok().and_then(|mut guard| guard.take());
    if let Some(saved) = saved {
        // 五分钟时间桶会在长抓取中途滚动。进度只跟来源和视界走，否则重度用户
        // 会在桶边界把已经拉到的页全部丢掉，永远重新开始。
        if saved.source_id == candidate.source_id && saved.cutoff_ms == cutoff_ms {
            return saved.progress;
        }
    }
    let end_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(cutoff_ms);
    FetchProgress::new(cutoff_ms, end_ms)
}

fn store_progress(candidate: &SourceCandidate, cutoff_ms: i64, progress: FetchProgress) {
    if let Ok(mut guard) = FETCH.lock() {
        *guard = Some(FetchCheckpoint {
            source_id: candidate.source_id.clone(),
            cutoff_ms,
            mtime_ns: candidate.mtime_ns,
            progress,
        });
    }
}

fn clear_fetch() {
    if let Ok(mut guard) = FETCH.lock() {
        *guard = None;
    }
}

fn usage_page(json: &Value) -> Result<(i64, Vec<Value>)> {
    // 空列表是合法的零用量；缺字段或类型变化不能当作零用量覆盖旧账本。
    let total = json_i64(json.get("totalUsageEventsCount"))
        .filter(|count| *count >= 0)
        .context("Cursor 用量响应缺少有效的事件总数，本轮不写入")?;
    let rows = json
        .get("usageEventsDisplay")
        .and_then(Value::as_array)
        .cloned()
        .context("Cursor 用量响应缺少事件列表，本轮不写入")?;
    Ok((total, rows))
}

fn map_cursor_error(error: ureq::Error) -> anyhow::Error {
    match error {
        ureq::Error::Status(code, _) => anyhow::anyhow!(cursor_status_message(code)),
        ureq::Error::Transport(transport) => {
            anyhow::anyhow!("Cursor 用量接口网络错误：{transport}")
        }
    }
}

fn cursor_status_message(code: u16) -> String {
    match code {
        401 | 403 => "Cursor 登录会话已失效，在 Cursor 里重新登录后再试".into(),
        429 => "Cursor 用量接口限流（429），稍后自动重试".into(),
        other => format!("Cursor 用量接口返回 HTTP {other}"),
    }
}

fn read_access_token(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let connection = open_state_db(path)?;
    let mut statement = connection
        .prepare("SELECT value FROM ItemTable WHERE key = 'cursorAuth/accessToken'")
        .context("读取 Cursor 登录会话失败")?;
    let mut rows = statement.query(rusqlite::params![])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let raw: String = row.get(0)?;
    let token = normalize_token(&raw);
    if token.is_empty() {
        Ok(None)
    } else {
        Ok(Some(token))
    }
}

fn open_state_db(path: &Path) -> Result<Connection> {
    // 只读打开。Cursor 用 WAL，必须读到 -wal 里尚未 checkpoint 的会话；
    // immutable 会跳过 WAL，可能把已登录误判成没有会话。
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open {}", path.display()))?;
    connection.pragma_update(None, "busy_timeout", 500_i64)?;
    Ok(connection)
}

fn normalize_token(raw: &str) -> String {
    raw.trim().trim_matches('"').trim().to_owned()
}

fn json_i64(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|value| value.round() as i64)),
        Value::String(text) => text
            .trim()
            .parse::<i64>()
            .ok()
            .or_else(|| text.trim().parse::<f64>().ok().map(|value| value.round() as i64)),
        _ => None,
    }
}

fn jwt_sub(token: &str) -> Result<String> {
    let payload = token.split('.').nth(1).context("Cursor 登录会话格式无法识别")?;
    let bytes = decode_base64url(payload).context("Cursor 登录会话格式无法识别")?;
    let json: Value = serde_json::from_slice(&bytes).context("Cursor 登录会话格式无法识别")?;
    json.get("sub")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .context("Cursor 登录会话缺少 sub")
}

fn decode_base64url(input: &str) -> Result<Vec<u8>> {
    fn value(byte: u8) -> Result<u8> {
        Ok(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => bail!("invalid base64"),
        })
    }
    let bytes = input.as_bytes();
    let mut output = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'=' {
            break;
        }
        let first = value(bytes[index])?;
        let second = value(*bytes.get(index + 1).context("truncated base64")?)?;
        output.push((first << 2) | (second >> 4));
        index += 2;
        if index >= bytes.len() || bytes[index] == b'=' {
            break;
        }
        let third = value(bytes[index])?;
        output.push((second << 4) | (third >> 2));
        index += 1;
        if index >= bytes.len() || bytes[index] == b'=' {
            break;
        }
        let fourth = value(bytes[index])?;
        output.push((third << 6) | fourth);
        index += 1;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "metrik-cursor-{label}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn state_db(dir: &Path, token: Option<&str>) -> PathBuf {
        let path = dir.join("state.vscdb");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)")
            .unwrap();
        if let Some(token) = token {
            connection
                .execute(
                    "INSERT INTO ItemTable (key, value) VALUES ('cursorAuth/accessToken', ?1)",
                    [token],
                )
                .unwrap();
        }
        connection.close().ok();
        path
    }

    fn sample_page() -> Value {
        serde_json::json!([
            {
                "timestamp": "1783591555915",
                "model": "composer-2.5",
                "conversationId": "conv-1",
                "tokenUsage": {
                    "inputTokens": 3595,
                    "outputTokens": 998,
                    "cacheReadTokens": 151103,
                    "cacheWriteTokens": 12
                }
            },
            {
                "timestamp": 1783591555915_i64,
                "model": "composer-2.5",
                "conversationId": "conv-1",
                "tokenUsage": {
                    "inputTokens": "3595",
                    "outputTokens": "998",
                    "cacheReadTokens": "151103",
                    "cacheWriteTokens": "12"
                }
            },
            {
                "timestamp": "1000",
                "model": "composer-2.5",
                "conversationId": "old",
                "tokenUsage": { "inputTokens": 10, "outputTokens": 1, "cacheReadTokens": 0, "cacheWriteTokens": 0 }
            },
            {
                "timestamp": "1783591600000",
                "model": "composer-2.5",
                "tokenUsage": { "inputTokens": 0, "outputTokens": 0, "cacheReadTokens": 0, "cacheWriteTokens": 0 }
            },
            { "model": "composer-2.5" }
        ])
    }

    #[test]
    fn dashboard_rows_split_tokens_the_same_way_as_other_agents() {
        let parsed = events_from_pages(&[sample_page()], 1_783_000_000_000);
        assert_eq!(parsed.malformed, 1);
        assert_eq!(
            parsed.events.len(),
            2,
            "同一毫秒的两条相同结构都要入账；零 token 与窗口外事件不入账"
        );
        assert!(parsed.events[1].event_key.ends_with("#2"));
        let event = &parsed.events[0];
        assert_eq!(event.adapter_id, "cursor");
        assert_eq!(event.session_id, "conv-1");
        assert_eq!(event.model.as_deref(), Some("composer-2.5"));
        assert_eq!(event.occurred_at_ms, 1_783_591_555_915);
        assert_eq!(event.tokens.input_uncached, 3595);
        assert_eq!(event.tokens.output, 998);
        assert_eq!(event.tokens.cache_read, 151103);
        assert_eq!(event.tokens.cache_write, 12);
        assert_eq!(event.tokens.reasoning_output, 0);
        assert_eq!(event.tokens.processed(), 3595 + 998 + 151103 + 12);
        assert!(event.event_key.starts_with("cursor:1783591555915|conv-1|composer-2.5|"));
        assert!(event.project_path.is_none());
    }

    #[test]
    fn a_revised_reading_is_a_different_event_key() {
        let first = events_from_pages(&[sample_page()], 0);
        let mut revised = sample_page();
        revised[0]["tokenUsage"]["outputTokens"] = serde_json::json!(1000);
        revised
            .as_array_mut()
            .unwrap()
            .truncate(1);
        let second = events_from_pages(&[revised], 0);
        assert_ne!(first.events[0].event_key, second.events[0].event_key);
    }

    #[test]
    fn refresh_bucket_stays_put_inside_five_minutes() {
        let start = 1_700_000_000_000;
        assert_eq!(refresh_bucket_ns(start), refresh_bucket_ns(start + 60_000));
        assert_ne!(
            refresh_bucket_ns(start),
            refresh_bucket_ns(start + REFRESH_MS)
        );
    }

    #[test]
    fn incomplete_pages_are_not_treated_as_a_full_read() {
        assert!(usage_fetch_is_complete(1000, Some(1000), true, false));
        assert!(usage_fetch_is_complete(20, Some(20), false, false));
        assert!(!usage_fetch_is_complete(1000, Some(2500), true, false));
        assert!(!usage_fetch_is_complete(15_000, None, true, true));
        assert!(usage_fetch_is_complete(20, None, false, false));
    }

    #[test]
    fn a_large_or_shifting_total_is_split_or_retried_instead_of_abandoned() {
        assert!(matches!(
            decide_total(None, page_budget()),
            TotalDecision::Accept
        ));
        assert!(matches!(
            decide_total(None, page_budget() + 1),
            TotalDecision::SplitRange
        ));
        assert!(matches!(
            decide_total(Some(3_000), 3_001),
            TotalDecision::RetryRange
        ));
        assert!(matches!(
            decide_total(Some(3_000), page_budget() + 5),
            TotalDecision::SplitRange
        ));
    }

    #[test]
    fn a_page_stops_at_the_budget_without_closing_the_range() {
        let mut progress = FetchProgress::new(0, 10_000);
        progress
            .apply_page(2_500, vec![serde_json::json!({}); PAGE_SIZE as usize])
            .unwrap();
        assert!(!progress.is_complete());
        assert!(progress.finished.is_empty());
        assert_eq!(progress.ranges.front().unwrap().page_next, 2);
        assert!(budget_for(Some(Duration::from_millis(50))).is_none());
        assert_eq!(
            budget_for(Some(Duration::from_millis(500))),
            Some(Duration::from_millis(500))
        );
        assert_eq!(budget_for(Some(Duration::from_secs(5))), Some(MAX_REQUEST));
    }

    #[test]
    fn an_oversized_window_splits_before_any_partial_page_is_kept() {
        let mut progress = FetchProgress::new(0, 10_000);
        progress.apply_page(page_budget() + 1, Vec::new()).unwrap();
        assert_eq!(progress.ranges.len(), 2);
        assert!(progress.ranges.iter().all(|range| range.pages.is_empty()));
        assert!(progress.finished.is_empty());
    }

    #[test]
    fn a_shifting_total_retries_then_splits_instead_of_failing() {
        let mut progress = FetchProgress::new(1_000, 5_000);
        progress
            .apply_page(2_000, vec![serde_json::json!({}); PAGE_SIZE as usize])
            .unwrap();
        for shift in 1..=RANGE_ATTEMPTS {
            progress
                .apply_page(2_000 + i64::from(shift), vec![serde_json::json!({}); PAGE_SIZE as usize])
                .unwrap();
            if shift < RANGE_ATTEMPTS {
                // 重拉后的第一页采用新总数；下一页若再变，才算下一次重试。
                progress
                    .apply_page(
                        2_000 + i64::from(shift),
                        vec![serde_json::json!({}); PAGE_SIZE as usize],
                    )
                    .unwrap();
            }
        }
        assert_eq!(progress.ranges.len(), 2);
        assert!(progress.finished.is_empty());
    }

    #[test]
    fn time_splits_overlap_on_the_midpoint_only() {
        let ((left_start, left_end), (right_start, right_end)) = split_millis(1_000, 5_000).unwrap();
        assert_eq!(left_start, 1_000);
        assert_eq!(right_end, 5_000);
        assert_eq!(right_start, left_end);
        assert!(left_end - left_start < 4_000);
        assert!(right_end - right_start < 4_000);
        assert!(split_millis(10, 10).is_none());
        let ((only_left, only_left_end), (only_right, only_right_end)) =
            split_millis(10, 11).unwrap();
        assert_eq!(
            (only_left, only_left_end, only_right, only_right_end),
            (10, 10, 11, 11)
        );
    }

    #[test]
    fn overlapping_boundary_rows_are_counted_once_unless_the_right_side_has_more() {
        let row = serde_json::json!({
            "timestamp": 50,
            "model": "composer-2.5",
            "conversationId": "conv-1",
            "tokenUsage": { "inputTokens": 3, "outputTokens": 1, "cacheReadTokens": 0, "cacheWriteTokens": 0 }
        });
        let extra = serde_json::json!({
            "timestamp": 50,
            "model": "composer-2.5",
            "conversationId": "conv-1",
            "tokenUsage": { "inputTokens": 9, "outputTokens": 1, "cacheReadTokens": 0, "cacheWriteTokens": 0 }
        });
        let merged = merge_overlapping_pages(
            vec![Value::Array(vec![row.clone(), row.clone()])],
            vec![Value::Array(vec![row.clone(), extra])],
            50,
        );
        let parsed = events_from_pages(&merged, 0);
        assert_eq!(parsed.events.len(), 3);
        assert_eq!(
            parsed
                .events
                .iter()
                .filter(|event| event.tokens.input_uncached == 3)
                .count(),
            2
        );
    }

    #[test]
    fn usage_page_rejects_missing_or_invalid_fields() {
        assert!(usage_page(&serde_json::json!({})).is_err());
        assert!(usage_page(&serde_json::json!({"totalUsageEventsCount": 0})).is_err());
        assert!(usage_page(&serde_json::json!({
            "totalUsageEventsCount": 0,
            "usageEventsDisplay": null
        }))
        .is_err());
        assert!(usage_page(&serde_json::json!({
            "totalUsageEventsCount": -1,
            "usageEventsDisplay": []
        }))
        .is_err());
        let (total, rows) = usage_page(&serde_json::json!({
            "totalUsageEventsCount": 0,
            "usageEventsDisplay": []
        }))
        .unwrap();
        assert_eq!(total, 0);
        assert!(rows.is_empty());
    }

    #[test]
    fn jwt_sub_reads_only_the_subject() {
        // {"sub":"user_01ABC"}
        let token = "aaa.eyJzdWIiOiJ1c2VyXzAxQUJDIn0.sig";
        assert_eq!(jwt_sub(token).unwrap(), "user_01ABC");
        let error = jwt_sub("not-a-jwt").unwrap_err().to_string();
        assert!(!error.contains("not-a-jwt"));
    }

    #[test]
    fn auth_errors_do_not_echo_the_session() {
        let message = cursor_status_message(401);
        assert!(message.contains("重新登录"));
        assert!(!message.contains("WorkosCursorSessionToken"));
        assert!(!message.contains("accessToken"));
    }

    #[test]
    fn missing_session_is_a_coverage_gap_not_a_zero_exact_read() {
        let dir = TestDirectory::new("gap");
        let path = state_db(dir.path(), None);
        let adapter = CursorAdapter::with_state_db(path.clone());
        let gaps = adapter.coverage_gaps();
        assert_eq!(gaps.len(), 1);
        assert!(gaps[0].contains("tokenCount"));
        let candidate = SourceCandidate {
            source_id: "cursor-test".into(),
            path,
            size: 1,
            mtime_ns: 1,
        };
        let error = adapter.parse(&candidate, 0).unwrap_err().to_string();
        assert!(error.contains("没有登录会话"));
        assert!(!error.contains("accessToken"));
    }

    #[test]
    fn discover_follows_the_state_db_and_ignores_its_mtime() {
        let dir = TestDirectory::new("discover");
        let absent = CursorAdapter::with_state_db(dir.path().join("missing.vscdb"));
        assert!(absent.discover(0).is_empty());

        let path = state_db(dir.path(), Some("token"));
        let adapter = CursorAdapter::with_state_db(path);
        let found = adapter.discover(i64::MAX / 4);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].size, 1);
        assert_eq!(adapter.discover(0)[0].mtime_ns, found[0].mtime_ns);
    }

    #[test]
    fn detected_state_db_is_the_cursor_global_storage_file() {
        let home = dirs::config_dir().unwrap_or_default();
        assert_eq!(
            CursorAdapter::detected().state_db,
            home.join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb")
        );
    }
}
