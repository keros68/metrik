mod antigravity;
mod claude;
mod codex;
mod grok;
mod hermes;
mod kimi;
mod opencode;
mod pi;
mod workbuddy;
mod zcode;

pub use antigravity::AntigravityAdapter;
pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use grok::GrokAdapter;
pub use hermes::HermesAdapter;
pub use kimi::KimiAdapter;
pub use opencode::OpencodeAdapter;
pub use pi::PiAdapter;
pub use workbuddy::WorkbuddyAdapter;
pub use zcode::ZcodeAdapter;

// 配额快照从日志读取，供 quota 注册表调用；根目录由调用点解析（测试可注入）。
pub use grok::{fetch_grok_quota_snapshot, grok_home, grok_home_exists};

use crate::domain::{stable_hash, ParsedSource};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

#[derive(Clone, Debug)]
pub struct SourceCandidate {
    pub source_id: String,
    pub path: PathBuf,
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
    /// 已知存在、但当前版本读不了的存储形态（例：OpenCode 1.2+ 改用 SQLite）。
    /// 非空时该 Agent 的覆盖必须标为"部分"并把原因展示给用户——此时显示的 0
    /// 是"读不到"，不是"没用过"，静默显示 0 违反诚实约束。默认没有。
    fn coverage_gaps(&self) -> Vec<String> {
        Vec::new()
    }
}

pub fn discover_jsonl(roots: &[PathBuf], adapter_id: &str, cutoff_ms: i64) -> Vec<SourceCandidate> {
    discover_files(roots, adapter_id, cutoff_ms, None)
}

/// 与 `discover_jsonl` 相同，但可限定固定文件名：Grok 的 updates.jsonl 与
/// 同目录的 chat_history / events 共享 .jsonl 扩展名，全收会拖慢扫描队列。
fn discover_files(
    roots: &[PathBuf],
    adapter_id: &str,
    cutoff_ms: i64,
    filename: Option<&str>,
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
            let name_matches = match filename {
                Some(wanted) => path.file_name().and_then(|value| value.to_str()) == Some(wanted),
                None => path.extension().and_then(|value| value.to_str()) == Some("jsonl"),
            };
            if !name_matches {
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

fn normalize_locator(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
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
}
