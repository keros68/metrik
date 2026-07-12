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
    connection
        .pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)
        .context("failed to record database schema version")?;
    Ok(())
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
