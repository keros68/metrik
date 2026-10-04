use super::{AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{normalize_project_path, ParsedSource, TokenVector, UsageEvent};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

/// DeepSeek Harness（dsh）把每个会话写成一个 append-only 的事件流
/// `~/.dsh/sessions/<encoded-cwd>/<session-id>/session.v4.jsonl.zstd`
/// （`DSH_HOME` 可改根目录；`compression: none` 时是同名 .jsonl 明文）。
/// 解析逻辑移植自 tokscale 的 `sessions/dsh.rs`（MIT），按 Metrik 的
/// UsageEvent/TokenVector 口径做了三处对齐：
/// - `inputTokens` 是不含缓存的新输入，缓存读/写单列——与 Metrik 一致，直取；
/// - `reasoningTokens` 是 `outputTokens` 的**子集**（completion_tokens_details），
///   按 minimax 的先例保留完整 output、reasoning 单列，`processed()` 不含它，
///   与 DSH 自己的 token meter（input+cache+output）相等；
/// - 来源不自带总量字段（只有 totalTokens），用它做口径自检。
pub struct DshAdapter {
    home: PathBuf,
}

impl DshAdapter {
    pub fn detected() -> Self {
        Self { home: dsh_home() }
    }

    #[cfg(test)]
    fn with_home(home: PathBuf) -> Self {
        Self { home }
    }
}

/// DSH 数据根：`DSH_HOME` 优先，否则 `~/.dsh`（与 tokscale 的约定一致）。
pub fn dsh_home() -> PathBuf {
    std::env::var("DSH_HOME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".dsh"))
}

/// Zstandard 帧魔数（RFC 8478 §3.1.1）。DSH 逐帧追加写入，按魔数而不是文件名
/// 判断编码；`compression: none` 的明文文件不带魔数，直接按 UTF-8 行解析。
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// 单个转录的读取/解码上限。真实转录是个位数 MiB；撞上限的按坏文件跳过。
const MAX_TRANSCRIPT_BYTES: usize = 64 * 1024 * 1024;
/// 解码时的流式读块大小。
const ZSTD_CHUNK_BYTES: usize = 128 * 1024;

/// 读取一个转录文件；zstd 帧流式解码，尾部撕裂帧保留可解码前缀（活跃会话
/// 正在被 DSH 追加写入时，扫描必然撞上不完整的尾帧，整文件丢弃等于漏计）。
fn read_transcript(path: &Path) -> Vec<u8> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut raw = Vec::new();
    if file
        .take(MAX_TRANSCRIPT_BYTES as u64 + 1)
        .read_to_end(&mut raw)
        .is_err()
    {
        return Vec::new();
    }
    if raw.len() > MAX_TRANSCRIPT_BYTES {
        return Vec::new();
    }
    if raw.len() < ZSTD_MAGIC.len() || raw[..ZSTD_MAGIC.len()] != ZSTD_MAGIC {
        return raw;
    }
    let Ok(mut decoder) = zstd::stream::read::Decoder::new(raw.as_slice()) else {
        return Vec::new();
    };
    let mut decoded = Vec::new();
    let mut chunk = vec![0u8; ZSTD_CHUNK_BYTES];
    loop {
        let want = chunk.len().min(MAX_TRANSCRIPT_BYTES - decoded.len() + 1);
        match decoder.read(&mut chunk[..want]) {
            Ok(0) => break,
            Ok(read) if decoded.len() + read <= MAX_TRANSCRIPT_BYTES => {
                decoded.extend_from_slice(&chunk[..read]);
            }
            // 撞上限或解码错误：保留已解码前缀（撕裂帧场景即完整帧们）。
            _ => break,
        }
    }
    decoded
}

/// usage 字段：`assistant/message` 与 `compaction/summary` 在 `data.usage`；
/// `assistant/attempt` 与带流式记录的消息取 Assistant 流里**最后一个** usage
/// chunk（前面的都是中间快照，不能相加）。
fn usage_for_event<'a>(value: &'a Value, event_type: &str) -> Option<&'a Value> {
    if event_type == "assistant/message" {
        return value
            .pointer("/data/usage")
            .or_else(|| last_stream_usage(value));
    }
    if event_type == "assistant/attempt" {
        return last_stream_usage(value);
    }
    if event_type == "compaction/summary" {
        return value.pointer("/data/usage");
    }
    None
}

fn last_stream_usage(value: &Value) -> Option<&Value> {
    value
        .pointer("/data/stream")?
        .as_array()?
        .iter()
        .rev()
        .find_map(|record| {
            let chunk = record.get("chunk")?;
            (record.get("type")?.as_str()? == "chunk" && chunk.get("type")?.as_str()? == "usage")
                .then(|| chunk.get("usage"))
                .flatten()
        })
}

/// 实际服务模型：优先 `replayState.response.responseModel`（provider 侧换模
/// 时由 pi-ai 记录），否则 `source.model`。
fn served_model<'a>(source: Option<&'a Value>) -> Option<&'a str> {
    source
        .and_then(|value| value.pointer("/replayState/response/responseModel"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            source
                .and_then(|value| value.get("model"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        })
}

fn int_field(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn is_transcript(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    // session.jsonl / session.v4.jsonl / 两者加 .zstd 后缀。
    let stripped = name.strip_suffix(".zstd").unwrap_or(name);
    let Some(rest) = stripped.strip_prefix("session") else {
        return false;
    };
    rest.is_empty() || rest.starts_with(".v")
}

impl AgentAdapter for DshAdapter {
    fn id(&self) -> &'static str {
        "dsh"
    }

    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate> {
        let root = self.home.join("sessions");
        let mut found = Vec::new();
        for entry in walkdir::WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.into_path();
            if !is_transcript(&path) {
                continue;
            }
            let Ok(metadata) = path.metadata() else {
                continue;
            };
            let mtime_ns = super::file_mtime_ns(&metadata);
            if mtime_ns / 1_000_000 < cutoff_ms {
                continue;
            }
            let normalized = super::normalize_locator(&path);
            found.push(SourceCandidate {
                source_id: crate::domain::stable_hash(&format!("{}|{normalized}", self.id())),
                path,
                size: metadata.len(),
                mtime_ns,
            });
        }
        found.sort_by(|left, right| left.path.cmp(&right.path));
        found
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        let decoded = read_transcript(&candidate.path);
        let mut events = Vec::new();
        let mut diagnostics = ScanDiagnostics::default();
        if decoded.is_empty() {
            return Ok(ParsedScan {
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
            });
        }

        // 转录目录名即会话 id 的兜底（首行 session 事件缺失时）。
        let session_id_from_path = candidate
            .path
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("unknown")
            .to_string();

        let mut session_id: Option<String> = None;
        let mut workspace: Option<String> = None;
        // fork 边界：前 seedLength 条事件是从父会话原样继承的，重记会把父
        // 会话的调用再计一遍。
        let mut seed_length: i64 = 0;
        let mut fallback_provider: Option<String> = None;
        let mut fallback_model: Option<String> = None;
        let mut seen: HashSet<String> = HashSet::new();
        // DSH 对同一 (turn, step) 的重复结算用后样本替换前样本；重试由
        // llm/retry-started 打开新槽位。
        let mut last_settlement: Option<(i64, i64, usize)> = None;

        for line in decoded.split(|byte| *byte == b'\n') {
            let line_owned = String::from_utf8_lossy(line);
            let line = line_owned.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                diagnostics.malformed_lines += 1;
                continue;
            };
            let Some(event_type) = value.get("type").and_then(Value::as_str) else {
                continue;
            };
            match event_type {
                "session" => {
                    session_id = value.get("id").and_then(Value::as_str).map(str::to_string);
                    workspace = value.get("cwd").and_then(Value::as_str).map(str::to_string);
                    seed_length = value
                        .get("seedLength")
                        .and_then(Value::as_i64)
                        .filter(|length| *length > 0)
                        .unwrap_or(0);
                }
                "request/header" => {
                    let config = value.pointer("/data/header/config");
                    fallback_provider = config
                        .and_then(|config| config.get("provider"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    fallback_model = config
                        .and_then(|config| config.get("model"))
                        .and_then(Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                        .map(str::to_string);
                }
                "llm/retry-started" => {
                    let retry_turn = value.pointer("/data/turn").and_then(Value::as_i64);
                    let retry_step = value.pointer("/data/step").and_then(Value::as_i64);
                    if last_settlement.is_some_and(|(turn, step, _)| {
                        Some(turn) == retry_turn && Some(step) == retry_step
                    }) {
                        last_settlement = None;
                    }
                }
                "assistant/message" | "assistant/attempt" | "compaction/summary" => {
                    let is_summary = event_type == "compaction/summary";
                    if seed_length > 0
                        && value
                            .get("seq")
                            .and_then(Value::as_i64)
                            .is_some_and(|seq| seq < seed_length)
                    {
                        continue;
                    }
                    let Some(usage) = usage_for_event(&value, event_type) else {
                        continue;
                    };
                    let reasoning = int_field(usage, "reasoningTokens").max(0);
                    let tokens = TokenVector {
                        input_uncached: int_field(usage, "inputTokens"),
                        cache_read: int_field(usage, "cacheReadTokens"),
                        cache_write: int_field(usage, "cacheWriteTokens"),
                        output: int_field(usage, "outputTokens").max(0),
                        reasoning_output: reasoning,
                    };
                    if tokens.input_uncached < 0 {
                        tokens_neg_diagnostics(&mut diagnostics);
                    }
                    // 来源自报的总量（input+cache+output，reasoning 已含在
                    // output 里）用于口径自检。
                    let reported_total = int_field(usage, "totalTokens");
                    if tokens.processed() != reported_total && reported_total > 0 {
                        diagnostics.total_mismatches += 1;
                    }
                    if tokens.processed() == 0 {
                        continue;
                    }
                    let Some(timestamp) = value.get("time").and_then(Value::as_i64) else {
                        continue;
                    };
                    if timestamp <= 0 {
                        continue;
                    }

                    let source = value.pointer("/data/message/source");
                    let model = served_model(source)
                        .or(fallback_model.as_deref())
                        .unwrap_or("unknown")
                        .to_string();
                    let provider = source
                        .and_then(|source| source.get("provider"))
                        .and_then(Value::as_str)
                        .or(fallback_provider.as_deref())
                        .unwrap_or("unknown")
                        .to_string();
                    let sid = session_id
                        .clone()
                        .unwrap_or_else(|| session_id_from_path.clone());

                    // 事件身份：compaction/summary 用 compactionId，普通消息用
                    // message.id，缺失时退回 seq；再加时间戳与路由/用量，区分
                    // 复制场景下"同 id 不同调用"的行。
                    let identity = if is_summary {
                        value
                            .pointer("/data/compactionId")
                            .and_then(Value::as_str)
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| format!("cmp:{value}"))
                            .or_else(|| {
                                value
                                    .get("seq")
                                    .and_then(Value::as_i64)
                                    .map(|seq| format!("seq:{seq}"))
                            })
                            .unwrap_or_else(|| format!("sid:{sid}"))
                    } else {
                        value
                            .pointer("/data/message/id")
                            .and_then(Value::as_str)
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| format!("msg:{value}"))
                            .or_else(|| {
                                value
                                    .get("seq")
                                    .and_then(Value::as_i64)
                                    .map(|seq| format!("seq:{seq}"))
                            })
                            .unwrap_or_else(|| format!("sid:{sid}"))
                    };
                    let kind = if is_summary { "summary:" } else { "" };
                    let event_key = format!(
                        "dsh:{kind}{identity}:{timestamp}:{provider}:{model}:{}:{}:{}:{}:{}",
                        tokens.input_uncached,
                        tokens.output,
                        tokens.cache_read,
                        tokens.cache_write,
                        tokens.reasoning_output
                    );
                    if !seen.insert(event_key.clone()) {
                        continue;
                    }
                    if timestamp < cutoff_ms {
                        // 已在保留期之外：不计入诊断，静默跳过。
                        continue;
                    }

                    let event = UsageEvent::new(
                        self.id(),
                        event_key,
                        timestamp,
                        sid,
                        Some(model),
                        tokens,
                        "exact",
                    )
                    .with_project(workspace.clone().and_then(|value| normalize_project_path(&value)));

                    let step = value
                        .pointer("/data/turn")
                        .and_then(Value::as_i64)
                        .zip(value.pointer("/data/step").and_then(Value::as_i64));
                    if is_summary {
                        events.push(event);
                    } else if let Some((turn, step)) = step {
                        if let Some((last_turn, last_step, index)) = last_settlement {
                            if last_turn == turn && last_step == step {
                                // 同一尝试槽位的更新样本：替换而不是追加。
                                events[index] = event;
                                continue;
                            }
                        }
                        events.push(event);
                        last_settlement = Some((turn, step, events.len() - 1));
                    } else {
                        events.push(event);
                        last_settlement = None;
                    }
                }
                _ => {}
            }
        }

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

fn tokens_neg_diagnostics(diagnostics: &mut ScanDiagnostics) {
    diagnostics.rejected_events += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zstd_compress(payload: &str) -> Vec<u8> {
        zstd::encode_all(payload.as_bytes(), 3).unwrap()
    }

    fn write_session(dir: &Path, session: &str, payload: &str) -> PathBuf {
        let session_dir = dir.join("sessions").join(session);
        std::fs::create_dir_all(&session_dir).unwrap();
        let path = session_dir.join("session.v4.jsonl.zstd");
        std::fs::write(&path, zstd_compress(payload)).unwrap();
        path
    }

    fn adapter_for(dir: &Path) -> DshAdapter {
        DshAdapter::with_home(dir.to_path_buf())
    }

    #[test]
    fn parses_zstd_assistant_usage() {
        let dir = std::env::temp_dir().join(format!("metrik-dsh-basic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rows = [
            r#"{"type":"session","id":"session-abc","createdAt":1,"cwd":"D:\\work\\usage"}"#,
            r#"{"type":"assistant/message","seq":1,"time":1786669454772,"data":{"turn":1,"step":1,"message":{"id":"m-1","source":{"provider":"deepseek-account","model":"deepseek-flash"}},"usage":{"inputTokens":130,"outputTokens":159,"cacheReadTokens":13824,"totalTokens":14113}}}"#,
        ];
        write_session(&dir, "session-abc", &rows.join("\n"));
        let adapter = adapter_for(&dir);

        let candidates = adapter.discover(0);
        assert_eq!(candidates.len(), 1);
        let scan = adapter.parse(&candidates[0], 0).unwrap();

        assert_eq!(scan.source.events.len(), 1);
        let event = &scan.source.events[0];
        assert_eq!(event.event_key, "dsh:msg:m-1:1786669454772:deepseek-account:deepseek-flash:130:159:13824:0:0");
        assert_eq!(event.session_id, "session-abc");
        assert_eq!(event.model.as_deref(), Some("deepseek-flash"));
        assert_eq!(event.tokens.input_uncached, 130);
        assert_eq!(event.tokens.output, 159);
        assert_eq!(event.tokens.cache_read, 13824);
        assert_eq!(event.tokens.processed(), 14113);
        assert_eq!(event.project_path.as_deref(), Some("D:/work/usage"));
        assert!(!scan.diagnostics.is_partial());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skips_seeded_prefix_and_replaces_settlements() {
        let dir = std::env::temp_dir().join(format!("metrik-dsh-fork-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rows = [
            r#"{"type":"session","id":"child","createdAt":1,"cwd":"/work","seedLength":2}"#,
            r#"{"type":"assistant/message","seq":1,"time":100,"data":{"turn":1,"step":1,"message":{"id":"parent-msg","source":{"provider":"p","model":"m"}},"usage":{"inputTokens":900,"outputTokens":100,"totalTokens":1000}}}"#,
            r#"{"type":"assistant/message","seq":2,"time":200,"data":{"turn":1,"step":1,"message":{"id":"own-1","source":{"provider":"p","model":"m"}},"usage":{"inputTokens":10,"outputTokens":20,"totalTokens":30}}}"#,
            r#"{"type":"assistant/message","seq":3,"time":300,"data":{"turn":1,"step":1,"message":{"id":"own-2","source":{"provider":"p","model":"m"}},"usage":{"inputTokens":11,"outputTokens":21,"totalTokens":32}}}"#,
        ];
        write_session(&dir, "child", &rows.join("\n"));
        let adapter = adapter_for(&dir);
        let scan = adapter.parse(&adapter.discover(0)[0], 0).unwrap();

        // seq=1 在种子边界内被跳过；seq=2/3 同 (turn, step)，后者替换前者。
        assert_eq!(scan.source.events.len(), 1);
        assert_eq!(scan.source.events[0].tokens.input_uncached, 11);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn counts_compaction_summary() {
        let dir = std::env::temp_dir().join(format!("metrik-dsh-cmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rows = [
            r#"{"type":"session","id":"s","createdAt":1,"cwd":"/work"}"#,
            r#"{"type":"assistant/message","seq":1,"time":100,"data":{"turn":1,"step":1,"message":{"id":"m","source":{"provider":"p","model":"m"}},"usage":{"inputTokens":10,"outputTokens":20,"totalTokens":30}}}"#,
            r#"{"type":"compaction/summary","seq":2,"time":200,"data":{"compactionId":"c1","message":{"source":{"provider":"p","model":"m"}},"usage":{"inputTokens":500,"outputTokens":200,"cacheReadTokens":4000,"totalTokens":4700}}}"#,
        ];
        write_session(&dir, "s", &rows.join("\n"));
        let adapter = adapter_for(&dir);
        let scan = adapter.parse(&adapter.discover(0)[0], 0).unwrap();

        assert_eq!(scan.source.events.len(), 2);
        let summary = &scan.source.events[1];
        assert!(summary.event_key.starts_with("dsh:summary:cmp:c1:"));
        assert_eq!(summary.tokens.cache_read, 4000);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn torn_trailing_frame_still_counts_committed_prefix() {
        let dir = std::env::temp_dir().join(format!("metrik-dsh-torn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let header = zstd_compress(
            r#"{"type":"session","id":"s-torn","createdAt":1,"cwd":"/work"}
"#,
        );
        let committed = zstd_compress(
            r#"{"type":"assistant/message","time":100,"data":{"turn":1,"step":1,"message":{"id":"m","source":{"provider":"p","model":"m"}},"usage":{"inputTokens":10,"outputTokens":20,"totalTokens":30}}}
"#,
        );
        let torn = zstd_compress(
            r#"{"type":"assistant/message","time":200,"data":{"turn":2,"step":1,"message":{"id":"m2","source":{"provider":"p","model":"m"}},"usage":{"inputTokens":1,"outputTokens":2,"totalTokens":3}}}
"#,
        );
        let mut payload = header;
        payload.extend_from_slice(&committed);
        payload.extend_from_slice(&torn[..torn.len() / 2]);
        let path = dir.join("sessions").join("s-torn").join("session.v4.jsonl.zstd");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, payload).unwrap();

        let adapter = adapter_for(&dir);
        let scan = adapter.parse(&adapter.discover(0)[0], 0).unwrap();
        assert_eq!(scan.source.events.len(), 1);
        assert_eq!(scan.source.events[0].tokens.input_uncached, 10);
        std::fs::remove_dir_all(&dir).ok();
    }
}
