use crate::domain::QuotaSample;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Claude 官方额度的 opt-in 凭据来源。
///
/// 隐私红线：默认关闭；用户在设置页显式开启后，才读取 Claude Code 自己
/// 保存的 OAuth token。token 只在内存中用于一次 GET 请求，不入库、不上传
/// 到 Metrik 之外的任何地方、不写日志。端点是 Claude Code 客户端自用的
/// 非官方接口（`/api/oauth/usage`），额度是账户级合并值（含网页版/桌面版
/// 消耗）；接口失效时如实报错，由上层回落到 statusLine 钩子文件，绝不编造
/// 数字。
///
/// 凭据来源按序尝试（不同平台/安装方式落点不同）：
/// 1. 环境变量 `CLAUDE_CODE_OAUTH_TOKEN`（用户显式指定的裸 token）；
/// 2. 凭据文件 `$CLAUDE_CONFIG_DIR|~/.claude` 下的 `.credentials.json`
///    （Linux、以及部分 Windows 安装）；
/// 3. macOS 钥匙串条目 `Claude Code-credentials`（macOS 上 Claude Code 默认
///    把 token 存进系统钥匙串而非明文文件）。读取经 `security` 命令，token
///    同样只在内存里用一次，不落盘。
const CREDENTIALS_FILE: &str = ".credentials.json";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const BETA_HEADER: &str = "oauth-2025-04-20";
const REQUIRED_SCOPE: &str = "user:profile";
/// 用户显式指定 token 的环境变量（与 Claude Code 官方同名）。
const ENV_TOKEN: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// Claude Code 在 macOS 钥匙串里存凭据用的 generic-password service 名。
#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
/// 覆盖 Claude 配置目录的环境变量（与 Claude Code 官方同名）。
const ENV_CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";

/// app_setting 里的开关键；"1" 表示用户已显式开启。
pub const SETTING_KEY: &str = "claude_oauth_quota_enabled";

pub const SOURCE_LABEL: &str = "官方额度（OAuth）";

#[derive(Deserialize)]
struct CredentialsFileShape {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<OauthCredentials>,
}

#[derive(Deserialize)]
struct OauthCredentials {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(default)]
    scopes: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeOauthStatus {
    pub enabled: bool,
    /// 本机存在 Claude Code 登录凭据文件且含 accessToken。
    pub credentials_present: bool,
    /// token 带 `user:profile` scope（用量端点必需）。
    pub scope_ok: bool,
}

#[derive(Deserialize)]
struct UsageResponse {
    five_hour: Option<UsageWindow>,
    seven_day: Option<UsageWindow>,
    /// 按模型的周限额（平铺字段，官方正逐步迁往 limits[]）。
    seven_day_opus: Option<UsageWindow>,
    seven_day_sonnet: Option<UsageWindow>,
    /// 新版格式：扁平的限额数组，每条可经 scope.model 标注所属模型
    /// （如促销期的模型专属周限）。同键时以这里的为准。
    limits: Option<Vec<LimitEntry>>,
    /// 超额付费用量（套餐外按量计费）；未开启时不产出窗口。
    extra_usage: Option<ExtraUsage>,
}

#[derive(Deserialize)]
struct UsageWindow {
    /// 已用百分比（0–100）。
    utilization: Option<f64>,
    /// ISO-8601 重置时刻。
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct LimitEntry {
    /// 已用百分比（0–100），与 UsageWindow.utilization 同义。
    percent: Option<f64>,
    resets_at: Option<String>,
    scope: Option<LimitScope>,
    is_active: Option<bool>,
}

#[derive(Deserialize)]
struct LimitScope {
    model: Option<LimitScopeModel>,
}

#[derive(Deserialize)]
struct LimitScopeModel {
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct ExtraUsage {
    is_enabled: Option<bool>,
    /// 已用超额预算的百分比（0–100）。
    utilization: Option<f64>,
}

impl LimitEntry {
    /// 归一成与平铺字段一致的窗口键：`seven_day_<模型名小写>`。
    /// 只认带模型 scope 的条目——不带 scope 的总量窗口平铺字段已经覆盖，
    /// 而 limits[] 里的分类键（kind/group）尚不稳定，不猜。
    fn window_key(&self) -> Option<String> {
        if self.is_active == Some(false) {
            return None;
        }
        let name = self
            .scope
            .as_ref()?
            .model
            .as_ref()?
            .display_name
            .as_deref()?;
        let slug = name
            .trim()
            .to_lowercase()
            .replace(|c: char| !c.is_ascii_alphanumeric(), "_");
        if slug.is_empty() {
            return None;
        }
        Some(format!("seven_day_{slug}"))
    }
}

pub struct ClaudeOauth {
    claude_dir: PathBuf,
    /// 是否咨询进程外的系统来源（环境变量、macOS 钥匙串）。生产环境为 true；
    /// 单测走 `with_dir`，仅读被测目录里的凭据文件，避免读到开发机真实凭据。
    consult_system: bool,
}

impl ClaudeOauth {
    pub fn detected() -> Self {
        Self {
            claude_dir: config_dir(),
            consult_system: true,
        }
    }

    #[cfg(test)]
    pub fn with_dir(claude_dir: PathBuf) -> Self {
        Self {
            claude_dir,
            consult_system: false,
        }
    }

    /// 按序尝试多个凭据来源，命中即返回；全部落空返回 None。
    fn read_credentials(&self) -> Option<OauthCredentials> {
        // 1. 用户显式指定的裸 token：直接采信，赋予必需 scope 以放行 scope 校验。
        if self.consult_system {
            if let Some(token) = env_token() {
                return Some(OauthCredentials {
                    access_token: Some(token),
                    scopes: vec![REQUIRED_SCOPE.to_owned()],
                });
            }
        }

        // 2. 明文凭据文件。
        if let Some(credentials) = std::fs::read_to_string(self.claude_dir.join(CREDENTIALS_FILE))
            .ok()
            .and_then(|raw| parse_credentials(&raw))
        {
            return Some(credentials);
        }

        // 3. macOS 钥匙串（Claude Code 在 mac 上的默认落点）。
        #[cfg(target_os = "macos")]
        if self.consult_system {
            if let Some(credentials) = read_macos_keychain().and_then(|raw| parse_credentials(&raw))
            {
                return Some(credentials);
            }
        }

        None
    }

    /// 只返回布尔状态，token 内容永不离开本函数所在进程的内存。
    pub fn status(&self, enabled: bool) -> ClaudeOauthStatus {
        let credentials = self.read_credentials();
        let scope_ok = credentials
            .as_ref()
            .is_some_and(|oauth| oauth.scopes.iter().any(|scope| scope == REQUIRED_SCOPE));
        ClaudeOauthStatus {
            enabled,
            credentials_present: credentials.is_some(),
            scope_ok,
        }
    }

    /// 拉取官方额度窗口：5h / 7d 总量、按模型周限（seven_day_opus 等平铺
    /// 字段与新版 limits[] 数组）、已开启的超额付费用量。窗口键与
    /// statusLine 钩子一致（five_hour / seven_day / seven_day_*），
    /// 下游展示无需区分来源。
    pub fn fetch_quota_samples(&self, timeout: Duration) -> Result<Vec<QuotaSample>> {
        let Some(credentials) = self.read_credentials() else {
            bail!("本机没有 Claude Code 登录凭据（环境变量、~/.claude/.credentials.json、macOS 钥匙串均未命中）");
        };
        if !credentials
            .scopes
            .iter()
            .any(|scope| scope == REQUIRED_SCOPE)
        {
            bail!("Claude 凭据缺少 user:profile 权限，无法查询用量。请重新运行 claude login");
        }
        let token = credentials.access_token.unwrap_or_default();

        let agent = ureq::AgentBuilder::new().timeout(timeout).build();
        let response = agent
            .get(USAGE_URL)
            .set("Authorization", &format!("Bearer {token}"))
            .set("Accept", "application/json")
            .set("anthropic-beta", BETA_HEADER)
            .set("User-Agent", "claude-code/2.1.0")
            .call()
            .map_err(|error| match error {
                // 错误信息里绝不能带请求头（token）。
                ureq::Error::Status(401, _) => {
                    anyhow::anyhow!("Claude 凭据已失效（401），请重新运行 claude login")
                }
                ureq::Error::Status(429, _) => {
                    anyhow::anyhow!("Claude 用量接口限流（429），稍后自动重试")
                }
                ureq::Error::Status(code, _) => {
                    anyhow::anyhow!("Claude 用量接口返回 HTTP {code}")
                }
                ureq::Error::Transport(transport) => {
                    anyhow::anyhow!("Claude 用量接口网络错误: {transport}")
                }
            })?;

        let body = response.into_string().context("读取 Claude 用量响应失败")?;
        let usage: UsageResponse =
            serde_json::from_str(&body).context("Claude 用量响应不是预期的 JSON")?;

        let samples = samples_from_usage(usage, chrono::Utc::now().timestamp_millis());
        if samples.is_empty() {
            bail!("Claude 用量响应缺少可用的额度窗口");
        }
        Ok(samples)
    }
}

/// Claude 配置目录：优先 `$CLAUDE_CONFIG_DIR`，否则 `~/.claude`。
fn config_dir() -> PathBuf {
    std::env::var_os(ENV_CONFIG_DIR)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude"))
}

/// 环境变量里的裸 token（去空白后非空才算）。
fn env_token() -> Option<String> {
    std::env::var(ENV_TOKEN)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// 把凭据 JSON（文件或钥匙串同一形状）解析成带非空 accessToken 的凭据。
fn parse_credentials(raw: &str) -> Option<OauthCredentials> {
    serde_json::from_str::<CredentialsFileShape>(raw.trim_start_matches('\u{feff}'))
        .ok()?
        .claude_ai_oauth
        .filter(|oauth| {
            oauth
                .access_token
                .as_deref()
                .is_some_and(|token| !token.trim().is_empty())
        })
}

/// 从 macOS 钥匙串取 Claude Code 存的凭据 JSON。`security -w` 只输出密码本体
/// （即那段 JSON）；条目不存在或被拒时命令非零退出，返回 None，绝不猜测。
/// 首次读取可能弹出系统钥匙串授权框，这是 macOS 的预期行为。
#[cfg(target_os = "macos")]
fn read_macos_keychain() -> Option<String> {
    let output = std::process::Command::new("security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// 把用量响应归一成额度样本：平铺窗口打底，limits[] 同键覆盖，
/// 超额付费（若开启）殿后。缺 utilization 的窗口一律丢弃，不编造数字。
fn samples_from_usage(usage: UsageResponse, now: i64) -> Vec<QuotaSample> {
    let mut windows: Vec<(String, UsageWindow)> = [
        ("five_hour", usage.five_hour),
        ("seven_day", usage.seven_day),
        ("seven_day_opus", usage.seven_day_opus),
        ("seven_day_sonnet", usage.seven_day_sonnet),
    ]
    .into_iter()
    .filter_map(|(key, window)| Some((key.to_owned(), window?)))
    .collect();

    // limits[] 与平铺字段可能描述同一窗口；官方正往 limits[] 迁移，同键以它为准。
    for entry in usage.limits.unwrap_or_default() {
        let Some(key) = entry.window_key() else {
            continue;
        };
        let window = UsageWindow {
            utilization: entry.percent,
            resets_at: entry.resets_at,
        };
        if let Some(existing) = windows
            .iter_mut()
            .find(|(existing_key, _)| *existing_key == key)
        {
            existing.1 = window;
        } else {
            windows.push((key, window));
        }
    }

    // 超额付费只在用户开启后单列一行；没开启就不占位，也不显示 0%。
    if let Some(extra) = usage.extra_usage {
        if extra.is_enabled == Some(true) && extra.utilization.is_some() {
            windows.push((
                "extra_usage".to_owned(),
                UsageWindow {
                    utilization: extra.utilization,
                    resets_at: None,
                },
            ));
        }
    }

    windows
        .into_iter()
        .filter_map(|(key, window)| {
            let used = window.utilization?;
            Some(QuotaSample {
                adapter_id: "claude",
                window_key: key,
                remaining_percent: (100.0 - used).clamp(0.0, 100.0),
                resets_at_ms: window.resets_at.as_deref().and_then(parse_iso8601_ms),
                collected_at_ms: now,
                source_label: SOURCE_LABEL.to_owned(),
                quality: "official_snapshot",
            })
        })
        .collect()
}

fn parse_iso8601_ms(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn status_reports_credentials_and_scope() {
        let dir = std::env::temp_dir().join(format!(
            "metrik-claude-oauth-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).unwrap();

        // 无凭据文件。
        let oauth = ClaudeOauth::with_dir(dir.clone());
        let status = oauth.status(false);
        assert!(!status.credentials_present);
        assert!(!status.scope_ok);

        // 有 token 但缺 user:profile。
        fs::write(
            dir.join(CREDENTIALS_FILE),
            r#"{"claudeAiOauth":{"accessToken":"sk-test","scopes":["user:inference"]}}"#,
        )
        .unwrap();
        let status = oauth.status(true);
        assert!(status.enabled);
        assert!(status.credentials_present);
        assert!(!status.scope_ok);

        // 完整 scope。
        fs::write(
            dir.join(CREDENTIALS_FILE),
            r#"{"claudeAiOauth":{"accessToken":"sk-test","scopes":["user:inference","user:profile"]}}"#,
        )
        .unwrap();
        assert!(oauth.status(true).scope_ok);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_credentials_reads_keychain_or_file_shape() {
        // 钥匙串 -w 输出与文件内容同形状，同一解析路径。
        let blob = r#"{"claudeAiOauth":{"accessToken":"sk-abc","scopes":["user:inference","user:profile"]}}"#;
        let parsed = parse_credentials(blob).expect("valid blob parses");
        assert_eq!(parsed.access_token.as_deref(), Some("sk-abc"));
        assert!(parsed.scopes.iter().any(|scope| scope == REQUIRED_SCOPE));

        // 前导 BOM 容忍。
        assert!(parse_credentials(&format!("\u{feff}{blob}")).is_some());

        // 空 token / 非 JSON / 缺字段一律 None，绝不返回半个凭据。
        assert!(parse_credentials(r#"{"claudeAiOauth":{"accessToken":"   "}}"#).is_none());
        assert!(parse_credentials(r#"{"claudeAiOauth":{}}"#).is_none());
        assert!(parse_credentials("not json").is_none());
    }

    #[test]
    fn with_dir_does_not_consult_system_sources() {
        // 单测构造器不看环境变量/钥匙串：空目录必然报告无凭据，
        // 即便开发机（如 macOS）钥匙串里有真实 Claude 凭据也不受影响。
        let dir = std::env::temp_dir().join(format!(
            "metrik-claude-oauth-isolation-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).unwrap();
        let status = ClaudeOauth::with_dir(dir.clone()).status(true);
        assert!(!status.credentials_present);
        assert!(!status.scope_ok);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn usage_response_maps_to_remaining_percent_samples() {
        let usage: UsageResponse = serde_json::from_str(
            r#"{
                "five_hour": {"utilization": 40.0, "resets_at": "2026-07-14T05:30:00.000Z"},
                "seven_day": {"utilization": 43.5, "resets_at": "2026-07-17T21:00:00Z"},
                "seven_day_opus": {"utilization": 12.0},
                "extra_usage": {"is_enabled": false}
            }"#,
        )
        .unwrap();
        let samples = samples_from_usage(usage, 1);
        assert_eq!(
            samples
                .iter()
                .map(|sample| sample.window_key.as_str())
                .collect::<Vec<_>>(),
            vec!["five_hour", "seven_day", "seven_day_opus"],
        );
        assert_eq!(samples[0].remaining_percent, 60.0);
        assert_eq!(samples[0].resets_at_ms, Some(1_784_007_000_000));
        assert_eq!(samples[1].remaining_percent, 56.5);
        // 未开启的超额付费不产出窗口。
        assert!(samples
            .iter()
            .all(|sample| sample.window_key != "extra_usage"));
    }

    #[test]
    fn limits_entries_override_flat_windows_and_add_scoped_models() {
        let usage: UsageResponse = serde_json::from_str(
            r#"{
                "five_hour": {"utilization": 10.0},
                "seven_day_opus": {"utilization": 12.0, "resets_at": "2026-07-17T21:00:00Z"},
                "limits": [
                    {"kind": "weekly_scoped", "group": "weekly", "percent": 30.0,
                     "resets_at": "2026-07-18T21:00:00Z",
                     "scope": {"model": {"id": "opus-4", "display_name": "Opus"}}},
                    {"kind": "weekly_scoped", "group": "weekly", "percent": 52.0,
                     "scope": {"model": {"display_name": "Fable"}}},
                    {"kind": "weekly_scoped", "group": "weekly", "percent": 99.0,
                     "is_active": false,
                     "scope": {"model": {"display_name": "Haiku"}}},
                    {"kind": "weekly", "group": "weekly", "percent": 44.0}
                ],
                "extra_usage": {"is_enabled": true, "utilization": 7.5}
            }"#,
        )
        .unwrap();
        let samples = samples_from_usage(usage, 1);
        let keys = samples
            .iter()
            .map(|sample| sample.window_key.as_str())
            .collect::<Vec<_>>();
        // 同键覆盖（opus 用 limits 值）、新增 scoped 模型（fable）、
        // 跳过 is_active=false（haiku）与不带模型 scope 的条目、追加超额付费。
        assert_eq!(
            keys,
            vec![
                "five_hour",
                "seven_day_opus",
                "seven_day_fable",
                "extra_usage"
            ],
        );
        let opus = &samples[1];
        assert_eq!(opus.remaining_percent, 70.0);
        assert_eq!(opus.resets_at_ms, parse_iso8601_ms("2026-07-18T21:00:00Z"));
        assert_eq!(samples[3].remaining_percent, 92.5);
    }
}
