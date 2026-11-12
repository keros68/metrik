use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use std::collections::HashSet;

pub const CURRENT_SCHEMA_VERSION: i64 = 2;

const REQUIRED_TABLES: [(&str, &[&str]); 4] = [
    (
        "scan_source",
        &[
            "source_seq",
            "source_id",
            "adapter_id",
            "logical_key",
            "locator",
            "observed_size",
            "mtime_ns",
            "coverage_start_ms",
            "parser_version",
            "last_success_ms",
            "last_error",
        ],
    ),
    (
        "usage_event",
        &[
            "id",
            "adapter_id",
            "event_key",
            "occurred_at_ms",
            "session_id",
            "model",
            "input_uncached_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "output_tokens",
            "reasoning_tokens",
            "processed_tokens",
            "quality",
            "source_seq",
        ],
    ),
    ("event_extra_source", &["event_id", "source_seq"]),
    (
        "quota_snapshot",
        &[
            "adapter_id",
            "window_key",
            "remaining_percent",
            "resets_at_ms",
            "collected_at_ms",
            "quality",
            "source_label",
        ],
    ),
];

const V1_REQUIRED_TABLES: [(&str, &[&str]); 4] = [
    (
        "scan_source",
        &[
            "source_id",
            "adapter_id",
            "logical_key",
            "locator",
            "observed_size",
            "mtime_ns",
            "coverage_start_ms",
            "parser_version",
            "last_success_ms",
            "last_error",
        ],
    ),
    (
        "usage_event",
        &[
            "event_id",
            "adapter_id",
            "event_key",
            "occurred_at_ms",
            "session_id",
            "model",
            "input_uncached_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "output_tokens",
            "reasoning_tokens",
            "processed_tokens",
            "quality",
            "payload_hash",
        ],
    ),
    (
        "event_observation",
        &["event_id", "source_id", "observed_at_ms"],
    ),
    (
        "quota_snapshot",
        &[
            "adapter_id",
            "window_key",
            "remaining_percent",
            "resets_at_ms",
            "collected_at_ms",
            "quality",
            "source_label",
        ],
    ),
];

pub fn ensure_schema(connection: &Connection) -> Result<()> {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .context("failed to read database schema version")?;
    if version > CURRENT_SCHEMA_VERSION {
        bail!(
            "database schema version {version} is newer than supported version {CURRENT_SCHEMA_VERSION}"
        );
    }

    // 稳态快速路径：schema 已就绪时只设置连接级外键开关，不发任何 DDL 或
    // PRAGMA user_version 写事务，避免每次 open_database 争抢单写槽。
    // 注意：后续若新增表或列，必须同步递增 CURRENT_SCHEMA_VERSION，否则已是当前版本的库会走此快速路径跳过迁移。
    if version == CURRENT_SCHEMA_VERSION && v2_all_tables_present(connection)? {
        connection.pragma_update(None, "foreign_keys", "ON")?;
        return Ok(());
    }

    let has_managed_tables = has_any_managed_table(connection)?;
    let v2_compatible = has_managed_tables && schema_is_compatible(connection)?;
    let v1_compatible =
        has_managed_tables && !v2_compatible && tables_match(connection, &V1_REQUIRED_TABLES)?;

    let mut migrated_from_v1 = false;
    if v1_compatible {
        migrate_v1_to_v2(connection)?;
        migrated_from_v1 = true;
    } else if has_managed_tables && !v2_compatible {
        // The ledger is a derived cache. Rebuilding an incompatible early
        // schema is safer than returning a permanently unusable partial DB;
        // the source Agent logs remain untouched and will be re-indexed.
        connection.pragma_update(None, "foreign_keys", "OFF")?;
        connection.execute_batch(
            "DROP TABLE IF EXISTS event_observation;
             DROP TABLE IF EXISTS event_extra_source;
             DROP TABLE IF EXISTS usage_event;
             DROP TABLE IF EXISTS quota_snapshot;
             DROP TABLE IF EXISTS scan_source;",
        )?;
    }

    connection
        .execute_batch(include_str!("../migrations/001_init.sql"))
        .context("failed to initialize usage database schema")?;
    rebuild_quota_snapshot_if_check_forbids_balances(connection)?;
    ensure_optional_columns(connection)?;

    // 将既有库切换为 INCREMENTAL auto_vacuum 并一次性回收历史死页（如旧版 v1
    // event_observation 与未开启 auto_vacuum 积累的 freelist）。
    let auto_vacuum: i64 = connection
        .pragma_query_value(None, "auto_vacuum", |row| row.get(0))
        .unwrap_or(0);
    if (migrated_from_v1 || auto_vacuum != 2) && connection.is_autocommit() {
        let freelist: i64 = connection
            .pragma_query_value(None, "freelist_count", |row| row.get(0))
            .unwrap_or(0);
        if migrated_from_v1 || (auto_vacuum != 2 && freelist > 0) {
            let _ = connection.pragma_update(None, "auto_vacuum", "INCREMENTAL");
            let _ = connection.execute_batch("VACUUM;");
        }
    }

    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection
        .pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)
        .context("failed to record database schema version")?;
    Ok(())
}

fn v2_all_tables_present(connection: &Connection) -> Result<bool> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table'
               AND name IN (
                   'scan_source', 'usage_event', 'event_extra_source', 'quota_snapshot',
                   'app_setting', 'remote_usage_event', 'sync_device', 'response_speed_sample'
               )",
            [],
            |row| row.get(0),
        )
        .context("failed to inspect v2 tables")?;
    Ok(count == 8)
}

/// 无损将 v1 账本（文本主键 `event_id` + `payload_hash` + 全量 `event_observation`）
/// 升级为紧凑的 v2 形态（整型主键 + 内联 `source_seq` + 多源溢出表 `event_extra_source`）。
fn migrate_v1_to_v2(connection: &Connection) -> Result<()> {
    ensure_optional_columns(connection)?;
    connection.pragma_update(None, "foreign_keys", "OFF")?;
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;

             CREATE TABLE scan_source_v2 (
                 source_seq        INTEGER PRIMARY KEY,
                 source_id         TEXT NOT NULL UNIQUE,
                 adapter_id        TEXT NOT NULL,
                 logical_key       TEXT NOT NULL,
                 locator           TEXT NOT NULL,
                 observed_size     INTEGER NOT NULL,
                 mtime_ns          INTEGER NOT NULL,
                 coverage_start_ms INTEGER NOT NULL,
                 parser_version    INTEGER NOT NULL,
                 last_success_ms   INTEGER NOT NULL,
                 last_error        TEXT
             );

             INSERT INTO scan_source_v2 (
                 source_id, adapter_id, logical_key, locator, observed_size,
                 mtime_ns, coverage_start_ms, parser_version, last_success_ms, last_error
             )
             SELECT
                 source_id, adapter_id, logical_key, locator,
                 -- 旧版 SQLite 来源指纹将 u64 直接强转为 i64 导致最高位溢出为负数；
                 -- 按位与 i64::MAX 与新版 positive_hash_u64 保持完全一致，避免无意义重扫。
                 CASE WHEN observed_size < 0 THEN (observed_size & 9223372036854775807) ELSE observed_size END,
                 mtime_ns, coverage_start_ms, parser_version, last_success_ms, last_error
             FROM scan_source;

             CREATE TABLE usage_event_v2 (
                 id                    INTEGER PRIMARY KEY,
                 adapter_id            TEXT NOT NULL,
                 event_key             TEXT NOT NULL,
                 occurred_at_ms        INTEGER NOT NULL,
                 session_id            TEXT NOT NULL,
                 model                 TEXT,
                 input_uncached_tokens INTEGER NOT NULL CHECK (input_uncached_tokens >= 0),
                 cache_read_tokens     INTEGER NOT NULL CHECK (cache_read_tokens >= 0),
                 cache_write_tokens    INTEGER NOT NULL CHECK (cache_write_tokens >= 0),
                 output_tokens         INTEGER NOT NULL CHECK (output_tokens >= 0),
                 reasoning_tokens      INTEGER NOT NULL CHECK (reasoning_tokens >= 0),
                 processed_tokens      INTEGER NOT NULL CHECK (processed_tokens >= 0),
                 quality               TEXT NOT NULL,
                 project_path          TEXT,
                 request_input_tokens  INTEGER,
                 response_duration_ms  INTEGER,
                 source_seq            INTEGER REFERENCES scan_source(source_seq) ON DELETE SET NULL,
                 UNIQUE(adapter_id, event_key)
             );

             CREATE INDEX idx_eo_migration_event ON event_observation(event_id);

             INSERT INTO usage_event_v2 (
                 adapter_id, event_key, occurred_at_ms, session_id, model,
                 input_uncached_tokens, cache_read_tokens, cache_write_tokens,
                 output_tokens, reasoning_tokens, processed_tokens, quality,
                 project_path, request_input_tokens, response_duration_ms,
                 source_seq
             )
             SELECT
                 u.adapter_id, u.event_key, u.occurred_at_ms, u.session_id, u.model,
                 u.input_uncached_tokens, u.cache_read_tokens, u.cache_write_tokens,
                 u.output_tokens, u.reasoning_tokens, u.processed_tokens, u.quality,
                 u.project_path, u.request_input_tokens, u.response_duration_ms,
                 (
                     SELECT MIN(s.source_seq)
                     FROM event_observation AS eo
                     JOIN scan_source_v2 AS s ON s.source_id = eo.source_id
                     WHERE eo.event_id = u.event_id
                 )
             FROM usage_event AS u;

             CREATE TABLE event_extra_source (
                 event_id   INTEGER NOT NULL REFERENCES usage_event(id) ON DELETE CASCADE,
                 source_seq INTEGER NOT NULL REFERENCES scan_source(source_seq) ON DELETE CASCADE,
                 PRIMARY KEY (event_id, source_seq)
             ) WITHOUT ROWID;

             INSERT OR IGNORE INTO event_extra_source (event_id, source_seq)
             SELECT u2.id, s.source_seq
             FROM event_observation AS eo
             JOIN scan_source_v2 AS s ON s.source_id = eo.source_id
             JOIN usage_event AS u1 ON u1.event_id = eo.event_id
             JOIN usage_event_v2 AS u2
               ON u2.adapter_id = u1.adapter_id AND u2.event_key = u1.event_key
             WHERE u2.source_seq IS NOT NULL AND s.source_seq != u2.source_seq;

             DROP TABLE event_observation;
             DROP TABLE usage_event;
             DROP TABLE scan_source;

             ALTER TABLE scan_source_v2 RENAME TO scan_source;
             ALTER TABLE usage_event_v2 RENAME TO usage_event;

             COMMIT;",
        )
        .context("failed to migrate v1 usage ledger to v2")?;
    Ok(())
}

/// DeepSeek 余额按契约把金额原样存进 `remaining_percent`（可以超过 100），但
/// 早期建表的 `CHECK (remaining_percent BETWEEN 0 AND 100)` 会让余额 >100 的
/// 写入连整轮配额事务一起失败——金额在 100 以内的账户从测不出这个问题。
/// schema 兼容性只看列集合，这样的老库不会被上面的整库重建覆盖，所以在这里
/// 单独识别并重建。quota_snapshot 是整体替换的派生表：丢掉的只是旧快照，
/// 下一轮刷新即按来源回填。
fn rebuild_quota_snapshot_if_check_forbids_balances(connection: &Connection) -> Result<()> {
    let sql: Option<String> = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'quota_snapshot'",
            [],
            |row| row.get(0),
        )
        .context("failed to inspect the quota_snapshot schema")?;
    let Some(sql) = sql else {
        return Ok(());
    };
    if !sql.contains("BETWEEN 0 AND 100") {
        return Ok(());
    }
    connection
        .execute_batch("DROP TABLE quota_snapshot;")
        .context("failed to drop the outdated quota_snapshot table")?;
    connection
        .execute_batch(include_str!("../migrations/001_init.sql"))
        .context("failed to recreate quota_snapshot without the percent-only check")?;
    Ok(())
}

/// 可空的后加列：老库用 `ALTER TABLE` 补上，不进 `REQUIRED_TABLES`。
/// 进了兼容性判定就会把所有老账本判为不兼容而整库重建；这些列缺失时旧数据
/// 依然可用（该列读出 NULL），随下一次重扫补齐即可。
fn ensure_optional_columns(connection: &Connection) -> Result<()> {
    for (table, column, definition) in [
        ("usage_event", "project_path", "TEXT"),
        ("usage_event", "request_input_tokens", "INTEGER"),
        ("usage_event", "response_duration_ms", "INTEGER"),
    ] {
        if !table_has_column(connection, table, column)? {
            connection
                .execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {definition}"
                ))
                .with_context(|| format!("failed to add {table}.{column}"))?;
        }
    }
    Ok(())
}

fn table_columns(connection: &Connection, table: &str) -> Result<HashSet<String>> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .with_context(|| format!("failed to inspect {table} schema"))?;
    let columns = statement
        .query_map([], |row| row.get(1))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(columns)
}

fn table_has_column(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    Ok(table_columns(connection, table)?.contains(column))
}

fn has_any_managed_table(connection: &Connection) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS (
                 SELECT 1 FROM sqlite_master
                 WHERE type = 'table'
                   AND name IN (
                       'scan_source', 'usage_event', 'event_observation',
                       'event_extra_source', 'quota_snapshot'
                   )
             )",
            [],
            |row| row.get(0),
        )
        .context("failed to inspect managed database tables")
}

fn tables_match(connection: &Connection, required: &[(&str, &[&str])]) -> Result<bool> {
    for (table, required_columns) in required {
        let columns = table_columns(connection, table)?;
        if required_columns
            .iter()
            .any(|column| !columns.contains(*column))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn schema_is_compatible(connection: &Connection) -> Result<bool> {
    tables_match(connection, &REQUIRED_TABLES)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY_V1_SCHEMA: &str = "
        CREATE TABLE scan_source (
            source_id         TEXT PRIMARY KEY,
            adapter_id        TEXT NOT NULL,
            logical_key       TEXT NOT NULL,
            locator           TEXT NOT NULL,
            observed_size     INTEGER NOT NULL,
            mtime_ns          INTEGER NOT NULL,
            coverage_start_ms INTEGER NOT NULL,
            parser_version    INTEGER NOT NULL,
            last_success_ms   INTEGER NOT NULL,
            last_error        TEXT
        );
        CREATE TABLE usage_event (
            event_id              TEXT PRIMARY KEY,
            adapter_id            TEXT NOT NULL,
            event_key             TEXT NOT NULL,
            occurred_at_ms        INTEGER NOT NULL,
            session_id            TEXT NOT NULL,
            model                 TEXT,
            input_uncached_tokens INTEGER NOT NULL,
            cache_read_tokens     INTEGER NOT NULL,
            cache_write_tokens    INTEGER NOT NULL,
            output_tokens         INTEGER NOT NULL,
            reasoning_tokens      INTEGER NOT NULL,
            processed_tokens      INTEGER NOT NULL,
            quality               TEXT NOT NULL,
            payload_hash          TEXT NOT NULL,
            UNIQUE(adapter_id, event_key)
        );
        CREATE TABLE event_observation (
            event_id       TEXT NOT NULL REFERENCES usage_event(event_id) ON DELETE CASCADE,
            source_id      TEXT NOT NULL REFERENCES scan_source(source_id) ON DELETE CASCADE,
            observed_at_ms INTEGER NOT NULL,
            PRIMARY KEY (event_id, source_id)
        );
        CREATE TABLE quota_snapshot (
            adapter_id        TEXT NOT NULL,
            window_key        TEXT NOT NULL,
            remaining_percent REAL NOT NULL CHECK (remaining_percent >= 0),
            resets_at_ms      INTEGER,
            collected_at_ms   INTEGER NOT NULL,
            quality           TEXT NOT NULL,
            source_label      TEXT NOT NULL,
            PRIMARY KEY (adapter_id, window_key)
        );
    ";

    #[test]
    fn initializes_and_versions_an_empty_database() {
        let connection = Connection::open_in_memory().unwrap();

        ensure_schema(&connection).unwrap();

        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        assert!(schema_is_compatible(&connection).unwrap());
    }

    #[test]
    fn adopts_a_compatible_unversioned_database_without_losing_rows() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        connection
            .execute(
                "INSERT INTO scan_source (
                    source_id, adapter_id, logical_key, locator, observed_size,
                    mtime_ns, coverage_start_ms, parser_version, last_success_ms, last_error
                 ) VALUES ('keep', 'codex', 'keep', 'keep.jsonl', 1, 1, 0, 2, 1, NULL)",
                [],
            )
            .unwrap();

        ensure_schema(&connection).unwrap();

        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM scan_source", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn migrates_v1_ledger_preserving_events_and_multi_source_observations() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(LEGACY_V1_SCHEMA).unwrap();
        connection
            .execute_batch(
                "INSERT INTO scan_source VALUES
                     ('src-a', 'claude', 'src-a', 'a.jsonl', -5, 1, 0, 7, 1, NULL),
                     ('src-b', 'claude', 'src-b', 'b.jsonl', 10, 1, 0, 7, 1, NULL);
                 INSERT INTO usage_event VALUES
                     ('ev-shared', 'claude', 'message:1', 100, 's1', 'claude-sonnet', 10, 0, 0, 5, 0, 15, 'exact', 'h1'),
                     ('ev-solo',   'claude', 'message:2', 200, 's1', 'claude-sonnet', 20, 0, 0, 5, 0, 25, 'exact', 'h2');
                 INSERT INTO event_observation VALUES
                     ('ev-shared', 'src-a', 100),
                     ('ev-shared', 'src-b', 100),
                     ('ev-solo',   'src-a', 200);",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();

        ensure_schema(&connection).unwrap();

        assert!(schema_is_compatible(&connection).unwrap());
        let (events, tokens, extra): (i64, i64, i64) = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM usage_event),
                        (SELECT SUM(processed_tokens) FROM usage_event),
                        (SELECT COUNT(*) FROM event_extra_source)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((events, tokens, extra), (2, 40, 1));
        let masked_size: i64 = connection
            .query_row(
                "SELECT observed_size FROM scan_source WHERE source_id = 'src-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(masked_size >= 0);
    }

    #[test]
    fn rebuilds_an_incompatible_derived_schema() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE scan_source (source_id TEXT PRIMARY KEY, locator TEXT NOT NULL);
                 CREATE TABLE usage_event (event_id TEXT PRIMARY KEY);",
            )
            .unwrap();

        ensure_schema(&connection).unwrap();

        assert!(schema_is_compatible(&connection).unwrap());
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM scan_source", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    /// 老库的 quota_snapshot 带着只允许百分比的 CHECK：余额按契约原样入库时
    /// 连整轮配额写入一起失败。升级必须把它单独重建，且不动其余表。
    #[test]
    fn rebuilds_a_quota_snapshot_whose_check_forbids_balances() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        connection
            .execute_batch(
                "DROP TABLE quota_snapshot;
                 CREATE TABLE quota_snapshot (
                     adapter_id       TEXT NOT NULL,
                     window_key       TEXT NOT NULL,
                     remaining_percent REAL NOT NULL CHECK (remaining_percent BETWEEN 0 AND 100),
                     resets_at_ms     INTEGER,
                     collected_at_ms  INTEGER NOT NULL,
                     quality          TEXT NOT NULL,
                     source_label     TEXT NOT NULL,
                     PRIMARY KEY (adapter_id, window_key)
                 );
                 INSERT INTO scan_source (source_id, adapter_id, logical_key, locator,
                     observed_size, mtime_ns, coverage_start_ms, parser_version,
                     last_success_ms, last_error)
                 VALUES ('keep', 'codex', 'keep', 'keep.jsonl', 1, 1, 0, 2, 1, NULL);",
            )
            .unwrap();

        ensure_schema(&connection).unwrap();

        let balance = connection.execute(
            "INSERT INTO quota_snapshot (adapter_id, window_key, remaining_percent,
                 resets_at_ms, collected_at_ms, quality, source_label)
             VALUES ('deepseek', 'balance_cny', 120.0, NULL, 1, 'official_live', 'test')",
            [],
        );
        assert!(balance.is_ok(), "余额超过 100 必须能入库");
        let kept: i64 = connection
            .query_row("SELECT COUNT(*) FROM scan_source", [], |row| row.get(0))
            .unwrap();
        assert_eq!(kept, 1, "重建只针对 quota_snapshot");
    }

    #[test]
    fn adds_optional_columns_to_a_ledger_that_predates_them() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(LEGACY_V1_SCHEMA).unwrap();
        connection
            .execute_batch(
                "INSERT INTO usage_event VALUES (
                     'keep', 'codex', 'key', 1, 'session', 'gpt-5.2',
                     1, 0, 0, 1, 0, 2, 'exact', 'hash'
                 );",
            )
            .unwrap();
        assert!(!table_has_column(&connection, "usage_event", "project_path").unwrap());

        ensure_schema(&connection).unwrap();

        assert!(table_has_column(&connection, "usage_event", "project_path").unwrap());
        assert!(table_has_column(&connection, "usage_event", "response_duration_ms").unwrap());
        let kept: Option<String> = connection
            .query_row(
                "SELECT project_path FROM usage_event WHERE event_key = 'key'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kept, None);
        let timed: (i64, Option<i64>) = connection
            .query_row(
                "SELECT processed_tokens, response_duration_ms FROM usage_event WHERE event_key = 'key'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(timed, (2, None), "adding timing must retain existing usage");
    }

    #[test]
    fn refuses_a_future_schema_version() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION + 1)
            .unwrap();

        let error = ensure_schema(&connection).unwrap_err();

        assert!(error.to_string().contains("newer than supported"));
    }
}
