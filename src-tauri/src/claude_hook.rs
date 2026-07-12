use crate::domain::QuotaSample;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

/// Claude Code 官方配额的零凭据来源：statusLine 钩子。
///
/// Claude Code 每次刷新状态栏都会把当前会话 JSON（含
/// `rate_limits.five_hour / seven_day` 的 used_percentage 与 resets_at）
/// 通过 stdin 推给 statusLine 命令。安装的脚本只提取这两个窗口并原子
/// 写入 `~/.claude/metrik-quota.json`，同时输出一行简洁的状态栏文本；
/// 不读取、不保存对话内容或凭据。卸载时恢复安装前的 statusLine 设置。
const QUOTA_FILE: &str = "metrik-quota.json";

#[cfg(windows)]
const SCRIPT_FILE: &str = "metrik-statusline.ps1";
#[cfg(not(windows))]
const SCRIPT_FILE: &str = "metrik-statusline.py";

#[cfg(windows)]
const SCRIPT_BODY: &str = r#"# Metrik statusLine hook: persist Claude Code rate limits, no content is stored.
$raw = [Console]::In.ReadToEnd()
try { $data = $raw | ConvertFrom-Json } catch { exit 0 }
$rl = $data.rate_limits
$payload = @{ receivedAtMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() }
if ($null -ne $rl) {
  $windows = @{}
  foreach ($key in @('five_hour', 'seven_day')) {
    $entry = $rl.$key
    if ($null -ne $entry -and $null -ne $entry.used_percentage) {
      $windows[$key] = @{ usedPercentage = [double]$entry.used_percentage }
      if ($null -ne $entry.resets_at) { $windows[$key].resetsAt = [double]$entry.resets_at }
    }
  }
  $payload.windows = $windows
}
$target = Join-Path $env:USERPROFILE '.claude\metrik-quota.json'
$tmp = "$target.tmp-$PID"
($payload | ConvertTo-Json -Depth 5 -Compress) | Out-File -FilePath $tmp -Encoding utf8
Move-Item -Force $tmp $target
$model = if ($data.model.display_name) { $data.model.display_name } else { 'Claude' }
$parts = @($model)
if ($payload.windows -and $payload.windows.five_hour) { $parts += ('5h ' + [math]::Round($payload.windows.five_hour.usedPercentage) + '%') }
if ($payload.windows -and $payload.windows.seven_day) { $parts += ('7d ' + [math]::Round($payload.windows.seven_day.usedPercentage) + '%') }
$parts -join ' | '
"#;

#[cfg(not(windows))]
const SCRIPT_BODY: &str = r#"#!/usr/bin/env python3
# Metrik statusLine hook: persist Claude Code rate limits, no content is stored.
import json, os, sys, tempfile, time

try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)

payload = {"receivedAtMs": int(time.time() * 1000)}
rl = data.get("rate_limits") or {}
windows = {}
for key in ("five_hour", "seven_day"):
    entry = rl.get(key) or {}
    if entry.get("used_percentage") is not None:
        windows[key] = {"usedPercentage": float(entry["used_percentage"])}
        if entry.get("resets_at") is not None:
            windows[key]["resetsAt"] = float(entry["resets_at"])
if windows:
    payload["windows"] = windows

target = os.path.expanduser("~/.claude/metrik-quota.json")
fd, tmp = tempfile.mkstemp(dir=os.path.dirname(target))
with os.fdopen(fd, "w") as handle:
    json.dump(payload, handle)
os.replace(tmp, target)

model = ((data.get("model") or {}).get("display_name")) or "Claude"
parts = [model]
if "five_hour" in windows:
    parts.append(f"5h {round(windows['five_hour']['usedPercentage'])}%")
if "seven_day" in windows:
    parts.append(f"7d {round(windows['seven_day']['usedPercentage'])}%")
print(" | ".join(parts))
"#;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeHookStatus {
    pub installed: bool,
    /// settings.json 里已有并非 Metrik 的 statusLine，安装会被拒绝。
    pub conflict: bool,
    pub last_data_at_ms: Option<i64>,
}

#[derive(Deserialize)]
struct QuotaFile {
    #[serde(rename = "receivedAtMs")]
    received_at_ms: i64,
    #[serde(default)]
    windows: QuotaWindows,
}

#[derive(Deserialize, Default)]
struct QuotaWindows {
    five_hour: Option<QuotaWindow>,
    seven_day: Option<QuotaWindow>,
}

#[derive(Deserialize)]
struct QuotaWindow {
    #[serde(rename = "usedPercentage")]
    used_percentage: f64,
    #[serde(rename = "resetsAt")]
    resets_at: Option<f64>,
}

pub struct ClaudeHook {
    claude_dir: PathBuf,
}

impl ClaudeHook {
    pub fn detected() -> Self {
        Self {
            claude_dir: dirs::home_dir().unwrap_or_default().join(".claude"),
        }
    }

    #[cfg(test)]
    pub fn with_dir(claude_dir: PathBuf) -> Self {
        Self { claude_dir }
    }

    fn settings_path(&self) -> PathBuf {
        self.claude_dir.join("settings.json")
    }

    fn script_path(&self) -> PathBuf {
        self.claude_dir.join(SCRIPT_FILE)
    }

    fn quota_path(&self) -> PathBuf {
        self.claude_dir.join(QUOTA_FILE)
    }

    fn hook_command(&self) -> String {
        let script = self.script_path();
        if cfg!(windows) {
            format!(
                "powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
                script.display()
            )
        } else {
            format!("python3 \"{}\"", script.display())
        }
    }

    fn read_settings(&self) -> Result<Value> {
        match std::fs::read_to_string(self.settings_path()) {
            Ok(raw) => {
                let trimmed = raw.trim_start_matches('\u{feff}');
                serde_json::from_str(trimmed).context("~/.claude/settings.json 不是有效 JSON")
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
            Err(error) => Err(error).context("无法读取 ~/.claude/settings.json"),
        }
    }

    fn write_settings(&self, settings: &Value) -> Result<()> {
        std::fs::create_dir_all(&self.claude_dir)?;
        let path = self.settings_path();
        let staged = path.with_extension(format!("json.metrik-{}", std::process::id()));
        std::fs::write(&staged, serde_json::to_string_pretty(settings)?)?;
        let installed = std::fs::rename(&staged, &path);
        if installed.is_err() {
            let _ = std::fs::remove_file(&staged);
        }
        installed.context("无法更新 ~/.claude/settings.json")
    }

    fn status_line_is_ours(&self, settings: &Value) -> bool {
        settings
            .get("statusLine")
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains(SCRIPT_FILE))
    }

    pub fn status(&self) -> Result<ClaudeHookStatus> {
        let settings = self.read_settings()?;
        let installed = self.status_line_is_ours(&settings);
        let conflict = !installed
            && settings
                .get("statusLine")
                .is_some_and(|value| !value.is_null());
        let last_data_at_ms = self.read_quota_file().map(|file| file.received_at_ms);
        Ok(ClaudeHookStatus {
            installed,
            conflict,
            last_data_at_ms,
        })
    }

    pub fn install(&self) -> Result<ClaudeHookStatus> {
        let mut settings = self.read_settings()?;
        if !self.status_line_is_ours(&settings)
            && settings
                .get("statusLine")
                .is_some_and(|value| !value.is_null())
        {
            bail!(
                "Claude Code 已配置其他 statusLine，为避免覆盖未安装。请先在 ~/.claude/settings.json 移除 statusLine 后重试。"
            );
        }

        std::fs::create_dir_all(&self.claude_dir)?;
        std::fs::write(self.script_path(), SCRIPT_BODY).context("无法写入钩子脚本")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(self.script_path(), std::fs::Permissions::from_mode(0o755));
        }

        let root = settings
            .as_object_mut()
            .context("settings.json 顶层不是对象")?;
        root.insert(
            "statusLine".into(),
            json!({ "type": "command", "command": self.hook_command(), "padding": 0 }),
        );
        self.write_settings(&settings)?;
        self.status()
    }

    pub fn uninstall(&self) -> Result<ClaudeHookStatus> {
        let mut settings = self.read_settings()?;
        if self.status_line_is_ours(&settings) {
            let root = settings
                .as_object_mut()
                .context("settings.json 顶层不是对象")?;
            root.remove("statusLine");
            self.write_settings(&settings)?;
        }
        let _ = std::fs::remove_file(self.script_path());
        let _ = std::fs::remove_file(self.quota_path());
        self.status()
    }

    fn read_quota_file(&self) -> Option<QuotaFile> {
        let raw = std::fs::read_to_string(self.quota_path()).ok()?;
        serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()
    }

    /// 把钩子落地的官方窗口转换成 QuotaSample；文件缺失或格式异常返回空，
    /// 不猜测、不沿用陈旧文件之外的任何来源。
    pub fn quota_samples(&self) -> Vec<QuotaSample> {
        let Some(file) = self.read_quota_file() else {
            return Vec::new();
        };
        let mut samples = Vec::new();
        let mut push = |window: &Option<QuotaWindow>, key: &'static str| {
            if let Some(window) = window {
                samples.push(QuotaSample {
                    adapter_id: "claude",
                    window_key: key,
                    remaining_percent: (100.0 - window.used_percentage).clamp(0.0, 100.0),
                    resets_at_ms: window.resets_at.map(|value| (value * 1000.0) as i64),
                    collected_at_ms: file.received_at_ms,
                    source_label: "statusLine 钩子".into(),
                    quality: "official_snapshot",
                });
            }
        };
        push(&file.windows.five_hour, "primary");
        push(&file.windows.seven_day, "secondary");
        samples
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
                "metrik-claude-hook-{label}-{}-{}",
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
    fn install_writes_script_and_status_line_then_uninstall_restores() {
        let test = TestDirectory::new("roundtrip");
        fs::write(
            test.path().join("settings.json"),
            r#"{"model": "opus", "env": {"KEY": "value"}}"#,
        )
        .unwrap();
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());

        let status = hook.install().unwrap();
        assert!(status.installed);
        assert!(!status.conflict);
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(settings["model"], "opus");
        assert_eq!(settings["env"]["KEY"], "value");
        assert!(settings["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("metrik-statusline"));
        assert!(hook.script_path().exists());

        let status = hook.uninstall().unwrap();
        assert!(!status.installed);
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        assert!(settings.get("statusLine").is_none());
        assert_eq!(settings["model"], "opus");
        assert!(!hook.script_path().exists());
    }

    #[test]
    fn existing_foreign_status_line_is_never_overwritten() {
        let test = TestDirectory::new("conflict");
        fs::write(
            test.path().join("settings.json"),
            r#"{"statusLine": {"type": "command", "command": "my-own-line"}}"#,
        )
        .unwrap();
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());

        let status = hook.status().unwrap();
        assert!(status.conflict);
        let error = hook.install().unwrap_err();
        assert!(error.to_string().contains("已配置其他 statusLine"));
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(settings["statusLine"]["command"], "my-own-line");
    }

    #[test]
    fn quota_file_converts_to_remaining_percent_samples() {
        let test = TestDirectory::new("quota");
        fs::write(
            test.path().join(QUOTA_FILE),
            r#"{"receivedAtMs": 1783000000000,
                "windows": {
                    "five_hour": {"usedPercentage": 6.0, "resetsAt": 1783003600.5},
                    "seven_day": {"usedPercentage": 41.5}
                }}"#,
        )
        .unwrap();
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());

        let samples = hook.quota_samples();

        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].adapter_id, "claude");
        assert_eq!(samples[0].window_key, "primary");
        assert!((samples[0].remaining_percent - 94.0).abs() < f64::EPSILON);
        assert_eq!(samples[0].resets_at_ms, Some(1_783_003_600_500));
        assert_eq!(samples[1].window_key, "secondary");
        assert!((samples[1].remaining_percent - 58.5).abs() < f64::EPSILON);
        assert_eq!(samples[1].resets_at_ms, None);

        // 缺失文件 → 空，不猜测。
        let empty = ClaudeHook::with_dir(test.path().join("missing"));
        assert!(empty.quota_samples().is_empty());
    }
}
