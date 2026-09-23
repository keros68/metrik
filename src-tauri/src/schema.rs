use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use std::collections::HashSet;

pub const CURRENT_SCHEMA_VERSION: i64 = 1;

const REQUIRED_TABLES: [(&str, &[&str]); 4] = [
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

    let has_managed_tables = has_any_managed_table(connection)?;
    let compatible = has_managed_tables && schema_is_compatible(connection)?;

    if has_managed_tables && !compatible {
        // The ledger is a derived cache. Rebuilding an incompatible early
        // schema is safer than returning a permanently unusable partial DB;
        // the source Agent logs remain untouched and will be re-indexed.
        connection.pragma_update(None, "foreign_keys", "OFF")?;
        connection.execute_batch(
            "DROP TABLE IF EXISTS event_observation;
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
    connection
        .pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)
        .context("failed to record database schema version")?;
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

fn table_has_column(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .with_context(|| format!("failed to inspect {table} schema"))?;
    let columns: HashSet<String> = statement
        .query_map([], |row| row.get(1))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(columns.contains(column))
}

fn has_any_managed_table(connection: &Connection) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS (
                 SELECT 1 FROM sqlite_master
                 WHERE type = 'table'
                   AND name IN ('scan_source', 'usage_event', 'event_observation', 'quota_snapshot')
             )",
            [],
            |row| row.get(0),
        )
        .context("failed to inspect managed database tables")
}

fn schema_is_compatible(connection: &Connection) -> Result<bool> {
    for (table, required_columns) in REQUIRED_TABLES {
        let mut statement = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .with_context(|| format!("failed to inspect {table} schema"))?;
        let columns: HashSet<String> = statement
            .query_map([], |row| row.get(1))?
            .collect::<rusqlite::Result<_>>()?;
        if required_columns
            .iter()
            .any(|column| !columns.contains(*column))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        // 回到加列之前的形态：其余表都在，只有这一列缺失。
        connection
            .execute_batch(
                "ALTER TABLE usage_event DROP COLUMN project_path;
                 ALTER TABLE usage_event DROP COLUMN request_input_tokens;
                 INSERT INTO usage_event VALUES (
                     'keep', 'codex', 'key', 1, 'session', 'gpt-5.2',
                     1, 0, 0, 1, 0, 2, 'exact', 'hash'
                 );",
            )
            .unwrap();
        assert!(!table_has_column(&connection, "usage_event", "project_path").unwrap());

        ensure_schema(&connection).unwrap();

        assert!(table_has_column(&connection, "usage_event", "project_path").unwrap());
        let kept: Option<String> = connection
            .query_row(
                "SELECT project_path FROM usage_event WHERE event_id = 'keep'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kept, None);
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
