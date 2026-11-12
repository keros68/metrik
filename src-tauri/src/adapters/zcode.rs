use super::{AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{ParsedSource, TokenVector, UsageEvent};
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;

/// ZCode（智谱 GLM coding plan 桌面端/CLI）把逐请求用量写进
/// `~/.zcode/cli/db/db.sqlite` 的 `model_usage` 表，主会话与子代理共用，
/// 每行有全局唯一 `id` 与毫秒时间戳。本 adapter 只读统计列，不读消息内容表。
pub struct ZcodeAdapter {
    database: PathBuf,
}

impl ZcodeAdapter {
    pub fn detected() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            database: home.join(".zcode").join("cli").join("db").join("db.sqlite"),
        }
    }

    #[cfg(test)]
    fn with_database(database: PathBuf) -> Self {
        Self { database }
    }
}

impl AgentAdapter for ZcodeAdapter {
    fn id(&self) -> &'static str {
        "zcode"
    }

    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate> {
        super::sqlite_candidate(&self.database, self.id(), cutoff_ms)
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        let connection = Connection::open_with_flags(
            &candidate.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("failed to open {}", candidate.path.display()))?;
        connection.pragma_update(None, "busy_timeout", 2_000_i64)?;
        let session_directories = super::opencode_session_directories(&connection);
        // status 是计时的可选证据；旧库缺这一列时仍读取原有 token，计时留空。
        let has_status = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('model_usage') WHERE name = 'status')",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        let completed = if has_status {
            "CASE WHEN status = 'completed' THEN 1 ELSE 0 END"
        } else {
            "0"
        };

        let mut statement = connection.prepare(&format!(
            "SELECT id, session_id, model_id,
                    COALESCE(completed_at, started_at),
                    COALESCE(input_tokens, 0), COALESCE(output_tokens, 0),
                    COALESCE(reasoning_tokens, 0),
                    COALESCE(cache_creation_input_tokens, 0),
                    COALESCE(cache_read_input_tokens, 0),
                    started_at, completed_at, {completed}
             FROM model_usage
             WHERE COALESCE(completed_at, started_at) >= ?1
             ORDER BY COALESCE(completed_at, started_at)",
        ))?;
        let rows = statement.query_map([cutoff_ms], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                // 可选计时字段损坏时不能丢掉仍可计量的 token。
                row.get::<_, Option<i64>>(9).ok().flatten(),
                row.get::<_, Option<i64>>(10).ok().flatten(),
                row.get::<_, bool>(11)?,
            ))
        })?;

        let mut events = Vec::new();
        let mut diagnostics = ScanDiagnostics::default();
        for row in rows {
            let Ok((
                id,
                session_id,
                model,
                occurred_at_ms,
                input,
                output,
                reasoning,
                cache_write,
                cache_read,
                started_at,
                completed_at,
                completed,
            )) = row
            else {
                diagnostics.malformed_lines += 1;
                continue;
            };
            // model_usage 的 input_tokens 已包含缓存读取；拆回未缓存部分，
            // 保持 processed = 未缓存输入 + 缓存读 + 缓存写 + 输出的统一口径。
            let tokens = TokenVector {
                input_uncached: (input - cache_read - cache_write).max(0),
                cache_read: cache_read.max(0),
                cache_write: cache_write.max(0),
                output: output.max(0),
                reasoning_output: reasoning.max(0),
            };
            if tokens.processed() == 0 {
                continue;
            }
            let project = session_directories.get(&session_id).cloned();
            // 这是完成响应的墙钟耗时，包含等待等开销，不是首末 token 的纯生成时长。
            let response_start = started_at.filter(|_| completed && tokens.output > 0);
            events.push(
                UsageEvent::new(
                    "zcode",
                    format!("request:{id}"),
                    occurred_at_ms,
                    session_id,
                    model,
                    tokens,
                    "exact",
                )
                .with_project(project)
                .with_response_timing(response_start, completed_at),
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "metrik-zcode-{label}-{}-{}",
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

    fn create_fixture_db(path: &Path) -> Connection {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE model_usage (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    provider_id TEXT,
                    model_id TEXT,
                    status TEXT,
                    started_at INTEGER,
                    completed_at INTEGER,
                    input_tokens INTEGER,
                    output_tokens INTEGER,
                    reasoning_tokens INTEGER,
                    cache_creation_input_tokens INTEGER,
                    cache_read_input_tokens INTEGER,
                    computed_total_tokens INTEGER
                );",
            )
            .unwrap();
        connection
    }

    #[test]
    fn model_usage_rows_become_exact_events_with_uncached_input_split() {
        let test = TestDirectory::new("basic");
        let db_path = test.path().join("db.sqlite");
        let fixture = create_fixture_db(&db_path);
        fixture
            .execute_batch(
                "INSERT INTO model_usage VALUES
                 ('req-1', 'sess-a', 'builtin:bigmodel-coding-plan', 'GLM-5.2', 'completed',
                  1783242608365, 1783242613824, 15265, 247, 0, 0, 14656, 15512),
                 ('req-2', 'sess-a', 'builtin:bigmodel-coding-plan', 'GLM-5.2', 'cancelled',
                  1783242620000, NULL, 100, 5, 0, 0, 0, 105);",
            )
            .unwrap();
        drop(fixture);
        let adapter = ZcodeAdapter::with_database(db_path);

        let candidates = adapter.discover(0);
        assert_eq!(candidates.len(), 1);
        let scan = adapter.parse(&candidates[0], 0).unwrap();

        assert_eq!(scan.source.events.len(), 2);
        let first = &scan.source.events[0];
        assert_eq!(first.event_key, "request:req-1");
        assert_eq!(first.occurred_at_ms, 1_783_242_613_824);
        assert_eq!(first.model.as_deref(), Some("GLM-5.2"));
        assert_eq!(first.tokens.input_uncached, 15265 - 14656);
        assert_eq!(first.tokens.cache_read, 14656);
        assert_eq!(first.tokens.processed(), 15265 + 247);
        assert_eq!(first.response_duration_ms, Some(5_459));
        // 被取消但已产生用量的请求按实际消耗入账，用 started_at 兜底。
        let second = &scan.source.events[1];
        assert_eq!(second.occurred_at_ms, 1_783_242_620_000);
        assert_eq!(second.tokens.processed(), 105);
        assert_eq!(second.response_duration_ms, None);
        assert!(!scan.diagnostics.is_partial());
    }

    #[test]
    fn response_timing_requires_a_completed_request_and_valid_positive_bounds() {
        let test = TestDirectory::new("response-timing");
        let db_path = test.path().join("db.sqlite");
        let fixture = create_fixture_db(&db_path);
        let cases = [
            (
                "valid",
                Some("completed"),
                Some(1_000),
                Some(4_500),
                50,
                Some(3_500),
            ),
            (
                "missing-start",
                Some("completed"),
                None,
                Some(4_500),
                50,
                None,
            ),
            (
                "missing-end",
                Some("completed"),
                Some(1_000),
                None,
                50,
                None,
            ),
            (
                "zero-start",
                Some("completed"),
                Some(0),
                Some(4_500),
                50,
                None,
            ),
            (
                "negative-start",
                Some("completed"),
                Some(-1),
                Some(4_500),
                50,
                None,
            ),
            (
                "equal",
                Some("completed"),
                Some(1_000),
                Some(1_000),
                50,
                None,
            ),
            (
                "reversed",
                Some("completed"),
                Some(4_500),
                Some(1_000),
                50,
                None,
            ),
            (
                "overflow",
                Some("completed"),
                Some(i64::MIN),
                Some(i64::MAX),
                50,
                None,
            ),
            (
                "cancelled",
                Some("cancelled"),
                Some(1_000),
                Some(4_500),
                50,
                None,
            ),
            ("error", Some("error"), Some(1_000), Some(4_500), 50, None),
            ("unknown-status", None, Some(1_000), Some(4_500), 50, None),
            (
                "zero-output",
                Some("completed"),
                Some(1_000),
                Some(4_500),
                0,
                None,
            ),
        ];
        for (id, status, started_at, completed_at, output, _) in &cases {
            fixture
                .execute(
                    "INSERT INTO model_usage
                     (id, session_id, model_id, status, started_at, completed_at,
                      input_tokens, output_tokens, reasoning_tokens,
                      cache_creation_input_tokens, cache_read_input_tokens)
                     VALUES (?1, 'sess-a', 'GLM-5.2', ?2, ?3, ?4, 100, ?5, 7, 0, 10)",
                    rusqlite::params![id, status, started_at, completed_at, output],
                )
                .unwrap();
        }
        // completed_at 仍可用于原有事件归日；新增的可选开始时间坏了不应丢 token。
        fixture
            .execute_batch(
                "INSERT INTO model_usage
                 (id, session_id, model_id, status, started_at, completed_at,
                  input_tokens, output_tokens, reasoning_tokens,
                  cache_creation_input_tokens, cache_read_input_tokens)
                 VALUES ('bad-start-type', 'sess-a', 'GLM-5.2', 'completed',
                         'not-a-timestamp', 4500, 100, 50, 7, 0, 10);",
            )
            .unwrap();
        drop(fixture);
        let adapter = ZcodeAdapter::with_database(db_path);
        let scan = adapter.parse(&adapter.discover(0)[0], 0).unwrap();
        assert!(!scan.diagnostics.is_partial());
        assert_eq!(scan.source.events.len(), cases.len() + 1);
        for (id, _, started_at, completed_at, output, duration) in cases {
            let event_key = format!("request:{id}");
            let event = scan
                .source
                .events
                .iter()
                .find(|event| event.event_key == event_key)
                .unwrap();
            let previous_event = UsageEvent::new(
                "zcode",
                event_key,
                completed_at.or(started_at).unwrap(),
                "sess-a".into(),
                Some("GLM-5.2".into()),
                TokenVector {
                    input_uncached: 90,
                    cache_read: 10,
                    cache_write: 0,
                    output,
                    reasoning_output: 7,
                },
                "exact",
            );
            assert_eq!(event.response_duration_ms, duration, "{id}");
            assert_eq!(event.tokens.processed(), 100 + output, "{id}");
            assert_eq!(event.tokens.output, output, "{id}");
            assert_eq!(event.event_id, previous_event.event_id, "{id}");
            assert!(event.same_measured_payload(&previous_event), "{id}");
        }
        let bad_start = scan
            .source
            .events
            .iter()
            .find(|event| event.event_key == "request:bad-start-type")
            .unwrap();
        assert_eq!(bad_start.response_duration_ms, None);
        assert_eq!(bad_start.tokens.processed(), 150);
    }

    #[test]
    fn a_legacy_database_without_status_keeps_usage_and_leaves_timing_unknown() {
        let test = TestDirectory::new("legacy-timing");
        let db_path = test.path().join("db.sqlite");
        let fixture = create_fixture_db(&db_path);
        fixture
            .execute_batch(
                "ALTER TABLE model_usage DROP COLUMN status;
                 INSERT INTO model_usage
                 (id, session_id, model_id, started_at, completed_at,
                  input_tokens, output_tokens, reasoning_tokens,
                  cache_creation_input_tokens, cache_read_input_tokens)
                 VALUES ('legacy', 'sess-a', 'GLM-5.2', 1000, 4500, 100, 50, 7, 0, 10);",
            )
            .unwrap();
        drop(fixture);
        let adapter = ZcodeAdapter::with_database(db_path);
        let scan = adapter.parse(&adapter.discover(0)[0], 0).unwrap();
        assert!(!scan.diagnostics.is_partial());
        assert_eq!(scan.source.events.len(), 1);
        let event = &scan.source.events[0];
        assert_eq!(event.event_key, "request:legacy");
        assert_eq!(event.occurred_at_ms, 4_500);
        assert_eq!(event.tokens.processed(), 150);
        assert_eq!(event.tokens.output, 50);
        assert_eq!(event.response_duration_ms, None);
    }

    #[test]
    fn cutoff_filters_rows_and_zero_usage_rows_are_skipped() {
        let test = TestDirectory::new("cutoff");
        let db_path = test.path().join("db.sqlite");
        let fixture = create_fixture_db(&db_path);
        fixture
            .execute_batch(
                "INSERT INTO model_usage VALUES
                 ('req-old', 'sess-a', 'p', 'GLM-5.2', 'completed', 500, 900, 10, 1, 0, 0, 0, 11),
                 ('req-zero', 'sess-a', 'p', 'GLM-5.2', 'error', 2000, 2100, 0, 0, 0, 0, 0, 0),
                 ('req-new', 'sess-a', 'p', 'GLM-5.2', 'completed', 2000, 2500, 20, 2, 0, 0, 0, 22);",
            )
            .unwrap();
        drop(fixture);
        let adapter = ZcodeAdapter::with_database(db_path);

        let candidates = adapter.discover(0);
        let scan = adapter.parse(&candidates[0], 1_000).unwrap();

        assert_eq!(scan.source.events.len(), 1);
        assert_eq!(scan.source.events[0].event_key, "request:req-new");
    }

    #[test]
    fn session_directory_becomes_the_project_and_is_optional() {
        let test = TestDirectory::new("project");
        let db_path = test.path().join("db.sqlite");
        let fixture = create_fixture_db(&db_path);
        fixture
            .execute_batch(
                "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT);
                 INSERT INTO session VALUES ('sess-a', 'D:\\work\\usage');
                 INSERT INTO model_usage VALUES
                 ('req-1', 'sess-a', 'p', 'GLM-5.2', 'completed', 1000, 1100, 10, 1, 0, 0, 0, 11),
                 ('req-2', 'sess-orphan', 'p', 'GLM-5.2', 'completed', 1000, 1100, 10, 1, 0, 0, 0, 11);",
            )
            .unwrap();
        drop(fixture);
        let adapter = ZcodeAdapter::with_database(db_path);

        let scan = adapter.parse(&adapter.discover(0)[0], 0).unwrap();

        assert_eq!(
            scan.source.events[0].project_path.as_deref(),
            Some("D:/work/usage")
        );
        // session 表里没有的会话保持未归属，不猜。
        assert_eq!(scan.source.events[1].project_path, None);
    }

    #[test]
    fn a_database_without_a_session_table_still_parses_usage() {
        let test = TestDirectory::new("no-session-table");
        let db_path = test.path().join("db.sqlite");
        let fixture = create_fixture_db(&db_path);
        fixture
            .execute_batch(
                "INSERT INTO model_usage VALUES
                 ('req-1', 'sess-a', 'p', 'GLM-5.2', 'completed', 1000, 1100, 10, 1, 0, 0, 0, 11);",
            )
            .unwrap();
        drop(fixture);
        let adapter = ZcodeAdapter::with_database(db_path);

        let scan = adapter.parse(&adapter.discover(0)[0], 0).unwrap();

        assert_eq!(scan.source.events.len(), 1);
        assert_eq!(scan.source.events[0].project_path, None);
    }

    #[test]
    fn missing_database_yields_no_candidates() {
        let test = TestDirectory::new("missing");
        let adapter = ZcodeAdapter::with_database(test.path().join("absent.sqlite"));
        assert!(adapter.discover(0).is_empty());
    }

    #[test]
    fn wal_sidecar_changes_alter_the_change_fingerprint() {
        let test = TestDirectory::new("wal");
        let db_path = test.path().join("db.sqlite");
        drop(create_fixture_db(&db_path));
        let adapter = ZcodeAdapter::with_database(db_path.clone());
        let before = adapter.discover(0).remove(0);

        let mut wal = db_path.as_os_str().to_os_string();
        wal.push("-wal");
        fs::write(PathBuf::from(wal), b"pretend wal contents").unwrap();
        let after = adapter.discover(0).remove(0);

        assert_eq!(before.source_id, after.source_id);
        assert_ne!(after.size, before.size);
    }
}
