use super::{AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{stable_hash, ParsedSource, TokenVector, UsageEvent};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

/// OpenCode 的两代存储都支持：
///
/// - 旧版 JSON 文件：`<data>/storage/message/<sessionID>/<messageID>.json`
///   （或更旧的 `storage/session/message/...`）。assistant 消息带 `tokens`；
///   正文在单独的 part 文件里，本 adapter 不读取。
/// - 1.2+ SQLite：`<data>/opencode.db`（其它发布渠道叫 `opencode-<channel>.db`），
///   `message` 表每行 `(id, session_id, data)`，`data` 是与旧 JSON 同形状的
///   消息体（tokens/modelID/time；id 与 sessionID 只在表列里）。库常驻 WAL，
///   变更检测要把 `-wal` 的 mtime/size 一并计入，否则 checkpoint 前扫不到新行。
///
/// 两代的事件身份同为 `message:<id>`，历史从 JSON 迁到库也不会重复计数。
pub struct OpencodeAdapter {
    roots: Vec<PathBuf>,
    /// `<data>/opencode` 数据根，用于发现 SQLite 库；测试可为 None。
    data_dir: Option<PathBuf>,
}

#[derive(Deserialize, Default)]
struct OpencodeMessage {
    id: Option<String>,
    #[serde(rename = "sessionID")]
    session_id: Option<String>,
    role: Option<String>,
    #[serde(rename = "modelID")]
    model_id: Option<String>,
    time: Option<OpencodeTime>,
    tokens: Option<OpencodeTokens>,
}

#[derive(Deserialize, Default)]
struct OpencodeTime {
    created: Option<i64>,
    completed: Option<i64>,
}

#[derive(Deserialize, Default)]
struct OpencodeTokens {
    #[serde(default)]
    input: i64,
    #[serde(default)]
    output: i64,
    #[serde(default)]
    reasoning: i64,
    #[serde(default)]
    cache: OpencodeCacheTokens,
}

#[derive(Deserialize, Default)]
struct OpencodeCacheTokens {
    #[serde(default)]
    read: i64,
    #[serde(default)]
    write: i64,
}

impl OpencodeAdapter {
    pub fn detected() -> Self {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|value| value.is_absolute())
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_default()
                    .join(".local")
                    .join("share")
            });
        let data_dir = data_home.join("opencode");
        let storage = data_dir.join("storage");
        Self {
            roots: vec![
                storage.join("message"),
                storage.join("session").join("message"),
            ],
            data_dir: Some(data_dir),
        }
    }

    #[cfg(test)]
    fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            data_dir: None,
        }
    }

    #[cfg(test)]
    fn with_data_dir(data_dir: PathBuf) -> Self {
        Self {
            roots: Vec::new(),
            data_dir: Some(data_dir),
        }
    }

    /// `<data>` 下的 SQLite 库：`opencode.db`（latest/beta 渠道）或
    /// `opencode-<channel>.db`。
    fn sqlite_stores(&self) -> Vec<PathBuf> {
        let Some(data_dir) = &self.data_dir else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(data_dir) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_type()
                    .map(|kind| kind.is_file())
                    .unwrap_or(false)
            })
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .map(|name| name.starts_with("opencode") && name.ends_with(".db"))
                    .unwrap_or(false)
            })
            .map(|entry| entry.path())
            .collect();
        paths.sort();
        paths
    }
}

/// 文件 mtime（纳秒）；拿不到记 0，让候选仍然成立（宁可多扫一次）。
fn mtime_ns(path: &std::path::Path) -> i64 {
    path.metadata()
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

impl AgentAdapter for OpencodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate> {
        let mut found = Vec::new();
        // SQLite 库整体是一个源：WAL 未 checkpoint 时主文件不变，size/mtime
        // 要把 -wal 计入，否则新会话在 checkpoint 前扫不到。库不按 cutoff
        // 过滤（历史行仍在其中，行级时间在 parse 里过滤）。
        for db_path in self.sqlite_stores() {
            let Ok(metadata) = db_path.metadata() else {
                continue;
            };
            let mut wal_path = db_path.as_os_str().to_os_string();
            wal_path.push("-wal");
            let wal_path = PathBuf::from(wal_path);
            let wal_size = wal_path.metadata().map(|meta| meta.len()).unwrap_or(0);
            let normalized = normalize_locator(&db_path);
            found.push(SourceCandidate {
                source_id: stable_hash(&format!("opencode|{normalized}")),
                size: metadata.len() + wal_size,
                mtime_ns: mtime_ns(&db_path).max(mtime_ns(&wal_path)),
                path: db_path,
            });
        }
        for root in self.roots.iter().filter(|root| root.exists()) {
            for entry in WalkDir::new(root)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file())
            {
                let path = entry.into_path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let Ok(metadata) = path.metadata() else {
                    continue;
                };
                let Ok(modified) = metadata.modified() else {
                    continue;
                };
                let Ok(since_epoch) = modified.duration_since(UNIX_EPOCH) else {
                    continue;
                };
                let mtime_ns = since_epoch.as_nanos().min(i64::MAX as u128) as i64;
                if mtime_ns / 1_000_000 < cutoff_ms {
                    continue;
                }
                let normalized = normalize_locator(&path);
                found.push(SourceCandidate {
                    source_id: stable_hash(&format!("opencode|{normalized}")),
                    path,
                    size: metadata.len(),
                    mtime_ns,
                });
            }
        }
        found.sort_by(|left, right| left.path.cmp(&right.path));
        found
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        if candidate.path.extension().and_then(|value| value.to_str()) == Some("db") {
            return self.parse_sqlite(candidate, cutoff_ms);
        }
        let raw = std::fs::read_to_string(&candidate.path)
            .with_context(|| format!("failed to read {}", candidate.path.display()))?;
        let mut diagnostics = ScanDiagnostics::default();
        let mut events = Vec::new();

        match serde_json::from_str::<OpencodeMessage>(&raw) {
            Ok(message) => {
                if let Some(event) = usage_event(&candidate.path, message, cutoff_ms) {
                    events.push(event);
                }
            }
            Err(_) => {
                diagnostics.malformed_lines += 1;
            }
        }

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
}

impl OpencodeAdapter {
    /// 1.2+ SQLite：`message` 表逐行读 `data` JSON；id 与 session 在表列里
    /// （data 里没有）。行内消息体与旧 JSON 文件同构，走同一套语义。
    fn parse_sqlite(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        let connection = rusqlite::Connection::open_with_flags(
            &candidate.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .with_context(|| format!("failed to open {}", candidate.path.display()))?;
        let mut statement = connection
            .prepare("SELECT id, session_id, data FROM message")
            .context("opencode.db 缺少预期的 message 表")?;
        let mut rows = statement
            .query([])
            .context("读取 opencode.db message 表失败")?;

        let mut diagnostics = ScanDiagnostics::default();
        let mut events = Vec::new();
        while let Some(row) = rows.next().context("遍历 opencode.db 行失败")? {
            let (message_id, session_id, raw): (String, Option<String>, String) =
                match (row.get(0), row.get(1), row.get(2)) {
                    (Ok(id), Ok(session), Ok(data)) => (id, session, data),
                    _ => {
                        diagnostics.unreadable_lines += 1;
                        continue;
                    }
                };
            let Ok(mut message) = serde_json::from_str::<OpencodeMessage>(&raw) else {
                diagnostics.malformed_lines += 1;
                continue;
            };
            message.id = Some(message_id);
            message.session_id = message.session_id.take().or(session_id);
            if let Some(event) = usage_event(&candidate.path, message, cutoff_ms) {
                events.push(event);
            }
        }
        events.sort_by_key(|event| event.occurred_at_ms);

        let logical_key = candidate
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("opencode.db")
            .to_owned();
        Ok(ParsedScan {
            source: ParsedSource {
                source_id: candidate.source_id.clone(),
                adapter_id: self.id(),
                locator: candidate.path.clone(),
                logical_key,
                size: candidate.size,
                mtime_ns: candidate.mtime_ns,
                events,
                quotas: Vec::new(),
            },
            diagnostics,
        })
    }
}

fn usage_event(
    path: &std::path::Path,
    message: OpencodeMessage,
    cutoff_ms: i64,
) -> Option<UsageEvent> {
    if message.role.as_deref() != Some("assistant") {
        return None;
    }
    let tokens = message.tokens?;
    let time = message.time.unwrap_or_default();
    let timestamp = time.completed.or(time.created)?;
    if timestamp < cutoff_ms {
        return None;
    }
    let message_id = message.id.or_else(|| {
        path.file_stem()
            .and_then(|value| value.to_str())
            .map(str::to_owned)
    })?;
    let vector = TokenVector {
        input_uncached: tokens.input.max(0),
        cache_read: tokens.cache.read.max(0),
        cache_write: tokens.cache.write.max(0),
        output: tokens.output.max(0),
        // OpenCode 单独上报 reasoning；与其他 adapter 一致仅作 output 子项，
        // 不再次计入处理总量。
        reasoning_output: tokens.reasoning.max(0),
    };
    if vector.processed() == 0 {
        return None;
    }
    Some(UsageEvent::new(
        "opencode",
        format!("message:{message_id}"),
        timestamp,
        message.session_id.unwrap_or_else(|| "unknown".into()),
        message.model_id,
        vector,
        "exact",
    ))
}

fn normalize_locator(path: &std::path::Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "metrik-opencode-{label}-{}-{}",
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

    fn write_message(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let session = dir.join("ses_a");
        fs::create_dir_all(&session).unwrap();
        let path = session.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn sqlite_message_rows_become_events_via_discover_and_parse() {
        let test = TestDirectory::new("sqlite-read");
        let db_path = test.path().join("opencode.db");
        let connection = rusqlite::Connection::open(&db_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE message (id TEXT, session_id TEXT, time_created INTEGER, data TEXT);",
            )
            .unwrap();
        // assistant 行：id/session 在列里，data 里没有（对齐真实库形状）。
        connection
            .execute(
                "INSERT INTO message VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    "msg_db_1",
                    "ses_db",
                    1_784_000_000_000_i64,
                    r#"{"role":"assistant","modelID":"big-pickle","providerID":"opencode",
                        "time":{"created":1784000000000,"completed":1784000009000},
                        "tokens":{"input":2014,"output":103,"reasoning":235,"cache":{"read":14784,"write":0}}}"#,
                ],
            )
            .unwrap();
        // user 行不计。
        connection
            .execute(
                "INSERT INTO message VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    "msg_db_user",
                    "ses_db",
                    1_784_000_000_000_i64,
                    r#"{"role":"user","time":{"created":1784000000000}}"#,
                ],
            )
            .unwrap();
        drop(connection);

        let adapter = OpencodeAdapter::with_data_dir(test.path().to_path_buf());
        let scans = scan(&adapter, 0);
        assert_eq!(scans.len(), 1, "库应作为单一源被发现");
        let events = &scans[0].source.events;
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.event_key, "message:msg_db_1");
        assert_eq!(event.session_id, "ses_db");
        assert_eq!(event.model.as_deref(), Some("big-pickle"));
        assert_eq!(event.occurred_at_ms, 1_784_000_009_000);
        assert_eq!(event.tokens.processed(), 2014 + 103 + 14784);
        assert_eq!(event.tokens.reasoning_output, 235);
        assert!(!scans[0].diagnostics.is_partial());
    }

    #[test]
    fn no_sqlite_store_means_no_db_candidate() {
        let test = TestDirectory::new("no-db");
        let adapter = OpencodeAdapter::with_data_dir(test.path().to_path_buf());
        assert!(adapter.discover(0).is_empty());
    }

    fn scan(adapter: &OpencodeAdapter, cutoff_ms: i64) -> Vec<ParsedScan> {
        adapter
            .discover(cutoff_ms)
            .iter()
            .map(|candidate| adapter.parse(candidate, cutoff_ms).unwrap())
            .collect()
    }

    #[test]
    fn assistant_message_tokens_become_one_exact_event() {
        let test = TestDirectory::new("assistant");
        write_message(
            test.path(),
            "msg_1.json",
            r#"{
                "id": "msg_1",
                "sessionID": "ses_a",
                "role": "assistant",
                "modelID": "claude-sonnet-4-5",
                "time": { "created": 1760000000000, "completed": 1760000009000 },
                "tokens": {
                    "input": 120, "output": 45, "reasoning": 10,
                    "cache": { "read": 900, "write": 300 }
                }
            }"#,
        );
        let adapter = OpencodeAdapter::with_roots(vec![test.path().to_path_buf()]);

        let scans = scan(&adapter, 0);

        assert_eq!(scans.len(), 1);
        let events = &scans[0].source.events;
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.adapter_id, "opencode");
        assert_eq!(event.event_key, "message:msg_1");
        assert_eq!(event.occurred_at_ms, 1_760_000_009_000);
        assert_eq!(event.session_id, "ses_a");
        assert_eq!(event.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(event.tokens.processed(), 120 + 900 + 300 + 45);
        assert_eq!(event.tokens.reasoning_output, 10);
        assert!(!scans[0].diagnostics.is_partial());
    }

    #[test]
    fn user_messages_and_zero_usage_are_skipped_without_diagnostics() {
        let test = TestDirectory::new("skips");
        write_message(
            test.path(),
            "msg_user.json",
            r#"{"id":"msg_user","sessionID":"ses_a","role":"user","time":{"created":1760000000000}}"#,
        );
        write_message(
            test.path(),
            "msg_empty.json",
            r#"{"id":"msg_empty","sessionID":"ses_a","role":"assistant",
               "time":{"created":1760000000000},
               "tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}}}"#,
        );
        let adapter = OpencodeAdapter::with_roots(vec![test.path().to_path_buf()]);

        let scans = scan(&adapter, 0);

        assert_eq!(scans.len(), 2);
        for parsed in &scans {
            assert!(parsed.source.events.is_empty());
            assert!(!parsed.diagnostics.is_partial());
        }
    }

    #[test]
    fn malformed_message_file_downgrades_to_partial() {
        let test = TestDirectory::new("malformed");
        write_message(test.path(), "msg_bad.json", "{not json");
        let adapter = OpencodeAdapter::with_roots(vec![test.path().to_path_buf()]);

        let scans = scan(&adapter, 0);

        assert_eq!(scans.len(), 1);
        assert!(scans[0].source.events.is_empty());
        assert_eq!(scans[0].diagnostics.malformed_lines, 1);
        assert!(scans[0].diagnostics.is_partial());
    }

    #[test]
    fn events_before_the_cutoff_are_not_returned() {
        let test = TestDirectory::new("cutoff");
        write_message(
            test.path(),
            "msg_old.json",
            r#"{"id":"msg_old","sessionID":"ses_a","role":"assistant",
               "time":{"completed":1000},
               "tokens":{"input":5,"output":5,"reasoning":0,"cache":{"read":0,"write":0}}}"#,
        );
        let adapter = OpencodeAdapter::with_roots(vec![test.path().to_path_buf()]);

        let scans = scan(&adapter, 0);
        assert_eq!(scans[0].source.events.len(), 1);

        let candidates = adapter.discover(0);
        let rescanned = adapter.parse(&candidates[0], 2_000).unwrap();
        assert!(rescanned.source.events.is_empty());
    }

    #[test]
    fn incomplete_message_uses_created_time_and_updates_in_place() {
        let test = TestDirectory::new("progressive");
        let path = write_message(
            test.path(),
            "msg_2.json",
            r#"{"id":"msg_2","sessionID":"ses_a","role":"assistant",
               "time":{"created":1760000000000},
               "tokens":{"input":10,"output":1,"reasoning":0,"cache":{"read":0,"write":0}}}"#,
        );
        let adapter = OpencodeAdapter::with_roots(vec![test.path().to_path_buf()]);
        let first = scan(&adapter, 0);
        assert_eq!(first[0].source.events[0].occurred_at_ms, 1_760_000_000_000);
        assert_eq!(first[0].source.events[0].tokens.processed(), 11);

        fs::write(
            &path,
            r#"{"id":"msg_2","sessionID":"ses_a","role":"assistant",
               "time":{"created":1760000000000,"completed":1760000020000},
               "tokens":{"input":10,"output":40,"reasoning":5,"cache":{"read":0,"write":0}}}"#,
        )
        .unwrap();
        let second = scan(&adapter, 0);
        let event = &second[0].source.events[0];
        assert_eq!(event.occurred_at_ms, 1_760_000_020_000);
        assert_eq!(event.tokens.processed(), 50);
        assert_eq!(event.event_key, "message:msg_2");
    }
}
