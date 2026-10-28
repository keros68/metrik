use crate::domain::{sane_resets_at_ms, QuotaSample};
use crate::i18n::tr;
#[cfg(not(windows))]
use crate::statusline::shell_single_quote;
use crate::statusline::{self, read_json_file, Host, Installer};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

/// Antigravity CLI（`agy`）官方配额的零凭据来源：statusLine 钩子。
///
/// CLI 内嵌的 language server 与 IDE 共用同一套 Connect RPC，但 csrf token
/// 只存在于进程内部（命令行、日志、配置文件都不落盘），外部进程拿不到，无法
/// 像 IDE 那样直连。官方给本机工具留的口子是 statusLine：CLI 每次状态变化都把
/// 会话 JSON（含 `quota` 对象的 `remaining_fraction` 与 `reset_time`）通过
/// stdin 推给用户配置的命令。安装的钩子只提取额度窗口并原子写入
/// `~/.gemini/antigravity-cli/metrik-antigravity-quota.json`，同时输出一行
/// 简洁的状态栏文本；不读取、不保存对话内容或凭据。安装、串联与委托见
/// `statusline` 模块。
pub(crate) const SOURCE_LABEL: &str = "Antigravity CLI statusLine 钩子";
const HOOK_FLAG: &str = "--antigravity-hook";

static HOST: Host = Host {
    name: "Antigravity CLI",
    settings_label: "~/.gemini/antigravity-cli/settings.json",
    quota_file: "metrik-antigravity-quota.json",
    backup_file: "metrik-antigravity-statusline.backup.json",
    metadata_file: "metrik-antigravity-statusline.json",
    flag: HOOK_FLAG,
    hook_command,
    status_line: |command| json!({ "type": "command", "command": command, "enabled": true }),
    // agy 全新安装就带着 `{"type":"","command":"","enabled":true}` 的空骨架。
    // 这不是用户配置：没有可串联的命令，直接覆盖是安全的，也不算冲突。
    empty_command_is_unconfigured: true,
    legacy_script: None,
};

pub type AntigravityHookStatus = statusline::HookStatus;

/// 落盘的额度快照。窗口键沿用官方桶名（`gemini-weekly` 归一成 `gemini_weekly`）。
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaFile {
    received_at_ms: i64,
    #[serde(default)]
    windows: std::collections::BTreeMap<String, QuotaWindowSnapshot>,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
struct QuotaWindowSnapshot {
    remaining_percent: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resets_at_ms: Option<i64>,
}

/// 官方 statusLine 载荷里的额度对象：
/// `"quota": {"gemini-weekly": {"remaining_fraction": 0.94, "reset_time": "...Z", "reset_in_seconds": 560580}}`
fn payload_from_input(input: &Value, now_ms: i64) -> QuotaFile {
    let windows = input
        .get("quota")
        .and_then(Value::as_object)
        .map(|buckets| {
            buckets
                .iter()
                .filter_map(|(key, entry)| {
                    let remaining_percent = remaining_percent(entry)?;
                    if !remaining_percent.is_finite() || !(0.0..=100.0).contains(&remaining_percent)
                    {
                        return None;
                    }
                    Some((
                        normalize_window_key(key),
                        QuotaWindowSnapshot {
                            remaining_percent,
                            resets_at_ms: reset_time_ms(entry, now_ms),
                        },
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    QuotaFile {
        received_at_ms: now_ms,
        windows,
    }
}

/// 官方桶名是连字符（`gemini-weekly`），引擎与界面的窗口键约定是下划线
/// （`gemini_weekly`）。统一小写归一，未知后缀原样保留。IDE 的 RPC 配额
/// 也用同样的桶名，两个来源共用这一处归一。
pub(crate) fn normalize_window_key(key: &str) -> String {
    key.trim().to_ascii_lowercase().replace('-', "_")
}

fn remaining_percent(entry: &Value) -> Option<f64> {
    if let Some(fraction) = entry.get("remaining_fraction").and_then(Value::as_f64) {
        return Some(fraction * 100.0);
    }
    entry.get("remaining_percent").and_then(Value::as_f64)
}

/// `reset_time` 是 RFC3339；缺了退回 `reset_in_seconds` 相对量。
fn reset_time_ms(entry: &Value, now_ms: i64) -> Option<i64> {
    if let Some(text) = entry.get("reset_time").and_then(Value::as_str) {
        if let Ok(time) = chrono::DateTime::parse_from_rfc3339(text) {
            return Some(time.timestamp_millis());
        }
    }
    entry
        .get("reset_in_seconds")
        .and_then(Value::as_i64)
        .map(|seconds| now_ms + seconds.saturating_mul(1000))
}

/// 状态栏文本的额度段：`Gemini 每周 94% · Gemini 5h 88%`。
fn quota_parts(payload: &QuotaFile) -> Vec<String> {
    payload
        .windows
        .iter()
        .map(|(key, window)| format!("{} {:.0}%", window_label(key), window.remaining_percent))
        .collect()
}

fn window_label(key: &str) -> String {
    let weekly = tr!("每周", "weekly");
    for (suffix, label) in [("_weekly", weekly.as_str()), ("_5h", "5h"), ("_7d", "7d")] {
        if let Some(model) = key.strip_suffix(suffix) {
            return format!("{} {label}", capitalize(model));
        }
    }
    key.replace('_', " ")
}

fn capitalize(model: &str) -> String {
    let mut chars = model.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

/// statusLine 命令引用 metrik 可执行文件。agy 在 Windows 上经 cmd.exe
/// 执行命令，且双引号会原样进入程序名（实测 `"C:\...metrik.exe"` 直接报
/// 「不是内部或外部命令」，`^` 转义也会静默失效）：命令必须是不带引号的
/// 裸路径。无空格路径直接用；有空格退到 8.3 短名（`C:\PROGRA~1\...`），
/// 系统未生成短名时明确拒绝，绝不装一个永不执行的钩子。Unix 上 agy 经
/// shell 执行，单引号包路径，与 Claude 钩子一致。
fn hook_command() -> Result<String> {
    let executable = std::env::current_exe().with_context(|| {
        tr!(
            "无法确定 metrik 可执行文件的绝对路径",
            "Couldn't determine the absolute path of the metrik executable"
        )
    })?;
    #[cfg(windows)]
    {
        let path = executable.to_string_lossy();
        if !path.contains(' ') {
            return Ok(format!("{path} {HOOK_FLAG}"));
        }
        let short = windows_short_path(&executable)
            .filter(|path| !path.contains(' '))
            .with_context(|| {
                tr!(
                    "metrik 安装路径含空格且系统未生成 8.3 短名，无法为 Antigravity CLI 安装钩子",
                    "The metrik install path contains spaces and the system has no 8.3 short name for it, so the hook can't be installed for Antigravity CLI"
                )
            })?;
        Ok(format!("{short} {HOOK_FLAG}"))
    }
    #[cfg(not(windows))]
    {
        Ok(format!(
            "{} {HOOK_FLAG}",
            shell_single_quote(&executable.to_string_lossy())
        ))
    }
}

pub struct AntigravityHook(Installer);

impl std::ops::Deref for AntigravityHook {
    type Target = Installer;

    fn deref(&self) -> &Installer {
        &self.0
    }
}

impl AntigravityHook {
    /// Antigravity CLI 的配置目录（`agy` 固定读写 `~/.gemini/antigravity-cli`，
    /// 没有 IDE 那样的环境变量覆盖）。
    pub fn detected() -> Self {
        Self::with_dir(
            dirs::home_dir()
                .unwrap_or_default()
                .join(".gemini")
                .join("antigravity-cli"),
        )
    }

    pub fn with_dir(cli_dir: PathBuf) -> Self {
        Self(Installer::new(cli_dir, &HOST))
    }

    /// 把钩子落地的全部官方窗口转换成 QuotaSample；文件缺失或格式异常返回空，
    /// 不猜测、不沿用陈旧文件之外的任何来源。
    pub fn quota_samples(&self) -> Vec<QuotaSample> {
        let Some(file) = read_json_file::<QuotaFile>(&self.quota_path()) else {
            return Vec::new();
        };
        file.windows
            .iter()
            .filter(|(_, window)| {
                window.remaining_percent.is_finite()
                    && (0.0..=100.0).contains(&window.remaining_percent)
            })
            .map(|(key, window)| QuotaSample {
                adapter_id: "antigravity",
                window_key: key.clone(),
                remaining_percent: window.remaining_percent,
                resets_at_ms: window
                    .resets_at_ms
                    .and_then(|value| sane_resets_at_ms(key, value, file.received_at_ms)),
                collected_at_ms: file.received_at_ms,
                source_label: SOURCE_LABEL.into(),
                quality: "official_snapshot",
            })
            .collect()
    }
}

/// GetShortPathNameW：把含空格的安装路径转成 `C:\PROGRA~1\...` 形式的 8.3
/// 短名。系统关闭 8.3 生成（Windows 11 的新卷默认）时返回原样路径或 None，
/// 调用方据此拒绝安装而不是装一个坏钩子。
#[cfg(windows)]
fn windows_short_path(executable: &std::path::Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetShortPathNameW;

    let wide: Vec<u16> = executable
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        // 传 None 先探长度；返回 0 表示路径不存在或没有短名。
        let len = GetShortPathNameW(PCWSTR::from_raw(wide.as_ptr()), None);
        if len == 0 {
            return None;
        }
        let mut buffer = vec![0u16; len as usize];
        let written = GetShortPathNameW(PCWSTR::from_raw(wide.as_ptr()), Some(&mut buffer));
        if written == 0 {
            return None;
        }
        buffer.truncate(written as usize);
        String::from_utf16(&buffer).ok()
    }
}

/// `metrik --antigravity-hook`：agy 把会话 JSON 从 stdin 推进来，这里提取额度
/// 落盘，并把状态栏文本写到 stdout。
pub fn run_hook() {
    statusline::run(|input| {
        render_hook_statusline(
            &AntigravityHook::detected(),
            input,
            std::time::Duration::from_secs(10),
        )
    });
}

fn render_hook_statusline(
    hook: &AntigravityHook,
    input: &[u8],
    delegate_timeout: std::time::Duration,
) -> String {
    let metadata = hook.read_metadata();
    let data = serde_json::from_slice::<Value>(input).ok();
    let payload = data
        .as_ref()
        .map(|value| payload_from_input(value, chrono::Utc::now().timestamp_millis()));
    let quota_text = payload
        .as_ref()
        .map(quota_parts)
        .unwrap_or_default()
        .join(" · ");
    statusline::render(
        metadata.as_ref(),
        input,
        payload.and_then(|payload| serde_json::to_vec(&payload).ok()),
        &quota_text,
        statusline::model_name(data.as_ref(), "Antigravity"),
        delegate_timeout,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "metrik-antigravity-hook-{label}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn install_writes_hook_then_uninstall_restores_agy_settings() {
        let test = TestDirectory::new("roundtrip");
        // agy 实际写出的 settings.json 形状：空 statusLine 骨架 + 其他键。
        fs::write(
            test.path().join("settings.json"),
            r#"{"model":"Gemini 3.8 Flash (Medium)","statusLine":{"type":"","command":"","enabled":true}}"#,
        )
        .unwrap();
        let hook = AntigravityHook::with_dir(test.path().to_path_buf());

        // agy 的空 statusLine 骨架是默认状态，不算冲突，可以直接安装。
        let before = hook.status().unwrap();
        assert!(!before.installed);
        assert!(!before.conflict);

        let status = hook.install().unwrap();
        assert!(status.installed);
        assert!(!status.conflict);
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(settings["model"], "Gemini 3.8 Flash (Medium)");
        assert!(settings["statusLine"]["command"]
            .as_str()
            .unwrap()
            .ends_with(HOOK_FLAG));
        assert_eq!(settings["statusLine"]["enabled"], true);
        assert!(hook.metadata_path().exists());

        let uninstalled = hook.uninstall().unwrap();
        assert!(!uninstalled.installed);
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        // 空 command 的骨架不是用户命令，卸载时直接移除 statusLine，其他键原样。
        assert!(settings.get("statusLine").is_none());
        assert_eq!(settings["model"], "Gemini 3.8 Flash (Medium)");
        assert!(!hook.metadata_path().exists());
        assert!(!hook.quota_path().exists());
    }

    #[test]
    fn foreign_status_line_is_backed_up_and_chained() {
        let test = TestDirectory::new("chained");
        fs::write(
            test.path().join("settings.json"),
            r#"{"statusLine":{"type":"command","command":"starship prompt","enabled":true}}"#,
        )
        .unwrap();
        let hook = AntigravityHook::with_dir(test.path().to_path_buf());

        let status = hook.install().unwrap();
        assert!(status.installed && status.chained);
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        assert!(settings["statusLine"]["command"]
            .as_str()
            .unwrap()
            .ends_with(HOOK_FLAG));

        let uninstalled = hook.uninstall().unwrap();
        assert!(!uninstalled.installed);
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(settings["statusLine"]["command"], "starship prompt");
        assert_eq!(settings["statusLine"]["enabled"], true);
    }

    #[test]
    fn status_line_without_command_field_refuses_install() {
        let test = TestDirectory::new("conflict");
        fs::write(
            test.path().join("settings.json"),
            r#"{"statusLine":{"enabled":false}}"#,
        )
        .unwrap();
        let hook = AntigravityHook::with_dir(test.path().to_path_buf());
        assert!(hook.status().unwrap().conflict);
        assert!(hook.install().is_err());
        // 拒绝安装时不得写任何文件。
        assert!(!hook.metadata_path().exists());
    }

    #[test]
    fn quota_payload_normalizes_official_bucket_names() {
        let input: Value = serde_json::from_str(
            r#"{
                "session_id": "abc",
                "model": {"id": "gemini-3-pro", "display_name": "Gemini 3 Pro"},
                "quota": {
                    "gemini-weekly": {
                        "remaining_fraction": 0.9378,
                        "reset_time": "2026-07-06T07:50:32Z",
                        "reset_in_seconds": 560580
                    },
                    "gemini-5h": {
                        "remaining_fraction": 0.5,
                        "reset_in_seconds": 3600
                    }
                }
            }"#,
        )
        .unwrap();
        let now = 1_750_000_000_000;
        let payload = payload_from_input(&input, now);
        let mut windows = payload.windows;
        assert_eq!(windows.len(), 2);
        let weekly = windows.remove("gemini_weekly").unwrap();
        assert!((weekly.remaining_percent - 93.78).abs() < 1e-9);
        assert_eq!(
            weekly.resets_at_ms,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-07-06T07:50:32Z")
                    .unwrap()
                    .timestamp_millis()
            )
        );
        let five_hour = windows.remove("gemini_5h").unwrap();
        assert!((five_hour.remaining_percent - 50.0).abs() < 1e-9);
        assert_eq!(five_hour.resets_at_ms, Some(now + 3_600_000));
    }

    #[test]
    fn quota_samples_map_hook_file_to_samples() {
        let test = TestDirectory::new("samples");
        let hook = AntigravityHook::with_dir(test.path().to_path_buf());
        assert!(hook.quota_samples().is_empty());

        let received = chrono::Utc::now().timestamp_millis();
        let reset = received + 86_400_000;
        let file = QuotaFile {
            received_at_ms: received,
            windows: [
                (
                    "gemini_weekly".to_owned(),
                    QuotaWindowSnapshot {
                        remaining_percent: 93.78,
                        resets_at_ms: Some(reset),
                    },
                ),
                (
                    "bogus".to_owned(),
                    QuotaWindowSnapshot {
                        remaining_percent: 150.0,
                        resets_at_ms: None,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        };
        std::fs::write(hook.quota_path(), serde_json::to_vec(&file).unwrap()).unwrap();

        let samples = hook.quota_samples();
        assert_eq!(samples.len(), 1, "超出 0-100 的窗口必须被拒绝");
        assert_eq!(samples[0].adapter_id, "antigravity");
        assert_eq!(samples[0].window_key, "gemini_weekly");
        assert!((samples[0].remaining_percent - 93.78).abs() < 1e-9);
        assert_eq!(samples[0].resets_at_ms, Some(reset));
        assert_eq!(samples[0].collected_at_ms, received);
        assert_eq!(samples[0].source_label, SOURCE_LABEL);
    }

    #[test]
    fn repair_rewrites_a_stale_hook_command() {
        let test = TestDirectory::new("repair");
        // 旧命令的路径分隔符按平台写：Unix 的 Path::file_name 不认反斜杠，
        // basename 识别只在同风格的路径下成立。
        let stale = if cfg!(windows) {
            r"C:\\old\\path\\metrik.exe"
        } else {
            "/old/path/metrik"
        };
        let settings = format!(
            r#"{{"statusLine":{{"type":"command","command":"{stale} --antigravity-hook","enabled":true}}}}"#
        );
        fs::write(test.path().join("settings.json"), settings).unwrap();
        let hook = AntigravityHook::with_dir(test.path().to_path_buf());
        // 命令指向别的 metrik 路径：仍认作我们的（basename 匹配），自愈会改写。
        assert!(hook.status().unwrap().installed);
        assert!(hook.repair().unwrap());
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        let expected = hook.hook_command().unwrap();
        assert_eq!(settings["statusLine"]["command"], expected);
        // 健康状态下不再写盘。
        assert!(!hook.repair().unwrap());
    }

    #[test]
    fn statusline_text_uses_prettified_window_labels() {
        let file = QuotaFile {
            received_at_ms: 0,
            windows: [
                (
                    "gemini_weekly".to_owned(),
                    QuotaWindowSnapshot {
                        remaining_percent: 93.78,
                        resets_at_ms: None,
                    },
                ),
                (
                    "gemini_5h".to_owned(),
                    QuotaWindowSnapshot {
                        remaining_percent: 88.0,
                        resets_at_ms: None,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        };
        let parts = quota_parts(&file);
        assert!(parts.contains(&"Gemini 每周 94%".to_owned()));
        assert!(parts.contains(&"Gemini 5h 88%".to_owned()));
    }
}
