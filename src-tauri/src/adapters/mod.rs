mod antigravity;
mod claude;
mod codex;
mod cursor;
mod dsh;
mod grok;
mod grokbot;
mod hermes;
mod kimi;
mod minimax;
mod opencode;
mod pi;
mod workbuddy;
mod zcode;

pub use antigravity::AntigravityAdapter;
pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use cursor::{cursor_state_db, CursorAdapter};
pub use dsh::{dsh_home, DshAdapter};
pub use grok::GrokAdapter;
pub use hermes::HermesAdapter;
pub use kimi::KimiAdapter;
pub use minimax::{minimax_data_dir, MinimaxAdapter};
pub use opencode::OpencodeAdapter;
pub use pi::PiAdapter;
pub use workbuddy::WorkbuddyAdapter;
pub use zcode::ZcodeAdapter;

// 配额快照从日志或 RPC 读取，供 quota 注册表调用。
pub use antigravity::fetch_antigravity_quota_snapshot;
pub use grok::{fetch_grok_quota_snapshot, grok_home, grok_home_exists};

// Cursor 用量与套餐余量默认关闭，设置开关与状态查询。
pub use cursor::{
    fetch_plan_quota as fetch_cursor_plan_quota, reset_runtime_state as reset_cursor_runtime_state,
    usage_status as cursor_usage_status, CursorUsageStatus,
    USAGE_SETTING_KEY as CURSOR_USAGE_SETTING_KEY,
};

// Grok Bot 共享额度池默认关闭，设置开关与状态查询。
pub use grokbot::{
    fetch_quota as fetch_grokbot_quota, grokbot_data_dir, quota_status as grokbot_quota_status,
    signed_in as grokbot_signed_in, GrokBotQuotaStatus,
    QUOTA_SETTING_KEY as GROKBOT_QUOTA_SETTING_KEY,
};

use crate::domain::{stable_hash, ParsedSource};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Lines};
use std::iter::Enumerate;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

#[derive(Clone, Debug)]
pub struct SourceCandidate {
    pub source_id: String,
    pub path: PathBuf,
    /// 文件来源是字节数；SQLite 来源是 DB/WAL 各自元数据的稳定变更指纹。
    pub size: u64,
    pub mtime_ns: i64,
}

const SCAN_DIAGNOSTICS_PREFIX_V1: &str = "jsonl-scan-v1";
const SCAN_DIAGNOSTICS_PREFIX_V2: &str = "jsonl-scan-v2";
const SCAN_DIAGNOSTICS_PREFIX_V3: &str = "jsonl-scan-v3";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanDiagnostics {
    pub malformed_lines: usize,
    pub unreadable_lines: usize,
    pub rejected_events: usize,
    /// 口径自检失败的读数条数：我们算出的分量和来源自己报的总量对不上。
    /// 见 `TokenVector::disagrees_with_reported_total`。
    pub total_mismatches: usize,
}

impl ScanDiagnostics {
    pub fn is_partial(&self) -> bool {
        self.malformed_lines > 0
            || self.unreadable_lines > 0
            || self.rejected_events > 0
            || self.total_mismatches > 0
    }

    pub fn storage_marker(&self) -> Option<String> {
        self.is_partial().then(|| {
            format!(
                "{SCAN_DIAGNOSTICS_PREFIX_V3}:{}:{}:{}:{}",
                self.malformed_lines,
                self.unreadable_lines,
                self.rejected_events,
                self.total_mismatches
            )
        })
    }

    pub fn from_storage_marker(marker: &str) -> Option<Self> {
        let mut parts = marker.split(':');
        let version = parts.next()?;
        let malformed_lines = parts.next()?.parse().ok()?;
        let unreadable_lines = parts.next()?.parse().ok()?;
        // 旧标记继续认：升级后不该因为标记格式变了就把既有来源判成"读过但未知"。
        let (rejected_events, total_mismatches) = match version {
            SCAN_DIAGNOSTICS_PREFIX_V1 => (0, 0),
            SCAN_DIAGNOSTICS_PREFIX_V2 => (parts.next()?.parse().ok()?, 0),
            SCAN_DIAGNOSTICS_PREFIX_V3 => {
                (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?)
            }
            _ => return None,
        };
        parts.next().is_none().then_some(())?;
        Some(Self {
            malformed_lines,
            unreadable_lines,
            rejected_events,
            total_mismatches,
        })
    }
}

#[derive(Debug)]
pub struct ParsedScan {
    pub source: ParsedSource,
    pub diagnostics: ScanDiagnostics,
}

pub trait AgentAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate>;
    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan>;
    fn has_pending(&self, _candidate: &SourceCandidate) -> bool {
        false
    }
    /// Return None when the parsing budget expires; the adapter retains progress.
    fn parse_until(
        &self,
        candidate: &SourceCandidate,
        cutoff_ms: i64,
        _deadline: std::time::Instant,
    ) -> Result<Option<ParsedScan>> {
        self.parse(candidate, cutoff_ms).map(Some)
    }
    /// 已知存在、但当前版本读不了的存储形态（例：OpenCode 1.2+ 改用 SQLite）。
    /// 非空时该 Agent 的覆盖必须标为"部分"并把原因展示给用户——此时显示的 0
    /// 是"读不到"，不是"没用过"，静默显示 0 违反诚实约束。默认没有。
    fn coverage_gaps(&self) -> Vec<String> {
        Vec::new()
    }
}

pub fn discover_jsonl(roots: &[PathBuf], adapter_id: &str, cutoff_ms: i64) -> Vec<SourceCandidate> {
    discover_files(roots, adapter_id, cutoff_ms, |path| {
        has_extension(path, "jsonl")
    })
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some(extension)
}

/// 与 `discover_jsonl` 相同，但由调用方决定收哪些文件：Grok 的 updates.jsonl 与
/// 同目录的 chat_history / events 共享 .jsonl 扩展名，全收会拖慢扫描队列。
fn discover_files(
    roots: &[PathBuf],
    adapter_id: &str,
    cutoff_ms: i64,
    wanted: impl Fn(&Path) -> bool,
) -> Vec<SourceCandidate> {
    let mut found = Vec::new();
    for root in roots.iter().filter(|root| root.exists()) {
        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.into_path();
            if !wanted(&path) {
                continue;
            }
            let Ok(metadata) = path.metadata() else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            let Ok(since_epoch) = modified.duration_since(UNIX_EPOCH) else {
                continue;
            };
            let mtime_ns = since_epoch.as_nanos().min(i64::MAX as u128) as i64;
            if mtime_ns / 1_000_000 < cutoff_ms {
                continue;
            }
            let normalized = normalize_locator(&path);
            found.push(SourceCandidate {
                source_id: stable_hash(&format!("{adapter_id}|{normalized}")),
                path,
                size: metadata.len(),
                mtime_ns,
            });
        }
    }
    found.sort_by(|left, right| left.path.cmp(&right.path));
    found
}

/// OpenCode 系（OpenCode 与其分支 zcode）的 `session` 表把会话工作目录记在
/// `directory` 列，用量表只带 `session_id`，靠这张映射补项目归属。
/// 表或列不存在（旧版本、纯文件存储）时返回空映射——项目归属缺失是可接受的
/// 降级，不能让整个源的用量因此解析失败。
pub fn opencode_session_directories(connection: &Connection) -> HashMap<String, String> {
    let Ok(mut statement) = connection.prepare("SELECT id, directory FROM session") else {
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

/// 逐行解析 JSONL 记录，产出 `(行号, 记录)`。空行跳过；读不出的行和解析失败
/// 的行（活跃文件末尾可能是半行，下次扫描会重新读取）只计数，遍历完由
/// `record_skipped` 计入诊断。
pub(super) struct JsonlRecords<T> {
    lines: Enumerate<Lines<BufReader<File>>>,
    unreadable_lines: usize,
    malformed_lines: usize,
    record: PhantomData<T>,
}

impl<T> JsonlRecords<T> {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        Ok(Self {
            lines: BufReader::with_capacity(256 * 1024, file)
                .lines()
                .enumerate(),
            unreadable_lines: 0,
            malformed_lines: 0,
            record: PhantomData,
        })
    }

    /// 文件在 cutoff 之前就没再变过时（`track_skipped_lines` 为假），跳过的行不计。
    pub(super) fn record_skipped(
        &self,
        diagnostics: &mut ScanDiagnostics,
        track_skipped_lines: bool,
    ) {
        if track_skipped_lines {
            diagnostics.unreadable_lines += self.unreadable_lines;
            diagnostics.malformed_lines += self.malformed_lines;
        }
    }
}

impl<T: DeserializeOwned> Iterator for JsonlRecords<T> {
    type Item = (usize, T);

    fn next(&mut self) -> Option<Self::Item> {
        for (line_index, line) in self.lines.by_ref() {
            let Ok(line) = line else {
                self.unreadable_lines += 1;
                continue;
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(&line) {
                Ok(record) => return Some((line_index, record)),
                Err(_) => self.malformed_lines += 1,
            }
        }
        None
    }
}

pub(super) fn normalize_locator(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

/// 文件 mtime（纳秒）；拿不到记 0，让候选仍然成立（宁可多扫一次）。
pub(super) fn file_mtime_ns(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

pub(super) fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

/// 单个 SQLite 库作为一个源。WAL 模式下写入先进 -wal，主库文件的 mtime/size
/// 可能长期不变：分别记录库与 -wal 的大小、mtime，再哈希成变更指纹。
/// 不能只相加大小、取最大 mtime：主库时间戳领先时会掩盖同大小的 WAL 重写。
///
/// 只看真正承载数据的文件。`-shm` 只是共享内存索引，连只读连接都会改写它
/// （读锁标记），只读打开时 SQLite 还会建出空的 `-wal`——把它们算进来，
/// Metrik 自己每读一次库，下一轮快照就以为库变了，没变的库每轮都被重扫。
/// 空的 `-wal` 不含任何帧；新数据要么写进非空的 `-wal`，要么经检查点写回
/// 主库（主库 mtime 随之变化），两种都仍会被看到。
pub(super) fn sqlite_candidate(
    database: &Path,
    adapter_id: &str,
    cutoff_ms: i64,
) -> Vec<SourceCandidate> {
    let Ok(metadata) = database.metadata() else {
        return Vec::new();
    };
    let database_size = metadata.len();
    let database_mtime_ns = file_mtime_ns(&metadata);
    let (mut wal_size, mut wal_mtime_ns) = (0, 0);
    let mut wal = database.as_os_str().to_os_string();
    wal.push("-wal");
    if let Ok(wal_meta) = std::fs::metadata(PathBuf::from(wal)) {
        if wal_meta.len() > 0 {
            wal_size = wal_meta.len();
            wal_mtime_ns = file_mtime_ns(&wal_meta);
        }
    }
    let mtime_ns = database_mtime_ns.max(wal_mtime_ns);
    if mtime_ns / 1_000_000 < cutoff_ms {
        return Vec::new();
    }
    // size 在 SQLite 来源中是现有 observed_size 缓存槽里的不透明指纹；mtime
    // 仍是实际文件时间，供发现 cutoff 与队列排序使用。空/不存在的 WAL 均为 0|0。
    let fingerprint = stable_hash(&format!(
        "sqlite-metadata-v1|{database_size}|{database_mtime_ns}|{wal_size}|{wal_mtime_ns}"
    ));
    let size = u64::from_str_radix(&fingerprint[..16], 16)
        .expect("stable_hash returns lowercase hexadecimal SHA-256");
    let normalized = normalize_locator(database);
    vec![SourceCandidate {
        source_id: stable_hash(&format!("{adapter_id}|{normalized}")),
        path: database.to_path_buf(),
        size,
        mtime_ns,
    }]
}

pub fn timestamp_str_ms(value: Option<&str>) -> Option<i64> {
    let value = value?;
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_diagnostics_marker_round_trips_without_source_content() {
        let diagnostics = ScanDiagnostics {
            malformed_lines: 2,
            unreadable_lines: 1,
            rejected_events: 3,
            total_mismatches: 4,
        };
        let marker = diagnostics.storage_marker().unwrap();

        assert_eq!(
            ScanDiagnostics::from_storage_marker(&marker),
            Some(diagnostics)
        );
        assert_eq!(ScanDiagnostics::default().storage_marker(), None);
        assert_eq!(ScanDiagnostics::from_storage_marker("unrelated"), None);
    }

    /// 升级不能让既有来源的诊断读不出来：旧标记一律仍要认，缺的字段补 0。
    #[test]
    fn legacy_scan_diagnostics_markers_remain_readable() {
        assert_eq!(
            ScanDiagnostics::from_storage_marker("jsonl-scan-v1:2:1"),
            Some(ScanDiagnostics {
                malformed_lines: 2,
                unreadable_lines: 1,
                rejected_events: 0,
                total_mismatches: 0,
            })
        );
        assert_eq!(
            ScanDiagnostics::from_storage_marker("jsonl-scan-v2:2:1:3"),
            Some(ScanDiagnostics {
                malformed_lines: 2,
                unreadable_lines: 1,
                rejected_events: 3,
                total_mismatches: 0,
            })
        );
        // 位数对不上的标记宁可不认，也不猜。
        assert_eq!(
            ScanDiagnostics::from_storage_marker("jsonl-scan-v2:2:1"),
            None
        );
        assert_eq!(
            ScanDiagnostics::from_storage_marker("jsonl-scan-v3:2:1:3"),
            None
        );
    }

    /// 口径自检独立于"行读坏了"：只有它非零也必须把来源标成数据不完整。
    #[test]
    fn a_total_mismatch_alone_marks_the_source_partial() {
        let diagnostics = ScanDiagnostics {
            total_mismatches: 1,
            ..Default::default()
        };
        assert!(diagnostics.is_partial());
        assert_eq!(
            ScanDiagnostics::from_storage_marker(&diagnostics.storage_marker().unwrap()),
            Some(diagnostics)
        );
    }

    /// 只读打开 WAL 库会改写 `-shm`（读锁标记），首次打开还会建出空的 `-wal`。
    /// 这些都不是数据变化：算进指纹的话，Metrik 自己读一次，下次快照就会把
    /// 没变的库再解析一遍，每轮刷新都重扫全部 SQLite 来源。
    #[test]
    fn reading_a_wal_database_does_not_change_its_fingerprint() {
        let dir = std::env::temp_dir().join(format!(
            "metrik-sqlite-fingerprint-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("source.db");
        let writer = Connection::open(&database).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE t (v INTEGER);
                 INSERT INTO t VALUES (1);",
            )
            .unwrap();
        drop(writer);

        let fingerprint = || {
            let candidate = &sqlite_candidate(&database, "test", 0)[0];
            (candidate.size, candidate.mtime_ns)
        };
        let read = || {
            let reader =
                Connection::open_with_flags(&database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .unwrap();
            reader
                .query_row("SELECT COUNT(*) FROM t", [], |row| row.get::<_, i64>(0))
                .unwrap();
        };
        let pause = || std::thread::sleep(std::time::Duration::from_millis(50));

        let before = fingerprint();
        for _ in 0..2 {
            pause();
            read();
            assert_eq!(fingerprint(), before, "只读不能让来源看起来变了");
        }

        // 真正的写入仍然要看得见：写入期间在 -wal 里，关闭后检查点写回主库。
        pause();
        let writer = Connection::open(&database).unwrap();
        writer.execute("INSERT INTO t VALUES (2)", []).unwrap();
        assert_ne!(fingerprint(), before, "-wal 里的新数据必须改变指纹");
        drop(writer);
        assert_ne!(fingerprint(), before, "检查点写回主库后仍然是变化");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sqlite_fingerprint_ignores_shm_and_empty_wal_sidecar_changes() {
        let dir = std::env::temp_dir().join(format!(
            "metrik-sqlite-sidecar-fingerprint-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("source.db");
        let shm = dir.join("source.db-shm");
        let wal = dir.join("source.db-wal");
        // 文件元数据层的测试不依赖 SQLite 是否在当前平台更新读锁的 mtime。
        std::fs::write(&database, b"database").unwrap();
        let fingerprint = || {
            let candidate = &sqlite_candidate(&database, "test", 0)[0];
            (candidate.size, candidate.mtime_ns)
        };
        let before = fingerprint();
        std::fs::write(&shm, b"read lock").unwrap();
        assert_eq!(fingerprint(), before);
        std::fs::write(&shm, b"changed read lock index").unwrap();
        assert_eq!(fingerprint(), before);
        std::fs::write(&wal, b"").unwrap();
        assert_eq!(fingerprint(), before);
        std::fs::remove_file(&wal).unwrap();
        assert_eq!(fingerprint(), before);

        std::fs::write(&wal, b"data frames").unwrap();
        let with_frames = fingerprint();
        assert_ne!(with_frames, before);
        std::fs::write(&database, b"checkpointed database").unwrap();
        assert_ne!(fingerprint(), with_frames);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_size_wal_rewrite_invalidates_cache_when_database_mtime_is_ahead() {
        let dir = std::env::temp_dir().join(format!(
            "metrik-sqlite-wal-reuse-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("source.db");
        let wal = dir.join("source.db-wal");
        let writer = Connection::open(&database).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 CREATE TABLE t (v INTEGER);
                 INSERT INTO t VALUES (1);
                 PRAGMA wal_checkpoint(TRUNCATE);
                 UPDATE t SET v = 2;
                 PRAGMA wal_checkpoint(RESTART);",
            )
            .unwrap();
        // 时钟回拨/保留时间戳的恢复都可能让主库时间领先后续 WAL 写入。
        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&database)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(future))
            .unwrap();
        let candidate = || sqlite_candidate(&database, "test", 0).remove(0);
        let source = |candidate: &SourceCandidate| ParsedSource {
            source_id: candidate.source_id.clone(),
            adapter_id: "test",
            locator: candidate.path.clone(),
            logical_key: "source".into(),
            size: candidate.size,
            mtime_ns: candidate.mtime_ns,
            events: Vec::new(),
            quotas: Vec::new(),
        };
        let current = |ledger: &Connection, candidate: &SourceCandidate| {
            crate::storage::source_is_current(
                ledger,
                &candidate.source_id,
                candidate.size,
                candidate.mtime_ns,
                0,
            )
            .unwrap()
        };
        let read = || {
            let reader =
                Connection::open_with_flags(&database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .unwrap();
            reader
                .query_row("SELECT v FROM t", [], |row| row.get::<_, i64>(0))
                .unwrap()
        };

        let mut ledger = crate::storage::open_database(&dir.join("ledger.db")).unwrap();
        let before = candidate();
        let before_wal = std::fs::metadata(&wal).unwrap();
        assert!(before.mtime_ns > file_mtime_ns(&before_wal));
        crate::storage::replace_source(&mut ledger, &source(&before), 0).unwrap();
        assert!(current(&ledger, &before));
        assert_eq!(read(), 2);
        assert!(current(&ledger, &candidate()), "只读不能使缓存过期");

        std::thread::sleep(std::time::Duration::from_millis(50));
        writer.execute("UPDATE t SET v = 3", []).unwrap();
        assert_eq!(read(), 3, "WAL 中的实际数据必须已经改变");
        let after = candidate();
        let after_wal = std::fs::metadata(&wal).unwrap();
        assert_eq!(
            before_wal.len(),
            after_wal.len(),
            "RESTART 后复用同大小 WAL"
        );
        assert_ne!(file_mtime_ns(&before_wal), file_mtime_ns(&after_wal));
        assert_eq!(after.source_id, before.source_id);
        assert_eq!(after.mtime_ns, before.mtime_ns, "最大 mtime 仍由主库决定");
        assert_ne!(after.size, before.size, "独立 WAL 元数据必须改变指纹");
        assert!(!current(&ledger, &after));

        crate::storage::replace_source(&mut ledger, &source(&after), 0).unwrap();
        assert_eq!(read(), 3);
        assert!(current(&ledger, &candidate()), "重扫后只读保持缓存有效");
        drop(ledger);
        drop(writer);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
