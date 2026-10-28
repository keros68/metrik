//! Grok Bot 桌面应用的共享额度池（Cursor 统一额度，opt-in）。
//!
//! Grok Bot 不在本地写逐轮 usage：`sand-client-persistence` 里的会话转录
//! 只有 UI 层消息（无 usage/model 字段，2026-10 实测），推理全部在云端，
//! 所以这张卡只承载配额。额度池是 SuperGrok / Grok Bot / 新版 Cursor
//! 共用的账号级统一池，桌面端自己用 Cursor 账号调
//! `api2.cursor.sh/aiserver.v1.DashboardService/GetSandUsageStatus` 显示
//! 用量百分比；本模块只读复刻同一条链路（2026-10-28 真机验证 200，且
//! 消融确认该端点只校验 Bearer，桌面客户端校验头全部可省）。
//!
//! 凭据链（全部只读）：
//! 1. `<数据目录>/sand-secrets.json` → `cursor-accounts`（内嵌明文 JSON，
//!    值为 Electron safeStorage 密文）→ active 账号的 `cursor-access-token`；
//! 2. `<数据目录>/Local State` → `os_crypt.encrypted_key` → DPAPI →
//!    32 字节 AES key（仅 Windows；macOS 在钥匙串、Linux 在 kwallet，
//!    尚未支持）；
//! 3. 密文格式 base64 → `v10` + 12 字节 nonce + AES-256-GCM(密文 + 16 字节
//!    tag)，解出 JWT（实测寿命约 43 天）。
//!
//! 隐私红线（与 Claude OAuth 同款）：默认关闭，用户显式开启后才读取；token
//! 只在内存中用于一次请求，不刷新、不落库、不写日志、不同步。token 由
//! Grok Bot 自己运行时刷新，盘上副本保持新鲜；被拒即如实报错，绝不估算。

use crate::domain::{sane_resets_at_ms, QuotaSample};
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;

/// app_setting 里的开关键；"1" 表示用户已显式开启。
pub const QUOTA_SETTING_KEY: &str = "grokbot_quota_enabled";

const SECRETS_FILE: &str = "sand-secrets.json";
const LOCAL_STATE_FILE: &str = "Local State";
const USAGE_URL: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService/GetSandUsageStatus";
const SOURCE_LABEL: &str = "Grok Bot 官方配额";
/// 与 Grok CLI 的 SuperGrok 周窗同一口径：实测 `currentPeriodStart` 到
/// `nextResetTimestampUtc` 恰为 7 天。
const WINDOW_KEY: &str = "seven_day";

/// Grok Bot 的应用数据目录（Electron userData）。Windows 是
/// `%APPDATA%/Grok Bot`，macOS 是 `~/Library/Application Support/Grok Bot`。
pub fn grokbot_data_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("Grok Bot")
}

pub fn secrets_path() -> PathBuf {
    grokbot_data_dir().join(SECRETS_FILE)
}

/// 只读状态，供设置页展示；不触碰密文内容，不解密。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokBotQuotaStatus {
    pub enabled: bool,
    pub installed: bool,
    /// `sand-secrets.json` 里有 active 账号即视为已登录（有 token 可用）。
    pub signed_in: bool,
}

pub fn quota_status(enabled: bool) -> GrokBotQuotaStatus {
    let text = std::fs::read_to_string(secrets_path()).ok();
    GrokBotQuotaStatus {
        enabled,
        installed: grokbot_data_dir().exists(),
        signed_in: text.as_deref().and_then(parse_sand_secrets).is_some(),
    }
}

/// `sand-secrets.json` 里有 active 账号且带 token 即视为已登录。
pub fn signed_in() -> bool {
    std::fs::read_to_string(secrets_path())
        .ok()
        .as_deref()
        .and_then(parse_sand_secrets)
        .is_some()
}

/// 一次额度拉取。调用方负责开关判断；没有凭据、解不开密文或被服务端
/// 拒绝时如实报错，返回空窗口或零值都不允许。
pub fn fetch_quota(timeout: Duration) -> Result<Vec<QuotaSample>> {
    let secrets_text = std::fs::read_to_string(secrets_path())
        .with_context(|| format!("无法读取 {SECRETS_FILE}（Grok Bot 未安装或未登录？）"))?;
    let secrets = parse_sand_secrets(&secrets_text)
        .context("Grok Bot 凭据文件里没有可用的 Cursor 账号，先在 Grok Bot 里登录")?;
    let key = derive_aes_key()?;
    let token = decrypt_v10(&key, &secrets.access_token)
        .context("解密 Grok Bot 凭据失败（safeStorage 密钥与本机不匹配？）")?;

    let response = ureq::AgentBuilder::new()
        .timeout(timeout)
        .build()
        .post(USAGE_URL)
        .set("content-type", "application/json")
        .set("connect-protocol-version", "1")
        .set("authorization", &format!("Bearer {token}"))
        .send_string("{}");
    let response = match response {
        Ok(response) => response,
        Err(ureq::Error::Status(401 | 403, _)) => {
            bail!("Grok Bot 凭据被拒，打开 Grok Bot 重新登录后再试")
        }
        Err(ureq::Error::Status(code, _)) => {
            bail!("Grok Bot 额度暂时读不到（HTTP {code}）")
        }
        Err(ureq::Error::Transport(_)) => bail!("Grok Bot 额度暂时读不到（网络错误）"),
    };
    let text = response
        .into_string()
        .context("读取 Grok Bot 额度响应失败")?;
    Ok(vec![parse_usage_response(&text)?])
}

// ---------- 凭据文件解析（纯函数，跨平台可测） ----------

#[derive(Debug, PartialEq)]
struct SandSecrets {
    access_token: String,
}

/// `sand-secrets.json` 顶层是普通 JSON，但 `cursor-accounts` 的值是**再嵌
/// 一层 JSON 的字符串**（外层键明文、账号字段是 safeStorage 密文）。这里
/// 只取 active 账号的密文，不解密。
fn parse_sand_secrets(text: &str) -> Option<SandSecrets> {
    #[derive(Deserialize)]
    struct Outer {
        #[serde(rename = "cursor-accounts")]
        cursor_accounts: Option<String>,
    }
    #[derive(Deserialize)]
    struct Accounts {
        active: Option<String>,
        accounts: Option<std::collections::HashMap<String, Account>>,
    }
    #[derive(Deserialize)]
    struct Account {
        #[serde(rename = "cursor-access-token")]
        cursor_access_token: Option<String>,
    }
    let outer = serde_json::from_str::<Outer>(text).ok()?;
    let inner_text = outer.cursor_accounts?;
    let mut inner = serde_json::from_str::<Accounts>(&inner_text).ok()?;
    let active = inner.active?;
    let access_token = inner
        .accounts
        .as_mut()?
        .remove(&active)?
        .cursor_access_token?
        .trim()
        .to_owned();
    if access_token.is_empty() {
        return None;
    }
    Some(SandSecrets { access_token })
}

/// `Local State` → `os_crypt.encrypted_key`（base64，前缀 `DPAPI`）→
/// 返回仍被 DPAPI 包着的密钥体。
fn parse_local_state_key(text: &str) -> Option<Vec<u8>> {
    #[derive(Deserialize)]
    struct LocalState {
        #[serde(rename = "os_crypt")]
        os_crypt: Option<OsCrypt>,
    }
    #[derive(Deserialize)]
    struct OsCrypt {
        encrypted_key: Option<String>,
    }
    let state = serde_json::from_str::<LocalState>(text).ok()?;
    let encoded = state.os_crypt?.encrypted_key?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    raw.strip_prefix(b"DPAPI").map(<[u8]>::to_vec)
}

/// 解一层 safeStorage 密文：base64 → `v10` + 12B nonce + AES-256-GCM
/// (密文 + 16B tag) → UTF-8 明文。
fn decrypt_v10(key: &[u8; 32], encoded: &str) -> Result<String> {
    use aes_gcm::aead::Aead;
    use aes_gcm::KeyInit;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .context("safeStorage 密文不是合法 base64")?;
    if raw.len() < 3 + 12 + 16 || &raw[..3] != b"v10" {
        bail!("不是预期的 v10 safeStorage 密文格式");
    }
    let cipher = aes_gcm::Aes256Gcm::new(key.into());
    let plain = cipher
        .decrypt(raw[3..15].into(), &raw[15..])
        .map_err(|_| anyhow::anyhow!("AES-GCM 解密失败（密钥不匹配或密文损坏）"))?;
    String::from_utf8(plain).context("safeStorage 明文不是 UTF-8")
}

/// Windows：读 `Local State` 并用当前用户 DPAPI 解出 32 字节 AES key。
/// macOS（钥匙串）与 Linux（kwallet/libsecret）尚未支持，如实报错。
fn derive_aes_key() -> Result<[u8; 32]> {
    #[cfg(windows)]
    {
        let text = std::fs::read_to_string(grokbot_data_dir().join(LOCAL_STATE_FILE))
            .context("无法读取 Grok Bot 的 Local State（Chromium 加密密钥所在文件）")?;
        let encrypted = parse_local_state_key(&text)
            .context("Local State 里没有 os_crypt 加密密钥（Grok Bot 版本变化？）");
        let plain = dpapi_unprotect(&encrypted?)?;
        plain
            .try_into()
            .map_err(|_| anyhow::anyhow!("DPAPI 解出的密钥不是 32 字节"))
    }
    #[cfg(not(windows))]
    {
        bail!("Grok Bot 额度直连目前仅支持 Windows（macOS/Linux 的凭据存储尚未适配）")
    }
}

// ---------- 响应解析（纯函数） ----------

/// `usagePercent` 是**已用**百分比（真机验证：两次读数随使用上涨，31.35 →
/// 31.86），换算成剩余入库。超出 0–100 或非有限值按无效丢弃，不钳制。
fn parse_usage_response(text: &str) -> Result<QuotaSample> {
    #[derive(Deserialize)]
    struct UsageResponse {
        #[serde(rename = "usagePercent")]
        usage_percent: Option<f64>,
        #[serde(rename = "nextResetTimestampUtc")]
        next_reset_timestamp_utc: Option<String>,
    }
    let response: UsageResponse =
        serde_json::from_str(text).context("Grok Bot 额度响应不是预期的 JSON")?;
    let used = response
        .usage_percent
        .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
        .context("Grok Bot 额度响应缺少有效的 usagePercent");
    let collected_at_ms = chrono::Utc::now().timestamp_millis();
    let resets_at_ms = response
        .next_reset_timestamp_utc
        .as_deref()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .and_then(|time| sane_resets_at_ms(WINDOW_KEY, time.timestamp_millis(), collected_at_ms));
    Ok(QuotaSample {
        adapter_id: "grokbot",
        window_key: WINDOW_KEY.into(),
        remaining_percent: 100.0 - used?,
        resets_at_ms,
        collected_at_ms,
        source_label: SOURCE_LABEL.into(),
        quality: "official_live",
    })
}

// ---------- Windows DPAPI ----------

#[cfg(windows)]
fn dpapi_unprotect(data: &[u8]) -> Result<Vec<u8>> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: 输入 Blob 指向本函数栈上的 data；输出由 CryptUnprotectData
    // 分配、下方 LocalFree 释放。无附加熵（与 Electron safeStorage 一致）。
    let result = unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if result.is_err() {
        bail!("DPAPI 解密失败（密钥不属于当前 Windows 用户？）");
    }
    // SAFETY: output.pbData 指向成功调用后分配的 cbData 字节。
    let plain =
        unsafe { std::slice::from_raw_parts(output.pbData as *const u8, output.cbData as usize) }
            .to_vec();
    // SAFETY: LocalFree 释放 DPAPI 输出缓冲。
    unsafe {
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
    };
    Ok(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sand_secrets_extracts_the_active_accounts_token() {
        let accounts = r#"{"active":"slot-a","accounts":{"slot-a":{"cursor-access-token":"  enc-a  ","cursor-refresh-token":"r"},"slot-b":{"cursor-access-token":"enc-b"}}}"#;
        let text = format!(
            r#"{{"version":1,"cursor-machine-id":"mid","cursor-accounts":{}}}"#,
            serde_json::to_string(accounts).unwrap()
        );
        assert_eq!(
            parse_sand_secrets(&text),
            Some(SandSecrets {
                access_token: "enc-a".into()
            })
        );
    }

    #[test]
    fn sand_secrets_rejects_empty_or_missing_token() {
        let empty = r#"{"active":"s","accounts":{"s":{"cursor-access-token":"  "}}}"#;
        assert_eq!(
            parse_sand_secrets(&format!(
                r#"{{"cursor-accounts":{}}}"#,
                serde_json::to_string(empty).unwrap()
            )),
            None
        );
        assert_eq!(parse_sand_secrets("{}"), None);
        assert_eq!(parse_sand_secrets("not json"), None);
    }

    #[test]
    fn local_state_key_strips_the_dpapi_prefix() {
        use base64::engine::general_purpose::STANDARD;
        let blob = [b"DPAPI".as_slice(), [7u8; 5].as_slice()].concat();
        let text = format!(
            r#"{{"os_crypt":{{"encrypted_key":"{}"}}}}"#,
            STANDARD.encode(&blob)
        );
        assert_eq!(parse_local_state_key(&text), Some(vec![7u8; 5]));
        // 没有前缀的密钥不是 Chromium 布局，拒绝而不是猜。
        let text = format!(
            r#"{{"os_crypt":{{"encrypted_key":"{}"}}}}"#,
            STANDARD.encode([1u8; 8])
        );
        assert_eq!(parse_local_state_key(&text), None);
        assert_eq!(parse_local_state_key("{}"), None);
    }

    #[test]
    fn decrypt_v10_roundtrip() {
        use aes_gcm::aead::{Aead, Payload};
        use aes_gcm::KeyInit;
        let key = [7u8; 32];
        let cipher = aes_gcm::Aes256Gcm::new((&key).into());
        let nonce = [3u8; 12];
        let sealed = cipher
            .encrypt(
                nonce.as_slice().into(),
                Payload {
                    msg: b"token-body",
                    aad: b"",
                },
            )
            .unwrap();
        let mut raw = b"v10".to_vec();
        raw.extend_from_slice(&nonce);
        raw.extend_from_slice(&sealed);
        use base64::engine::general_purpose::STANDARD;
        let encoded = STANDARD.encode(&raw);
        assert_eq!(decrypt_v10(&key, &encoded).unwrap(), "token-body");
        // 错误的密钥必须失败，而不是返回垃圾。
        assert!(decrypt_v10(&[8u8; 32], &encoded).is_err());
        // 非 v10 布局直接拒绝。
        assert!(decrypt_v10(&key, &STANDARD.encode([0u8; 40])).is_err());
    }

    #[test]
    fn usage_response_converts_used_percent_to_remaining() {
        let text = r#"{"currentPeriodStart":"2026-10-25T02:36:03.516Z","nextResetTimestampUtc":"2026-11-01T02:36:03.516Z","usagePercent":31.352852,"hasAvailableUsage":true}"#;
        let sample = parse_usage_response(text).unwrap();
        assert_eq!(sample.adapter_id, "grokbot");
        assert_eq!(sample.window_key, "seven_day");
        assert!((sample.remaining_percent - 68.647148).abs() < 1e-4);
        assert!(sample.resets_at_ms.is_some());
        assert_eq!(sample.quality, "official_live");
        assert_eq!(sample.source_label, "Grok Bot 官方配额");
    }

    #[test]
    fn usage_response_rejects_invalid_percentages() {
        // 百分比越界不是可钳制的值，是坏数据（产品约束）。
        for bad in ["999.0", "-1.0", "null"] {
            let text = format!(r#"{{"usagePercent":{bad}}}"#);
            assert!(parse_usage_response(&text).is_err(), "{bad} 应被拒绝");
        }
        assert!(parse_usage_response("{}").is_err());
    }

    #[test]
    fn data_dir_lives_under_the_config_root() {
        let dir = grokbot_data_dir();
        assert!(dir.ends_with("Grok Bot"));
        assert_eq!(secrets_path(), dir.join("sand-secrets.json"));
    }
}
