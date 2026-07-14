use crate::domain::{ParsedSource, QuotaSample, TokenVector, UsageEvent};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use std::path::{Path, PathBuf};

// Version 3 rebuilds Claude sources after provider message identity stopped
// depending on the optional requestId field.
// Version 4 rebuilds Codex sources so fork/subagent replay token_counts stop
// double-counting the parent thread's usage (and stop showing as unknown model).
pub const PARSER_VERSION: i64 = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplaceSourceOutcome {
    pub rejected_events: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventWriteOutcome {
    Accepted,
    RejectedClaudeModelConflict,
}

struct StoredUsageEvent {
    adapter_id: String,
    event_key: String,
    occurred_at_ms: i64,
    session_id: String,
    model: Option<String>,
    tokens: TokenVector,
    payload_hash: String,
}

pub fn open_database(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create app data directory {}", parent.display()))?;
    }
    let connection = Connection::open(path)
        .with_context(|| format!("failed to open usage database {}", path.display()))?;
    connection.pragma_update(None, "busy_timeout", 5_000_i64)?;
    crate::schema::ensure_schema(&connection)?;
    Ok(connection)
}

/// Opens the ledger for read-only queries (reports, session lists). Unlike
/// [`open_database`], this never runs schema migrations and never issues the
/// `PRAGMA user_version` write that migrations require — both would contend
/// for SQLite's single writer slot with an in-progress log scan and could
/// stall a page that must stay snappy. `SQLITE_OPEN_READ_ONLY` also refuses
/// to create a missing database file, so a ledger that hasn't been built yet
/// surfaces as an explicit error instead of hanging or fabricating a fresh
/// empty schema.
pub fn open_database_read_only(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open usage database read-only {}", path.display()))?;
    // Read-only WAL access never blocks on a concurrent writer's transaction,
    // but keep a bounded timeout so a wedged database fails fast instead of
    // hanging the calling command indefinitely.
    connection.pragma_update(None, "busy_timeout", 5_000_i64)?;
    Ok(connection)
}

/// Clears only Metrik's derived ledger rows so the source adapters can rebuild
/// them from the Agent logs. The database file and any unmanaged tables stay in
/// place; this function never opens or mutates a stored source locator.
pub fn reset_derived_ledger(path: &Path) -> Result<()> {
    let mut connection = open_database(path)?;
    reset_derived_ledger_connection(&mut connection)
}

fn reset_derived_ledger_connection(connection: &mut Connection) -> Result<()> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("failed to begin local ledger reset")?;
    transaction
        .execute_batch(
            "DELETE FROM event_observation;
             DELETE FROM usage_event;
             DELETE FROM quota_snapshot;
             DELETE FROM scan_source;",
        )
        .context("failed to clear derived usage tables")?;
    transaction
        .commit()
        .context("failed to commit local ledger reset")?;
    Ok(())
}

pub fn get_app_setting(connection: &Connection, key: &str) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT value FROM app_setting WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()
        .with_context(|| format!("failed to read setting {key}"))
}

pub fn set_app_setting(connection: &Connection, key: &str, value: &str) -> Result<()> {
    connection.execute(
        "INSERT INTO app_setting (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn source_is_current(
    connection: &Connection,
    source_id: &str,
    size: u64,
    mtime_ns: i64,
    requested_coverage_start_ms: i64,
) -> Result<bool> {
    let state = connection
        .query_row(
            "SELECT observed_size, mtime_ns, parser_version, coverage_start_ms
             FROM scan_source WHERE source_id = ?1",
            [source_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    Ok(matches!(
        state,
        Some((stored_size, stored_mtime, parser_version, coverage_start_ms))
            if stored_size == size as i64
                && stored_mtime == mtime_ns
                && parser_version == PARSER_VERSION
                && coverage_start_ms <= requested_coverage_start_ms
    ))
}

pub fn adapter_has_stale_parser_sources(connection: &Connection, adapter_id: &str) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS (
                 SELECT 1 FROM scan_source
                 WHERE adapter_id = ?1
                   AND parser_version != ?2
             )",
            params![adapter_id, PARSER_VERSION],
            |row| row.get(0),
        )
        .context("failed to inspect adapter parser versions")
}

pub fn source_needs_parser_rebuild(connection: &Connection, source_id: &str) -> Result<bool> {
    let parser_version = connection
        .query_row(
            "SELECT parser_version FROM scan_source WHERE source_id = ?1",
            [source_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    Ok(matches!(parser_version, Some(version) if version != PARSER_VERSION))
}

pub fn replace_source(
    connection: &mut Connection,
    source: &ParsedSource,
    coverage_start_ms: i64,
) -> Result<ReplaceSourceOutcome> {
    let now = Utc::now().timestamp_millis();
    let transaction = connection.transaction()?;
    let mut outcome = ReplaceSourceOutcome::default();

    transaction.execute(
        "INSERT INTO scan_source (
            source_id, adapter_id, logical_key, locator, observed_size,
            mtime_ns, coverage_start_ms, parser_version, last_success_ms, last_error
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL)
        ON CONFLICT(source_id) DO UPDATE SET
            adapter_id = excluded.adapter_id,
            logical_key = excluded.logical_key,
            locator = excluded.locator,
            observed_size = excluded.observed_size,
            mtime_ns = excluded.mtime_ns,
            coverage_start_ms = excluded.coverage_start_ms,
            parser_version = excluded.parser_version,
            last_success_ms = excluded.last_success_ms,
            last_error = NULL",
        params![
            source.source_id,
            source.adapter_id,
            source.logical_key,
            source.locator.to_string_lossy(),
            source.size as i64,
            source.mtime_ns,
            coverage_start_ms,
            PARSER_VERSION,
            now,
        ],
    )?;

    // A parser invoked with a narrow coverage window only returns events in
    // that window. Reconcile that slice and retain older observations until a
    // wider scan explicitly covers them.
    transaction.execute(
        "DELETE FROM event_observation
         WHERE source_id = ?1
           AND event_id IN (
               SELECT event_id FROM usage_event WHERE occurred_at_ms >= ?2
           )",
        params![source.source_id, coverage_start_ms],
    )?;
    delete_orphan_events(&transaction)?;

    for event in &source.events {
        let write_outcome = insert_or_merge_usage_event(&transaction, event, &source.locator)?;
        if write_outcome == EventWriteOutcome::RejectedClaudeModelConflict {
            outcome.rejected_events += 1;
            continue;
        }

        transaction.execute(
            "INSERT OR REPLACE INTO event_observation (event_id, source_id, observed_at_ms)
             VALUES (?1, ?2, ?3)",
            params![event.event_id, source.source_id, now],
        )?;
    }

    for quota in &source.quotas {
        upsert_quota_tx(&transaction, quota)?;
    }

    transaction.commit()?;
    Ok(outcome)
}

fn insert_or_merge_usage_event(
    transaction: &Transaction<'_>,
    event: &UsageEvent,
    locator: &Path,
) -> Result<EventWriteOutcome> {
    let stored = transaction
        .query_row(
            "SELECT adapter_id, event_key, occurred_at_ms, session_id, model,
                    input_uncached_tokens, cache_read_tokens, cache_write_tokens,
                    output_tokens, reasoning_tokens, payload_hash
             FROM usage_event WHERE event_id = ?1",
            [&event.event_id],
            |row| {
                Ok(StoredUsageEvent {
                    adapter_id: row.get(0)?,
                    event_key: row.get(1)?,
                    occurred_at_ms: row.get(2)?,
                    session_id: row.get(3)?,
                    model: row.get(4)?,
                    tokens: TokenVector {
                        input_uncached: row.get(5)?,
                        cache_read: row.get(6)?,
                        cache_write: row.get(7)?,
                        output: row.get(8)?,
                        reasoning_output: row.get(9)?,
                    },
                    payload_hash: row.get(10)?,
                })
            },
        )
        .optional()?;

    let Some(stored) = stored else {
        transaction.execute(
            "INSERT INTO usage_event (
                event_id, adapter_id, event_key, occurred_at_ms, session_id, model,
                input_uncached_tokens, cache_read_tokens, cache_write_tokens,
                output_tokens, reasoning_tokens, processed_tokens, quality, payload_hash
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                event.event_id,
                event.adapter_id,
                event.event_key,
                event.occurred_at_ms,
                event.session_id,
                event.model,
                event.tokens.input_uncached,
                event.tokens.cache_read,
                event.tokens.cache_write,
                event.tokens.output,
                event.tokens.reasoning_output,
                event.tokens.processed(),
                event.quality,
                event.payload_hash,
            ],
        )?;
        return Ok(EventWriteOutcome::Accepted);
    };

    // Always retain the stable-hash collision guard. A matching event_id must
    // resolve to exactly the same adapter and provider event key.
    if stored.adapter_id != event.adapter_id || stored.event_key != event.event_key {
        return Err(anyhow!(
            "event identity collision for {} from {}",
            event.event_id,
            locator.display()
        ));
    }
    // Claude emits the same provider message repeatedly while usage fields are
    // being completed, and may copy it into a branched session log. Those are
    // observations of one event, so merge each token component monotonically.
    // Fallback Claude keys and all other adapters keep strict payload matching.
    let mergeable_claude_message =
        event.adapter_id == "claude" && event.event_key.starts_with("message:");
    // A contradictory model makes the provider message ambiguous. Reject only
    // this observation; the caller will commit the source's other valid events
    // and surface partial coverage through scan diagnostics.
    if mergeable_claude_message {
        if let (Some(stored_model), Some(candidate_model)) =
            (stored.model.as_deref(), event.model.as_deref())
        {
            if stored_model != candidate_model {
                return Ok(EventWriteOutcome::RejectedClaudeModelConflict);
            }
        }
    }
    let fills_missing_model =
        mergeable_claude_message && stored.model.is_none() && event.model.is_some();
    if stored.payload_hash == event.payload_hash && !fills_missing_model {
        return Ok(EventWriteOutcome::Accepted);
    }
    if !mergeable_claude_message {
        return Err(anyhow!(
            "event identity collision for {} from {}",
            event.event_id,
            locator.display()
        ));
    }
    let mut merged_tokens = stored.tokens;
    merged_tokens.component_max(&event.tokens);
    let occurred_at_ms = stored.occurred_at_ms.max(event.occurred_at_ms);
    let session_id = if stored.session_id <= event.session_id {
        stored.session_id
    } else {
        event.session_id.clone()
    };
    let model = stored.model.or_else(|| event.model.clone());
    let merged = UsageEvent::new(
        event.adapter_id,
        event.event_key.clone(),
        occurred_at_ms,
        session_id,
        model,
        merged_tokens,
        event.quality,
    );

    transaction.execute(
        "UPDATE usage_event SET
            occurred_at_ms = ?2,
            session_id = ?3,
            model = ?4,
            input_uncached_tokens = ?5,
            cache_read_tokens = ?6,
            cache_write_tokens = ?7,
            output_tokens = ?8,
            reasoning_tokens = ?9,
            processed_tokens = ?10,
            quality = ?11,
            payload_hash = ?12
         WHERE event_id = ?1",
        params![
            merged.event_id,
            merged.occurred_at_ms,
            merged.session_id,
            merged.model,
            merged.tokens.input_uncached,
            merged.tokens.cache_read,
            merged.tokens.cache_write,
            merged.tokens.output,
            merged.tokens.reasoning_output,
            merged.tokens.processed(),
            merged.quality,
            merged.payload_hash,
        ],
    )?;
    Ok(EventWriteOutcome::Accepted)
}

pub fn upsert_quota(connection: &Connection, quota: &QuotaSample) -> Result<()> {
    connection.execute(
        "INSERT INTO quota_snapshot (
            adapter_id, window_key, remaining_percent, resets_at_ms,
            collected_at_ms, quality, source_label
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ON CONFLICT(adapter_id, window_key) DO UPDATE SET
            remaining_percent = excluded.remaining_percent,
            resets_at_ms = excluded.resets_at_ms,
            collected_at_ms = excluded.collected_at_ms,
            quality = excluded.quality,
            source_label = excluded.source_label
        WHERE excluded.collected_at_ms >= quota_snapshot.collected_at_ms",
        params![
            quota.adapter_id,
            quota.window_key,
            quota.remaining_percent,
            quota.resets_at_ms,
            quota.collected_at_ms,
            quota.quality,
            quota.source_label,
        ],
    )?;
    Ok(())
}

fn upsert_quota_tx(transaction: &Transaction<'_>, quota: &QuotaSample) -> Result<()> {
    transaction.execute(
        "INSERT INTO quota_snapshot (
            adapter_id, window_key, remaining_percent, resets_at_ms,
            collected_at_ms, quality, source_label
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ON CONFLICT(adapter_id, window_key) DO UPDATE SET
            remaining_percent = excluded.remaining_percent,
            resets_at_ms = excluded.resets_at_ms,
            collected_at_ms = excluded.collected_at_ms,
            quality = excluded.quality,
            source_label = excluded.source_label
        WHERE excluded.collected_at_ms >= quota_snapshot.collected_at_ms",
        params![
            quota.adapter_id,
            quota.window_key,
            quota.remaining_percent,
            quota.resets_at_ms,
            quota.collected_at_ms,
            quota.quality,
            quota.source_label,
        ],
    )?;
    Ok(())
}

pub fn prune_missing_sources(connection: &mut Connection) -> Result<()> {
    let mut statement = connection.prepare("SELECT source_id, locator FROM scan_source")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            PathBuf::from(row.get::<_, String>(1)?),
        ))
    })?;
    let missing: Vec<String> = rows
        .filter_map(Result::ok)
        .filter(|(_, locator)| !locator.exists())
        .map(|(source_id, _)| source_id)
        .collect();
    drop(statement);

    if missing.is_empty() {
        return Ok(());
    }

    let transaction = connection.transaction()?;
    for source_id in missing {
        transaction.execute("DELETE FROM scan_source WHERE source_id = ?1", [source_id])?;
    }
    delete_orphan_events(&transaction)?;
    transaction.commit()?;
    Ok(())
}

pub fn prune_old_events(connection: &Connection, cutoff_ms: i64) -> Result<()> {
    connection.execute(
        "DELETE FROM usage_event WHERE occurred_at_ms < ?1",
        [cutoff_ms],
    )?;
    Ok(())
}

fn delete_orphan_events(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute(
        "DELETE FROM usage_event
         WHERE NOT EXISTS (
             SELECT 1 FROM event_observation WHERE event_observation.event_id = usage_event.event_id
         )",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ParsedSource, TokenVector, UsageEvent};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static READ_ONLY_TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDbDir(PathBuf);

    impl TestDbDir {
        fn new(label: &str) -> Self {
            let sequence = READ_ONLY_TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "metrik-storage-{label}-{}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn database_path(&self) -> PathBuf {
            self.0.join("usage.sqlite3")
        }
    }

    impl Drop for TestDbDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn source(source_id: &str, adapter_id: &'static str, events: Vec<UsageEvent>) -> ParsedSource {
        ParsedSource {
            source_id: source_id.into(),
            adapter_id,
            locator: PathBuf::from(format!("{source_id}.jsonl")),
            logical_key: source_id.into(),
            size: 20,
            mtime_ns: 1,
            events,
            quotas: vec![],
        }
    }

    #[test]
    fn ledger_reset_clears_only_managed_derived_rows() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_preference (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO app_preference VALUES ('theme', 'system');
                 INSERT INTO scan_source (
                     source_id, adapter_id, logical_key, locator, observed_size,
                     mtime_ns, coverage_start_ms, parser_version, last_success_ms, last_error
                 ) VALUES ('source', 'codex', 'source', 'agent-log.jsonl', 10, 1, 0, 3, 1, NULL);
                 INSERT INTO usage_event (
                     event_id, adapter_id, event_key, occurred_at_ms, session_id, model,
                     input_uncached_tokens, cache_read_tokens, cache_write_tokens,
                     output_tokens, reasoning_tokens, processed_tokens, quality, payload_hash
                 ) VALUES ('event', 'codex', 'session:event', 1, 'session', NULL,
                           10, 0, 0, 2, 0, 12, 'exact', 'hash');
                 INSERT INTO event_observation VALUES ('event', 'source', 1);
                 INSERT INTO quota_snapshot VALUES (
                     'codex', 'primary', 75, 1000, 1, 'official_live', 'Codex'
                 );",
            )
            .unwrap();

        reset_derived_ledger_connection(&mut connection).unwrap();

        for table in [
            "scan_source",
            "usage_event",
            "event_observation",
            "quota_snapshot",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "{table} should be empty after a reset");
        }
        let preference: String = connection
            .query_row(
                "SELECT value FROM app_preference WHERE key = 'theme'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(preference, "system");
    }

    #[test]
    fn replacing_the_same_source_is_idempotent() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let event = UsageEvent::new(
            "codex",
            "session:1".into(),
            100,
            "session".into(),
            None,
            TokenVector {
                input_uncached: 10,
                output: 5,
                ..Default::default()
            },
            "cumulative_delta",
        );
        let source = source("source", "codex", vec![event]);

        replace_source(&mut connection, &source, 0).unwrap();
        replace_source(&mut connection, &source, 0).unwrap();

        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM usage_event", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn claude_message_updates_merge_component_wise_across_sources() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let event_key = "message:message-a".to_owned();
        let first = UsageEvent::new(
            "claude",
            event_key.clone(),
            100,
            "session-z".into(),
            Some("claude-sonnet".into()),
            TokenVector {
                input_uncached: 100,
                output: 5,
                ..Default::default()
            },
            "exact",
        );
        let completed = UsageEvent::new(
            "claude",
            event_key,
            200,
            "session-a".into(),
            Some("claude-sonnet".into()),
            TokenVector {
                input_uncached: 80,
                output: 9,
                ..Default::default()
            },
            "exact",
        );

        replace_source(
            &mut connection,
            &source("source-a", "claude", vec![first]),
            0,
        )
        .unwrap();
        replace_source(
            &mut connection,
            &source("source-b", "claude", vec![completed]),
            0,
        )
        .unwrap();

        let row: (i64, i64, i64, i64, String) = connection
            .query_row(
                "SELECT input_uncached_tokens, output_tokens, processed_tokens,
                        occurred_at_ms, session_id
                 FROM usage_event",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row, (100, 9, 109, 200, "session-a".into()));
        let observations: i64 = connection
            .query_row("SELECT COUNT(*) FROM event_observation", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(observations, 2);
    }

    #[test]
    fn non_claude_payload_conflicts_remain_errors() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let first = UsageEvent::new(
            "codex",
            "session:fingerprint".into(),
            100,
            "session".into(),
            None,
            TokenVector {
                input_uncached: 10,
                ..Default::default()
            },
            "cumulative_delta",
        );
        let conflicting = UsageEvent::new(
            "codex",
            "session:fingerprint".into(),
            100,
            "session".into(),
            None,
            TokenVector {
                input_uncached: 20,
                ..Default::default()
            },
            "cumulative_delta",
        );

        replace_source(
            &mut connection,
            &source("source-a", "codex", vec![first]),
            0,
        )
        .unwrap();
        let error = replace_source(
            &mut connection,
            &source("source-b", "codex", vec![conflicting]),
            0,
        )
        .unwrap_err();

        assert!(error.to_string().contains("event identity collision"));
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM usage_event", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn claude_model_conflict_rejects_only_that_event_and_commits_the_source() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let event_key = "message:message-a".to_owned();
        let first = UsageEvent::new(
            "claude",
            event_key.clone(),
            100,
            "session-a".into(),
            Some("claude-sonnet".into()),
            TokenVector {
                input_uncached: 10,
                ..Default::default()
            },
            "exact",
        );
        let conflicting = UsageEvent::new(
            "claude",
            event_key,
            100,
            "session-a".into(),
            Some("claude-opus".into()),
            TokenVector {
                input_uncached: 10,
                ..Default::default()
            },
            "exact",
        );
        let valid = UsageEvent::new(
            "claude",
            "message:message-b".into(),
            200,
            "session-b".into(),
            Some("claude-opus".into()),
            TokenVector {
                input_uncached: 7,
                ..Default::default()
            },
            "exact",
        );

        replace_source(
            &mut connection,
            &source("source-a", "claude", vec![first]),
            0,
        )
        .unwrap();
        let outcome = replace_source(
            &mut connection,
            &source("source-b", "claude", vec![conflicting, valid]),
            0,
        )
        .unwrap();

        assert_eq!(outcome.rejected_events, 1);
        let tokens: Vec<i64> = connection
            .prepare("SELECT processed_tokens FROM usage_event ORDER BY event_key")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(tokens, [10, 7]);
        assert!(source_is_current(&connection, "source-b", 20, 1, 0).unwrap());
    }

    #[test]
    fn narrow_source_reconciliation_retains_older_observations() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        let old = UsageEvent::new(
            "codex",
            "session:old".into(),
            100,
            "session".into(),
            None,
            TokenVector {
                input_uncached: 10,
                ..Default::default()
            },
            "cumulative_delta",
        );
        let recent = UsageEvent::new(
            "codex",
            "session:recent".into(),
            2_000,
            "session".into(),
            None,
            TokenVector {
                input_uncached: 20,
                ..Default::default()
            },
            "cumulative_delta",
        );
        replace_source(
            &mut connection,
            &source("source", "codex", vec![old, recent]),
            0,
        )
        .unwrap();

        let refreshed_recent = UsageEvent::new(
            "codex",
            "session:recent-v2".into(),
            2_100,
            "session".into(),
            None,
            TokenVector {
                input_uncached: 30,
                ..Default::default()
            },
            "cumulative_delta",
        );
        replace_source(
            &mut connection,
            &source("source", "codex", vec![refreshed_recent]),
            1_000,
        )
        .unwrap();

        let old_tokens: i64 = connection
            .query_row(
                "SELECT input_uncached_tokens FROM usage_event WHERE event_key = 'session:old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_tokens, 10);
        let event_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM usage_event", [], |row| row.get(0))
            .unwrap();
        assert_eq!(event_count, 2);
        let observation_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM event_observation", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(observation_count, 2);
    }

    #[test]
    fn stale_parser_sources_are_detected_for_retained_history() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        connection
            .execute(
                "INSERT INTO scan_source (
                    source_id, adapter_id, logical_key, locator, observed_size,
                    mtime_ns, coverage_start_ms, parser_version, last_success_ms, last_error
                 ) VALUES ('recent', 'claude', 'recent', 'recent.jsonl', 10,
                           2000000000, 0, ?1, 0, NULL)",
                [PARSER_VERSION - 1],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO scan_source (
                    source_id, adapter_id, logical_key, locator, observed_size,
                    mtime_ns, coverage_start_ms, parser_version, last_success_ms, last_error
                 ) VALUES ('old', 'claude', 'old', 'old.jsonl', 10,
                           500000000, 0, ?1, 0, NULL)",
                [PARSER_VERSION - 1],
            )
            .unwrap();

        assert!(source_needs_parser_rebuild(&connection, "recent").unwrap());
        assert!(adapter_has_stale_parser_sources(&connection, "claude").unwrap());
        assert!(!adapter_has_stale_parser_sources(&connection, "codex").unwrap());

        replace_source(&mut connection, &source("recent", "claude", Vec::new()), 0).unwrap();
        replace_source(&mut connection, &source("old", "claude", Vec::new()), 0).unwrap();
        assert!(!source_needs_parser_rebuild(&connection, "recent").unwrap());
        assert!(!adapter_has_stale_parser_sources(&connection, "claude").unwrap());
    }

    #[test]
    fn read_only_connection_queries_succeed_while_a_writer_holds_a_transaction() {
        let dir = TestDbDir::new("read-only-concurrent");
        let path = dir.database_path();

        // Establish the schema (and WAL mode) through the normal write path first.
        open_database(&path).unwrap();

        let mut writer = Connection::open(&path).unwrap();
        writer
            .pragma_update(None, "busy_timeout", 5_000_i64)
            .unwrap();
        let write_txn = writer
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        write_txn
            .execute(
                "INSERT INTO scan_source (
                    source_id, adapter_id, logical_key, locator, observed_size,
                    mtime_ns, coverage_start_ms, parser_version, last_success_ms, last_error
                 ) VALUES ('source', 'codex', 'source', 'agent-log.jsonl', 10, 1, 0, 3, 1, NULL)",
                [],
            )
            .unwrap();

        // The writer's transaction is still open (uncommitted) when the
        // read-only connection queries the ledger. This is the scenario the
        // report/session pages hit while a background scan is mid-flight.
        let reader = open_database_read_only(&path).unwrap();
        let count: i64 = reader
            .query_row("SELECT COUNT(*) FROM usage_event", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        write_txn.commit().unwrap();
    }

    #[test]
    fn read_only_connection_reports_a_clear_error_for_a_missing_database() {
        let dir = TestDbDir::new("read-only-missing");
        let path = dir.database_path();

        let error = open_database_read_only(&path).unwrap_err();

        assert!(error
            .to_string()
            .contains("failed to open usage database read-only"));
    }
}
