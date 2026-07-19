mod adapters;
mod app_server;
mod claude_hook;
mod claude_oauth;
mod coding_quota;
mod domain;
mod engine;
#[cfg(target_os = "macos")]
mod macos;
mod pricing;
mod schema;
mod storage;
mod sync;

use anyhow::{Context, Result};
use domain::{QuotaSample, UsageReport, UsageSessions, UsageSnapshot};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{Manager, State};

type SharedQuotaCache = Arc<Mutex<Option<(Instant, Vec<QuotaSample>)>>>;
/// 走网络的官方配额（GLM/Kimi）按 adapter 分桶缓存，跨快照持有以限流。
type SharedHttpQuotaCache = Arc<Mutex<HashMap<&'static str, (Instant, Vec<QuotaSample>)>>>;

const DATABASE_FILE_NAME: &str = "metrik.sqlite3";
const RECOVERY_DATABASE_FILE_NAME: &str = "metrik.recovery.sqlite3";
const SQLITE_SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];
static MIGRATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct AppState {
    database_path: PathBuf,
    scan_gate: Arc<Mutex<()>>,
    quota_cache: SharedQuotaCache,
    claude_quota_cache: SharedQuotaCache,
    http_quota_cache: SharedHttpQuotaCache,
}

fn sqlite_sidecar_path(database_path: &Path, suffix: &str) -> PathBuf {
    let mut path = database_path.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn staging_database_path(local_database: &Path) -> Result<PathBuf> {
    let parent = local_database
        .parent()
        .context("local database path has no parent directory")?;
    let file_name = local_database
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(DATABASE_FILE_NAME);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = MIGRATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(
        ".{file_name}.migration-{}-{timestamp}-{sequence}",
        std::process::id()
    )))
}

fn emergency_database_path() -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = MIGRATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "metrik.session.{}.{}.{}.sqlite3",
        std::process::id(),
        timestamp,
        sequence
    ))
}

fn cleanup_staged_database(staged_database: &Path) {
    let _ = fs::remove_file(staged_database);
    for suffix in SQLITE_SIDECAR_SUFFIXES {
        let _ = fs::remove_file(sqlite_sidecar_path(staged_database, suffix));
    }
}

fn sqlite_sidecar_exists(database_path: &Path) -> Result<bool> {
    for suffix in SQLITE_SIDECAR_SUFFIXES {
        let sidecar = sqlite_sidecar_path(database_path, suffix);
        if sidecar
            .try_exists()
            .with_context(|| format!("failed to inspect SQLite sidecar {}", sidecar.display()))?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn recovery_database_path(local_database: &Path, ordinal: u64) -> Result<PathBuf> {
    let parent = local_database
        .parent()
        .context("local database path has no parent directory")?;
    if ordinal == 1 {
        Ok(parent.join(RECOVERY_DATABASE_FILE_NAME))
    } else {
        Ok(parent.join(format!("metrik.recovery-{ordinal}.sqlite3")))
    }
}

fn select_recovery_database(local_database: &Path) -> Result<PathBuf> {
    let parent = local_database
        .parent()
        .context("local database path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create local app data directory {}",
            parent.display()
        )
    })?;

    let mut ordinal = 1_u64;
    loop {
        let candidate = recovery_database_path(local_database, ordinal)?;
        if candidate.try_exists().with_context(|| {
            format!(
                "failed to inspect recovery database {}",
                candidate.display()
            )
        })? {
            return Ok(candidate);
        }

        // A sidecar without its matching main file may belong to a crashed or
        // interrupted database. Never let SQLite attach it to a fresh file.
        if sqlite_sidecar_exists(&candidate)? {
            ordinal = ordinal
                .checked_add(1)
                .context("exhausted recovery database names")?;
            continue;
        }

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                drop(file);
                if sqlite_sidecar_exists(&candidate)? {
                    // This empty main file was created by this attempt. Remove
                    // it rather than risk pairing it with a racing orphan.
                    fs::remove_file(&candidate).with_context(|| {
                        format!(
                            "failed to discard conflicted recovery database {}",
                            candidate.display()
                        )
                    })?;
                    ordinal = ordinal
                        .checked_add(1)
                        .context("exhausted recovery database names")?;
                    continue;
                }
                return Ok(candidate);
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                // Another instance reserved the same deterministic recovery
                // file after our check. Reusing that main file is safe.
                return Ok(candidate);
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to reserve recovery database {}",
                        candidate.display()
                    )
                });
            }
        }
    }
}

fn migrate_legacy_database(legacy_database: &Path, local_database: &Path) -> Result<()> {
    if legacy_database == local_database || local_database.try_exists()? {
        return Ok(());
    }
    if !legacy_database.try_exists()? {
        return Ok(());
    }

    let local_parent = local_database
        .parent()
        .context("local database path has no parent directory")?;
    fs::create_dir_all(local_parent).with_context(|| {
        format!(
            "failed to create local app data directory {}",
            local_parent.display()
        )
    })?;

    // Recheck after creating the directory so a concurrently-created local
    // database always wins over the legacy copy.
    if local_database.try_exists()? {
        return Ok(());
    }

    let staged_database = staging_database_path(local_database)?;
    let migration_result = (|| -> Result<()> {
        fs::copy(legacy_database, &staged_database).with_context(|| {
            format!(
                "failed to stage legacy database {}",
                legacy_database.display()
            )
        })?;

        let mut copied_sidecars = Vec::new();
        for suffix in SQLITE_SIDECAR_SUFFIXES {
            let legacy_sidecar = sqlite_sidecar_path(legacy_database, suffix);
            if legacy_sidecar.try_exists()? {
                let staged_sidecar = sqlite_sidecar_path(&staged_database, suffix);
                fs::copy(&legacy_sidecar, &staged_sidecar).with_context(|| {
                    format!(
                        "failed to stage legacy SQLite sidecar {}",
                        legacy_sidecar.display()
                    )
                })?;
                copied_sidecars.push(suffix);
            }
        }

        if local_database.try_exists()? {
            return Ok(());
        }

        // Install sidecars first and the main database last. Hard links provide
        // create-if-absent semantics on every supported desktop platform, so a
        // newer local database or sidecar can never be overwritten.
        let mut installed_sidecars = Vec::new();
        for suffix in SQLITE_SIDECAR_SUFFIXES {
            let local_sidecar = sqlite_sidecar_path(local_database, suffix);
            if local_sidecar.try_exists()? {
                anyhow::bail!(
                    "refusing to overwrite local SQLite sidecar {}",
                    local_sidecar.display()
                );
            }
        }

        let install_result = (|| -> Result<()> {
            for suffix in copied_sidecars {
                let staged_sidecar = sqlite_sidecar_path(&staged_database, suffix);
                let local_sidecar = sqlite_sidecar_path(local_database, suffix);
                fs::hard_link(&staged_sidecar, &local_sidecar).with_context(|| {
                    format!(
                        "refusing to overwrite local SQLite sidecar {}",
                        local_sidecar.display()
                    )
                })?;
                installed_sidecars.push(local_sidecar);
            }

            fs::hard_link(&staged_database, local_database).with_context(|| {
                format!(
                    "refusing to overwrite local database {}",
                    local_database.display()
                )
            })?;
            Ok(())
        })();

        if install_result.is_err() {
            // Only remove sidecars installed by this attempt when no other
            // process won the race and created the local database.
            if !local_database.try_exists()? {
                for sidecar in installed_sidecars {
                    let _ = fs::remove_file(sidecar);
                }
            }
        }
        install_result
    })();

    cleanup_staged_database(&staged_database);
    migration_result
}

fn resolve_database_path(legacy_database: &Path, local_database: &Path) -> Result<PathBuf> {
    match migrate_legacy_database(legacy_database, local_database) {
        Ok(()) => {
            if local_database.try_exists().with_context(|| {
                format!(
                    "failed to inspect local database {}",
                    local_database.display()
                )
            })? || !sqlite_sidecar_exists(local_database)?
            {
                return Ok(local_database.to_path_buf());
            }

            let recovery = select_recovery_database(local_database)?;
            eprintln!(
                "local database has an orphan SQLite sidecar; using recovery database {}",
                recovery.display()
            );
            Ok(recovery)
        }
        Err(migration_error) => {
            // A concurrent instance may have installed the local database
            // while this migration was staging files. Its main file wins.
            if local_database.try_exists().with_context(|| {
                format!(
                    "failed to inspect local database {} after migration failure",
                    local_database.display()
                )
            })? {
                return Ok(local_database.to_path_buf());
            }

            let recovery = select_recovery_database(local_database).with_context(|| {
                format!(
                    "legacy database migration failed ({migration_error:#}) and no recovery database could be used"
                )
            })?;
            eprintln!(
                "legacy database migration failed ({migration_error:#}); using recovery database {}",
                recovery.display()
            );
            Ok(recovery)
        }
    }
}

#[tauri::command]
async fn usage_snapshot(
    period: String,
    force: Option<bool>,
    state: State<'_, AppState>,
) -> Result<UsageSnapshot, String> {
    let database_path = state.database_path.clone();
    let scan_gate = Arc::clone(&state.scan_gate);
    let quota_cache = Arc::clone(&state.quota_cache);
    let claude_quota_cache = Arc::clone(&state.claude_quota_cache);
    let http_quota_cache = Arc::clone(&state.http_quota_cache);

    tauri::async_runtime::spawn_blocking(move || {
        let _gate = scan_gate
            .lock()
            .map_err(|_| "usage scan lock poisoned".to_owned())?;
        engine::build_snapshot(
            &database_path,
            &period,
            &quota_cache,
            &claude_quota_cache,
            &http_quota_cache,
            force.unwrap_or(false),
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("usage scan task failed: {error}"))?
}

/// 只读历史报告：只查询本地账本已有数据，绝不触发日志扫描，不与 `usage_snapshot`
/// 共用扫描锁，保证报告页秒开。
#[tauri::command]
async fn usage_report(state: State<'_, AppState>) -> Result<UsageReport, String> {
    let database_path = state.database_path.clone();

    tauri::async_runtime::spawn_blocking(move || {
        engine::build_report(&database_path).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("usage report task failed: {error}"))?
}

/// 只读会话明细：只查询本地账本已有数据，绝不触发日志扫描，不占用扫描锁。
#[tauri::command]
async fn usage_sessions(
    period: String,
    state: State<'_, AppState>,
) -> Result<UsageSessions, String> {
    let database_path = state.database_path.clone();

    tauri::async_runtime::spawn_blocking(move || {
        engine::build_sessions(&database_path, &period).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("usage sessions task failed: {error}"))?
}

/// 把前端拼好的 CSV 文本写入「下载」目录并返回完整路径。WebView 里的
/// blob 下载在 Tauri 下不会触发，所以导出必须走这条本地写入通道。
/// 内容由前端生成，只含账本统计字段，不含对话正文。
#[tauri::command]
async fn export_csv(file_name: String, content: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let safe_name: String = file_name
            .chars()
            .map(|c| match c {
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
                other => other,
            })
            .collect();
        let directory = dirs::download_dir()
            .or_else(dirs::home_dir)
            .ok_or_else(|| "无法定位下载目录".to_owned())?;
        let mut target = directory.join(&safe_name);
        let mut counter = 1;
        while target.exists() {
            let stem = safe_name.trim_end_matches(".csv");
            target = directory.join(format!("{stem}-{counter}.csv"));
            counter += 1;
        }
        std::fs::write(&target, content.as_bytes())
            .map_err(|error| format!("写入 CSV 失败: {error}"))?;
        Ok(target.to_string_lossy().into_owned())
    })
    .await
    .map_err(|error| format!("csv export task failed: {error}"))?
}

#[tauri::command]
async fn rebuild_local_ledger(
    period: String,
    state: State<'_, AppState>,
) -> Result<UsageSnapshot, String> {
    let database_path = state.database_path.clone();
    let scan_gate = Arc::clone(&state.scan_gate);
    let quota_cache = Arc::clone(&state.quota_cache);
    let claude_quota_cache = Arc::clone(&state.claude_quota_cache);
    let http_quota_cache = Arc::clone(&state.http_quota_cache);

    tauri::async_runtime::spawn_blocking(move || {
        let _gate = scan_gate
            .lock()
            .map_err(|_| "usage scan lock poisoned".to_owned())?;
        storage::reset_derived_ledger(&database_path).map_err(|error| error.to_string())?;
        engine::build_snapshot(
            &database_path,
            &period,
            &quota_cache,
            &claude_quota_cache,
            &http_quota_cache,
            false,
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("local ledger rebuild task failed: {error}"))?
}

/// 只读状态：开关是否开启、本机是否有 Claude 登录凭据、scope 是否满足。
/// 永不向前端返回 token 内容。
#[tauri::command]
async fn claude_oauth_status(
    state: State<'_, AppState>,
) -> Result<claude_oauth::ClaudeOauthStatus, String> {
    let database_path = state.database_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let connection =
            storage::open_database_read_only(&database_path).map_err(|error| error.to_string())?;
        let enabled = storage::get_app_setting(&connection, claude_oauth::SETTING_KEY)
            .map_err(|error| error.to_string())?
            .as_deref()
            == Some("1");
        Ok(claude_oauth::ClaudeOauth::detected().status(enabled))
    })
    .await
    .map_err(|error| format!("claude oauth status task failed: {error}"))?
}

#[tauri::command]
async fn set_claude_oauth(
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<claude_oauth::ClaudeOauthStatus, String> {
    let database_path = state.database_path.clone();
    let scan_gate = Arc::clone(&state.scan_gate);
    let claude_quota_cache = Arc::clone(&state.claude_quota_cache);
    tauri::async_runtime::spawn_blocking(move || {
        let _gate = scan_gate
            .lock()
            .map_err(|_| "usage scan lock poisoned".to_owned())?;
        let connection =
            storage::open_database(&database_path).map_err(|error| error.to_string())?;
        storage::set_app_setting(
            &connection,
            claude_oauth::SETTING_KEY,
            if enabled { "1" } else { "0" },
        )
        .map_err(|error| error.to_string())?;
        if !enabled {
            // 关闭后清掉 OAuth 来源的展示行，下次扫描由钩子文件重新填充。
            connection
                .execute(
                    "DELETE FROM quota_snapshot WHERE adapter_id = 'claude' AND source_label = ?1",
                    [claude_oauth::SOURCE_LABEL],
                )
                .map_err(|error| error.to_string())?;
        }
        // 清缓存让下一次快照立即按新开关取数。
        if let Ok(mut guard) = claude_quota_cache.lock() {
            *guard = None;
        }
        Ok(claude_oauth::ClaudeOauth::detected().status(enabled))
    })
    .await
    .map_err(|error| format!("set claude oauth task failed: {error}"))?
}

#[tauri::command]
async fn sync_settings(state: State<'_, AppState>) -> Result<domain::SyncView, String> {
    let database_path = state.database_path.clone();
    let scan_gate = Arc::clone(&state.scan_gate);

    tauri::async_runtime::spawn_blocking(move || {
        let _gate = scan_gate
            .lock()
            .map_err(|_| "usage scan lock poisoned".to_owned())?;
        let connection =
            storage::open_database(&database_path).map_err(|error| error.to_string())?;
        sync::sync_view(&connection).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("sync settings task failed: {error}"))?
}

#[tauri::command]
async fn configure_sync(
    directory: Option<String>,
    state: State<'_, AppState>,
) -> Result<domain::SyncView, String> {
    let database_path = state.database_path.clone();
    let scan_gate = Arc::clone(&state.scan_gate);

    tauri::async_runtime::spawn_blocking(move || {
        let _gate = scan_gate
            .lock()
            .map_err(|_| "usage scan lock poisoned".to_owned())?;
        let mut connection =
            storage::open_database(&database_path).map_err(|error| error.to_string())?;
        sync::configure(&mut connection, directory).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("sync configuration task failed: {error}"))?
}

/// Windows 的 SWCA Acrylic：与 Win11 的 DWM Acrylic 不同，它接受自定义
/// tint 颜色，磨砂更通透、可控（CodexBar 式亮玻璃在 Windows 上的对应物）。
#[cfg(windows)]
mod swca {
    use core::ffi::c_void;

    #[repr(C)]
    struct AccentPolicy {
        accent_state: u32,
        accent_flags: u32,
        gradient_color: u32,
        animation_id: u32,
    }

    #[repr(C)]
    struct WindowCompositionAttribData {
        attrib: u32,
        pv_data: *mut core::ffi::c_void,
        cb_data: usize,
    }

    #[repr(C)]
    struct Margins {
        left: i32,
        right: i32,
        top: i32,
        bottom: i32,
    }

    const WCA_ACCENT_POLICY: u32 = 19;
    const ACCENT_DISABLED: u32 = 0;
    const ACCENT_ENABLE_BLURBEHIND: u32 = 3;
    const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;
    const ACCENT_ENABLE_HOSTBACKDROP: u32 = 5;
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
    const DWMSBT_NONE: u32 = 1;
    const DWMSBT_TRANSIENTWINDOW: u32 = 3;

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryA(name: *const u8) -> isize;
        fn GetProcAddress(module: isize, name: *const u8) -> *const core::ffi::c_void;
    }

    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: isize,
            attribute: u32,
            value: *const c_void,
            value_size: u32,
        ) -> i32;
        fn DwmExtendFrameIntoClientArea(hwnd: isize, margins: *const Margins) -> i32;
    }

    type SetWindowCompositionAttributeFn =
        unsafe extern "system" fn(isize, *mut WindowCompositionAttribData) -> i32;

    /// 未文档化导出，不在 user32 的导入库里，必须运行时解析。
    fn set_window_composition_attribute() -> Option<SetWindowCompositionAttributeFn> {
        unsafe {
            let module = LoadLibraryA(c"user32.dll".as_ptr().cast());
            if module == 0 {
                return None;
            }
            let proc = GetProcAddress(module, c"SetWindowCompositionAttribute".as_ptr().cast());
            if proc.is_null() {
                None
            } else {
                Some(std::mem::transmute::<
                    *const core::ffi::c_void,
                    SetWindowCompositionAttributeFn,
                >(proc))
            }
        }
    }

    fn set_dwm_attribute<T>(hwnd: isize, attribute: u32, value: &T) -> Result<(), String> {
        let result = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                attribute,
                value as *const _ as *const c_void,
                std::mem::size_of::<T>() as u32,
            )
        };
        if result >= 0 {
            Ok(())
        } else {
            Err(format!(
                "DwmSetWindowAttribute({attribute}) failed with HRESULT 0x{:08X}",
                result as u32
            ))
        }
    }

    pub fn set_dwm_acrylic(hwnd: isize, dark: bool) -> Result<(), String> {
        set_dwm_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &(dark as u32))?;
        set_dwm_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_TRANSIENTWINDOW)
    }

    /// 原生材质会填满整个方形 HWND；给窗口加 Win11 系统圆角，
    /// 让玻璃与 CSS 卡片圆角贴合，避免四角露出方形材质。
    pub fn set_round_corners(hwnd: isize, round: bool) -> Result<(), String> {
        const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
        const DWMWCP_DEFAULT: u32 = 0;
        const DWMWCP_ROUND: u32 = 2;
        let preference = if round { DWMWCP_ROUND } else { DWMWCP_DEFAULT };
        set_dwm_attribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &preference)
    }

    pub fn clear_dwm_acrylic(hwnd: isize) -> Result<(), String> {
        set_dwm_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_NONE)
    }

    pub fn extend_glass_frame(hwnd: isize, enabled: bool) -> Result<(), String> {
        let margin = if enabled { -1 } else { 0 };
        let margins = Margins {
            left: margin,
            right: margin,
            top: margin,
            bottom: margin,
        };
        let result = unsafe { DwmExtendFrameIntoClientArea(hwnd, &margins) };
        if result >= 0 {
            Ok(())
        } else {
            Err(format!(
                "DwmExtendFrameIntoClientArea failed with HRESULT 0x{:08X}",
                result as u32
            ))
        }
    }

    fn set_policy(
        hwnd: isize,
        state: u32,
        flags: u32,
        tint: Option<[u8; 4]>,
    ) -> Result<(), String> {
        let Some(set_attribute) = set_window_composition_attribute() else {
            return Err("SetWindowCompositionAttribute is unavailable".into());
        };
        let color = match tint {
            Some([r, g, b, a]) => {
                (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | ((a as u32) << 24)
            }
            None => 0,
        };
        let mut policy = AccentPolicy {
            accent_state: state,
            accent_flags: flags,
            gradient_color: color,
            animation_id: 0,
        };
        let mut data = WindowCompositionAttribData {
            attrib: WCA_ACCENT_POLICY,
            pv_data: &mut policy as *mut _ as *mut core::ffi::c_void,
            cb_data: std::mem::size_of::<AccentPolicy>(),
        };
        let result = unsafe { set_attribute(hwnd, &mut data) };
        if result == 0 {
            Err("SetWindowCompositionAttribute rejected the acrylic policy".into())
        } else {
            Ok(())
        }
    }

    pub fn set_acrylic(hwnd: isize, tint: Option<[u8; 4]>) -> Result<(), String> {
        match tint {
            // Acrylic uses no flags. Flag 2 belongs to the plain BlurBehind path.
            Some(tint) => set_policy(hwnd, ACCENT_ENABLE_ACRYLICBLURBEHIND, 0, Some(tint)),
            None => set_policy(hwnd, ACCENT_DISABLED, 0, None),
        }
    }

    pub fn set_blur(hwnd: isize, tint: [u8; 4]) -> Result<(), String> {
        set_policy(hwnd, ACCENT_ENABLE_BLURBEHIND, 2, Some(tint))
    }

    pub fn enable_host_backdrop(hwnd: isize) -> Result<(), String> {
        set_policy(hwnd, ACCENT_ENABLE_HOSTBACKDROP, 0, None)
    }
}

#[cfg(windows)]
mod host_backdrop {
    use std::cell::RefCell;

    use windows::{
        core::Interface,
        System::DispatcherQueueController,
        Win32::{
            Foundation::HWND,
            System::WinRT::{
                Composition::ICompositorDesktopInterop, CreateDispatcherQueueController,
                DispatcherQueueOptions, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
            },
        },
        UI::Composition::{
            CompositionBackdropBrush, Compositor, Desktop::DesktopWindowTarget, SpriteVisual,
        },
    };
    use windows_numerics::Vector2;

    struct BackdropState {
        _dispatcher: Option<DispatcherQueueController>,
        _compositor: Compositor,
        _target: DesktopWindowTarget,
        _visual: SpriteVisual,
        _brush: CompositionBackdropBrush,
    }

    thread_local! {
        static BACKDROP: RefCell<Option<BackdropState>> = const { RefCell::new(None) };
    }

    pub fn enable(hwnd: isize) -> Result<(), String> {
        BACKDROP.with(|slot| {
            if slot.borrow().is_some() {
                return Ok(());
            }

            let options = DispatcherQueueOptions {
                dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            };
            // Tauri's UI thread may already own a dispatcher queue. Keep a newly created
            // controller alive when creation succeeds; an existing queue is also valid.
            let dispatcher = unsafe { CreateDispatcherQueueController(options) }.ok();
            let compositor = Compositor::new().map_err(|error| error.to_string())?;
            let interop: ICompositorDesktopInterop =
                compositor.cast().map_err(|error| error.to_string())?;
            let target = unsafe {
                interop.CreateDesktopWindowTarget(HWND(hwnd as *mut core::ffi::c_void), false)
            }
            .map_err(|error| error.to_string())?;
            let visual = compositor
                .CreateSpriteVisual()
                .map_err(|error| error.to_string())?;
            visual
                .SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })
                .map_err(|error| error.to_string())?;
            visual.SetOpacity(0.78).map_err(|error| error.to_string())?;
            let brush = compositor
                .CreateHostBackdropBrush()
                .map_err(|error| error.to_string())?;
            visual.SetBrush(&brush).map_err(|error| error.to_string())?;
            target.SetRoot(&visual).map_err(|error| error.to_string())?;

            slot.replace(Some(BackdropState {
                _dispatcher: dispatcher,
                _compositor: compositor,
                _target: target,
                _visual: visual,
                _brush: brush,
            }));
            Ok(())
        })
    }

    pub fn disable() {
        BACKDROP.with(|slot| {
            slot.take();
        });
    }
}

/// 无边框窗口（decorations: false）在 Windows 上默认不进任务栏：任务栏按钮
/// 由 WS_EX_APPWINDOW 决定，而 Tauri 的 setSkipTaskbar 只管 WS_EX_TOOLWINDOW，
/// 补不上这个样式。样式必须在窗口隐藏时改，重新显示后 shell 才会重读。
#[cfg(windows)]
mod taskbar {
    use core::ffi::c_void;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };

    pub fn set_button(hwnd: isize, visible: bool) {
        let handle = HWND(hwnd as *mut c_void);
        unsafe {
            let current = GetWindowLongPtrW(handle, GWL_EXSTYLE) as u32;
            let updated = if visible {
                (current | WS_EX_APPWINDOW.0) & !WS_EX_TOOLWINDOW.0
            } else {
                (current & !WS_EX_APPWINDOW.0) | WS_EX_TOOLWINDOW.0
            };
            if updated != current {
                SetWindowLongPtrW(handle, GWL_EXSTYLE, updated as isize);
            }
        }
    }
}

/// 完整视图要出现在任务栏，小组件不要。调用方负责在隐藏状态下调用并随后重新显示。
#[tauri::command]
async fn set_taskbar_button(window: tauri::WebviewWindow, visible: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        let hwnd = window.hwnd().map_err(|error| error.to_string())?.0 as isize;
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        window
            .run_on_main_thread(move || {
                taskbar::set_button(hwnd, visible);
                let _ = sender.send(());
            })
            .map_err(|error| error.to_string())?;
        let _ = receiver.recv_timeout(std::time::Duration::from_secs(2));
    }
    #[cfg(not(windows))]
    {
        let _ = (&window, visible);
    }
    Ok(())
}

#[tauri::command]
async fn set_glass_backdrop(
    window: tauri::WebviewWindow,
    enabled: bool,
    tint: [u8; 4],
    dark: bool,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        let hwnd = window.hwnd().map_err(|error| error.to_string())?.0 as isize;
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        window
            .run_on_main_thread(move || {
                let result = if enabled {
                    let _ = swca::clear_dwm_acrylic(hwnd);
                    let _ = swca::set_round_corners(hwnd, true);
                    let host_result = swca::extend_glass_frame(hwnd, true)
                        .and_then(|_| swca::enable_host_backdrop(hwnd))
                        .and_then(|_| host_backdrop::enable(hwnd));
                    match host_result {
                        Ok(()) => {
                            eprintln!("[glass] HostBackdrop enabled");
                            Ok(())
                        }
                        Err(host_error) => {
                            eprintln!("[glass] HostBackdrop failed: {host_error}");
                            let _ = swca::set_acrylic(hwnd, None);
                            swca::set_blur(hwnd, tint).or_else(|blur_error| {
                                swca::set_dwm_acrylic(hwnd, dark).map_err(|dwm_error| {
                                    format!(
                                        "HostBackdrop failed: {host_error}; BlurBehind failed: \
                                         {blur_error}; DWM fallback failed: {dwm_error}"
                                    )
                                })
                            })
                        }
                    }
                } else {
                    host_backdrop::disable();
                    let _ = swca::extend_glass_frame(hwnd, false);
                    let _ = swca::set_round_corners(hwnd, false);
                    let dwm_result = swca::clear_dwm_acrylic(hwnd);
                    let swca_result = swca::set_acrylic(hwnd, None);
                    dwm_result.and(swca_result)
                };
                let _ = sender.send(result);
            })
            .map_err(|error| error.to_string())?;

        tauri::async_runtime::spawn_blocking(move || {
            receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|error| format!("glass composition task did not finish: {error}"))?
        })
        .await
        .map_err(|error| format!("glass composition task failed: {error}"))?
    }
    #[cfg(not(windows))]
    {
        let _ = (window, enabled, tint, dark);
        Err("SWCA acrylic 仅适用于 Windows".into())
    }
}

#[tauri::command]
async fn claude_hook_status() -> Result<claude_hook::ClaudeHookStatus, String> {
    tauri::async_runtime::spawn_blocking(|| {
        claude_hook::ClaudeHook::detected()
            .status()
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("claude hook status task failed: {error}"))?
}

#[tauri::command]
async fn set_claude_hook(enabled: bool) -> Result<claude_hook::ClaudeHookStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let hook = claude_hook::ClaudeHook::detected();
        if enabled {
            hook.install()
        } else {
            hook.uninstall()
        }
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("claude hook task failed: {error}"))?
}

/// 完整视图在 macOS 是带原生标题栏的独立窗口：用户手动选择明暗、且与系统相反时，
/// 让原生标题栏跟随内容主题（"自动"传 None，交回系统决定，与内容一致）。
/// Windows 的完整视图无边框、无原生标题栏，无需处理，这里对其它平台是 no-op。
#[tauri::command]
fn set_native_theme(window: tauri::WebviewWindow, theme: Option<String>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let resolved = match theme.as_deref() {
            Some("dark") => Some(tauri::Theme::Dark),
            Some("light") => Some(tauri::Theme::Light),
            _ => None,
        };
        window
            .set_theme(resolved)
            .map_err(|error| error.to_string())?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (&window, theme);
    }
    Ok(())
}

/// macOS 的完整视图是一个独立窗口（面板不能兼任），Windows 仍是同一个窗口变形，
/// 所以这个命令只在 macOS 上有实现，前端也只在 macOS 上调用它。
#[tauri::command]
fn open_expanded_window(app: tauri::AppHandle, nav: Option<String>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        macos::open_expanded_window(app, nav)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, nav);
        Err("独立的完整视图窗口仅用于 macOS".into())
    }
}

/// macOS 菜单栏面板在紧凑卡片与胶囊条之间切换尺寸；后端负责在改尺寸后
/// 重新锚定菜单栏图标。其它平台不调用此命令。
#[tauri::command]
fn resize_macos_panel(app: tauri::AppHandle, width: f64, height: f64) -> Result<(), String> {
    if !(48.0..=640.0).contains(&width) || !(40.0..=640.0).contains(&height) {
        return Err("macOS 面板尺寸超出允许范围".into());
    }
    #[cfg(target_os = "macos")]
    {
        macos::resize_panel(&app, width, height)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, width, height);
        Err("菜单栏面板尺寸切换仅用于 macOS".into())
    }
}

/// macOS 菜单栏按用户选择显示 Agent 图标和官方额度；其它平台没有对应状态项，
/// 前端也不会调用。None 表示额度不可用，不能按 0% 处理。
#[tauri::command]
fn update_macos_status_items(
    app: tauri::AppHandle,
    agents: Vec<String>,
    remaining: Vec<Option<f64>>,
    stale: Vec<bool>,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        macos::update_status_items(&app, &agents, &remaining, &stale)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, agents, remaining, stale);
        Err("菜单栏用量状态项仅用于 macOS".into())
    }
}

/// 托盘菜单请求完整视图；前端监听后自己完成变形（见 windowClient 的
/// onTrayShowExpanded）。macOS 的完整视图是独立窗口，走 macos.rs 自己的菜单栏。
#[cfg(all(desktop, not(target_os = "macos")))]
const TRAY_SHOW_EXPANDED: &str = "tray://show-expanded";

#[cfg(all(desktop, not(target_os = "macos")))]
fn toggle_main_window(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let minimized = window.is_minimized().unwrap_or(false);
    let visible = window.is_visible().unwrap_or(false);
    if visible && !minimized {
        let _ = window.hide();
    } else {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(all(desktop, not(target_os = "macos")))]
fn setup_tray(app: &mut tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    use tauri::Emitter;

    let toggle = MenuItem::with_id(app, "toggle", "显示 / 隐藏", true, None::<&str>)?;
    let expanded = MenuItem::with_id(app, "expanded", "显示完整视图", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出 Metrik", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &expanded, &separator, &quit])?;

    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Metrik")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "toggle" => toggle_main_window(app),
            // 胶囊/卡片直达完整视图，省掉"先弹卡片再点展开"这一步。窗口形态归
            // 前端所有（Windows 是单窗口变形），所以这里只发意图：前端切到
            // expanded 时自己会 show + focus，托盘再动窗口只会抢出闪帧。
            "expanded" => {
                let _ = app.emit(TRAY_SHOW_EXPANDED, ());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 前端窗口形态必须使用编译期真实平台，不能依赖 WebView user-agent。
    let builder = tauri::Builder::default().plugin(tauri_plugin_os::init());

    #[cfg(target_os = "macos")]
    let builder = builder.plugin(tauri_nspanel::init());

    #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        // macOS 上小插件是菜单栏面板：第二个实例把面板弹回图标下方，而不是显示一个游离窗口。
        #[cfg(target_os = "macos")]
        macos::show_panel(app);

        #[cfg(not(target_os = "macos"))]
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }));

    // 开机启动由用户在设置页 opt-in；这里只注册能力，不默认启用。
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_autostart::init(
        tauri_plugin_autostart::MacosLauncher::LaunchAgent,
        None,
    ));

    // 更新检查由用户在设置页手动触发；不自动下载、不静默安装。
    // 更新包用项目自己的 minisign 密钥签名，防止分发链路被掉包。
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init());

    builder
        .setup(|app| {
            // macOS 是一个菜单栏应用：面板 + 独立完整视图窗口 + template 图标，
            // 与 Windows 的"单窗口变形 + 自绘按钮"完全分开。
            #[cfg(target_os = "macos")]
            macos::setup(app)?;

            #[cfg(all(desktop, not(target_os = "macos")))]
            setup_tray(app)?;

            let database_path = match (
                app.path().app_data_dir(),
                app.path().app_local_data_dir(),
            ) {
                (Ok(legacy_app_data), Ok(local_app_data)) => {
                    let local_database = local_app_data.join(DATABASE_FILE_NAME);
                    resolve_database_path(
                        &legacy_app_data.join(DATABASE_FILE_NAME),
                        &local_database,
                    )
                    .unwrap_or_else(|error| {
                        eprintln!(
                            "Metrik could not prepare its local ledger ({error:#}); using a session ledger instead"
                        );
                        emergency_database_path()
                    })
                }
                (legacy_result, local_result) => {
                    let legacy_error = legacy_result.err().map(|error| error.to_string());
                    let local_error = local_result.err().map(|error| error.to_string());
                    eprintln!(
                        "Metrik could not resolve its application data directories (legacy: {legacy_error:?}, local: {local_error:?}); using a session ledger instead"
                    );
                    emergency_database_path()
                }
            };
            app.manage(AppState {
                database_path,
                scan_gate: Arc::new(Mutex::new(())),
                quota_cache: Arc::new(Mutex::new(None)),
                claude_quota_cache: Arc::new(Mutex::new(None)),
                http_quota_cache: Arc::new(Mutex::new(HashMap::new())),
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭时收进托盘常驻，退出走托盘菜单。
            // macOS 的完整视图是独立窗口，红灯就该真的关掉它（关掉后 App 退回菜单栏）。
            #[cfg(desktop)]
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() != "expanded" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            usage_snapshot,
            usage_report,
            usage_sessions,
            export_csv,
            rebuild_local_ledger,
            sync_settings,
            configure_sync,
            claude_hook_status,
            set_claude_hook,
            claude_oauth_status,
            set_claude_oauth,
            set_taskbar_button,
            set_glass_backdrop,
            set_native_theme,
            open_expanded_window,
            resize_macos_panel,
            update_macos_status_items
        ])
        .run(tauri::generate_context!())
        .expect("error while running Metrik");
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let sequence = MIGRATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("metrik-{label}-{}-{sequence}", std::process::id()));
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
    fn migrates_database_and_sidecars_without_removing_legacy_files() {
        let test = TestDirectory::new("database-migration");
        let legacy = test.path().join("roaming").join(DATABASE_FILE_NAME);
        let local = test.path().join("local").join(DATABASE_FILE_NAME);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, b"legacy database").unwrap();
        for (suffix, contents) in [
            ("-wal", b"legacy wal".as_slice()),
            ("-shm", b"legacy shm".as_slice()),
            ("-journal", b"legacy journal".as_slice()),
        ] {
            fs::write(sqlite_sidecar_path(&legacy, suffix), contents).unwrap();
        }

        migrate_legacy_database(&legacy, &local).unwrap();

        assert_eq!(fs::read(&local).unwrap(), b"legacy database");
        assert_eq!(
            fs::read(sqlite_sidecar_path(&local, "-wal")).unwrap(),
            b"legacy wal"
        );
        assert_eq!(
            fs::read(sqlite_sidecar_path(&local, "-shm")).unwrap(),
            b"legacy shm"
        );
        assert_eq!(
            fs::read(sqlite_sidecar_path(&local, "-journal")).unwrap(),
            b"legacy journal"
        );
        assert_eq!(fs::read(&legacy).unwrap(), b"legacy database");
        assert!(sqlite_sidecar_path(&legacy, "-wal").exists());
        assert!(sqlite_sidecar_path(&legacy, "-shm").exists());
        assert!(sqlite_sidecar_path(&legacy, "-journal").exists());
    }

    #[test]
    fn existing_local_database_is_never_overwritten() {
        let test = TestDirectory::new("database-migration-existing-local");
        let legacy = test.path().join("roaming").join(DATABASE_FILE_NAME);
        let local = test.path().join("local").join(DATABASE_FILE_NAME);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::create_dir_all(local.parent().unwrap()).unwrap();
        fs::write(&legacy, b"legacy database").unwrap();
        fs::write(sqlite_sidecar_path(&legacy, "-wal"), b"legacy wal").unwrap();
        fs::write(&local, b"newer local database").unwrap();
        fs::write(sqlite_sidecar_path(&local, "-wal"), b"newer local wal").unwrap();

        migrate_legacy_database(&legacy, &local).unwrap();

        assert_eq!(fs::read(&local).unwrap(), b"newer local database");
        assert_eq!(
            fs::read(sqlite_sidecar_path(&local, "-wal")).unwrap(),
            b"newer local wal"
        );
    }

    #[test]
    fn conflicting_local_sidecar_is_not_overwritten_or_partially_installed() {
        let test = TestDirectory::new("database-migration-sidecar-conflict");
        let legacy = test.path().join("roaming").join(DATABASE_FILE_NAME);
        let local = test.path().join("local").join(DATABASE_FILE_NAME);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::create_dir_all(local.parent().unwrap()).unwrap();
        fs::write(&legacy, b"legacy database").unwrap();
        fs::write(sqlite_sidecar_path(&local, "-wal"), b"local wal").unwrap();

        let error = migrate_legacy_database(&legacy, &local).unwrap_err();

        assert!(error.to_string().contains("refusing to overwrite"));
        assert!(!local.exists());
        assert_eq!(
            fs::read(sqlite_sidecar_path(&local, "-wal")).unwrap(),
            b"local wal"
        );
    }

    #[test]
    fn migration_sidecar_copy_failure_uses_recovery_database() {
        let test = TestDirectory::new("database-migration-copy-fallback");
        let legacy = test.path().join("roaming").join(DATABASE_FILE_NAME);
        let local = test.path().join("local").join(DATABASE_FILE_NAME);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, b"legacy database").unwrap();
        let unreadable_sidecar = sqlite_sidecar_path(&legacy, "-wal");
        fs::create_dir(&unreadable_sidecar).unwrap();

        let resolved = resolve_database_path(&legacy, &local).unwrap();

        assert_eq!(resolved, recovery_database_path(&local, 1).unwrap());
        assert!(resolved.exists());
        assert_eq!(fs::read(&legacy).unwrap(), b"legacy database");
        assert!(unreadable_sidecar.is_dir());
        assert!(!local.exists());
    }

    #[test]
    fn migration_conflict_uses_recovery_without_consuming_local_orphan() {
        let test = TestDirectory::new("database-migration-conflict-fallback");
        let legacy = test.path().join("roaming").join(DATABASE_FILE_NAME);
        let local = test.path().join("local").join(DATABASE_FILE_NAME);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::create_dir_all(local.parent().unwrap()).unwrap();
        fs::write(&legacy, b"legacy database").unwrap();
        let orphan = sqlite_sidecar_path(&local, "-wal");
        fs::write(&orphan, b"orphan local wal").unwrap();

        let resolved = resolve_database_path(&legacy, &local).unwrap();

        assert_eq!(resolved, recovery_database_path(&local, 1).unwrap());
        assert!(resolved.exists());
        assert!(!local.exists());
        assert_eq!(fs::read(orphan).unwrap(), b"orphan local wal");
        assert_eq!(fs::read(legacy).unwrap(), b"legacy database");
    }

    #[test]
    fn recovery_selection_skips_candidate_with_orphan_sidecar() {
        let test = TestDirectory::new("database-recovery-sidecar-conflict");
        let legacy = test.path().join("roaming").join(DATABASE_FILE_NAME);
        let local = test.path().join("local").join(DATABASE_FILE_NAME);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::create_dir_all(local.parent().unwrap()).unwrap();
        fs::write(&legacy, b"legacy database").unwrap();
        fs::write(sqlite_sidecar_path(&local, "-wal"), b"local orphan").unwrap();
        let first_recovery = recovery_database_path(&local, 1).unwrap();
        let first_orphan = sqlite_sidecar_path(&first_recovery, "-journal");
        fs::write(&first_orphan, b"recovery orphan").unwrap();

        let resolved = resolve_database_path(&legacy, &local).unwrap();

        assert_eq!(resolved, recovery_database_path(&local, 2).unwrap());
        assert!(resolved.exists());
        assert!(!first_recovery.exists());
        assert_eq!(fs::read(first_orphan).unwrap(), b"recovery orphan");
    }

    #[test]
    fn existing_recovery_database_is_reused_after_migration_conflict() {
        let test = TestDirectory::new("database-recovery-reuse");
        let legacy = test.path().join("roaming").join(DATABASE_FILE_NAME);
        let local = test.path().join("local").join(DATABASE_FILE_NAME);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::create_dir_all(local.parent().unwrap()).unwrap();
        fs::write(&legacy, b"legacy database").unwrap();
        fs::write(sqlite_sidecar_path(&local, "-wal"), b"local orphan").unwrap();
        let recovery = recovery_database_path(&local, 1).unwrap();
        fs::write(&recovery, b"existing recovery").unwrap();
        fs::write(sqlite_sidecar_path(&recovery, "-wal"), b"recovery wal").unwrap();

        let resolved = resolve_database_path(&legacy, &local).unwrap();

        assert_eq!(resolved, recovery);
        assert_eq!(fs::read(&resolved).unwrap(), b"existing recovery");
        assert_eq!(
            fs::read(sqlite_sidecar_path(&resolved, "-wal")).unwrap(),
            b"recovery wal"
        );
    }

    #[test]
    fn migrated_wal_database_retains_uncheckpointed_rows() {
        let test = TestDirectory::new("wal-database-migration");
        let legacy = test.path().join("roaming").join(DATABASE_FILE_NAME);
        let local = test.path().join("local").join(DATABASE_FILE_NAME);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();

        let source = Connection::open(&legacy).unwrap();
        source.pragma_update(None, "journal_mode", "WAL").unwrap();
        source.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        source
            .execute_batch(
                "CREATE TABLE sample (value TEXT NOT NULL);\
                 INSERT INTO sample (value) VALUES ('from wal');",
            )
            .unwrap();
        assert!(sqlite_sidecar_path(&legacy, "-wal").exists());

        migrate_legacy_database(&legacy, &local).unwrap();

        let migrated = Connection::open(&local).unwrap();
        let value: String = migrated
            .query_row("SELECT value FROM sample", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "from wal");
        drop(migrated);
        drop(source);
        assert!(legacy.exists());
    }

    #[test]
    fn missing_legacy_database_is_a_noop() {
        let test = TestDirectory::new("database-migration-no-source");
        let legacy = test.path().join("roaming").join(DATABASE_FILE_NAME);
        let local = test.path().join("local").join(DATABASE_FILE_NAME);

        migrate_legacy_database(&legacy, &local).unwrap();

        assert!(!local.exists());
    }
}
#[test]
fn emergency_database_paths_are_unique_and_stay_in_the_temp_directory() {
    let first = emergency_database_path();
    let second = emergency_database_path();

    assert_eq!(first.parent(), Some(std::env::temp_dir().as_path()));
    assert_eq!(second.parent(), Some(std::env::temp_dir().as_path()));
    assert_ne!(first, second);
    assert_eq!(
        first.extension().and_then(|value| value.to_str()),
        Some("sqlite3")
    );
}
