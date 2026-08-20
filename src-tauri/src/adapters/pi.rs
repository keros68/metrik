//! Pi（badlogic/pi-mono）及其同格式分支（Oh My Pi 等）的会话日志（JSONL）。
//!
//! 布局：`~/.pi/agent/sessions/--<编码 cwd>--/<时间戳>_<uuid>.jsonl`（OMP 在
//! `~/.omp/agent/sessions/`）。每个文件一个会话：首行 `session` 头（含真实
//! `cwd`，目录名是有损编码不能反推），其后为树形 entry 列表。
//!
//! 计量口径（2026-08 按本机 849 条 assistant usage 行核实）：
//! - 只计 `type=="message"` 且 `message.role=="assistant"` 且带 `usage` 的行；
//!   每行是一次 API 调用的最终用量（流式中间态不落盘，`stopReason=="pending"`
//!   的部分消息不会持久化），不是累计计数器。
//! - `totalTokens == input + output + cacheRead + cacheWrite` 在全部样本成立，
//!   `reasoning` 是 output 的子项（tokscale 的 Pi 解析器同此结论）。逐行做
//!   `disagrees_with_reported_total` 自检。
//! - `cost` 对订阅套餐恒为 0（wire 端就是零费率），成本估算交给 pricing 层。
//!
//! 身份：assistant 消息带 provider 生成的 `responseId`（本机 849 行里 848 行
//! 有、全局唯一；唯一缺失的一行是 `stopReason=="aborted"`）。它跨会话稳定，
//! `/fork`、`/clone` 把 entry 原样复制进新文件后仍是同一逻辑事件——以它为主键
//! （与 Claude 的 message id 同型）。缺失时退回
//! `session + entry id + 时间戳`。`compaction` / `branch_summary` 顶层 `usage`
//! 与 `toolResult` 内嵌 `usage` 是摘要生成/工具内嵌 LLM 调用的真实计费，pi
//! 自己的会话合计也包含它们，一并入账。
//!
//! 会话是树（`/tree` 分叉）但每个 entry 有独立 id，分叉不复制值、不产生新事件。

use super::{
    discover_jsonl, timestamp_str_ms, AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate,
};
use crate::domain::{ParsedSource, TokenVector, UsageEvent};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

pub struct PiAdapter {
    roots: Vec<PathBuf>,
}

/// 会话头（首行，或 OMP 的 `title` 记录之后）。
#[derive(Deserialize)]
struct PiSessionHeader {
    id: Option<String>,
    cwd: Option<String>,
}

/// 一条 entry。未知字段交给调用方按 `type` 分发。
#[derive(Deserialize)]
struct PiEntry {
    #[serde(rename = "type")]
    entry_type: String,
    id: Option<String>,
    timestamp: Option<String>,
    message: Option<PiMessage>,
    /// `compaction` / `branch_summary` 的摘要生成用量挂在 entry 顶层。
    usage: Option<PiUsage>,
}

#[derive(Deserialize)]
struct PiMessage {
    role: Option<String>,
    model: Option<String>,
    /// provider 生成的响应标识；aborted 消息可能缺失。
    #[serde(rename = "responseId")]
    response_id: Option<String>,
    usage: Option<PiUsage>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PiUsage {
    #[serde(default)]
    input: i64,
    #[serde(default)]
    output: i64,
    #[serde(default)]
    cache_read: i64,
    #[serde(default)]
    cache_write: i64,
    #[serde(default)]
    total_tokens: i64,
    /// output 的子项，只作展示明细，processed() 不重复相加。
    #[serde(default)]
    reasoning: i64,
}

impl PiUsage {
    fn tokens(&self) -> TokenVector {
        TokenVector {
            input_uncached: self.input.max(0),
            cache_read: self.cache_read.max(0),
            cache_write: self.cache_write.max(0),
            output: self.output.max(0),
            reasoning_output: self.reasoning.max(0),
        }
    }
}

/// OMP 在 `session` 头之前可能写入的元数据记录（tokscale#802）：跳过即可。
const PRE_SESSION_METADATA_TYPES: &[&str] = &["title"];

#[derive(Clone)]
struct PendingEvent {
    timestamp: i64,
    session_id: String,
    event_key: String,
    model: Option<String>,
    tokens: TokenVector,
}

impl PiAdapter {
    pub fn detected() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            roots: vec![
                home.join(".pi").join("agent").join("sessions"),
                home.join(".omp").join("agent").join("sessions"),
            ],
        }
    }

    #[cfg(test)]
    fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }
}

impl AgentAdapter for PiAdapter {
    fn id(&self) -> &'static str {
        "pi"
    }

    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate> {
        discover_jsonl(&self.roots, self.id(), cutoff_ms)
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        let file = File::open(&candidate.path)
            .with_context(|| format!("failed to open {}", candidate.path.display()))?;
        let reader = BufReader::with_capacity(256 * 1024, file);

        let mut session_id: Option<String> = None;
        let mut project: Option<String> = None;
        let mut saw_content = false;
        // 头之前出现未知记录时丢弃整个文件：那不是 Pi 的会话文件（或格式已
        // 变），读下去只会读错。
        let mut discard_file = false;
        // 同 key 分组：同文件内重复出现同一 responseId 是异常（正常一条 entry 一行、
        // 落盘即最终值），按 Claude 的处理拒绝含糊的那条，保留其余事件。
        let mut events: HashMap<String, PendingEvent> = HashMap::new();
        let mut rejected_keys: HashSet<String> = HashSet::new();
        let mut diagnostics = ScanDiagnostics::default();
        let track_skipped_lines = candidate.mtime_ns / 1_000_000 >= cutoff_ms;

        for line in reader.lines() {
            let line = match line {
                Ok(line) => line,
                Err(_) => {
                    if track_skipped_lines {
                        diagnostics.unreadable_lines += 1;
                    }
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            saw_content = true;
            // 头之前的记录只允许已知元数据；其余视为外来文件，整个丢弃（与
            // tokscale 的处理一致：宁可不读，不读错）。
            if session_id.is_none() {
                let Ok(probe) = serde_json::from_str::<PiEntryTypeProbe>(&line) else {
                    if track_skipped_lines {
                        diagnostics.malformed_lines += 1;
                    }
                    continue;
                };
                match probe.entry_type.as_str() {
                    "session" => {
                        if let Ok(header) = serde_json::from_str::<PiSessionHeader>(&line) {
                            session_id = Some(
                                header
                                    .id
                                    .filter(|id| !id.trim().is_empty())
                                    .unwrap_or_else(|| "unknown-session".into()),
                            );
                            project = header.cwd;
                        }
                        continue;
                    }
                    kind if PRE_SESSION_METADATA_TYPES.contains(&kind) => continue,
                    _ => {
                        if track_skipped_lines {
                            diagnostics.malformed_lines += 1;
                        }
                        discard_file = true;
                        break;
                    }
                }
            }

            let Ok(entry) = serde_json::from_str::<PiEntry>(&line) else {
                // 活跃文件末尾可能是半行，下次扫描会重新读取。
                if track_skipped_lines {
                    diagnostics.malformed_lines += 1;
                }
                continue;
            };
            let session = session_id.clone().unwrap_or_default();
            let timestamp = timestamp_str_ms(entry.timestamp.as_deref());
            let (usage, model, identity) = match entry.entry_type.as_str() {
                "message" => {
                    let Some(message) = entry.message else {
                        continue;
                    };
                    match message.role.as_deref() {
                        Some("assistant") => {
                            let Some(usage) = message.usage else {
                                continue;
                            };
                            let identity = match message
                                .response_id
                                .as_deref()
                                .map(str::trim)
                                .filter(|id| !id.is_empty())
                            {
                                Some(response_id) => format!("response:{response_id}"),
                                None => fallback_identity(&entry.id, timestamp),
                            };
                            (usage, message.model, identity)
                        }
                        // 工具内嵌 LLM 调用的用量：真实计费，但没有 provider
                        // 响应标识，退回事件内身份。
                        Some("toolResult") => {
                            let Some(usage) = message.usage else {
                                continue;
                            };
                            (
                                usage,
                                message.model,
                                fallback_identity(&entry.id, timestamp),
                            )
                        }
                        _ => continue,
                    }
                }
                "compaction" | "branch_summary" => {
                    let Some(usage) = entry.usage else {
                        continue;
                    };
                    let prefix = if entry.entry_type == "compaction" {
                        "compaction"
                    } else {
                        "branch"
                    };
                    // 身份同样不含会话 id：fork/clone 会连 compaction 条目一起复制。
                    (
                        usage,
                        None,
                        format!("{prefix}:{}", entry.id.as_deref().unwrap_or("?")),
                    )
                }
                _ => continue,
            };
            let Some(timestamp) = timestamp else {
                continue;
            };
            if timestamp < cutoff_ms {
                continue;
            }

            let tokens = usage.tokens();
            if tokens.processed() == 0 {
                continue;
            }
            if tokens.disagrees_with_reported_total(usage.total_tokens) {
                diagnostics.total_mismatches += 1;
            }

            if rejected_keys.contains(&identity) {
                continue;
            }
            let candidate_event = PendingEvent {
                timestamp,
                session_id: session.clone(),
                event_key: identity.clone(),
                model: model.clone(),
                tokens: tokens.clone(),
            };
            match events.get_mut(&identity) {
                Some(stored) => {
                    let model_conflict = match (stored.model.as_deref(), model.as_deref()) {
                        (Some(stored_model), Some(candidate_model)) => {
                            stored_model != candidate_model
                        }
                        _ => false,
                    };
                    if model_conflict {
                        events.remove(&identity);
                        rejected_keys.insert(identity);
                        diagnostics.rejected_events += 1;
                        continue;
                    }
                    // 同 key 同模型：正常只会在文件被原样复制时出现（fork/clone），
                    // 分量取最大值即可；真异常由口径自检兜底。
                    stored.tokens.component_max(&tokens);
                    if timestamp >= stored.timestamp {
                        stored.timestamp = timestamp;
                    }
                }
                None => {
                    events.insert(identity, candidate_event);
                }
            }
        }

        // 有内容但没有 session 头：不是 Pi 的会话文件（或格式变了），整文件不计。
        if session_id.is_none() && saw_content && !discard_file {
            diagnostics.malformed_lines += 1;
        }
        if discard_file {
            events.clear();
        }

        let session_id = session_id.unwrap_or_else(|| "unknown-session".into());
        let mut events: Vec<UsageEvent> = events
            .into_values()
            .map(|event| {
                UsageEvent::new(
                    self.id(),
                    event.event_key,
                    event.timestamp,
                    event.session_id,
                    event.model,
                    event.tokens,
                    "exact",
                )
                .with_project(project.clone())
            })
            .collect();
        events.sort_by_key(|event| event.occurred_at_ms);

        Ok(ParsedScan {
            source: ParsedSource {
                source_id: candidate.source_id.clone(),
                adapter_id: self.id(),
                locator: candidate.path.clone(),
                logical_key: session_id,
                size: candidate.size,
                mtime_ns: candidate.mtime_ns,
                events,
                quotas: Vec::new(),
            },
            diagnostics,
        })
    }
}

#[derive(Deserialize)]
struct PiEntryTypeProbe {
    #[serde(rename = "type")]
    entry_type: String,
}

/// 无 `responseId` 的行（如 aborted）退回事件内身份：entry id + 时间戳。
/// 刻意不含会话 id：entry id 是文件内唯一的 8 位十六进制，加上毫秒时间戳
/// 后跨会话碰撞可忽略，而含会话 id 会让 /fork 复制的同一条 aborted 消息
/// 变成两个事件。
fn fallback_identity(entry_id: &Option<String>, timestamp: Option<i64>) -> String {
    let id = entry_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| timestamp.map(|value| value.to_string()).unwrap_or_default());
    format!("fallback:{id}")
}

// 会话文件目录名（`--D--work-usage--`）是有损编码，不可反推项目路径；
// 项目归属只认 session 头里的 `cwd`（见 `project_attribution_never_comes_
// from_the_directory_name` 测试）。

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    fn session_file(label: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "metrik-pi-{label}-{}-{}.jsonl",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let mut file = File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        path
    }

    fn candidate_for(path: &Path) -> SourceCandidate {
        let metadata = path.metadata().unwrap();
        SourceCandidate {
            source_id: "source".into(),
            path: path.to_path_buf(),
            size: metadata.len(),
            mtime_ns: 1,
        }
    }

    const HEADER: &str = r#"{"type":"session","version":3,"id":"01a01c94-3c09-7714-b020-f054ef4540aa","timestamp":"2026-08-20T00:31:11.882Z","cwd":"D:\\work\\usage"}"#;

    #[test]
    fn assistant_usage_is_counted_with_response_identity_and_header_cwd() {
        // 字段形状取自本机真实日志（zai-coding-cn / glm-5.3）。
        let path = session_file(
            "assistant",
            concat!(
                r#"{"type":"session","version":3,"id":"ses-1","timestamp":"2026-08-20T00:31:11.882Z","cwd":"D:\\work\\usage"}"#,
                "\n",
                r#"{"type":"message","id":"631fa049","parentId":"793123d5","timestamp":"2026-08-20T00:32:00.897Z","message":{"role":"assistant","content":[{"type":"text","text":"hi"}],"provider":"zai-coding-cn","model":"glm-5.3","responseId":"202608171505273491557405ff476a","usage":{"input":2882,"output":561,"cacheRead":1088,"cacheWrite":0,"reasoning":503,"totalTokens":4531,"cost":{"total":0}},"stopReason":"stop"}}"#,
                "\n",
            ),
        );

        let parsed = PiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 1);
        let event = &parsed.source.events[0];
        assert_eq!(event.event_key, "response:202608171505273491557405ff476a");
        assert_eq!(event.tokens.input_uncached, 2_882);
        assert_eq!(event.tokens.cache_read, 1_088);
        assert_eq!(event.tokens.output, 561);
        // reasoning 是 output 子项，只记明细不重复计入 processed。
        assert_eq!(event.tokens.reasoning_output, 503);
        assert_eq!(event.tokens.processed(), 4_531);
        assert_eq!(event.model.as_deref(), Some("glm-5.3"));
        assert_eq!(event.project_path.as_deref(), Some("D:/work/usage"));
        assert_eq!(parsed.source.logical_key, "ses-1");
        assert!(!parsed.diagnostics.is_partial());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn an_aborted_message_without_response_id_falls_back_to_entry_identity() {
        // 本机真实样本：stopReason=aborted 的行没有 responseId 但带 usage——
        // 这次调用确实计费，照常入账。
        let path = session_file(
            "aborted",
            concat!(
                r#"{"type":"session","version":3,"id":"ses-1","cwd":"/tmp"}"#,
                "\n",
                r#"{"type":"message","id":"c6115304","timestamp":"2026-08-19T00:30:00.000Z","message":{"role":"assistant","provider":"zai-coding-cn","model":"glm-5.3","usage":{"input":100,"output":20,"cacheRead":0,"cacheWrite":0,"totalTokens":120},"stopReason":"aborted"}}"#,
                "\n",
            ),
        );

        let parsed = PiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].event_key, "fallback:c6115304");
        assert_eq!(parsed.source.events[0].tokens.processed(), 120);
        std::fs::remove_file(path).ok();
    }

    /// /fork 与 /clone 把 entry 原样复制进新会话文件：responseId 不变 →
    /// 两个源解析出同一 event_key 与同一 payload，账本按同一事件去重。
    #[test]
    fn a_forked_copy_keeps_the_same_identity() {
        let entry = r#"{"type":"message","id":"631fa049","timestamp":"2026-08-20T00:32:00.897Z","message":{"role":"assistant","provider":"zai-coding-cn","model":"glm-5.3","responseId":"resp-1","usage":{"input":2882,"output":561,"cacheRead":1088,"totalTokens":4531}}}"#;
        let parent_header =
            serde_json::json!({"type": "session", "id": "ses-parent", "cwd": "/tmp/p"}).to_string();
        let fork_header = serde_json::json!({
            "type": "session",
            "id": "ses-child",
            "cwd": "/tmp/c",
            "parentSession": "/parent.jsonl"
        })
        .to_string();
        let parent = session_file("fork-parent", &format!("{parent_header}\n{entry}\n"));
        let fork = session_file("fork-child", &format!("{fork_header}\n{entry}\n"));

        let adapter = PiAdapter::with_roots(vec![]);
        let parent_scan = adapter.parse(&candidate_for(&parent), i64::MIN).unwrap();
        let fork_scan = adapter.parse(&candidate_for(&fork), i64::MIN).unwrap();

        assert_eq!(parent_scan.source.events.len(), 1);
        assert_eq!(fork_scan.source.events.len(), 1);
        assert_eq!(
            parent_scan.source.events[0].event_key,
            fork_scan.source.events[0].event_key
        );
        // payload_hash 含会话 id，副本与原件必然不同：账本层对 pi 做分量最大值
        // 合并（见 storage 的 `pi_fork_copy_observes_one_event`），此处不比较。
        // 会话各自记录，项目归属各归各的 cwd。
        assert_eq!(fork_scan.source.events[0].session_id, "ses-child");
        assert_eq!(
            fork_scan.source.events[0].project_path.as_deref(),
            Some("/tmp/c")
        );
        std::fs::remove_file(parent).ok();
        std::fs::remove_file(fork).ok();
    }

    #[test]
    fn compaction_and_toolresult_usage_are_counted() {
        // 摘要生成与工具内嵌 LLM 调用都是真实计费；pi 自己的会话合计也含它们。
        let path = session_file(
            "compaction",
            concat!(
                r#"{"type":"session","id":"ses-1","cwd":"/tmp"}"#,
                "\n",
                r#"{"type":"compaction","id":"f6g7h8i9","timestamp":"2026-08-20T01:00:00.000Z","summary":"…","tokensBefore":50000,"usage":{"input":8000,"output":900,"cacheRead":0,"cacheWrite":0,"totalTokens":8900}}"#,
                "\n",
                r#"{"type":"branch_summary","id":"g7h8i9j0","timestamp":"2026-08-20T01:05:00.000Z","fromId":"f6g7h8i9","summary":"…","usage":{"input":500,"output":100,"totalTokens":600}}"#,
                "\n",
                r#"{"type":"message","id":"t1","timestamp":"2026-08-20T01:06:00.000Z","message":{"role":"toolResult","toolCallId":"call_1","toolName":"ask_question","usage":{"input":50,"output":10,"totalTokens":60},"isError":false}}"#,
                "\n",
                // 无 usage 的普通 toolResult 不计。
                r#"{"type":"message","id":"t2","timestamp":"2026-08-20T01:07:00.000Z","message":{"role":"toolResult","toolCallId":"call_2","toolName":"bash","isError":false}}"#,
                "\n",
            ),
        );

        let parsed = PiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        let keys: Vec<&str> = parsed
            .source
            .events
            .iter()
            .map(|event| event.event_key.as_str())
            .collect();
        assert_eq!(
            keys,
            vec!["compaction:f6g7h8i9", "branch:g7h8i9j0", "fallback:t1"]
        );
        let total: i64 = parsed
            .source
            .events
            .iter()
            .map(|event| event.tokens.processed())
            .sum();
        assert_eq!(total, 8_900 + 600 + 60);
        assert!(!parsed.diagnostics.is_partial());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn a_reported_total_that_disagrees_marks_the_source_partial() {
        let path = session_file(
            "mismatch",
            concat!(
                r#"{"type":"session","id":"ses-1","cwd":"/tmp"}"#,
                "\n",
                r#"{"type":"message","id":"m1","timestamp":"2026-08-20T00:32:00.000Z","message":{"role":"assistant","model":"glm-5.3","responseId":"r1","usage":{"input":100,"output":50,"totalTokens":999}}}"#,
                "\n",
            ),
        );

        let parsed = PiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.diagnostics.total_mismatches, 1);
        assert!(parsed.diagnostics.is_partial());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn duplicate_identity_with_a_conflicting_model_is_rejected_keeping_other_events() {
        let path = session_file(
            "conflict",
            concat!(
                r#"{"type":"session","id":"ses-1","cwd":"/tmp"}"#,
                "\n",
                r#"{"type":"message","id":"a1","timestamp":"2026-08-20T00:32:00.000Z","message":{"role":"assistant","model":"glm-5.3","responseId":"r1","usage":{"input":100,"output":50,"totalTokens":150}}}"#,
                "\n",
                r#"{"type":"message","id":"a2","timestamp":"2026-08-20T00:33:00.000Z","message":{"role":"assistant","model":"glm-5.2","responseId":"r1","usage":{"input":200,"output":80,"totalTokens":280}}}"#,
                "\n",
                r#"{"type":"message","id":"a3","timestamp":"2026-08-20T00:34:00.000Z","message":{"role":"assistant","model":"glm-5.3","responseId":"r2","usage":{"input":10,"output":5,"totalTokens":15}}}"#,
                "\n",
            ),
        );

        let parsed = PiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].event_key, "response:r2");
        assert_eq!(parsed.diagnostics.rejected_events, 1);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn title_records_before_the_header_are_skipped_and_unknown_ones_discard_the_file() {
        // OMP 会先写自动标题记录再写 session 头；不认识的头前置记录按
        // 外来文件处理，整文件不计并标注数据不完整。
        let tolerant = session_file(
            "omp-title",
            concat!(
                r#"{"type":"title","v":1,"title":"Comment on GitHub issue","source":"auto"}"#,
                "\n",
                r#"{"type":"session","id":"ses-omp","cwd":"/tmp"}"#,
                "\n",
                r#"{"type":"message","id":"m1","timestamp":"2026-08-20T00:32:00.000Z","message":{"role":"assistant","model":"glm-5.3","responseId":"r1","usage":{"input":10,"output":5,"totalTokens":15}}}"#,
                "\n",
            ),
        );
        let foreign = session_file(
            "foreign",
            concat!(
                r#"{"type":"totally_unknown_thing","foo":"bar"}"#,
                "\n",
                r#"{"type":"session","id":"ses-x","cwd":"/tmp"}"#,
                "\n",
                r#"{"type":"message","id":"m1","timestamp":"2026-08-20T00:32:00.000Z","message":{"role":"assistant","model":"glm-5.3","responseId":"r1","usage":{"input":10,"output":5,"totalTokens":15}}}"#,
                "\n",
            ),
        );

        let adapter = PiAdapter::with_roots(vec![]);
        let parsed = adapter.parse(&candidate_for(&tolerant), i64::MIN).unwrap();
        assert_eq!(parsed.source.events.len(), 1);
        assert!(!parsed.diagnostics.is_partial());

        let parsed = adapter.parse(&candidate_for(&foreign), i64::MIN).unwrap();
        assert!(parsed.source.events.is_empty());
        assert_eq!(parsed.diagnostics.malformed_lines, 1);
        std::fs::remove_file(tolerant).ok();
        std::fs::remove_file(foreign).ok();
    }

    #[test]
    fn malformed_lines_and_cutoff_respect_diagnostics() {
        let path = session_file(
            "diagnostics",
            concat!(
                r#"{"type":"session","id":"ses-1","cwd":"/tmp"}"#,
                "\n",
                "not-json\n",
                r#"{"type":"message","id":"old","timestamp":"2026-08-01T00:00:00.000Z","message":{"role":"assistant","model":"glm-5.3","responseId":"r-old","usage":{"input":10,"output":5,"totalTokens":15}}}"#,
                "\n",
                r#"{"type":"message","id":"new","timestamp":"2026-08-20T00:00:00.000Z","message":{"role":"assistant","model":"glm-5.3","responseId":"r-new","usage":{"input":20,"output":8,"totalTokens":28}}}"#,
                "\n",
            ),
        );

        let cutoff = timestamp_str_ms(Some("2026-08-10T00:00:00.000Z")).unwrap();
        // mtime 设为当前时间，与 cutoff 同代：跳行诊断才会如实计数。
        let now_ns = chrono::Utc::now().timestamp_millis() * 1_000_000;
        let candidate = SourceCandidate {
            source_id: "source".into(),
            path: path.clone(),
            size: path.metadata().unwrap().len(),
            mtime_ns: now_ns,
        };
        let parsed = PiAdapter::with_roots(vec![])
            .parse(&candidate, cutoff)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].event_key, "response:r-new");
        assert_eq!(parsed.diagnostics.malformed_lines, 1);
        assert!(parsed.diagnostics.is_partial());
        std::fs::remove_file(path).ok();
    }

    /// 目录名是有损编码（路径分隔符与字面 `-` 都写成 `-`），项目归属只能来自
    /// session 头的 cwd；没有 cwd 就保持未归属，不从目录名反推。
    #[test]
    fn project_attribution_never_comes_from_the_directory_name() {
        let dir = std::env::temp_dir().join(format!(
            "metrik-pi-dirname-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"session","id":"ses-1"}"#,
                "\n",
                r#"{"type":"message","id":"m1","timestamp":"2026-08-20T00:32:00.000Z","message":{"role":"assistant","model":"glm-5.3","responseId":"r1","usage":{"input":10,"output":5,"totalTokens":15}}}"#,
                "\n",
            ),
        )
        .unwrap();

        let parsed = PiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].project_path, None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn detected_roots_cover_pi_and_omp() {
        let home = dirs::home_dir().unwrap_or_default();
        let adapter = PiAdapter::detected();
        assert_eq!(
            adapter.roots,
            vec![
                home.join(".pi").join("agent").join("sessions"),
                home.join(".omp").join("agent").join("sessions"),
            ]
        );
    }

    #[test]
    fn the_header_macro_matches_the_documented_shape() {
        // 防止示例漂移：上面多条测试手写了头，这里固定真实形状可解析。
        let header: PiSessionHeader = serde_json::from_str(HEADER).unwrap();
        assert_eq!(
            header.id.as_deref(),
            Some("01a01c94-3c09-7714-b020-f054ef4540aa")
        );
        assert_eq!(header.cwd.as_deref(), Some("D:\\work\\usage"));
    }
}
