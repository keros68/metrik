//! aicross 一次性派发的用量（`<AICROSS_HOME>/usage/<年-月>.jsonl`）。
//!
//! aicross 派发 claude、codex、pi 时关掉了会话保存，这些调用不会出现在各家
//! 自己的日志里；它改为每次派发追加一行用量记录（schema `aicross-usage/1`），
//! 四个分量互不重叠。kimi 与 agy 的会话仍由各自的 CLI 保存，aicross 不为它们
//! 写这份记录，所以这里不会与原生来源重复计数。
//!
//! aicross 是派发工具，不是计量身份：用量记到实际执行的 Agent 名下，pi 再按
//! provider 归属，与 pi 自己的会话日志同一套规则。
//!
//! 已核对真机口径（2026-11，claude 与 codex 各一次真实派发）：四个分量之和等于
//! CLI 报告的输入总量加输出。pi 的记录沿用 pi 会话日志的分量定义。

use super::{
    discover_jsonl, AgentAdapter, JsonlRecords, ParsedScan, ScanDiagnostics, SourceCandidate,
};
use crate::domain::{ParsedSource, TokenVector, UsageEvent};
use crate::pi_providers;
use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

const SCHEMA: &str = "aicross-usage/1";

pub struct AicrossAdapter {
    roots: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct UsageLine {
    schema: String,
    id: String,
    at_ms: i64,
    #[serde(default)]
    run_id: String,
    agent: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    project: String,
    #[serde(default)]
    model: String,
    input_uncached: i64,
    cache_read: i64,
    cache_write: i64,
    output: i64,
}

impl AicrossAdapter {
    pub fn detected() -> Self {
        Self {
            roots: vec![aicross_home().join("usage")],
        }
    }

    #[cfg(test)]
    fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }
}

/// 与 aicross 的 `data_root()` 同源：`AICROSS_HOME` 覆盖，否则 `~/.aicross`。
fn aicross_home() -> PathBuf {
    std::env::var_os("AICROSS_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".aicross"))
}

/// 执行这次派发的 Agent 对应的计量卡片；不认识的执行者返回 None。
fn credited_agent(agent: &str, provider: &str) -> Option<&'static str> {
    match agent {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "pi" => Some(pi_providers::credited_agent(Some(provider))),
        _ => None,
    }
}

impl AgentAdapter for AicrossAdapter {
    fn id(&self) -> &'static str {
        "aicross"
    }

    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate> {
        discover_jsonl(&self.roots, self.id(), cutoff_ms)
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        let mut records = JsonlRecords::<UsageLine>::open(&candidate.path)?;
        let mut diagnostics = ScanDiagnostics::default();
        let track_skipped_lines = candidate.mtime_ns / 1_000_000 >= cutoff_ms;
        let mut events = Vec::new();

        for (_, line) in records.by_ref() {
            if line.at_ms < cutoff_ms {
                continue;
            }
            let tokens = TokenVector {
                input_uncached: line.input_uncached,
                cache_read: line.cache_read,
                cache_write: line.cache_write,
                output: line.output,
                reasoning_output: 0,
            };
            let readable = line.schema == SCHEMA
                && !line.id.is_empty()
                && [
                    tokens.input_uncached,
                    tokens.cache_read,
                    tokens.cache_write,
                    tokens.output,
                ]
                .iter()
                .all(|value| *value >= 0);
            // 新版记录格式、未知执行者和非法数值都如实计为读不了，不猜含义。
            let Some(agent) = credited_agent(&line.agent, &line.provider).filter(|_| readable)
            else {
                if track_skipped_lines {
                    diagnostics.rejected_events += 1;
                }
                continue;
            };
            let session_id = if line.run_id.is_empty() {
                format!("aicross:{}", line.id)
            } else {
                format!("aicross:{}", line.run_id)
            };
            events.push(
                UsageEvent::new(
                    agent,
                    format!("aicross:{}", line.id),
                    line.at_ms,
                    session_id,
                    Some(line.model).filter(|model| !model.is_empty()),
                    tokens,
                    "exact",
                )
                .with_project(Some(line.project)),
            );
        }
        records.record_skipped(&mut diagnostics, track_skipped_lines);

        Ok(ParsedScan {
            source: ParsedSource {
                source_id: candidate.source_id.clone(),
                adapter_id: self.id(),
                locator: candidate.path.clone(),
                logical_key: candidate.source_id.clone(),
                size: candidate.size,
                mtime_ns: candidate.mtime_ns,
                events,
                quotas: Vec::new(),
            },
            diagnostics,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(name: &str, body: &str, cutoff_ms: i64) -> ParsedScan {
        let dir =
            std::env::temp_dir().join(format!("metrik-aicross-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("2026-11.jsonl"), body).unwrap();
        let adapter = AicrossAdapter::with_roots(vec![dir.clone()]);
        let candidates = adapter.discover(i64::MIN);
        assert_eq!(candidates.len(), 1);
        let parsed = adapter.parse(&candidates[0], cutoff_ms).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        parsed
    }

    // 前两行是真机派发写出的记录（2026-11），只改了 project。
    const REAL: &str = r#"{"schema": "aicross-usage/1", "id": "d1b8dc53245347aa8f882feadf7b2613", "at_ms": 1794367412812, "run_id": "20261111-check", "agent": "claude", "provider": "", "project": "/work/demo", "model": "claude-haiku-5-5", "input_uncached": 2, "cache_read": 649, "cache_write": 2363, "output": 4}
{"schema": "aicross-usage/1", "id": "cda42ff81b1a4cfb8581075160b605bc", "at_ms": 1794367433829, "run_id": "20261111-check", "agent": "codex", "provider": "", "project": "/work/demo", "model": "gpt-5.6-luna", "input_uncached": 10122, "cache_read": 11008, "cache_write": 0, "output": 5}
{"schema": "aicross-usage/1", "id": "pi-glm", "at_ms": 1794367440000, "run_id": "", "agent": "pi", "provider": "zai-coding-cn", "project": "", "model": "glm-5.3", "input_uncached": 508, "cache_read": 3, "cache_write": 0, "output": 9}
{"schema": "aicross-usage/1", "id": "pi-direct", "at_ms": 1794367450000, "run_id": "r2", "agent": "pi", "provider": "anthropic", "project": "", "model": "", "input_uncached": 1, "cache_read": 0, "cache_write": 0, "output": 1}
"#;

    #[test]
    fn dispatches_are_credited_to_the_agent_that_ran_them() {
        let parsed = parse("credit", REAL, i64::MIN);
        assert!(!parsed.diagnostics.is_partial());
        let events = &parsed.source.events;
        let summary: Vec<_> = events
            .iter()
            .map(|event| (event.adapter_id, event.tokens.processed()))
            .collect();
        assert_eq!(
            summary,
            [
                ("claude", 3018),
                ("codex", 21135),
                ("zcode", 520),
                ("pi", 2)
            ]
        );
        assert_eq!(events[0].tokens.cache_write, 2363);
        assert_eq!(events[0].model.as_deref(), Some("claude-haiku-5-5"));
        assert_eq!(events[0].project_path.as_deref(), Some("/work/demo"));
        // 同一次派发的各路归到一个会话；没有派发编号时各自成会话。
        assert_eq!(events[0].session_id, events[1].session_id);
        assert_eq!(events[2].session_id, "aicross:pi-glm");
        assert_eq!(events[2].project_path, None);
        assert_eq!(events[3].model, None);
    }

    #[test]
    fn unreadable_records_are_counted_not_guessed() {
        let body = format!(
            "{REAL}{}\n{}\n{}\nnot json\n",
            r#"{"schema": "aicross-usage/2", "id": "future", "at_ms": 1794367460000, "agent": "claude", "input_uncached": 9, "cache_read": 0, "cache_write": 0, "output": 9}"#,
            r#"{"schema": "aicross-usage/1", "id": "other", "at_ms": 1794367460000, "agent": "kimi", "input_uncached": 9, "cache_read": 0, "cache_write": 0, "output": 9}"#,
            r#"{"schema": "aicross-usage/1", "id": "negative", "at_ms": 1794367460000, "agent": "codex", "input_uncached": -1, "cache_read": 0, "cache_write": 0, "output": 9}"#,
        );
        let parsed = parse("unreadable", &body, i64::MIN);
        assert_eq!(parsed.source.events.len(), 4);
        assert_eq!(parsed.diagnostics.rejected_events, 3);
        assert_eq!(parsed.diagnostics.malformed_lines, 1);
    }

    #[test]
    fn records_before_the_cutoff_are_skipped() {
        let parsed = parse("cutoff", REAL, 1_794_367_440_000);
        assert_eq!(parsed.source.events.len(), 2);
        assert!(!parsed.diagnostics.is_partial());
    }
}
