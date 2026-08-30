mod adapters;
mod app_server;
mod claude_hook;
mod claude_oauth;
mod coding_quota;
mod detect;
mod domain;
mod engine;
#[cfg(target_os = "macos")]
mod macos;
mod pi_providers;
#[cfg(target_os = "linux")]
mod pinned_hover;
mod pricing;
mod projects;
mod quota;
mod schema;
mod storage;
mod sync;
#[cfg(target_os = "macos")]
mod widget_snapshot;

use anyhow::{Context, Result};
use domain::{UsageProjects, UsageReport, UsageSessions, UsageSnapshot};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{Manager, State};

/// 各家官方配额按 adapter 分桶缓存，跨快照持有以限流。取数节奏由各 provider
/// 自己声明，见 `quota` 模块。
type SharedQuotaCache = Arc<quota::QuotaCache>;

const DATABASE_FILE_NAME: &str = "metrik.sqlite3";
const RECOVERY_DATABASE_FILE_NAME: &str = "metrik.recovery.sqlite3";
const SQLITE_SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];
static MIGRATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct AppState {
    database_path: PathBuf,
    scan_gate: Arc<Mutex<()>>,
    quota_cache: SharedQuotaCache,
    #[cfg(target_os = "linux")]
    pinned_hover_monitor: Arc<pinned_hover::Monitor>,
}

/// Linux 首窗必须在 WebView 初始化前决定位置，否则 GTK 会先映射默认居中的
/// 窗口，再由前端 localStorage 移动，造成肉眼可见的闪烁。
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct LinuxStartupPosition {
    x: i32,
    y: i32,
    #[serde(default)]
    offset_x: i32,
    #[serde(default)]
    offset_y: i32,
}

#[cfg(target_os = "linux")]
const LINUX_STARTUP_POSITION_FILE: &str = "linux-startup-position.json";

#[cfg(target_os = "linux")]
fn linux_startup_position_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .app_local_data_dir()
        .ok()
        .map(|directory| directory.join(LINUX_STARTUP_POSITION_FILE))
}

#[cfg(target_os = "linux")]
fn read_linux_startup_position(app: &tauri::AppHandle) -> Option<LinuxStartupPosition> {
    let path = linux_startup_position_path(app)?;
    let contents = fs::read(path).ok()?;
    serde_json::from_slice(&contents).ok()
}

#[cfg(target_os = "linux")]
fn restore_linux_startup_position(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if let Some(position) = read_linux_startup_position(app) {
        let _ = window.set_position(tauri::PhysicalPosition::new(
            position.x.saturating_add(position.offset_x),
            position.y.saturating_add(position.offset_y),
        ));
    } else {
        // 首次安装没有位置可恢复时才居中；后续启动永远使用持久化坐标。
        let _ = window.center();
    }
    let _ = window.show();
}

/// Linux 托盘菜单和前端的置顶状态保持同一份权威值，以便菜单项准确显示
/// “置顶”或“取消置顶”。该状态只属于 Linux shell，Windows/macOS 不会编译它。
#[cfg(target_os = "linux")]
#[derive(Default)]
struct LinuxTrayPinMenu {
    pinned: AtomicBool,
    item: Mutex<Option<tauri::menu::MenuItem<tauri::Wry>>>,
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
    widget_agents: Option<Vec<String>>,
    state: State<'_, AppState>,
) -> Result<UsageSnapshot, String> {
    let database_path = state.database_path.clone();
    let scan_gate = Arc::clone(&state.scan_gate);
    let quota_cache = Arc::clone(&state.quota_cache);

    tauri::async_runtime::spawn_blocking(move || {
        let _gate = scan_gate
            .lock()
            .map_err(|_| "usage scan lock poisoned".to_owned())?;
        let snapshot = engine::build_snapshot(
            &database_path,
            &period,
            &quota_cache,
            force.unwrap_or(false),
        )
        .map_err(|error| error.to_string())?;

        // WidgetKit 只存在于 macOS；其它平台上这个参数没有消费者，
        // 显式标记已读，避免 clippy 的 unused-variables 报警。
        #[cfg(not(target_os = "macos"))]
        let _ = &widget_agents;

        #[cfg(target_os = "macos")]
        let requested_widget_agents = widget_agents
            .as_deref()
            .map(widget_snapshot::normalize_agent_filter);

        #[cfg(target_os = "macos")]
        let widget_agents = storage::open_database(&database_path)
            .and_then(|connection| {
                let saved = storage::get_app_setting(
                    &connection,
                    widget_snapshot::AGENT_FILTER_SETTING_KEY,
                )?;

                // 数据轮询不是设置写入通道：一旦有权威选择，任何窗口携带的旧
                // widget_agents 都只能被忽略。首次升级没有保存值时，才用调用
                // 窗口的 localStorage 播种一次。
                let (resolved, should_persist) = widget_snapshot::resolve_agent_filter(
                    saved.as_deref(),
                    requested_widget_agents.as_deref(),
                );
                if should_persist {
                    let agents = resolved.as_ref().expect("initial selection is present");
                    let encoded = serde_json::to_string(agents)?;
                    storage::set_app_setting(
                        &connection,
                        widget_snapshot::AGENT_FILTER_SETTING_KEY,
                        &encoded,
                    )?;
                }
                Ok(resolved)
            })
            .unwrap_or_else(|error| {
                eprintln!("Metrik could not read its WidgetKit Agent selection ({error:#})");
                requested_widget_agents.filter(|agents| !agents.is_empty())
            });

        #[cfg(target_os = "macos")]
        if let Err(error) = widget_snapshot::persist(&snapshot, widget_agents.as_deref()) {
            // Widget publication is a secondary output. A temporary App Group or
            // filesystem failure must not hide the primary Metrik usage result.
            eprintln!("Metrik could not publish its WidgetKit snapshot ({error:#})");
        }

        Ok(snapshot)
    })
    .await
    .map_err(|error| format!("usage scan task failed: {error}"))?
}

/// 设置窗口显式提交 macOS Agent 选择。它是唯一允许覆盖已保存选择的通道；
/// 普通 usage_snapshot 轮询只能读取该值，防止隐藏 WebView 用旧状态回滚设置。
#[tauri::command]
async fn set_macos_agent_selection(
    agents: Vec<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let agents = widget_snapshot::normalize_agent_filter(&agents);
        if agents.is_empty() {
            return Err("macOS 菜单栏至少保留一个 Agent".into());
        }
        let database_path = state.database_path.clone();
        let scan_gate = Arc::clone(&state.scan_gate);
        tauri::async_runtime::spawn_blocking(move || {
            let _gate = scan_gate
                .lock()
                .map_err(|_| "usage scan lock poisoned".to_owned())?;
            let connection =
                storage::open_database(&database_path).map_err(|error| error.to_string())?;
            let encoded = serde_json::to_string(&agents).map_err(|error| error.to_string())?;
            storage::set_app_setting(
                &connection,
                widget_snapshot::AGENT_FILTER_SETTING_KEY,
                &encoded,
            )
            .map_err(|error| error.to_string())?;
            drop(connection);

            // 选择本身不需要重新扫描 Agent 日志；用缓存快照即可立刻更新
            // WidgetKit 内容与时间线，额度/用量数值保持现有来源语义。
            let snapshot = engine::build_cached_snapshot(&database_path, "today")
                .map_err(|error| error.to_string())?;
            widget_snapshot::persist(&snapshot, Some(&agents))
                .map_err(|error| error.to_string())?;
            // 用户显式操作，即时 reload 一次让小组件马上换选择；频率受操作次数
            // 限制，不会耗尽 WidgetKit 的应用级刷新配额（轮询路径不 reload）。
            widget_snapshot::reload_timelines();
            Ok(())
        })
        .await
        .map_err(|error| format!("macOS Agent selection task failed: {error}"))?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (agents, state);
        Err("macOS Agent 选择仅用于 macOS".into())
    }
}

/// 读取后端保存的 macOS Agent 权威选择。macOS 的设置窗口、菜单栏面板、桌面组件
/// 是各自独立的 WebView，localStorage 互不同步，本地值只能当缓存；各窗口启动时
/// 以此为准。空数组表示还没有保存过选择（首次启动），窗口退回本地缓存播种。
#[tauri::command]
async fn get_macos_agent_selection(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    #[cfg(target_os = "macos")]
    {
        let database_path = state.database_path.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let connection =
                storage::open_database(&database_path).map_err(|error| error.to_string())?;
            let saved =
                storage::get_app_setting(&connection, widget_snapshot::AGENT_FILTER_SETTING_KEY)
                    .map_err(|error| error.to_string())?;
            Ok(
                widget_snapshot::resolve_agent_filter(saved.as_deref(), None)
                    .0
                    .unwrap_or_default(),
            )
        })
        .await
        .map_err(|error| format!("macOS Agent selection task failed: {error}"))?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = state;
        Err("macOS Agent 选择仅用于 macOS".into())
    }
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

/// 只读项目明细：与会话明细同源，按分组规则归并后聚合。
#[tauri::command]
async fn usage_projects(
    period: String,
    state: State<'_, AppState>,
) -> Result<UsageProjects, String> {
    let database_path = state.database_path.clone();

    tauri::async_runtime::spawn_blocking(move || {
        engine::build_projects(&database_path, &period).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("usage projects task failed: {error}"))?
}

/// 读取项目分组规则（手动项目根与隐藏目录）。
#[tauri::command]
async fn project_rules(state: State<'_, AppState>) -> Result<projects::ProjectRules, String> {
    let database_path = state.database_path.clone();

    tauri::async_runtime::spawn_blocking(move || {
        let connection =
            storage::open_database_read_only(&database_path).map_err(|error| error.to_string())?;
        projects::load_rules(&connection).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("project rules task failed: {error}"))?
}

/// 保存项目分组规则，返回归一化去重后的结果。只写 `app_setting` 一行，
/// 不触发扫描；分组在查询层生效，下一次读取即是新规则。
#[tauri::command]
async fn set_project_rules(
    rules: projects::ProjectRules,
    state: State<'_, AppState>,
) -> Result<projects::ProjectRules, String> {
    let database_path = state.database_path.clone();

    tauri::async_runtime::spawn_blocking(move || {
        let connection =
            storage::open_database(&database_path).map_err(|error| error.to_string())?;
        projects::save_rules(&connection, rules).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("set project rules task failed: {error}"))?
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

    tauri::async_runtime::spawn_blocking(move || {
        let _gate = scan_gate
            .lock()
            .map_err(|_| "usage scan lock poisoned".to_owned())?;
        storage::reset_derived_ledger(&database_path).map_err(|error| error.to_string())?;
        engine::build_snapshot(&database_path, &period, &quota_cache, false)
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
        let failure = claude_oauth::last_failure(&connection).map_err(|error| error.to_string())?;
        Ok(claude_oauth::ClaudeOauth::detected()
            .status(enabled)
            .with_failure(failure))
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
    let quota_cache = Arc::clone(&state.quota_cache);
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
        // 上一轮的失败原因对新开关无效，留着会指向已经不存在的问题。
        claude_oauth::clear_failure(&connection).map_err(|error| error.to_string())?;
        // 清缓存让下一次快照立即按新开关取数。
        if let Ok(mut guard) = quota_cache.lock() {
            guard.remove("claude");
        }
        Ok(claude_oauth::ClaudeOauth::detected().status(enabled))
    })
    .await
    .map_err(|error| format!("set claude oauth task failed: {error}"))?
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct QoderCookieView {
    configured: bool,
    /// "file"（设置页保存）或 "env"（环境变量）；None = 未配置。
    source: Option<&'static str>,
    /// 保存/清除/验证的结果描述，给设置页直接展示。
    message: Option<String>,
}

#[tauri::command]
async fn qoder_cookie_status() -> Result<QoderCookieView, String> {
    let source = coding_quota::qoder_cookie_source();
    Ok(QoderCookieView {
        configured: source.is_some(),
        source,
        message: None,
    })
}

/// 保存（Some）或清除（None）设置页提供的 Qoder cookie；保存后立即拉一次
/// 官方 Credits 验证，把结果原样反馈——用户不用猜有没有配对。
#[tauri::command]
async fn configure_qoder_cookie(cookie: Option<String>) -> Result<QoderCookieView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // 宽容解析：整段请求标头 / cURL / 带 Cookie: 前缀 / 裸值都接受。
        let normalized = match cookie.as_deref() {
            Some(raw) => Some(
                coding_quota::normalize_qoder_cookie_input(raw)
                    .ok_or_else(|| "粘贴内容里没有找到 Cookie 行".to_owned())?,
            ),
            None => None,
        };
        let saved = coding_quota::write_qoder_cookie_file(normalized.as_deref())
            .map_err(|error| error.to_string())?;
        let source = coding_quota::qoder_cookie_source();
        if !saved {
            return Ok(QoderCookieView {
                configured: source.is_some(),
                source,
                message: Some("已清除本地保存的 cookie。".to_owned()),
            });
        }
        let message = match coding_quota::fetch_qoder_quota(std::time::Duration::from_secs(10)) {
            Ok(samples) => {
                let sample = &samples[0];
                let reset = sample
                    .resets_at_ms
                    .map(|at| {
                        let minutes = (at - chrono::Utc::now().timestamp_millis()) / 60_000;
                        if minutes > 0 {
                            format!("，约 {} 小时后重置", (minutes + 30) / 60)
                        } else {
                            String::new()
                        }
                    })
                    .unwrap_or_default();
                format!(
                    "已保存并验证成功：Credits 剩余 {:.0}%{reset}。",
                    sample.remaining_percent
                )
            }
            Err(error) => format!("已保存，但验证失败：{error}"),
        };
        Ok(QoderCookieView {
            configured: true,
            source,
            message: Some(message),
        })
    })
    .await
    .map_err(|error| format!("qoder cookie task failed: {error}"))?
}

#[tauri::command]
async fn sync_settings(state: State<'_, AppState>) -> Result<domain::SyncView, String> {
    let database_path = state.database_path.clone();

    // 与会话/项目/分组规则同一惯例：设置页的读取不占扫描锁。占了的话，打开
    // 设置正好赶上一次扫描，整张同步卡片要等扫描结束才出现——界面看着是
    // 空的，然后突然长出一截。写连接仍然要保留：首次调用会补写设备身份。
    tauri::async_runtime::spawn_blocking(move || {
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

#[tauri::command]
async fn remove_sync_device(
    device_id: String,
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
        sync::remove_device(&mut connection, &device_id).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("sync device removal task failed: {error}"))?
}

/// 无边框窗口（decorations: false）在 Windows 上默认不进任务栏：任务栏按钮
/// 由 WS_EX_APPWINDOW 决定，而 Tauri 的 setSkipTaskbar 只管 WS_EX_TOOLWINDOW，
/// 补不上这个样式。样式必须在窗口隐藏时改，重新显示后 shell 才会重读。
///
/// 光改样式还不够：Tauri 的 setSkipTaskbar(true) 是用 ITaskbarList::DeleteTab
/// 把窗口从任务栏**注销**的，样式翻转撤不掉这个注销（Win11 实测：APPWINDOW
/// 已置位、隐藏重显后按钮依旧不出现，AddTab 一调立即出现）。所以两件事都做：
/// 样式对齐 + ITaskbarList 登记/注销。
#[cfg(windows)]
mod taskbar {
    use core::ffi::c_void;
    use windows::core::GUID;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::ITaskbarList;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };

    const CLSID_TASKBAR_LIST: GUID = GUID::from_u128(0x56fdf344_fd6d_11d0_958a_006097c9a090);

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
            // 主线程调用（run_on_main_thread），COM 已由 WebView2 初始化。
            // 失败静默：任务栏登记是体验增强，不值得让整个变形流程失败。
            if let Ok(list) =
                CoCreateInstance::<_, ITaskbarList>(&CLSID_TASKBAR_LIST, None, CLSCTX_INPROC_SERVER)
            {
                let _ = list.HrInit();
                if visible {
                    let _ = list.AddTab(handle);
                } else {
                    let _ = list.DeleteTab(handle);
                }
            }
        }
    }
}

/// Win11 默认给顶层窗口画系统圆角，并沿那条弧描一道边。tao 的 `to_window_styles()`
/// 无条件加 `WS_CAPTION`（decorations: false 只在 `to_adjusted_window_styles()` 里剥，
/// 那个只用于算尺寸），所以无边框窗口照样被 DWM 圆角，而 tao/wry/tauri 三层都没有
/// 设置过这个属性。
///
/// DWM 的半径跟系统 DPI 走，`#root` 那条 `--glass-radius` 按物理像素钉死，两者不存在
/// 能重合的缩放档；再叠上客户区相对窗口的缩进，画面上就是两个同心圆角矩形——四角外
/// 面多一圈底，暗色档尤其明显。关掉 DWM 这条，`--glass-radius` 才是唯一轮廓。
///
/// 只关圆角，不用 `SetWindowRgn`：GDI region 是二值边界，压在 per-pixel alpha 上是硬
/// 锯齿；而且它在变形与内容测量并发时会短暂沿用旧区域（见 WINDOWS-GLASS-IMPLEMENTATION
/// 第 7 节）。pogget 用 region 是因为它是不透明窗口，没有 alpha 可用。
#[cfg(windows)]
fn disable_system_corner_rounding(hwnd: isize) {
    use core::ffi::c_void;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    };

    let handle = HWND(hwnd as *mut c_void);
    let preference = DWMWCP_DONOTROUND;
    unsafe {
        // 失败静默：圆角是装饰，不值得让启动失败。
        let _ = DwmSetWindowAttribute(
            handle,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const _ as *const c_void,
            std::mem::size_of_val(&preference) as u32,
        );
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

/// Windows 托盘余量徽标：把前端渲染好的 RGBA 位图换成任务栏托盘图标。
/// 隐藏窗口后任务栏按钮本身就没了，能常驻显示数字的只有通知区域的托盘图标；
/// 位图由 webview 里的 canvas 画出（渲染权威在前端，与悬浮窗尺寸同一套哲学），
/// 后端只做校验和登记。icon 为 None 时恢复应用默认图标。
#[derive(Debug, serde::Deserialize)]
struct TrayQuotaIcon {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

/// 托盘图标尺寸上限：徽标实际是 32×32，这里只拦离谱的输入，不当作配置项。
const TRAY_ICON_MAX_EDGE: u32 = 256;

/// 负载校验：尺寸有界且 RGBA 长度与宽高一致。独立成函数供单测，不依赖平台。
fn validate_tray_quota_icon(icon: &TrayQuotaIcon) -> Result<(), String> {
    if icon.width == 0
        || icon.height == 0
        || icon.width > TRAY_ICON_MAX_EDGE
        || icon.height > TRAY_ICON_MAX_EDGE
    {
        return Err(format!(
            "托盘图标尺寸超出范围：{}×{}",
            icon.width, icon.height
        ));
    }
    let expected = icon.width as usize * icon.height as usize * 4;
    if icon.rgba.len() != expected {
        return Err(format!(
            "托盘图标像素数据长度不匹配：{} ≠ {expected}",
            icon.rgba.len()
        ));
    }
    Ok(())
}

/// 设置 Windows 托盘图标为余量徽标；icon 为 None 时恢复默认图标与默认提示。
/// 其它平台没有这条路径（前端只在 Windows 调用），这里是 no-op。
#[tauri::command]
fn set_tray_quota_icon(
    app: tauri::AppHandle,
    icon: Option<TrayQuotaIcon>,
    tooltip: Option<String>,
) -> Result<(), String> {
    // 校验放在平台分支之前：非 Windows 构建里也读字段、走同一契约，
    // 否则 dead_code 会把 TrayQuotaIcon、校验函数和上限常量判成死代码。
    if let Some(payload) = icon.as_ref() {
        validate_tray_quota_icon(payload)?;
    }
    #[cfg(windows)]
    {
        let Some(tray) = app.tray_by_id("main") else {
            return Err("任务栏托盘图标不存在".into());
        };
        match icon {
            Some(payload) => {
                let image =
                    tauri::image::Image::new_owned(payload.rgba, payload.width, payload.height);
                tray.set_icon(Some(image))
                    .map_err(|error| error.to_string())?;
            }
            None => {
                let fallback = app
                    .default_window_icon()
                    .cloned()
                    .ok_or_else(|| "默认应用图标不可用".to_string())?;
                tray.set_icon(Some(fallback))
                    .map_err(|error| error.to_string())?;
            }
        }
        // 提示文本跟着数字走（如 "Metrik · Claude 剩余 87%"）；恢复时回到默认。
        let tooltip = tooltip.filter(|text| !text.trim().is_empty());
        tray.set_tooltip(tooltip.as_deref().or(Some("Metrik")))
            .map_err(|error| error.to_string())?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (app, icon, tooltip);
        Ok(())
    }
}

/// X11 允许客户端读取和恢复全局窗口坐标；Wayland 刻意不暴露这套能力。
/// 前端据此决定是否启用位置记忆、钳位与边缘挂靠，而不是把所有 Linux
/// 会话一律降级。
#[cfg(target_os = "linux")]
fn linux_session_supports_global_window_coordinates(
    session: &str,
    has_wayland_display: bool,
    has_x11_display: bool,
) -> bool {
    let session = session.to_ascii_lowercase();
    if session == "wayland" || has_wayland_display {
        return false;
    }
    session == "x11" || has_x11_display
}

#[cfg(target_os = "linux")]
#[tauri::command]
fn linux_supports_global_window_coordinates() -> bool {
    linux_session_supports_global_window_coordinates(
        &std::env::var("XDG_SESSION_TYPE").unwrap_or_default(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        std::env::var_os("DISPLAY").is_some(),
    )
}

/// Configure the pinned widget's native hover watcher. On X11 one long-lived
/// worker queries window-relative pointer coordinates and changes the GTK
/// toplevel opacity directly.
#[cfg(target_os = "linux")]
#[tauri::command]
fn configure_pinned_hover(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    enabled: bool,
    target_opacity: f64,
) -> bool {
    pinned_hover::configure(
        window,
        Arc::clone(&state.pinned_hover_monitor),
        enabled,
        target_opacity,
    )
}

/// Synchronize the Linux tray item's label after a settings-page change.
#[cfg(target_os = "linux")]
#[tauri::command]
fn sync_linux_tray_pinned(state: State<'_, LinuxTrayPinMenu>, pinned: bool) {
    state.pinned.store(pinned, Ordering::Release);
    if let Ok(item) = state.item.lock() {
        if let Some(item) = item.as_ref() {
            let _ = item.set_text(if pinned { "取消置顶" } else { "置顶" });
        }
    }
}

/// Persist the exact GTK outer position and its inner/outer offset whenever a
/// Linux floating form is moved. This small native file is intentionally
/// separate from WebKit localStorage so setup can restore it before the first
/// WebView frame is ever mapped.
#[cfg(target_os = "linux")]
#[tauri::command]
fn persist_linux_startup_position(
    app: tauri::AppHandle,
    x: i32,
    y: i32,
    offset_x: i32,
    offset_y: i32,
) -> Result<(), String> {
    let Some(path) = linux_startup_position_path(&app) else {
        return Err("Linux application data directory is unavailable".into());
    };
    let position = LinuxStartupPosition {
        x,
        y,
        offset_x,
        offset_y,
    };
    let encoded = serde_json::to_vec(&position).map_err(|error| error.to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "Linux startup position has no parent directory".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    fs::write(path, encoded).map_err(|error| error.to_string())
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
// objc/cocoa 的老宏在 clippy 下报 unexpected_cfgs/deprecated，与 macos.rs 头部同理豁免。
#[cfg_attr(target_os = "macos", allow(deprecated, unexpected_cfgs))]
#[tauri::command]
fn set_native_theme(window: tauri::WebviewWindow, theme: Option<String>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use objc::{class, msg_send, sel, sel_impl};
        use tauri_nspanel::cocoa::base::{id, nil};
        use tauri_nspanel::cocoa::foundation::NSString;

        let resolved = match theme.as_deref() {
            Some("dark") => Some(tauri::Theme::Dark),
            Some("light") => Some(tauri::Theme::Light),
            _ => None,
        };
        window
            .set_theme(resolved)
            .map_err(|error| error.to_string())?;

        // 再把窗口级 appearance 直接压上（"自动"清回 nil，跟随系统）。
        // 标题栏能跟随它的前提是创建时清掉了 FullSizeContentView
        // （见 macos.rs 的 strip_fullsize_content_view）。
        // AppKit 不是线程安全的：Tauri 命令跑在线程池里，离主线程的
        // setAppearance: 会被静默忽略，必须派回主线程执行。
        let ns_window = window.ns_window().map_err(|error| error.to_string())? as usize;
        window
            .app_handle()
            .run_on_main_thread(move || {
                let appearance: id = unsafe {
                    match resolved {
                        Some(tauri::Theme::Dark) => {
                            let name = NSString::alloc(nil).init_str("NSAppearanceNameDarkAqua");
                            msg_send![class!(NSAppearance), appearanceNamed: name]
                        }
                        Some(tauri::Theme::Light) => {
                            let name = NSString::alloc(nil).init_str("NSAppearanceNameAqua");
                            msg_send![class!(NSAppearance), appearanceNamed: name]
                        }
                        _ => nil,
                    }
                };
                let ns_window = ns_window as id;
                unsafe {
                    let _: () = msg_send![ns_window, setAppearance: appearance];
                }
            })
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

/// macOS 可选桌面组件由独立原生窗口承载；其它平台没有对应形态。
#[tauri::command]
fn set_macos_desktop_widget_visible(app: tauri::AppHandle, visible: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        macos::set_desktop_widget_visible(&app, visible)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, visible);
        Err("桌面组件仅用于 macOS".into())
    }
}

/// macOS 菜单栏面板在紧凑卡片与胶囊条之间切换尺寸；后端负责在改尺寸后
/// 重新锚定菜单栏图标。其它平台不调用此命令。
/// 高度按高分屏可用高留足余量（前端已按 screen.availHeight 钳过），
/// 这里只挡明显异常的值。
#[tauri::command]
fn resize_macos_panel(app: tauri::AppHandle, width: f64, height: f64) -> Result<(), String> {
    if !(48.0..=640.0).contains(&width) || !(40.0..=2400.0).contains(&height) {
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
/// 显示哪些 Agent 以后端保存的权威选择为准：设置窗口、菜单栏面板、桌面组件是
/// 各自独立的 WebView，任何窗口携带的旧列表都不能把已勾选的 Agent 抹掉；
/// 调用方只提供各 Agent 的额度值（按 id 对照，与顺序无关）。
#[tauri::command]
fn update_macos_status_items(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    agents: Vec<String>,
    remaining: Vec<Option<f64>>,
    stale: Vec<bool>,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if agents.len() != remaining.len() || agents.len() != stale.len() {
            return Err("macOS 菜单栏状态项参数长度不一致".into());
        }
        let saved = storage::open_database(&state.database_path)
            .and_then(|connection| {
                storage::get_app_setting(&connection, widget_snapshot::AGENT_FILTER_SETTING_KEY)
            })
            .unwrap_or_else(|error| {
                eprintln!("Metrik could not read its menu bar Agent selection ({error:#})");
                None
            });
        // 调用方的 agents/remaining/stale 是等长平行数组，先按 id 建值对照表，
        // 再按权威选择重排；权威选择里缺值的 Agent 按不可用显示 "--"。
        let mut values: std::collections::HashMap<&str, (Option<f64>, bool)> =
            std::collections::HashMap::new();
        for index in 0..agents.len() {
            values.insert(agents[index].as_str(), (remaining[index], stale[index]));
        }
        let (resolved, _) = widget_snapshot::resolve_agent_filter(saved.as_deref(), Some(&agents));
        let agents = resolved.unwrap_or_default();
        let remaining: Vec<Option<f64>> = agents
            .iter()
            .map(|id| values.get(id.as_str()).and_then(|value| value.0))
            .collect();
        let stale: Vec<bool> = agents
            .iter()
            .map(|id| values.get(id.as_str()).is_some_and(|value| value.1))
            .collect();
        macos::update_status_items(&app, &agents, &remaining, &stale)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, state, agents, remaining, stale);
        Err("菜单栏用量状态项仅用于 macOS".into())
    }
}

/// 托盘菜单请求完整视图；前端监听后自己完成变形（见 windowClient 的
/// onTrayShowExpanded）。macOS 的完整视图是独立窗口，走 macos.rs 自己的菜单栏。
#[cfg(all(desktop, not(target_os = "macos")))]
const TRAY_SHOW_EXPANDED: &str = "tray://show-expanded";

#[cfg(target_os = "linux")]
const TRAY_SET_PINNED: &str = "tray://set-pinned";

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
    #[cfg(target_os = "linux")]
    let pinned = MenuItem::with_id(app, "pinned", "置顶", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出 Metrik", true, None::<&str>)?;
    #[cfg(target_os = "linux")]
    let menu = Menu::with_items(app, &[&toggle, &expanded, &pinned, &separator, &quit])?;
    #[cfg(not(target_os = "linux"))]
    let menu = Menu::with_items(app, &[&toggle, &expanded, &separator, &quit])?;

    #[cfg(target_os = "linux")]
    if let Ok(mut item) = app.state::<LinuxTrayPinMenu>().item.lock() {
        *item = Some(pinned.clone());
    }

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
            #[cfg(target_os = "linux")]
            "pinned" => {
                let state = app.state::<LinuxTrayPinMenu>();
                let next = !state.pinned.fetch_xor(true, Ordering::AcqRel);
                if let Ok(item) = state.item.lock() {
                    if let Some(item) = item.as_ref() {
                        let _ = item.set_text(if next { "取消置顶" } else { "置顶" });
                    }
                }
                let _ = app.emit(TRAY_SET_PINNED, next);
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

    // 更新检查可由用户手动触发，也可按设置自动触发；不自动下载、不静默安装。
    // 更新包用项目自己的 minisign 密钥签名，防止分发链路被掉包。
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init());

    // 设置页“关于”里的仓库/邮箱链接用系统浏览器、邮件客户端打开。
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_opener::init());

    builder
        .setup(|app| {
            // macOS 是一个菜单栏应用：面板 + 独立完整视图窗口 + template 图标，
            // 与 Windows 的"单窗口变形 + 自绘按钮"完全分开。
            #[cfg(target_os = "macos")]
            macos::setup(app)?;

            #[cfg(target_os = "linux")]
            app.manage(LinuxTrayPinMenu::default());

            #[cfg(target_os = "linux")]
            restore_linux_startup_position(app.app_handle());

            #[cfg(all(desktop, not(target_os = "macos")))]
            setup_tray(app)?;

            #[cfg(windows)]
            if let Some(window) = app.get_webview_window("main") {
                if let Ok(hwnd) = window.hwnd() {
                    disable_system_corner_rounding(hwnd.0 as isize);
                }
            }

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
            #[cfg(target_os = "macos")]
            match engine::build_cached_snapshot(&database_path, "today") {
                Ok(snapshot) => {
                    // 冷启动时前端 localStorage 尚未挂载，只能使用上一次前端快照请求
                    // 同步进 app_setting 的选择。旧版本没有该设置时保留磁盘上的现有
                    // WidgetKit 快照，避免升级启动瞬间擅自变成全量 Agent。
                    let saved_agents = storage::open_database_read_only(&database_path)
                        .ok()
                        .and_then(|connection| {
                            storage::get_app_setting(
                                &connection,
                                widget_snapshot::AGENT_FILTER_SETTING_KEY,
                            )
                            .ok()
                            .flatten()
                        })
                        .and_then(|encoded| serde_json::from_str::<Vec<String>>(&encoded).ok())
                        .map(|agents| widget_snapshot::normalize_agent_filter(&agents))
                        .filter(|agents| !agents.is_empty());
                    if let Some(agents) = saved_agents {
                        if let Err(error) = widget_snapshot::persist(&snapshot, Some(&agents)) {
                            eprintln!("Metrik could not seed its WidgetKit snapshot ({error:#})");
                        }
                        // 每次启动 reload 一次：升级安装后让小组件立刻用新快照和新
                        // extension 重建时间线。每进程一次，不影响刷新配额。
                        widget_snapshot::reload_timelines();
                    }
                }
                Err(error) => {
                    eprintln!("Metrik could not read a cached WidgetKit snapshot ({error:#})");
                }
            }
            // 旧版本写坏的 statusLine 只有用户手动关一次开关才会重写，而界面显示
            // 的是「已安装」，没人会想到去切。启动时静默补一次，坏在旧版本上的人
            // 升级即恢复。不属于 Metrik 的 statusLine 不会被碰。
            match claude_hook::ClaudeHook::detected().repair() {
                Ok(true) => eprintln!("Metrik repaired a stale Claude Code statusLine hook"),
                Ok(false) => {}
                Err(error) => eprintln!(
                    "Metrik could not check the Claude Code statusLine hook ({error:#})"
                ),
            }

            app.manage(AppState {
                database_path,
                scan_gate: Arc::new(Mutex::new(())),
                quota_cache: Arc::new(Mutex::new(HashMap::new())),
                #[cfg(target_os = "linux")]
                pinned_hover_monitor: Arc::new(pinned_hover::Monitor::default()),
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
            usage_projects,
            project_rules,
            set_project_rules,
            export_csv,
            rebuild_local_ledger,
            sync_settings,
            configure_sync,
            remove_sync_device,
            claude_hook_status,
            set_claude_hook,
            claude_oauth_status,
            set_claude_oauth,
            qoder_cookie_status,
            configure_qoder_cookie,
            set_taskbar_button,
            set_tray_quota_icon,
            #[cfg(target_os = "linux")]
            linux_supports_global_window_coordinates,
            #[cfg(target_os = "linux")]
            configure_pinned_hover,
            #[cfg(target_os = "linux")]
            sync_linux_tray_pinned,
            #[cfg(target_os = "linux")]
            persist_linux_startup_position,
            set_native_theme,
            open_expanded_window,
            set_macos_desktop_widget_visible,
            resize_macos_panel,
            set_macos_agent_selection,
            get_macos_agent_selection,
            update_macos_status_items
        ])
        .run(tauri::generate_context!())
        .expect("error while running Metrik");
}

pub fn run_statusline() {
    claude_hook::run_statusline();
}

#[cfg(target_os = "macos")]
pub fn publish_widget_snapshot_from_database(database_path: &Path) -> Result<PathBuf> {
    // 走真正的扫描路径而非只读缓存：这样即便前端面板不可见（用户只看菜单栏/
    // 桌面组件），被外部定时器周期性起一次的本 helper 也能发现新产生的日志
    // 并刷新 WidgetKit snapshot。force=false 避免在旁路进程里触发配额 API 限流。
    let quota_cache: SharedQuotaCache = Arc::new(quota::QuotaCache::default());
    let snapshot = engine::build_snapshot(database_path, "today", &quota_cache, false)?;
    // 读回用户在设置里勾选并排过序的 Agent 列表（usage_snapshot command 会把它
    // 持久化到 app_setting）。CLI 旁路刷新必须带上同一份顺序，否则 Widget 会回退
    // 到 AGENT_IDS 的固定顺序，与用户在软件里的排列不一致。
    let agent_filter = storage::open_database_read_only(database_path)
        .ok()
        .and_then(|connection| {
            storage::get_app_setting(&connection, widget_snapshot::AGENT_FILTER_SETTING_KEY)
                .ok()
                .flatten()
        })
        .and_then(|encoded| serde_json::from_str::<Vec<String>>(&encoded).ok());
    widget_snapshot::persist(&snapshot, agent_filter.as_deref())
}

#[cfg(not(target_os = "macos"))]
pub fn publish_widget_snapshot_from_database(_database_path: &Path) -> Result<PathBuf> {
    anyhow::bail!("WidgetKit snapshots are only available on macOS")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_global_coordinates_are_enabled_only_for_x11_sessions() {
        assert!(linux_session_supports_global_window_coordinates(
            "x11", false, true
        ));
        assert!(linux_session_supports_global_window_coordinates(
            "", false, true
        ));
        assert!(!linux_session_supports_global_window_coordinates(
            "wayland", true, true
        ));
        assert!(!linux_session_supports_global_window_coordinates(
            "", true, true
        ));
    }

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

    #[test]
    fn tray_quota_icon_payloads_are_validated_before_use() {
        let badge = TrayQuotaIcon {
            rgba: vec![10; 32 * 32 * 4],
            width: 32,
            height: 32,
        };
        assert_eq!(validate_tray_quota_icon(&badge), Ok(()));

        let truncated = TrayQuotaIcon {
            rgba: vec![10; 100],
            width: 32,
            height: 32,
        };
        assert!(validate_tray_quota_icon(&truncated)
            .unwrap_err()
            .contains("长度不匹配"));

        let oversize = TrayQuotaIcon {
            rgba: vec![0; (TRAY_ICON_MAX_EDGE as usize + 1) * 4],
            width: TRAY_ICON_MAX_EDGE + 1,
            height: 1,
        };
        assert!(validate_tray_quota_icon(&oversize)
            .unwrap_err()
            .contains("尺寸超出范围"));

        let empty = TrayQuotaIcon {
            rgba: Vec::new(),
            width: 0,
            height: 0,
        };
        assert!(validate_tray_quota_icon(&empty).is_err());
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
