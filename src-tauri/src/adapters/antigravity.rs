//! Google Antigravity（Codeium/Windsurf 血统的 agentic IDE）用量。
//!
//! 与其他 adapter 不同：Antigravity 不落本地日志，用量只存在于本机
//! `language_server` 进程的私有 RPC 后面（`exa.language_server_pb`，自签 TLS，
//! 认证令牌从进程命令行取）。因此本 adapter 是一个轮询 RPC 客户端，IDE 必须
//! 正在运行才能读到；进程不在时返回空（显示为 0），绝不估算。
//!
//! 套进 `AgentAdapter` 框架：`discover` = 列出会话（cascade）+ 一个配额入口，
//! `parse` = 取该会话的用量记录。账本按 `responseId` 去重——RPC 每次返回该
//! 会话的**全量**记录，活跃生成会被多次观察到、计数逐步增长，正是账本已有的
//! 「按 provider id 合并、分量取最大值」语义（与 Claude 同型，见 storage.rs）。
//!
//! 口径注记（未在真机验证，上线前须核对）：
//! - `cacheReadTokens` 有；cache write 无证据存在，恒记 0 并如实标注。
//! - `thinkingOutputTokens` 作为 output 的子项（reasoning_output），不重复计入
//!   processed。参考实现 tokscale 假设 output 不含 thinking 而分开相加；本项目
//!   口径相反。若真机总量对不上，改这里而不是账单口径。
//! - 模型名占位符（`model_placeholder_m37` 等）查表转真名，查不到原样保留。

use super::{AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{stable_hash, ParsedSource, QuotaSample, TokenVector, UsageEvent};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const RPC_BASE_METHOD: &str = "/exa.language_server_pb.LanguageServerService";
const QUOTA_SOURCE_ID: &str = "antigravity-quota";

pub struct AntigravityAdapter {
    // 端点发现（进程扫描 + 端口探测）昂贵；缓存结果，含"没找到"的负缓存，
    // 避免未安装 Antigravity 时每次快照都 spawn 一次 PowerShell。
    endpoint: Mutex<Option<(Instant, Option<Endpoint>)>>,
}

#[derive(Clone)]
struct Endpoint {
    base_url: String,
    csrf: String,
}

// 未找到时的冷却，避免高频进程扫描。
const NEGATIVE_TTL: Duration = Duration::from_secs(45);

impl AntigravityAdapter {
    pub fn detected() -> Self {
        Self {
            endpoint: Mutex::new(None),
        }
    }

    /// 解析可用端点：命中缓存直接用；否则发现进程、取 csrf、探测监听端口。
    fn endpoint(&self) -> Option<Endpoint> {
        let mut guard = self.endpoint.lock().ok()?;
        if let Some((captured, cached)) = guard.as_ref() {
            match cached {
                // 缓存的端点用一次 Heartbeat 验活，失活则重新发现。
                Some(endpoint) if heartbeat_ok(endpoint) => return Some(endpoint.clone()),
                // 负缓存未过期：不重复扫描进程。
                None if captured.elapsed() < NEGATIVE_TTL => return None,
                _ => {}
            }
        }
        let resolved = discover_endpoint();
        *guard = Some((Instant::now(), resolved.clone()));
        resolved
    }

    fn call(&self, endpoint: &Endpoint, method: &str, body: &Value) -> Result<Value> {
        rpc_call(endpoint, method, body)
    }
}

impl AgentAdapter for AntigravityAdapter {
    fn id(&self) -> &'static str {
        "antigravity"
    }

    fn discover(&self, _cutoff_ms: i64) -> Vec<SourceCandidate> {
        let Some(endpoint) = self.endpoint() else {
            return Vec::new();
        };
        let mut candidates = Vec::new();
        // 配额入口：即使没有任何会话也要能取到官方配额。
        candidates.push(SourceCandidate {
            source_id: QUOTA_SOURCE_ID.to_owned(),
            path: PathBuf::from("antigravity://quota"),
            size: 0,
            // 配额每次都刷新：mtime 用一个恒变的哨兵强制重扫。
            mtime_ns: i64::MAX,
        });
        if let Ok(list) = self.call(&endpoint, "GetAllCascadeTrajectories", &json!({})) {
            for cascade in normalize_trajectory_summaries(&list) {
                candidates.push(SourceCandidate {
                    source_id: stable_hash(&format!("antigravity|{}", cascade.id)),
                    path: PathBuf::from(format!("antigravity://cascade/{}", cascade.id)),
                    // stepCount 变化即内容变化，用它当"文件大小"触发重扫。
                    size: cascade.step_count.max(0) as u64,
                    mtime_ns: cascade.last_modified_ms.saturating_mul(1_000_000),
                });
            }
        }
        candidates
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        let endpoint = self
            .endpoint()
            .ok_or_else(|| anyhow!("Antigravity language server 未在运行"))?;

        if candidate.source_id == QUOTA_SOURCE_ID {
            let quotas = self
                .call(
                    &endpoint,
                    "RetrieveUserQuotaSummary",
                    &json!({ "forceRefresh": true }),
                )
                .map(|value| parse_quota_summary(&value))
                .unwrap_or_default();
            let mut source = empty_source(candidate, "antigravity-quota");
            source.quotas = quotas;
            return Ok(ParsedScan {
                source,
                diagnostics: ScanDiagnostics::default(),
            });
        }

        // cascade 路径形如 antigravity://cascade/<id>
        let cascade_id = candidate
            .path
            .to_string_lossy()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        let metadata = self.call(
            &endpoint,
            "GetCascadeTrajectoryGeneratorMetadata",
            &json!({ "cascadeId": cascade_id }),
        )?;
        let events = normalize_generator_metadata(&metadata, &cascade_id, cutoff_ms);

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
            diagnostics: ScanDiagnostics::default(),
        })
    }
}

fn empty_source(candidate: &SourceCandidate, logical_key: &str) -> ParsedSource {
    ParsedSource {
        source_id: candidate.source_id.clone(),
        adapter_id: "antigravity",
        locator: candidate.path.clone(),
        logical_key: logical_key.to_owned(),
        size: candidate.size,
        mtime_ns: candidate.mtime_ns,
        events: Vec::new(),
        quotas: Vec::new(),
    }
}

// ── 纯解析函数（可测试，不碰 I/O） ─────────────────────────────

struct CascadeRef {
    id: String,
    last_modified_ms: i64,
    step_count: i64,
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
                window_key: bucket_id,
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
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(AcceptAnyServerCert))
        .with_no_client_auth();
    ureq::AgentBuilder::new()
        .timeout(timeout)
        .tls_config(std::sync::Arc::new(config))
        .build()
}

fn rpc_call(endpoint: &Endpoint, method: &str, body: &Value) -> Result<Value> {
    let agent = insecure_agent(Duration::from_secs(6));
    let url = format!("{}{RPC_BASE_METHOD}/{method}", endpoint.base_url);
    let response = agent
        .post(&url)
        .set("Content-Type", "application/json")
        .set("Connect-Protocol-Version", "1")
        .set("X-Codeium-Csrf-Token", &endpoint.csrf)
        .send_string(&body.to_string())
        .map_err(|error| anyhow!("Antigravity RPC {method} 失败: {error}"))?;
    let text = response
        .into_string()
        .context("读取 Antigravity 响应失败")?;
    serde_json::from_str(&text).context("Antigravity 响应不是有效 JSON")
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
    let process = find_language_server_process()?;
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

#[cfg(windows)]
fn find_language_server_process() -> Option<ServerProcess> {
    use std::os::windows::process::CommandExt;
    let script = "$ErrorActionPreference='SilentlyContinue'; Get-CimInstance Win32_Process | \
                  Select-Object ProcessId,CommandLine | ConvertTo-Json -Compress";
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(0x0800_0000)
        .output()
        .ok()?;
    let json: Value = serde_json::from_slice(&output.stdout).ok()?;
    let entries: Vec<&Value> = match &json {
        Value::Array(items) => items.iter().collect(),
        single @ Value::Object(_) => vec![single],
        _ => return None,
    };
    for entry in entries {
        let command = entry
            .get("CommandLine")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !is_antigravity_command(command) {
            continue;
        }
        let Some(csrf) = extract_csrf(command) else {
            continue;
        };
        let pid = entry.get("ProcessId").and_then(Value::as_u64)? as u32;
        return Some(ServerProcess {
            pid,
            csrf,
            declared_port: extract_declared_port(command),
        });
    }
    None
}

#[cfg(windows)]
fn listening_ports(pid: u32) -> Vec<u16> {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new("netstat.exe")
        .args(["-ano", "-p", "TCP"])
        .creation_flags(0x0800_0000)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut ports = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 5 || parts[0] != "TCP" || !parts[3].eq_ignore_ascii_case("LISTENING") {
            continue;
        }
        if parts[4].parse::<u32>().ok() != Some(pid) {
            continue;
        }
        if let Some(port) = parts[1].rsplit(':').next().and_then(|p| p.parse().ok()) {
            ports.push(port);
        }
    }
    ports.dedup();
    ports
}

#[cfg(not(windows))]
fn find_language_server_process() -> Option<ServerProcess> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-ax", "-o", "pid=,command="])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let line = line.trim_start();
        let (pid_str, command) = line.split_once(char::is_whitespace)?;
        if !is_antigravity_command(command) {
            continue;
        }
        let Some(csrf) = extract_csrf(command) else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        return Some(ServerProcess {
            pid,
            csrf,
            declared_port: extract_declared_port(command),
        });
    }
    None
}

#[cfg(not(windows))]
fn listening_ports(pid: u32) -> Vec<u16> {
    let output = std::process::Command::new("lsof")
        .args(["-Pan", "-p", &pid.to_string(), "-iTCP", "-sTCP:LISTEN"])
        .output();
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
        assert_eq!(events[0].tokens.processed(), 100 + 900 + 0 + 40);
        assert_eq!(events[0].tokens.reasoning_output, 12);
        assert_eq!(events[0].tokens.cache_write, 0);
        assert_eq!(events[0].event_key, "response:resp-a");
        assert_eq!(events[0].model.as_deref(), Some("claude-sonnet-4.6"));
        assert_eq!(events[0].session_id, "cascade-1");
        assert_eq!(events[1].event_key, "response:resp-b");
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
