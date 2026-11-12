PRAGMA auto_vacuum = INCREMENTAL;
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS scan_source (
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

CREATE TABLE IF NOT EXISTS usage_event (
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

-- 绝大多数事件只被一个本地来源文件观察到，直接记录在 usage_event.source_seq；
-- 仅当同一逻辑事件跨多个来源出现（如会话分支或归档副本）时，才在此溢出表登记附加来源。
CREATE TABLE IF NOT EXISTS event_extra_source (
    event_id   INTEGER NOT NULL REFERENCES usage_event(id) ON DELETE CASCADE,
    source_seq INTEGER NOT NULL REFERENCES scan_source(source_seq) ON DELETE CASCADE,
    PRIMARY KEY (event_id, source_seq)
) WITHOUT ROWID;

-- remaining_percent 对百分比窗口是 0–100，但对 DeepSeek 等余额窗口是账户金额，
-- 按契约原样存放、可以超过 100（负数仍在解析层拦截），故只保下界。
CREATE TABLE IF NOT EXISTS quota_snapshot (
    adapter_id       TEXT NOT NULL,
    window_key       TEXT NOT NULL,
    remaining_percent REAL NOT NULL CHECK (remaining_percent >= 0),
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

CREATE INDEX IF NOT EXISTS idx_usage_event_source_seq
    ON usage_event(source_seq);

CREATE INDEX IF NOT EXISTS idx_event_extra_source_seq
    ON event_extra_source(source_seq);

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

-- 主键以 device_id 开头，按 event_id 查不到索引；跨设备去重要逐条查同一事件
-- 是否还出现在别的设备上。
CREATE INDEX IF NOT EXISTS idx_remote_usage_event_identity
    ON remote_usage_event(event_id, device_id);

CREATE TABLE IF NOT EXISTS sync_device (
    device_id      TEXT PRIMARY KEY,
    label          TEXT NOT NULL,
    exported_at_ms INTEGER NOT NULL,
    last_import_ms INTEGER NOT NULL
);

-- Local opt-in request timing is independent of the token ledger: telemetry
-- enriches speed statistics and must never create a second usage event.
CREATE TABLE IF NOT EXISTS response_speed_sample (
    sample_id       TEXT PRIMARY KEY,
    adapter_id      TEXT NOT NULL CHECK (adapter_id IN ('claude', 'codex')),
    model           TEXT NOT NULL,
    occurred_at_ms  INTEGER NOT NULL,
    output_tokens   INTEGER NOT NULL CHECK (output_tokens > 0),
    duration_ms     REAL NOT NULL CHECK (duration_ms > 0)
);

CREATE INDEX IF NOT EXISTS idx_response_speed_sample_time
    ON response_speed_sample(occurred_at_ms);
