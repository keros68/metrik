//! GLM（ZCode）与 Kimi 的官方 coding-plan 配额拉取。
//!
//! 与 codex（`app_server`）、claude（`claude_oauth`）同型：用量走本地日志
//! adapter，配额则是一次**实时 GET** 官方接口。凭据从本机既有工具的配置里
//! 读取，只在本函数内存里用于一次请求——不入库、不写日志、不发往 Metrik
//! 之外任何地方；错误信息里绝不带 Authorization 头（token）。
//!
//! 隐私/诚实约束：拿不到凭据或接口失败时返回 Err，上层据此**不写任何配额行**
//! （卡片如实显示"配额不可用"），绝不用本地 token 用量估算冒充官方配额。
//!
//! 设计取舍：只接**主动暴露明文凭据**的来源（环境变量、OpenCode `auth.json`）。
//! 原生 zcode 桌面端把 OAuth token 加密存 `~/.zcode/v2/credentials.json`
//! （`enc:v1` 设备绑定，且不开本地端口），已在真机确认第三方读不到——不去逆向
//! 解密它（脆弱、分平台、侵入其内部）。故 OAuth-only 的原生用户会如实显示
//! "配额不可用"，用明文 key（含 OpenCode 生态）的用户自动生效。
//!
//! 未在真机验证（上线前须核对，字段名来自参考实现而非真实账号抓包）：
//! - Kimi 的环境变量名未能确认，故不从环境变量取 Kimi key；其原生 config 若
//!   同样加密，`extract_scalar` 只会拿到 `enc:` 串 → 拉取失败 → 如实不可用。
//! - 响应字段名按参考实现（cc-switch、opencode-quota、kimi-code-usage）取，
//!   做了多别名兜底；真机对不上时改这里的解析，别改账本口径。

use crate::domain::QuotaSample;
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

// GLM coding-plan 配额端点：国内 bigmodel 与国际 z.ai 两套，按 key 来源择一。
const GLM_BIGMODEL_URL: &str = "https://open.bigmodel.cn/api/monitor/usage/quota/limit";
const GLM_ZAI_URL: &str = "https://api.z.ai/api/monitor/usage/quota/limit";
// Kimi coding-plan 用量端点（须用 Kimi Code 控制台 key，非开放平台 key）。
const KIMI_USAGE_URL: &str = "https://api.kimi.com/coding/v1/usages";

// ── 拉取入口（供 engine 层带缓存调用） ─────────────────────────

pub fn fetch_zcode_quota(timeout: Duration) -> Result<Vec<QuotaSample>> {
    let cred = resolve_glm_credential()
        .context("未找到 GLM/ZCode 的 API key（~/.zcode、OpenCode auth.json 或 ZAI_* 环境变量）")?;
    let url = match cred.region {
        GlmRegion::Bigmodel => GLM_BIGMODEL_URL,
        GlmRegion::Zai => GLM_ZAI_URL,
    };
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let response = agent
        .get(url)
        // GLM 是裸 token，不带 Bearer 前缀。
        .set("Authorization", &cred.token)
        .set("Accept", "application/json")
        .call()
        .map_err(|error| map_ureq_error("GLM", error))?;
    let body = response.into_string().context("读取 GLM 配额响应失败")?;
    let json: Value = serde_json::from_str(&body).context("GLM 配额响应不是预期的 JSON")?;
    let samples = parse_glm_quota(&json);
    if samples.is_empty() {
        bail!("GLM 配额响应缺少可用窗口");
    }
    Ok(samples)
}

pub fn fetch_kimi_quota(timeout: Duration) -> Result<Vec<QuotaSample>> {
    let token = resolve_kimi_credential()
        .context("未找到 Kimi 的 coding API key（~/.kimi/config.toml 或 OpenCode auth.json）")?;
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let response = agent
        .get(KIMI_USAGE_URL)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json")
        .call()
        .map_err(|error| map_ureq_error("Kimi", error))?;
    let body = response.into_string().context("读取 Kimi 配额响应失败")?;
    let json: Value = serde_json::from_str(&body).context("Kimi 配额响应不是预期的 JSON")?;
    let samples = parse_kimi_quota(&json);
    if samples.is_empty() {
        bail!("Kimi 配额响应缺少可用窗口");
    }
    Ok(samples)
}

/// ureq 错误 → 面向用户的消息。绝不能把请求头（token）带进错误里。
fn map_ureq_error(provider: &str, error: ureq::Error) -> anyhow::Error {
    match error {
        ureq::Error::Status(401, _) | ureq::Error::Status(403, _) => {
            anyhow!("{provider} 配额凭据已失效（认证被拒），请重新登录对应 CLI")
        }
        ureq::Error::Status(429, _) => anyhow!("{provider} 配额接口限流（429），稍后自动重试"),
        ureq::Error::Status(code, _) => anyhow!("{provider} 配额接口返回 HTTP {code}"),
        ureq::Error::Transport(transport) => {
            anyhow!("{provider} 配额接口网络错误: {transport}")
        }
    }
}

// ── 凭据解析 ───────────────────────────────────────────────────

enum GlmRegion {
    Bigmodel,
    Zai,
}

struct GlmCredential {
    token: String,
    region: GlmRegion,
}

/// GLM key 解析：环境变量 → OpenCode `auth.json` 的明文 key。环境变量名决定
/// 走国内 bigmodel 还是国际 z.ai 端点。
///
/// 原生 zcode（智谱 GLM 桌面端）不在此列：它的 OAuth token 存在
/// `~/.zcode/v2/credentials.json` 且是 `enc:v1` 设备绑定加密（Electron
/// safeStorage/DPAPI），第三方应用读不了、也不该去解——故 GLM 配额需用户
/// 自备明文 key（环境变量或 OpenCode）。
fn resolve_glm_credential() -> Option<GlmCredential> {
    // 国际站 z.ai key → z.ai 端点。
    for name in ["ZAI_CODING_PLAN_API_KEY", "ZAI_API_KEY"] {
        if let Some(token) = env_nonempty(name) {
            return Some(GlmCredential {
                token,
                region: GlmRegion::Zai,
            });
        }
    }
    // 国内智谱 bigmodel key → bigmodel 端点。
    for name in ["ZHIPUAI_API_KEY", "ZHIPU_API_KEY", "GLM_API_KEY"] {
        if let Some(token) = env_nonempty(name) {
            return Some(GlmCredential {
                token,
                region: GlmRegion::Bigmodel,
            });
        }
    }
    let opencode = read_opencode_auth();
    if let Some(token) = nonempty(opencode.get("zhipuai-coding-plan")) {
        return Some(GlmCredential {
            token,
            region: GlmRegion::Bigmodel,
        });
    }
    if let Some(token) = nonempty(opencode.get("zai-coding-plan")) {
        return Some(GlmCredential {
            token,
            region: GlmRegion::Zai,
        });
    }
    None
}

/// Kimi key 解析：原生 `~/.kimi[-code]/config.toml|json` → OpenCode `auth.json`
/// 的 `kimi-for-coding`。环境变量名未确认，不猜。
fn resolve_kimi_credential() -> Option<String> {
    for path in kimi_config_paths() {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Some(key) = extract_scalar(&raw, "api_key") {
                return Some(key);
            }
        }
    }
    nonempty(read_opencode_auth().get("kimi-for-coding"))
}

#[derive(Deserialize)]
struct AuthEntry {
    key: Option<String>,
}

/// OpenCode `auth.json`：`{ "<provider>": { "type": "api", "key": "..." } }`。
fn parse_provider_key_map(raw: &str) -> HashMap<String, String> {
    serde_json::from_str::<HashMap<String, AuthEntry>>(raw.trim_start_matches('\u{feff}'))
        .map(|map| {
            map.into_iter()
                .filter_map(|(provider, entry)| {
                    let key = entry.key?;
                    (!key.trim().is_empty()).then_some((provider, key))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn read_opencode_auth() -> HashMap<String, String> {
    for path in opencode_auth_paths() {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            let map = parse_provider_key_map(&raw);
            if !map.is_empty() {
                return map;
            }
        }
    }
    HashMap::new()
}

fn opencode_auth_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        let base = PathBuf::from(xdg);
        if base.is_absolute() {
            paths.push(base.join("opencode").join("auth.json"));
        }
    }
    if let Some(home) = dirs::home_dir() {
        paths.push(
            home.join(".local")
                .join("share")
                .join("opencode")
                .join("auth.json"),
        );
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        paths.push(PathBuf::from(appdata).join("opencode").join("auth.json"));
    }
    paths
}

fn kimi_config_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    ["config.toml", "config.json"]
        .into_iter()
        .flat_map(|name| {
            [
                home.join(".kimi").join(name),
                home.join(".kimi-code").join(name),
            ]
        })
        .collect()
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn nonempty(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// 从 TOML 或 JSON 文本里抠出 `field = "..."` / `"field": "..."` 的标量值。
/// 只为最好努力地读一个 key，故不引 TOML 依赖，逐行扫首个引号值。
fn extract_scalar(text: &str, field: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| extract_field_in_line(line, field))
}

fn extract_field_in_line(line: &str, field: &str) -> Option<String> {
    for (index, _) in line.match_indices(field) {
        // 标识符边界：`my_api_key` 不能命中 `api_key`。
        if index > 0 {
            let prev = line.as_bytes()[index - 1];
            if prev == b'_' || prev.is_ascii_alphanumeric() {
                continue;
            }
        }
        let after = line[index + field.len()..]
            .trim_start_matches(['"', '\''])
            .trim_start();
        let Some(after) = after.strip_prefix('=').or_else(|| after.strip_prefix(':')) else {
            continue;
        };
        if let Some(value) = first_quoted(after) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn first_quoted(text: &str) -> Option<String> {
    let start = text.find(['"', '\''])?;
    let quote = &text[start..=start];
    let after = &text[start + 1..];
    let end = after.find(quote)?;
    Some(after[..end].to_owned())
}

// ── 响应解析（纯函数，可测试） ─────────────────────────────────

/// GLM：`data.limits[]` 里取 `TOKENS_LIMIT` 两条（5 小时 + 每周），按下次重置
/// 时间升序 → 短窗在前。`percentage` 是已用百分比。`TIME_LIMIT`（月度 MCP 次数）
/// 单位不同，跳过。
fn parse_glm_quota(value: &Value) -> Vec<QuotaSample> {
    let data = value.get("data").unwrap_or(value);
    let Some(limits) = data.get("limits").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut windows: Vec<(Option<i64>, f64)> = limits
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("TOKENS_LIMIT"))
        .filter_map(|item| {
            let used = item.get("percentage").and_then(Value::as_f64)?;
            let reset = first_time(
                item,
                &["nextResetTime", "resetTime", "reset_at", "reset_time"],
            );
            Some((reset, used))
        })
        .collect();
    windows.sort_by(|left, right| match (left.0, right.0) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    });

    let now = chrono::Utc::now().timestamp_millis();
    windows
        .into_iter()
        .take(2)
        .enumerate()
        .map(|(index, (reset, used))| QuotaSample {
            adapter_id: "zcode",
            window_key: if index == 0 { "five_hour" } else { "seven_day" }.to_owned(),
            remaining_percent: (100.0 - used).clamp(0.0, 100.0),
            resets_at_ms: reset,
            collected_at_ms: now,
            source_label: "GLM 官方配额".into(),
            quality: "official_live",
        })
        .collect()
}

/// Kimi：`data[]`（或兜底 `limits[]`）每条一个限流窗口。按 `duration`+`timeUnit`
/// 归到 5 小时 / 每周窗口；`model_name == "all"` 是每周汇总，优先保留。
fn parse_kimi_quota(value: &Value) -> Vec<QuotaSample> {
    let entries = value
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| value.get("limits").and_then(Value::as_array))
        .or_else(|| value.as_array());
    let Some(entries) = entries else {
        return Vec::new();
    };

    let now = chrono::Utc::now().timestamp_millis();
    let mut by_key: BTreeMap<&'static str, (bool, QuotaSample)> = BTreeMap::new();
    for entry in entries {
        let limit = first_f64(entry, &["limit", "limit_amount"]);
        let remaining = first_f64(entry, &["remaining"]);
        let used = first_f64(entry, &["used", "used_amount"]);
        let remaining_percent = match (remaining, limit) {
            (Some(r), Some(l)) if l > 0.0 => r / l * 100.0,
            _ => match (used, limit) {
                (Some(u), Some(l)) if l > 0.0 => 100.0 - u / l * 100.0,
                _ => continue,
            },
        };
        let is_all = entry.get("model_name").and_then(Value::as_str) == Some("all");
        let key = kimi_window_key(entry, is_all);
        let reset = first_time(entry, &["resetTime", "reset_at", "reset_time"]).or_else(|| {
            entry
                .get("reset_in")
                .and_then(Value::as_i64)
                .map(|seconds| now + seconds * 1000)
        });
        let sample = QuotaSample {
            adapter_id: "kimi",
            window_key: key.to_owned(),
            remaining_percent: remaining_percent.clamp(0.0, 100.0),
            resets_at_ms: reset,
            collected_at_ms: now,
            source_label: "Kimi 官方配额".into(),
            quality: "official_live",
        };
        // 每周窗口优先采用 model_name=="all" 的汇总行。
        match by_key.get(key) {
            Some((existing_all, _)) if *existing_all && !is_all => {}
            _ => {
                by_key.insert(key, (is_all, sample));
            }
        }
    }
    by_key.into_values().map(|(_, sample)| sample).collect()
}

fn kimi_window_key(entry: &Value, is_all: bool) -> &'static str {
    let window = entry.get("window").unwrap_or(entry);
    let duration = window.get("duration").and_then(Value::as_i64);
    let unit =
        first_str(window, &["timeUnit", "time_unit"]).map(|value| value.to_ascii_uppercase());
    if let (Some(duration), Some(unit)) = (duration, unit.as_deref()) {
        let minutes = match unit {
            "MINUTE" => duration,
            "HOUR" => duration * 60,
            "DAY" => duration * 1440,
            "WEEK" => duration * 10080,
            "MONTH" => duration * 43200,
            _ => 0,
        };
        if minutes > 0 {
            return if minutes <= 360 {
                "five_hour"
            } else {
                "seven_day"
            };
        }
    }
    if is_all {
        "seven_day"
    } else {
        "five_hour"
    }
}

fn first_f64(value: &Value, names: &[&str]) -> Option<f64> {
    names
        .iter()
        .find_map(|name| value.get(name).and_then(Value::as_f64))
}

fn first_str<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| value.get(name).and_then(Value::as_str))
}

fn first_time(value: &Value, names: &[&str]) -> Option<i64> {
    names
        .iter()
        .find_map(|name| value.get(name).and_then(value_to_ms))
}

/// 时间字段可能是毫秒/秒的整数，或 RFC3339 字符串。统一成毫秒。
fn value_to_ms(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(normalize_epoch_ms(number));
    }
    if let Some(text) = value.as_str() {
        if let Ok(number) = text.parse::<i64>() {
            return Some(normalize_epoch_ms(number));
        }
        return chrono::DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|parsed| parsed.timestamp_millis());
    }
    None
}

fn normalize_epoch_ms(value: i64) -> i64 {
    // 10^12 以下按秒解释（约合 2001 年后的秒级时间戳），否则已是毫秒。
    if value.abs() < 1_000_000_000_000 {
        value * 1000
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glm_quota_maps_tokens_limits_to_two_windows_by_reset_order() {
        // 每周（较晚重置）故意排在 5 小时（较早重置）之前，验证按重置时间排序。
        let json: Value = serde_json::from_str(
            r#"{
                "code": 200, "success": true,
                "data": {
                    "level": "pro",
                    "limits": [
                        {"type": "TIME_LIMIT", "percentage": 7, "remaining": 928},
                        {"type": "TOKENS_LIMIT", "percentage": 53, "nextResetTime": 1900000000000},
                        {"type": "TOKENS_LIMIT", "percentage": 44, "nextResetTime": 1800000000000}
                    ]
                }
            }"#,
        )
        .unwrap();
        let samples = parse_glm_quota(&json);
        assert_eq!(samples.len(), 2, "只取两条 TOKENS_LIMIT，跳过 TIME_LIMIT");
        assert_eq!(samples[0].window_key, "five_hour");
        assert_eq!(samples[0].remaining_percent, 56.0); // 100 - 44，重置更早
        assert_eq!(samples[0].resets_at_ms, Some(1800000000000));
        assert_eq!(samples[1].window_key, "seven_day");
        assert_eq!(samples[1].remaining_percent, 47.0); // 100 - 53
        assert_eq!(samples[0].adapter_id, "zcode");
    }

    #[test]
    fn glm_quota_empty_without_token_limits() {
        let json: Value =
            serde_json::from_str(r#"{"data":{"limits":[{"type":"TIME_LIMIT","percentage":7}]}}"#)
                .unwrap();
        assert!(parse_glm_quota(&json).is_empty());
    }

    #[test]
    fn kimi_quota_classifies_windows_and_prefers_weekly_summary() {
        let json: Value = serde_json::from_str(
            r#"{
                "data": [
                    {"model_name": "kimi-k2", "limit": 100, "used": 30, "duration": 5, "timeUnit": "HOUR", "resetTime": 1800000000},
                    {"model_name": "all", "limit": 200, "remaining": 60, "duration": 7, "timeUnit": "DAY", "reset_in": 3600}
                ]
            }"#,
        )
        .unwrap();
        let mut samples = parse_kimi_quota(&json);
        samples.sort_by(|a, b| a.window_key.cmp(&b.window_key));
        assert_eq!(samples.len(), 2);
        // five_hour：由 used/limit 推得 remaining=70%
        let five = samples
            .iter()
            .find(|s| s.window_key == "five_hour")
            .unwrap();
        assert_eq!(five.remaining_percent, 70.0);
        assert_eq!(five.resets_at_ms, Some(1800000000000)); // 秒→毫秒
        assert_eq!(five.adapter_id, "kimi");
        // seven_day：由 remaining/limit 推得 30%
        let week = samples
            .iter()
            .find(|s| s.window_key == "seven_day")
            .unwrap();
        assert_eq!(week.remaining_percent, 30.0);
    }

    #[test]
    fn kimi_weekly_all_row_wins_over_per_model_for_same_window() {
        // 两条都归为每周窗口：model_name=="all" 的汇总应胜出。
        let json: Value = serde_json::from_str(
            r#"{"data": [
                {"model_name": "kimi-k2", "limit": 100, "remaining": 10, "duration": 7, "timeUnit": "DAY"},
                {"model_name": "all", "limit": 100, "remaining": 80, "duration": 7, "timeUnit": "DAY"}
            ]}"#,
        )
        .unwrap();
        let samples = parse_kimi_quota(&json);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].window_key, "seven_day");
        assert_eq!(samples[0].remaining_percent, 80.0);
    }

    #[test]
    fn opencode_auth_reads_provider_keys() {
        let raw = r#"{
            "zhipuai-coding-plan": {"type": "api", "key": "glm-secret"},
            "kimi-for-coding": {"type": "api", "key": "sk-kimi-secret"},
            "blank": {"type": "api", "key": "  "}
        }"#;
        let map = parse_provider_key_map(raw);
        assert_eq!(map.get("zhipuai-coding-plan").unwrap(), "glm-secret");
        assert_eq!(map.get("kimi-for-coding").unwrap(), "sk-kimi-secret");
        assert!(!map.contains_key("blank"), "空 key 过滤掉");
    }

    #[test]
    fn extract_scalar_handles_toml_and_json_shapes() {
        let toml = "[providers.kimi-for-coding]\ntype = \"kimi\"\napi_key = \"sk-from-toml\"\n";
        assert_eq!(extract_scalar(toml, "api_key").unwrap(), "sk-from-toml");
        let json = "{ \"api_key\": \"sk-from-json\" }";
        assert_eq!(extract_scalar(json, "api_key").unwrap(), "sk-from-json");
        assert_eq!(extract_scalar("# api_key = \"x\"", "api_key"), None);
    }

    #[test]
    fn value_to_ms_normalizes_seconds_and_iso() {
        assert_eq!(
            value_to_ms(&Value::from(1_800_000_000_i64)),
            Some(1_800_000_000_000)
        );
        assert_eq!(
            value_to_ms(&Value::from(1_800_000_000_000_i64)),
            Some(1_800_000_000_000)
        );
        assert_eq!(
            value_to_ms(&Value::from("2026-07-15T00:00:00Z")),
            chrono::DateTime::parse_from_rfc3339("2026-07-15T00:00:00Z")
                .ok()
                .map(|value| value.timestamp_millis())
        );
    }
}
