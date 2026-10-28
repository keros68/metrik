use crate::domain::{sane_resets_at_ms, QuotaSample};
use crate::i18n::tr;
#[cfg(not(windows))]
use crate::statusline::shell_single_quote;
use crate::statusline::{self, read_json_file, Host, Installer, LegacyScript};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

/// Claude Code 官方配额的零凭据来源：statusLine 钩子。
///
/// Claude Code 每次刷新状态栏都会把当前会话 JSON（含
/// `rate_limits.five_hour / seven_day` 的 used_percentage 与 resets_at）
/// 通过 stdin 推给 statusLine 命令。`metrik --statusline` 只提取这两个窗口并
/// 原子写入 `~/.claude/metrik-quota.json`，同时输出一行简洁的状态栏文本；
/// 不读取、不保存对话内容或凭据。安装、串联与委托见 `statusline` 模块。
const QUOTA_FILE: &str = "metrik-quota.json";
const BACKUP_FILE: &str = "metrik-statusline.backup.json";
pub(crate) const SOURCE_LABEL: &str = "statusLine 钩子";

/// 旧版本生成的委托脚本文件名，只在升级迁移时用于识别并删除；不再生成。
#[cfg(windows)]
const SCRIPT_FILE: &str = "metrik-statusline.ps1";
#[cfg(not(windows))]
const SCRIPT_FILE: &str = "metrik-statusline.py";

static HOST: Host = Host {
    name: "Claude Code",
    settings_label: "~/.claude/settings.json",
    quota_file: QUOTA_FILE,
    backup_file: BACKUP_FILE,
    metadata_file: "metrik-statusline.json",
    flag: "--statusline",
    hook_command,
    status_line: |command| json!({ "type": "command", "command": command, "padding": 0 }),
    empty_command_is_unconfigured: false,
    legacy_script: Some(LegacyScript {
        file: SCRIPT_FILE,
        delegate: legacy_script_delegate,
    }),
};

pub type ClaudeHookStatus = statusline::HookStatus;

#[derive(Deserialize)]
struct QuotaFile {
    #[serde(rename = "receivedAtMs")]
    received_at_ms: i64,
    #[serde(default)]
    windows: std::collections::BTreeMap<String, QuotaWindow>,
}

#[derive(Deserialize)]
struct QuotaWindow {
    #[serde(rename = "usedPercentage")]
    used_percentage: f64,
    #[serde(rename = "resetsAt")]
    resets_at: Option<f64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusLinePayload {
    received_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    windows: Option<std::collections::BTreeMap<String, StatusLineWindow>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusLineWindow {
    used_percentage: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    resets_at: Option<f64>,
}

fn numeric_value(value: &Value) -> Option<f64> {
    let value = value
        .as_f64()
        .or_else(|| value.as_str()?.parse::<f64>().ok())?;
    value.is_finite().then_some(value)
}

fn percentage_value(value: &Value) -> Option<f64> {
    let value = numeric_value(value)?;
    (0.0..=100.0).contains(&value).then_some(value)
}

fn payload_from_input(input: &Value) -> StatusLinePayload {
    let windows = input
        .get("rate_limits")
        .and_then(Value::as_object)
        .map(|rate_limits| {
            rate_limits
                .iter()
                .filter_map(|(key, entry)| {
                    let used_percentage = percentage_value(entry.get("used_percentage")?)?;
                    let resets_at = entry.get("resets_at").and_then(numeric_value);
                    Some((
                        key.clone(),
                        StatusLineWindow {
                            used_percentage,
                            resets_at,
                        },
                    ))
                })
                .collect()
        });
    StatusLinePayload {
        received_at_ms: chrono::Utc::now().timestamp_millis(),
        windows,
    }
}

fn quota_parts(payload: &StatusLinePayload) -> Vec<String> {
    let Some(windows) = payload.windows.as_ref() else {
        return Vec::new();
    };
    [("five_hour", "5h"), ("seven_day", "7d")]
        .into_iter()
        .filter_map(|(key, label)| {
            windows
                .get(key)
                .map(|window| format!("{label} {:.0}%", window.used_percentage))
        })
        .collect()
}

fn render_statusline(
    hook: &ClaudeHook,
    input: &[u8],
    delegate_timeout: std::time::Duration,
) -> String {
    let metadata = hook.read_metadata();
    let data = serde_json::from_slice::<Value>(input).ok();
    let payload = data.as_ref().map(payload_from_input);
    let quota_text = payload
        .as_ref()
        .map(quota_parts)
        .unwrap_or_default()
        .join(" ");
    statusline::render(
        metadata.as_ref(),
        input,
        payload.and_then(|payload| serde_json::to_vec(&payload).ok()),
        &quota_text,
        statusline::model_name(data.as_ref(), "Claude"),
        delegate_timeout,
    )
}

pub fn run_statusline() {
    statusline::run(|input| {
        render_statusline(
            &ClaudeHook::detected(),
            input,
            std::time::Duration::from_secs(10),
        )
    });
}

/// 旧委托脚本里的原命令：Windows `.ps1` 的 `$delegate = '...'`（`'` 转义成
/// `''`），其他平台 `.py` 的 `DELEGATE = "..."`（JSON 字符串）。
fn legacy_script_delegate(script: &str) -> Option<String> {
    #[cfg(windows)]
    {
        let line = script
            .lines()
            .find_map(|l| l.strip_prefix("$delegate = '"))?;
        Some(line.strip_suffix('\'')?.replace("''", "'"))
    }
    #[cfg(not(windows))]
    {
        let line = script.lines().find_map(|l| l.strip_prefix("DELEGATE = "))?;
        serde_json::from_str::<String>(line).ok()
    }
}

/// statusLine 命令必须是「带引号的可执行文件绝对路径 + --statusline」。
/// Windows 上两个条件缺一不可：绝对路径——Path 写成 REG_SZ 的机器上
/// `%SystemRoot%` 不展开、System32 不在 PATH 里，裸 `powershell` 解析不到；
/// 引号——Claude Code 经 Git Bash 执行 statusLine，裸路径的反斜杠会被 bash
/// 当转义符吃掉（`C:\Windows\...` → `C:Windows...`）。两种失败都是「exit 127
/// 加零输出」，状态栏只表现为空白。Unix 上 Claude Code 经 `/bin/sh -c` 执行，
/// 用单引号包住路径避免空格与 shell 展开。
fn hook_command() -> Result<String> {
    let executable = std::env::current_exe().with_context(|| {
        tr!(
            "无法确定 metrik 可执行文件的绝对路径",
            "Couldn't determine the absolute path of the metrik executable"
        )
    })?;
    #[cfg(windows)]
    {
        Ok(format!("\"{}\" --statusline", executable.display()))
    }
    #[cfg(not(windows))]
    {
        Ok(format!(
            "{} --statusline",
            shell_single_quote(&executable.to_string_lossy())
        ))
    }
}

pub struct ClaudeHook(Installer);

impl std::ops::Deref for ClaudeHook {
    type Target = Installer;

    fn deref(&self) -> &Installer {
        &self.0
    }
}

impl ClaudeHook {
    /// Claude 配置目录：优先 `$CLAUDE_CONFIG_DIR`，否则 `~/.claude`。
    ///
    /// 与 `claude_oauth::config_dir` 保持一致：设了 `CLAUDE_CONFIG_DIR` 的用户，
    /// 钩子必须装进 Claude Code 实际读取的那个 settings.json，否则界面显示
    /// 「已安装」，实际永不执行、零数据。
    pub fn detected() -> Self {
        Self::with_dir(
            std::env::var_os("CLAUDE_CONFIG_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude")),
        )
    }

    pub fn with_dir(claude_dir: PathBuf) -> Self {
        Self(Installer::new(claude_dir, &HOST))
    }

    #[cfg(test)]
    fn script_path(&self) -> PathBuf {
        self.quota_path().with_file_name(SCRIPT_FILE)
    }

    /// 把钩子落地的全部官方窗口转换成 QuotaSample（原始窗口名作 key）；
    /// 文件缺失或格式异常返回空，不猜测、不沿用陈旧文件之外的任何来源。
    pub fn quota_samples(&self) -> Vec<QuotaSample> {
        let Some(file) = read_json_file::<QuotaFile>(&self.quota_path()) else {
            return Vec::new();
        };
        file.windows
            .iter()
            .filter(|(_, window)| {
                window.used_percentage.is_finite()
                    && (0.0..=100.0).contains(&window.used_percentage)
            })
            .map(|(key, window)| QuotaSample {
                adapter_id: "claude",
                window_key: key.clone(),
                remaining_percent: 100.0 - window.used_percentage,
                resets_at_ms: window
                    .resets_at
                    .map(|value| (value * 1000.0) as i64)
                    .and_then(|value| sane_resets_at_ms(key, value, file.received_at_ms)),
                collected_at_ms: file.received_at_ms,
                source_label: SOURCE_LABEL.into(),
                quality: "official_snapshot",
            })
            .collect()
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
    fn install_writes_hook_and_status_line_then_uninstall_restores() {
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
        // 所有平台都装成原生入口：命令以 --statusline 结尾、元数据落位、无脚本。
        assert!(settings["statusLine"]["command"]
            .as_str()
            .unwrap()
            .ends_with(" --statusline"));
        assert!(hook.metadata_path().exists());
        assert!(!hook.script_path().exists());

        let status = hook.uninstall().unwrap();
        assert!(!status.installed);
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        assert!(settings.get("statusLine").is_none());
        assert_eq!(settings["model"], "opus");
        // 用户文件的键序原样保留，不被重排成字母序。
        let raw = fs::read_to_string(test.path().join("settings.json")).unwrap();
        assert!(raw.find("\"model\"") < raw.find("\"env\""), "{raw}");
        assert!(!hook.script_path().exists());
        assert!(!hook.metadata_path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn install_keeps_a_symlinked_settings_file_linked() {
        let test = TestDirectory::new("symlink");
        let dotfiles = test.path().join("dotfiles-settings.json");
        fs::write(&dotfiles, r#"{"model": "opus"}"#).unwrap();
        let claude_dir = test.path().join("claude");
        fs::create_dir_all(&claude_dir).unwrap();
        std::os::unix::fs::symlink(&dotfiles, claude_dir.join("settings.json")).unwrap();

        ClaudeHook::with_dir(claude_dir.clone()).install().unwrap();

        assert!(fs::symlink_metadata(claude_dir.join("settings.json"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(fs::read_to_string(&dotfiles)
            .unwrap()
            .contains("statusLine"));
    }

    /// 回归：statusLine 由 Git Bash 执行，命令必须是带引号的绝对路径。
    /// 裸 `powershell`（PATH 解析不到）和不带引号的绝对路径（反斜杠被
    /// bash 吃掉）都会静默 exit 127，状态栏空白且钩子从不落盘。
    #[cfg(windows)]
    #[test]
    fn windows_hook_command_is_quoted_absolute_path() {
        let test = TestDirectory::new("hookcmd");
        let command = ClaudeHook::with_dir(test.path().to_path_buf())
            .hook_command()
            .unwrap();
        assert!(command.starts_with('"'), "exe 路径必须加引号: {command}");
        let exe = command[1..].split('"').next().unwrap();
        assert!(
            command.ends_with("\" --statusline"),
            "第一个 token 后只能是原生模式参数: {command}"
        );
        assert!(
            std::path::Path::new(exe).is_absolute(),
            "必须用绝对路径，不能依赖 PATH: {command}"
        );
    }

    /// 回归：Unix 上 Claude Code 经 `/bin/sh -c` 执行 statusLine，命令必须是
    /// 「单引号包住的绝对路径 + --statusline」，含空格的应用路径也不会被 shell
    /// 拆成两段（`/Applications/Metrik.app/...` 这类路径没有空格，但用户自定义
    /// 安装位置可能有）。
    #[cfg(not(windows))]
    #[test]
    fn unix_hook_command_is_single_quoted_absolute_path() {
        let test = TestDirectory::new("hookcmd");
        let command = ClaudeHook::with_dir(test.path().to_path_buf())
            .hook_command()
            .unwrap();
        assert!(
            command.starts_with('\''),
            "exe 路径必须单引号包住: {command}"
        );
        assert!(
            command.ends_with("' --statusline"),
            "第一个 token 后只能是原生模式参数: {command}"
        );
        let exe = command[1..].split('\'').next().unwrap();
        assert!(
            std::path::Path::new(exe).is_absolute(),
            "必须用绝对路径，不能依赖 PATH: {command}"
        );
    }

    #[test]
    fn existing_foreign_status_line_is_chained_and_restored() {
        let test = TestDirectory::new("chain");
        fs::write(
            test.path().join("settings.json"),
            r#"{"statusLine": {"type": "command", "command": "my-own-line 'quoted'", "padding": 0}}"#,
        )
        .unwrap();
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());

        // 可串联的 statusLine 不算冲突。
        let status = hook.status().unwrap();
        assert!(!status.conflict);
        assert!(!status.chained);

        // 安装：备份原设置、持久化原命令、statusLine 指向 Metrik。
        let status = hook.install().unwrap();
        assert!(status.installed);
        assert!(status.chained);
        // 委托原文进元数据（所有平台一致），不再嵌入脚本。
        assert_eq!(
            hook.read_metadata().unwrap().delegate,
            "my-own-line 'quoted'"
        );
        assert!(!hook.script_path().exists());
        let backup: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join(BACKUP_FILE)).unwrap())
                .unwrap();
        assert_eq!(backup["command"], "my-own-line 'quoted'");
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(
            settings["statusLine"]["command"],
            hook.hook_command().unwrap()
        );

        // 重装不丢串联：statusLine 已是我们的，命令沿用备份。
        let status = hook.install().unwrap();
        assert!(status.chained);
        assert_eq!(hook.installed_delegate().unwrap(), "my-own-line 'quoted'");

        // 卸载：原有 statusLine 原样恢复，备份清理。
        let status = hook.uninstall().unwrap();
        assert!(!status.installed);
        assert!(!status.chained);
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(settings["statusLine"]["command"], "my-own-line 'quoted'");
        assert_eq!(settings["statusLine"]["padding"], 0);
        assert!(!test.path().join(BACKUP_FILE).exists());
    }

    /// 在隔离环境里运行原生 statusLine 逻辑。
    fn run_native(hook: &ClaudeHook, payload: &[u8]) -> String {
        render_statusline(hook, payload, std::time::Duration::from_secs(10))
    }

    /// 回归：额度落盘位置必须是安装元数据保存的绝对路径。
    ///
    /// 不能从运行时的 HOME/USERPROFILE 现推，否则设置了 CLAUDE_CONFIG_DIR 的
    /// 用户会显示「已安装」却永远没有数据。
    #[test]
    fn native_writes_quota_to_the_installed_absolute_path() {
        let test = TestDirectory::new("quotapath");
        let hook = ClaudeHook::with_dir(test.path().join("cfg"));
        fs::create_dir_all(test.path().join("cfg")).unwrap();
        hook.install().unwrap();

        let mut metadata = hook.read_metadata().unwrap();
        assert!(metadata.quota_path.is_absolute());
        assert_eq!(metadata.quota_path, hook.quota_path());
        let installed_quota_dir = test.path().join("installed-quota");
        fs::create_dir_all(&installed_quota_dir).unwrap();
        metadata.quota_path = installed_quota_dir.join(QUOTA_FILE);
        hook.write_metadata(&metadata).unwrap();
        let output = run_native(
            &hook,
            br#"{"model":{"display_name":"Sonnet"},"rate_limits":{"five_hour":{"used_percentage":42},"seven_day":{"used_percentage":19.6,"resets_at":1785197748}}}"#,
        );
        assert_eq!(output, "Sonnet | 5h 42% 7d 20%");

        let written =
            fs::read_to_string(&metadata.quota_path).expect("额度必须写进安装时确定的目录");
        let quota: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(quota["windows"]["five_hour"]["usedPercentage"], 42.0);
        assert_eq!(quota["windows"]["seven_day"]["resetsAt"], 1785197748.0);
        assert!(
            !hook.quota_path().exists(),
            "不能从运行时 hook 目录重推落盘路径"
        );
        assert!(!metadata
            .quota_path
            .with_file_name(format!("{QUOTA_FILE}.tmp-{}", std::process::id()))
            .exists());
    }

    /// 回归：负载解析失败时，用户原有的 statusLine 仍然要渲染。
    ///
    /// 原来是 `catch { exit 0 }`——我们自己解析不了就整个退出，把用户的
    /// statusLine 一起带走了。额度抓不到是我们的事，不该由用户承担。
    #[cfg(windows)]
    #[test]
    fn windows_native_forwards_unparsable_input_to_delegate() {
        let test = TestDirectory::new("badjson");
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
        fs::write(
            test.path().join("settings.json"),
            json!({
                "statusLine": {
                    "type": "command",
                    "command": format!(r#""{system_root}\System32\cmd.exe" /D /C findstr /R ".*""#),
                }
            })
            .to_string(),
        )
        .unwrap();
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());
        hook.install().unwrap();

        let out = run_native(&hook, b"this is not json at all");
        assert_eq!(out, "this is not json at all");
        assert!(!hook.quota_path().exists(), "无效 JSON 不应写入额度文件");
    }

    /// 单行委托追加额度；多行面板保持自己的排版，不能被压成一行。
    #[cfg(windows)]
    #[test]
    fn windows_native_preserves_multiline_delegate_layout() {
        let test = TestDirectory::new("multiline");
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
        let settings = |command: String| {
            json!({
                "statusLine": {
                    "type": "command",
                    "command": command,
                }
            })
            .to_string()
        };
        let payload = br#"{"rate_limits":{"five_hour":{"used_percentage":7}}}"#;

        fs::write(
            test.path().join("settings.json"),
            settings(format!(
                r#""{system_root}\System32\cmd.exe" /D /C echo ONE"#
            )),
        )
        .unwrap();
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());
        hook.install().unwrap();
        assert_eq!(run_native(&hook, payload), "ONE | 5h 7%");

        let multiline_script = test.path().join("multiline.cmd");
        fs::write(
            &multiline_script,
            "@echo off\r\necho FIRST\r\necho SECOND\r\n",
        )
        .unwrap();
        let multiline = format!(r#""{}""#, multiline_script.display());
        let mut metadata = hook.read_metadata().unwrap();
        metadata.delegate = multiline;
        hook.write_metadata(&metadata).unwrap();
        assert_eq!(run_native(&hook, payload), "FIRST\r\nSECOND");
    }

    /// 回归：负载解析失败时，用户原有的 statusLine 仍然要渲染（Unix）。
    /// 旧 Python 钩子 `except: exit` 会把用户的 statusLine 一起带走。
    #[cfg(unix)]
    #[test]
    fn unix_native_forwards_unparsable_input_to_delegate() {
        let test = TestDirectory::new("badjson");
        fs::write(
            test.path().join("settings.json"),
            json!({ "statusLine": { "type": "command", "command": "cat" } }).to_string(),
        )
        .unwrap();
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());
        hook.install().unwrap();

        // `cat` 把 stdin 原样回显：解析失败也要把输入透传给委托。
        let out = run_native(&hook, b"this is not json at all");
        assert_eq!(out, "this is not json at all");
        assert!(!hook.quota_path().exists(), "无效 JSON 不应写入额度文件");
    }

    /// 单行委托追加额度；多行面板保持自己的排版，不能被压成一行（Unix）。
    #[cfg(unix)]
    #[test]
    fn unix_native_preserves_multiline_delegate_layout() {
        let test = TestDirectory::new("multiline");
        let payload = br#"{"rate_limits":{"five_hour":{"used_percentage":7}}}"#;

        fs::write(
            test.path().join("settings.json"),
            json!({ "statusLine": { "type": "command", "command": "printf ONE" } }).to_string(),
        )
        .unwrap();
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());
        hook.install().unwrap();
        assert_eq!(run_native(&hook, payload), "ONE | 5h 7%");

        let mut metadata = hook.read_metadata().unwrap();
        // printf 把 `\n` 展开成换行：委托输出两行面板。
        metadata.delegate = "printf 'FIRST\\nSECOND\\n'".to_owned();
        hook.write_metadata(&metadata).unwrap();
        assert_eq!(run_native(&hook, payload), "FIRST\nSECOND");
    }

    /// 回归：原生路径必须清理上一次调用被终止后留下的额度暂存文件。
    ///
    /// Claude Code 每次重渲染状态栏都会终止上一次调用，而串联一个委托要好几秒，
    /// 所以绝大多数调用都是中途被终止的，改名那一步来不及执行，暂存文件会一直
    /// 留在盘上。
    #[test]
    fn native_sweeps_temp_files_from_terminated_runs() {
        use std::time::{Duration, SystemTime};

        let test = TestDirectory::new("tempsweep");
        let claude_dir = test.path().join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        let hook = ClaudeHook::with_dir(claude_dir.clone());
        hook.install().unwrap();

        // 上一次被终止后留下的残留，以及一份正在被别的调用使用的新文件。
        let orphan = claude_dir.join("metrik-quota.json.tmp-999999");
        let live = claude_dir.join("metrik-quota.json.tmp-111111");
        for path in [&orphan, &live] {
            fs::write(path, "{}").unwrap();
        }
        fs::File::options()
            .write(true)
            .open(&orphan)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(30 * 60))
            .unwrap();

        run_native(
            &hook,
            br#"{"rate_limits":{"five_hour":{"used_percentage":7}}}"#,
        );

        assert!(!orphan.exists(), "过期的暂存文件没被清掉");
        assert!(live.exists(), "还在用的暂存文件不能被误删");
    }

    /// 回归：委托超过时限时要终止整个进程树，不能冻结状态栏或留下后代进程。
    #[cfg(windows)]
    #[test]
    fn windows_native_times_out_and_terminates_delegate_tree() {
        use std::os::windows::process::CommandExt;
        use std::time::{Duration, Instant};

        let test = TestDirectory::new("timeout");
        let marker = test.path().join("descendant.txt");
        let script = test.path().join("delegate.ps1");
        fs::write(
            &script,
            r#"$child = Start-Process -FilePath "$env:SystemRoot\System32\PING.EXE" -ArgumentList "-n","12","127.0.0.1" -WindowStyle Hidden -PassThru
$child.Id | Set-Content -LiteralPath $args[0] -Encoding ascii
Wait-Process -Id $child.Id
"#,
        )
        .unwrap();
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
        fs::write(
            test.path().join("settings.json"),
            json!({
                "statusLine": {
                    "type": "command",
                    "command": format!(
                        r#""{system_root}\System32\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "{}" "{}""#,
                        script.display(),
                        marker.display()
                    ),
                }
            })
            .to_string(),
        )
        .unwrap();
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());
        hook.install().unwrap();

        // Use the production timeout so a contended CI runner still has time to start
        // PowerShell and record the descendant before the process tree is terminated.
        let delegate_timeout = Duration::from_secs(10);
        let started = Instant::now();
        let output = render_statusline(&hook, b"{}", delegate_timeout);
        assert_eq!(output, "");
        assert!(started.elapsed() < delegate_timeout + Duration::from_secs(4));

        let descendant_pid = fs::read_to_string(&marker)
            .expect("delegate should record its descendant before timing out")
            .trim()
            .parse::<u32>()
            .unwrap();
        let listing = std::process::Command::new("tasklist.exe")
            .args([
                "/FI",
                &format!("PID eq {descendant_pid}"),
                "/FO",
                "CSV",
                "/NH",
            ])
            .creation_flags(0x0800_0000)
            .output()
            .unwrap();
        let descendant_alive =
            String::from_utf8_lossy(&listing.stdout).contains(&format!("\"{descendant_pid}\""));
        if descendant_alive {
            let _ = std::process::Command::new("taskkill.exe")
                .args(["/PID", &descendant_pid.to_string(), "/T", "/F"])
                .creation_flags(0x0800_0000)
                .status();
        }
        assert!(!descendant_alive, "超时后代进程仍在运行: {descendant_pid}");
    }

    /// 回归：委托超时要按进程组终止，连带它后台拉起的后代一起收掉，不能冻结
    /// 状态栏或留下孤儿进程（Unix）。
    #[cfg(unix)]
    #[test]
    fn unix_native_times_out_and_terminates_delegate_group() {
        use std::time::{Duration, Instant};

        let test = TestDirectory::new("timeout");
        let marker = test.path().join("descendant.pid");
        // 后台拉起 sleep 后代，记下它的 pid 再 wait 阻塞：超时后整组被 killpg。
        let delegate = format!("sleep 30 & echo $! > '{}'; wait", marker.display());
        fs::write(
            test.path().join("settings.json"),
            json!({ "statusLine": { "type": "command", "command": delegate } }).to_string(),
        )
        .unwrap();
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());
        hook.install().unwrap();

        let delegate_timeout = Duration::from_secs(3);
        let started = Instant::now();
        let output = render_statusline(&hook, b"{}", delegate_timeout);
        assert_eq!(output, "");
        assert!(started.elapsed() < delegate_timeout + Duration::from_secs(4));

        // 给内核一点时间收尸后再探活。
        std::thread::sleep(Duration::from_millis(200));
        let descendant_pid = fs::read_to_string(&marker)
            .expect("delegate should record its descendant before timing out")
            .trim()
            .parse::<i32>()
            .unwrap();
        // kill(pid, 0) 探活：进程还在返回 0，已回收返回 -1(ESRCH)。
        let alive = unsafe { libc::kill(descendant_pid, 0) } == 0;
        if alive {
            unsafe {
                libc::kill(descendant_pid, libc::SIGKILL);
            }
        }
        assert!(!alive, "超时后代进程仍在运行: {descendant_pid}");
    }

    /// Windows 升级自愈：旧 `.ps1` 仍是第二真相源时，把 delegate 迁入元数据，
    /// 改写 settings.json 指向原生入口，并删除旧脚本。
    #[cfg(windows)]
    #[test]
    fn repair_migrates_legacy_windows_script_to_native_metadata() {
        let test = TestDirectory::new("migratelegacy");
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());
        fs::write(
            hook.script_path(),
            "\u{feff}# legacy hook\r\n$delegate = 'my-own-line ''quoted'''\r\n",
        )
        .unwrap();
        fs::write(
            test.path().join("settings.json"),
            json!({
                "statusLine": {
                    "type": "command",
                    "command": format!(
                        r#"powershell.exe -NoProfile -File "{}""#,
                        hook.script_path().display()
                    ),
                    "padding": 0,
                }
            })
            .to_string(),
        )
        .unwrap();

        assert!(hook.repair().unwrap());
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(
            settings["statusLine"]["command"],
            hook.hook_command().unwrap()
        );
        let metadata = hook.read_metadata().unwrap();
        assert_eq!(metadata.delegate, "my-own-line 'quoted'");
        assert!(metadata.quota_path.is_absolute());
        assert!(!hook.script_path().exists());
        assert!(!hook.repair().unwrap(), "迁移完成后不应每次启动都重写");
    }

    /// Unix 升级自愈：旧 `.py` 仍在时，把 delegate 迁入元数据，改写 settings.json
    /// 指向原生入口（不再依赖 python3），并删除旧脚本。
    #[cfg(not(windows))]
    #[test]
    fn repair_migrates_legacy_python_script_to_native_metadata() {
        let test = TestDirectory::new("migratelegacy");
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());
        // 旧生成器把 delegate 作为 JSON 字符串写进 `DELEGATE = "..."` 那行。
        fs::write(
            hook.script_path(),
            "#!/usr/bin/env python3\nDELEGATE = \"my-own-line 'quoted'\"\nTARGET = \"x\"\n",
        )
        .unwrap();
        fs::write(
            test.path().join("settings.json"),
            json!({
                "statusLine": {
                    "type": "command",
                    "command": format!("python3 \"{}\"", hook.script_path().display()),
                    "padding": 0,
                }
            })
            .to_string(),
        )
        .unwrap();

        assert!(hook.repair().unwrap());
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(
            settings["statusLine"]["command"],
            hook.hook_command().unwrap()
        );
        assert!(!settings["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("python3"));
        let metadata = hook.read_metadata().unwrap();
        assert_eq!(metadata.delegate, "my-own-line 'quoted'");
        assert!(metadata.quota_path.is_absolute());
        assert!(!hook.script_path().exists());
        assert!(!hook.repair().unwrap(), "迁移完成后不应每次启动都重写");
    }

    /// 启动自愈：元数据丢失（用户清理 .claude、同步工具、杀软）后，命令仍指向
    /// 原生入口的情况要修好，且串联的原命令不能丢——从备份回读补回元数据。
    #[cfg(not(windows))]
    #[test]
    fn repair_rewrites_missing_metadata_and_recovers_delegate_from_backup() {
        let test = TestDirectory::new("repairstale");
        fs::write(
            test.path().join("settings.json"),
            r#"{"statusLine": {"type": "command", "command": "my-own-line", "padding": 0}}"#,
        )
        .unwrap();
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());
        hook.install().unwrap();
        assert_eq!(hook.read_metadata().unwrap().delegate, "my-own-line");

        // 已经正确：不再写盘，否则每次启动都动 settings.json。
        assert!(!hook.repair().unwrap(), "命令正确时不该重写");

        // 元数据文件被删：状态栏落不了盘，自愈要补回，delegate 从备份恢复。
        fs::remove_file(hook.metadata_path()).unwrap();
        assert!(hook.repair().unwrap(), "元数据丢失应该被补回");
        assert_eq!(
            hook.read_metadata().unwrap().delegate,
            "my-own-line",
            "自愈把 delegate 丢了"
        );
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(
            settings["statusLine"]["command"],
            hook.hook_command().unwrap()
        );

        // 重新生成之后不该再写盘。
        assert!(!hook.repair().unwrap(), "补回之后不该再重写");
    }

    /// 自愈只修 Metrik 自己的 statusLine。别人的配置——包括用户压根没装钩子
    /// 的情况——一律不碰，否则自愈本身就变成了这次要修的那种覆盖。
    #[test]
    fn repair_never_touches_a_foreign_or_absent_status_line() {
        let test = TestDirectory::new("repairforeign");
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());

        // 没有 settings.json：不该无中生有装一个钩子。
        assert!(!hook.repair().unwrap());
        assert!(!test.path().join("settings.json").exists());

        // 别人的 statusLine：原样保留。
        let foreign = r#"{"statusLine": {"type": "command", "command": "my-own-line"}}"#;
        fs::write(test.path().join("settings.json"), foreign).unwrap();
        assert!(!hook.repair().unwrap());
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(settings["statusLine"]["command"], "my-own-line");
        assert!(!hook.script_path().exists());
    }

    /// 回归：备份文件被删掉（用户清理 .claude、同步工具、杀软）之后，
    /// 重装不能把用户原有的 statusLine 静默降级成「无委托」，卸载也不能
    /// 把 statusLine 整个删掉——两者都会让原配置永久消失。
    #[test]
    fn deleted_backup_recovers_delegate_from_installation_state() {
        let test = TestDirectory::new("lostbackup");
        fs::write(
            test.path().join("settings.json"),
            r#"{"statusLine": {"type": "command", "command": "my-own-line 'quoted'", "padding": 0}}"#,
        )
        .unwrap();
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());
        hook.install().unwrap();

        // 备份没了，但 statusLine 仍指向 Metrik。
        fs::remove_file(test.path().join(BACKUP_FILE)).unwrap();

        // 重装：delegate 必须从安装状态回读，不能退化成无委托。
        hook.install().unwrap();
        assert_eq!(
            hook.installed_delegate().unwrap(),
            "my-own-line 'quoted'",
            "重装丢了 delegate"
        );

        // 卸载：按持久化的原命令还原，而不是删掉 statusLine。
        hook.uninstall().unwrap();
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(settings["statusLine"]["command"], "my-own-line 'quoted'");
    }

    #[test]
    fn status_line_without_command_field_is_a_conflict() {
        let test = TestDirectory::new("conflict");
        fs::write(
            test.path().join("settings.json"),
            r#"{"statusLine": {"type": "something-else"}}"#,
        )
        .unwrap();
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());

        let status = hook.status().unwrap();
        assert!(status.conflict);
        let error = hook.install().unwrap_err();
        assert!(error.to_string().contains("无法串联"));
        let settings: Value =
            serde_json::from_str(&fs::read_to_string(test.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(settings["statusLine"]["type"], "something-else");
    }

    #[test]
    fn status_detects_replacement_and_reinstall_chains_the_new_command() {
        let test = TestDirectory::new("replaced");
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());
        hook.install().unwrap();

        fs::write(
            test.path().join("settings.json"),
            r#"{"statusLine":{"type":"command","command":"replacement-line"}}"#,
        )
        .unwrap();

        let status = hook.status().unwrap();
        assert!(!status.installed);
        assert!(status.replaced);
        assert!(!status.conflict);

        let status = hook.install().unwrap();
        assert!(status.installed);
        assert!(!status.replaced);
        assert!(status.chained);
        assert_eq!(
            hook.installed_delegate().as_deref(),
            Some("replacement-line")
        );
    }

    #[test]
    fn payload_rejects_non_finite_and_out_of_range_percentages() {
        let payload = payload_from_input(&json!({
            "rate_limits": {
                "negative": {"used_percentage": -1},
                "too_large": {"used_percentage": 101},
                "not_finite": {"used_percentage": "NaN"},
                "zero": {"used_percentage": 0},
                "full": {"used_percentage": "100"}
            }
        }));
        let windows = payload.windows.unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows["zero"].used_percentage, 0.0);
        assert_eq!(windows["full"].used_percentage, 100.0);
    }

    #[test]
    fn hook_status_marks_old_data_stale() {
        let test = TestDirectory::new("stale-status");
        let received_at_ms =
            chrono::Utc::now().timestamp_millis() - statusline::MAX_SNAPSHOT_AGE_MS - 1;
        fs::write(
            test.path().join(QUOTA_FILE),
            format!(r#"{{"receivedAtMs":{received_at_ms},"windows":{{}}}}"#),
        )
        .unwrap();

        let status = ClaudeHook::with_dir(test.path().to_path_buf())
            .status()
            .unwrap();
        assert!(status.stale);
        assert_eq!(status.last_data_at_ms, Some(received_at_ms));
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
        assert_eq!(samples[0].window_key, "five_hour");
        assert!((samples[0].remaining_percent - 94.0).abs() < f64::EPSILON);
        assert_eq!(samples[0].resets_at_ms, Some(1_783_003_600_500));
        assert_eq!(samples[1].window_key, "seven_day");
        assert!((samples[1].remaining_percent - 58.5).abs() < f64::EPSILON);
        assert_eq!(samples[1].resets_at_ms, None);

        // 缺失文件 → 空，不猜测。
        let empty = ClaudeHook::with_dir(test.path().join("missing"));
        assert!(empty.quota_samples().is_empty());
    }

    #[test]
    fn quota_file_drops_invalid_percentages_instead_of_clamping_them() {
        let test = TestDirectory::new("quota-invalid");
        fs::write(
            test.path().join(QUOTA_FILE),
            r#"{"receivedAtMs": 1783000000000,
                "windows": {
                    "negative": {"usedPercentage": -1},
                    "too_large": {"usedPercentage": 101},
                    "valid": {"usedPercentage": 42}
                }}"#,
        )
        .unwrap();

        let samples = ClaudeHook::with_dir(test.path().to_path_buf()).quota_samples();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].window_key, "valid");
        assert_eq!(samples[0].remaining_percent, 58.0);
    }

    #[test]
    fn sentinel_resets_at_is_dropped_but_plausible_kept() {
        let test = TestDirectory::new("sentinel");
        // Claude Code 在重置时间未知时下发哨兵值（1900000000 秒 ≈ 2030 年），
        // 展示出来是"1331 天后重置"（用户实测）。超出窗口语义的重置时间丢弃。
        fs::write(
            test.path().join(QUOTA_FILE),
            r#"{"receivedAtMs": 1784938548385,
                "windows": {
                    "five_hour": {"usedPercentage": 42.3, "resetsAt": 1900000000},
                    "seven_day": {"usedPercentage": 18.7, "resetsAt": 1900000000}
                }}"#,
        )
        .unwrap();
        let hook = ClaudeHook::with_dir(test.path().to_path_buf());
        let samples = hook.quota_samples();
        assert_eq!(samples.len(), 2);
        assert!(samples.iter().all(|sample| sample.resets_at_ms.is_none()));

        // 窗口语义内的重置时间保留：5h 窗 +2 小时、7d 窗 +3 天。
        fs::write(
            test.path().join(QUOTA_FILE),
            r#"{"receivedAtMs": 1784938548385,
                "windows": {
                    "five_hour": {"usedPercentage": 42.3, "resetsAt": 1784945748.0},
                    "seven_day": {"usedPercentage": 18.7, "resetsAt": 1785197748.0}
                }}"#,
        )
        .unwrap();
        let samples = hook.quota_samples();
        assert_eq!(samples[0].resets_at_ms, Some(1_784_945_748_000));
        assert_eq!(samples[1].resets_at_ms, Some(1_785_197_748_000));
    }
}
