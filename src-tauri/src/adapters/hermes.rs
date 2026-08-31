//! Hermes（Nous Research 的 agent CLI，v0.20 核对）把逐次 API 调用的用量增量
//! 累加进 `~/.hermes/state.db` 的 `session_model_usage` 表：主键
//! `(session_id, model, billing_provider, billing_base_url, billing_mode, task)`，
//! 每次 API 调用 ON CONFLICT 累加（hermes_state.py `_record_model_usage`）。
//! 本 adapter 只读用量与 cwd 列，不读消息内容表。
//!
//! 计量口径（2026-08-31 按 hermes 源码 agent/usage_pricing.py `normalize_usage`
//! 与本机数据核实）：
//! - `input_tokens` 已**剔除**缓存（`prompt_total - cache_read - cache_write`），
//!   与本账本 `input_uncached_tokens` 同口径，原样入账；
//! - `reasoning_tokens` 取自 Responses/Chat Completions 的
//!   `*_details.reasoning_tokens`，是 output 的子项，只作展示明细；
//! - 表里没有来源自报的总量列，`disagrees_with_reported_total` 无判据可查，
//!   不做口径自检（字段语义已按源码钉死）。
//!
//! 身份与合并：行是**累计值**而不是逐调用历史，每次扫描都会重新观察到更大的
//! 数——与 Antigravity 的活会话快照同型。事件键固定为
//! `hermes:{session}|{model}|{provider}|{base_url}|{mode}|{task}`（主键全列），
//! 账本层按前缀识别做分量最大值合并（见 storage）；时间戳取 `last_seen`
//! （秒，REAL），长会话的用量落在最后一次活跃的那天——来源没有更细的历史。
//! fork/subagent 只回填 cwd 等元数据、不复制用量行，无 Codex 式重放风险。
//! 任务维度（`task`：vision / compression / title_generation …）是真实计费的
//! 辅助调用，与主循环一并入账（同 pi 的 compaction 口径）。
//!
//! 归属：hermes 是 harness，走别家 coding plan 的用量按路由记到对应计量 Agent
//! （见 `hermes_providers`）；直连 API 与无路由的记录留在 hermes。hermes 没有
//! 自己的套餐，不显示配额。

use super::{AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{stable_hash, ParsedSource, TokenVector, UsageEvent};
use crate::hermes_providers;
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

pub struct HermesAdapter {
    database: PathBuf,
}

impl HermesAdapter {
    pub fn detected() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            database: home.join(".hermes").join("state.db"),
        }
    }

    #[cfg(test)]
    fn with_database(database: PathBuf) -> Self {
        Self { database }
    }
}

impl AgentAdapter for HermesAdapter {
    fn id(&self) -> &'static str {
        "hermes"
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
            source_id: stable_hash(&format!("hermes|{normalized}")),
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

        // last_seen 是秒（REAL），cutoff 是毫秒：换算后比较。
        let cutoff_seconds = cutoff_ms as f64 / 1_000.0;
        let mut statement = match connection.prepare(
            "SELECT u.session_id, u.model, u.billing_provider, u.billing_base_url,
                    u.billing_mode, u.task,
                    u.input_tokens, u.output_tokens, u.cache_read_tokens,
                    u.cache_write_tokens, u.reasoning_tokens,
                    u.last_seen, s.cwd
             FROM session_model_usage u
             LEFT JOIN sessions s ON s.id = u.session_id
             WHERE u.last_seen >= ?1",
        ) {
            // 旧版 hermes 没有这张表：不是读错，是确实没有数据可读。
            Err(error) if error.to_string().contains("no such table") => {
                return Ok(ParsedScan {
                    source: ParsedSource {
                        source_id: candidate.source_id.clone(),
                        adapter_id: self.id(),
                        locator: candidate.path.clone(),
                        logical_key: candidate.source_id.clone(),
                        size: candidate.size,
                        mtime_ns: candidate.mtime_ns,
                        events: Vec::new(),
                        quotas: Vec::new(),
                    },
                    diagnostics: ScanDiagnostics::default(),
                });
            }
            other => other?,
        };
        let rows = statement.query_map([cutoff_seconds], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, Option<f64>>(11)?,
                row.get::<_, Option<String>>(12)?,
            ))
        })?;

        let mut events = Vec::new();
        let mut diagnostics = ScanDiagnostics::default();
        for row in rows {
            let Ok((
                session_id,
                model,
                provider,
                base_url,
                billing_mode,
                task,
                input,
                output,
                cache_read,
                cache_write,
                reasoning,
                last_seen_seconds,
                cwd,
            )) = row
            else {
                diagnostics.malformed_lines += 1;
                continue;
            };
            let Some(last_seen_seconds) = last_seen_seconds else {
                // 没有时间的行没法落账：计一次诊断，不让它静默消失。
                diagnostics.malformed_lines += 1;
                continue;
            };
            // 口径见模块文档：input 已剔除缓存；reasoning 是 output 子项。
            let tokens = TokenVector {
                input_uncached: input.max(0),
                cache_read: cache_read.max(0),
                cache_write: cache_write.max(0),
                output: output.max(0),
                reasoning_output: reasoning.max(0),
            };
            if tokens.processed() == 0 {
                continue;
            }
            let occurred_at_ms = (last_seen_seconds * 1_000.0).round() as i64;
            // 主键全列进事件键：同键即同一路由行，重扫时数值只增，由账本层
            // 按分量最大值合并（storage 按 `hermes:` 前缀识别）。
            let event_key = format!(
                "hermes:{session_id}|{}|{}|{}|{}|{}",
                model.as_deref().unwrap_or(""),
                provider.as_deref().unwrap_or(""),
                base_url.as_deref().unwrap_or(""),
                billing_mode.as_deref().unwrap_or(""),
                task.as_deref().unwrap_or(""),
            );
            events.push(
                UsageEvent::new(
                    hermes_providers::credited_agent(provider.as_deref(), base_url.as_deref()),
                    event_key,
                    occurred_at_ms,
                    session_id,
                    model,
                    tokens,
                    "exact",
                )
                .with_project(cwd),
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
                "metrik-hermes-{label}-{}-{}",
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
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    cwd TEXT
                );
                CREATE TABLE session_model_usage (
                    session_id TEXT NOT NULL,
                    model TEXT NOT NULL,
                    billing_provider TEXT NOT NULL DEFAULT '',
                    billing_base_url TEXT NOT NULL DEFAULT '',
                    billing_mode TEXT NOT NULL DEFAULT '',
                    task TEXT NOT NULL DEFAULT '',
                    api_call_count INTEGER NOT NULL DEFAULT 0,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                    reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                    first_seen REAL,
                    last_seen REAL,
                    PRIMARY KEY (session_id, model, billing_provider, billing_base_url, billing_mode, task)
                );",
            )
            .unwrap();
        connection
    }

    fn insert_usage(
        connection: &Connection,
        session_id: &str,
        model: &str,
        provider: &str,
        base_url: &str,
        task: &str,
        input: i64,
        cache_read: i64,
        output: i64,
        reasoning: i64,
        last_seen_seconds: f64,
    ) {
        connection
            .execute(
                "INSERT INTO session_model_usage (
                    session_id, model, billing_provider, billing_base_url, billing_mode,
                    task, input_tokens, output_tokens, cache_read_tokens,
                    cache_write_tokens, reasoning_tokens, last_seen
                ) VALUES (?1, ?2, ?3, ?4, '', ?5, ?6, ?7, ?8, 0, ?9, ?10)",
                rusqlite::params![
                    session_id,
                    model,
                    provider,
                    base_url,
                    task,
                    input,
                    output,
                    cache_read,
                    reasoning,
                    last_seen_seconds,
                ],
            )
            .unwrap();
    }

    /// 各路由归属各自的计量卡片；分量原样入账（input 已剔除缓存），reasoning
    /// 只作 output 子项不重复相加；cwd 做项目归属。
    #[test]
    fn usage_rows_are_attributed_by_route_with_verbatim_components() {
        let test = TestDirectory::new("attribution");
        let db_path = test.path().join("state.db");
        let fixture = create_fixture_db(&db_path);
        fixture
            .execute_batch("INSERT INTO sessions (id, cwd) VALUES ('sess-a', '/tmp/work');")
            .unwrap();
        // GLM Coding Plan 路由 → zcode 卡。
        insert_usage(
            &fixture,
            "sess-a",
            "glm-5.2",
            "custom",
            "https://open.bigmodel.cn/api/coding/paas/v4",
            "",
            100,
            300,
            20,
            10,
            1_788_000_000.5,
        );
        // Kimi Code 订阅路由 → kimi 卡。
        insert_usage(
            &fixture,
            "sess-a",
            "k3",
            "custom:kimi",
            "https://api.kimi.com/coding/v1",
            "",
            50,
            0,
            10,
            0,
            1_788_000_100.0,
        );
        // 直连 API → 留 hermes；辅助调用（task 非空）同样入账。
        insert_usage(
            &fixture,
            "sess-a",
            "deepseek-v4-flash",
            "custom",
            "https://token.sensenova.cn/v1",
            "compression",
            80,
            40,
            5,
            0,
            1_788_000_200.0,
        );
        drop(fixture);
        let adapter = HermesAdapter::with_database(db_path);

        let scan = adapter.parse(&adapter.discover(0).remove(0), 0).unwrap();

        assert_eq!(scan.source.events.len(), 3);
        let by_card = |card: &str, key_part: &str| {
            scan.source
                .events
                .iter()
                .find(|event| event.adapter_id == card && event.event_key.contains(key_part))
                .unwrap_or_else(|| panic!("missing {card} event for {key_part}"))
        };
        let glm = by_card("zcode", "glm-5.2");
        assert_eq!(glm.tokens.input_uncached, 100);
        assert_eq!(glm.tokens.cache_read, 300);
        assert_eq!(glm.tokens.output, 20);
        // reasoning 是 output 子项：记明细，但不进 processed。
        assert_eq!(glm.tokens.reasoning_output, 10);
        assert_eq!(glm.tokens.processed(), 100 + 300 + 20);
        assert_eq!(glm.project_path.as_deref(), Some("/tmp/work"));
        // last_seen 秒 → 毫秒。
        assert_eq!(glm.occurred_at_ms, 1_788_000_000_500);

        let kimi = by_card("kimi", "k3");
        assert_eq!(kimi.tokens.processed(), 60);

        let hermes = by_card("hermes", "deepseek-v4-flash");
        assert_eq!(hermes.tokens.input_uncached, 80);
        assert!(hermes.event_key.contains("|compression"));
        assert!(!scan.diagnostics.is_partial());
    }

    /// 累计行的重观察由账本层做分量最大值合并：事件键带 `hermes:` 前缀。
    /// 这里钉住键形状，storage 的合并测试负责账本行为。
    #[test]
    fn event_keys_are_stable_and_namespaced_by_the_full_route() {
        let test = TestDirectory::new("identity");
        let db_path = test.path().join("state.db");
        let fixture = create_fixture_db(&db_path);
        insert_usage(
            &fixture,
            "sess-1",
            "glm-5-turbo",
            "custom",
            "https://open.bigmodel.cn/api/coding/paas/v4",
            "",
            10,
            0,
            1,
            0,
            1_788_000_000.0,
        );
        drop(fixture);
        let adapter = HermesAdapter::with_database(db_path);

        let scan = adapter.parse(&adapter.discover(0).remove(0), 0).unwrap();

        assert_eq!(scan.source.events.len(), 1);
        assert_eq!(
            scan.source.events[0].event_key,
            "hermes:sess-1|glm-5-turbo|custom|https://open.bigmodel.cn/api/coding/paas/v4||",
        );
    }

    /// cutoff 按 last_seen 过滤（秒 vs 毫秒换算）；零用量行不入账。
    #[test]
    fn cutoff_filters_rows_and_zero_usage_rows_are_skipped() {
        let test = TestDirectory::new("cutoff");
        let db_path = test.path().join("state.db");
        let fixture = create_fixture_db(&db_path);
        insert_usage(
            &fixture,
            "sess-old",
            "glm-5.2",
            "custom",
            "https://open.bigmodel.cn/api/coding/paas/v4",
            "",
            10,
            0,
            1,
            0,
            900.0,
        );
        insert_usage(
            &fixture,
            "sess-zero",
            "glm-5.2",
            "custom",
            "https://open.bigmodel.cn/api/coding/paas/v4",
            "",
            0,
            0,
            0,
            0,
            2_000.0,
        );
        insert_usage(
            &fixture,
            "sess-new",
            "glm-5.2",
            "custom",
            "https://open.bigmodel.cn/api/coding/paas/v4",
            "",
            20,
            0,
            2,
            0,
            2_000.5,
        );
        drop(fixture);
        let adapter = HermesAdapter::with_database(db_path);

        // cutoff 毫秒 → 秒：1000s 之前的 sess-old 被过滤。
        let scan = adapter
            .parse(&adapter.discover(0).remove(0), 1_000_000)
            .unwrap();

        assert_eq!(scan.source.events.len(), 1);
        assert!(scan.source.events[0].session_id == "sess-new");
    }

    /// sessions 表里没有的会话保持未归属，不猜；没有 last_seen 的行进不了
    /// 查询（hermes 的 upsert 总会写 last_seen，NULL 只可能来自外部写入），
    /// 不计入诊断——不是读坏了，是本来就没有可落账的时间。
    #[test]
    fn orphan_sessions_have_no_project_and_timeless_rows_stay_out() {
        let test = TestDirectory::new("diagnostics");
        let db_path = test.path().join("state.db");
        let fixture = create_fixture_db(&db_path);
        fixture
            .execute_batch("INSERT INTO sessions (id, cwd) VALUES ('sess-a', '/tmp/a');")
            .unwrap();
        insert_usage(
            &fixture,
            "sess-a",
            "glm-5.2",
            "custom",
            "https://open.bigmodel.cn/api/coding/paas/v4",
            "",
            10,
            0,
            1,
            0,
            1_000.0,
        );
        insert_usage(
            &fixture,
            "sess-orphan",
            "glm-5.2",
            "custom",
            "https://open.bigmodel.cn/api/coding/paas/v4",
            "",
            10,
            0,
            1,
            0,
            1_000.0,
        );
        fixture
            .execute_batch(
                "INSERT INTO session_model_usage (
                    session_id, model, input_tokens, output_tokens
                ) VALUES ('sess-notime', 'glm-5.2', 5, 1)",
            )
            .unwrap();
        drop(fixture);
        let adapter = HermesAdapter::with_database(db_path);

        let scan = adapter.parse(&adapter.discover(0).remove(0), 0).unwrap();

        assert_eq!(scan.source.events.len(), 2);
        assert!(!scan
            .source
            .events
            .iter()
            .any(|event| event.session_id == "sess-notime"));
        assert!(!scan.diagnostics.is_partial());
        let orphan = scan
            .source
            .events
            .iter()
            .find(|event| event.session_id == "sess-orphan")
            .unwrap();
        assert_eq!(orphan.project_path, None);
    }

    /// 旧版 hermes 没有 session_model_usage 表：空扫描，不是错误。
    #[test]
    fn a_database_without_the_usage_table_parses_as_empty() {
        let test = TestDirectory::new("no-table");
        let db_path = test.path().join("state.db");
        let fixture = Connection::open(&db_path).unwrap();
        fixture
            .execute_batch("CREATE TABLE sessions (id TEXT PRIMARY KEY);")
            .unwrap();
        drop(fixture);
        let adapter = HermesAdapter::with_database(db_path);

        let scan = adapter.parse(&adapter.discover(0).remove(0), 0).unwrap();

        assert!(scan.source.events.is_empty());
        assert!(!scan.diagnostics.is_partial());
    }

    #[test]
    fn missing_database_yields_no_candidates() {
        let test = TestDirectory::new("missing");
        let adapter = HermesAdapter::with_database(test.path().join("absent.db"));
        assert!(adapter.discover(0).is_empty());
    }

    #[test]
    fn wal_sidecar_changes_alter_the_change_fingerprint() {
        let test = TestDirectory::new("wal");
        let db_path = test.path().join("state.db");
        drop(create_fixture_db(&db_path));
        let adapter = HermesAdapter::with_database(db_path.clone());
        let before = adapter.discover(0).remove(0);

        let mut wal = db_path.as_os_str().to_os_string();
        wal.push("-wal");
        fs::write(PathBuf::from(wal), b"pretend wal contents").unwrap();
        let after = adapter.discover(0).remove(0);

        assert_eq!(before.source_id, after.source_id);
        assert!(after.size > before.size);
    }

    #[test]
    fn detected_database_is_the_hermes_state_db() {
        let home = dirs::home_dir().unwrap_or_default();
        assert_eq!(
            HermesAdapter::detected().database,
            home.join(".hermes").join("state.db"),
        );
    }
}
