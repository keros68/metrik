//! Google Antigravity（Codeium/Windsurf 血统的 agentic IDE）用量。
//!
//! 两个来源，产出同一份 `GeneratorMetadata`、同一套事件键：
//!
//! 1. **本地会话库**（桌面版、IDE 与 CLI）：每个会话（cascade）落成
//!    对应数据目录下的 `conversations/<cascadeId>.db`（SQLite，WAL）。
//!    `gen_metadata.data` 是 protobuf 编码的 `GeneratorMetadata`，与 RPC
//!    `GetCascadeTrajectoryGeneratorMetadata` 返回的是同一份数据；时间取自它
//!    产出的 step（`steps.metadata` 的创建时间）。关闭应用后仍可读取历史。
//!    项目归属取自同一数据根下 `conversation_summaries.db` 的工作区 URI。
//! 2. **language server 私有 RPC**（`exa.language_server_pb`，自签 TLS，认证
//!    令牌从进程命令行取）：只用于本地没有 `.db` 的会话（旧版 IDE 把会话存成
//!    `.pb`，不是这种库）。IDE 必须正在运行；进程不在时返回空，绝不估算。
//!    2.23 实测：端点发现与配额 RPC 正常，但 `GetAllCascadeTrajectories` 返回
//!    空对象，`chatStartMetadata` 也不再带 `createdAt`（只剩 `checkpointIndex`
//!    与 `contextWindowMetadata`）——单靠 RPC，2.x 的用量一条都读不到。对本地
//!    20 个会话逐个调 `GetCascadeTrajectoryGeneratorMetadata`，与本地库解析出的
//!    1383 个事件 event_id、token 分量、模型名全部一致。
//!
//! 套进 `AgentAdapter` 框架：`discover` = 本地库 + RPC 会话 + 一个配额入口，
//! `parse` = 取该会话的用量记录。账本按 `responseId` 去重——每次都读到该会话的
//! **全量**记录，活跃生成会被多次观察到、计数逐步增长，正是账本已有的
//! 「按 provider id 合并、分量取最大值」语义（与 Claude 同型，见 storage.rs）。
//!
//! 口径注记（2026-10 用本机 Antigravity 2.23 的 20 个会话、1383 次生成核对）：
//! - `inputTokens` 不含缓存：上下文增长时 input 回落、`cacheReadTokens` 接上。
//! - cache write 字段在全部样本里都没出现过（只用过 Gemini），恒记 0。
//! - `outputTokens == thinkingOutputTokens + responseOutputTokens` 在全部样本里
//!   成立，所以 thinking 是 output 的子项（reasoning_output），不重复计入
//!   processed。参考实现 tokscale 把两者分开相加，是错的。本地库每次读取都复核
//!   这条等式，对不上就把来源标成数据不完整。
//! - `responseId` 全部存在且跨会话唯一。
//! - 模型名优先用 `responseModel`（真名）；占位符（`model_placeholder_m37` 等）
//!   查表转真名，查不到原样保留。

use super::{AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{stable_hash, ParsedSource, QuotaSample, TokenVector, UsageEvent};
use anyhow::{anyhow, Context, Result};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const RPC_BASE_METHOD: &str = "/exa.language_server_pb.LanguageServerService";
/// RPC 会话的定位符前缀；本地库的定位符是真实文件路径。
const RPC_LOCATOR_PREFIX: &str = "antigravity://cascade/";

#[derive(Clone)]
struct Endpoint {
    base_url: String,
    csrf: String,
}

// 未找到时的冷却，避免高频进程扫描。
const NEGATIVE_TTL: Duration = Duration::from_secs(45);

static ENDPOINT_CACHE: Mutex<Option<(Instant, Option<Endpoint>)>> = Mutex::new(None);

/// 解析可用端点：命中缓存直接用；否则发现进程、取 csrf、探测监听端口。
fn get_endpoint() -> Option<Endpoint> {
    let mut guard = ENDPOINT_CACHE.lock().ok()?;
    cached_endpoint(&mut guard, NEGATIVE_TTL, heartbeat_ok, discover_endpoint)
}

/// 端点缓存的纯逻辑（I/O 由调用方注入，可测）：正缓存每次用 `is_alive` 验活，
/// 失活重新发现；负缓存（None）在 TTL 内不重复扫描进程，过期才再试。
/// 缓存必须放在 adapter 之外跨快照持有——adapter 每次快照都重建，实例级缓存
/// 会被重置，未装 Antigravity 的机器就会每轮快照 spawn 一次进程扫描（#186）。
fn cached_endpoint<T>(
    cache: &mut Option<(Instant, Option<T>)>,
    negative_ttl: Duration,
    is_alive: impl Fn(&T) -> bool,
    discover: impl FnOnce() -> Option<T>,
) -> Option<T>
where
    T: Clone,
{
    if let Some((captured, cached)) = cache.as_ref() {
        match cached {
            Some(endpoint) if is_alive(endpoint) => return Some(endpoint.clone()),
            None if captured.elapsed() < negative_ttl => return None,
            _ => {}
        }
    }
    let resolved = discover();
    *cache = Some((Instant::now(), resolved.clone()));
    resolved
}

/// 官方配额拉取入口（供 quota 统一注册表调用）。
pub fn fetch_antigravity_quota_snapshot(timeout: Duration) -> Result<Vec<QuotaSample>> {
    let endpoint = get_endpoint().ok_or_else(|| anyhow!("Antigravity language server 未在运行"))?;
    let value = rpc_call_with_timeout(
        &endpoint,
        "RetrieveUserQuotaSummary",
        &json!({ "forceRefresh": true }),
        timeout,
    )?;
    Ok(parse_quota_summary(&value))
}

pub struct AntigravityAdapter {
    /// 桌面版、独立 IDE 和 CLI 各有自己的会话库和摘要库。
    conversation_dirs: Vec<PathBuf>,
}

pub fn antigravity_data_dirs() -> Vec<PathBuf> {
    let gemini = dirs::home_dir().unwrap_or_default().join(".gemini");
    antigravity_data_dirs_in(&gemini)
}

fn antigravity_data_dirs_in(gemini: &Path) -> Vec<PathBuf> {
    ["antigravity", "antigravity-ide", "antigravity-cli"]
        .iter()
        .map(|app| gemini.join(app))
        .collect()
}

impl AntigravityAdapter {
    pub fn detected() -> Self {
        let gemini = dirs::home_dir().unwrap_or_default().join(".gemini");
        Self::from_gemini_dir(&gemini)
    }

    fn from_gemini_dir(gemini: &Path) -> Self {
        Self {
            conversation_dirs: antigravity_data_dirs_in(gemini)
                .into_iter()
                .map(|dir| dir.join("conversations"))
                .collect(),
        }
    }

    #[cfg(test)]
    fn with_conversation_dirs(conversation_dirs: Vec<PathBuf>) -> Self {
        Self { conversation_dirs }
    }

    /// 全部本地会话库，不看 cutoff：RPC 那边要靠它判断哪些会话已有本地来源。
    /// `-wal`/`-shm` 旁路文件的扩展名不是 `db`，不会混进来。
    fn local_databases(&self) -> Vec<PathBuf> {
        let mut databases: Vec<PathBuf> = self
            .conversation_dirs
            .iter()
            .filter_map(|dir| std::fs::read_dir(dir).ok())
            .flat_map(|entries| entries.filter_map(Result::ok))
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("db"))
            .collect();
        databases.sort();
        databases
    }
}

impl AgentAdapter for AntigravityAdapter {
    fn id(&self) -> &'static str {
        "antigravity"
    }

    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate> {
        let databases = self.local_databases();
        let mut candidates: Vec<SourceCandidate> = databases
            .iter()
            .flat_map(|database| local_conversation_candidate(database, cutoff_ms))
            .collect();

        let Some(endpoint) = get_endpoint() else {
            return candidates;
        };
        let local_ids: HashSet<&str> = databases
            .iter()
            .filter_map(|database| database.file_stem()?.to_str())
            .collect();
        if let Ok(list) = rpc_call(&endpoint, "GetAllCascadeTrajectories", &json!({})) {
            candidates.extend(rpc_candidates(&list, &local_ids));
        }
        candidates
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        let (cascade_id, events, diagnostics) = match candidate
            .path
            .to_string_lossy()
            .strip_prefix(RPC_LOCATOR_PREFIX)
        {
            Some(cascade_id) => {
                let endpoint = get_endpoint()
                    .ok_or_else(|| anyhow!("Antigravity language server 未在运行"))?;
                let metadata = rpc_call(
                    &endpoint,
                    "GetCascadeTrajectoryGeneratorMetadata",
                    &json!({ "cascadeId": cascade_id }),
                )?;
                let events = normalize_generator_metadata(&metadata, cascade_id, cutoff_ms);
                (cascade_id.to_owned(), events, ScanDiagnostics::default())
            }
            None => parse_local_database(&candidate.path, cutoff_ms)?,
        };

        Ok(ParsedScan {
            source: ParsedSource {
                source_id: candidate.source_id.clone(),
                adapter_id: self.id(),
                locator: candidate.path.clone(),
                logical_key: cascade_id,
                size: candidate.size,
                mtime_ns: candidate.mtime_ns,
                events,
                quotas: Vec::new(),
            },
            diagnostics,
        })
    }
}

/// 会话库仍决定来源身份、排序时间和保留期；摘要库是项目归属的额外依赖。
/// 将摘要库与 WAL 的各自状态写入不透明 size 指纹，不能只取最大 mtime：
/// 会话库的时间可能较新，掩盖后到的摘要。SHM 是读者也会改写的协调文件，
/// 不纳入摘要指纹；只读创建的空 WAL 与没有 WAL 等价，避免解析触发重扫。
fn local_conversation_candidate(database: &Path, cutoff_ms: i64) -> Vec<SourceCandidate> {
    let mut candidates = super::sqlite_candidate(database, "antigravity", cutoff_ms);
    for candidate in &mut candidates {
        let mut fingerprint = format!("{}:{}", candidate.size, candidate.mtime_ns);
        if let Some(summaries) = conversation_summaries_path(database) {
            for suffix in ["", "-wal"] {
                let mut path = summaries.as_os_str().to_os_string();
                path.push(suffix);
                match std::fs::metadata(PathBuf::from(path)) {
                    Ok(metadata) if suffix == "-wal" && metadata.len() == 0 => {
                        fingerprint.push_str("|-wal:missing");
                    }
                    Ok(metadata) => fingerprint.push_str(&format!(
                        "|{suffix}:{}:{}",
                        metadata.len(),
                        super::file_mtime_ns(&metadata)
                    )),
                    Err(_) => fingerprint.push_str(&format!("|{suffix}:missing")),
                }
            }
        }
        let hash = stable_hash(&fingerprint);
        candidate.size = super::positive_hash_u64(&hash);
    }
    candidates
}

// ── 纯解析函数（可测试，不碰 I/O） ─────────────────────────────

struct CascadeRef {
    id: String,
    last_modified_ms: i64,
    step_count: i64,
}

/// RPC 会话列表 → 候选。已有本地库的会话跳过：同一份数据不必经 RPC 再读一遍。
fn rpc_candidates(list: &Value, local_ids: &HashSet<&str>) -> Vec<SourceCandidate> {
    normalize_trajectory_summaries(list)
        .into_iter()
        .filter(|cascade| !local_ids.contains(cascade.id.as_str()))
        .map(|cascade| SourceCandidate {
            source_id: stable_hash(&format!("antigravity|{}", cascade.id)),
            path: PathBuf::from(format!("{RPC_LOCATOR_PREFIX}{}", cascade.id)),
            // stepCount 变化即内容变化，用它当"文件大小"触发重扫。
            size: cascade.step_count.max(0) as u64,
            mtime_ns: cascade.last_modified_ms.saturating_mul(1_000_000),
        })
        .collect()
}

/// `trajectorySummaries` 可能是数组，也可能是以 cascadeId 为键的对象；
/// 老版本字段名 `cascadeTrajectories`。三种形态都兼容。
fn normalize_trajectory_summaries(value: &Value) -> Vec<CascadeRef> {
    let container = value
        .get("trajectorySummaries")
        .or_else(|| value.get("cascadeTrajectories"))
        .or_else(|| value.get("trajectories"));
    let entries: Vec<&Value> = match container {
        Some(Value::Array(items)) => items.iter().collect(),
        Some(Value::Object(map)) => map.values().collect(),
        _ => return Vec::new(),
    };
    entries
        .into_iter()
        .filter_map(|entry| {
            let id = first_string(entry, &["cascadeId", "trajectoryId", "id", "sessionId"])?;
            Some(CascadeRef {
                id,
                last_modified_ms: first_time_ms(entry, &["lastModifiedTime", "lastModified"])
                    .unwrap_or(0),
                step_count: first_i64(entry, &["stepCount", "numSteps", "totalSteps"]).unwrap_or(0),
            })
        })
        .collect()
}

/// `generatorMetadata[].chatModel.retryInfos[].usage` 是权威计量位置；
/// 每条带 `responseId` 作为跨轮询去重键（provider message id）。
fn normalize_generator_metadata(
    value: &Value,
    cascade_id: &str,
    cutoff_ms: i64,
) -> Vec<UsageEvent> {
    let Some(rows) = value.get("generatorMetadata").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut events = Vec::new();
    for (row_index, row) in rows.iter().enumerate() {
        let chat_model = row.get("chatModel").unwrap_or(row);
        let model = resolve_model(first_string(chat_model, &["responseModel", "model"]));
        let created_ms = chat_model
            .get("chatStartMetadata")
            .and_then(|meta| first_time_ms(meta, &["createdAt", "timestamp"]))
            .unwrap_or(0);

        // retryInfos[].usage 是主路径；缺失时回退到 chatModel.usage（旧形态）。
        let usages: Vec<(&Value, usize)> =
            match chat_model.get("retryInfos").and_then(Value::as_array) {
                Some(retries) => retries
                    .iter()
                    .enumerate()
                    .filter_map(|(i, retry)| retry.get("usage").map(|usage| (usage, i)))
                    .collect(),
                None => chat_model
                    .get("usage")
                    .map(|usage| vec![(usage, 0usize)])
                    .unwrap_or_default(),
            };

        for (usage, retry_index) in usages {
            let tokens = TokenVector {
                input_uncached: first_i64(usage, &["inputTokens"]).unwrap_or(0).max(0),
                cache_read: first_i64(usage, &["cacheReadTokens"]).unwrap_or(0).max(0),
                // cache write 无证据存在：恒 0，不伪造。
                cache_write: 0,
                output: first_i64(usage, &["outputTokens", "responseOutputTokens"])
                    .unwrap_or(0)
                    .max(0),
                // thinking 是 output 的子项，不重复计入 processed。
                reasoning_output: first_i64(usage, &["thinkingOutputTokens"])
                    .unwrap_or(0)
                    .max(0),
            };
            if tokens.processed() == 0 {
                continue;
            }
            let occurred_at_ms = first_time_ms(usage, &["createdAt", "timestamp"])
                .filter(|value| *value > 0)
                .unwrap_or(created_ms);
            if occurred_at_ms < cutoff_ms {
                continue;
            }
            // responseId 作 provider 身份；缺失时回退到位置身份（稳定但不跨版本）。
            let response_id = first_string(usage, &["responseId"])
                .unwrap_or_else(|| format!("{cascade_id}:{row_index}:{retry_index}"));
            events.push(UsageEvent::new(
                "antigravity",
                format!("response:{response_id}"),
                occurred_at_ms,
                cascade_id.to_owned(),
                model.clone(),
                tokens,
                "rpc_snapshot",
            ));
        }
    }
    events
}

/// 官方配额：两个 group（Gemini / Claude+GPT），各含 weekly + 5h 桶。
/// `remainingFraction` 可能直接给，也可能包在 oneof `remaining.remainingFraction`。
/// 桶名是连字符（2.23 实测 `gemini-weekly`、`3p-5h`），按 statusLine 钩子的
/// 约定归一成下划线，否则界面认不出窗口、标签不会美化。
fn parse_quota_summary(value: &Value) -> Vec<QuotaSample> {
    let root = value
        .get("response")
        .or_else(|| value.get("summary"))
        .unwrap_or(value);
    let Some(groups) = root.get("groups").and_then(Value::as_array) else {
        return Vec::new();
    };
    let now = chrono::Utc::now().timestamp_millis();
    let mut samples = Vec::new();
    for group in groups {
        let Some(buckets) = group.get("buckets").and_then(Value::as_array) else {
            continue;
        };
        for bucket in buckets {
            if bucket.get("disabled").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let Some(bucket_id) = first_string(bucket, &["bucketId", "displayName"]) else {
                continue;
            };
            let remaining = bucket
                .get("remainingFraction")
                .and_then(Value::as_f64)
                .or_else(|| {
                    bucket
                        .get("remaining")
                        .and_then(|inner| inner.get("remainingFraction"))
                        .and_then(Value::as_f64)
                });
            let Some(remaining) = remaining else {
                continue;
            };
            samples.push(QuotaSample {
                adapter_id: "antigravity",
                window_key: crate::antigravity_hook::normalize_window_key(&bucket_id),
                remaining_percent: (remaining * 100.0).clamp(0.0, 100.0),
                resets_at_ms: first_time_ms(bucket, &["resetTime", "resetsAt"]),
                collected_at_ms: now,
                source_label: "Antigravity 官方配额".into(),
                quality: "official_live",
            });
        }
    }
    samples
}

/// 占位符模型名 → 真名。查不到原样保留（不猜、不丢）。表会随版本滞后。
fn resolve_model(raw: Option<String>) -> Option<String> {
    let raw = raw.filter(|value| !value.is_empty())?;
    let mapped = match raw.to_ascii_lowercase().as_str() {
        "model_placeholder_m26" => "claude-opus-4-6",
        "model_placeholder_m35" => "claude-sonnet-4-6",
        "model_placeholder_m36" | "model_placeholder_m37" => "gemini-3.1-pro",
        "model_placeholder_m47" => "gemini-3-flash-preview",
        "model_openai_gpt_oss_120b_medium" => "gpt-oss-120b-medium",
        _ => return Some(raw),
    };
    Some(mapped.to_owned())
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    })
}

fn first_i64(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        value.get(key).and_then(|found| {
            found
                .as_i64()
                .or_else(|| found.as_u64().map(|v| v as i64))
                .or_else(|| found.as_f64().map(|v| v as i64))
                // 真机响应里 token 计数是字符串（"21861"）——protobuf 的 int64
                // JSON 编码惯例。不解析字符串就会全部读成 0，事件被当空跳过。
                .or_else(|| found.as_str().and_then(|text| text.trim().parse().ok()))
        })
    })
}

/// 时间字段可能是数字（毫秒或秒）或 ISO-8601 字符串，统一成毫秒。
fn first_time_ms(value: &Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        let Some(found) = value.get(key) else {
            continue;
        };
        if let Some(number) = found.as_i64().or_else(|| found.as_f64().map(|v| v as i64)) {
            // 10 位约为秒，13 位约为毫秒。
            return Some(if number < 100_000_000_000 {
                number * 1000
            } else {
                number
            });
        }
        if let Some(text) = found.as_str() {
            if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(text) {
                return Some(parsed.timestamp_millis());
            }
        }
    }
    None
}

// ── 本地会话库（Antigravity 2.x：protobuf 存在 SQLite 里） ─────────
//
// 字段号是从真实会话库里对出来的（值与 RPC JSON 一一对应），只列用到的：
//   GeneratorMetadata  1 chatModel  2 产出的 step 下标（packed）
//   ChatModelMetadata  4 usage  17 retryInfos[]（2 usage）  19 responseModel
//   ModelUsageStats    2 inputTokens  3 outputTokens  5 cacheReadTokens
//                      9 thinkingOutputTokens  10 responseOutputTokens  11 responseId
//   CortexStepMetadata 1 createdAt（Timestamp：1 秒  2 纳秒）

const GENERATION_CHAT_MODEL: u64 = 1;
const GENERATION_STEP_INDICES: u64 = 2;
const CHAT_MODEL_USAGE: u64 = 4;
const CHAT_MODEL_RETRY_INFOS: u64 = 17;
const CHAT_MODEL_RESPONSE_MODEL: u64 = 19;
const RETRY_INFO_USAGE: u64 = 2;
const USAGE_OUTPUT: u64 = 3;
const USAGE_THINKING_OUTPUT: u64 = 9;
const USAGE_RESPONSE_OUTPUT: u64 = 10;
const USAGE_RESPONSE_ID: u64 = 11;
const USAGE_COUNTERS: [(u64, &str); 5] = [
    (2, "inputTokens"),
    (USAGE_OUTPUT, "outputTokens"),
    (5, "cacheReadTokens"),
    (USAGE_THINKING_OUTPUT, "thinkingOutputTokens"),
    (USAGE_RESPONSE_OUTPUT, "responseOutputTokens"),
];
const STEP_CREATED_AT: u64 = 1;

/// protobuf 线格式的一个字段值。定长字段用不到，只跳过。
enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

/// 一条 protobuf 消息拆成的 (字段号, 值) 列表。没有 .proto 也能按字段号取值。
struct ProtoMessage<'a>(Vec<(u64, Wire<'a>)>);

fn read_varint(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(*pos)?;
        *pos += 1;
        // 第十个字节只能容纳 u64 的最后一位；否则移位会悄悄截掉溢出位。
        if shift == 63 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

impl<'a> ProtoMessage<'a> {
    /// 任何越界或未知线型都返回 None：宁可整条记为读不懂，也不从错位的字节里
    /// 读出一个看似合理的数。
    fn parse(bytes: &'a [u8]) -> Option<Self> {
        let mut fields = Vec::new();
        let mut pos = 0;
        while pos < bytes.len() {
            let key = read_varint(bytes, &mut pos)?;
            if key >> 3 == 0 || key >> 3 > (1 << 29) - 1 {
                return None;
            }
            let value = match key & 7 {
                0 => Wire::Varint(read_varint(bytes, &mut pos)?),
                1 | 5 => {
                    pos = pos.checked_add(if key & 7 == 1 { 8 } else { 4 })?;
                    Wire::Fixed
                }
                2 => {
                    let len = usize::try_from(read_varint(bytes, &mut pos)?).ok()?;
                    let end = pos.checked_add(len)?;
                    let payload = bytes.get(pos..end)?;
                    pos = end;
                    Wire::Bytes(payload)
                }
                _ => return None,
            };
            if pos > bytes.len() {
                return None;
            }
            fields.push((key >> 3, value));
        }
        Some(Self(fields))
    }

    fn varint(&self, number: u64) -> Option<u64> {
        self.0.iter().find_map(|(field, value)| match value {
            Wire::Varint(found) if *field == number => Some(*found),
            _ => None,
        })
    }

    fn all_fields(&self, number: u64) -> impl Iterator<Item = &Wire<'a>> + '_ {
        self.0
            .iter()
            .filter_map(move |(field, value)| (*field == number).then_some(value))
    }

    fn has_field(&self, number: u64) -> bool {
        self.all_fields(number).next().is_some()
    }

    fn all_bytes(&self, number: u64) -> impl Iterator<Item = &'a [u8]> + '_ {
        self.all_fields(number).filter_map(|value| match value {
            Wire::Bytes(found) => Some(*found),
            _ => None,
        })
    }

    fn message(&self, number: u64) -> Option<ProtoMessage<'a>> {
        self.all_bytes(number).next().and_then(ProtoMessage::parse)
    }

    fn string(&self, number: u64) -> Option<String> {
        let bytes = self.all_bytes(number).next()?;
        std::str::from_utf8(bytes)
            .ok()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    }

    /// repeated 标量：proto3 默认 packed，也兼容逐个编码的旧写法。
    fn repeated_varints(&self, number: u64) -> Option<Vec<u64>> {
        let mut values = Vec::new();
        for value in self.all_fields(number) {
            match value {
                Wire::Varint(found) => values.push(*found),
                Wire::Bytes(packed) => {
                    let mut pos = 0;
                    while pos < packed.len() {
                        values.push(read_varint(packed, &mut pos)?);
                    }
                }
                _ => return None,
            }
        }
        Some(values)
    }
}

/// 读一个本地会话库：每行 `gen_metadata` 转成 RPC 的 JSON 形状，再交给
/// `normalize_generator_metadata`——两个来源共用一套口径与事件键，同一次生成
/// 不论从哪边读到都落成同一个事件。项目归属取自会话摘要库（见
/// `conversation_workspace`），RPC 那边没有工作区信息。
fn parse_local_database(
    path: &Path,
    cutoff_ms: i64,
) -> Result<(String, Vec<UsageEvent>, ScanDiagnostics)> {
    parse_local_database_with_step_read(path, cutoff_ms, || {})
}

// after_step_read 让 WAL 并发测试能在两次查询之间确定性地提交写入。
fn parse_local_database_with_step_read(
    path: &Path,
    cutoff_ms: i64,
    after_step_read: impl FnOnce(),
) -> Result<(String, Vec<UsageEvent>, ScanDiagnostics)> {
    let cascade_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_owned();
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open {}", path.display()))?;
    connection.pragma_update(None, "busy_timeout", 2_000_i64)?;
    // 两张表必须来自同一份快照：否则 WAL 写入可能让新生成找不到它的新 step。
    // 连接以 READ_ONLY 打开，DEFERRED 事务只固定读快照，不阻塞 WAL 写入者。
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let step_times = step_created_ms(&transaction)?;
    after_step_read();

    let mut diagnostics = ScanDiagnostics::default();
    let mut rows = Vec::new();
    {
        let mut statement = transaction.prepare("SELECT data FROM gen_metadata ORDER BY idx")?;
        let blobs = statement.query_map([], |row| row.get::<_, Option<Vec<u8>>>(0))?;
        for blob in blobs {
            let row = blob
                .ok()
                .flatten()
                .and_then(|data| generation_json(&data, &step_times, &mut diagnostics));
            // 读不懂的行留空位，保持行号与 RPC 返回的数组下标一致（回退身份要用）。
            rows.push(row.unwrap_or_else(|| {
                diagnostics.malformed_lines += 1;
                Value::Null
            }));
        }
    }
    transaction.commit()?;
    let project = conversation_workspace(path, &cascade_id);
    let events: Vec<UsageEvent> = normalize_generator_metadata(
        &json!({ "generatorMetadata": rows }),
        &cascade_id,
        cutoff_ms,
    )
    .into_iter()
    .map(|event| event.with_project(project.clone()))
    .collect();
    Ok((cascade_id, events, diagnostics))
}

fn conversation_summaries_path(conversation_db: &Path) -> Option<PathBuf> {
    Some(
        conversation_db
            .parent()?
            .parent()?
            .join("conversation_summaries.db"),
    )
}

/// 会话的工作目录。2.x 把会话摘要放在会话库目录的上一级
/// （`~/.gemini/antigravity/conversation_summaries.db`），`workspace_uris` 记着
/// 会话打开时的工作区。摘要库、表或这一行不存在时返回 None：项目归属缺失是
/// 可接受的降级，不能让用量因此解析失败。新会话的摘要可能晚于会话库落盘，
/// 这时先记为未归属；摘要库或 WAL 变化也会触发重扫，账本只回填空值。
fn conversation_workspace(conversation_db: &Path, cascade_id: &str) -> Option<String> {
    let summaries = conversation_summaries_path(conversation_db)?;
    let connection = Connection::open_with_flags(
        summaries,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    connection
        .pragma_update(None, "busy_timeout", 2_000_i64)
        .ok()?;
    let uris: String = connection
        .query_row(
            "SELECT workspace_uris FROM conversation_summaries WHERE conversation_id = ?1",
            [cascade_id],
            |row| row.get(0),
        )
        .ok()?;
    workspace_path(&uris)
}

/// `workspace_uris` 是 JSON 字符串数组，元素是 VS Code 风格的 file URI
/// （`file:///c%3A/Users/me/proj`）。取第一个本机 file URI 转成路径：本机
/// 20 个会话都只有一个根，多根工作区没见过，取第一个根。远程、WSL 等其他
/// 方案、带主机名、UNC、查询/片段与控制字符不猜，继续尝试下一个根。
fn workspace_path(uris: &str) -> Option<String> {
    let uris: Vec<String> = serde_json::from_str(uris).ok()?;
    uris.iter().find_map(|uri| local_file_uri_path(uri))
}

fn local_file_uri_path(uri: &str) -> Option<String> {
    let encoded_path = uri.strip_prefix("file:///")?;
    // 先判断 URI 分量再解码：编码的 %23/%3F 是文件名，原始 #/? 是其他分量。
    if encoded_path.contains(['#', '?', '\\']) || uri.chars().any(char::is_control) {
        return None;
    }
    let path = percent_decode(encoded_path)?;
    // file:////host 与编码的根分隔符都可能变成 UNC；反斜杠的含义也不猜。
    if path.starts_with('/') || path.contains('\\') || path.chars().any(char::is_control) {
        return None;
    }
    let windows_drive = matches!(path.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic());
    if windows_drive {
        // 只接受绝对盘符路径，不能把 c:relative 当成一个记录过的项目目录。
        (path.as_bytes().get(2) == Some(&b'/')).then_some(path)
    } else {
        Some(format!("/{path}"))
    }
}

/// URI 的百分号解码。非法转义或解出非 UTF-8 时返回 None，不产出半截路径。
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            if !hex.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            decoded.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// step 下标 → 创建时间（毫秒）。生成本身不带时间，归日只能借它产出的 step。
fn step_created_ms(connection: &Connection) -> Result<HashMap<u64, i64>> {
    let mut statement =
        connection.prepare("SELECT idx, metadata FROM steps WHERE metadata IS NOT NULL")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
    })?;
    let mut times = HashMap::new();
    for (index, metadata) in rows.filter_map(Result::ok) {
        let Some(created) =
            ProtoMessage::parse(&metadata).and_then(|step| step.message(STEP_CREATED_AT))
        else {
            continue;
        };
        let (Ok(index), Some(seconds)) = (u64::try_from(index), created.varint(1)) else {
            continue;
        };
        let Ok(seconds) = i64::try_from(seconds) else {
            continue;
        };
        let millis = i64::try_from(created.varint(2).unwrap_or(0) / 1_000_000).unwrap_or(0);
        if seconds > 0 {
            times.insert(index, seconds.saturating_mul(1000).saturating_add(millis));
        }
    }
    Ok(times)
}

/// 一行 `gen_metadata` → RPC `generatorMetadata[]` 的一个元素。
/// 读不懂、或有用量却找不到时间的行返回 None（调用方计入 malformed）。
/// 无用量且无时间的尝试返回 `Value::Null`。损坏的重试单独计入 malformed，
/// 保留空位，其他可读重试仍然入账。
fn generation_json(
    data: &[u8],
    step_times: &HashMap<u64, i64>,
    diagnostics: &mut ScanDiagnostics,
) -> Option<Value> {
    let generation = ProtoMessage::parse(data)?;
    let Some(chat_model) = generation.message(GENERATION_CHAT_MODEL) else {
        // 字段缺失才是非聊天生成；存在却解不开时不能当成正常空行。
        return (!generation.has_field(GENERATION_CHAT_MODEL)).then_some(Value::Null);
    };
    let created_ms = generation
        .repeated_varints(GENERATION_STEP_INDICES)?
        .into_iter()
        .find_map(|index| step_times.get(&index).copied());

    let mut needs_timestamp = false;
    let mut usage_json = |usage: &ProtoMessage, total_mismatches: &mut usize| {
        // proto3 会省略零值：纯 thinking 输出和空失败重试也要复核这条等式。
        let response = usage.varint(USAGE_RESPONSE_OUTPUT).unwrap_or(0);
        let thinking = usage.varint(USAGE_THINKING_OUTPUT).unwrap_or(0);
        if thinking.checked_add(response) != Some(usage.varint(USAGE_OUTPUT).unwrap_or(0)) {
            *total_mismatches += 1;
        }
        let mut fields = serde_json::Map::new();
        for (number, key) in USAGE_COUNTERS {
            match usage.varint(number) {
                Some(value) => {
                    needs_timestamp |= value > 0;
                    fields.insert(key.into(), json!(value));
                }
                None => needs_timestamp |= usage.has_field(number),
            }
        }
        if let Some(response_id) = usage.string(USAGE_RESPONSE_ID) {
            fields.insert("responseId".into(), json!(response_id));
        }
        Value::Object(fields)
    };

    let mut row = serde_json::Map::new();
    if let Some(model) = chat_model.string(CHAT_MODEL_RESPONSE_MODEL) {
        row.insert("responseModel".into(), json!(model));
    }
    let retries: Vec<Value> = chat_model
        .all_fields(CHAT_MODEL_RETRY_INFOS)
        .map(|value| {
            let retry = match value {
                Wire::Bytes(data) => ProtoMessage::parse(data),
                _ => None,
            };
            let usage = retry.and_then(|retry| match retry.message(RETRY_INFO_USAGE) {
                Some(usage) => Some(usage_json(&usage, &mut diagnostics.total_mismatches)),
                None if !retry.has_field(RETRY_INFO_USAGE) => Some(json!({})),
                _ => None,
            });
            match usage {
                Some(usage) => json!({ "usage": usage }),
                None => {
                    diagnostics.malformed_lines += 1;
                    Value::Null
                }
            }
        })
        .collect();
    if !chat_model.has_field(CHAT_MODEL_RETRY_INFOS) {
        if let Some(usage) = chat_model.message(CHAT_MODEL_USAGE) {
            row.insert(
                "usage".into(),
                usage_json(&usage, &mut diagnostics.total_mismatches),
            );
        } else if chat_model.has_field(CHAT_MODEL_USAGE) {
            return None;
        }
    } else {
        row.insert("retryInfos".into(), Value::Array(retries));
    }
    let Some(created_ms) = created_ms else {
        return (!needs_timestamp).then_some(Value::Null);
    };
    row.insert(
        "chatStartMetadata".into(),
        json!({ "createdAt": created_ms }),
    );
    Some(json!({ "chatModel": row }))
}

// ── I/O 层（TLS、进程/端口发现、RPC） ──────────────────────────

/// 仅用于连接本机 127.0.0.1 的自签 language server：接受任意证书。
/// 全部参考实现（tokscale/CodexBar/…）都这么做——服务端证书本就是自签的。
#[derive(Debug)]
struct AcceptAnyServerCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        use rustls::SignatureScheme::*;
        vec![
            RSA_PKCS1_SHA256,
            RSA_PKCS1_SHA384,
            RSA_PKCS1_SHA512,
            ECDSA_NISTP256_SHA256,
            ECDSA_NISTP384_SHA384,
            RSA_PSS_SHA256,
            RSA_PSS_SHA384,
            RSA_PSS_SHA512,
            ED25519,
        ]
    }
}

fn insecure_agent(timeout: Duration) -> ureq::Agent {
    // 显式传 ring provider：不依赖进程级默认（未安装/有歧义都会 panic）。
    let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("ring 支持全部默认协议版本")
    .dangerous()
    .with_custom_certificate_verifier(std::sync::Arc::new(AcceptAnyServerCert))
    .with_no_client_auth();
    ureq::AgentBuilder::new()
        .timeout(timeout)
        .tls_config(std::sync::Arc::new(config))
        .build()
}

fn rpc_call_with_timeout(
    endpoint: &Endpoint,
    method: &str,
    body: &Value,
    timeout: Duration,
) -> Result<Value> {
    let agent = insecure_agent(timeout);
    let url = format!("{}{RPC_BASE_METHOD}/{method}", endpoint.base_url);
    let response = agent
        .post(&url)
        .set("Content-Type", "application/json")
        .set("Connect-Protocol-Version", "1")
        .set("X-Codeium-Csrf-Token", &endpoint.csrf)
        .send_string(&body.to_string())
        .map_err(|error| anyhow!("Antigravity RPC {method} 失败：{error}"))?;
    let text = response
        .into_string()
        .context("读取 Antigravity 响应失败")?;
    serde_json::from_str(&text).context("Antigravity 响应不是有效 JSON")
}

fn rpc_call(endpoint: &Endpoint, method: &str, body: &Value) -> Result<Value> {
    rpc_call_with_timeout(endpoint, method, body, Duration::from_secs(6))
}

fn heartbeat_ok(endpoint: &Endpoint) -> bool {
    rpc_call(
        endpoint,
        "Heartbeat",
        &json!({ "uuid": "00000000-0000-0000-0000-000000000000" }),
    )
    .is_ok()
}

/// 发现 language server 进程 → 取 csrf 与候选端口 → 逐端口 Heartbeat 探测。
fn discover_endpoint() -> Option<Endpoint> {
    // Antigravity 会 spawn 多个 language_server 进程，只有一个真正在监听 RPC
    // 端口（本机实测：5 个带 csrf 的进程里仅 1 个 LISTENING）。逐个进程、逐个
    // 端口试 Heartbeat，先通者胜——只认第一个进程会在它不是服务进程时漏掉数据。
    for process in find_language_server_processes() {
        let mut ports = listening_ports(process.pid);
        // 命令行里声明的端口优先试。
        if let Some(declared) = process.declared_port {
            ports.insert(0, declared);
            ports.dedup();
        }
        for port in ports {
            for scheme in ["https", "http"] {
                let endpoint = Endpoint {
                    base_url: format!("{scheme}://127.0.0.1:{port}"),
                    csrf: process.csrf.clone(),
                };
                if heartbeat_ok(&endpoint) {
                    return Some(endpoint);
                }
            }
        }
    }
    None
}

struct ServerProcess {
    pid: u32,
    csrf: String,
    declared_port: Option<u16>,
}

fn extract_csrf(command: &str) -> Option<String> {
    let idx = command.find("--csrf_token")?;
    let rest = command[idx + "--csrf_token".len()..].trim_start_matches(['=', ' ', '\t']);
    let token: String = rest
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() || *c == '-')
        .collect();
    (!token.is_empty()).then_some(token)
}

fn extract_declared_port(command: &str) -> Option<u16> {
    let idx = command.find("--extension_server_port")?;
    let rest =
        command[idx + "--extension_server_port".len()..].trim_start_matches(['=', ' ', '\t']);
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn is_antigravity_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    (lower.contains("language_server") || lower.contains("language-server"))
        && (lower.contains("antigravity") || lower.contains("--csrf_token"))
}

/// 从一条进程命令行里认出 Antigravity language server 并取出 csrf 与声明端口。
fn server_process(pid: u32, command: &str) -> Option<ServerProcess> {
    if !is_antigravity_command(command) {
        return None;
    }
    Some(ServerProcess {
        pid,
        csrf: extract_csrf(command)?,
        declared_port: extract_declared_port(command),
    })
}

/// Windows 走原生 API，不拉起 PowerShell / netstat：未装 Antigravity 的机器也会按
/// 负缓存节奏反复发现，每次一个 PowerShell 在常驻进程里代价太高（#186），
/// 关机过程中还会以 0xc0000142 弹窗。命令行是 UTF-16 原文，没有代码页问题。
#[cfg(windows)]
fn find_language_server_processes() -> Vec<ServerProcess> {
    native::language_server_command_lines()
        .into_iter()
        .filter_map(|(pid, command)| server_process(pid, &command))
        .collect()
}

#[cfg(windows)]
fn listening_ports(pid: u32) -> Vec<u16> {
    native::listening_ipv4_ports(pid)
}

#[cfg(windows)]
mod native {
    use windows::Wdk::System::Threading::{
        NtQueryInformationProcess, ProcessCommandLineInformation,
    };
    use windows::Win32::Foundation::{CloseHandle, HANDLE, UNICODE_STRING};
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
        TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows::Win32::Networking::WinSock::AF_INET;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: 句柄由本模块打开，只在这里关闭一次。
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// 可执行文件名像 language server 的进程及其命令行。先按文件名筛，只对候选
    /// 打开进程句柄；读不到命令行的（更高权限的进程）跳过，与 WMI 查询一致。
    pub(super) fn language_server_command_lines() -> Vec<(u32, String)> {
        let mut found = Vec::new();
        // SAFETY: 快照句柄由 OwnedHandle 关闭；PROCESSENTRY32W 按要求先填 dwSize。
        unsafe {
            let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return found;
            };
            let snapshot = OwnedHandle(snapshot);
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut more = Process32FirstW(snapshot.0, &mut entry).is_ok();
            while more {
                let name_len = entry
                    .szExeFile
                    .iter()
                    .position(|&unit| unit == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name =
                    String::from_utf16_lossy(&entry.szExeFile[..name_len]).to_ascii_lowercase();
                if name.contains("language_server") || name.contains("language-server") {
                    if let Some(command) = command_line(entry.th32ProcessID) {
                        found.push((entry.th32ProcessID, command));
                    }
                }
                more = Process32NextW(snapshot.0, &mut entry).is_ok();
            }
        }
        found
    }

    pub(super) fn command_line(pid: u32) -> Option<String> {
        // SAFETY: 缓冲区按 8 字节对齐分配，长度如实传入；读取 UNICODE_STRING 指向
        // 的字符前核对它落在同一块缓冲区内。
        unsafe {
            let process =
                OwnedHandle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?);
            let mut needed = 0u32;
            let _ = NtQueryInformationProcess(
                process.0,
                ProcessCommandLineInformation,
                std::ptr::null_mut(),
                0,
                &mut needed,
            );
            if (needed as usize) < std::mem::size_of::<UNICODE_STRING>() {
                return None;
            }
            let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
            let capacity = buffer.len() * 8;
            NtQueryInformationProcess(
                process.0,
                ProcessCommandLineInformation,
                buffer.as_mut_ptr().cast(),
                capacity as u32,
                &mut needed,
            )
            .ok()
            .ok()?;
            let text = &*buffer.as_ptr().cast::<UNICODE_STRING>();
            let units = usize::from(text.Length) / 2;
            if units == 0 || text.Buffer.is_null() {
                return None;
            }
            let start = buffer.as_ptr() as usize;
            let begin = text.Buffer.0 as usize;
            if begin < start || begin + units * 2 > start + capacity {
                return None;
            }
            Some(String::from_utf16_lossy(std::slice::from_raw_parts(
                text.Buffer.0,
                units,
            )))
        }
    }

    /// 该进程在 IPv4 上监听的 TCP 端口（与 `netstat -ano -p TCP` 同口径）。
    pub(super) fn listening_ipv4_ports(pid: u32) -> Vec<u16> {
        const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
        let mut size = std::mem::size_of::<MIB_TCPTABLE_OWNER_PID>() as u32;
        // 表在两次调用之间可能变大，缓冲不够就按系统给的新尺寸再来。
        for _ in 0..4 {
            let mut buffer = vec![0u32; (size as usize).div_ceil(4)];
            let capacity = buffer.len() * 4;
            size = capacity as u32;
            // SAFETY: 缓冲区按 4 字节对齐，长度如实传入。
            let status = unsafe {
                GetExtendedTcpTable(
                    Some(buffer.as_mut_ptr().cast()),
                    &mut size,
                    false,
                    u32::from(AF_INET.0),
                    TCP_TABLE_OWNER_PID_LISTENER,
                    0,
                )
            };
            if status == ERROR_INSUFFICIENT_BUFFER {
                continue;
            }
            if status != 0 {
                return Vec::new();
            }
            let table = buffer.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
            // SAFETY: 调用成功后表头有效；按 dwNumEntries 读行前核对不越过缓冲区。
            let rows = unsafe {
                let count = (*table).dwNumEntries as usize;
                let first = std::ptr::addr_of!((*table).table).cast::<MIB_TCPROW_OWNER_PID>();
                let end = first as usize + count * std::mem::size_of::<MIB_TCPROW_OWNER_PID>();
                if end > buffer.as_ptr() as usize + capacity {
                    return Vec::new();
                }
                std::slice::from_raw_parts(first, count)
            };
            let mut ports: Vec<u16> = rows
                .iter()
                .filter(|row| row.dwOwningPid == pid)
                .map(|row| u16::from_be(row.dwLocalPort as u16))
                .collect();
            ports.dedup();
            return ports;
        }
        Vec::new()
    }
}

#[cfg(not(windows))]
fn find_language_server_processes() -> Vec<ServerProcess> {
    // `ww` 是必须的：macOS 的 ps 默认把命令行截断到终端宽度，而 --csrf_token
    // 在 language_server 那条长命令行的靠后位置——截断后取不到 csrf，端点发现
    // 会静默失败。Linux 的 ps 同样接受 ww（不限宽），两平台通用。
    let Ok(output) = crate::child_process::output(
        crate::child_process::Site::AntigravityProcessScan,
        std::process::Command::new("/bin/ps").args(["-axww", "-o", "pid=,command="]),
    ) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut processes = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        let Some((pid_str, command)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        processes.extend(server_process(pid, command));
    }
    processes
}

#[cfg(not(windows))]
fn listening_ports(pid: u32) -> Vec<u16> {
    let output = crate::child_process::output(
        crate::child_process::Site::AntigravityPortScan,
        std::process::Command::new("lsof").args([
            "-Pan",
            "-p",
            &pid.to_string(),
            "-iTCP",
            "-sTCP:LISTEN",
        ]),
    );
    let Ok(output) = output else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut ports = Vec::new();
    for line in text.lines() {
        if let Some(port) = line
            .rsplit(':')
            .next()
            .and_then(|tail| tail.split_whitespace().next())
            .and_then(|p| p.parse().ok())
        {
            ports.push(port);
        }
    }
    ports.dedup();
    ports
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn native_command_line_reads_a_running_process() {
        let command = native::command_line(std::process::id()).expect("own command line");
        let exe = std::env::current_exe().unwrap();
        let stem = exe.file_stem().unwrap().to_string_lossy();
        assert!(command.contains(stem.as_ref()), "{command}");
    }

    #[cfg(windows)]
    #[test]
    fn native_listening_ports_find_a_port_this_process_owns() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(native::listening_ipv4_ports(std::process::id()).contains(&port));
        assert!(!native::listening_ipv4_ports(u32::MAX).contains(&port));
    }

    #[test]
    fn server_process_requires_an_antigravity_language_server_with_csrf() {
        let command = r"C:\Users\me\AppData\Local\Programs\Antigravity\resources\app\extensions\antigravity\bin\language_server_windows_x64.exe --csrf_token 0a1b-2c --extension_server_port 51234";
        let process = server_process(42, command).unwrap();
        assert_eq!(process.pid, 42);
        assert_eq!(process.csrf, "0a1b-2c");
        assert_eq!(process.declared_port, Some(51234));
        assert!(server_process(42, "language_server_windows_x64.exe --port 1").is_none());
        assert!(server_process(42, "notepad.exe --csrf_token abc").is_none());
    }

    /// #186 的回归场景：未装 Antigravity 时连续快照不得每次都做进程发现；
    /// 负缓存只在过期后才允许再试一次。
    #[test]
    fn negative_cache_suppresses_discovery_until_ttl_expires() {
        let discoveries = std::cell::Cell::new(0u32);
        let discover = || {
            discoveries.set(discoveries.get() + 1);
            None::<String>
        };

        let mut cache: Option<(Instant, Option<String>)> = None;
        // 首次快照真的扫描，落一条新鲜负缓存。
        assert!(cached_endpoint(&mut cache, NEGATIVE_TTL, |_: &String| true, discover).is_none());
        assert_eq!(discoveries.get(), 1);
        // TTL 内的后续快照被负缓存挡住，不再扫描。
        assert!(cached_endpoint(&mut cache, NEGATIVE_TTL, |_: &String| true, discover).is_none());
        assert_eq!(discoveries.get(), 1, "TTL 内不得重复扫描进程");

        // 缓存过期后才再试一次。Instant 不能凭空构造过去时刻，开机不足 TTL
        // 的极端环境跳过这一段（上一段已覆盖核心回归）。
        if let Some(expired) = (Instant::now() - Duration::from_secs(1)).checked_sub(NEGATIVE_TTL) {
            cache = Some((expired, None));
            assert!(
                cached_endpoint(&mut cache, NEGATIVE_TTL, |_: &String| true, discover).is_none()
            );
            assert_eq!(discoveries.get(), 2, "负缓存过期后应重新发现");
        }
    }

    #[test]
    fn positive_cache_avoids_rediscovery_until_endpoint_dies() {
        let discoveries = std::cell::Cell::new(0u32);
        let discover = || {
            discoveries.set(discoveries.get() + 1);
            Some("endpoint-b")
        };
        let mut cache: Option<(Instant, Option<&'static str>)> =
            Some((Instant::now(), Some("endpoint-a")));
        assert_eq!(
            cached_endpoint(&mut cache, NEGATIVE_TTL, |_: &&'static str| true, discover),
            Some("endpoint-a")
        );
        assert_eq!(discoveries.get(), 0, "正缓存验活通过时不得重新发现");

        assert_eq!(
            cached_endpoint(&mut cache, NEGATIVE_TTL, |_: &&'static str| false, discover),
            Some("endpoint-b")
        );
        assert_eq!(discoveries.get(), 1, "端点失活后必须重新发现并替换缓存");
        assert_eq!(cache.as_ref().unwrap().1, Some("endpoint-b"));
    }

    #[test]
    fn parses_generator_metadata_from_retry_infos_with_reasoning_as_output_subitem() {
        let value = json!({
            "generatorMetadata": [{
                "chatModel": {
                    "responseModel": "claude-sonnet-4.6",
                    "chatStartMetadata": { "createdAt": "2026-07-14T05:00:00Z" },
                    "retryInfos": [
                        { "usage": {
                            "inputTokens": 100, "outputTokens": 40,
                            "cacheReadTokens": 900, "thinkingOutputTokens": 12,
                            "responseId": "resp-a"
                        }},
                        { "usage": {
                            "inputTokens": 5, "outputTokens": 8,
                            "cacheReadTokens": 0, "responseId": "resp-b"
                        }}
                    ]
                }
            }]
        });
        let events = normalize_generator_metadata(&value, "cascade-1", i64::MIN);
        assert_eq!(events.len(), 2);
        // processed = input + cache_read + cache_write(0) + output；reasoning 不叠加。
        // 那个 `+ 0` 是 cache_write 这一项，写出来才对得上公式；clippy 的
        // identity_op 只在 --all-targets 下看得到，为它删掉反而丢了意图。
        #[allow(clippy::identity_op)]
        let expected_processed = 100 + 900 + 0 + 40;
        assert_eq!(events[0].tokens.processed(), expected_processed);
        assert_eq!(events[0].tokens.reasoning_output, 12);
        assert_eq!(events[0].tokens.cache_write, 0);
        assert_eq!(events[0].event_key, "response:resp-a");
        assert_eq!(events[0].model.as_deref(), Some("claude-sonnet-4.6"));
        assert_eq!(events[0].session_id, "cascade-1");
        assert_eq!(events[1].event_key, "response:resp-b");
    }

    #[test]
    fn string_encoded_token_counts_are_parsed() {
        // 真机响应形状（本机实测）：token 计数是字符串、无 retryInfos、
        // 直接挂 chatModel.usage，模型是占位符 + responseModel。
        let value = json!({
            "generatorMetadata": [{
                "chatModel": {
                    "model": "MODEL_PLACEHOLDER_M20",
                    "responseModel": "gemini-3-flash-medium-a",
                    "chatStartMetadata": { "createdAt": "2026-07-20T03:00:00Z" },
                    "usage": {
                        "model": "MODEL_PLACEHOLDER_M20",
                        "inputTokens": "21861",
                        "outputTokens": "96",
                        "thinkingOutputTokens": "58",
                        "responseId": "resp-str"
                    }
                }
            }]
        });
        let events = normalize_generator_metadata(&value, "cascade-str", i64::MIN);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.tokens.input_uncached, 21861);
        assert_eq!(event.tokens.output, 96);
        assert_eq!(event.tokens.reasoning_output, 58);
        assert_eq!(event.tokens.processed(), 21861 + 96);
        assert_eq!(event.event_key, "response:resp-str");
    }

    #[test]
    fn placeholder_models_map_to_real_names_but_unknown_stay_verbatim() {
        assert_eq!(
            resolve_model(Some("MODEL_PLACEHOLDER_M37".into())).as_deref(),
            Some("gemini-3.1-pro")
        );
        // 未知占位符原样保留，不丢弃、不猜。
        assert_eq!(
            resolve_model(Some("model_placeholder_m84".into())).as_deref(),
            Some("model_placeholder_m84")
        );
        assert_eq!(resolve_model(Some("".into())), None);
    }

    #[test]
    fn quota_summary_parses_both_fraction_shapes() {
        let value = json!({
            "groups": [{
                "displayName": "Gemini Models",
                "buckets": [
                    { "bucketId": "gemini_weekly", "remainingFraction": 0.4, "resetTime": "2026-07-20T00:00:00Z" },
                    { "bucketId": "gemini_5h", "remaining": { "remainingFraction": 0.75 } },
                    { "bucketId": "disabled_one", "remainingFraction": 0.1, "disabled": true }
                ]
            }]
        });
        let samples = parse_quota_summary(&value);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].window_key, "gemini_weekly");
        assert!((samples[0].remaining_percent - 40.0).abs() < 1e-9);
        assert_eq!(samples[1].window_key, "gemini_5h");
        assert!((samples[1].remaining_percent - 75.0).abs() < 1e-9);
    }

    /// Antigravity 2.23 的 RetrieveUserQuotaSummary 真机响应形状：外层包一层
    /// `response`，桶名是连字符，要归一成与 statusLine 钩子相同的窗口键。
    #[test]
    fn quota_summary_normalizes_hyphenated_bucket_ids() {
        let value = json!({ "response": { "groups": [
            { "displayName": "Gemini Models", "buckets": [
                { "bucketId": "gemini-weekly", "displayName": "Weekly Limit Remaining",
                  "window": "weekly", "remainingFraction": 1, "resetTime": "2026-10-20T00:00:00Z" },
                { "bucketId": "gemini-5h", "window": "5h", "remainingFraction": 0.5 }
            ]},
            { "displayName": "Claude and GPT models", "buckets": [
                { "bucketId": "3p-5h", "window": "5h", "remainingFraction": 0.25 }
            ]}
        ]}});
        let keys: Vec<String> = parse_quota_summary(&value)
            .into_iter()
            .map(|sample| sample.window_key)
            .collect();
        assert_eq!(keys, ["gemini_weekly", "gemini_5h", "3p_5h"]);
    }

    #[test]
    fn trajectory_summaries_accept_array_and_object_forms() {
        let as_array = json!({ "trajectorySummaries": [
            { "cascadeId": "a", "lastModifiedTime": 1784000000000i64, "stepCount": 3 }
        ]});
        let as_object = json!({ "trajectorySummaries": {
            "a": { "cascadeId": "a", "lastModifiedTime": 1784000000000i64, "stepCount": 3 }
        }});
        assert_eq!(normalize_trajectory_summaries(&as_array).len(), 1);
        assert_eq!(normalize_trajectory_summaries(&as_object).len(), 1);
        assert_eq!(normalize_trajectory_summaries(&as_array)[0].id, "a");
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "metrik-antigravity-{label}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            std::fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn pb_varint_value(mut value: u64, out: &mut Vec<u8>) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    fn pb_varint(number: u64, value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        pb_varint_value(number << 3, &mut out);
        pb_varint_value(value, &mut out);
        out
    }

    fn pb_bytes(number: u64, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        pb_varint_value((number << 3) | 2, &mut out);
        pb_varint_value(payload.len() as u64, &mut out);
        out.extend_from_slice(payload);
        out
    }

    /// 真实库里的 ModelUsageStats 形状：1 模型枚举、7 messageId、8 响应头都在，
    /// 解析时必须被安全跳过。
    fn usage_blob(input: u64, cache_read: u64, thinking: u64, response: u64, id: &str) -> Vec<u8> {
        let mut usage = [
            pb_varint(1, 1318),
            pb_varint(2, input),
            pb_varint(3, thinking + response),
        ]
        .concat();
        if cache_read > 0 {
            usage.extend(pb_varint(5, cache_read));
        }
        usage.extend(pb_varint(6, 24));
        usage.extend(pb_bytes(7, b"bot-0000"));
        usage.extend(pb_bytes(
            8,
            &[pb_bytes(1, b"sessionID"), pb_bytes(2, b"-1")].concat(),
        ));
        usage.extend(pb_varint(9, thinking));
        usage.extend(pb_varint(10, response));
        usage.extend(pb_bytes(11, id.as_bytes()));
        usage
    }

    /// 失败的重试：usage 只带零值字段（proto3 不写零），这里给个空消息。
    fn generation_blob(steps: &[u64], model: &str, retries: &[Vec<u8>]) -> Vec<u8> {
        let mut chat_model = pb_varint(3, 1318);
        if let Some(last) = retries.last() {
            chat_model.extend(pb_bytes(4, last));
        }
        for usage in retries {
            chat_model.extend(pb_bytes(17, &pb_bytes(2, usage)));
        }
        chat_model.extend(pb_bytes(19, model.as_bytes()));
        let mut packed = Vec::new();
        for step in steps {
            pb_varint_value(*step, &mut packed);
        }
        [
            pb_bytes(1, &chat_model),
            pb_bytes(2, &packed),
            pb_bytes(4, b"00000000-0000-0000-0000-000000000000"),
        ]
        .concat()
    }

    fn step_blob(seconds: u64, nanos: u64) -> Vec<u8> {
        [
            pb_bytes(1, &[pb_varint(1, seconds), pb_varint(2, nanos)].concat()),
            pb_varint(3, 2),
        ]
        .concat()
    }

    /// 只建用得到的表与列，形状取自 Antigravity 2.23 的会话库。
    fn write_conversation(path: &Path, steps: &[(i64, Vec<u8>)], generations: &[Vec<u8>]) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE steps (idx integer, step_type integer NOT NULL DEFAULT 0,
                     metadata blob, PRIMARY KEY (idx));
                 CREATE TABLE gen_metadata (idx integer, data blob,
                     size integer NOT NULL DEFAULT 0, PRIMARY KEY (idx));",
            )
            .unwrap();
        for (index, metadata) in steps {
            connection
                .execute(
                    "INSERT INTO steps (idx, step_type, metadata) VALUES (?1, 15, ?2)",
                    rusqlite::params![index, metadata],
                )
                .unwrap();
        }
        for (index, data) in generations.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO gen_metadata (idx, data, size) VALUES (?1, ?2, ?3)",
                    rusqlite::params![index as i64, data, data.len() as i64],
                )
                .unwrap();
        }
    }

    #[test]
    fn cli_database_reads_usage_by_model_with_the_step_timestamp() {
        let test = TestDirectory::new("local");
        let conversations = test.0.join("antigravity-cli").join("conversations");
        std::fs::create_dir_all(&conversations).unwrap();
        let path = conversations.join("cascade-local.db");
        let created = 1_791_239_681u64;
        write_conversation(
            &path,
            &[
                (1, step_blob(created, 795_389_800)),
                (3, step_blob(created + 6, 0)),
            ],
            &[
                // CLI can record a failed attempt with only a model enum in usage
                // and no resulting step.
                pb_bytes(1, &pb_bytes(17, &pb_bytes(2, &pb_varint(1, 1318)))),
                generation_blob(
                    &[1, 2],
                    "gemini-3.8-flash",
                    &[usage_blob(13_470, 0, 265, 74, "resp-a")],
                ),
                // 两次失败重试（零用量）后成功：只记成功那次。
                generation_blob(
                    &[3, 4],
                    "claude-sonnet-4-6",
                    &[
                        Vec::new(),
                        Vec::new(),
                        usage_blob(6_336, 24_437, 50, 81, "resp-b"),
                    ],
                ),
            ],
        );
        let adapter = AntigravityAdapter::from_gemini_dir(&test.0);
        let databases = adapter.local_databases();
        assert_eq!(databases, [path]);
        let candidates = local_conversation_candidate(&databases[0], 0);
        let scan = adapter.parse(&candidates[0], 0).unwrap();

        assert!(!scan.diagnostics.is_partial(), "{:?}", scan.diagnostics);
        assert_eq!(scan.source.logical_key, "cascade-local");
        let events = &scan.source.events;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_key, "response:resp-a");
        assert_eq!(events[0].session_id, "cascade-local");
        assert_eq!(events[0].model.as_deref(), Some("gemini-3.8-flash"));
        assert_eq!(events[0].occurred_at_ms, created as i64 * 1000 + 795);
        assert_eq!(events[0].tokens.input_uncached, 13_470);
        assert_eq!(events[0].tokens.output, 339);
        assert_eq!(events[0].tokens.reasoning_output, 265);
        assert_eq!(events[0].tokens.processed(), 13_470 + 339);
        assert_eq!(events[1].event_key, "response:resp-b");
        assert_eq!(events[1].model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(events[1].occurred_at_ms, (created as i64 + 6) * 1000);
        assert_eq!(events[1].tokens.cache_read, 24_437);
        assert_eq!(events[1].tokens.processed(), 6_336 + 24_437 + 131);
    }

    /// 同一次生成从本地库和 RPC 读到，必须落成同一个事件（同 event_id、同分量），
    /// 账本才会合并而不是重复计数。
    #[test]
    fn local_and_rpc_observations_of_a_generation_are_the_same_event() {
        let test = TestDirectory::new("parity");
        let path = test.0.join("cascade-parity.db");
        write_conversation(
            &path,
            &[(1, step_blob(1_791_239_681, 0))],
            &[generation_blob(
                &[1],
                "gemini-3.8-flash",
                &[usage_blob(100, 900, 12, 28, "resp-same")],
            )],
        );
        let (_, local, _) = parse_local_database(&path, 0).unwrap();
        let rpc = normalize_generator_metadata(
            &json!({ "generatorMetadata": [{ "chatModel": {
                "responseModel": "gemini-3.8-flash",
                "chatStartMetadata": { "createdAt": "2026-10-05T22:34:41Z" },
                "retryInfos": [{ "usage": {
                    "inputTokens": "100", "outputTokens": "40", "cacheReadTokens": "900",
                    "thinkingOutputTokens": "12", "responseOutputTokens": "28",
                    "responseId": "resp-same"
                }}]
            }}]}),
            "cascade-parity",
            0,
        );
        assert_eq!(local.len(), 1);
        assert_eq!(rpc.len(), 1);
        assert_eq!(local[0].event_id, rpc[0].event_id);
        assert!(local[0].same_measured_payload(&rpc[0]));
    }

    #[test]
    fn unreadable_rows_and_output_split_mismatches_mark_the_source_partial() {
        let test = TestDirectory::new("partial");
        let path = test.0.join("cascade-partial.db");
        // output 99 与 thinking 5 + response 5 对不上。
        let contradictory = [
            pb_varint(2, 10),
            pb_varint(3, 99),
            pb_varint(9, 5),
            pb_varint(10, 5),
            pb_bytes(11, b"resp-bad"),
        ]
        .concat();
        write_conversation(
            &path,
            &[(1, step_blob(1_791_239_681, 0))],
            &[
                // 截断的字节：不能读出一个看似合理的数。
                vec![0x0a, 0x10, 0x01],
                // 找不到产出 step 的时间：无法归日。
                generation_blob(&[7], "gemini-3.8-flash", &[usage_blob(1, 0, 1, 1, "x")]),
                // Token 计数不可读，无法确认是无用量的尝试。
                pb_bytes(1, &pb_bytes(4, &pb_bytes(2, b"not a counter"))),
                generation_blob(&[1], "gemini-3.8-flash", &[contradictory]),
            ],
        );
        let (_, events, diagnostics) = parse_local_database(&path, 0).unwrap();
        assert_eq!(diagnostics.malformed_lines, 3);
        assert_eq!(diagnostics.total_mismatches, 1);
        assert!(diagnostics.is_partial());
        // 能读的那行仍然入账。
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_key, "response:resp-bad");
    }

    #[test]
    fn malformed_nested_messages_keep_valid_retries_at_their_original_positions() {
        let test = TestDirectory::new("nested-partial");
        let path = test.0.join("cascade-nested.db");
        let generation = |chat: &[u8]| [pb_bytes(1, chat), pb_bytes(2, &[1])].concat();
        let fallback_usage = usage_blob(100, 0, 0, 1, "top-level-must-not-count");
        let no_id_usage = [pb_varint(2, 5), pb_varint(3, 1), pb_varint(10, 1)].concat();
        let mixed_retries = [
            pb_bytes(4, &fallback_usage),
            // retry 消息自身截断。
            pb_bytes(17, &[0x12, 0x05, 0x01]),
            // 没有 usage 的失败重试合法，仍然占据原位置。
            pb_bytes(17, &[]),
            pb_bytes(17, &pb_bytes(2, &no_id_usage)),
            // retry 可读、内嵌 usage 截断，也必须单独标 partial。
            pb_bytes(17, &pb_bytes(2, &[0x80])),
        ]
        .concat();
        let all_corrupt_retries = [
            pb_bytes(4, &fallback_usage),
            pb_bytes(17, &[0x12, 0x05, 0x01]),
        ]
        .concat();
        write_conversation(
            &path,
            &[(1, step_blob(1_791_239_681, 0))],
            &[
                // chatModel 存在，但内容截断；不是正常的非聊天生成。
                pb_bytes(1, &[0x0a, 0x05, 0x01]),
                // 顶层 usage 截断。
                generation(&pb_bytes(4, &[0x80])),
                generation(&mixed_retries),
                generation(&all_corrupt_retries),
                // 非聊天生成与空 usage 的失败重试不能误报。
                Vec::new(),
                generation(&pb_bytes(17, &pb_bytes(2, &[]))),
            ],
        );

        let (_, events, diagnostics) = parse_local_database(&path, 0).unwrap();
        assert_eq!(diagnostics.malformed_lines, 5);
        assert_eq!(diagnostics.total_mismatches, 0);
        assert!(diagnostics.is_partial());
        assert_eq!(events.len(), 1);
        // 前面的损坏行和重试都不能挤掉下标、改变无 responseId 事件的身份。
        assert_eq!(events[0].event_key, "response:cascade-nested:2:2");
        assert_eq!(events[0].tokens.processed(), 6);
    }

    #[test]
    fn wrong_wire_types_for_nested_messages_are_not_treated_as_absent() {
        let test = TestDirectory::new("wrong-wire");
        let path = test.0.join("cascade-wrong-wire.db");
        let generation = |chat: &[u8]| [pb_bytes(1, chat), pb_bytes(2, &[1])].concat();
        write_conversation(
            &path,
            &[(1, step_blob(1_791_239_681, 0))],
            &[
                pb_varint(1, 1),
                generation(&pb_varint(4, 1)),
                generation(&pb_varint(17, 1)),
                generation(&pb_bytes(17, &pb_varint(2, 1))),
            ],
        );
        let (_, events, diagnostics) = parse_local_database(&path, 0).unwrap();
        assert!(events.is_empty());
        assert_eq!(diagnostics.malformed_lines, 4);
        assert!(diagnostics.is_partial());
    }

    #[test]
    fn output_split_check_includes_proto3_omitted_zero_components() {
        let test = TestDirectory::new("omitted-zero");
        let path = test.0.join("cascade-zero.db");
        let thinking_only = [pb_varint(3, 5), pb_varint(9, 5), pb_bytes(11, b"thinking")].concat();
        let response_only = [pb_varint(3, 5), pb_varint(10, 5), pb_bytes(11, b"response")].concat();
        let wrong_thinking = [
            pb_varint(3, 99),
            pb_varint(9, 5),
            pb_bytes(11, b"wrong-thinking"),
        ]
        .concat();
        let wrong_zero = [pb_varint(3, 5), pb_bytes(11, b"wrong-zero")].concat();
        write_conversation(
            &path,
            &[(1, step_blob(1_791_239_681, 0))],
            &[
                generation_blob(&[1], "m", &[thinking_only]),
                generation_blob(&[1], "m", &[response_only]),
                generation_blob(&[1], "m", &[Vec::new()]),
                generation_blob(&[1], "m", &[wrong_thinking]),
                generation_blob(&[1], "m", &[wrong_zero]),
            ],
        );
        let (_, events, diagnostics) = parse_local_database(&path, 0).unwrap();
        assert_eq!(diagnostics.malformed_lines, 0);
        assert_eq!(diagnostics.total_mismatches, 2);
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].event_key, "response:thinking");
        assert_eq!(events[0].tokens.output, 5);
        assert_eq!(events[0].tokens.reasoning_output, 5);
        assert_eq!(events[1].event_key, "response:response");
        assert_eq!(events[1].tokens.reasoning_output, 0);
    }

    #[test]
    fn malformed_packed_step_indices_do_not_use_the_valid_prefix() {
        let test = TestDirectory::new("packed-partial");
        let path = test.0.join("cascade-packed.db");
        let usage = usage_blob(1, 0, 0, 1, "truncated-indices");
        let truncated = [pb_bytes(1, &pb_bytes(4, &usage)), pb_bytes(2, &[1, 0x80])].concat();
        write_conversation(
            &path,
            &[(1, step_blob(1_791_239_681, 0))],
            &[
                truncated,
                generation_blob(&[1], "m", &[usage_blob(1, 0, 0, 1, "valid-indices")]),
            ],
        );
        let (_, events, diagnostics) = parse_local_database(&path, 0).unwrap();
        assert_eq!(diagnostics.malformed_lines, 1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_key, "response:valid-indices");
    }

    #[test]
    fn wal_commit_between_table_reads_is_visible_only_to_the_next_scan() {
        let test = TestDirectory::new("wal-snapshot");
        let path = test.0.join("cascade-wal.db");
        let created = 1_791_239_681;
        write_conversation(
            &path,
            &[(1, step_blob(created, 0))],
            &[generation_blob(
                &[1],
                "m",
                &[usage_blob(1, 0, 0, 1, "before")],
            )],
        );
        let mut writer = Connection::open(&path).unwrap();
        writer.busy_timeout(Duration::from_secs(2)).unwrap();
        let (_, initial_events, diagnostics) =
            parse_local_database_with_step_read(&path, 0, || {
                let transaction = writer.transaction().unwrap();
                transaction
                    .execute(
                        "INSERT INTO steps (idx, step_type, metadata) VALUES (2, 15, ?1)",
                        [step_blob(created + 1, 0)],
                    )
                    .unwrap();
                let data = generation_blob(&[2], "m", &[usage_blob(2, 0, 0, 1, "after")]);
                transaction
                    .execute(
                        "INSERT INTO gen_metadata (idx, data, size) VALUES (1, ?1, ?2)",
                        rusqlite::params![&data, data.len() as i64],
                    )
                    .unwrap();
                // WAL 中即使读事务还没结束，也允许另一个连接提交。
                transaction.commit().unwrap();
            })
            .unwrap();
        assert!(!diagnostics.is_partial(), "{diagnostics:?}");
        assert_eq!(initial_events.len(), 1);
        assert_eq!(initial_events[0].event_key, "response:before");

        let (_, next_events, diagnostics) = parse_local_database(&path, 0).unwrap();
        assert!(!diagnostics.is_partial(), "{diagnostics:?}");
        assert_eq!(next_events.len(), 2);
        assert_eq!(next_events[1].event_key, "response:after");
        assert_eq!(next_events[1].occurred_at_ms, (created + 1) as i64 * 1000);
    }

    #[test]
    fn local_rows_before_the_cutoff_are_dropped() {
        let test = TestDirectory::new("cutoff");
        let path = test.0.join("cascade-cutoff.db");
        write_conversation(
            &path,
            &[
                (1, step_blob(1_791_000_000, 0)),
                (3, step_blob(1_791_100_000, 0)),
            ],
            &[
                generation_blob(&[1], "m", &[usage_blob(1, 0, 0, 1, "old")]),
                generation_blob(&[3], "m", &[usage_blob(1, 0, 0, 1, "new")]),
            ],
        );
        let (_, events, _) = parse_local_database(&path, 1_791_050_000_000).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_key, "response:new");
    }

    /// 真实布局：`<数据根>/conversations/<id>.db` 与 `<数据根>/conversation_summaries.db`。
    #[test]
    fn local_database_takes_the_project_from_conversation_summaries() {
        let test = TestDirectory::new("project");
        let conversations = test.0.join("conversations");
        std::fs::create_dir_all(&conversations).unwrap();
        let generation =
            |id: &str| generation_blob(&[1], "gemini-3.8-flash", &[usage_blob(10, 0, 1, 1, id)]);
        for (cascade, response) in [("with-workspace", "resp-1"), ("no-summary", "resp-2")] {
            write_conversation(
                &conversations.join(format!("{cascade}.db")),
                &[(1, step_blob(1_791_239_681, 0))],
                &[generation(response)],
            );
        }

        // 摘要库还不存在（新装、或摘要晚于会话落盘）：用量照常入账，只是未归属。
        let (_, events, _) =
            parse_local_database(&conversations.join("with-workspace.db"), 0).unwrap();
        assert_eq!(events[0].project_path, None);

        let summaries = Connection::open(test.0.join("conversation_summaries.db")).unwrap();
        summaries
            .execute_batch(
                "CREATE TABLE conversation_summaries (conversation_id text,
                     workspace_uris text NOT NULL, PRIMARY KEY (conversation_id));
                 INSERT INTO conversation_summaries VALUES
                     ('with-workspace', '[\"file:///d%3A/work/%E9%A1%B9%E7%9B%AE%20A\"]');",
            )
            .unwrap();
        drop(summaries);

        let (_, events, diagnostics) =
            parse_local_database(&conversations.join("with-workspace.db"), 0).unwrap();
        assert!(!diagnostics.is_partial());
        // 盘符按账本约定统一大写，与其他 Agent 报的同一目录归到同一项目。
        assert_eq!(events[0].project_path.as_deref(), Some("D:/work/项目 A"));
        let (_, events, _) = parse_local_database(&conversations.join("no-summary.db"), 0).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].project_path, None);
    }

    #[test]
    fn summary_creation_and_wal_updates_invalidate_cached_conversations() {
        let test = TestDirectory::new("project-cache");
        let conversations = test.0.join("conversations");
        std::fs::create_dir_all(&conversations).unwrap();
        let path = conversations.join("late-summary.db");
        write_conversation(
            &path,
            &[(1, step_blob(1_791_239_681, 0))],
            &[generation_blob(
                &[1],
                "m",
                &[usage_blob(10, 0, 1, 1, "resp-late")],
            )],
        );
        // 保持会话库侧的 WAL 文件稳定，且让它的 mtime 始终晚于摘要库。
        let conversation_writer = Connection::open(&path).unwrap();
        conversation_writer
            .execute_batch("SELECT COUNT(*) FROM gen_metadata;")
            .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::SystemTime::now() + Duration::from_secs(86_400)),
            )
            .unwrap();
        let adapter = AntigravityAdapter::with_conversation_dirs(vec![conversations]);
        let candidate = || local_conversation_candidate(&path, 0).remove(0);
        let is_current = |ledger: &Connection, source: &SourceCandidate| {
            crate::storage::source_is_current(
                ledger,
                &source.source_id,
                source.size,
                source.mtime_ns,
                0,
            )
            .unwrap()
        };
        let mut ledger = Connection::open_in_memory().unwrap();
        ledger
            .execute_batch(include_str!("../../migrations/001_init.sql"))
            .unwrap();

        let missing = candidate();
        let original = adapter.parse(&missing, 0).unwrap();
        assert_eq!(original.source.events[0].project_path, None);
        crate::storage::replace_source(&mut ledger, &original.source, 0).unwrap();
        assert!(is_current(&ledger, &candidate()));

        let summaries_path = conversation_summaries_path(&path).unwrap();
        let summaries = Connection::open(&summaries_path).unwrap();
        summaries
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 CREATE TABLE conversation_summaries (conversation_id text PRIMARY KEY,
                     workspace_uris text NOT NULL);",
            )
            .unwrap();
        let empty = candidate();
        assert_eq!(empty.source_id, missing.source_id);
        assert_eq!(empty.mtime_ns, missing.mtime_ns);
        assert!(!is_current(&ledger, &empty));
        let no_row = adapter.parse(&empty, 0).unwrap();
        assert_eq!(no_row.source.events[0].project_path, None);
        crate::storage::replace_source(&mut ledger, &no_row.source, 0).unwrap();
        assert!(is_current(&ledger, &candidate()));

        let main_before = std::fs::metadata(&summaries_path).unwrap();
        summaries
            .execute(
                "INSERT INTO conversation_summaries VALUES (?1, ?2)",
                rusqlite::params!["late-summary", r#"["file:///d%3A/work/project%20A"]"#],
            )
            .unwrap();
        let main_after = std::fs::metadata(&summaries_path).unwrap();
        assert_eq!(main_before.len(), main_after.len());
        assert_eq!(
            main_before.modified().unwrap(),
            main_after.modified().unwrap()
        );
        let with_row = candidate();
        assert_eq!(with_row.mtime_ns, missing.mtime_ns);
        assert!(!is_current(&ledger, &with_row));
        let attributed = adapter.parse(&with_row, 0).unwrap();
        let before = &original.source.events[0];
        let after = &attributed.source.events[0];
        assert_eq!(after.project_path.as_deref(), Some("D:/work/project A"));
        assert_eq!(after.event_id, before.event_id);
        assert!(after.same_measured_payload(before));
        assert_eq!(after.tokens, before.tokens);
        crate::storage::replace_source(&mut ledger, &attributed.source, 0).unwrap();
        // 读摘要 WAL 可能改写 SHM；这种读取不能让来源每轮都重新解析。
        assert!(is_current(&ledger, &candidate()));
        let stored_project: Option<String> = ledger
            .query_row("SELECT project_path FROM usage_event", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stored_project.as_deref(), Some("D:/work/project A"));

        drop(summaries);
        std::fs::remove_file(summaries_path).unwrap();
        assert!(!is_current(&ledger, &candidate()));
        // 摘要的变动不能扩大保留期：cutoff 仍只检查会话库自身。
        assert!(local_conversation_candidate(&path, missing.mtime_ns / 1_000_000 + 1).is_empty());
    }

    #[test]
    fn empty_summary_wal_reads_keep_cached_usage_current_but_real_writes_do_not() {
        let test = TestDirectory::new("empty-summary-wal");
        let conversations = test.0.join("conversations");
        std::fs::create_dir_all(&conversations).unwrap();
        let path = conversations.join("cached.db");
        write_conversation(
            &path,
            &[(1, step_blob(1_791_239_681, 0))],
            &[generation_blob(
                &[1],
                "m",
                &[usage_blob(10, 0, 1, 1, "resp-cached")],
            )],
        );
        let conversation_reader = Connection::open(&path).unwrap();
        conversation_reader
            .execute_batch("SELECT COUNT(*) FROM gen_metadata;")
            .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::SystemTime::now() + Duration::from_secs(86_400)),
            )
            .unwrap();

        let summaries_path = conversation_summaries_path(&path).unwrap();
        let summaries = Connection::open(&summaries_path).unwrap();
        summaries
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE conversation_summaries (conversation_id text PRIMARY KEY,
                     workspace_uris text NOT NULL);
                 INSERT INTO conversation_summaries VALUES
                     ('cached', '[\"file:///d%3A/work/original\"]');",
            )
            .unwrap();
        drop(summaries);
        let mut wal_path = summaries_path.as_os_str().to_os_string();
        wal_path.push("-wal");
        let wal_path = PathBuf::from(wal_path);
        assert!(!wal_path.exists());
        let candidate = || local_conversation_candidate(&path, 0).remove(0);
        let adapter = AntigravityAdapter::with_conversation_dirs(vec![conversations]);
        let mut ledger = Connection::open_in_memory().unwrap();
        ledger
            .execute_batch(include_str!("../../migrations/001_init.sql"))
            .unwrap();
        let initial = candidate();
        let scan = adapter.parse(&initial, 0).unwrap();
        crate::storage::replace_source(&mut ledger, &scan.source, 0).unwrap();
        let is_current = |ledger: &Connection| {
            let next = candidate();
            crate::storage::source_is_current(ledger, &next.source_id, next.size, next.mtime_ns, 0)
                .unwrap()
        };

        // 真实的只读打开创建空 WAL；下一次发现不能因此重复读取所有会话。
        assert_eq!(std::fs::metadata(&wal_path).unwrap().len(), 0);
        assert!(is_current(&ledger));
        std::fs::remove_file(&wal_path).unwrap();
        assert!(is_current(&ledger));
        let empty_wal = std::fs::File::create(&wal_path).unwrap();
        empty_wal
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::SystemTime::now() + Duration::from_secs(172_800)),
            )
            .unwrap();
        drop(empty_wal);
        assert!(is_current(&ledger));

        let summaries = Connection::open(&summaries_path).unwrap();
        summaries
            .execute_batch("PRAGMA wal_autocheckpoint = 0;")
            .unwrap();
        let main_before = std::fs::metadata(&summaries_path).unwrap();
        summaries
            .execute(
                "UPDATE conversation_summaries SET workspace_uris = ?1 WHERE conversation_id = ?2",
                rusqlite::params![r#"["file:///d%3A/work/renamed"]"#, "cached"],
            )
            .unwrap();
        let main_after = std::fs::metadata(&summaries_path).unwrap();
        assert_eq!(main_before.len(), main_after.len());
        assert_eq!(
            main_before.modified().unwrap(),
            main_after.modified().unwrap()
        );
        assert!(std::fs::metadata(&wal_path).unwrap().len() > 0);
        assert!(!is_current(&ledger));
        let updated = adapter.parse(&candidate(), 0).unwrap();
        assert_eq!(
            updated.source.events[0].project_path.as_deref(),
            Some("D:/work/renamed")
        );
        assert_eq!(
            updated.source.events[0].event_id,
            scan.source.events[0].event_id
        );
        assert_eq!(
            updated.source.events[0].tokens,
            scan.source.events[0].tokens
        );
        crate::storage::replace_source(&mut ledger, &updated.source, 0).unwrap();
        assert!(is_current(&ledger));
        let stored_project: String = ledger
            .query_row("SELECT project_path FROM usage_event", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            stored_project, "D:/work/original",
            "已记录的项目归属不能改写"
        );
    }

    #[test]
    fn workspace_uris_decode_to_local_paths_and_never_guess() {
        assert_eq!(
            workspace_path(r#"["file:///c%3A/Users/me/DAVID%27s%20%E5%B0%B1%E4%B8%9A"]"#)
                .as_deref(),
            Some("c:/Users/me/DAVID's 就业")
        );
        assert_eq!(
            workspace_path(r#"["file:///home/me/proj"]"#).as_deref(),
            Some("/home/me/proj")
        );
        // 多根：跳过非本机的根，取第一个本机 file URI。
        assert_eq!(
            workspace_path(r#"["vscode-remote://wsl+Ubuntu/home/me", "file:///Users/me/a"]"#)
                .as_deref(),
            Some("/Users/me/a")
        );
        assert_eq!(workspace_path(r#"["file://server/share/proj"]"#), None);
        assert_eq!(workspace_path(r#"["file:///c%3A/bad%2"]"#), None);
        assert_eq!(workspace_path(r#"["file:///c%3A/bad%+1"]"#), None);
        assert_eq!(workspace_path(r#"["file:///c%3A/%FF"]"#), None);
        assert_eq!(workspace_path("[]"), None);
        assert_eq!(workspace_path(""), None);
    }

    #[test]
    fn workspace_uris_reject_ambiguous_roots_and_try_the_next_recorded_local_path() {
        let invalid = [
            "file://server/share/project",
            "file:////server/share/project",
            "file://///server/share/project",
            "file:///%2Fserver/share/project",
            "file:///%5C%5Cserver/share/project",
            "file:///c%3A/work/%5Cserver",
            "file:///c%3A/work/project#fragment",
            "file:///home/user/project?query",
            "file:///c%3A/work/%00",
            "file:///home/user/%0Aproject",
            "file:///home/user/%7Fproject",
            "file:///home/user/%C2%85project",
            "file:///c:/work/\0project",
            "file:///home/user/\nproject",
            "file:///c:/work/\\project",
            "file:///c:relative",
            "file:///c%3A/bad%2",
            "file:///c%3A/%FF",
        ];
        for uri in invalid {
            assert_eq!(workspace_path(&json!([uri]).to_string()), None, "{uri:?}");
            assert_eq!(
                workspace_path(&json!([uri, "file:///d%3A/work/valid"]).to_string()).as_deref(),
                Some("d:/work/valid"),
                "{uri:?} 不能挡住后面的合法本地根"
            );
        }
        for (uri, expected) in [
            ("file:///", "/"),
            ("file:///c%3A/", "c:/"),
            ("file:///home/user/a%23b%3Fc", "/home/user/a#b?c"),
            ("file:///home/user/%252Fproject", "/home/user/%2Fproject"),
            ("file:///d%3A/work/%E9%A1%B9%E7%9B%AE%20A", "d:/work/项目 A"),
        ] {
            assert_eq!(
                workspace_path(&json!([uri]).to_string()).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn local_databases_skip_wal_sidecars_and_other_files() {
        let test = TestDirectory::new("discover");
        for name in [
            "b.db",
            "a.db",
            "a.db-wal",
            "a.db-shm",
            "old.pb",
            "notes.txt",
        ] {
            std::fs::write(test.0.join(name), b"").unwrap();
        }
        let adapter =
            AntigravityAdapter::with_conversation_dirs(vec![test.0.clone(), test.0.join("absent")]);
        let names: Vec<String> = adapter
            .local_databases()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.db", "b.db"]);
    }

    #[test]
    fn rpc_skips_cascades_that_have_a_local_database() {
        let list = json!({ "trajectorySummaries": [
            { "cascadeId": "on-disk", "lastModifiedTime": 1784000000000i64, "stepCount": 3 },
            { "cascadeId": "rpc-only", "lastModifiedTime": 1784000000000i64, "stepCount": 5 }
        ]});
        let local_ids: HashSet<&str> = ["on-disk"].into_iter().collect();
        let candidates = rpc_candidates(&list, &local_ids);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].path,
            PathBuf::from("antigravity://cascade/rpc-only")
        );
        assert_eq!(candidates[0].size, 5);
    }

    #[test]
    fn proto_reader_rejects_truncated_input_and_reads_packed_and_plain_repeats() {
        assert!(ProtoMessage::parse(&[0x0a, 0x05, 0x01]).is_none());
        assert!(ProtoMessage::parse(&[0xff, 0xff]).is_none());
        let bytes = [
            pb_bytes(2, &[0x01, 0x96, 0x01]),
            pb_varint(2, 7),
            pb_varint(9, 3),
        ]
        .concat();
        let message = ProtoMessage::parse(&bytes).unwrap();
        assert_eq!(message.repeated_varints(2), Some(vec![1u64, 150, 7]));
        assert_eq!(message.varint(9), Some(3));
        assert_eq!(message.varint(8), None);
        assert_eq!(
            ProtoMessage::parse(&pb_bytes(2, &[1, 0x80]))
                .unwrap()
                .repeated_varints(2),
            None
        );
    }

    #[test]
    fn proto_reader_rejects_varint_overflow_and_invalid_field_numbers() {
        let mut max = vec![0xff; 9];
        max.push(1);
        assert_eq!(read_varint(&max, &mut 0), Some(u64::MAX));
        max[9] = 2;
        assert_eq!(read_varint(&max, &mut 0), None);
        assert!(ProtoMessage::parse(&pb_varint(0, 1)).is_none());
        assert!(ProtoMessage::parse(&pb_varint(1 << 29, 1)).is_none());
    }

    #[test]
    fn command_line_parsing_extracts_csrf_and_port() {
        let cmd = "language_server_windows_x64.exe --app_data_dir antigravity \
                   --csrf_token 1a2b3c4d-5e6f-7890-abcd-ef0123456789 --extension_server_port 51234";
        assert!(is_antigravity_command(cmd));
        assert_eq!(
            extract_csrf(cmd).as_deref(),
            Some("1a2b3c4d-5e6f-7890-abcd-ef0123456789")
        );
        assert_eq!(extract_declared_port(cmd), Some(51234));
    }
}
