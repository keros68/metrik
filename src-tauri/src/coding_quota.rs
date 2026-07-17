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
//! 设计取舍：只接**主动暴露明文凭据**的来源（环境变量、OpenCode `auth.json`、
//! Kimi Code 的明文 OAuth 文件）。原生 zcode 桌面端把 OAuth token 加密存
//! `~/.zcode/v2/credentials.json`（`enc:v1` 设备绑定，且不开本地端口），已在
//! 真机确认第三方读不到——不去逆向解密它（脆弱、分平台、侵入其内部）。故
//! zcode 的 OAuth-only 用户会如实显示"配额不可用"。
//!
//! Kimi：**已按真机账号抓包核对**（2026-07）。Kimi Code 走 OAuth，`config.toml`
//! 的 `api_key` 恒为空串，凭据是 `credentials/kimi-code.json` 里的明文
//! access_token，配额接口认它。该令牌**只活 15 分钟**且由 Kimi Code 自行续期；
//! Metrik 只读不刷新，所以令牌过期时拉取失败 → 上层保留上次的配额行、按陈旧
//! 标注（`official_live` 超过 7 分钟即标 `~`），不会伪装成新数据。
//! 环境变量名仍未确认，故不从环境变量取 Kimi key。
//!
//! GLM：**已按真机账号抓包核实**（2026-07，bigmodel 端点）。响应形状与参考
//! 实现一致（`data.limits[]`，两条 `TOKENS_LIMIT` 按 nextResetTime 升序 =
//! 5h/每周，`percentage` 为已用%）；注意该接口把业务错误放在 HTTP 200 的
//! body 里（`code` 非 200，认证失败是 1000），必须识别，否则错 key 会被
//! 误报成"缺少可用窗口"。凭据是候选列表逐把尝试：环境变量里的开放平台 key
//! 与 coding-plan key 同形（32hex.16位）但互不通用，离线分不出。

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
// Kimi coding-plan 用量端点。认 Kimi Code 的 OAuth access_token（实测响应里
// `authentication.method` 回 METHOD_ACCESS_TOKEN），也认控制台 key；开放平台
// key 不是这套。
const KIMI_USAGE_URL: &str = "https://api.kimi.com/coding/v1/usages";

// ── 拉取入口（供 engine 层带缓存调用） ─────────────────────────

/// 多个来源都可能有 GLM key，且离线无法判断哪把是 coding-plan 的——真机上就
/// 发生过：环境变量 `GLM_API_KEY` 是开放平台 key（配额接口不认），真正的
/// coding-plan key 在 zcode 的 provider 配置里。所以逐把尝试，先成功者胜；
/// 全部失败时报最后一个错误，不再让一把错 key 一票否决。
pub fn fetch_zcode_quota(timeout: Duration) -> Result<Vec<QuotaSample>> {
    let candidates = resolve_glm_credentials();
    if candidates.is_empty() {
        bail!("未找到 GLM/ZCode 的 API key（zcode 配置、环境变量或 OpenCode auth.json）");
    }
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let mut last_error = None;
    for cred in candidates {
        match fetch_glm_once(&agent, &cred) {
            Ok(samples) => return Ok(samples),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.expect("candidates 非空则必有错误"))
}

fn fetch_glm_once(agent: &ureq::Agent, cred: &GlmCredential) -> Result<Vec<QuotaSample>> {
    let url = match cred.region {
        GlmRegion::Bigmodel => GLM_BIGMODEL_URL,
        GlmRegion::Zai => GLM_ZAI_URL,
    };
    let response = agent
        .get(url)
        // GLM 是裸 token，不带 Bearer 前缀（真机验证 Bearer 也认，保持裸即可）。
        .set("Authorization", &cred.token)
        .set("Accept", "application/json")
        .call()
        .map_err(|error| map_ureq_error("GLM", error))?;
    let body = response.into_string().context("读取 GLM 配额响应失败")?;
    let json: Value = serde_json::from_str(&body).context("GLM 配额响应不是预期的 JSON")?;
    // 该接口把业务错误放在 HTTP 200 的 body 里（认证失败是 code 1000）。
    // 不识别它就会误报成"缺少可用窗口"，把认证问题伪装成解析问题。
    if let Some(error) = glm_business_error(&json) {
        bail!(error);
    }
    let samples = parse_glm_quota(&json);
    if samples.is_empty() {
        bail!("GLM 配额响应缺少可用窗口");
    }
    Ok(samples)
}

/// 成功时 `code` 是 200；其它值是业务错误（真机实测：错 key → HTTP 200 +
/// `{"code":1000,"msg":"身份验证失败。"}`）。没有 code 字段就不拦。
fn glm_business_error(json: &Value) -> Option<String> {
    let code = json.get("code").and_then(Value::as_i64)?;
    if code == 200 {
        return None;
    }
    let msg = json
        .get("msg")
        .and_then(Value::as_str)
        .unwrap_or("未知错误");
    Some(format!("GLM 配额接口返回业务错误 code {code}: {msg}"))
}

pub fn fetch_kimi_quota(timeout: Duration) -> Result<Vec<QuotaSample>> {
    let token = resolve_kimi_credential().context(
        "未找到 Kimi 的凭据（~/.kimi-code 的 config.toml/credentials 或 OpenCode auth.json）",
    )?;
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

/// GLM key 的全部候选：zcode 的 provider 配置 → 环境变量 → OpenCode `auth.json`。
/// 全部收集、由调用方逐把尝试——不同来源的 key 可能是不同产品的（开放平台 key
/// 打不通 coding-plan 配额接口），离线分不出真假。
///
/// zcode 桌面端有两类凭据：OAuth token 在 `credentials.json` 里 `enc:v1`
/// 设备绑定加密（读不了、也不去解）；但用户手动配置的 provider（`source:
/// custom`）把 apiKey **明文**存在 `v2/config.json`——2026-07 真机核实，
/// 这正是能打通配额接口的那把 coding-plan key，符合"只接主动暴露明文凭据"。
fn resolve_glm_credentials() -> Vec<GlmCredential> {
    let mut candidates: Vec<GlmCredential> = Vec::new();
    let mut push = |token: String, region: GlmRegion| {
        if !candidates.iter().any(|cred| cred.token == token) {
            candidates.push(GlmCredential { token, region });
        }
    };

    // zcode 的 provider 配置最优先：它是 coding-plan 产品自己的配置，
    // key 与套餐必然对应；环境变量反而可能是别的产品的。
    for path in zcode_config_paths() {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            for (token, region) in glm_keys_from_zcode_config(&raw) {
                push(token, region);
            }
        }
    }
    // 国际站 z.ai key → z.ai 端点。
    for name in ["ZAI_CODING_PLAN_API_KEY", "ZAI_API_KEY"] {
        if let Some(token) = env_nonempty(name) {
            push(token, GlmRegion::Zai);
        }
    }
    // 国内智谱 bigmodel key → bigmodel 端点。
    for name in ["ZHIPUAI_API_KEY", "ZHIPU_API_KEY", "GLM_API_KEY"] {
        if let Some(token) = env_nonempty(name) {
            push(token, GlmRegion::Bigmodel);
        }
    }
    let opencode = read_opencode_auth();
    if let Some(token) = nonempty(opencode.get("zhipuai-coding-plan")) {
        push(token, GlmRegion::Bigmodel);
    }
    if let Some(token) = nonempty(opencode.get("zai-coding-plan")) {
        push(token, GlmRegion::Zai);
    }
    candidates
}

fn zcode_config_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    vec![home.join(".zcode").join("v2").join("config.json")]
}

/// 从 zcode 的 `v2/config.json` 里取 coding-plan provider 的明文 apiKey。
/// 结构：`provider.<name>.options.apiKey`。必须结构化解析——整个文件里有
/// 多个 apiKey（含 start-plan 的 JWT、moonshot 服务的 key），按行扫会抓错。
/// `enabled: false` 的 provider 跳过（真机上 start-plan 就是禁用且未开通的）。
fn glm_keys_from_zcode_config(raw: &str) -> Vec<(String, GlmRegion)> {
    let Ok(json) = serde_json::from_str::<Value>(raw.trim_start_matches('\u{feff}')) else {
        return Vec::new();
    };
    let Some(providers) = json.get("provider").and_then(Value::as_object) else {
        return Vec::new();
    };
    [
        ("builtin:bigmodel-coding-plan", GlmRegion::Bigmodel),
        ("builtin:zai-coding-plan", GlmRegion::Zai),
    ]
    .into_iter()
    .filter_map(|(name, region)| {
        let entry = providers.get(name)?;
        if entry.get("enabled").and_then(Value::as_bool) == Some(false) {
            return None;
        }
        let key = entry.pointer("/options/apiKey")?.as_str()?.trim();
        (!key.is_empty()).then(|| (key.to_owned(), region))
    })
    .collect()
}

/// Kimi key 解析：原生 `~/.kimi[-code]/config.toml|json` 的 `api_key` → 原生
/// OAuth 凭据文件 → OpenCode `auth.json` 的 `kimi-for-coding`。
/// 环境变量名未确认，不猜。
fn resolve_kimi_credential() -> Option<String> {
    for path in kimi_config_paths() {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Some(key) = extract_scalar(&raw, "api_key") {
                return Some(key);
            }
        }
    }
    for path in kimi_oauth_paths() {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Some(token) = extract_scalar(&raw, "access_token") {
                return Some(token);
            }
        }
    }
    nonempty(read_opencode_auth().get("kimi-for-coding"))
}

/// Kimi Code 用 OAuth 登录：`config.toml` 的 `api_key` 恒为空串，真凭据是这里的
/// 明文 access_token（实测配额接口认它，`authentication.method` 回
/// `METHOD_ACCESS_TOKEN`）。只读，不刷新也不写回——续期是 Kimi Code 自己的事；
/// 令牌过期就照实显示不可用。加密存储的凭据一律不碰（见 zcode）。
fn kimi_oauth_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    [".kimi-code", ".kimi"]
        .into_iter()
        .map(|dir| home.join(dir).join("credentials").join("kimi-code.json"))
        .collect()
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

/// Kimi：`limits[]` 每条是一个限流窗口——长度在 `window`（`duration` +
/// `TIME_UNIT_*`），数值在 `detail` 里且是字符串（`"100"`）。每周窗口不在
/// `limits[]` 里，而是顶层的 `usage` 块。形状据真机抓包（2026-07）。
fn parse_kimi_quota(value: &Value) -> Vec<QuotaSample> {
    let now = chrono::Utc::now().timestamp_millis();
    let mut by_key: BTreeMap<&'static str, QuotaSample> = BTreeMap::new();

    if let Some(limits) = value.get("limits").and_then(Value::as_array) {
        for entry in limits {
            let key = kimi_window_key(entry);
            // 数值嵌在 detail 里；直接挂在条目上时同样能取到。
            let numbers = entry.get("detail").unwrap_or(entry);
            if let Some(sample) = kimi_sample(key, numbers, now) {
                by_key.entry(key).or_insert(sample);
            }
        }
    }
    // 每周窗口只有顶层 usage 有；limits[] 里不含它。
    if let Some(usage) = value.get("usage") {
        if let Some(sample) = kimi_sample("seven_day", usage, now) {
            by_key.entry("seven_day").or_insert(sample);
        }
    }
    by_key.into_values().collect()
}

fn kimi_sample(key: &'static str, numbers: &Value, now: i64) -> Option<QuotaSample> {
    let limit = first_f64(numbers, &["limit", "limit_amount"])?;
    if limit <= 0.0 {
        return None;
    }
    // 有 remaining 就直接用，否则由 used 反推。
    let remaining_percent = match first_f64(numbers, &["remaining"]) {
        Some(remaining) => remaining / limit * 100.0,
        None => 100.0 - first_f64(numbers, &["used", "used_amount"])? / limit * 100.0,
    };
    let reset = first_time(numbers, &["resetTime", "reset_at", "reset_time"]).or_else(|| {
        numbers
            .get("reset_in")
            .and_then(Value::as_i64)
            .map(|seconds| now + seconds * 1000)
    });
    Some(QuotaSample {
        adapter_id: "kimi",
        window_key: key.to_owned(),
        remaining_percent: remaining_percent.clamp(0.0, 100.0),
        resets_at_ms: reset,
        collected_at_ms: now,
        source_label: "Kimi 官方配额".into(),
        quality: "official_live",
    })
}

fn kimi_window_key(entry: &Value) -> &'static str {
    let window = entry.get("window").unwrap_or(entry);
    let duration = window.get("duration").and_then(Value::as_i64);
    let unit =
        first_str(window, &["timeUnit", "time_unit"]).map(|value| value.to_ascii_uppercase());
    // 实测单位带 TIME_UNIT_ 前缀（TIME_UNIT_MINUTE）；裸单位也照样认。
    let unit = unit
        .as_deref()
        .map(|unit| unit.trim_start_matches("TIME_UNIT_"));
    if let (Some(duration), Some(unit)) = (duration, unit) {
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
    "five_hour"
}

fn first_f64(value: &Value, names: &[&str]) -> Option<f64> {
    names.iter().find_map(|name| {
        value.get(name).and_then(|found| {
            // Kimi 的配额数字以字符串返回（"100"）。
            found
                .as_f64()
                .or_else(|| found.as_str()?.trim().parse().ok())
        })
    })
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

    /// 真机抓包（2026-07，bigmodel coding plan）。TIME_LIMIT（月度工具调用）
    /// 要跳过；两条 TOKENS_LIMIT 按 nextResetTime 升序 = 5h / 每周。
    const GLM_LIVE_RESPONSE: &str = r#"{
        "code": 200, "msg": "操作成功", "success": true,
        "data": {
            "level": "pro",
            "limits": [
                {"type": "TIME_LIMIT", "unit": 5, "number": 1, "usage": 1000, "currentValue": 1,
                 "remaining": 999, "percentage": 1, "nextResetTime": 1785724831967,
                 "usageDetails": [{"modelCode": "search-prime", "usage": 0}]},
                {"type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 2, "nextResetTime": 1784305870779},
                {"type": "TOKENS_LIMIT", "unit": 6, "number": 1, "percentage": 22, "nextResetTime": 1784688031980}
            ]
        }
    }"#;

    #[test]
    fn glm_quota_reads_the_live_shape() {
        let json: Value = serde_json::from_str(GLM_LIVE_RESPONSE).unwrap();
        assert!(glm_business_error(&json).is_none());
        let samples = parse_glm_quota(&json);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].window_key, "five_hour");
        assert_eq!(samples[0].remaining_percent, 98.0);
        assert_eq!(samples[0].resets_at_ms, Some(1784305870779));
        assert_eq!(samples[1].window_key, "seven_day");
        assert_eq!(samples[1].remaining_percent, 78.0);
    }

    #[test]
    fn glm_business_error_in_http_200_body_is_not_mistaken_for_missing_windows() {
        // 真机实测：错 key → HTTP 200 + code 1000。之前这会误报"缺少可用窗口"。
        let json: Value =
            serde_json::from_str(r#"{"code":1000,"msg":"身份验证失败。","success":false}"#)
                .unwrap();
        let error = glm_business_error(&json).expect("必须识别为业务错误");
        assert!(error.contains("1000"));
        assert!(error.contains("身份验证失败"));
    }

    #[test]
    fn zcode_config_yields_only_enabled_coding_plan_keys() {
        // 结构仿真机 v2/config.json：coding-plan 明文 key 启用；start-plan 禁用
        // （其 JWT 不得被抓走）；zai 的 key 为空串要跳过。
        let raw = r#"{
            "provider": {
                "builtin:bigmodel-coding-plan": {"enabled": true, "options": {"apiKey": "aaaa.bbbb"}},
                "builtin:bigmodel-start-plan": {"enabled": false, "options": {"apiKey": "eyJhbGciOi.disabled"}},
                "builtin:zai-coding-plan": {"enabled": true, "options": {"apiKey": ""}}
            }
        }"#;
        let keys = glm_keys_from_zcode_config(raw);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].0, "aaaa.bbbb");
        assert!(matches!(keys[0].1, GlmRegion::Bigmodel));

        // 坏 JSON / 缺 provider 段都要安静地返回空，不 panic。
        assert!(glm_keys_from_zcode_config("not json").is_empty());
        assert!(glm_keys_from_zcode_config("{}").is_empty());
    }

    /// 打真实 bigmodel 接口的烟测（解析器只能证明"对得上夹具"；接口形状漂移
    /// 只有它能发现）。需要本机 zcode 配置或环境变量里有可用的 coding-plan key。
    #[test]
    #[ignore = "reads local GLM credentials and calls the live quota API"]
    fn live_glm_quota_smoke_test() {
        let samples = fetch_zcode_quota(Duration::from_secs(15)).expect("fetch glm quota");
        assert!(!samples.is_empty(), "配额响应里没有可用窗口");
        for sample in &samples {
            println!(
                "glm quota: window={} remaining={:.1}% resets_at={:?}",
                sample.window_key, sample.remaining_percent, sample.resets_at_ms
            );
            assert_eq!(sample.adapter_id, "zcode");
            assert!((0.0..=100.0).contains(&sample.remaining_percent));
        }
    }

    /// 真机抓包（2026-07，Kimi Code + OAuth 登录），只把 userId 换成占位符。
    /// 之前的夹具照参考实现编造了 `data[]`/`model_name`，真实接口里并不存在。
    const KIMI_LIVE_RESPONSE: &str = r#"{
        "user": {"userId": "test-user", "region": "REGION_CN", "membership": {"level": "LEVEL_INTERMEDIATE"}, "businessId": ""},
        "usage": {"limit": "100", "remaining": "100", "resetTime": "2026-07-24T08:31:19.749909Z"},
        "limits": [
            {"window": {"duration": 300, "timeUnit": "TIME_UNIT_MINUTE"},
             "detail": {"limit": "100", "used": "2", "remaining": "98", "resetTime": "2026-07-17T13:31:19.749909Z"}}
        ],
        "parallel": {"limit": "20"},
        "totalQuota": {"limit": "100", "remaining": "99"},
        "authentication": {"method": "METHOD_ACCESS_TOKEN", "scope": "FEATURE_CODING"},
        "subType": "TYPE_PURCHASE"
    }"#;

    #[test]
    fn kimi_quota_reads_the_live_shape_nested_detail_and_string_numbers() {
        let json: Value = serde_json::from_str(KIMI_LIVE_RESPONSE).unwrap();
        let mut samples = parse_kimi_quota(&json);
        samples.sort_by(|a, b| a.window_key.cmp(&b.window_key));
        assert_eq!(samples.len(), 2);

        // 300 分钟 = 5 小时窗；数值嵌在 detail 里且是字符串。
        let five = samples
            .iter()
            .find(|s| s.window_key == "five_hour")
            .unwrap();
        assert_eq!(five.remaining_percent, 98.0);
        assert_eq!(five.adapter_id, "kimi");
        assert_eq!(
            five.resets_at_ms,
            chrono::DateTime::parse_from_rfc3339("2026-07-17T13:31:19.749909Z")
                .ok()
                .map(|value| value.timestamp_millis())
        );

        // 每周窗口来自顶层 usage，limits[] 里没有它。
        let week = samples
            .iter()
            .find(|s| s.window_key == "seven_day")
            .unwrap();
        assert_eq!(week.remaining_percent, 100.0);
        assert_eq!(
            week.resets_at_ms,
            chrono::DateTime::parse_from_rfc3339("2026-07-24T08:31:19.749909Z")
                .ok()
                .map(|value| value.timestamp_millis())
        );
    }

    /// 解析器只能证明"对得上夹具"；接口形状会漂移（这个 bug 就是这么来的：
    /// 字段名照参考实现编造，真接口对不上）。这个烟测打真接口，是唯一能发现
    /// 漂移的地方。需要本机装了 Kimi Code 并已登录。
    #[test]
    #[ignore = "reads the current user's Kimi Code credential and calls the live quota API"]
    fn live_kimi_quota_smoke_test() {
        let samples = fetch_kimi_quota(Duration::from_secs(15)).expect("fetch kimi quota");
        assert!(!samples.is_empty(), "配额响应里没有可用窗口");
        for sample in &samples {
            println!(
                "kimi quota: window={} remaining={:.1}% resets_at={:?}",
                sample.window_key, sample.remaining_percent, sample.resets_at_ms
            );
            assert_eq!(sample.adapter_id, "kimi");
            assert!((0.0..=100.0).contains(&sample.remaining_percent));
        }
    }

    #[test]
    fn kimi_quota_derives_remaining_from_used_and_ignores_zero_limit() {
        // 只有 used：由 used/limit 反推剩余。
        let json: Value = serde_json::from_str(
            r#"{"limits": [{"window": {"duration": 5, "timeUnit": "HOUR"},
                            "detail": {"limit": 200, "used": 50}}]}"#,
        )
        .unwrap();
        let samples = parse_kimi_quota(&json);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].window_key, "five_hour");
        assert_eq!(samples[0].remaining_percent, 75.0);

        // limit 为 0 时不能按 0% 处理，直接跳过该窗口。
        let json: Value =
            serde_json::from_str(r#"{"usage": {"limit": "0", "remaining": "0"}}"#).unwrap();
        assert!(parse_kimi_quota(&json).is_empty());
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
