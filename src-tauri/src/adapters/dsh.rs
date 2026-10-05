//! DeepSeek Harness（dsh，deepseek-ai/deepseek-harness）的会话日志。
//!
//! 布局：`<DSH_HOME 或 ~/.dsh>/sessions/--<编码 cwd>--/<会话 id>/` 下每个格式代际
//! 一个文件：v0 是 `session.jsonl`，之后是 `session.vN.jsonl`；默认 zstd 压缩，
//! 再加 `.zstd` 后缀（`compression: none` 时为明文）。格式迁移会把新代际写在
//! 旧文件旁边且旧文件保留，所以每个会话目录只读版本号最大的那一份，否则整段
//! 会话计两次。子代理是独立会话，有自己的目录，不需要从父会话里取。
//!
//! 文件是逐事件追加的 JSONL：首行 `session` 头（`id`、`cwd`、`isSeeded`），之后
//! 每行 `{type, seq, time(毫秒), data}`；zstd 时首帧只含头，之后每批追加一帧。
//! 扫描撞上正在写的会话时尾帧可能不完整，按 DSH 自己的读法保留可解出的前缀。
//!
//! 计量口径（按 DSH 源码 `llm/llm/src/types.ts` 的 `TokenUsage` 与
//! `token-meter` 的投影规则，2026-10 核对）：
//! - 计费调用有三种：`assistant/message`、`assistant/attempt`（没有提交消息的
//!   失败/重试/取消调用，是另一次计费，不是消息的重复）与 `compaction/summary`
//!   （压缩时的摘要调用，DSH 自己的计量表不计，但它是真实花费，与 Pi 的
//!   compaction 用量同样入账）。用量取 `data.usage`，没有时取流里最后一个
//!   usage chunk（之前的都是中间快照，不能相加）。
//! - 分量互不重叠：`inputTokens` 不含缓存，缓存读/写单列；`reasoningTokens`
//!   是 `outputTokens` 的子集，只作明细，`processed()` 不重复相加；
//!   `totalTokens` 是整次调用总量，逐条做口径自检。
//! - 同一 (turn, step) 的后一个结算样本替换前一个；`llm/retry-started` 关闭
//!   该槽位，重试另记一次。
//! - fork 会把父会话的前缀原样复制进子会话（`message.id`、`compactionId`、
//!   `seq` 都不变）。继承边界是最后一个 `session/end-seed {inherited:true}`
//!   的 seq，之前的事件属于父会话；v0/v1 的头里另有 `seedLength`。
//!
//! 身份：`message.id` 与 `compactionId` 是每次调用的 UUID，跨文件稳定，复制
//! 进别的会话仍是同一事件，账本按分量最大值合并（与 Pi 相同）；attempt 没有
//! 自己的 id，用会话 id + seq。
//!
//! dsh 与 Pi、Hermes 一样是 harness：走 coding plan 的用量记到对应计量卡片，
//! DeepSeek 官方 API 与账户余额等按量计费的路由留在 dsh 卡片。

use super::{AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{stable_hash, ParsedSource, TokenVector, UsageEvent};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

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

/// DSH 数据根：非空的 `DSH_HOME` 优先，否则 `~/.dsh`（与 DSH 的 home-paths 一致）。
pub fn dsh_home() -> PathBuf {
    std::env::var_os("DSH_HOME")
        .filter(|value| !value.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".dsh"))
}

/// 把 dsh 的 provider 路由映射到计量 Agent。provider id 来自 pi-ai 目录（与 Pi
/// 同一份），套餐路由的归属沿用 `pi_providers`；`deepseek-official`、
/// `deepseek-account`（DeepSeek 平台余额）等按量计费的路由留在 dsh。
fn credited_agent(provider: Option<&str>) -> &'static str {
    match crate::pi_providers::credited_agent(provider) {
        "pi" => "dsh",
        card => card,
    }
}

/// zstd 帧魔数（RFC 8478 §3.1.1）：按内容判断编码，不按文件名。
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
/// 单个会话文件读盘与解压后的上限。真实会话是个位数 MiB；超限的整份跳过并
/// 标为数据不完整。zstd 的压缩比没有上限，所以解压上限要在解码循环里卡住。
const MAX_TRANSCRIPT_BYTES: usize = 64 * 1024 * 1024;
const ZSTD_CHUNK_BYTES: usize = 128 * 1024;

/// 读一个会话文件。`None` 表示超过上限。解码出错（尾帧不完整）时保留已解出
/// 的前缀；前缀里被截断的最后一行由调用方按"未写完"处理。
fn read_transcript(path: &Path, limit: usize) -> std::io::Result<Option<Vec<u8>>> {
    let mut raw = Vec::new();
    std::fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > limit {
        return Ok(None);
    }
    if !raw.starts_with(&ZSTD_MAGIC) {
        return Ok(Some(raw));
    }
    let Ok(mut decoder) = zstd::stream::read::Decoder::new(raw.as_slice()) else {
        return Ok(Some(Vec::new()));
    };
    let mut decoded = Vec::new();
    let mut chunk = vec![0u8; ZSTD_CHUNK_BYTES];
    loop {
        // 多要一个字节：恰好填满上限的文件仍要问一次后面还有没有。
        let want = (limit - decoded.len()).saturating_add(1).min(chunk.len());
        match decoder.read(&mut chunk[..want]) {
            Ok(0) | Err(_) => break,
            Ok(read) if decoded.len() + read > limit => return Ok(None),
            Ok(read) => decoded.extend_from_slice(&chunk[..read]),
        }
    }
    Ok(Some(decoded))
}

/// 会话文件名对应的格式代际：`session.jsonl` 是 0，`session.vN.jsonl` 是 N；
/// 两者都可带 `.zstd`。临时文件、大写、前导零等非规范名不算。
fn generation(file_name: &str) -> Option<u32> {
    let name = file_name.strip_suffix(".zstd").unwrap_or(file_name);
    let stem = name.strip_prefix("session")?.strip_suffix(".jsonl")?;
    if stem.is_empty() {
        return Some(0);
    }
    let digits = stem.strip_prefix(".v")?;
    if digits.starts_with('0') || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|version| *version > 0)
}

fn non_empty(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn int_field(usage: &Value, key: &str) -> i64 {
    usage.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// 流里最后一个 usage chunk（`{type:"chunk", chunk:{type:"usage", usage}}`）。
fn last_stream_usage(data: &Value) -> Option<&Value> {
    data.get("stream")?
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

struct Pending {
    seq: Option<i64>,
    event_key: String,
    timestamp: i64,
    credited_agent: &'static str,
    model: Option<String>,
    tokens: TokenVector,
}

#[derive(Default)]
struct Ledger {
    events: Vec<Option<Pending>>,
    by_key: HashMap<String, usize>,
}

impl Ledger {
    /// 同一身份在文件里再次出现时按分量最大值合并，与账本跨文件的规则一致。
    fn insert(&mut self, pending: Pending) -> usize {
        if let Some(&index) = self.by_key.get(&pending.event_key) {
            if let Some(stored) = self.events[index].as_mut() {
                stored.tokens.component_max(&pending.tokens);
                stored.timestamp = stored.timestamp.max(pending.timestamp);
                stored.seq = stored.seq.max(pending.seq);
                if stored.model.is_none() {
                    stored.model = pending.model;
                }
                return index;
            }
        }
        self.by_key
            .insert(pending.event_key.clone(), self.events.len());
        self.events.push(Some(pending));
        self.events.len() - 1
    }

    fn remove(&mut self, index: usize) {
        if let Some(removed) = self.events[index].take() {
            self.by_key.remove(&removed.event_key);
        }
    }
}

impl AgentAdapter for DshAdapter {
    fn id(&self) -> &'static str {
        "dsh"
    }

    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate> {
        // 每个会话目录只取版本号最大的代际；同代际压缩与明文并存时取较新的。
        let mut newest: HashMap<PathBuf, (u32, i64, PathBuf, u64)> = HashMap::new();
        for entry in WalkDir::new(self.home.join("sessions"))
            .max_depth(3)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            let Some(version) = entry.file_name().to_str().and_then(generation) else {
                continue;
            };
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let mtime_ns = super::file_mtime_ns(&metadata);
            let Some(directory) = entry.path().parent().map(Path::to_path_buf) else {
                continue;
            };
            let candidate = (version, mtime_ns, entry.into_path(), metadata.len());
            match newest.get(&directory) {
                Some(current) if (current.0, current.1) >= (candidate.0, candidate.1) => {}
                _ => {
                    newest.insert(directory, candidate);
                }
            }
        }
        let mut found: Vec<SourceCandidate> = newest
            .into_values()
            .filter(|(_, mtime_ns, _, _)| mtime_ns / 1_000_000 >= cutoff_ms)
            .map(|(_, mtime_ns, path, size)| SourceCandidate {
                source_id: stable_hash(&format!("dsh|{}", super::normalize_locator(&path))),
                path,
                size,
                mtime_ns,
            })
            .collect();
        found.sort_by(|left, right| left.path.cmp(&right.path));
        found
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        parse_transcript(candidate, cutoff_ms, MAX_TRANSCRIPT_BYTES)
    }
}

fn parse_transcript(
    candidate: &SourceCandidate,
    cutoff_ms: i64,
    limit: usize,
) -> Result<ParsedScan> {
    let mut diagnostics = ScanDiagnostics::default();
    // 会话目录名即会话 id（URL 编码过），头缺失时兜底。
    let directory_session = candidate
        .path
        .parent()
        .and_then(Path::file_name)
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown-session".into());
    let decoded = match read_transcript(&candidate.path, limit)? {
        Some(decoded) => decoded,
        None => {
            // 超过上限：读不了不等于没用过，标为数据不完整。
            diagnostics.unreadable_lines += 1;
            Vec::new()
        }
    };

    let mut session_id: Option<String> = None;
    let mut project: Option<String> = None;
    let mut seeded = false;
    let mut legacy_seed_length = 0_i64;
    let mut inherited_cut: Option<i64> = None;
    let mut route_provider: Option<String> = None;
    let mut route_model: Option<String> = None;
    let mut ledger = Ledger::default();
    let mut settlement: Option<(i64, i64, usize)> = None;

    let complete = decoded.ends_with(b"\n");
    let lines: Vec<&[u8]> = decoded.split(|byte| *byte == b'\n').collect();
    let last_line = lines.len().saturating_sub(1);
    for (index, line) in lines.into_iter().enumerate() {
        let text = String::from_utf8_lossy(line);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            // 没有换行收尾的最后一行是正在写入的记录，不是坏行。
            if index != last_line || complete {
                diagnostics.malformed_lines += 1;
            }
            continue;
        };
        let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let seq = value.get("seq").and_then(Value::as_i64);
        let data = value.get("data").unwrap_or(&Value::Null);
        match event_type {
            "session" => {
                session_id = non_empty(value.get("id")).map(str::to_owned);
                project = non_empty(value.get("cwd")).map(str::to_owned);
                seeded = value.get("isSeeded").and_then(Value::as_bool) == Some(true);
                legacy_seed_length = value
                    .get("seedLength")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .max(0);
            }
            "session/end-seed" => {
                if data.get("inherited").and_then(Value::as_bool) == Some(true) {
                    inherited_cut = seq.or(inherited_cut);
                }
                // 继承边界两侧不是同一次结算。
                settlement = None;
            }
            "request/header" => {
                let config = data.pointer("/header/config");
                route_provider =
                    non_empty(config.and_then(|c| c.get("provider"))).map(str::to_owned);
                route_model = non_empty(config.and_then(|c| c.get("model"))).map(str::to_owned);
            }
            "llm/retry-started" => {
                let slot = data
                    .get("turn")
                    .and_then(Value::as_i64)
                    .zip(data.get("step").and_then(Value::as_i64));
                if settlement.is_some_and(|(turn, step, _)| Some((turn, step)) == slot) {
                    settlement = None;
                }
            }
            "assistant/message" | "assistant/attempt" | "compaction/summary" => {
                let usage = match event_type {
                    "assistant/message" => data.get("usage").or_else(|| last_stream_usage(data)),
                    "assistant/attempt" => last_stream_usage(data),
                    _ => data.get("usage"),
                };
                let Some(usage) = usage else {
                    continue;
                };
                let tokens = TokenVector {
                    input_uncached: int_field(usage, "inputTokens"),
                    cache_read: int_field(usage, "cacheReadTokens"),
                    cache_write: int_field(usage, "cacheWriteTokens"),
                    output: int_field(usage, "outputTokens"),
                    reasoning_output: int_field(usage, "reasoningTokens"),
                };
                // 负数或 reasoning 超过 output 违反 TokenUsage 的约定（DSH 自己的
                // 计量表也拒收），整条不计。
                if tokens.input_uncached < 0
                    || tokens.cache_read < 0
                    || tokens.cache_write < 0
                    || tokens.output < 0
                    || tokens.reasoning_output < 0
                    || tokens.reasoning_output > tokens.output
                {
                    diagnostics.rejected_events += 1;
                    continue;
                }
                if tokens.processed() == 0 {
                    continue;
                }
                if tokens.disagrees_with_reported_total(int_field(usage, "totalTokens")) {
                    diagnostics.total_mismatches += 1;
                }
                let Some(timestamp) = value
                    .get("time")
                    .and_then(Value::as_i64)
                    .filter(|time| *time > 0)
                else {
                    continue;
                };

                let sid = session_id.as_deref().unwrap_or(&directory_session);
                let (provider, model, event_key) = match event_type {
                    "assistant/message" => {
                        let source = data.pointer("/message/source");
                        // provider 侧换模时实际服务的模型记在 responseModel。
                        let model = non_empty(
                            source.and_then(|s| s.pointer("/replayState/response/responseModel")),
                        )
                        .or_else(|| non_empty(source.and_then(|s| s.get("model"))));
                        let key = match (non_empty(data.pointer("/message/id")), seq) {
                            (Some(id), _) => format!("dsh:msg:{id}"),
                            (None, Some(seq)) => format!("dsh:seq:{sid}:{seq}"),
                            (None, None) => format!("dsh:time:{sid}:{timestamp}"),
                        };
                        (
                            non_empty(source.and_then(|s| s.get("provider"))),
                            model,
                            key,
                        )
                    }
                    "assistant/attempt" => {
                        let key = match seq {
                            Some(seq) => format!("dsh:attempt:{sid}:{seq}"),
                            None => format!("dsh:attempt:{sid}:time:{timestamp}"),
                        };
                        (None, None, key)
                    }
                    _ => {
                        let key = match (non_empty(data.get("compactionId")), seq) {
                            (Some(id), _) => format!("dsh:summary:{id}"),
                            (None, Some(seq)) => format!("dsh:summary:{sid}:{seq}"),
                            (None, None) => format!("dsh:summary:{sid}:time:{timestamp}"),
                        };
                        (
                            non_empty(data.get("provider")),
                            non_empty(data.get("model")),
                            key,
                        )
                    }
                };
                let provider = provider.or(route_provider.as_deref());
                let pending = Pending {
                    seq,
                    event_key,
                    timestamp,
                    credited_agent: credited_agent(provider),
                    model: model.or(route_model.as_deref()).map(str::to_owned),
                    tokens,
                };

                if event_type == "compaction/summary" {
                    // 摘要不是循环里的一步，不占也不替换结算槽位。
                    ledger.insert(pending);
                    continue;
                }
                let slot = data
                    .get("turn")
                    .and_then(Value::as_i64)
                    .zip(data.get("step").and_then(Value::as_i64));
                match (slot, settlement) {
                    (Some((turn, step)), Some((last_turn, last_step, index)))
                        if (turn, step) == (last_turn, last_step) =>
                    {
                        ledger.remove(index);
                        settlement = Some((turn, step, ledger.insert(pending)));
                    }
                    (Some((turn, step)), _) => {
                        settlement = Some((turn, step, ledger.insert(pending)));
                    }
                    (None, _) => {
                        ledger.insert(pending);
                        settlement = None;
                    }
                }
            }
            _ => {}
        }
    }

    // fork 继承的前缀属于父会话，那边已经计过。
    let inherited_before = if seeded {
        inherited_cut.unwrap_or(0).max(legacy_seed_length)
    } else {
        legacy_seed_length
    };
    let session_id = session_id.unwrap_or(directory_session);
    let events = ledger
        .events
        .into_iter()
        .flatten()
        .filter(|pending| pending.seq.is_none_or(|seq| seq >= inherited_before))
        .filter(|pending| pending.timestamp >= cutoff_ms)
        .map(|pending| {
            UsageEvent::new(
                pending.credited_agent,
                pending.event_key,
                pending.timestamp,
                session_id.clone(),
                pending.model,
                pending.tokens,
                "exact",
            )
            .with_project(project.clone())
        })
        .collect();

    Ok(ParsedScan {
        source: ParsedSource {
            source_id: candidate.source_id.clone(),
            adapter_id: "dsh",
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

#[cfg(test)]
mod tests {
    use super::*;

    struct TempHome(PathBuf);

    impl TempHome {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("metrik-dsh-{name}-{}", std::process::id()));
            std::fs::remove_dir_all(&path).ok();
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        /// 按 DSH 的写法落盘：头一帧，之后每行一帧。
        fn write_zstd(&self, session: &str, file: &str, rows: &[&str]) -> PathBuf {
            let mut payload = Vec::new();
            for row in rows {
                payload.extend(zstd::encode_all(format!("{row}\n").as_bytes(), 3).unwrap());
            }
            self.write_raw(session, file, &payload)
        }

        fn write_raw(&self, session: &str, file: &str, bytes: &[u8]) -> PathBuf {
            let directory = self
                .0
                .join("sessions")
                .join("--D-work-usage--")
                .join(session);
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join(file);
            std::fs::write(&path, bytes).unwrap();
            path
        }

        fn scan(&self) -> Vec<ParsedScan> {
            let adapter = DshAdapter::with_home(self.0.clone());
            adapter
                .discover(0)
                .iter()
                .map(|candidate| adapter.parse(candidate, 0).unwrap())
                .collect()
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    const HEADER: &str = r#"{"type":"session","version":4,"id":"s-1","createdAt":1,"cwd":"D:\\work\\usage","isSeeded":false,"delegationDepth":0}"#;

    fn message(seq: i64, id: &str, turn: i64, step: i64, provider: &str, usage: &str) -> String {
        format!(
            r#"{{"type":"assistant/message","seq":{seq},"time":{},"data":{{"turn":{turn},"step":{step},"message":{{"id":"{id}","role":"assistant","source":{{"kind":"model","provider":"{provider}","model":"deepseek-v4-pro"}}}},"stream":[],"usage":{usage}}}}}"#,
            1_790_000_000_000 + seq
        )
    }

    #[test]
    fn reads_v4_usage_with_reasoning_inside_output() {
        let home = TempHome::new("basic");
        let served = r#"{"type":"assistant/message","seq":2,"time":1790000000002,"data":{"turn":1,"step":1,"message":{"id":"m-1","source":{"kind":"model","provider":"deepseek-official","model":"deepseek-chat","replayState":{"kind":"pi-ai","response":{"responseModel":"deepseek-v4-flash"}}}},"stream":[],"usage":{"inputTokens":130,"outputTokens":159,"reasoningTokens":100,"cacheReadTokens":13824,"totalTokens":14113}}}"#;
        home.write_zstd("s-1", "session.v4.jsonl.zstd", &[HEADER, served]);

        let scans = home.scan();
        assert_eq!(scans.len(), 1);
        let event = &scans[0].source.events[0];
        assert_eq!(event.adapter_id, "dsh");
        assert_eq!(event.event_key, "dsh:msg:m-1");
        assert_eq!(event.session_id, "s-1");
        assert_eq!(event.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(event.tokens.output, 159);
        assert_eq!(event.tokens.reasoning_output, 100);
        assert_eq!(event.tokens.processed(), 14113);
        assert_eq!(event.project_path.as_deref(), Some("D:/work/usage"));
        assert!(!scans[0].diagnostics.is_partial());
    }

    #[test]
    fn only_the_newest_generation_of_a_session_is_read() {
        let home = TempHome::new("generations");
        let row = message(
            2,
            "m-1",
            1,
            1,
            "deepseek-official",
            r#"{"inputTokens":10,"outputTokens":5}"#,
        );
        home.write_zstd("s-1", "session.v3.jsonl.zstd", &[HEADER, &row]);
        let newest = home.write_zstd("s-1", "session.v4.jsonl.zstd", &[HEADER, &row]);
        home.write_raw("s-1", "session.v4.jsonl.zstd.tmp", b"");
        home.write_raw("s-1", "session.v04.jsonl", b"");

        let candidates = DshAdapter::with_home(home.0.clone()).discover(0);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, newest);
        assert_eq!(generation("session.jsonl.zstd"), Some(0));
        assert_eq!(generation("session.v12.jsonl"), Some(12));
        assert_eq!(generation("session.v0.jsonl"), None);
    }

    #[test]
    fn fork_prefix_before_the_inherited_marker_belongs_to_the_parent() {
        let home = TempHome::new("fork");
        let header = r#"{"type":"session","version":4,"id":"child","createdAt":1,"cwd":"/work","isSeeded":true,"parentSession":"parent","delegationDepth":0}"#;
        let parent_message = message(
            1,
            "parent-m",
            1,
            1,
            "deepseek-official",
            r#"{"inputTokens":900,"outputTokens":100}"#,
        );
        let parent_attempt = r#"{"type":"assistant/attempt","seq":2,"time":1790000000002,"data":{"turn":1,"step":2,"stream":[{"type":"chunk","chunk":{"type":"usage","usage":{"inputTokens":50,"outputTokens":1}}}]}}"#;
        // 祖先的标记被一起复制过来；真正的边界是最后一个。
        let ancestor_marker =
            r#"{"type":"session/end-seed","seq":3,"time":1790000000003,"data":{"inherited":true}}"#;
        let parent_late = message(
            4,
            "parent-late",
            2,
            1,
            "deepseek-official",
            r#"{"inputTokens":7,"outputTokens":7}"#,
        );
        let marker =
            r#"{"type":"session/end-seed","seq":5,"time":1790000000005,"data":{"inherited":true}}"#;
        let own = message(
            6,
            "own",
            2,
            1,
            "deepseek-official",
            r#"{"inputTokens":10,"outputTokens":20}"#,
        );
        home.write_zstd(
            "child",
            "session.v4.jsonl.zstd",
            &[
                header,
                &parent_message,
                parent_attempt,
                ancestor_marker,
                &parent_late,
                marker,
                &own,
            ],
        );

        let events = &home.scan()[0].source.events;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_key, "dsh:msg:own");
        assert_eq!(events[0].tokens.processed(), 30);
    }

    #[test]
    fn legacy_seed_length_still_marks_the_inherited_prefix() {
        let home = TempHome::new("legacy-seed");
        let header =
            r#"{"type":"session","id":"child","createdAt":1,"cwd":"/work","seedLength":2}"#;
        let inherited = message(
            1,
            "parent",
            1,
            1,
            "deepseek-official",
            r#"{"inputTokens":900,"outputTokens":100}"#,
        );
        let own = message(
            2,
            "own",
            2,
            1,
            "deepseek-official",
            r#"{"inputTokens":10,"outputTokens":20}"#,
        );
        home.write_zstd("child", "session.jsonl.zstd", &[header, &inherited, &own]);

        let events = &home.scan()[0].source.events;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_key, "dsh:msg:own");
    }

    #[test]
    fn settlements_replace_within_a_step_and_retries_count_separately() {
        let home = TempHome::new("settlement");
        let failed = r#"{"type":"assistant/attempt","seq":1,"time":1790000000001,"data":{"turn":1,"step":1,"stream":[{"type":"chunk","chunk":{"type":"usage","usage":{"inputTokens":1,"outputTokens":1}}},{"type":"chunk","chunk":{"type":"usage","usage":{"inputTokens":40,"outputTokens":2}}}]}}"#;
        let retry = r#"{"type":"llm/retry-started","seq":2,"time":1790000000002,"data":{"retryId":"r","turn":1,"step":1,"retry":1}}"#;
        let first = message(
            3,
            "m-a",
            1,
            1,
            "deepseek-official",
            r#"{"inputTokens":10,"outputTokens":20}"#,
        );
        let resample = message(
            4,
            "m-b",
            1,
            1,
            "deepseek-official",
            r#"{"inputTokens":11,"outputTokens":21}"#,
        );
        home.write_zstd(
            "s-1",
            "session.v4.jsonl.zstd",
            &[HEADER, failed, retry, &first, &resample],
        );

        let events = &home.scan()[0].source.events;
        let keys: Vec<&str> = events
            .iter()
            .map(|event| event.event_key.as_str())
            .collect();
        assert_eq!(keys, ["dsh:attempt:s-1:1", "dsh:msg:m-b"]);
        // attempt 取流里最后一个 usage 样本，不累加中间快照。
        assert_eq!(events[0].tokens.processed(), 42);
        assert_eq!(events[1].tokens.processed(), 32);
    }

    #[test]
    fn compaction_summary_is_its_own_call_on_its_own_route() {
        let home = TempHome::new("summary");
        let request = r#"{"type":"request/header","seq":1,"time":1790000000001,"data":{"header":{"config":{"provider":"deepseek-official","model":"deepseek-v4-pro"}},"reason":"initial"}}"#;
        let reply = message(
            2,
            "m-1",
            1,
            1,
            "deepseek-official",
            r#"{"inputTokens":10,"outputTokens":20}"#,
        );
        let summary = r#"{"type":"compaction/summary","seq":3,"time":1790000000003,"data":{"compactionId":"c-1","summary":[],"shadowedRange":{"start":1,"end":2},"shadowedSeqs":[1,2],"shadowedTokenCount":30,"provider":"zai-coding-cn","model":"glm-5.3","usage":{"inputTokens":500,"outputTokens":200,"cacheReadTokens":4000,"totalTokens":4700}}}"#;
        home.write_zstd(
            "s-1",
            "session.v4.jsonl.zstd",
            &[HEADER, request, &reply, summary],
        );

        let events = &home.scan()[0].source.events;
        assert_eq!(events.len(), 2);
        let summary = &events[1];
        assert_eq!(summary.event_key, "dsh:summary:c-1");
        assert_eq!(summary.adapter_id, "zcode");
        assert_eq!(summary.model.as_deref(), Some("glm-5.3"));
        assert_eq!(summary.tokens.processed(), 4700);
    }

    #[test]
    fn coding_plan_routes_credit_their_own_cards() {
        assert_eq!(credited_agent(Some("zai")), "zcode");
        assert_eq!(credited_agent(Some("zai-coding-cn")), "zcode");
        assert_eq!(credited_agent(Some("qwen-token-plan-cn")), "qwen");
        assert_eq!(credited_agent(Some("opencode-go")), "opencode");
        assert_eq!(credited_agent(Some("kimi-coding")), "kimi");
        assert_eq!(credited_agent(Some("openai-codex")), "codex");
        // 按量计费的路由留在 dsh。
        assert_eq!(credited_agent(Some("deepseek-official")), "dsh");
        assert_eq!(credited_agent(Some("deepseek-account")), "dsh");
        assert_eq!(credited_agent(Some("deepseek")), "dsh");
        assert_eq!(credited_agent(None), "dsh");
    }

    #[test]
    fn attempts_follow_the_latest_request_route() {
        let home = TempHome::new("attempt-route");
        let request = r#"{"type":"request/header","seq":1,"time":1790000000001,"data":{"header":{"config":{"provider":"kimi-coding","model":"kimi-k3"}},"reason":"initial"}}"#;
        let attempt = r#"{"type":"assistant/attempt","seq":2,"time":1790000000002,"data":{"turn":1,"step":1,"stream":[{"type":"chunk","chunk":{"type":"usage","usage":{"inputTokens":40,"outputTokens":2}}}]}}"#;
        home.write_zstd("s-1", "session.v4.jsonl.zstd", &[HEADER, request, attempt]);

        let event = &home.scan()[0].source.events[0];
        assert_eq!(event.adapter_id, "kimi");
        assert_eq!(event.model.as_deref(), Some("kimi-k3"));
    }

    #[test]
    fn torn_trailing_frame_keeps_the_committed_prefix_without_flagging_it() {
        let home = TempHome::new("torn");
        let committed = message(
            1,
            "m-1",
            1,
            1,
            "deepseek-official",
            r#"{"inputTokens":10,"outputTokens":20}"#,
        );
        let mut payload = Vec::new();
        for row in [HEADER, committed.as_str()] {
            payload.extend(zstd::encode_all(format!("{row}\n").as_bytes(), 3).unwrap());
        }
        let torn_row = message(
            2,
            "m-2",
            2,
            1,
            "deepseek-official",
            r#"{"inputTokens":1,"outputTokens":2}"#,
        );
        let torn = zstd::encode_all(format!("{torn_row}\n").as_bytes(), 3).unwrap();
        payload.extend_from_slice(&torn[..torn.len() / 2]);
        home.write_raw("s-1", "session.v4.jsonl.zstd", &payload);

        let scans = home.scan();
        assert_eq!(scans[0].source.events.len(), 1);
        assert_eq!(scans[0].source.events[0].event_key, "dsh:msg:m-1");
        assert!(!scans[0].diagnostics.is_partial());
    }

    #[test]
    fn plain_jsonl_is_read_and_a_half_written_last_line_is_not_malformed() {
        let home = TempHome::new("plain");
        let row = message(
            1,
            "m-1",
            1,
            1,
            "deepseek-official",
            r#"{"inputTokens":10,"outputTokens":20}"#,
        );
        let body = format!("{HEADER}\n{row}\n{{\"type\":\"assistant/mess");
        home.write_raw("s-1", "session.v4.jsonl", body.as_bytes());

        let scans = home.scan();
        assert_eq!(scans[0].source.events.len(), 1);
        assert!(!scans[0].diagnostics.is_partial());
    }

    #[test]
    fn usage_that_breaks_the_token_contract_is_rejected() {
        let home = TempHome::new("rejected");
        let negative = message(
            1,
            "m-neg",
            1,
            1,
            "deepseek-official",
            r#"{"inputTokens":-5,"outputTokens":20}"#,
        );
        let reasoning = message(
            2,
            "m-rsn",
            2,
            1,
            "deepseek-official",
            r#"{"inputTokens":5,"outputTokens":20,"reasoningTokens":30}"#,
        );
        let mismatch = message(
            3,
            "m-tot",
            3,
            1,
            "deepseek-official",
            r#"{"inputTokens":5,"outputTokens":20,"totalTokens":99}"#,
        );
        home.write_zstd(
            "s-1",
            "session.v4.jsonl.zstd",
            &[HEADER, &negative, &reasoning, &mismatch],
        );

        let scan = &home.scan()[0];
        assert_eq!(scan.source.events.len(), 1);
        assert_eq!(scan.diagnostics.rejected_events, 2);
        assert_eq!(scan.diagnostics.total_mismatches, 1);
    }

    #[test]
    fn oversized_transcripts_are_reported_not_silently_zero() {
        let home = TempHome::new("oversized");
        let path = home.write_zstd("s-1", "session.v4.jsonl.zstd", &[HEADER]);
        let candidate = DshAdapter::with_home(home.0.clone()).discover(0).remove(0);
        assert_eq!(candidate.path, path);

        let scan = parse_transcript(&candidate, 0, 8).unwrap();
        assert!(scan.source.events.is_empty());
        assert_eq!(scan.diagnostics.unreadable_lines, 1);
    }
}
