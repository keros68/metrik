mod claude;
mod codex;
mod opencode;
mod zcode;

pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use opencode::OpencodeAdapter;
pub use zcode::ZcodeAdapter;

use crate::domain::{stable_hash, ParsedSource};
use anyhow::Result;
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanDiagnostics {
    pub malformed_lines: usize,
    pub unreadable_lines: usize,
    pub rejected_events: usize,
}

impl ScanDiagnostics {
    pub fn is_partial(&self) -> bool {
        self.malformed_lines > 0 || self.unreadable_lines > 0 || self.rejected_events > 0
    }

    pub fn storage_marker(&self) -> Option<String> {
        self.is_partial().then(|| {
            format!(
                "{SCAN_DIAGNOSTICS_PREFIX_V2}:{}:{}:{}",
                self.malformed_lines, self.unreadable_lines, self.rejected_events
            )
        })
    }

    pub fn from_storage_marker(marker: &str) -> Option<Self> {
        let mut parts = marker.split(':');
        let version = parts.next()?;
        let malformed_lines = parts.next()?.parse().ok()?;
        let unreadable_lines = parts.next()?.parse().ok()?;
        let rejected_events = match version {
            SCAN_DIAGNOSTICS_PREFIX_V1 => 0,
            SCAN_DIAGNOSTICS_PREFIX_V2 => parts.next()?.parse().ok()?,
            _ => return None,
        };
        parts.next().is_none().then_some(())?;
        Some(Self {
            malformed_lines,
            unreadable_lines,
            rejected_events,
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
}

pub fn discover_jsonl(roots: &[PathBuf], adapter_id: &str, cutoff_ms: i64) -> Vec<SourceCandidate> {
    let mut found = Vec::new();
    for root in roots.iter().filter(|root| root.exists()) {
        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.into_path();
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
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
        };
        let marker = diagnostics.storage_marker().unwrap();

        assert_eq!(
            ScanDiagnostics::from_storage_marker(&marker),
            Some(diagnostics)
        );
        assert_eq!(ScanDiagnostics::default().storage_marker(), None);
        assert_eq!(ScanDiagnostics::from_storage_marker("unrelated"), None);
    }

    #[test]
    fn legacy_scan_diagnostics_marker_remains_readable() {
        assert_eq!(
            ScanDiagnostics::from_storage_marker("jsonl-scan-v1:2:1"),
            Some(ScanDiagnostics {
                malformed_lines: 2,
                unreadable_lines: 1,
                rejected_events: 0,
            })
        );
    }
}
