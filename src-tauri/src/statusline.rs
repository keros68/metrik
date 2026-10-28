//! statusLine 钩子的通用部分。
//!
//! Claude Code 与 Antigravity CLI 都把会话 JSON 经 stdin 推给 settings.json
//! 里配置的 statusLine 命令。两边的安装、串联用户原有命令、启动自愈、卸载
//! 还原与委托执行完全一致；各宿主的差异（文件名、命令写法、statusLine 对象
//! 的附加字段）集中在 [`Host`]，额度载荷的解析留在各自模块。
//!
//! 用户已有自定义 statusLine 时不覆盖而是串联：原设置备份到备份文件，委托
//! 原文与额度落盘的绝对路径写进元数据；钩子运行时落完额度再把 stdin 原样
//! 转给原命令，卸载时原样恢复。

use crate::i18n::tr;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 钩子落盘的额度快照超过这个年龄即视为过期。
pub(crate) const MAX_SNAPSHOT_AGE_MS: i64 = 15 * 60 * 1000;

/// 宿主之间的差异。
pub(crate) struct Host {
    /// 错误提示里的宿主名。
    pub name: &'static str,
    /// 错误提示里的 settings.json 位置。
    pub settings_label: &'static str,
    pub quota_file: &'static str,
    pub backup_file: &'static str,
    pub metadata_file: &'static str,
    /// metrik 的命令行旗标；命令以它结尾且指向 metrik 时认作我们安装的。
    pub flag: &'static str,
    /// 本宿主能执行的 statusLine 命令。
    pub hook_command: fn() -> Result<String>,
    /// 用给定命令构造本宿主的 statusLine 对象。
    pub status_line: fn(&str) -> Value,
    /// command 为空的 statusLine 视为未配置，可直接覆盖。
    pub empty_command_is_unconfigured: bool,
    /// 旧版本生成的委托脚本，只用于升级迁移。
    pub legacy_script: Option<LegacyScript>,
}

pub(crate) struct LegacyScript {
    pub file: &'static str,
    /// 从脚本正文回读委托原文。
    pub delegate: fn(&str) -> Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookStatus {
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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusLineMetadata {
    pub delegate: String,
    pub quota_path: PathBuf,
}

/// 两个宿主的额度文件都以 `receivedAtMs` 记录落盘时间。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReceivedAt {
    received_at_ms: i64,
}

pub struct Installer {
    dir: PathBuf,
    host: &'static Host,
}

impl Installer {
    pub fn new(dir: PathBuf, host: &'static Host) -> Self {
        Self { dir, host }
    }

    fn settings_path(&self) -> PathBuf {
        self.dir.join("settings.json")
    }

    pub fn quota_path(&self) -> PathBuf {
        self.dir.join(self.host.quota_file)
    }

    fn backup_path(&self) -> PathBuf {
        self.dir.join(self.host.backup_file)
    }

    pub fn metadata_path(&self) -> PathBuf {
        self.dir.join(self.host.metadata_file)
    }

    pub fn hook_command(&self) -> Result<String> {
        (self.host.hook_command)()
    }

    fn legacy_script_path(&self) -> Option<PathBuf> {
        self.host
            .legacy_script
            .as_ref()
            .map(|script| self.dir.join(script.file))
    }

    fn read_backup(&self) -> Option<Value> {
        read_json_file(&self.backup_path())
    }

    pub fn read_metadata(&self) -> Option<StatusLineMetadata> {
        read_json_file(&self.metadata_path())
    }

    fn expected_metadata(&self, delegate: String) -> Result<StatusLineMetadata> {
        let quota_path = self.quota_path();
        let quota_path = if quota_path.is_absolute() {
            quota_path
        } else {
            let host = self.host.name;
            std::env::current_dir()
                .with_context(|| {
                    tr!(
                        "无法确定 {host} quota 文件的绝对路径",
                        "Couldn't determine the absolute path of the {host} quota file"
                    )
                })?
                .join(quota_path)
        };
        Ok(StatusLineMetadata {
            delegate,
            quota_path,
        })
    }

    pub fn write_metadata(&self, metadata: &StatusLineMetadata) -> Result<()> {
        write_atomically(&self.metadata_path(), &serde_json::to_vec_pretty(metadata)?).with_context(
            || {
                tr!(
                    "无法写入 statusLine 元数据",
                    "Couldn't write the statusLine metadata"
                )
            },
        )
    }

    /// 备份文件是用户可见的普通文件，会被清理工具、同步工具或杀软删掉。备份
    /// 没了而 statusLine 仍是我们的时，重装若把 delegate 当成空，用户原有的
    /// statusLine 就永久消失。所以先从元数据回读；升级迁移时再退回旧脚本。
    pub fn installed_delegate(&self) -> Option<String> {
        if let Some(metadata) = self.read_metadata() {
            return Some(metadata.delegate);
        }
        let script = self.host.legacy_script.as_ref()?;
        let body = std::fs::read_to_string(self.dir.join(script.file)).ok()?;
        (script.delegate)(&body)
    }

    fn read_settings(&self) -> Result<Value> {
        let label = self.host.settings_label;
        match std::fs::read_to_string(self.settings_path()) {
            Ok(raw) => {
                let trimmed = raw.trim_start_matches('\u{feff}');
                serde_json::from_str(trimmed)
                    .with_context(|| tr!("{label} 不是有效 JSON", "{label} is not valid JSON"))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
            Err(error) => {
                Err(error).with_context(|| tr!("无法读取 {label}", "Couldn't read {label}"))
            }
        }
    }

    fn write_settings(&self, settings: &Value) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        // 写到符号链接的目标上：dotfiles 仓库管理的 settings.json 不能被换成普通文件。
        let path = std::fs::canonicalize(self.settings_path()).unwrap_or(self.settings_path());
        let label = self.host.settings_label;
        write_atomically(&path, serde_json::to_string_pretty(settings)?.as_bytes())
            .with_context(|| tr!("无法更新 {label}", "Couldn't update {label}"))
    }

    fn status_line_is_ours(&self, settings: &Value) -> bool {
        let Some(command) = settings
            .get("statusLine")
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str)
        else {
            return false;
        };
        if let Some(script) = &self.host.legacy_script {
            if command.contains(script.file) {
                return true;
            }
        }
        if self
            .hook_command()
            .is_ok_and(|expected| command == expected)
        {
            return true;
        }
        is_moved_metrik_command(command, self.host.flag)
    }

    /// 已存在、且不是可直接覆盖的空骨架的 statusLine。
    fn configured_status_line<'a>(&self, settings: &'a Value) -> Option<&'a Value> {
        settings.get("statusLine").filter(|value| {
            if value.is_null() {
                return false;
            }
            let empty_command = value
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| command.trim().is_empty());
            // 空骨架只在宿主声明时才算「未配置」。
            if self.host.empty_command_is_unconfigured {
                !empty_command
            } else {
                true
            }
        })
    }

    pub fn status(&self) -> Result<HookStatus> {
        let settings = self.read_settings()?;
        let installed = self.status_line_is_ours(&settings);
        // 只有存在且无法串联（缺 command 字段）的 statusLine 才算冲突。
        let conflict = !installed
            && self.configured_status_line(&settings).is_some()
            && foreign_command(&settings).is_none();
        let chained = installed
            && (self.read_backup().is_some()
                || self
                    .installed_delegate()
                    .is_some_and(|value| !value.is_empty()));
        let last_data_at_ms =
            read_json_file::<ReceivedAt>(&self.quota_path()).map(|file| file.received_at_ms);
        let now_ms = chrono::Utc::now().timestamp_millis();
        Ok(HookStatus {
            installed,
            conflict,
            chained,
            replaced: !installed && self.read_metadata().is_some(),
            last_data_at_ms,
            stale: last_data_at_ms
                .is_some_and(|received| now_ms.saturating_sub(received) > MAX_SNAPSHOT_AGE_MS),
        })
    }

    pub fn install(&self) -> Result<HookStatus> {
        let mut settings = self.read_settings()?;
        std::fs::create_dir_all(&self.dir)?;

        // 已有他人 statusLine：备份原设置并串联其 command；重装时沿用已备份的命令。
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
        } else if let Some(existing) = self.configured_status_line(&settings).cloned() {
            let Some(command) = foreign_command(&settings) else {
                let host = self.host.name;
                bail!(tr!(
                    "{host} 已配置无法串联的 statusLine（缺少 command 字段），为避免覆盖，未安装。",
                    "{host} already has a statusLine that can't be chained (no command field). Not installed, to avoid overwriting it."
                ));
            };
            std::fs::write(self.backup_path(), serde_json::to_string_pretty(&existing)?)
                .with_context(|| {
                    tr!(
                        "无法备份原有 statusLine 设置",
                        "Couldn't back up the existing statusLine setting"
                    )
                })?;
            delegate = command;
        }

        self.write_metadata(&self.expected_metadata(delegate)?)?;

        let root = settings.as_object_mut().with_context(|| {
            tr!(
                "settings.json 顶层不是对象",
                "The top level of settings.json is not an object"
            )
        })?;
        root.insert(
            "statusLine".into(),
            (self.host.status_line)(&self.hook_command()?),
        );
        self.write_settings(&settings)?;
        // 原生入口不再需要委托脚本：删掉升级前留下的旧脚本。
        if let Some(path) = self.legacy_script_path() {
            let _ = std::fs::remove_file(path);
        }
        self.status()
    }

    /// 启动时自愈：statusLine 已经属于 Metrik，但命令过时（应用移动、旧脚本
    /// 迁移）或元数据丢失时重装一次。
    ///
    /// `install()` 只有界面上那个开关会调，升级和开机都不会；旧版本写坏的
    /// statusLine 升级后依然是坏的，界面却显示「已安装」，所以启动时回头补一次。
    /// statusLine 不属于 Metrik 时一律不碰；已经健康时不写盘。
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
            && !self.legacy_script_path().is_some_and(|path| path.exists())
        {
            return Ok(false);
        }
        self.install()?;
        Ok(true)
    }

    pub fn uninstall(&self) -> Result<HookStatus> {
        let mut settings = self.read_settings()?;
        if self.status_line_is_ours(&settings) {
            let root = settings.as_object_mut().with_context(|| {
                tr!(
                    "settings.json 顶层不是对象",
                    "The top level of settings.json is not an object"
                )
            })?;
            // 串联安装的：把用户原有的 statusLine 原样恢复。备份被删时退而求其次，
            // 用委托原文重建——宁可丢次要字段，也不能把用户的 statusLine 整个删掉。
            let restored = self.read_backup().or_else(|| {
                let delegate = self.installed_delegate().filter(|d| !d.is_empty())?;
                Some((self.host.status_line)(&delegate))
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
        if let Some(path) = self.legacy_script_path() {
            let _ = std::fs::remove_file(path);
        }
        let _ = std::fs::remove_file(self.metadata_path());
        let _ = std::fs::remove_file(self.quota_path());
        let _ = std::fs::remove_file(self.backup_path());
        self.status()
    }
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

/// 钩子入口：读完 stdin，把渲染出的状态栏文本写到 stdout。
pub(crate) fn run(render: impl FnOnce(&[u8]) -> String) {
    use std::io::{Read, Write};

    let mut input = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut input);
    let output = render(&input);
    if !output.is_empty() {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(output.as_bytes());
        let _ = stdout.write_all(b"\n");
    }
}

/// 会话 JSON 里的模型显示名，缺失时用宿主默认名。
pub(crate) fn model_name<'a>(data: Option<&'a Value>, fallback: &'a str) -> &'a str {
    data.and_then(|value| value.get("model"))
        .and_then(|value| value.get("display_name"))
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .unwrap_or(fallback)
}

/// 落盘额度快照，再渲染状态栏：有串联命令时执行它，单行输出在行尾追加额度，
/// 多行输出原样透传（多行 statusLine 自带排版）；没有串联时显示模型名与额度。
pub(crate) fn render(
    metadata: Option<&StatusLineMetadata>,
    input: &[u8],
    payload: Option<Vec<u8>>,
    quota_text: &str,
    model: &str,
    delegate_timeout: std::time::Duration,
) -> String {
    if let Some(metadata) = metadata {
        if let Some(bytes) = payload {
            let _ = write_atomically(&metadata.quota_path, &bytes);
        }
        let stale_before = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(5 * 60))
            .unwrap_or(std::time::UNIX_EPOCH);
        if let (Some(directory), Some(name)) = (
            metadata.quota_path.parent(),
            metadata.quota_path.file_name(),
        ) {
            let prefix = format!("{}.tmp-", name.to_string_lossy());
            sweep_stale_files(directory, &prefix, stale_before);
        }
    }

    if let Some(delegate) = metadata
        .map(|metadata| metadata.delegate.trim())
        .filter(|delegate| !delegate.is_empty())
    {
        let delegated = run_delegate(delegate, input, delegate_timeout);
        if delegated.is_empty() {
            return quota_text.to_owned();
        }
        if !delegated.contains('\r') && !delegated.contains('\n') && !quota_text.is_empty() {
            return format!("{delegated} | {quota_text}");
        }
        return delegated;
    }

    if quota_text.is_empty() {
        model.to_owned()
    } else {
        format!("{model} | {quota_text}")
    }
}

/// 读取钩子自己的 JSON 文件（额度快照、元数据、备份）；缺失或损坏返回 None。
pub(crate) fn read_json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()
}

/// 先写同目录的 `<文件名>.tmp-<pid>` 再改名，读者永远看不到半个文件。
/// 被终止的调用留下的暂存文件由 `sweep_stale_files` 按这个前缀清理。
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut staged = path.as_os_str().to_owned();
    staged.push(format!(".tmp-{}", std::process::id()));
    let staged = PathBuf::from(staged);
    std::fs::write(&staged, bytes)?;
    let installed = std::fs::rename(&staged, path);
    if installed.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    installed
}

/// 应用被移动/重装后绝对路径会变：命令仍以钩子旗标结尾且第一个 token 的
/// 文件名是 metrik 可执行文件时，认作我们的，交给自愈改写。
pub(crate) fn is_moved_metrik_command(command: &str, flag: &str) -> bool {
    command.trim_end().ends_with(flag)
        && first_command_token(command).is_some_and(|executable| {
            Path::new(executable)
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| {
                    name.eq_ignore_ascii_case("metrik") || name.eq_ignore_ascii_case("metrik.exe")
                })
        })
}

pub(crate) fn sweep_stale_files(
    directory: &Path,
    prefix: &str,
    stale_before: std::time::SystemTime,
) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(prefix) {
            continue;
        }
        let is_stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified < stale_before);
        if is_stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// statusLine 命令的第一个 token（可执行文件路径），支持 `"..."`（Windows）和
/// `'...'`（Unix）两种引用，以及裸路径。用于识别命令是否指向 metrik 本体。
pub(crate) fn first_command_token(command: &str) -> Option<&str> {
    let command = command.trim();
    if let Some(rest) = command.strip_prefix('"') {
        return rest.split('"').next();
    }
    if let Some(rest) = command.strip_prefix('\'') {
        return rest.split('\'').next();
    }
    let end = command.find(char::is_whitespace).unwrap_or(command.len());
    Some(&command[..end])
}

/// 用单引号包住路径供 `/bin/sh -c` 安全解析：内部单引号按 `'\''` 转义。
#[cfg(not(windows))]
pub(crate) fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(windows)]
pub(crate) fn command_has_shell_syntax(command: &str) -> bool {
    let mut quoted = false;
    command.chars().any(|character| match character {
        '"' => {
            quoted = !quoted;
            false
        }
        '|' | '&' | ';' | '<' | '>' | '(' | ')' if !quoted => true,
        _ => false,
    })
}

#[cfg(windows)]
pub(crate) fn split_delegate_command(command: &str) -> Option<(&str, &str)> {
    let command = command.trim();
    if let Some(quoted) = command.strip_prefix('"') {
        let end = quoted.find('"')?;
        return Some((&quoted[..end], quoted[end + 1..].trim_start()));
    }
    let end = command.find(char::is_whitespace).unwrap_or(command.len());
    Some((&command[..end], command[end..].trim_start()))
}

#[cfg(windows)]
pub(crate) fn delegate_process(delegate: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;

    let direct = split_delegate_command(delegate);
    // 直接启动只认 .exe：`npx`、`ccstatusline` 这类 npm 包装是 .cmd，
    // 只有 cmd.exe 会按 PATHEXT 解析，所以除显式 .exe/.com 外都交给 cmd。
    let needs_shell = command_has_shell_syntax(delegate)
        || direct.is_none_or(|(executable, _)| {
            !matches!(
                Path::new(executable)
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(str::to_ascii_lowercase)
                    .as_deref(),
                Some("exe" | "com")
            )
        });
    let mut command = if needs_shell {
        let comspec = std::env::var_os("COMSPEC")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let system_root =
                    std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
                PathBuf::from(system_root).join("System32").join("cmd.exe")
            });
        let mut command = std::process::Command::new(comspec);
        command.args(["/D", "/S", "/C"]).raw_arg(delegate);
        command
    } else if let Some((executable, arguments)) = direct {
        let mut command = std::process::Command::new(executable);
        if !arguments.is_empty() {
            command.raw_arg(arguments);
        }
        command
    } else {
        std::process::Command::new(delegate)
    };
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    command
}

/// 委托原文交给 `/bin/sh -c` 执行——与旧 Python 钩子的 `shell=True` 语义一致，
/// 管道 `|`、`&&`、引号等都由 shell 解释。子进程放进自己的进程组，超时后按组
/// 终止（`killpg`），对齐 Windows 的进程树终止：委托再拉起后代也不会残留。
#[cfg(unix)]
pub(crate) fn delegate_process(delegate: &str) -> std::process::Command {
    use std::os::unix::process::CommandExt;

    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(delegate)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        // 独立进程组（pgid == 子进程 pid），超时后能连带后代一起收掉。
        .process_group(0);
    command
}

#[cfg(unix)]
fn terminate_process_group(pgid: i32) {
    // 负 pid 语义由 killpg 表达：终止整组，包括委托拉起的后代。
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

pub(crate) fn run_delegate(delegate: &str, input: &[u8], timeout: std::time::Duration) -> String {
    use std::io::{Read, Write};

    let Ok(mut child) = crate::child_process::spawn(
        crate::child_process::Site::ClaudeStatuslineDelegate,
        &mut delegate_process(delegate),
    ) else {
        return String::new();
    };
    // process_group(0) 让 pgid 等于子进程 pid。
    #[cfg(unix)]
    let pgid = child.id() as i32;
    let output = child.stdout.take();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut output) = output {
            let _ = output.read_to_end(&mut bytes);
        }
        let _ = sender.send(bytes);
    });
    // 写入放到线程里：不读 stdin 的委托写满管道缓冲时，超时判断不能被卡住。
    if let Some(mut stdin) = child.stdin.take() {
        let input = input.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                #[cfg(windows)]
                crate::child_process::terminate_process_tree(&mut child);
                #[cfg(unix)]
                {
                    terminate_process_group(pgid);
                    let _ = child.wait();
                }
                break;
            }
        }
    }
    // 委托退出后若有后代仍占着 stdout，读线程会一直等 EOF；到期就放弃输出。
    let grace = deadline
        .saturating_duration_since(std::time::Instant::now())
        .max(std::time::Duration::from_millis(200));
    let bytes = receiver.recv_timeout(grace).unwrap_or_default();
    String::from_utf8_lossy(&bytes)
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}
