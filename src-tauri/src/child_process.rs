//! 本机子进程的唯一出口。
//!
//! Metrik 常驻托盘，周期性路径上的一次 spawn 会被刷新节奏放大：#186 里每轮
//! 快照一个 PowerShell（实测每 5.8 秒一个），#210 里每次额度探测遗留一个约
//! 300 MB 的 git 克隆。两次都是用户抓进程才发现的，所以这里把子进程变成可数的
//! 东西：
//!
//! - 生产代码只能经本模块拉起子进程，每个调用点登记为一个 [`Site`]。源码扫描
//!   测试禁止绕过本模块，新增调用点必须在 `Site` 里登记，评审一眼能看到。
//! - 每次拉起按站点计数（按线程计，测试互不干扰）。`spawn_budget` 测试用它断言
//!   稳态刷新不拉起任何子进程。
//! - Windows 会话正在结束时拒绝拉起：关机过程中新启动的控制台进程会以
//!   0xc0000142 初始化失败并弹窗。
//! - 需要连同后代一起收掉的进程（[`spawn_tree`]）在 Windows 上放进一个
//!   关闭即杀的作业对象：直接子进程先退出、进程树断开时，后代也逃不掉。

use std::cell::Cell;
use std::io;
use std::process::{Child, Command, ExitStatus, Output};

/// 生产代码里每一个拉起子进程的调用点。新增一项前先确认它不在周期性刷新路径上，
/// 或者有跨快照的节流（参见 `spawn_budget` 测试）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Site {
    /// Codex 额度探测：短命的 `codex app-server`，按 quota 注册表的节奏。
    CodexAppServer,
    /// Windows 作业对象不可用时的进程树兜底终止（`taskkill /T`）。
    #[cfg_attr(not(windows), allow(dead_code))]
    ProcessTreeKill,
    /// Antigravity 端点发现：列进程（macOS/Linux `ps`；Windows 走原生 API）。
    #[cfg_attr(windows, allow(dead_code))]
    AntigravityProcessScan,
    /// Antigravity 端点发现：查监听端口（macOS/Linux `lsof`；Windows 走原生 API）。
    #[cfg_attr(windows, allow(dead_code))]
    AntigravityPortScan,
    /// Claude statusLine 钩子转调用户原有的状态栏命令（在钩子进程里，不在主程序里）。
    ClaudeStatuslineDelegate,
    /// macOS 钥匙串读取 Claude OAuth 凭据（仅在用户开启 OAuth 备选后）。
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    MacosKeychain,
    /// macOS WidgetKit 快照发布与刷新 helper。
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    MacosWidgetHelper,
}

impl Site {
    const COUNT: usize = 7;

    fn index(self) -> usize {
        self as usize
    }
}

thread_local! {
    static SPAWNED: Cell<[u32; Site::COUNT]> = const { Cell::new([0; Site::COUNT]) };
}

/// 当前线程上各站点累计拉起的次数。
#[cfg(test)]
pub(crate) fn spawned_on_this_thread() -> Vec<(Site, u32)> {
    const ALL: [Site; Site::COUNT] = [
        Site::CodexAppServer,
        Site::ProcessTreeKill,
        Site::AntigravityProcessScan,
        Site::AntigravityPortScan,
        Site::ClaudeStatuslineDelegate,
        Site::MacosKeychain,
        Site::MacosWidgetHelper,
    ];
    let counts = SPAWNED.with(Cell::get);
    ALL.into_iter()
        .map(|site| (site, counts[site.index()]))
        .collect()
}

fn admit(site: Site, command: &mut Command) -> io::Result<()> {
    if session_ending() {
        return Err(io::Error::other("the Windows session is ending"));
    }
    SPAWNED.with(|cell| {
        let mut counts = cell.get();
        counts[site.index()] = counts[site.index()].saturating_add(1);
        cell.set(counts);
    });
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：常驻进程拉起的控制台程序不得闪出黑框。
        command.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = command;
    Ok(())
}

#[cfg(windows)]
fn session_ending() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SHUTTINGDOWN};
    // SAFETY: GetSystemMetrics 只读系统状态，无前置条件。
    unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) != 0 }
}

#[cfg(not(windows))]
fn session_ending() -> bool {
    false
}

pub(crate) fn spawn(site: Site, command: &mut Command) -> io::Result<Child> {
    admit(site, command)?;
    command.spawn()
}

#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn output(site: Site, command: &mut Command) -> io::Result<Output> {
    admit(site, command)?;
    command.output()
}

pub(crate) fn status(site: Site, command: &mut Command) -> io::Result<ExitStatus> {
    admit(site, command)?;
    command.status()
}

/// 一个连同全部后代一起管理的子进程。Drop 时终止整棵树。
pub(crate) struct TreeChild {
    child: Option<Child>,
    #[cfg(windows)]
    job: Option<job::KillOnCloseJob>,
}

/// 拉起一个需要连同后代一起收掉的子进程。Windows 上立即放进关闭即杀的作业对象，
/// 之后派生的后代自动继承作业；放不进去（极少见）时终止回落到 `taskkill /T`。
/// 放入作业之前的几毫秒内派生的后代不在作业里，同样由回落路径覆盖不到——
/// `cmd.exe` 与 Node 启动都远慢于此，实际不会发生。
pub(crate) fn spawn_tree(site: Site, command: &mut Command) -> io::Result<TreeChild> {
    let child = spawn(site, command)?;
    #[cfg(windows)]
    let job = job::KillOnCloseJob::assign(&child);
    Ok(TreeChild {
        child: Some(child),
        #[cfg(windows)]
        job,
    })
}

impl TreeChild {
    pub(crate) fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("tree child already terminated")
    }

    pub(crate) fn terminate(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        #[cfg(windows)]
        if let Some(job) = self.job.take() {
            job.terminate();
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        terminate_process_tree(&mut child);
    }
}

impl Drop for TreeChild {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// 终止一个不在作业对象里的子进程及其后代。Windows 用 `taskkill /T`，这要求
/// 进程树仍然完整（直接子进程还活着）；其余平台只收直接子进程，需要连带后代的
/// 调用方自行安排进程组。
pub(crate) fn terminate_process_tree(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }

    #[cfg(windows)]
    {
        let mut taskkill = Command::new("taskkill.exe");
        taskkill
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let _ = status(Site::ProcessTreeKill, &mut taskkill);
    }

    // 跨平台兜底，也负责在 Windows 杀掉后代之后回收直接子进程。
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub(super) struct KillOnCloseJob(HANDLE);

    // SAFETY: 作业对象句柄是内核对象句柄，可在线程间移动。
    unsafe impl Send for KillOnCloseJob {}

    impl KillOnCloseJob {
        pub(super) fn assign(child: &Child) -> Option<Self> {
            // SAFETY: 参数都是本函数内构造的有效值；句柄失败路径都在这里关闭。
            unsafe {
                let handle = CreateJobObjectW(None, windows::core::PCWSTR::null()).ok()?;
                let job = Self(handle);
                let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&limits).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
                .ok()?;
                AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())).ok()?;
                Some(job)
            }
        }

        pub(super) fn terminate(self) {
            // SAFETY: 句柄由 assign 创建且仍然有效；Drop 随后关闭它。
            unsafe {
                let _ = TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for KillOnCloseJob {
        fn drop(&mut self) {
            // SAFETY: 句柄只在这里关闭一次。关闭即触发 KILL_ON_JOB_CLOSE。
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生产代码不得绕过本模块直接拉起子进程。扫描范围是 `src/` 下每个文件在
    /// 顶层测试模块之前的部分（测试夹具可以直接用 `Command`）。
    #[test]
    fn production_code_spawns_only_through_this_module() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut pending = vec![root.clone()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().and_then(|ext| ext.to_str()) != Some("rs")
                    || path.ends_with("child_process.rs")
                {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                let production = source
                    .split("\n#[cfg(test)]\nmod ")
                    .next()
                    .unwrap_or_default();
                // 只看用到 `Command` 的文件；`self.status()` 是各钩子自己的状态查询。
                if !production.contains("Command") {
                    continue;
                }
                for (line_number, line) in production.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or_default();
                    if [".spawn()", ".output()", ".status()"]
                        .iter()
                        .any(|call| code.contains(call) && !code.contains(&format!("self{call}")))
                    {
                        offenders.push(format!(
                            "{}:{}: {}",
                            path.strip_prefix(&root).unwrap().display(),
                            line_number + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "spawn through child_process::{{spawn, output, status, spawn_tree}} and register a Site:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn counts_are_per_site_and_per_thread() {
        let before = spawned_on_this_thread();
        let mut command = Command::new(if cfg!(windows) { "cmd.exe" } else { "true" });
        if cfg!(windows) {
            command.args(["/D", "/C", "exit 0"]);
        }
        let _ = status(Site::ProcessTreeKill, &mut command);
        let after = spawned_on_this_thread();
        for ((site, was), (_, now)) in before.iter().zip(&after) {
            let expected = was + u32::from(*site == Site::ProcessTreeKill);
            assert_eq!(*now, expected, "{site:?}");
        }
        let other = std::thread::spawn(spawned_on_this_thread).join().unwrap();
        assert!(other.iter().all(|(_, count)| *count == 0));
    }

    /// 作业对象的价值所在：直接子进程先退出、进程树已经断开，后代仍被收掉。
    /// 这正是 #210 的形状——app-server 读到 EOF 自行退出，`taskkill /T` 找不到
    /// 它派生的 git。
    #[cfg(windows)]
    #[test]
    fn job_reaps_descendants_after_the_direct_child_has_exited() {
        let marker = std::env::temp_dir().join(format!(
            "metrik-job-tree-{}-{}.txt",
            std::process::id(),
            chrono::Utc::now().timestamp_millis()
        ));
        let escaped_marker = marker.to_string_lossy().replace('\'', "''");
        let script = format!(
            "$child = Start-Process -FilePath \"$env:SystemRoot\\System32\\PING.EXE\" \
             -ArgumentList '-n','60','127.0.0.1' -WindowStyle Hidden -PassThru; \
             $child.Id | Set-Content -LiteralPath '{escaped_marker}' -Encoding ascii"
        );
        let mut command = Command::new("powershell.exe");
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &script,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut tree = spawn_tree(Site::CodexAppServer, &mut command).unwrap();
        let exited = tree.child_mut().wait().unwrap();
        assert!(exited.success(), "fixture PowerShell failed: {exited}");
        let descendant_pid = std::fs::read_to_string(&marker)
            .ok()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .expect("fixture recorded its descendant before exiting");
        assert!(
            process_alive(descendant_pid),
            "descendant should outlive its parent until the job is closed"
        );

        tree.terminate();

        let survived = process_alive(descendant_pid);
        if survived {
            let mut kill = Command::new("taskkill.exe");
            kill.args(["/PID", &descendant_pid.to_string(), "/F"]);
            let _ = kill.output();
        }
        let _ = std::fs::remove_file(marker);
        assert!(!survived, "descendant {descendant_pid} escaped the job");
    }

    #[cfg(windows)]
    fn process_alive(pid: u32) -> bool {
        use std::os::windows::process::CommandExt;
        let filter = format!("PID eq {pid}");
        let output = Command::new("tasklist.exe")
            .args(["/FI", &filter, "/FO", "CSV", "/NH"])
            .creation_flags(0x0800_0000)
            .output()
            .expect("tasklist should inspect the process");
        String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
    }
}
