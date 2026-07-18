use super::{AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{stable_hash, ParsedSource, TokenVector, UsageEvent};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

/// OpenCode 将每条消息保存为独立 JSON 文件：
/// `<data>/storage/message/<sessionID>/<messageID>.json`（新版）或
/// `<data>/storage/session/message/<sessionID>/<messageID>.json`（旧版）。
/// assistant 消息带 `tokens` 字段；正文保存在单独的 part 文件中，本 adapter 不读取。
///
/// OpenCode 1.2+ 改存 SQLite（`<data>/opencode.db`，其它发布渠道叫
/// `opencode-<channel>.db`），本 adapter 尚不支持读取。检测到库文件时通过
/// `coverage_gaps` 上报，让 UI 标"部分覆盖"——否则新版用户会看到静默的 0，
/// 读起来像"没用过"而不是"读不到"。
pub struct OpencodeAdapter {
    roots: Vec<PathBuf>,
    /// `<data>/opencode` 数据根，用于探测 SQLite 库；测试可为 None。
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
    /// `opencode-<channel>.db`。只探测文件名，不打开——我们读不了它，
    /// 探测的意义就是如实上报读不了。
    fn sqlite_stores(&self) -> Vec<String> {
        let Some(data_dir) = &self.data_dir else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(data_dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_type()
                    .map(|kind| kind.is_file())
                    .unwrap_or(false)
            })
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .filter(|name| name.starts_with("opencode") && name.ends_with(".db"))
            .collect();
        names.sort();
        names
    }
}

impl AgentAdapter for OpencodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn coverage_gaps(&self) -> Vec<String> {
        let stores = self.sqlite_stores();
        if stores.is_empty() {
            return Vec::new();
        }
        vec![format!(
            "检测到 OpenCode 1.2+ 的 SQLite 存储（{}），当前版本尚不支持读取，其中的会话未计入统计",
            stores.join("、")
        )]
    }

    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate> {
        let mut found = Vec::new();
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
    fn sqlite_store_is_reported_as_a_coverage_gap_not_silently_zero() {
        let test = TestDirectory::new("sqlite-gap");
        // 没有库文件：无 gap（未安装或旧版 JSON 用户不受影响）。
        let adapter = OpencodeAdapter::with_data_dir(test.path().to_path_buf());
        assert!(adapter.coverage_gaps().is_empty());

        // latest 渠道与其它渠道的库都要认出来；伴生的 -wal/-shm 不算独立存储。
        fs::write(test.path().join("opencode.db"), b"sqlite").unwrap();
        fs::write(test.path().join("opencode-nightly.db"), b"sqlite").unwrap();
        fs::write(test.path().join("opencode.db-wal"), b"wal").unwrap();
        let gaps = adapter.coverage_gaps();
        assert_eq!(gaps.len(), 1);
        assert!(gaps[0].contains("opencode.db"));
        assert!(gaps[0].contains("opencode-nightly.db"));
        assert!(!gaps[0].contains("db-wal"));
        assert!(gaps[0].contains("尚不支持读取"));

        // 纯 JSON 测试构造器不探测（data_dir 为 None）。
        assert!(OpencodeAdapter::with_roots(Vec::new())
            .coverage_gaps()
            .is_empty());
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
