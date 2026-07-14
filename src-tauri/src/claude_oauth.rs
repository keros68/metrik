use crate::domain::QuotaSample;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Claude 官方额度的 opt-in 凭据来源。
///
/// 隐私红线：默认关闭；用户在设置页显式开启后，才读取 Claude Code 自己
/// 保存的 OAuth token（`~/.claude/.credentials.json`）。token 只在内存中
/// 用于一次 GET 请求，不入库、不上传到 Metrik 之外的任何地方、不写日志。
/// 端点是 Claude Code 客户端自用的非官方接口（`/api/oauth/usage`），额度
/// 是账户级合并值（含网页版/桌面版消耗）；接口失效时如实报错，由上层
/// 回落到 statusLine 钩子文件，绝不编造数字。
const CREDENTIALS_FILE: &str = ".credentials.json";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const BETA_HEADER: &str = "oauth-2025-04-20";
const REQUIRED_SCOPE: &str = "user:profile";

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
}

#[derive(Deserialize)]
struct UsageWindow {
    /// 已用百分比（0–100）。
    utilization: Option<f64>,
    /// ISO-8601 重置时刻。
    resets_at: Option<String>,
}

pub struct ClaudeOauth {
    claude_dir: PathBuf,
}

impl ClaudeOauth {
    pub fn detected() -> Self {
        Self {
            claude_dir: dirs::home_dir().unwrap_or_default().join(".claude"),
        }
    }

    #[cfg(test)]
    pub fn with_dir(claude_dir: PathBuf) -> Self {
        Self { claude_dir }
    }

    fn read_credentials(&self) -> Option<OauthCredentials> {
        let raw = std::fs::read_to_string(self.claude_dir.join(CREDENTIALS_FILE)).ok()?;
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

    /// 拉取官方 5h / 7d 窗口。窗口键与 statusLine 钩子一致（five_hour /
    /// seven_day），下游展示无需区分来源。
    pub fn fetch_quota_samples(&self, timeout: Duration) -> Result<Vec<QuotaSample>> {
        let Some(credentials) = self.read_credentials() else {
            bail!("本机没有 Claude Code 登录凭据（~/.claude/.credentials.json）");
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

        let now = chrono::Utc::now().timestamp_millis();
        let samples = [
            ("five_hour", usage.five_hour),
            ("seven_day", usage.seven_day),
        ]
        .into_iter()
        .filter_map(|(key, window)| {
            let window = window?;
            let used = window.utilization?;
            Some(QuotaSample {
                adapter_id: "claude",
                window_key: key.to_owned(),
                remaining_percent: (100.0 - used).clamp(0.0, 100.0),
                resets_at_ms: window.resets_at.as_deref().and_then(parse_iso8601_ms),
                collected_at_ms: now,
                source_label: SOURCE_LABEL.to_owned(),
                quality: "official_snapshot",
            })
        })
        .collect::<Vec<_>>();

        if samples.is_empty() {
            bail!("Claude 用量响应缺少 five_hour/seven_day 窗口");
        }
        Ok(samples)
    }
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
        assert_eq!(usage.five_hour.as_ref().unwrap().utilization, Some(40.0));
        assert_eq!(
            parse_iso8601_ms(usage.five_hour.unwrap().resets_at.as_deref().unwrap()),
            Some(1_784_007_000_000)
        );
        assert_eq!(usage.seven_day.unwrap().utilization, Some(43.5));
    }
}
