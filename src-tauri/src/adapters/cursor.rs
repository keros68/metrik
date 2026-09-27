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
//! 事件是增量而不是累计行，身份用时间、会话、模型与分量指纹。整页拉完才落库：
//! 残缺结果若走 `replace_source` 会删掉这次没看到的旧事件。没有官方配额窗口。

use super::{AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{stable_hash, ParsedSource, TokenVector, UsageEvent};
use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FILTERED_USAGE_URL: &str = "https://cursor.com/api/dashboard/get-filtered-usage-events";
const AUTH_ME_URL: &str = "https://cursor.com/api/auth/me";
/// 仪表盘事件不在本机文件里，不能用 state.vscdb 的 mtime（它几乎每次按键都变）。
/// 五分钟一个桶：桶内快照只做元数据比对，不重复打接口。
const REFRESH_MS: i64 = 5 * 60 * 1000;
const PAGE_SIZE: i64 = 1000;
const MAX_PAGES: i64 = 15;

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
        let mut diagnostics = ScanDiagnostics::default();
        // 没有会话时不写空结果：replace_source 会把已入账的事件清掉。
        let Some(token) = read_access_token(&self.state_db)? else {
            bail!("Cursor 没有登录会话，本轮不写入");
        };
        let pages = fetch_usage_pages(&token, cutoff_ms)?;
        let parsed = events_from_pages(&pages, cutoff_ms);
        diagnostics.malformed_lines = parsed.malformed;
        let events = parsed.events;
        Ok(ParsedScan {
            source: ParsedSource {
                source_id: candidate.source_id.clone(),
                adapter_id: self.id(),
                locator: candidate.path.clone(),
                logical_key: candidate.source_id.clone(),
                size: candidate.size,
                mtime_ns: candidate.mtime_ns,
                events,
                quotas: Vec::new(),
            },
            diagnostics,
        })
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
    let mut seen = HashSet::new();
    let mut malformed = 0usize;
    for page in pages {
        let Some(rows) = page.as_array() else {
            malformed += 1;
            continue;
        };
        for row in rows {
            match event_from_row(row, cutoff_ms) {
                RowOutcome::Event(event) => {
                    if seen.insert(event.event_key.clone()) {
                        events.push(event);
                    }
                }
                RowOutcome::Skip => {}
                RowOutcome::Malformed => malformed += 1,
            }
        }
    }
    ParsedPages { events, malformed }
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

fn fetch_usage_pages(access_token: &str, cutoff_ms: i64) -> Result<Vec<Value>> {
    let sub = jwt_sub(access_token)?;
    let cookie = format!("WorkosCursorSessionToken={sub}%3A%3A{access_token}");
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(12))
        .build();
    let me = agent
        .get(AUTH_ME_URL)
        .set("Cookie", &cookie)
        .set("Accept", "application/json")
        .call()
        .map_err(map_cursor_error)?;
    let me_json: Value = serde_json::from_str(
        &me.into_string()
            .context("读取 Cursor 登录状态失败")?,
    )
    .context("Cursor 登录状态不是预期的 JSON")?;
    let user_id = json_i64(me_json.get("id"))
        .filter(|id| *id > 0)
        .context("Cursor 登录会话缺少用户标识")?;

    let end_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(cutoff_ms);

    let mut pages = Vec::new();
    let mut fetched = 0i64;
    let mut total = None;
    let mut last_page_full = false;
    let mut hit_cap = false;
    for page in 1..=MAX_PAGES {
        let body = serde_json::json!({
            "teamId": 0,
            "startDate": cutoff_ms.to_string(),
            "endDate": end_ms.to_string(),
            "page": page,
            "pageSize": PAGE_SIZE,
            "userId": user_id,
        });
        let response = agent
            .post(FILTERED_USAGE_URL)
            .set("Cookie", &cookie)
            .set("Origin", "https://cursor.com")
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .send_string(&body.to_string())
            .map_err(map_cursor_error)?;
        let json: Value = serde_json::from_str(
            &response
                .into_string()
                .context("读取 Cursor 用量响应失败")?,
        )
        .context("Cursor 用量响应不是预期的 JSON")?;
        if let Some(count) = json_i64(json.get("totalUsageEventsCount")) {
            total = Some(count.max(0));
        }
        let rows = json
            .get("usageEventsDisplay")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let count = rows.len() as i64;
        fetched += count;
        last_page_full = count >= PAGE_SIZE;
        pages.push(Value::Array(rows));
        if !last_page_full || total.is_some_and(|total| fetched >= total) {
            break;
        }
        if page == MAX_PAGES {
            hit_cap = true;
        }
    }
    if !usage_fetch_is_complete(fetched, total, last_page_full, hit_cap) {
        bail!("Cursor 用量事件未完整拉完，本轮不写入，避免用残缺结果覆盖已有账本");
    }
    Ok(pages)
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
        assert_eq!(parsed.events.len(), 1, "重复行与零 token、窗口外事件都不入账");
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
