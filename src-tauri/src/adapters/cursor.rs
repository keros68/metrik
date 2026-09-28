//! Cursor 的 Token 用量。
//!
//! 当前版本的 Cursor 不把逐次 token 写进本机（`state.vscdb` 里 bubble 的
//! `tokenCount` 多为 0），数字只能取 cursor.com 仪表盘的同一份逐次事件：
//! `POST https://cursor.com/api/dashboard/get-filtered-usage-events`。
//! 这是会联网、会用到登录会话的来源，所以默认关闭，由设置里的开关
//! （`USAGE_SETTING_KEY`）显式开启。
//!
//! 凭据：只读 Cursor 自己以明文存在 `ItemTable.cursorAuth/accessToken` 的会话，
//! 每次请求前现读，只留在内存，不刷新、不落库、不写日志。离过期不足一分钟的
//! 会话不发出去。
//!
//! 口径（与仪表盘 CSV 的列一致：总量 = 缓存写 + 未缓存输入 + 缓存读 + 输出）：
//! - `inputTokens` → `input_uncached`
//! - `cacheReadTokens` / `cacheWriteTokens` 分开计入
//! - `outputTokens` → `output`；事件没有推理子项，`reasoning_output` 为 0
//! - 事件不报 token 总量（`totalCents` 是金额），不做自报总量核对
//!
//! 分段：保留期按 UTC 日切成来源，每段各自拉全再整段替换。已过去的日子
//! mtime 固定，拉完一次就不再请求；只有今天和昨天跟着 15 分钟的刷新桶重拉，
//! 稳态每 15 分钟两次请求。一段没拉完只在内存里留进度，下次快照续拉；
//! 失败后按原因退避，换了登录会话立即解除。
//!
//! 身份：接口没有 request id，事件键用时间、会话、模型与分量；同一日内完全
//! 相同的指纹按出现次序加后缀，并行调用不会被并成一条。
//!
//! 事件是账号级的，包含这个账号在所有设备上的用量；同步导出因此跳过 Cursor，
//! 避免两台都开启的设备各算一遍。接口不带工作目录，用量不归入项目。

use super::{AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{stable_hash, ParsedSource, TokenVector, UsageEvent};
use anyhow::{bail, Context, Result};
use base64::Engine;
use rusqlite::{types::Value as SqlValue, Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const USAGE_SETTING_KEY: &str = "cursor_usage_enabled";

const USAGE_EVENTS_URL: &str = "https://cursor.com/api/dashboard/get-filtered-usage-events";
const DAY_MS: i64 = 86_400_000;
/// 今天和昨天的重拉间隔。仪表盘数据按小时聚合，更密的轮询拿不到新东西。
const REFRESH_MS: i64 = 15 * 60 * 1000;
/// 今天之前还要重拉几天：晚到的事件会落在昨天。
const RECENT_DAYS: i64 = 1;
const PAGE_SIZE: i64 = 1000;
const MAX_PAGES: i64 = 50;
/// 翻页中总数变了（有新事件进来）就从第一页重拉；连续变这么多次先放弃本轮。
const MAX_RESTARTS: u32 = 3;
/// 离过期不足这么久的会话不发出去，由 Cursor 自己刷新。
const EXPIRY_MARGIN_MS: i64 = 60_000;
/// 剩余预算不够一次请求时挂起到下次快照。单次请求的超时夹在这两个值之间，
/// 慢网络下也能拉完一页，又不会长时间占着扫描锁。
const MIN_REQUEST_WINDOW: Duration = Duration::from_millis(250);
const MIN_TIMEOUT: Duration = Duration::from_millis(1500);
const MAX_TIMEOUT: Duration = Duration::from_secs(3);
const AUTH_BACKOFF: Duration = Duration::from_secs(60 * 60);
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(15 * 60);
const TEMPORARY_BACKOFF: Duration = Duration::from_secs(5 * 60);

const NOT_SIGNED_IN: &str = "检测到 Cursor，但本机没有登录会话，读不到用量";
const EXPIRED: &str = "Cursor 登录会话已过期，打开 Cursor 让它刷新后再试";

static PROGRESS: Mutex<Option<DayProgress>> = Mutex::new(None);
static HEALTH: Mutex<Option<Failure>> = Mutex::new(None);

pub struct CursorAdapter {
    state_db: PathBuf,
    enabled: bool,
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
    pub fn detected(enabled: bool) -> Self {
        Self {
            state_db: cursor_state_db(),
            enabled,
        }
    }

    #[cfg(test)]
    fn with_state_db(state_db: PathBuf, enabled: bool) -> Self {
        Self { state_db, enabled }
    }

    fn day_candidates(&self, cutoff_ms: i64, now_ms: i64) -> Vec<(i64, SourceCandidate)> {
        let locator = normalize_locator(&self.state_db);
        let today = now_ms.div_euclid(DAY_MS);
        let bucket_ns = now_ms.div_euclid(REFRESH_MS) * REFRESH_MS * 1_000_000;
        (cutoff_ms.div_euclid(DAY_MS)..=today)
            .map(|day| {
                let mtime_ns = if day >= today - RECENT_DAYS {
                    bucket_ns
                } else {
                    (day + 1) * DAY_MS * 1_000_000
                };
                let candidate = SourceCandidate {
                    source_id: stable_hash(&format!("cursor|{locator}|{day}")),
                    path: self.state_db.clone(),
                    size: 1,
                    mtime_ns,
                };
                (day, candidate)
            })
            .collect()
    }
}

impl AgentAdapter for CursorAdapter {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate> {
        if !self.enabled || !self.state_db.exists() {
            return Vec::new();
        }
        self.day_candidates(cutoff_ms, now_ms())
            .into_iter()
            .map(|(_, candidate)| candidate)
            .collect()
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        self.parse_until(
            candidate,
            cutoff_ms,
            Instant::now() + Duration::from_secs(60),
        )?
        .context("Cursor 用量未在单次解析时限内拉完")
    }

    fn has_pending(&self, candidate: &SourceCandidate) -> bool {
        PROGRESS.lock().ok().is_some_and(|guard| {
            guard
                .as_ref()
                .is_some_and(|progress| progress.source_id == candidate.source_id)
        })
    }

    fn parse_until(
        &self,
        candidate: &SourceCandidate,
        cutoff_ms: i64,
        deadline: Instant,
    ) -> Result<Option<ParsedScan>> {
        let now = now_ms();
        let Some(day) = self
            .day_candidates(cutoff_ms, now)
            .into_iter()
            .find(|(_, known)| known.source_id == candidate.source_id)
            .map(|(day, _)| day)
        else {
            bail!("Cursor 用量来源已不在保留期内");
        };
        let session = match read_session(&self.state_db)? {
            SessionState::Usable(session) if session.expires_at_ms > now + EXPIRY_MARGIN_MS => {
                session
            }
            SessionState::Usable(_) => bail!(EXPIRED),
            SessionState::Missing => bail!(NOT_SIGNED_IN),
        };
        if let Some(message) = active_backoff(&session.fingerprint, Instant::now()) {
            bail!(message);
        }

        let (start_ms, end_ms) = (day * DAY_MS, (day + 1) * DAY_MS);
        let mut progress = take_progress(&candidate.source_id);
        let step = collect_day(&mut progress, deadline, |page, timeout| {
            fetch_page(&session, start_ms, end_ms, page, timeout)
        });
        match step {
            Ok(DayStep::Pending) => {
                store_progress(progress);
                Ok(None)
            }
            Ok(DayStep::Complete) => {
                clear_failure();
                let parsed = events_from_rows(&progress.rows, start_ms, end_ms, cutoff_ms);
                Ok(Some(ParsedScan {
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
                }))
            }
            Err(error) => {
                let message = error.message();
                record_failure(&session.fingerprint, &error, Instant::now());
                bail!(message)
            }
        }
    }

    fn coverage_gaps(&self) -> Vec<String> {
        if !self.enabled || !self.state_db.exists() {
            return Vec::new();
        }
        match read_session(&self.state_db) {
            Ok(SessionState::Missing) => return vec![NOT_SIGNED_IN.into()],
            Ok(SessionState::Usable(session))
                if session.expires_at_ms <= now_ms() + EXPIRY_MARGIN_MS =>
            {
                return vec![EXPIRED.into()]
            }
            Ok(SessionState::Usable(_)) => {}
            Err(_) => return vec!["读取 Cursor 登录状态失败".into()],
        }
        HEALTH
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|failure| failure.message.clone()))
            .into_iter()
            .collect()
    }
}

/// 设置页展示用；不含会话内容。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorUsageStatus {
    pub enabled: bool,
    pub installed: bool,
    pub signed_in: bool,
    pub expired: bool,
}

pub fn usage_status(enabled: bool) -> CursorUsageStatus {
    let state_db = cursor_state_db();
    let installed = state_db.exists();
    let session = installed.then(|| read_session(&state_db).ok()).flatten();
    let (signed_in, expired) = match session {
        Some(SessionState::Usable(session)) => {
            (true, session.expires_at_ms <= now_ms() + EXPIRY_MARGIN_MS)
        }
        _ => (false, false),
    };
    CursorUsageStatus {
        enabled,
        installed,
        signed_in,
        expired,
    }
}

/// 关闭开关时丢掉内存里的进度与退避，下次开启从头拉。
pub fn reset_runtime_state() {
    if let Ok(mut guard) = PROGRESS.lock() {
        *guard = None;
    }
    clear_failure();
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn normalize_locator(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

struct Session {
    cookie: String,
    expires_at_ms: i64,
    /// 区分退避属于哪一个会话；不是会话本身。
    fingerprint: String,
}

enum SessionState {
    Missing,
    Usable(Session),
}

fn read_session(state_db: &Path) -> Result<SessionState> {
    let connection = Connection::open_with_flags(
        state_db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open {}", state_db.display()))?;
    // 不用 immutable：Cursor 开着 WAL，刚写入的会话可能还在 -wal 里。
    connection.pragma_update(None, "busy_timeout", 500_i64)?;
    let raw: Option<SqlValue> = connection
        .query_row(
            "SELECT value FROM ItemTable WHERE key = 'cursorAuth/accessToken'",
            [],
            |row| row.get(0),
        )
        .optional()
        .context("读取 Cursor 登录会话失败")?;
    let token = match raw {
        Some(SqlValue::Text(text)) => text,
        Some(SqlValue::Blob(bytes)) => decode_blob(&bytes),
        _ => String::new(),
    };
    let token = token.trim().trim_matches('"').trim();
    if token.is_empty() {
        return Ok(SessionState::Missing);
    }
    session_from_token(token).map(SessionState::Usable)
}

/// VS Code 系的状态库偶尔把值存成 BLOB，其中有 UTF-16LE。
fn decode_blob(bytes: &[u8]) -> String {
    let looks_utf16 = bytes.len() >= 2 && bytes.len().is_multiple_of(2) && bytes[1] == 0;
    if looks_utf16 {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn session_from_token(token: &str) -> Result<Session> {
    let payload = token
        .split('.')
        .nth(1)
        .context("Cursor 登录会话格式无法识别")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .context("Cursor 登录会话格式无法识别")?;
    let claims: Value = serde_json::from_slice(&bytes).context("Cursor 登录会话格式无法识别")?;
    // `sub` 形如 `auth0|user_xxx`，仪表盘 cookie 用最后一段。
    let user_id = claims
        .get("sub")
        .and_then(Value::as_str)
        .and_then(|sub| sub.rsplit('|').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("Cursor 登录会话缺少用户标识")?;
    let expires_at_ms = claims
        .get("exp")
        .and_then(Value::as_i64)
        .context("Cursor 登录会话缺少有效期")?
        .saturating_mul(1000);
    Ok(Session {
        cookie: format!("WorkosCursorSessionToken={user_id}%3A%3A{token}"),
        expires_at_ms,
        fingerprint: stable_hash(token),
    })
}

#[derive(Debug)]
enum FetchError {
    Auth,
    RateLimited,
    Temporary(String),
    Invalid(String),
}

impl FetchError {
    fn message(&self) -> String {
        match self {
            Self::Auth => "Cursor 拒绝了本机的登录会话，在 Cursor 里重新登录后自动恢复".into(),
            Self::RateLimited => "Cursor 用量接口限流，15 分钟后再试".into(),
            Self::Temporary(reason) => format!("Cursor 用量暂时读不到（{reason}），5 分钟后再试"),
            Self::Invalid(reason) => format!("Cursor 用量响应无法识别（{reason}），5 分钟后再试"),
        }
    }

    fn backoff(&self) -> Duration {
        match self {
            Self::Auth => AUTH_BACKOFF,
            Self::RateLimited => RATE_LIMIT_BACKOFF,
            Self::Temporary(_) | Self::Invalid(_) => TEMPORARY_BACKOFF,
        }
    }
}

struct Failure {
    fingerprint: String,
    until: Instant,
    message: String,
}

fn record_failure(fingerprint: &str, error: &FetchError, now: Instant) {
    if let Ok(mut guard) = HEALTH.lock() {
        *guard = Some(Failure {
            fingerprint: fingerprint.to_owned(),
            until: now + error.backoff(),
            message: error.message(),
        });
    }
}

fn clear_failure() {
    if let Ok(mut guard) = HEALTH.lock() {
        *guard = None;
    }
}

/// 退避只对出错的那个会话生效：用户在 Cursor 里重新登录后立即重试。
fn active_backoff(fingerprint: &str, now: Instant) -> Option<String> {
    let guard = HEALTH.lock().ok()?;
    let failure = guard.as_ref()?;
    (failure.fingerprint == fingerprint && now < failure.until).then(|| failure.message.clone())
}

struct Page {
    total: i64,
    rows: Vec<Value>,
}

fn fetch_page(
    session: &Session,
    start_ms: i64,
    end_ms: i64,
    page: i64,
    timeout: Duration,
) -> Result<Page, FetchError> {
    let body = serde_json::json!({
        "teamId": 0,
        "startDate": start_ms.to_string(),
        "endDate": end_ms.to_string(),
        "page": page,
        "pageSize": PAGE_SIZE,
    });
    let response = ureq::AgentBuilder::new()
        .timeout(timeout)
        .build()
        .post(USAGE_EVENTS_URL)
        .set("Cookie", &session.cookie)
        // 仪表盘接口的 CSRF 检查要求同源 Origin。
        .set("Origin", "https://cursor.com")
        .set("Accept", "application/json")
        .set("Content-Type", "application/json")
        .send_string(&body.to_string());
    let response = match response {
        Ok(response) => response,
        Err(ureq::Error::Status(401 | 403, _)) => return Err(FetchError::Auth),
        Err(ureq::Error::Status(429, _)) => return Err(FetchError::RateLimited),
        Err(ureq::Error::Status(code, _)) => {
            return Err(FetchError::Temporary(format!("HTTP {code}")))
        }
        Err(ureq::Error::Transport(_)) => return Err(FetchError::Temporary("网络错误".into())),
    };
    let text = response
        .into_string()
        .map_err(|_| FetchError::Temporary("响应读取中断".into()))?;
    let json: Value =
        serde_json::from_str(&text).map_err(|_| FetchError::Invalid("不是 JSON".into()))?;
    parse_page(&json).map_err(FetchError::Invalid)
}

/// 空窗口返回 `{}`；末尾的空页省略事件数组但保留总数。其余缺字段都不能当作零用量。
fn parse_page(json: &Value) -> Result<Page, String> {
    let object = json.as_object().ok_or("响应不是对象")?;
    if object.is_empty() {
        return Ok(Page {
            total: 0,
            rows: Vec::new(),
        });
    }
    let total = json_i64(object.get("totalUsageEventsCount"))
        .filter(|total| *total >= 0)
        .ok_or("缺少事件总数")?;
    let rows = match object.get("usageEventsDisplay") {
        Some(Value::Array(rows)) => rows.clone(),
        None => Vec::new(),
        Some(_) => return Err("事件列表类型不对".into()),
    };
    Ok(Page { total, rows })
}

struct DayProgress {
    source_id: String,
    next_page: i64,
    total: Option<i64>,
    restarts: u32,
    rows: Vec<Value>,
}

impl DayProgress {
    fn new(source_id: &str) -> Self {
        Self {
            source_id: source_id.to_owned(),
            next_page: 1,
            total: None,
            restarts: 0,
            rows: Vec::new(),
        }
    }
}

fn take_progress(source_id: &str) -> DayProgress {
    PROGRESS
        .lock()
        .ok()
        .and_then(|mut guard| guard.take())
        .filter(|progress| progress.source_id == source_id)
        .unwrap_or_else(|| DayProgress::new(source_id))
}

fn store_progress(progress: DayProgress) {
    if let Ok(mut guard) = PROGRESS.lock() {
        *guard = Some(progress);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum DayStep {
    Pending,
    Complete,
}

/// 翻页直到拿满服务端报的总数。预算不够时挂起，进度留在 `progress` 里。
fn collect_day(
    progress: &mut DayProgress,
    deadline: Instant,
    mut fetch: impl FnMut(i64, Duration) -> Result<Page, FetchError>,
) -> Result<DayStep, FetchError> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining < MIN_REQUEST_WINDOW {
            return Ok(DayStep::Pending);
        }
        let page = fetch(
            progress.next_page,
            remaining.clamp(MIN_TIMEOUT, MAX_TIMEOUT),
        )?;
        if progress.total.is_some_and(|total| total != page.total) {
            progress.restarts += 1;
            if progress.restarts >= MAX_RESTARTS {
                return Err(FetchError::Temporary("翻页期间事件总数持续变化".into()));
            }
            progress.next_page = 1;
            progress.total = None;
            progress.rows.clear();
            continue;
        }
        progress.total = Some(page.total);
        let empty = page.rows.is_empty();
        progress.rows.extend(page.rows);
        if progress.rows.len() as i64 >= page.total {
            return Ok(DayStep::Complete);
        }
        if empty || progress.next_page >= MAX_PAGES {
            return Err(FetchError::Invalid("事件数少于报告的总数".into()));
        }
        progress.next_page += 1;
    }
}

struct ParsedRows {
    events: Vec<UsageEvent>,
    malformed: usize,
}

/// 只收时间落在本日 `[start_ms, end_ms)` 内的事件：接口端点的开闭没有文档，
/// 边界上的事件由它所在的那一天计入。
fn events_from_rows(rows: &[Value], start_ms: i64, end_ms: i64, cutoff_ms: i64) -> ParsedRows {
    let mut events = Vec::new();
    let mut malformed = 0;
    let mut occurrences: HashMap<String, u32> = HashMap::new();
    for row in rows {
        let Some(occurred_at_ms) = json_i64(row.get("timestamp")) else {
            malformed += 1;
            continue;
        };
        if occurred_at_ms < start_ms.max(cutoff_ms) || occurred_at_ms >= end_ms {
            continue;
        }
        let usage = row.get("tokenUsage");
        let component = |name: &str| {
            json_i64(usage.and_then(|usage| usage.get(name)))
                .unwrap_or(0)
                .max(0)
        };
        let tokens = TokenVector {
            input_uncached: component("inputTokens"),
            cache_read: component("cacheReadTokens"),
            cache_write: component("cacheWriteTokens"),
            output: component("outputTokens"),
            reasoning_output: 0,
        };
        if tokens.processed() == 0 {
            continue;
        }
        let text = |name: &str| {
            row.get(name)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let model = text("model");
        let session_id = text("conversationId").unwrap_or_else(|| "cursor".into());
        let base = format!(
            "cursor:{occurred_at_ms}|{session_id}|{}|{}|{}|{}|{}",
            model.as_deref().unwrap_or(""),
            tokens.input_uncached,
            tokens.cache_read,
            tokens.cache_write,
            tokens.output,
        );
        let seen = occurrences.entry(base.clone()).or_insert(0);
        *seen += 1;
        let event_key = if *seen == 1 {
            base
        } else {
            format!("{base}#{seen}")
        };
        events.push(UsageEvent::new(
            "cursor",
            event_key,
            occurred_at_ms,
            session_id,
            model,
            tokens,
            "exact",
        ));
    }
    ParsedRows { events, malformed }
}

/// 仪表盘把毫秒时间戳和部分计数写成字符串。
fn json_i64(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn token_with(claims: Value) -> String {
        let encode = |value: &Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
        };
        format!(
            "{}.{}.signature",
            encode(&json!({"alg": "HS256"})),
            encode(&claims)
        )
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "metrik-cursor-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn state_db_with(dir: &Path, value: Option<SqlValue>) -> PathBuf {
        let path = dir.join("state.vscdb");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)",
            )
            .unwrap();
        if let Some(value) = value {
            connection
                .execute(
                    "INSERT INTO ItemTable (key, value) VALUES ('cursorAuth/accessToken', ?1)",
                    [value],
                )
                .unwrap();
        }
        path
    }

    fn page(total: i64, count: usize) -> Page {
        Page {
            total,
            rows: (0..count).map(|index| json!({"index": index})).collect(),
        }
    }

    #[test]
    fn detected_state_db_is_the_cursor_global_storage_file() {
        let path = cursor_state_db();
        assert!(path.ends_with(
            Path::new("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb")
        ));
    }

    #[test]
    fn session_cookie_uses_the_user_segment_of_the_subject() {
        let token = token_with(json!({"sub": "auth0|user_abc123", "exp": 1_900_000_000}));
        let session = session_from_token(&token).unwrap();
        assert_eq!(
            session.cookie,
            format!("WorkosCursorSessionToken=user_abc123%3A%3A{token}")
        );
        assert_eq!(session.expires_at_ms, 1_900_000_000_000);
        assert_ne!(session.fingerprint, token);
        assert!(session_from_token("not-a-jwt").is_err());
        assert!(session_from_token(&token_with(json!({"exp": 1}))).is_err());
    }

    #[test]
    fn the_session_is_read_from_text_and_utf16_values() {
        let token = token_with(json!({"sub": "auth0|user_x", "exp": 1_900_000_000}));
        let dir = temp_dir("session");

        let text_dir = dir.join("text");
        fs::create_dir_all(&text_dir).unwrap();
        let text_db = state_db_with(&text_dir, Some(SqlValue::Text(format!("\"{token}\""))));
        assert!(matches!(
            read_session(&text_db).unwrap(),
            SessionState::Usable(_)
        ));

        let blob_dir = dir.join("blob");
        fs::create_dir_all(&blob_dir).unwrap();
        let utf16: Vec<u8> = token.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let blob_db = state_db_with(&blob_dir, Some(SqlValue::Blob(utf16)));
        assert!(matches!(
            read_session(&blob_db).unwrap(),
            SessionState::Usable(_)
        ));

        let empty_dir = dir.join("empty");
        fs::create_dir_all(&empty_dir).unwrap();
        let empty_db = state_db_with(&empty_dir, None);
        assert!(matches!(
            read_session(&empty_db).unwrap(),
            SessionState::Missing
        ));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn nothing_is_discovered_until_the_setting_is_on() {
        let dir = temp_dir("discover");
        let state_db = state_db_with(&dir, None);
        let cutoff = now_ms() - 3 * DAY_MS;
        assert!(CursorAdapter::with_state_db(state_db.clone(), false)
            .discover(cutoff)
            .is_empty());
        assert_eq!(
            CursorAdapter::with_state_db(state_db, true)
                .discover(cutoff)
                .len(),
            4
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_today_and_yesterday_follow_the_refresh_bucket() {
        let adapter = CursorAdapter::with_state_db(PathBuf::from("state.vscdb"), true);
        let now = 100 * DAY_MS + 5 * 60 * 60 * 1000;
        let days = adapter.day_candidates(now - 4 * DAY_MS, now);
        assert_eq!(
            days.iter().map(|(day, _)| *day).collect::<Vec<_>>(),
            vec![96, 97, 98, 99, 100]
        );
        let bucket = now.div_euclid(REFRESH_MS) * REFRESH_MS * 1_000_000;
        assert_eq!(days[4].1.mtime_ns, bucket);
        assert_eq!(days[3].1.mtime_ns, bucket);
        assert_eq!(days[2].1.mtime_ns, 99 * DAY_MS * 1_000_000);

        // 同一个刷新桶里再发现一次，来源不变，不会重新请求。
        let later = adapter.day_candidates(now - 4 * DAY_MS, now + 60_000);
        assert_eq!(later[4].1.source_id, days[4].1.source_id);
        assert_eq!(later[4].1.mtime_ns, days[4].1.mtime_ns);
        // 已过去的日子 mtime 固定，拉过一次就不再请求。
        let next_day = adapter.day_candidates(now - 4 * DAY_MS, now + DAY_MS);
        assert_eq!(next_day[2].1.mtime_ns, days[2].1.mtime_ns);
    }

    #[test]
    fn rows_split_tokens_like_the_dashboard_and_stay_inside_their_day() {
        let start = 10 * DAY_MS;
        let end = start + DAY_MS;
        let row = |at: i64, input: Value| {
            json!({
                "timestamp": at.to_string(),
                "model": "claude-4.5-sonnet",
                "conversationId": "conv-1",
                "tokenUsage": {
                    "inputTokens": input,
                    "outputTokens": 1612,
                    "cacheReadTokens": 66964,
                    "cacheWriteTokens": 0,
                    "totalCents": 3.1
                }
            })
        };
        let rows = vec![
            row(start + 1_000, json!(8263)),
            // 同一毫秒、同一指纹的第二次调用单独计数。
            row(start + 1_000, json!("8263")),
            row(start - 1, json!(1)),
            row(end, json!(1)),
            json!({"timestamp": (start + 2).to_string(), "tokenUsage": {}}),
            json!({"model": "x"}),
        ];
        let parsed = events_from_rows(&rows, start, end, 0);
        assert_eq!(parsed.malformed, 1);
        assert_eq!(parsed.events.len(), 2);
        let first = &parsed.events[0];
        assert_eq!(first.tokens.input_uncached, 8263);
        assert_eq!(first.tokens.cache_read, 66964);
        assert_eq!(first.tokens.cache_write, 0);
        assert_eq!(first.tokens.output, 1612);
        assert_eq!(first.tokens.processed(), 76839);
        assert_eq!(first.session_id, "conv-1");
        assert_eq!(parsed.events[1].event_key, format!("{}#2", first.event_key));

        let after_cutoff = events_from_rows(&rows, start, end, start + 5_000);
        assert!(after_cutoff.events.is_empty());
    }

    #[test]
    fn a_page_needs_a_total_unless_the_window_is_empty() {
        assert_eq!(parse_page(&json!({})).unwrap().total, 0);
        let terminal = parse_page(&json!({"totalUsageEventsCount": 3})).unwrap();
        assert_eq!((terminal.total, terminal.rows.len()), (3, 0));
        let full = parse_page(&json!({
            "totalUsageEventsCount": "2",
            "usageEventsDisplay": [{}, {}]
        }))
        .unwrap();
        assert_eq!((full.total, full.rows.len()), (2, 2));
        assert!(parse_page(&json!({"usageEventsDisplay": []})).is_err());
        assert!(
            parse_page(&json!({"totalUsageEventsCount": 1, "usageEventsDisplay": {}})).is_err()
        );
        assert!(parse_page(&json!([])).is_err());
    }

    #[test]
    fn pages_are_collected_until_the_reported_total() {
        let mut progress = DayProgress::new("day");
        let mut requested = Vec::new();
        let step = collect_day(
            &mut progress,
            Instant::now() + Duration::from_secs(5),
            |number, _| {
                requested.push(number);
                Ok(match number {
                    1 => page(2_300, 1_000),
                    2 => page(2_300, 1_000),
                    _ => page(2_300, 300),
                })
            },
        );
        assert_eq!(step.unwrap(), DayStep::Complete);
        assert_eq!(requested, vec![1, 2, 3]);
        assert_eq!(progress.rows.len(), 2_300);
    }

    #[test]
    fn a_short_page_before_the_total_is_an_error_not_a_zero() {
        let mut progress = DayProgress::new("day");
        let step = collect_day(
            &mut progress,
            Instant::now() + Duration::from_secs(5),
            |number, _| Ok(if number == 1 { page(5, 2) } else { page(5, 0) }),
        );
        assert!(matches!(step, Err(FetchError::Invalid(_))));
    }

    #[test]
    fn a_changing_total_restarts_from_the_first_page() {
        let mut progress = DayProgress::new("day");
        let mut calls = 0;
        let step = collect_day(
            &mut progress,
            Instant::now() + Duration::from_secs(5),
            |number, _| {
                calls += 1;
                Ok(match (calls, number) {
                    (1, 1) => page(1_500, 1_000),
                    (2, 2) => page(1_501, 1_000),
                    (_, 1) => page(1_501, 1_000),
                    _ => page(1_501, 501),
                })
            },
        );
        assert_eq!(step.unwrap(), DayStep::Complete);
        assert_eq!(progress.rows.len(), 1_501);
        assert_eq!(progress.restarts, 1);
    }

    #[test]
    fn an_exhausted_budget_keeps_progress_for_the_next_snapshot() {
        let mut progress = DayProgress::new("day");
        let step = collect_day(&mut progress, Instant::now(), |_, _| {
            panic!("no request without budget")
        });
        assert_eq!(step.unwrap(), DayStep::Pending);

        progress.next_page = 2;
        progress.total = Some(1_200);
        progress.rows = page(1_200, 1_000).rows;
        let step = collect_day(
            &mut progress,
            Instant::now() + Duration::from_secs(5),
            |number, _| {
                assert_eq!(number, 2);
                Ok(page(1_200, 200))
            },
        );
        assert_eq!(step.unwrap(), DayStep::Complete);
        assert_eq!(progress.rows.len(), 1_200);
    }

    #[test]
    fn backoff_applies_to_the_failed_session_only() {
        let now = Instant::now();
        record_failure("session-a", &FetchError::RateLimited, now);
        assert!(active_backoff("session-a", now).is_some());
        assert!(active_backoff("session-b", now).is_none());
        assert!(active_backoff("session-a", now + RATE_LIMIT_BACKOFF).is_none());
        clear_failure();
        assert!(active_backoff("session-a", now).is_none());
    }
}
