mod adapters;
mod app_server;
mod domain;
mod engine;
mod schema;
mod storage;

use anyhow::{Context, Result};
use domain::{QuotaSample, UsageSnapshot};
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{Manager, State};

type SharedQuotaCache = Arc<Mutex<Option<(Instant, Vec<QuotaSample>)>>>;

const DATABASE_FILE_NAME: &str = "metrik.sqlite3";
const RECOVERY_DATABASE_FILE_NAME: &str = "metrik.recovery.sqlite3";
const SQLITE_SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];
static MIGRATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct AppState {
    database_path: PathBuf,
    scan_gate: Arc<Mutex<()>>,
    quota_cache: SharedQuotaCache,
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
    state: State<'_, AppState>,
) -> Result<UsageSnapshot, String> {
    let database_path = state.database_path.clone();
    let scan_gate = Arc::clone(&state.scan_gate);
    let quota_cache = Arc::clone(&state.quota_cache);

    tauri::async_runtime::spawn_blocking(move || {
        let _gate = scan_gate
            .lock()
            .map_err(|_| "usage scan lock poisoned".to_owned())?;
        engine::build_snapshot(&database_path, &period, &quota_cache)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("usage scan task failed: {error}"))?
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
        engine::build_snapshot(&database_path, &period, &quota_cache)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("local ledger rebuild task failed: {error}"))?
}

#[cfg(desktop)]
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

#[cfg(desktop)]
fn setup_tray(app: &mut tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let toggle = MenuItem::with_id(app, "toggle", "显示 / 隐藏", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 Metrik", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &quit])?;

    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Metrik")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "toggle" => toggle_main_window(app),
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
    let builder = tauri::Builder::default();

    #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }));

    builder
        .setup(|app| {
            // macOS 下只保留右上角菜单栏图标，不占用 Dock。
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            #[cfg(desktop)]
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
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭时收进托盘常驻，退出走托盘菜单。
            #[cfg(desktop)]
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            usage_snapshot,
            rebuild_local_ledger
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
