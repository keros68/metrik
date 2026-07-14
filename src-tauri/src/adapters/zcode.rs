use super::{AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{stable_hash, ParsedSource, TokenVector, UsageEvent};
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

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
        let Ok(metadata) = self.database.metadata() else {
            return Vec::new();
        };
        // WAL 模式下写入先进 -wal，主库文件的 mtime/size 可能长期不变。
        // 把三个文件的状态合并成一个变更指纹，任何一个变化都会触发重扫。
        let mut size = metadata.len();
        let mut mtime_ns = file_mtime_ns(&metadata);
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = self.database.as_os_str().to_os_string();
            sidecar.push(suffix);
            if let Ok(sidecar_meta) = std::fs::metadata(PathBuf::from(sidecar)) {
                size += sidecar_meta.len();
                mtime_ns = mtime_ns.max(file_mtime_ns(&sidecar_meta));
            }
        }
        if mtime_ns / 1_000_000 < cutoff_ms {
            return Vec::new();
        }
        let normalized = {
            let value = self.database.to_string_lossy().replace('\\', "/");
            if cfg!(windows) {
                value.to_lowercase()
            } else {
                value
            }
        };
        vec![SourceCandidate {
            source_id: stable_hash(&format!("zcode|{normalized}")),
            path: self.database.clone(),
            size,
            mtime_ns,
        }]
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        let connection = Connection::open_with_flags(
            &candidate.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("failed to open {}", candidate.path.display()))?;
        connection.pragma_update(None, "busy_timeout", 2_000_i64)?;

        let mut statement = connection.prepare(
            "SELECT id, session_id, model_id,
                    COALESCE(completed_at, started_at),
                    COALESCE(input_tokens, 0), COALESCE(output_tokens, 0),
                    COALESCE(reasoning_tokens, 0),
                    COALESCE(cache_creation_input_tokens, 0),
                    COALESCE(cache_read_input_tokens, 0)
             FROM model_usage
             WHERE COALESCE(completed_at, started_at) >= ?1
             ORDER BY COALESCE(completed_at, started_at)",
        )?;
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
            events.push(UsageEvent::new(
                "zcode",
                format!("request:{id}"),
                occurred_at_ms,
                session_id,
                model,
                tokens,
                "exact",
            ));
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

fn file_mtime_ns(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
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
        // 被取消但已产生用量的请求按实际消耗入账，用 started_at 兜底。
        let second = &scan.source.events[1];
        assert_eq!(second.occurred_at_ms, 1_783_242_620_000);
        assert_eq!(second.tokens.processed(), 105);
        assert!(!scan.diagnostics.is_partial());
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
        assert!(after.size > before.size);
    }
}
