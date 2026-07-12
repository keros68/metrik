PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS scan_source (
    source_id        TEXT PRIMARY KEY,
    adapter_id       TEXT NOT NULL,
    logical_key      TEXT NOT NULL,
    locator          TEXT NOT NULL,
    observed_size    INTEGER NOT NULL,
    mtime_ns         INTEGER NOT NULL,
    coverage_start_ms INTEGER NOT NULL,
    parser_version   INTEGER NOT NULL,
    last_success_ms  INTEGER NOT NULL,
    last_error       TEXT
);

CREATE TABLE IF NOT EXISTS usage_event (
    event_id                 TEXT PRIMARY KEY,
    adapter_id               TEXT NOT NULL,
    event_key                TEXT NOT NULL,
    occurred_at_ms           INTEGER NOT NULL,
    session_id               TEXT NOT NULL,
    model                    TEXT,
    input_uncached_tokens    INTEGER NOT NULL CHECK (input_uncached_tokens >= 0),
    cache_read_tokens        INTEGER NOT NULL CHECK (cache_read_tokens >= 0),
    cache_write_tokens       INTEGER NOT NULL CHECK (cache_write_tokens >= 0),
    output_tokens            INTEGER NOT NULL CHECK (output_tokens >= 0),
    reasoning_tokens         INTEGER NOT NULL CHECK (reasoning_tokens >= 0),
    processed_tokens         INTEGER NOT NULL CHECK (processed_tokens >= 0),
    quality                  TEXT NOT NULL,
    payload_hash             TEXT NOT NULL,
    UNIQUE(adapter_id, event_key)
);

CREATE TABLE IF NOT EXISTS event_observation (
    event_id       TEXT NOT NULL REFERENCES usage_event(event_id) ON DELETE CASCADE,
    source_id      TEXT NOT NULL REFERENCES scan_source(source_id) ON DELETE CASCADE,
    observed_at_ms INTEGER NOT NULL,
    PRIMARY KEY (event_id, source_id)
);

CREATE TABLE IF NOT EXISTS quota_snapshot (
    adapter_id       TEXT NOT NULL,
    window_key       TEXT NOT NULL,
    remaining_percent REAL NOT NULL CHECK (remaining_percent BETWEEN 0 AND 100),
    resets_at_ms     INTEGER,
    collected_at_ms  INTEGER NOT NULL,
    quality          TEXT NOT NULL,
    source_label     TEXT NOT NULL,
    PRIMARY KEY (adapter_id, window_key)
);

CREATE INDEX IF NOT EXISTS idx_usage_event_time
    ON usage_event(occurred_at_ms);

CREATE INDEX IF NOT EXISTS idx_usage_event_adapter_time
    ON usage_event(adapter_id, occurred_at_ms);

-- 同步与设置表在既有库上按需补建；它们不参与 schema 兼容性判定，
-- 缺失时不会触发派生账本重建。
CREATE TABLE IF NOT EXISTS app_setting (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS remote_usage_event (
    device_id        TEXT NOT NULL,
    event_id         TEXT NOT NULL,
    adapter_id       TEXT NOT NULL,
    occurred_at_ms   INTEGER NOT NULL,
    processed_tokens INTEGER NOT NULL CHECK (processed_tokens >= 0),
    PRIMARY KEY (device_id, event_id)
);

CREATE INDEX IF NOT EXISTS idx_remote_usage_event_time
    ON remote_usage_event(occurred_at_ms);

CREATE TABLE IF NOT EXISTS sync_device (
    device_id      TEXT PRIMARY KEY,
    label          TEXT NOT NULL,
    exported_at_ms INTEGER NOT NULL,
    last_import_ms INTEGER NOT NULL
);
