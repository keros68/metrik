use crate::domain::{sane_resets_at_ms, QuotaSample};
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
/// 3. macOS 钥匙串（macOS 上 Claude Code 默认把 token 存进系统钥匙串而非
///    明文文件）：先试旧 service 名 `Claude Code-credentials`，未命中再从
///    `security dump-keychain` 的条目里找 v2.1.52+ 的
///    `Claude Code-credentials-<hash>`。读取经 `security` 命令，token 同样
///    只在内存里用一次，不落盘。
///
/// access token 只活几小时，且只有 Claude Code 自己跑起来才会刷新：真机实测
/// `claude auth status --json` 退出码 0、报告已登录，钥匙串里的 `expiresAt`
/// 却纹丝不动（过期十小时后依旧是同一个时刻），而 `claude auth` 下只有
/// login / logout / status，没有刷新入口。凭据里那把 refreshToken 我们不动：
/// OAuth 刷新令牌通常一次性，我们换了却不写回，用户真正在用的 Claude Code
/// 登录就可能被顶掉——为看一个额度数字不值得。
///
/// 因此过期即如实说明并回落到 statusLine 钩子。这个功能有个前提：最近用过
/// Claude Code。设置页把这句写在前面，别让人撞了 401 再猜。
const CREDENTIALS_FILE: &str = ".credentials.json";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const BETA_HEADER: &str = "oauth-2025-04-20";
const REQUIRED_SCOPE: &str = "user:profile";
/// 用户显式指定 token 的环境变量（与 Claude Code 官方同名）。
const ENV_TOKEN: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// Claude Code 在 macOS 钥匙串里存凭据用的 generic-password service 名。
/// v2.1.52 起改成 `Claude Code-credentials-<hash>`，哈希不可推导，只能从
/// 钥匙串条目里找；旧名仍在用，两者都要试。
#[cfg(any(target_os = "macos", test))]
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
/// 覆盖 Claude 配置目录的环境变量（与 Claude Code 官方同名）。
const ENV_CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";

/// app_setting 里的开关键；"1" 表示用户已显式开启。
pub const SETTING_KEY: &str = "claude_oauth_quota_enabled";

/// 最近一次直连查询失败的原因。失败被静默吞掉时用户只会看到"没有额度"，
/// 却拿不到任何可行动的信息（凭据过期？缺 scope？限流？），所以如实留档。
/// 存的是本模块自己生成的错误文案，不含 token、请求头或响应体。
const LAST_ERROR_SETTING_KEY: &str = "claude_oauth_last_error";

pub const SOURCE_LABEL: &str = "官方配额（OAuth）";

/// 直连的最近一次失败，供设置页与配额卡如实说明为什么没有数字。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeOauthFailure {
    pub at_ms: i64,
    pub message: String,
}

pub fn record_failure(connection: &rusqlite::Connection, message: &str) -> Result<()> {
    let failure = ClaudeOauthFailure {
        at_ms: chrono::Utc::now().timestamp_millis(),
        message: message.to_owned(),
    };
    let raw =
        serde_json::to_string(&failure).context("failed to serialize claude oauth failure")?;
    crate::storage::set_app_setting(connection, LAST_ERROR_SETTING_KEY, &raw)
}

pub fn clear_failure(connection: &rusqlite::Connection) -> Result<()> {
    crate::storage::set_app_setting(connection, LAST_ERROR_SETTING_KEY, "")
}

pub fn last_failure(connection: &rusqlite::Connection) -> Result<Option<ClaudeOauthFailure>> {
    let Some(raw) = crate::storage::get_app_setting(connection, LAST_ERROR_SETTING_KEY)? else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    // 记录损坏时当作没有记录，不因为一条诊断把设置页打不开。
    Ok(serde_json::from_str(&raw).ok())
}

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
    /// 过期时刻。Claude Code 写的是毫秒，但字段名与单位在不同版本间变过，
    /// 两种命名都收，秒/毫秒按量级判断。
    #[serde(default, rename = "expiresAt", alias = "expires_at")]
    expires_at: Option<i64>,
}

impl OauthCredentials {
    /// token 是否已过期。读不到过期时刻就当作未过期——不能因为少一个字段
    /// 就把一份可用的凭据判死。
    fn is_expired(&self, now_ms: i64) -> bool {
        let Some(raw) = self.expires_at else {
            return false;
        };
        // 1e11 毫秒约合 1973 年，秒则约合公元 5138 年：两种单位不会混淆。
        let at_ms = if raw.abs() < 100_000_000_000 {
            raw.saturating_mul(1000)
        } else {
            raw
        };
        at_ms <= now_ms
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeOauthStatus {
    pub enabled: bool,
    /// 本机存在 Claude Code 登录凭据文件且含 accessToken。
    pub credentials_present: bool,
    /// token 带 `user:profile` scope（用量端点必需）。
    pub scope_ok: bool,
    /// token 已过期。过期的凭据同样"存在"，只报 credentials_present 会让
    /// 设置页写着「凭据可用」而实际每次查询都被拒。
    pub expired: bool,
    /// 最近一次直连查询失败；开关本身正常时，问题只会出现在这里。
    pub last_failure: Option<ClaudeOauthFailure>,
}

impl ClaudeOauthStatus {
    pub fn with_failure(mut self, failure: Option<ClaudeOauthFailure>) -> Self {
        self.last_failure = failure;
        self
    }
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
    utilization: Option<serde_json::Value>,
    /// ISO-8601 或 Unix 秒/毫秒重置时刻。
    resets_at: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct LimitEntry {
    kind: Option<String>,
    group: Option<String>,
    /// 已用百分比（0–100），与 UsageWindow.utilization 同义。
    percent: Option<serde_json::Value>,
    resets_at: Option<serde_json::Value>,
    scope: Option<LimitScope>,
    is_active: Option<bool>,
}

#[derive(Deserialize)]
struct LimitScope {
    model: Option<LimitScopeModel>,
    surface: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct LimitScopeModel {
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct ExtraUsage {
    is_enabled: Option<bool>,
    /// 已用超额预算的百分比（0–100）。
    utilization: Option<serde_json::Value>,
}

impl LimitEntry {
    /// 按明确的周期和模型分类；未知或限定使用入口的窗口不能当作账户总量。
    fn window_key(&self) -> Option<String> {
        let kind = self.kind.as_deref().unwrap_or_default();
        let group = self.group.as_deref().unwrap_or_default();
        let scope = self.scope.as_ref();
        if scope.is_some_and(|scope| scope.surface.is_some()) {
            return None;
        }
        let model = scope.and_then(|scope| scope.model.as_ref());
        if kind == "session" && (group.is_empty() || group == "session") && model.is_none() {
            return Some("five_hour".into());
        }
        let legacy_model_scope = kind.is_empty() && group.is_empty() && model.is_some();
        if !legacy_model_scope
            && (group != "weekly" || !matches!(kind, "weekly" | "weekly_all" | "weekly_scoped"))
        {
            return None;
        }
        let Some(model) = model else {
            return matches!(kind, "weekly" | "weekly_all").then(|| "seven_day".into());
        };
        let name = model.display_name.as_deref()?.trim();
        let name = name.strip_prefix("Claude ").unwrap_or(name);
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
                    // 用户自己给的裸 token，没有过期信息可依据。
                    expires_at: None,
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
        let now_ms = chrono::Utc::now().timestamp_millis();
        let expired = credentials
            .as_ref()
            .is_some_and(|oauth| oauth.is_expired(now_ms));
        ClaudeOauthStatus {
            enabled,
            credentials_present: credentials.is_some(),
            scope_ok,
            expired,
            last_failure: None,
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
        // 过期就别发那个注定被拒的请求：刷新只有 Claude Code 自己做得到。
        if credentials.is_expired(chrono::Utc::now().timestamp_millis()) {
            bail!("Claude 凭据已过期，运行一次 Claude Code 即可自动刷新");
        }
        self.request_usage(&credentials, timeout)
    }

    fn request_usage(
        &self,
        credentials: &OauthCredentials,
        timeout: Duration,
    ) -> Result<Vec<QuotaSample>> {
        if !credentials
            .scopes
            .iter()
            .any(|scope| scope == REQUIRED_SCOPE)
        {
            bail!("Claude 凭据缺少 user:profile 权限，无法查询用量。重新运行 claude login");
        }
        let token = credentials.access_token.clone().unwrap_or_default();

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
                    anyhow::anyhow!("Claude 凭据被拒（401），重新运行 claude login")
                }
                ureq::Error::Status(429, _) => {
                    anyhow::anyhow!("Claude 用量接口限流（429），稍后自动重试")
                }
                ureq::Error::Status(code, _) => {
                    anyhow::anyhow!("Claude 用量接口返回 HTTP {code}")
                }
                ureq::Error::Transport(transport) => {
                    anyhow::anyhow!("Claude 用量接口网络错误：{transport}")
                }
            })?;

        let parse = || -> Result<Vec<QuotaSample>> {
            let body = response.into_string().context("读取 Claude 用量响应失败")?;
            let usage: UsageResponse =
                serde_json::from_str(&body).context("Claude 用量响应不是预期的 JSON")?;
            let samples = samples_from_usage(usage, chrono::Utc::now().timestamp_millis());
            if samples.is_empty() {
                bail!("Claude 用量响应缺少可用的额度窗口");
            }
            Ok(samples)
        };
        parse()
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

/// 从 macOS 钥匙串取 Claude Code 存的凭据 JSON。先试沿用至今的旧 service 名；
/// 未命中再从钥匙串条目里找 v2.1.52+ 的 `Claude Code-credentials-<hash>`。
/// 只试旧名会让装了较新 Claude Code 的 Mac 用户明明登录过却被判为"未找到凭据"。
#[cfg(target_os = "macos")]
fn read_macos_keychain() -> Option<String> {
    if let Some(raw) = keychain_password(KEYCHAIN_SERVICE) {
        return Some(raw);
    }
    for service in discover_keychain_services() {
        if let Some(raw) = keychain_password(&service) {
            return Some(raw);
        }
    }
    None
}

/// `security -w` 只输出密码本体（即那段 JSON）；条目不存在或被拒时命令非零
/// 退出，返回 None，绝不猜测。首次读取可能弹出系统钥匙串授权框，这是 macOS
/// 的预期行为。
#[cfg(target_os = "macos")]
fn keychain_password(service: &str) -> Option<String> {
    let output = std::process::Command::new("security")
        .args(["find-generic-password", "-s", service, "-w"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// 列出钥匙串里所有 Claude Code 的 generic-password service 名。
/// `dump-keychain` 不带 `-d` 只列元数据、不读密码本体，因此不会弹密码框。
#[cfg(target_os = "macos")]
fn discover_keychain_services() -> Vec<String> {
    let Ok(output) = std::process::Command::new("security")
        .arg("dump-keychain")
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    keychain_services_from_dump(&String::from_utf8_lossy(&output.stdout))
}

/// 从 `dump-keychain` 的输出里挑出 Claude Code 的 service 名。条目行形如
/// `    "svce"<blob>="Claude Code-credentials-<hash>"`。旧名已经单独试过，
/// 这里排掉，避免白跑一次。
///
/// 解析与平台无关，故不加 `cfg(macos)` 门——否则在 Windows 上连测都测不了，
/// 而这段正是最需要回归保护的部分。
#[cfg(any(target_os = "macos", test))]
fn keychain_services_from_dump(dump: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for line in dump.lines() {
        let Some(rest) = line.trim().strip_prefix("\"svce\"<blob>=\"") else {
            continue;
        };
        let Some(service) = rest.strip_suffix('"') else {
            continue;
        };
        if service.starts_with(KEYCHAIN_SERVICE)
            && service != KEYCHAIN_SERVICE
            && !found.iter().any(|seen| seen == service)
        {
            found.push(service.to_owned());
        }
    }
    found
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
        if entry.is_active == Some(false) {
            windows.retain(|(existing_key, _)| *existing_key != key);
            continue;
        }
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
            let used = quota_number(window.utilization.as_ref()?)?;
            if !(0.0..=100.0).contains(&used) {
                return None;
            }
            Some(QuotaSample {
                adapter_id: "claude",
                window_key: key.clone(),
                remaining_percent: (100.0 - used).clamp(0.0, 100.0),
                resets_at_ms: window
                    .resets_at
                    .as_ref()
                    .and_then(parse_reset_ms)
                    .and_then(|value| sane_resets_at_ms(&key, value, now)),
                collected_at_ms: now,
                source_label: SOURCE_LABEL.to_owned(),
                quality: "official_snapshot",
            })
        })
        .collect()
}

fn quota_number(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|value| value.is_finite())
}

fn parse_reset_ms(value: &serde_json::Value) -> Option<i64> {
    if let Some(parsed) = value.as_str().and_then(parse_iso8601_ms) {
        return Some(parsed);
    }
    let timestamp = quota_number(value)?;
    let millis = if timestamp.abs() < 100_000_000_000.0 {
        timestamp * 1000.0
    } else {
        timestamp
    };
    (millis > 0.0 && millis < i64::MAX as f64).then_some(millis as i64)
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

    fn memory_ledger() -> rusqlite::Connection {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        connection
    }

    #[test]
    fn failure_is_recorded_until_a_later_success_clears_it() {
        let connection = memory_ledger();
        assert!(last_failure(&connection).unwrap().is_none());

        record_failure(
            &connection,
            "Claude 凭据已失效（401），重新运行 claude login",
        )
        .unwrap();
        let recorded = last_failure(&connection).unwrap().unwrap();
        assert_eq!(
            recorded.message,
            "Claude 凭据已失效（401），重新运行 claude login"
        );
        assert!(recorded.at_ms > 0);

        // 后一次失败覆盖前一次：展示的必须是当前的问题。
        record_failure(&connection, "Claude 用量接口限流（429），稍后自动重试").unwrap();
        assert_eq!(
            last_failure(&connection).unwrap().unwrap().message,
            "Claude 用量接口限流（429），稍后自动重试"
        );

        clear_failure(&connection).unwrap();
        assert!(last_failure(&connection).unwrap().is_none());
    }

    /// Claude Code v2.1.52 起把凭据存进带哈希后缀的 service 名。开发机是
    /// Windows，这段只能靠样本回归——真机行为需在 macOS 上另行核实。
    #[test]
    fn keychain_dump_yields_hashed_service_names_only() {
        let dump = r#"
keychain: "/Users/dev/Library/Keychains/login.keychain-db"
class: "genp"
attributes:
    0x00000007 <blob>="Claude Code-credentials-a1b2c3"
    "acct"<blob>="unknown"
    "svce"<blob>="Claude Code-credentials"
class: "genp"
attributes:
    "acct"<blob>="dev"
    "svce"<blob>="Claude Code-credentials-a1b2c3"
class: "genp"
attributes:
    "svce"<blob>="Claude Code-credentials-a1b2c3"
class: "genp"
attributes:
    "svce"<blob>="com.apple.assistant"
"#;
        // 旧名单独试过所以排除；标签行（0x00000007）不算；重复项去掉。
        assert_eq!(
            keychain_services_from_dump(dump),
            vec!["Claude Code-credentials-a1b2c3".to_owned()]
        );
    }

    #[test]
    fn keychain_dump_without_claude_entries_is_empty() {
        assert!(keychain_services_from_dump("\"svce\"<blob>=\"com.apple.assistant\"").is_empty());
        assert!(keychain_services_from_dump("").is_empty());
    }

    #[test]
    fn expiry_accepts_both_seconds_and_milliseconds() {
        let now_ms = 1_785_800_000_000_i64;
        let credentials = |expires_at| OauthCredentials {
            access_token: Some("t".into()),
            scopes: vec![REQUIRED_SCOPE.to_owned()],
            expires_at,
        };

        // 毫秒
        assert!(credentials(Some(now_ms - 1)).is_expired(now_ms));
        assert!(!credentials(Some(now_ms + 60_000)).is_expired(now_ms));
        // 秒
        assert!(credentials(Some(now_ms / 1000 - 1)).is_expired(now_ms));
        assert!(!credentials(Some(now_ms / 1000 + 60)).is_expired(now_ms));
        // 读不到过期时刻不判死，交给服务端裁决。
        assert!(!credentials(None).is_expired(now_ms));
    }

    #[test]
    fn corrupt_failure_record_reads_as_no_failure() {
        let connection = memory_ledger();
        crate::storage::set_app_setting(&connection, LAST_ERROR_SETTING_KEY, "not json").unwrap();
        assert!(last_failure(&connection).unwrap().is_none());
    }

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

    /// 过期凭据不能既报"可用"又每次都被拒：状态要说实话，查询要早退。
    /// 早退还顺带保证了这条用例不碰网络——过期分支在请求之前。
    #[test]
    fn expired_credentials_are_reported_and_short_circuit_the_request() {
        let dir = std::env::temp_dir().join(format!(
            "metrik-claude-oauth-expired-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).unwrap();
        let oauth = ClaudeOauth::with_dir(dir.clone());

        let past = chrono::Utc::now().timestamp_millis() - 60_000;
        fs::write(
            dir.join(CREDENTIALS_FILE),
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"sk-test","scopes":["user:profile"],"expiresAt":{past}}}}}"#
            ),
        )
        .unwrap();
        let status = oauth.status(true);
        assert!(status.credentials_present);
        assert!(status.scope_ok);
        assert!(status.expired);

        let error = oauth
            .fetch_quota_samples(Duration::from_secs(1))
            .expect_err("expired credentials must not be sent to the endpoint");
        assert!(
            error.to_string().contains("已过期"),
            "unexpected error: {error}"
        );

        // 未过期的凭据不该被误判——过期判定只认过去的时刻。
        let future = chrono::Utc::now().timestamp_millis() + 3_600_000;
        fs::write(
            dir.join(CREDENTIALS_FILE),
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"sk-test","scopes":["user:profile"],"expiresAt":{future}}}}}"#
            ),
        )
        .unwrap();
        assert!(!oauth.status(true).expired);

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
        // 采集时刻用真实量级（2026-07-14T00:00Z）：重置时间的合理性校验
        // 依赖它与重置时刻的相对关系。
        let samples = samples_from_usage(usage, 1_783_987_200_000);
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
        let samples = samples_from_usage(usage, 1_783_987_200_000);
        let keys = samples
            .iter()
            .map(|sample| sample.window_key.as_str())
            .collect::<Vec<_>>();
        // 同键覆盖（opus 用 limits 值）、新增 scoped 模型（fable）、
        // 跳过 is_active=false（haiku）、识别总周限、追加超额付费。
        assert_eq!(
            keys,
            vec![
                "five_hour",
                "seven_day_opus",
                "seven_day_fable",
                "seven_day",
                "extra_usage"
            ],
        );
        let opus = &samples[1];
        assert_eq!(opus.remaining_percent, 70.0);
        assert_eq!(opus.resets_at_ms, parse_iso8601_ms("2026-07-18T21:00:00Z"));
        assert_eq!(samples[4].remaining_percent, 92.5);
    }

    #[test]
    fn array_only_windows_keep_percentage_units_and_parse_reset_formats() {
        let usage = serde_json::from_value(serde_json::json!({"limits": [
            {"kind":"session", "group":"session", "percent":0.5, "resets_at":1784007000},
            {"kind":"weekly_all", "group":"weekly", "percent":"18", "resets_at":"1784322000000"},
            {"kind":"weekly_scoped", "group":"weekly", "percent":9,
             "scope":{"model":{"display_name":"Claude Sonnet"},"surface":null}}
        ]}))
        .unwrap();
        let samples = samples_from_usage(usage, 1_783_987_200_000);
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0].remaining_percent, 99.5);
        assert_eq!(samples[0].resets_at_ms, Some(1_784_007_000_000));
        assert_eq!(samples[1].window_key, "seven_day");
        assert_eq!(samples[1].remaining_percent, 82.0);
        assert_eq!(samples[1].resets_at_ms, Some(1_784_322_000_000));
        assert_eq!(samples[2].window_key, "seven_day_sonnet");
    }

    #[test]
    fn inactive_windows_remove_flat_fallback_and_unknown_scopes_do_not_become_totals() {
        let usage = serde_json::from_value(serde_json::json!({
            "five_hour":{"utilization":10}, "seven_day":{"utilization":20},
            "seven_day_sonnet":{"utilization":30},
            "limits":[
                {"kind":"weekly_all","group":"weekly","percent":0,"is_active":false},
                {"kind":"weekly_scoped","group":"weekly","is_active":false,
                 "scope":{"model":{"display_name":"Sonnet"}}},
                {"kind":"weekly_scoped","group":"weekly","percent":90},
                {"kind":"weekly_all","group":"weekly","percent":90,
                 "scope":{"surface":{"display_name":"Cowork"}}},
                {"kind":"future_kind","group":"weekly","percent":90}
            ]
        }))
        .unwrap();
        let samples = samples_from_usage(usage, 1_783_987_200_000);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].window_key, "five_hour");
    }

    #[test]
    fn invalid_or_missing_percentages_do_not_become_zero_usage() {
        for percent in [
            serde_json::Value::Null,
            serde_json::json!("bad"),
            serde_json::json!(-1),
            serde_json::json!(101),
        ] {
            let usage = serde_json::from_value(serde_json::json!({"limits":[
                {"kind":"session","group":"session","percent":percent}
            ]}))
            .unwrap();
            assert!(samples_from_usage(usage, 1_783_987_200_000).is_empty());
        }
    }

    #[test]
    fn model_only_legacy_entry_remains_supported() {
        let usage = serde_json::from_value(serde_json::json!({"limits":[
            {"percent":25,"scope":{"model":{"display_name":"Fable"}}}
        ]}))
        .unwrap();
        let samples = samples_from_usage(usage, 1_783_987_200_000);
        assert_eq!(samples[0].window_key, "seven_day_fable");
        assert_eq!(samples[0].remaining_percent, 75.0);
    }

    #[test]
    fn absurd_resets_at_is_dropped() {
        // 与 statusLine 钩子同源的哨兵：重置时间未知时后端可能下发远未来
        // 时间（实测 2030 年），超出窗口语义的一律丢弃，不显示"1331 天后重置"。
        let usage: UsageResponse = serde_json::from_str(
            r#"{"five_hour": {"utilization": 42.3, "resets_at": "2030-03-17T07:23:20Z"}}"#,
        )
        .unwrap();
        let samples = samples_from_usage(usage, 1_783_987_200_000);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].resets_at_ms, None);
        // 百分比不受影响，照常展示。
        assert!((samples[0].remaining_percent - 57.7).abs() < f64::EPSILON);
    }
}
