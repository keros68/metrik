use crate::speed_telemetry::SpeedSample;
use anyhow::Result;
use rusqlite::{params, Connection};

pub const RETENTION_MS: i64 = 182 * 24 * 60 * 60 * 1000;

pub struct ModelTiming {
    pub agent: String,
    pub model: String,
    pub output_tokens: f64,
    pub duration_ms: f64,
    pub sample_count: i64,
}

/// A retried OTLP batch observes the same request. Never add its tokens to the
/// usage ledger, or overwrite an established sample with conflicting metadata.
pub fn store_samples(
    connection: &mut Connection,
    samples: &[SpeedSample],
    now_ms: i64,
) -> Result<()> {
    let transaction = connection.transaction()?;
    for sample in samples {
        if !matches!(sample.agent.as_str(), "claude" | "codex")
            || sample.id.len() != 64
            || !sample.id.bytes().all(|value| value.is_ascii_hexdigit())
            || sample.model.trim().is_empty()
            || sample.model.len() > 200
            || sample.model.chars().any(char::is_control)
            || sample.output_tokens <= 0
            || !sample.duration_ms.is_finite()
            || sample.duration_ms <= 0.0
            || sample.duration_ms > RETENTION_MS as f64
            || !(sample.output_tokens as f64 * 1000.0 / sample.duration_ms).is_finite()
            || sample.occurred_at_ms < now_ms.saturating_sub(RETENTION_MS)
            || sample.occurred_at_ms > now_ms.saturating_add(5 * 60 * 1000)
        {
            continue;
        }
        transaction.execute(
            "INSERT OR IGNORE INTO response_speed_sample
             (sample_id, adapter_id, model, occurred_at_ms, output_tokens, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                sample.id,
                sample.agent,
                sample.model,
                sample.occurred_at_ms,
                sample.output_tokens,
                sample.duration_ms
            ],
        )?;
    }
    transaction.execute(
        "DELETE FROM response_speed_sample WHERE occurred_at_ms < ?1",
        [now_ms.saturating_sub(RETENTION_MS)],
    )?;
    transaction.commit()?;
    Ok(())
}

pub fn model_timings(
    connection: &Connection,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<ModelTiming>> {
    let mut statement = connection.prepare(
        "SELECT adapter_id, model, SUM(CAST(output_tokens AS REAL)), SUM(duration_ms), COUNT(*)
         FROM response_speed_sample
         WHERE occurred_at_ms >= ?1 AND occurred_at_ms < ?2
         GROUP BY adapter_id, model",
    )?;
    let rows = statement.query_map(params![start_ms, end_ms], |row| {
        Ok(ModelTiming {
            agent: row.get(0)?,
            model: row.get(1)?,
            output_tokens: row.get(2)?,
            duration_ms: row.get(3)?,
            sample_count: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn source_counts(
    connection: &Connection,
    agent: &str,
    now_ms: i64,
) -> Result<(i64, Option<i64>)> {
    Ok(connection.query_row(
        "SELECT COUNT(*), MAX(occurred_at_ms) FROM response_speed_sample
         WHERE adapter_id = ?1 AND occurred_at_ms >= ?2 AND occurred_at_ms <= ?3",
        params![agent, now_ms.saturating_sub(RETENTION_MS), now_ms],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::stable_hash;

    #[test]
    fn invalid_numeric_samples_never_create_nonfinite_model_rates() {
        let mut connection = Connection::open_in_memory().unwrap();
        crate::schema::ensure_schema(&connection).unwrap();
        let now = 1_800_000_000_000;
        for (index, duration_ms) in [
            f64::NAN,
            f64::INFINITY,
            0.0,
            -1.0,
            1.0e-310,
            RETENTION_MS as f64 + 1.0,
        ]
        .into_iter()
        .enumerate()
        {
            store_samples(
                &mut connection,
                &[SpeedSample {
                    id: stable_hash(&index.to_string()),
                    agent: "claude".into(),
                    model: "model".into(),
                    occurred_at_ms: now,
                    output_tokens: 10,
                    duration_ms,
                }],
                now,
            )
            .unwrap();
        }
        assert!(model_timings(&connection, now, now + 1).unwrap().is_empty());
    }

    #[test]
    fn request_retries_and_conflicts_do_not_duplicate_samples_or_token_usage() {
        let mut connection = Connection::open_in_memory().unwrap();
        crate::schema::ensure_schema(&connection).unwrap();
        let now = 1_800_000_000_000;
        let sample = SpeedSample {
            id: stable_hash("claude|request"),
            agent: "claude".into(),
            model: "model".into(),
            occurred_at_ms: now,
            output_tokens: 20,
            duration_ms: 2000.0,
        };
        store_samples(&mut connection, &[sample], now).unwrap();
        let changed = SpeedSample {
            id: stable_hash("claude|request"),
            agent: "claude".into(),
            model: "other".into(),
            occurred_at_ms: now,
            output_tokens: 999,
            duration_ms: 1.0,
        };
        store_samples(&mut connection, &[changed], now).unwrap();
        let timings = model_timings(&connection, now, now + 1).unwrap();
        assert_eq!(timings.len(), 1);
        assert_eq!(timings[0].output_tokens, 20.0);
        assert_eq!(timings[0].duration_ms, 2000.0);
        assert_eq!(timings[0].sample_count, 1);
        let usage: i64 = connection
            .query_row("SELECT COUNT(*) FROM usage_event", [], |row| row.get(0))
            .unwrap();
        assert_eq!(usage, 0);
    }

    #[test]
    fn timing_retention_and_query_windows_are_independent_of_the_usage_ledger() {
        let mut connection = Connection::open_in_memory().unwrap();
        crate::schema::ensure_schema(&connection).unwrap();
        let now = 1_800_000_000_000;
        for (id, agent, at) in [
            ("old", "claude", now - RETENTION_MS - 1),
            ("recent", "claude", now - 10),
            ("new", "codex", now),
            ("future", "codex", now + 600_000),
        ] {
            store_samples(
                &mut connection,
                &[SpeedSample {
                    id: stable_hash(id),
                    agent: agent.into(),
                    model: "model".into(),
                    occurred_at_ms: at,
                    output_tokens: 10,
                    duration_ms: 1000.0,
                }],
                now,
            )
            .unwrap();
        }
        assert_eq!(
            source_counts(&connection, "claude", now).unwrap(),
            (1, Some(now - 10))
        );
        assert_eq!(model_timings(&connection, now, now + 1).unwrap().len(), 1);
        // Normal scans prune retained timings even with collection switched off.
        crate::storage::prune_old_events(&connection, now + 1).unwrap();
        assert!(model_timings(&connection, 0, i64::MAX).unwrap().is_empty());
    }
}
