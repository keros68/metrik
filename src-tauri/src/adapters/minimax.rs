use super::{AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{ParsedSource, TokenVector, UsageEvent};
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::PathBuf;

/// MiniMax Code（`mcode`）把逐请求用量写进
/// `<数据根>/v2/sqlite/runtime-state.sqlite` 的 `local_runtime_token_usage` 表，
/// 数据根默认 `~/.minimax`，可由 `MINIMAX_DATA_DIR`（优先）或 `MAVIS_DATA_DIR`
/// 改写（官方 FAQ agent.minimax.io/docs/cli/faq.md）。每行一次请求，`id` 是自增
/// 主键，`ts` 为毫秒；主会话、子任务与定时会话共用这张表。本 adapter 只读统计列
/// 与会话表的工作目录，不读消息内容表。
///
/// 表结构取自 CC Switch 的公开实现（session_usage_mcode.rs，v3.20.4）及其测试
/// 夹具，尚未用真实安装的数据核对。两处语义因此保守处理：
/// - `input_tokens` 按不含缓存的新输入计（与参考实现一致）；
/// - `reasoning_tokens` 按 Metrik 的统一口径视为 `output_tokens` 的子集，
///   只有某行推理数大于输出数（不可能是子集）时才把两者相加。
pub struct MinimaxAdapter {
    database: PathBuf,
}

impl MinimaxAdapter {
    pub fn detected() -> Self {
        Self {
            database: minimax_data_dir()
                .join("v2")
                .join("sqlite")
                .join("runtime-state.sqlite"),
        }
    }

    #[cfg(test)]
    fn with_database(database: PathBuf) -> Self {
        Self { database }
    }
}

/// MiniMax Code 的数据根：环境变量取第一个非空值，否则 `~/.minimax`。
pub fn minimax_data_dir() -> PathBuf {
    ["MINIMAX_DATA_DIR", "MAVIS_DATA_DIR"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_owned())
        .find(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".minimax"))
}

/// `custom_provider:router/vendor/model` 形式的模型名去掉第一段路由前缀，
/// 与参考实现一致；没有 `/` 时原样保留。
fn model_name(raw: &str) -> &str {
    raw.split_once('/').map_or(raw, |(_, model)| model)
}

/// 会话 → 工作目录。表或列不存在时返回空映射，项目归属缺失不影响用量。
fn session_workspaces(connection: &Connection) -> HashMap<String, String> {
    let Ok(mut statement) =
        connection.prepare("SELECT session_id, workspace_dir FROM local_runtime_sessions")
    else {
        return HashMap::new();
    };
    let Ok(rows) = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    }) else {
        return HashMap::new();
    };
    rows.filter_map(Result::ok)
        .filter_map(|(id, directory)| directory.map(|value| (id, value)))
        .collect()
}

impl AgentAdapter for MinimaxAdapter {
    fn id(&self) -> &'static str {
        "minimax"
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
        let workspaces = session_workspaces(&connection);

        let mut statement = connection.prepare(
            "SELECT id, session_id, model, ts,
                    COALESCE(input_tokens, 0), COALESCE(output_tokens, 0),
                    COALESCE(reasoning_tokens, 0), COALESCE(cache_read_tokens, 0),
                    COALESCE(cache_write_tokens, 0)
             FROM local_runtime_token_usage
             WHERE ts >= ?1
             ORDER BY id",
        )?;
        let rows = statement.query_map([cutoff_ms], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
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
            let Ok((id, session_id, model, ts, input, output, reasoning, cache_read, cache_write)) =
                row
            else {
                diagnostics.malformed_lines += 1;
                continue;
            };
            let (input, output, reasoning) = (input.max(0), output.max(0), reasoning.max(0));
            let output = if reasoning > output {
                output + reasoning
            } else {
                output
            };
            let tokens = TokenVector {
                input_uncached: input,
                cache_read: cache_read.max(0),
                cache_write: cache_write.max(0),
                output,
                reasoning_output: reasoning,
            };
            if tokens.processed() == 0 {
                continue;
            }
            let session_id = session_id.unwrap_or_else(|| "unknown-session".into());
            let project = workspaces.get(&session_id).cloned();
            events.push(
                UsageEvent::new(
                    self.id(),
                    format!("row:{id}"),
                    ts,
                    session_id,
                    model.as_deref().map(model_name).map(str::to_owned),
                    tokens,
                    "exact",
                )
                .with_project(project),
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
                "metrik-minimax-{label}-{}-{}",
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

    /// 表结构照搬参考实现的测试夹具（CC Switch session_usage_mcode.rs）。
    fn create_fixture_db(path: &Path) -> Connection {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE local_runtime_token_usage (
                    id INTEGER PRIMARY KEY, session_id TEXT, model TEXT, ts INTEGER,
                    input_tokens INTEGER, output_tokens INTEGER, reasoning_tokens INTEGER,
                    cache_read_tokens INTEGER, cache_write_tokens INTEGER, cost_usd REAL
                );",
            )
            .unwrap();
        connection
    }

    #[test]
    fn usage_rows_become_exact_events_with_stripped_model_names() {
        let test = TestDirectory::new("basic");
        let db_path = test.path().join("runtime-state.sqlite");
        let fixture = create_fixture_db(&db_path);
        fixture
            .execute_batch(
                "INSERT INTO local_runtime_token_usage VALUES
                 (1, 'mvs_a', 'custom_provider:router/MiniMax-M3', 1790000000000, 10, 20, 3, 40, 5, 0.25),
                 (2, 'mvs_a', 'MiniMax-M2.7', 1790000001000, 0, 0, 0, 0, 0, NULL),
                 (3, 'mvs_b', NULL, 1790000002000, 7, 2, 9, 0, 0, NULL);",
            )
            .unwrap();
        drop(fixture);
        let adapter = MinimaxAdapter::with_database(db_path);

        let candidates = adapter.discover(0);
        assert_eq!(candidates.len(), 1);
        let scan = adapter.parse(&candidates[0], 0).unwrap();

        // 全零行不入账。
        assert_eq!(scan.source.events.len(), 2);
        let first = &scan.source.events[0];
        assert_eq!(first.event_key, "row:1");
        assert_eq!(first.occurred_at_ms, 1_790_000_000_000);
        assert_eq!(first.model.as_deref(), Some("MiniMax-M3"));
        assert_eq!(first.tokens.input_uncached, 10);
        assert_eq!(first.tokens.cache_read, 40);
        assert_eq!(first.tokens.cache_write, 5);
        // 推理数不超过输出数时视为其子集，不另加。
        assert_eq!(first.tokens.output, 20);
        assert_eq!(first.tokens.processed(), 10 + 40 + 5 + 20);
        // 推理数大于输出数，不可能是子集，两者相加。
        let second = &scan.source.events[1];
        assert_eq!(second.tokens.output, 11);
        assert_eq!(second.model, None);
        assert!(!scan.diagnostics.is_partial());
    }

    #[test]
    fn cutoff_filters_rows_and_sessions_supply_the_project() {
        let test = TestDirectory::new("project");
        let db_path = test.path().join("runtime-state.sqlite");
        let fixture = create_fixture_db(&db_path);
        fixture
            .execute_batch(
                "CREATE TABLE local_runtime_sessions (
                    session_id TEXT PRIMARY KEY, title TEXT, workspace_dir TEXT
                 );
                 INSERT INTO local_runtime_sessions VALUES ('mvs_a', 'Project', 'D:\\work\\usage');
                 INSERT INTO local_runtime_token_usage VALUES
                 (1, 'mvs_a', 'MiniMax-M3', 500, 10, 1, 0, 0, 0, NULL),
                 (2, 'mvs_a', 'MiniMax-M3', 2000, 10, 1, 0, 0, 0, NULL),
                 (3, 'mvs_orphan', 'MiniMax-M3', 2000, 10, 1, 0, 0, 0, NULL);",
            )
            .unwrap();
        drop(fixture);
        let adapter = MinimaxAdapter::with_database(db_path);

        let scan = adapter.parse(&adapter.discover(0)[0], 1_000).unwrap();

        assert_eq!(scan.source.events.len(), 2);
        assert_eq!(
            scan.source.events[0].project_path.as_deref(),
            Some("D:/work/usage")
        );
        // 会话表里没有的会话保持未归属，不猜。
        assert_eq!(scan.source.events[1].project_path, None);
    }

    #[test]
    fn missing_database_yields_no_candidates() {
        let test = TestDirectory::new("missing");
        let adapter = MinimaxAdapter::with_database(test.path().join("absent.sqlite"));
        assert!(adapter.discover(0).is_empty());
    }

    #[test]
    fn router_prefix_is_stripped_only_once() {
        assert_eq!(
            model_name("custom_provider:router/vendor/model"),
            "vendor/model"
        );
        assert_eq!(model_name("MiniMax-M3"), "MiniMax-M3");
    }
}
