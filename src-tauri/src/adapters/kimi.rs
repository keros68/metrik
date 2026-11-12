use super::{
    discover_files, file_mtime_ns, non_empty, AgentAdapter, JsonlRecords, ParsedScan,
    ScanDiagnostics, SourceCandidate,
};
use crate::domain::{ParsedSource, TokenVector, UsageEvent};
use anyhow::Result;
use serde::Deserialize;
use serde_json::value::RawValue;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Kimi CLI / Kimi Code 的 wire 日志（JSONL）。两代格式并存：
///
/// - 新版 `~/.kimi-code/sessions/<workspace>/<session>/agents/<agent>/wire.jsonl`：
///   顶层 `{"type":"usage.record","model":…,"usage":{camelCase},"usageScope":"turn|session","time":<ms>}`。
///   **只计 `usageScope == "turn"`（单轮增量）**；`session` 作用域是会话累计总量，
///   计入会重复计数。
/// - 旧版 `~/.kimi/sessions/<group>/<session>/wire.jsonl`：
///   `{"timestamp":<秒·浮点>,"message":{"type":"StatusUpdate","payload":{"token_usage":{snake_case},"message_id":…}}}`。
///   旧版无 scope 字段且未确认是否对同一 `message_id` 渐进更新，因此按 message_id
///   合并、分量取最大值——真增量时每个 id 只出现一次（取 max 无害），渐进更新时
///   正好避免重复计数。
///
/// 会话 ID 来自目录路径（记录里没有）。旧版 StatusUpdate 不带模型名，
/// 保持 `None`（诚实标注"未标注模型"，不猜测）。
pub struct KimiAdapter {
    roots: Vec<PathBuf>,
    project_cache: Mutex<HashMap<PathBuf, HomeProjectCache>>,
}

#[derive(Default)]
struct HomeProjectCache {
    workspaces_stamp: Option<(i64, u64)>,
    workspaces: HashMap<String, String>,
    session_index_stamp: Option<(i64, u64)>,
    session_dirs: HashMap<String, String>,
}

/// 顶层轻量信封：`usage`、`event`、`message` 都保留为 `RawValue`，只在命中对应
/// 记录类型后才反序列化载荷。这样既避免为每条无关请求/工具快照分配嵌套结构，
/// 也防止 `{"type":"turn.step.interrupted","message":"..."}` 这类同名字符串字段
/// 把合法会话误判成 `malformed_lines`（数据残缺）。
#[derive(Deserialize, Default)]
struct KimiRecord {
    // 新版
    #[serde(rename = "type")]
    record_type: Option<String>,
    model: Option<String>,
    usage: Option<Box<RawValue>>,
    #[serde(rename = "usageScope")]
    usage_scope: Option<String>,
    time: Option<i64>,
    /// `context.append_loop_event` 的载荷；只读 `step.end` 的用量与耗时。
    event: Option<Box<RawValue>>,
    // 旧版
    timestamp: Option<f64>,
    message: Option<Box<RawValue>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LoopEvent {
    #[serde(rename = "type")]
    event_type: Option<String>,
    usage: Option<NewUsage>,
    llm_first_token_latency_ms: Option<i64>,
    llm_stream_duration_ms: Option<i64>,
}

#[derive(Deserialize, Default, PartialEq)]
struct NewUsage {
    #[serde(rename = "inputOther", default)]
    input_other: i64,
    #[serde(rename = "inputCacheRead", default)]
    input_cache_read: i64,
    #[serde(rename = "inputCacheCreation", default)]
    input_cache_creation: i64,
    #[serde(default)]
    output: i64,
}

#[derive(Deserialize, Default)]
struct LegacyMessage {
    #[serde(rename = "type")]
    message_type: Option<String>,
    payload: Option<LegacyPayload>,
}

#[derive(Deserialize, Default)]
struct LegacyPayload {
    token_usage: Option<LegacyUsage>,
    message_id: Option<String>,
}

#[derive(Deserialize, Default)]
struct LegacyUsage {
    #[serde(default)]
    input_other: i64,
    #[serde(default)]
    input_cache_read: i64,
    #[serde(default)]
    input_cache_creation: i64,
    #[serde(default)]
    output: i64,
}

pub fn kimi_code_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    std::env::var_os("KIMI_CODE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".kimi-code"))
}

impl KimiAdapter {
    pub fn detected() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        // 新版数据根可被 KIMI_CODE_HOME 覆盖。
        let kimi_code = kimi_code_dir();
        // Kimi Work（kimi-desktop）内嵌同源 kimi-code 内核，把会话写在
        // daimon 运行时的 home 下（2026-08 真机核实：wire.jsonl 逐字段同构，
        // 含 usageScope=turn；项目归属在 home/session_index.jsonl）。与
        // coding_quota::kimiwork_token_paths 的根一致。
        let kimi_work = dirs::config_dir()
            .map(|config| {
                config
                    .join("kimi-desktop")
                    .join("daimon-share")
                    .join("daimon")
                    .join("runtime")
                    .join("kimi-code")
                    .join("home")
                    .join("sessions")
            })
            .unwrap_or_else(|| home.join(".kimi-desktop-missing"));
        Self {
            roots: vec![
                kimi_code.join("sessions"),
                home.join(".kimi").join("sessions"),
                kimi_work,
            ],
            project_cache: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            project_cache: Mutex::new(HashMap::new()),
        }
    }

    /// 项目归属映射，按 `<home>` 缓存 `workspaces.json` 与 `session_index.jsonl`（按
    /// mtime + size 失效），避免上百个 `wire.jsonl` 每个都重复读盘与全量反序列化。
    fn project_root_from_path(&self, path: &Path) -> Option<String> {
        let sessions_dir = path
            .ancestors()
            .find(|dir| dir.file_name().and_then(|name| name.to_str()) == Some("sessions"))?;
        let home = sessions_dir.parent()?;
        let mut segments = path.strip_prefix(sessions_dir).ok()?.iter();
        let workspace_id = segments.next()?.to_str()?;
        let session_id = segments.next().and_then(|value| value.to_str());

        let Ok(mut guard) = self.project_cache.lock() else {
            return None;
        };
        let entry = guard.entry(home.to_path_buf()).or_default();

        let workspaces_path = home.join("workspaces.json");
        let ws_stamp = file_stamp(&workspaces_path);
        if entry.workspaces_stamp != ws_stamp {
            match ws_stamp {
                None => {
                    entry.workspaces.clear();
                    entry.workspaces_stamp = None;
                }
                Some(_) => match std::fs::read_to_string(&workspaces_path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<KimiWorkspaces>(&raw).ok())
                {
                    Some(parsed) => {
                        let mut next = HashMap::with_capacity(parsed.workspaces.len());
                        for (id, ws) in parsed.workspaces {
                            if let Some(root) = ws.root.filter(|r| !r.trim().is_empty()) {
                                next.insert(id, root);
                            }
                        }
                        entry.workspaces = next;
                        entry.workspaces_stamp = ws_stamp;
                    }
                    // 文件被瞬时锁住或非原子重写截断时保留旧映射（stale-on-error），
                    // 不缓存当前 stamp 以便后续查询重试，避免当次解析把会话记为未归属。
                    None => entry.workspaces_stamp = None,
                },
            }
        }
        if let Some(root) = entry.workspaces.get(workspace_id) {
            return Some(root.clone());
        }

        if let Some(session_id) = session_id {
            let index_path = home.join("session_index.jsonl");
            let idx_stamp = file_stamp(&index_path);
            if entry.session_index_stamp != idx_stamp {
                match idx_stamp {
                    None => {
                        entry.session_dirs.clear();
                        entry.session_index_stamp = None;
                    }
                    Some(_) => match std::fs::read_to_string(&index_path) {
                        Ok(raw) if raw.trim().is_empty() && !entry.session_dirs.is_empty() => {
                            // 已有映射时读到空文件，通常是截断重写的中间态，保留旧映射等重试。
                            entry.session_index_stamp = None;
                        }
                        Ok(raw) => {
                            let (dirs, truncated_tail) = parse_session_index_map(&raw);
                            if truncated_tail {
                                entry.session_dirs.extend(dirs);
                                entry.session_index_stamp = None;
                            } else {
                                entry.session_dirs = dirs;
                                entry.session_index_stamp = idx_stamp;
                            }
                        }
                        Err(_) => entry.session_index_stamp = None,
                    },
                }
            }
            if let Some(dir) = entry.session_dirs.get(session_id) {
                return Some(dir.clone());
            }
        }
        None
    }
}

fn file_stamp(path: &Path) -> Option<(i64, u64)> {
    let meta = path.metadata().ok()?;
    Some((file_mtime_ns(&meta), meta.len()))
}

/// 会话 ID 取自路径：新版 `sessions/<workspace>/<session>/agents/<agent>/wire.jsonl`
/// 取 `<session>/<agent>`（子 agent 各自成流，合并会丢失粒度）；旧版
/// `sessions/<group>/<session>/wire.jsonl` 取 `<session>`。
fn session_id_from_path(path: &Path) -> String {
    let parts: Vec<&str> = path
        .iter()
        .filter_map(|part| part.to_str())
        .map(|part| part.trim_end_matches('/'))
        .collect();
    let agents_at = parts.iter().rposition(|part| *part == "agents");
    if let Some(index) = agents_at {
        if index >= 1 && index + 1 < parts.len() {
            return format!("{}/{}", parts[index - 1], parts[index + 1]);
        }
    }
    path.parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("unknown-session")
        .to_owned()
}

#[derive(Deserialize, Default)]
struct KimiWorkspaces {
    #[serde(default)]
    workspaces: BTreeMap<String, KimiWorkspace>,
}

#[derive(Deserialize, Default)]
struct KimiWorkspace {
    root: Option<String>,
}

#[derive(Deserialize)]
struct SessionIndexEntry {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "workDir")]
    work_dir: Option<String>,
}

/// Kimi Work 的会话索引（home/session_index.jsonl）：每行一个会话，
/// `workDir` 是真实工作目录。同 sessionId 多行时后行覆盖（目录可能被改）。
/// 返回 `(映射表, 是否存在末尾无换行的半写坏行)`：中间已封尾的历史坏行跳过即可，
/// 只有文件末尾正在追加的半写行才要求调用方放弃固化缓存指纹并在下次重试。
fn parse_session_index_map(raw: &str) -> (HashMap<String, String>, bool) {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let ends_with_newline = raw.ends_with('\n');
    let lines: Vec<&str> = raw.lines().collect();
    let last_non_empty_idx = lines.iter().rposition(|line| !line.trim().is_empty());

    let mut dirs = HashMap::new();
    let mut truncated_tail = false;
    for (idx, line) in lines.into_iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<SessionIndexEntry>(line) else {
            if !ends_with_newline && Some(idx) == last_non_empty_idx {
                truncated_tail = true;
            }
            continue;
        };
        if let Some(id) = entry.session_id.filter(|id| !id.is_empty()) {
            match entry.work_dir.filter(|dir| !dir.trim().is_empty()) {
                Some(dir) => {
                    dirs.insert(id, dir);
                }
                None => {
                    dirs.remove(&id);
                }
            }
        }
    }
    (dirs, truncated_tail)
}

impl AgentAdapter for KimiAdapter {
    fn id(&self) -> &'static str {
        "kimi"
    }

    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate> {
        discover_files(&self.roots, self.id(), cutoff_ms, |path| {
            path.ends_with("wire.jsonl")
        })
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        let mut records = JsonlRecords::<KimiRecord>::open(&candidate.path)?;

        let session_id = session_id_from_path(&candidate.path);
        let project = self.project_root_from_path(&candidate.path);
        let mut events: Vec<UsageEvent> = Vec::new();
        // 旧版按 message_id 合并（分量取最大值），值同时记录首见时间。
        let mut legacy: BTreeMap<String, (i64, TokenVector)> = BTreeMap::new();
        let mut diagnostics = ScanDiagnostics::default();
        let track_skipped_lines = candidate.mtime_ns / 1_000_000 >= cutoff_ms;
        // 刚入账、还在等同一次请求 `step.end` 的单轮用量：(事件下标, 原始用量)。
        let mut awaiting_step_end: Option<(usize, NewUsage)> = None;

        for (_, record) in records.by_ref() {
            // `step.end` 紧跟在它那次请求的用量记录之后，带同一份用量和这次请求的
            // 耗时：首 token 延迟（含请求构建）加流式输出时长。用量对不上就不配对。
            if record.record_type.as_deref() == Some("context.append_loop_event") {
                let Some(raw_event) = record.event else {
                    continue;
                };
                let Ok(event) = serde_json::from_str::<LoopEvent>(raw_event.get()) else {
                    continue;
                };
                if event.event_type.as_deref() != Some("step.end") {
                    continue;
                }
                if let (Some((index, usage)), Some(step_usage)) =
                    (awaiting_step_end.take(), event.usage)
                {
                    if usage == step_usage {
                        let duration = event
                            .llm_first_token_latency_ms
                            .zip(event.llm_stream_duration_ms)
                            .filter(|(first, stream)| *first >= 0 && *stream >= 0)
                            .map(|(first, stream)| first + stream);
                        let timed = events[index].clone().with_response_duration(duration);
                        events[index] = timed;
                    }
                }
                continue;
            }
            // 新版：只认单轮增量记录。
            if record.record_type.as_deref() == Some("usage.record") {
                // scope 缺失时不猜（可能是未来新增的累计口径），跳过并标记部分覆盖。
                if record.usage_scope.as_deref() != Some("turn") {
                    if track_skipped_lines && record.usage_scope.is_none() {
                        diagnostics.malformed_lines += 1;
                    }
                    continue;
                }
                let (Some(raw_usage), Some(timestamp)) = (record.usage, record.time) else {
                    continue;
                };
                let Ok(usage) = serde_json::from_str::<NewUsage>(raw_usage.get()) else {
                    if track_skipped_lines {
                        diagnostics.malformed_lines += 1;
                    }
                    continue;
                };
                let tokens = TokenVector {
                    input_uncached: usage.input_other.max(0),
                    cache_read: usage.input_cache_read.max(0),
                    cache_write: usage.input_cache_creation.max(0),
                    output: usage.output.max(0),
                    reasoning_output: 0,
                };
                if tokens.processed() == 0 || timestamp < cutoff_ms {
                    awaiting_step_end = None;
                    continue;
                }
                awaiting_step_end = Some((events.len(), usage));
                let fingerprint = format!(
                    "{timestamp}:{}:{}:{}:{}",
                    tokens.input_uncached, tokens.cache_read, tokens.cache_write, tokens.output
                );
                events.push(
                    UsageEvent::new(
                        self.id(),
                        format!("{session_id}:{fingerprint}"),
                        timestamp,
                        session_id.clone(),
                        non_empty(record.model),
                        tokens,
                        "turn_delta",
                    )
                    .with_project(project.clone()),
                );
                continue;
            }
            if record.record_type.is_some() {
                continue;
            }

            // 旧版 StatusUpdate（顶层无 type 字段）。
            let Some(raw_message) = record.message else {
                continue;
            };
            let Ok(message) = serde_json::from_str::<LegacyMessage>(raw_message.get()) else {
                if track_skipped_lines {
                    diagnostics.malformed_lines += 1;
                }
                continue;
            };
            if message.message_type.as_deref() != Some("StatusUpdate") {
                continue;
            }
            let Some(payload) = message.payload else {
                continue;
            };
            let Some(usage) = payload.token_usage else {
                continue;
            };
            // 旧版时间戳是 Unix 秒（浮点），新版是毫秒——别搞混。
            let Some(timestamp) = record.timestamp.map(|value| (value * 1000.0) as i64) else {
                continue;
            };
            let tokens = TokenVector {
                input_uncached: usage.input_other.max(0),
                cache_read: usage.input_cache_read.max(0),
                cache_write: usage.input_cache_creation.max(0),
                output: usage.output.max(0),
                reasoning_output: 0,
            };
            if tokens.processed() == 0 || timestamp < cutoff_ms {
                continue;
            }
            let key = payload
                .message_id
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| format!("ts:{timestamp}"));
            let entry = legacy
                .entry(key)
                .or_insert((timestamp, TokenVector::default()));
            entry.1.component_max(&tokens);
        }
        records.record_skipped(&mut diagnostics, track_skipped_lines);

        events.extend(legacy.into_iter().map(|(message_id, (timestamp, tokens))| {
            UsageEvent::new(
                self.id(),
                format!("{session_id}:{message_id}"),
                timestamp,
                session_id.clone(),
                None,
                tokens,
                "message_merge",
            )
            .with_project(project.clone())
        }));

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    fn candidate_for(path: &Path) -> SourceCandidate {
        let metadata = path.metadata().unwrap();
        SourceCandidate {
            source_id: "source".into(),
            path: path.to_path_buf(),
            size: metadata.len(),
            mtime_ns: 1,
        }
    }

    fn wire_file(label: &str, relative: &[&str], body: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "metrik-kimi-{label}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let mut directory = root.clone();
        for part in relative {
            directory = directory.join(part);
        }
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("wire.jsonl");
        let mut file = File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn new_format_counts_turn_scope_only_and_never_session_totals() {
        // session 作用域是会话累计总量，计入会重复计数。
        let path = wire_file(
            "turn-scope",
            &["sessions", "ws-1", "session-a", "agents", "main"],
            concat!(
                r#"{"type":"usage.record","model":"kimi-code/kimi-for-coding","usage":{"inputOther":3064,"output":76,"inputCacheRead":14848,"inputCacheCreation":0},"usageScope":"turn","time":1782113184943}"#,
                "\n",
                r#"{"type":"usage.record","model":"kimi-code/kimi-for-coding","usage":{"inputOther":120,"output":40,"inputCacheRead":0,"inputCacheCreation":512},"usageScope":"turn","time":1782113200000}"#,
                "\n",
                r#"{"type":"usage.record","model":"kimi-code/kimi-for-coding","usage":{"inputOther":3184,"output":116,"inputCacheRead":14848,"inputCacheCreation":512},"usageScope":"session","time":1782113200001}"#,
                "\n",
            ),
        );

        let parsed = KimiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 2);
        let total: i64 = parsed
            .source
            .events
            .iter()
            .map(|event| event.tokens.processed())
            .sum();
        // 只有两条 turn：(3064+14848+0+76) + (120+0+512+40) = 18660
        assert_eq!(total, 18_660);
        assert_eq!(
            parsed.source.events[0].model.as_deref(),
            Some("kimi-code/kimi-for-coding")
        );
        // 会话 ID 取自路径（含子 agent 粒度）。
        assert_eq!(parsed.source.events[0].session_id, "session-a/main");
        assert_eq!(parsed.source.events[0].tokens.cache_read, 14_848);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn step_end_times_the_request_it_reports_usage_for() {
        let path = wire_file(
            "step-timing",
            &["sessions", "ws-1", "session-t", "agents", "main"],
            concat!(
                r#"{"type":"usage.record","model":"kimi-code/k3","usage":{"inputOther":100,"output":20,"inputCacheRead":0,"inputCacheCreation":0},"usageScope":"turn","time":1782113184943}"#,
                "\n",
                r#"{"type":"context.append_loop_event","event":{"type":"step.end","usage":{"inputOther":100,"output":20,"inputCacheRead":0,"inputCacheCreation":0},"llmFirstTokenLatencyMs":900,"llmStreamDurationMs":2100},"time":1782113184950}"#,
                "\n",
                r#"{"type":"usage.record","model":"kimi-code/k3","usage":{"inputOther":50,"output":10,"inputCacheRead":0,"inputCacheCreation":0},"usageScope":"turn","time":1782113190000}"#,
                "\n",
                r#"{"type":"context.append_loop_event","event":{"type":"tool.call"},"time":1782113190001}"#,
                "\n",
                r#"{"type":"context.append_loop_event","event":{"type":"step.end","usage":{"inputOther":51,"output":10,"inputCacheRead":0,"inputCacheCreation":0},"llmFirstTokenLatencyMs":900,"llmStreamDurationMs":2100},"time":1782113190002}"#,
                "\n",
                r#"{"type":"usage.record","model":"kimi-code/k3","usage":{"inputOther":7,"output":3,"inputCacheRead":0,"inputCacheCreation":0},"usageScope":"turn","time":1782113200000}"#,
                "\n",
                r#"{"type":"context.append_loop_event","event":{"type":"step.end","finishReason":"error"},"time":1782113200001}"#,
                "\n",
            ),
        );

        let parsed = KimiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        // 用量对不上的 step.end 和没有耗时的 step.end 都不配对；用量照常入账。
        let durations: Vec<_> = parsed
            .source
            .events
            .iter()
            .map(|event| event.response_duration_ms)
            .collect();
        assert_eq!(durations, [Some(3000), None, None]);
        assert_eq!(parsed.source.events.len(), 3);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// Kimi Work（kimi-desktop）的 wire.jsonl 与 CLI 同构（真机核实），只是
    /// home 在 daimon 运行时下；项目归属走 session_index.jsonl 的 sessionId 映射。
    #[test]
    fn kimi_work_sessions_parse_and_map_the_work_dir() {
        let path = wire_file(
            "kimi-work",
            &[
                "sessions",
                "wd_aidraw_4befc391f392",
                "conv-1f6e",
                "agents",
                "main",
            ],
            concat!(
                r#"{"type":"usage.record","model":"k2d6-agent","usage":{"inputOther":12445,"output":46,"inputCacheRead":13312,"inputCacheCreation":0},"usageScope":"turn","time":1784991559431}"#,
                "\n",
            ),
        );
        let sessions_dir = path
            .ancestors()
            .find(|dir| dir.file_name().and_then(|name| name.to_str()) == Some("sessions"))
            .unwrap();
        let home = sessions_dir.parent().unwrap().to_path_buf();
        // 没有索引文件时不归属（与 CLI 同一条纪律：不拿工作区 ID 冒充路径）。
        let unmapped = KimiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();
        assert_eq!(unmapped.source.events[0].project_path, None);

        std::fs::write(
            home.join("session_index.jsonl"),
            concat!(
                r#"{"sessionId":"conv-other","sessionDir":"…","workDir":"D:/other"}"#,
                "\n",
                r#"{"sessionId":"conv-1f6e","sessionDir":"…","workDir":"D:\\work\\AIdraw"}"#,
                "\n",
            ),
        )
        .unwrap();
        let parsed = KimiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 1);
        // 会话 ID 与 CLI 新版同构：<session>/<agent>。
        assert_eq!(parsed.source.events[0].session_id, "conv-1f6e/main");
        assert_eq!(parsed.source.events[0].tokens.processed(), 25_803);
        assert_eq!(
            parsed.source.events[0].project_path.as_deref(),
            Some("D:/work/AIdraw")
        );
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn detected_roots_cover_cli_and_kimi_work() {
        let home = dirs::home_dir().unwrap_or_default();
        let config = dirs::config_dir().unwrap_or_default();
        let roots = KimiAdapter::detected().roots;
        assert_eq!(roots[0], home.join(".kimi-code").join("sessions"));
        assert_eq!(roots[1], home.join(".kimi").join("sessions"));
        assert_eq!(
            roots[2],
            config
                .join("kimi-desktop")
                .join("daimon-share")
                .join("daimon")
                .join("runtime")
                .join("kimi-code")
                .join("home")
                .join("sessions")
        );
    }

    #[test]
    fn workspace_id_resolves_to_its_real_root_and_only_through_the_mapping_file() {
        let path = wire_file(
            "workspace",
            &["sessions", "wd_usage_abc", "session-a", "agents", "main"],
            concat!(
                r#"{"type":"usage.record","model":"kimi-code/kimi-for-coding","usage":{"inputOther":100,"output":10,"inputCacheRead":0,"inputCacheCreation":0},"usageScope":"turn","time":1782113184943}"#,
                "\n",
            ),
        );
        let sessions_dir = path
            .ancestors()
            .find(|dir| dir.file_name().and_then(|name| name.to_str()) == Some("sessions"))
            .unwrap();
        let data_dir = sessions_dir.parent().unwrap().to_path_buf();

        // 没有 workspaces.json 时不拿工作区 ID 冒充路径。
        let unmapped = KimiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();
        assert_eq!(unmapped.source.events[0].project_path, None);

        std::fs::write(
            data_dir.join("workspaces.json"),
            r#"{"version":1,"workspaces":{"wd_usage_abc":{"root":"D:/work/usage","name":"usage"}}}"#,
        )
        .unwrap();
        let mapped = KimiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        assert_eq!(
            mapped.source.events[0].project_path.as_deref(),
            Some("D:/work/usage")
        );
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn legacy_status_updates_merge_by_message_id_taking_component_maxima() {
        // 同一 message_id 若被渐进更新，取分量最大值即为该消息的最终用量，
        // 不会把中间态叠加成重复计数；真增量时每个 id 只出现一次，取 max 无害。
        let path = wire_file(
            "legacy",
            &["sessions", "group-1", "session-b"],
            concat!(
                r#"{"type":"metadata","protocol_version":"1.3"}"#,
                "\n",
                r#"{"timestamp":1770983426.420942,"message":{"type":"StatusUpdate","payload":{"token_usage":{"input_other":1000,"output":500,"input_cache_read":0,"input_cache_creation":0},"message_id":"chatcmpl-a"}}}"#,
                "\n",
                r#"{"timestamp":1770983427.100000,"message":{"type":"StatusUpdate","payload":{"token_usage":{"input_other":1562,"output":2463,"input_cache_read":0,"input_cache_creation":0},"message_id":"chatcmpl-a"}}}"#,
                "\n",
                r#"{"timestamp":1770983500.000000,"message":{"type":"StatusUpdate","payload":{"token_usage":{"input_other":10,"output":20,"input_cache_read":30,"input_cache_creation":40},"message_id":"chatcmpl-b"}}}"#,
                "\n",
            ),
        );

        let parsed = KimiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 2);
        let total: i64 = parsed
            .source
            .events
            .iter()
            .map(|event| event.tokens.processed())
            .sum();
        // chatcmpl-a 取最大值 1562+2463 = 4025（不是 1500+4025），chatcmpl-b = 100
        assert_eq!(total, 4_125);
        // 旧版 StatusUpdate 不带模型名：诚实留空，不猜。
        assert!(parsed
            .source
            .events
            .iter()
            .all(|event| event.model.is_none()));
        // 时间戳是 Unix 秒（浮点）→ 毫秒。
        assert_eq!(parsed.source.events[0].occurred_at_ms, 1_770_983_426_420);
        assert_eq!(parsed.source.events[0].session_id, "session-b");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn malformed_lines_downgrade_the_scan_without_losing_valid_events() {
        let path = wire_file(
            "diagnostics",
            &["sessions", "ws-1", "session-c", "agents", "main"],
            concat!(
                r#"{"type":"usage.record","usage":{"inputOther":100,"output":10},"usageScope":"turn","time":1782113184943}"#,
                "\n",
                "not-json\n",
            ),
        );

        let parsed = KimiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].tokens.processed(), 110);
        assert_eq!(parsed.diagnostics.malformed_lines, 1);
        assert!(parsed.diagnostics.is_partial());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn interrupted_turn_with_string_message_does_not_mark_scan_partial() {
        let path = wire_file(
            "interrupted",
            &["sessions", "ws-1", "session-int", "agents", "main"],
            concat!(
                r#"{"type":"usage.record","model":"kimi-code/k3","usage":{"inputOther":200,"output":50,"inputCacheRead":0,"inputCacheCreation":0},"usageScope":"turn","time":1782113184943}"#,
                "\n",
                r#"{"type":"turn.step.interrupted","message":"User interrupted the step","time":1782113185000}"#,
                "\n",
            ),
        );

        let parsed = KimiAdapter::with_roots(vec![])
            .parse(&candidate_for(&path), i64::MIN)
            .unwrap();

        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].tokens.processed(), 250);
        assert!(!parsed.diagnostics.is_partial(), "{:?}", parsed.diagnostics);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn project_mapping_retries_after_unparseable_snapshot_and_keeps_stale_on_error() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let wire = root
            .join("sessions")
            .join("ws-1")
            .join("sess-1")
            .join("wire.jsonl");
        let wire_idx = root
            .join("sessions")
            .join("ws-unknown")
            .join("sess-2")
            .join("wire.jsonl");
        std::fs::create_dir_all(wire.parent().unwrap()).unwrap();
        std::fs::create_dir_all(wire_idx.parent().unwrap()).unwrap();
        std::fs::write(&wire, "").unwrap();
        std::fs::write(&wire_idx, "").unwrap();

        let workspaces_path = root.join("workspaces.json");
        let valid = r#"{"workspaces":{"ws-1":{"root":"D:/work/metrik"}}}"#;
        // 构造与 valid 字节长度完全一致的损坏内容，并钉死相同的 mtime，模拟锁冲突/半写恢复后 stamp 不变。
        let broken = "{".to_owned() + &" ".repeat(valid.len() - 1);
        assert_eq!(broken.len(), valid.len());

        let fixed_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_780_000_000);
        std::fs::write(&workspaces_path, &broken).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&workspaces_path)
            .unwrap()
            .set_modified(fixed_time)
            .unwrap();

        let adapter = KimiAdapter::with_roots(vec![root.clone()]);
        assert_eq!(adapter.project_root_from_path(&wire), None);

        std::fs::write(&workspaces_path, valid).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&workspaces_path)
            .unwrap()
            .set_modified(fixed_time)
            .unwrap();

        assert_eq!(
            adapter.project_root_from_path(&wire).as_deref(),
            Some("D:/work/metrik")
        );

        // 非原子重写截断 workspaces.json 时保留已缓存映射（stale-on-error），当次不掉回未归属。
        let newer_time = fixed_time + std::time::Duration::from_secs(5);
        std::fs::write(&workspaces_path, "{").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&workspaces_path)
            .unwrap()
            .set_modified(newer_time)
            .unwrap();
        assert_eq!(
            adapter.project_root_from_path(&wire).as_deref(),
            Some("D:/work/metrik")
        );

        // session_index.jsonl：中间已封尾的历史坏行不阻塞 stamp 缓存；末尾无换行的半写行则自动重试。
        let index_path = root.join("session_index.jsonl");
        std::fs::write(
            &index_path,
            "not-json-history-line\n{\"sessionId\":\"sess-2\",\"workDir\":\"D:/work/idx",
        )
        .unwrap();
        assert_eq!(adapter.project_root_from_path(&wire_idx), None);

        std::fs::write(
            &index_path,
            "not-json-history-line\n{\"sessionId\":\"sess-2\",\"workDir\":\"D:/work/idx\"}\n",
        )
        .unwrap();
        assert_eq!(
            adapter.project_root_from_path(&wire_idx).as_deref(),
            Some("D:/work/idx")
        );
        let cached_stamp = adapter
            .project_cache
            .lock()
            .unwrap()
            .get(&root)
            .and_then(|entry| entry.session_index_stamp);
        assert!(
            cached_stamp.is_some(),
            "historical malformed line must not disable session_index stamp caching"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
