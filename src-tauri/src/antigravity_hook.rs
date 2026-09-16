#[cfg(not(windows))]
use crate::claude_hook::shell_single_quote;
use crate::claude_hook::{first_command_token, run_delegate, sweep_stale_files};
use crate::domain::{sane_resets_at_ms, QuotaSample};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Antigravity CLI（`agy`）官方配额的零凭据来源：statusLine 钩子。
///
/// CLI 内嵌的 language server 与 IDE 共用同一套 Connect RPC，但 csrf token
/// 只存在于进程内部（命令行、日志、配置文件都不落盘），外部进程拿不到，无法
/// 像 IDE 那样直连。官方给本机工具留的口子是 statusLine：CLI 每次状态变化都把
/// 会话 JSON（含 `quota` 对象的 `remaining_fraction` 与 `reset_time`）通过
/// stdin 推给用户配置的命令。安装的钩子只提取额度窗口并原子写入
/// `~/.gemini/antigravity-cli/metrik-antigravity-quota.json`，同时输出一行
/// 简洁的状态栏文本；不读取、不保存对话内容或凭据。
///
/// 与 Claude 钩子一致：用户已有自定义 statusLine 时备份并串联，钩子落完额度
/// 后把 stdin 原样转给原命令渲染；卸载时原样恢复。
const QUOTA_FILE: &str = "metrik-antigravity-quota.json";
const BACKUP_FILE: &str = "metrik-antigravity-statusline.backup.json";
const METADATA_FILE: &str = "metrik-antigravity-statusline.json";
pub(crate) const SOURCE_LABEL: &str = "Antigravity CLI statusLine 钩子";
const MAX_SNAPSHOT_AGE_MS: i64 = 15 * 60 * 1000;
const HOOK_FLAG: &str = "--antigravity-hook";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityHookStatus {
    pub installed: bool,
    /// 已有无法串联的 statusLine（缺 command 字段），安装被拒绝。
    pub conflict: bool,
    /// 已安装且串联了用户原有的 statusLine 命令。
    pub chained: bool,
    /// Metrik 留有安装元数据，但当前 statusLine 已被其他命令替换。
    pub replaced: bool,
    pub last_data_at_ms: Option<i64>,
    pub stale: bool,
}

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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusLineMetadata {
    delegate: String,
    quota_path: PathBuf,
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
/// （`gemini_weekly`）。统一小写归一，未知后缀原样保留。
fn normalize_window_key(key: &str) -> String {
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
    for (suffix, label) in [("_weekly", "每周"), ("_5h", "5h"), ("_7d", "7d")] {
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

fn write_quota_atomically(path: &Path, payload: &QuotaFile) -> Result<()> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("Antigravity quota path has no file name")?;
    let staged = path.with_file_name(format!("{file_name}.tmp-{}", std::process::id()));
    std::fs::write(&staged, serde_json::to_vec(payload)?)
        .context("unable to stage Antigravity quota snapshot")?;
    let installed = std::fs::rename(&staged, path);
    if installed.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    installed.context("unable to install Antigravity quota snapshot")
}

/// 现有非 Metrik statusLine 的 command 原文（可串联时返回）。
fn foreign_command(settings: &Value) -> Option<String> {
    settings
        .get("statusLine")?
        .get("command")?
        .as_str()
        .filter(|command| !command.trim().is_empty())
        .map(str::to_owned)
}

/// agy 全新安装就带着 `{"type":"","command":"","enabled":true}` 的空骨架。
/// 这不是用户配置：没有可串联的命令，直接覆盖是安全的，也不算冲突。
/// Claude 的 settings.json 没有这种默认骨架，因此这条判定只在 agy 这边需要。
fn status_line_is_unconfigured(status_line: &Value) -> bool {
    status_line
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.trim().is_empty())
}

pub struct AntigravityHook {
    cli_dir: PathBuf,
}

impl AntigravityHook {
    /// Antigravity CLI 的配置目录（`agy` 固定读写 `~/.gemini/antigravity-cli`，
    /// 没有 IDE 那样的环境变量覆盖）。
    pub fn detected() -> Self {
        Self {
            cli_dir: dirs::home_dir()
                .unwrap_or_default()
                .join(".gemini")
                .join("antigravity-cli"),
        }
    }

    #[cfg(test)]
    pub fn with_dir(cli_dir: PathBuf) -> Self {
        Self { cli_dir }
    }

    fn settings_path(&self) -> PathBuf {
        self.cli_dir.join("settings.json")
    }

    fn quota_path(&self) -> PathBuf {
        self.cli_dir.join(QUOTA_FILE)
    }

    fn backup_path(&self) -> PathBuf {
        self.cli_dir.join(BACKUP_FILE)
    }

    fn metadata_path(&self) -> PathBuf {
        self.cli_dir.join(METADATA_FILE)
    }

    fn read_backup(&self) -> Option<Value> {
        let raw = std::fs::read_to_string(self.backup_path()).ok()?;
        serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()
    }

    fn read_metadata(&self) -> Option<StatusLineMetadata> {
        let raw = std::fs::read_to_string(self.metadata_path()).ok()?;
        serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()
    }

    fn expected_metadata(&self, delegate: String) -> Result<StatusLineMetadata> {
        let quota_path = self.quota_path();
        let quota_path = if quota_path.is_absolute() {
            quota_path
        } else {
            std::env::current_dir()
                .context("无法确定 Antigravity quota 文件的绝对路径")?
                .join(quota_path)
        };
        Ok(StatusLineMetadata {
            delegate,
            quota_path,
        })
    }

    fn write_metadata(&self, metadata: &StatusLineMetadata) -> Result<()> {
        let path = self.metadata_path();
        let staged = path.with_extension(format!("json.metrik-{}", std::process::id()));
        std::fs::write(&staged, serde_json::to_vec_pretty(metadata)?)
            .context("无法写入 statusLine 元数据")?;
        let installed = std::fs::rename(&staged, &path);
        if installed.is_err() {
            let _ = std::fs::remove_file(&staged);
        }
        installed.context("无法安装 statusLine 元数据")
    }

    /// 备份文件可能被清理工具删掉；元数据还在时从元数据回读 delegate，
    /// 与 Claude 钩子同一道防线。
    fn installed_delegate(&self) -> Option<String> {
        self.read_metadata().map(|metadata| metadata.delegate)
    }

    /// statusLine 命令引用 metrik 可执行文件。agy 在 Windows 上经 cmd.exe
    /// 执行命令，且双引号会原样进入程序名（实测 `"C:\...metrik.exe"` 直接报
    /// 「不是内部或外部命令」，`^` 转义也会静默失效）：命令必须是不带引号的
    /// 裸路径。无空格路径直接用；有空格退到 8.3 短名（`C:\PROGRA~1\...`），
    /// 系统未生成短名时明确拒绝，绝不装一个永不执行的钩子。Unix 上 agy 经
    /// shell 执行，单引号包路径，与 Claude 钩子一致。
    fn hook_command(&self) -> Result<String> {
        let executable = std::env::current_exe().context("无法确定 metrik 可执行文件的绝对路径")?;
        #[cfg(windows)]
        {
            let path = executable.to_string_lossy();
            if !path.contains(' ') {
                return Ok(format!("{path} {HOOK_FLAG}"));
            }
            let short = windows_short_path(&executable)
                .filter(|path| !path.contains(' '))
                .context(
                    "metrik 安装路径含空格且系统未生成 8.3 短名，无法为 Antigravity CLI 安装钩子",
                )?;
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

    fn read_settings(&self) -> Result<Value> {
        match std::fs::read_to_string(self.settings_path()) {
            Ok(raw) => {
                let trimmed = raw.trim_start_matches('\u{feff}');
                serde_json::from_str(trimmed).context("settings.json 不是有效 JSON")
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
            Err(error) => Err(error).context("无法读取 ~/.gemini/antigravity-cli/settings.json"),
        }
    }

    fn write_settings(&self, settings: &Value) -> Result<()> {
        std::fs::create_dir_all(&self.cli_dir)?;
        let path = self.settings_path();
        let staged = path.with_extension(format!("json.metrik-{}", std::process::id()));
        std::fs::write(&staged, serde_json::to_string_pretty(settings)?)?;
        let installed = std::fs::rename(&staged, &path);
        if installed.is_err() {
            let _ = std::fs::remove_file(&staged);
        }
        installed.context("无法更新 ~/.gemini/antigravity-cli/settings.json")
    }

    fn status_line_is_ours(&self, settings: &Value) -> bool {
        let Some(command) = settings
            .get("statusLine")
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str)
        else {
            return false;
        };
        if self
            .hook_command()
            .is_ok_and(|expected| command == expected)
        {
            return true;
        }
        // 应用被移动/重装后绝对路径会变：命令仍以钩子旗标结尾且第一个 token
        // 的文件名是 metrik 可执行文件时，认作我们的，交给自愈改写。
        command.trim_end().ends_with(HOOK_FLAG)
            && first_command_token(command).is_some_and(|executable| {
                Path::new(executable)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|name| {
                        name.eq_ignore_ascii_case("metrik")
                            || name.eq_ignore_ascii_case("metrik.exe")
                    })
            })
    }

    pub fn status(&self) -> Result<AntigravityHookStatus> {
        let settings = self.read_settings()?;
        let installed = self.status_line_is_ours(&settings);
        let conflict = !installed
            && settings
                .get("statusLine")
                .is_some_and(|value| !value.is_null())
            && foreign_command(&settings).is_none()
            && !settings
                .get("statusLine")
                .is_some_and(status_line_is_unconfigured);
        let chained = installed
            && (self.read_backup().is_some()
                || self
                    .installed_delegate()
                    .is_some_and(|value| !value.is_empty()));
        let last_data_at_ms = self.read_quota_file().map(|file| file.received_at_ms);
        let stale = last_data_at_ms.is_some_and(|received_at_ms| {
            now_ms().saturating_sub(received_at_ms) > MAX_SNAPSHOT_AGE_MS
        });
        Ok(AntigravityHookStatus {
            installed,
            conflict,
            chained,
            replaced: !installed && self.read_metadata().is_some(),
            last_data_at_ms,
            stale,
        })
    }

    pub fn install(&self) -> Result<AntigravityHookStatus> {
        let mut settings = self.read_settings()?;
        std::fs::create_dir_all(&self.cli_dir)?;

        let mut delegate = String::new();
        if self.status_line_is_ours(&settings) {
            delegate = self
                .read_backup()
                .as_ref()
                .and_then(|backup| backup.get("command"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| self.installed_delegate())
                .unwrap_or_default();
        } else if let Some(existing) = settings
            .get("statusLine")
            .filter(|value| !value.is_null())
            // agy 默认的空骨架（command 为空）不是用户配置，直接覆盖不算侵权。
            .filter(|value| !status_line_is_unconfigured(value))
            .cloned()
        {
            let Some(command) = foreign_command(&settings) else {
                bail!(
                    "Antigravity CLI 已配置无法串联的 statusLine（缺少 command 字段），为避免覆盖，未安装。"
                );
            };
            std::fs::write(self.backup_path(), serde_json::to_string_pretty(&existing)?)
                .context("无法备份原有 statusLine 设置")?;
            delegate = command;
        }

        self.write_metadata(&self.expected_metadata(delegate.clone())?)?;

        let root = settings
            .as_object_mut()
            .context("settings.json 顶层不是对象")?;
        root.insert(
            "statusLine".into(),
            json!({ "type": "command", "command": self.hook_command()?, "enabled": true }),
        );
        self.write_settings(&settings)?;
        self.status()
    }

    /// 启动时自愈：statusLine 属于 Metrik 但命令过时（应用移动/重装）时重装一次。
    /// 不属于 Metrik 的 statusLine 一律不碰；命令与元数据都健康时不写盘。
    pub fn repair(&self) -> Result<bool> {
        let settings = self.read_settings()?;
        if !self.status_line_is_ours(&settings) {
            return Ok(false);
        }
        let installed_command = settings
            .get("statusLine")
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let delegate = self.installed_delegate().unwrap_or_default();
        let expected_metadata = self.expected_metadata(delegate)?;
        if installed_command == self.hook_command()?
            && self.read_metadata().as_ref() == Some(&expected_metadata)
        {
            return Ok(false);
        }
        self.install()?;
        Ok(true)
    }

    pub fn uninstall(&self) -> Result<AntigravityHookStatus> {
        let mut settings = self.read_settings()?;
        if self.status_line_is_ours(&settings) {
            let root = settings
                .as_object_mut()
                .context("settings.json 顶层不是对象")?;
            let restored = self.read_backup().or_else(|| {
                let delegate = self.installed_delegate().filter(|d| !d.is_empty())?;
                Some(json!({ "type": "command", "command": delegate, "enabled": true }))
            });
            match restored {
                Some(backup) => {
                    root.insert("statusLine".into(), backup);
                }
                None => {
                    root.remove("statusLine");
                }
            }
            self.write_settings(&settings)?;
        }
        let _ = std::fs::remove_file(self.metadata_path());
        let _ = std::fs::remove_file(self.quota_path());
        let _ = std::fs::remove_file(self.backup_path());
        self.status()
    }

    fn read_quota_file(&self) -> Option<QuotaFile> {
        let raw = std::fs::read_to_string(self.quota_path()).ok()?;
        serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()
    }

    /// 把钩子落地的全部官方窗口转换成 QuotaSample；文件缺失或格式异常返回空，
    /// 不猜测、不沿用陈旧文件之外的任何来源。
    pub fn quota_samples(&self) -> Vec<QuotaSample> {
        let Some(file) = self.read_quota_file() else {
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

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// GetShortPathNameW：把含空格的安装路径转成 `C:\PROGRA~1\...` 形式的 8.3
/// 短名。系统关闭 8.3 生成（Windows 11 的新卷默认）时返回原样路径或 None，
/// 调用方据此拒绝安装而不是装一个坏钩子。
#[cfg(windows)]
fn windows_short_path(executable: &Path) -> Option<String> {
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
    use std::io::{Read, Write};

    let mut input = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut input);
    let output = render_hook_statusline(
        &AntigravityHook::detected(),
        &input,
        &std::env::temp_dir(),
        std::time::Duration::from_secs(10),
    );
    if !output.is_empty() {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(output.as_bytes());
        let _ = stdout.write_all(b"\n");
    }
}

fn render_hook_statusline(
    hook: &AntigravityHook,
    input: &[u8],
    temp_dir: &Path,
    delegate_timeout: std::time::Duration,
) -> String {
    let metadata = hook.read_metadata();
    let data = serde_json::from_slice::<Value>(input).ok();
    let payload = data
        .as_ref()
        .map(|value| payload_from_input(value, now_ms()));

    if let (Some(metadata), Some(payload)) = (metadata.as_ref(), payload.as_ref()) {
        let _ = write_quota_atomically(&metadata.quota_path, payload);
    }

    let stale_before = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(5 * 60))
        .unwrap_or(std::time::UNIX_EPOCH);
    sweep_stale_files(temp_dir, "metrik-antigravity-statusline-", stale_before);
    if let Some(quota_dir) = metadata
        .as_ref()
        .and_then(|metadata| metadata.quota_path.parent())
    {
        sweep_stale_files(
            quota_dir,
            "metrik-antigravity-quota.json.tmp-",
            stale_before,
        );
    }

    let parts = payload.as_ref().map(quota_parts).unwrap_or_default();
    if let Some(delegate) = metadata
        .as_ref()
        .map(|metadata| metadata.delegate.trim())
        .filter(|delegate| !delegate.is_empty())
    {
        let delegated = run_delegate(delegate, input, delegate_timeout);
        if !delegated.is_empty() {
            if !delegated.contains('\r') && !delegated.contains('\n') && !parts.is_empty() {
                return format!("{delegated} | {}", parts.join(" · "));
            }
            return delegated;
        }
        return parts.join(" · ");
    }

    let model = data
        .as_ref()
        .and_then(|value| value.get("model"))
        .and_then(|value| value.get("display_name"))
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .unwrap_or("Antigravity");
    if parts.is_empty() {
        model.to_owned()
    } else {
        format!("{model} | {}", parts.join(" · "))
    }
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
        fs::write(
            test.path().join("settings.json"),
            r#"{"statusLine":{"type":"command","command":"C:\\old\\path\\metrik.exe --antigravity-hook","enabled":true}}"#,
        )
        .unwrap();
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
