use crate::domain::{stable_hash, SyncDeviceView, SyncView};
use crate::storage::{get_app_setting as get_setting, set_app_setting as set_setting};
use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const SETTING_SYNC_DIR: &str = "sync.dir";
const SETTING_DEVICE_ID: &str = "sync.device_id";
const SETTING_DEVICE_LABEL: &str = "sync.device_label";
const SETTING_LAST_EXPORT_MS: &str = "sync.last_export_ms";
const SETTING_LAST_ERROR: &str = "sync.last_error";

const EXPORT_FORMAT_VERSION: i64 = 1;
const EXPORT_HORIZON_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const SYNC_INTERVAL_MS: i64 = 5 * 60 * 1000;
const MAX_IMPORT_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// 单条远端事件的 token 上限，远超任何真实请求；挡住畸形值让 SQLite 求和溢出。
const MAX_EVENT_TOKENS: i64 = 10_000_000_000;

/// 同步导出只包含派生统计字段：事件标识、Agent、时间与处理量。
/// 对话正文、Prompt、模型输出、凭据与源文件路径都不在导出边界内。
#[derive(Serialize, Deserialize)]
struct ExportFile {
    version: i64,
    #[serde(rename = "deviceId")]
    device_id: String,
    label: String,
    #[serde(rename = "exportedAtMs")]
    exported_at_ms: i64,
    events: Vec<ExportEvent>,
}

#[derive(Serialize, Deserialize)]
struct ExportEvent {
    id: String,
    agent: String,
    at: i64,
    tokens: i64,
}

fn delete_setting(connection: &Connection, key: &str) -> Result<()> {
    connection.execute("DELETE FROM app_setting WHERE key = ?1", [key])?;
    Ok(())
}

/// 旧兜底名。`HOSTNAME` 是 shell 变量，GUI 启动的 macOS 应用拿不到，于是
/// 每一台 mac 都叫这个名字——列表里一排「此设备」，而且恰恰没有一台是本机。
/// 保留常量是为了识别并修掉已经存下来的那些。
const LEGACY_FALLBACK_LABEL: &str = "此设备";
const FALLBACK_LABEL: &str = "未命名设备";

/// 主机名里可展示的那一段：去掉域名后缀（macOS 常见的 `.local`），
/// 空串一律当作没拿到。
fn short_hostname(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let head = trimmed.split('.').next().unwrap_or("").trim();
    (!head.is_empty()).then(|| head.to_owned())
}

/// unix 的主机名走 `gethostname(3)`：环境变量在 GUI 进程里不可靠，而这个
/// 系统调用不依赖 shell。
#[cfg(unix)]
fn system_hostname() -> Option<String> {
    // 256 足够容纳 POSIX 的 HOST_NAME_MAX（Linux 64、macOS 255）。
    let mut buffer = [0_u8; 256];
    // SAFETY: 传入自有缓冲区与其真实长度；失败返回非零，此时不读缓冲区。
    let code = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if code != 0 {
        return None;
    }
    // 找不到 NUL 说明名字塞满了缓冲区、可能被截断（256 > HOST_NAME_MAX，
    // 实际到不了）。宁可当作没拿到，也不拿一个残缺的名字当设备名。
    let end = buffer.iter().position(|byte| *byte == 0).unwrap_or(0);
    short_hostname(&String::from_utf8_lossy(&buffer[..end]))
}

#[cfg(not(unix))]
fn system_hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .ok()
        .as_deref()
        .and_then(short_hostname)
}

fn device_label() -> String {
    system_hostname().unwrap_or_else(|| FALLBACK_LABEL.into())
}

/// 返回（并在首次调用时生成）本机的稳定设备标识与名称。
pub fn device_identity(connection: &Connection) -> Result<(String, String)> {
    let label = match get_setting(connection, SETTING_DEVICE_LABEL)? {
        // 旧版本在 macOS 上一律存成「此设备」，而这里只在缺失时才写，所以那个
        // 名字会一直留着。能算出真名字就顶掉它——只认这一个旧兜底值，用户
        // 自己改过的名字不碰。
        Some(value) if value == LEGACY_FALLBACK_LABEL => match system_hostname() {
            Some(hostname) => {
                set_setting(connection, SETTING_DEVICE_LABEL, &hostname)?;
                hostname
            }
            None => value,
        },
        Some(value) => value,
        None => {
            let value = device_label();
            set_setting(connection, SETTING_DEVICE_LABEL, &value)?;
            value
        }
    };
    let id = match get_setting(connection, SETTING_DEVICE_ID)? {
        Some(value) => value,
        None => {
            let seed = format!(
                "{label}|{}|{}",
                std::process::id(),
                Utc::now().timestamp_nanos_opt().unwrap_or_default()
            );
            let value = stable_hash(&seed)[..16].to_owned();
            set_setting(connection, SETTING_DEVICE_ID, &value)?;
            value
        }
    };
    Ok((id, label))
}

pub fn sync_directory(connection: &Connection) -> Result<Option<PathBuf>> {
    Ok(get_setting(connection, SETTING_SYNC_DIR)?.map(PathBuf::from))
}

fn export_file_name(device_id: &str) -> String {
    format!("metrik-usage-{device_id}.json")
}

/// 设备标识来自共享目录里的文件内容，会被拼进删除路径，只接受字母数字。
fn is_valid_device_id(device_id: &str) -> bool {
    !device_id.is_empty()
        && device_id.len() <= 64
        && device_id.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// 校验并保存同步目录；传 `None` 时关闭同步并清空已合并的远端统计，
/// 避免不再刷新的数字冒充当前值。原始 Agent 日志与本机账本不受影响。
pub fn configure(connection: &mut Connection, directory: Option<String>) -> Result<SyncView> {
    match directory {
        Some(raw) => {
            let dir = PathBuf::from(raw.trim());
            if !dir.is_absolute() {
                bail!("同步目录必须是绝对路径");
            }
            if !dir.is_dir() {
                bail!("同步目录不存在或不是文件夹");
            }
            let probe = dir.join(format!(".metrik-probe-{}", std::process::id()));
            std::fs::write(&probe, b"metrik").context("同步目录不可写入")?;
            let _ = std::fs::remove_file(&probe);
            set_setting(connection, SETTING_SYNC_DIR, &dir.to_string_lossy())?;
            delete_setting(connection, SETTING_LAST_EXPORT_MS)?;
            delete_setting(connection, SETTING_LAST_ERROR)?;
            run_sync(connection, Utc::now().timestamp_millis());
        }
        None => {
            let (device_id, _) = device_identity(connection)?;
            if let Some(dir) = sync_directory(connection)? {
                let _ = std::fs::remove_file(dir.join(export_file_name(&device_id)));
            }
            let transaction = connection.transaction()?;
            transaction.execute_batch(
                "DELETE FROM remote_usage_event;
                 DELETE FROM sync_device;",
            )?;
            transaction.execute(
                "DELETE FROM app_setting WHERE key IN (?1, ?2, ?3)",
                params![SETTING_SYNC_DIR, SETTING_LAST_EXPORT_MS, SETTING_LAST_ERROR],
            )?;
            transaction.commit()?;
        }
    }
    sync_view(connection)
}

/// 删除一台远端设备：清空它的同步事件与设备记录，并移除共享文件夹里
/// 它的导出文件。只删本地行不删文件的话，下次 `run_sync` 会因为
/// `sync_device.exported_at_ms` 已不存在而重新导入同一份文件，删除形同无效。
/// 本机设备（自身）不接受删除——它不在"已发现的设备"列表里，这里只做防御性校验。
pub fn remove_device(connection: &mut Connection, device_id: &str) -> Result<SyncView> {
    let (own_device_id, _) = device_identity(connection)?;
    if device_id == own_device_id {
        bail!("无法删除本机设备");
    }
    if !is_valid_device_id(device_id) {
        bail!("设备标识无效");
    }

    if let Some(dir) = sync_directory(connection)? {
        let _ = std::fs::remove_file(dir.join(export_file_name(device_id)));
    }

    let transaction = connection.transaction()?;
    transaction.execute(
        "DELETE FROM remote_usage_event WHERE device_id = ?1",
        [device_id],
    )?;
    transaction.execute("DELETE FROM sync_device WHERE device_id = ?1", [device_id])?;
    transaction.commit()?;

    sync_view(connection)
}

pub fn sync_view(connection: &Connection) -> Result<SyncView> {
    let (device_id, device_label) = device_identity(connection)?;
    let directory = sync_directory(connection)?;
    let last_export_ms = get_setting(connection, SETTING_LAST_EXPORT_MS)?
        .and_then(|value| value.parse::<i64>().ok());
    let last_error = get_setting(connection, SETTING_LAST_ERROR)?;

    let mut statement = connection.prepare(
        "SELECT d.device_id, d.label, d.exported_at_ms, d.last_import_ms,
                (SELECT COUNT(*) FROM remote_usage_event r WHERE r.device_id = d.device_id)
         FROM sync_device d
         ORDER BY d.label, d.device_id",
    )?;
    let devices = statement
        .query_map([], |row| {
            Ok(SyncDeviceView {
                id: row.get(0)?,
                label: row.get(1)?,
                exported_at_ms: row.get(2)?,
                last_import_ms: row.get(3)?,
                events: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(SyncView {
        enabled: directory.is_some(),
        directory: directory.map(|value| value.to_string_lossy().into_owned()),
        device_id,
        device_label,
        last_export_ms,
        last_error,
        devices,
    })
}

/// 尽力而为地执行一轮导出 + 导入。任何失败都不会中断统计流程，
/// 只把摘要写进 `sync.last_error` 供界面显示。
pub fn run_sync(connection: &mut Connection, now_ms: i64) {
    let enabled = match sync_directory(connection) {
        Ok(Some(dir)) => dir,
        _ => return,
    };
    let throttled = get_setting(connection, SETTING_LAST_EXPORT_MS)
        .ok()
        .flatten()
        .and_then(|value| value.parse::<i64>().ok())
        .is_some_and(|last| now_ms - last < SYNC_INTERVAL_MS);
    if throttled {
        return;
    }

    let mut failures = Vec::new();
    if let Err(error) = export_local_events(connection, &enabled, now_ms) {
        failures.push(format!("导出失败：{error:#}"));
    }
    if let Err(error) = import_remote_files(connection, &enabled, now_ms) {
        failures.push(format!("导入失败：{error:#}"));
    }

    let _ = set_setting(connection, SETTING_LAST_EXPORT_MS, &now_ms.to_string());
    if failures.is_empty() {
        let _ = delete_setting(connection, SETTING_LAST_ERROR);
    } else {
        let _ = set_setting(connection, SETTING_LAST_ERROR, &failures.join("；"));
    }
}

fn export_local_events(connection: &Connection, dir: &Path, now_ms: i64) -> Result<()> {
    let (device_id, label) = device_identity(connection)?;
    let mut statement = connection.prepare(
        // Cursor 的事件是账号级的，每台开启的设备都拉到同一份，导出会重复计数。
        "SELECT event_id, adapter_id, occurred_at_ms, processed_tokens
         FROM usage_event WHERE occurred_at_ms >= ?1 AND adapter_id != 'cursor'
         ORDER BY occurred_at_ms",
    )?;
    let events = statement
        .query_map([now_ms - EXPORT_HORIZON_MS], |row| {
            Ok(ExportEvent {
                id: row.get(0)?,
                agent: row.get(1)?,
                at: row.get(2)?,
                tokens: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let payload = serde_json::to_string(&ExportFile {
        version: EXPORT_FORMAT_VERSION,
        device_id: device_id.clone(),
        label,
        exported_at_ms: now_ms,
        events,
    })?;
    let target = dir.join(export_file_name(&device_id));
    let staged = dir.join(format!(
        ".{}.tmp-{}",
        export_file_name(&device_id),
        std::process::id()
    ));
    std::fs::write(&staged, payload)
        .with_context(|| format!("failed to stage sync export {}", staged.display()))?;
    let installed = std::fs::rename(&staged, &target);
    if installed.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    installed.with_context(|| format!("failed to install sync export {}", target.display()))
}

fn import_remote_files(connection: &mut Connection, dir: &Path, now_ms: i64) -> Result<()> {
    let (own_device_id, _) = device_identity(connection)?;
    let mut failures = 0_usize;
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read sync directory {}", dir.display()))?;

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !name.starts_with("metrik-usage-") || !name.ends_with(".json") {
            continue;
        }
        if path
            .metadata()
            .map(|meta| meta.len() > MAX_IMPORT_FILE_BYTES)
            .unwrap_or(true)
        {
            failures += 1;
            continue;
        }
        match import_one_file(connection, &path, &own_device_id, now_ms) {
            Ok(()) => {}
            Err(_) => failures += 1,
        }
    }

    if failures > 0 {
        bail!("{failures} 个设备导出文件未能读取或解析");
    }
    Ok(())
}

fn import_one_file(
    connection: &mut Connection,
    path: &Path,
    own_device_id: &str,
    now_ms: i64,
) -> Result<()> {
    let raw = std::fs::read_to_string(path)?;
    let file: ExportFile = serde_json::from_str(&raw)?;
    if file.version != EXPORT_FORMAT_VERSION {
        bail!("unsupported sync export version {}", file.version);
    }
    if file.device_id == own_device_id {
        return Ok(());
    }
    // 设备身份绑定文件名：删除设备时按标识拼出的文件名删文件。
    if !is_valid_device_id(&file.device_id)
        || path.file_name().and_then(|name| name.to_str())
            != Some(export_file_name(&file.device_id).as_str())
    {
        bail!("sync export device id does not match its file name");
    }

    let already_imported: Option<i64> = connection
        .query_row(
            "SELECT exported_at_ms FROM sync_device WHERE device_id = ?1",
            [&file.device_id],
            |row| row.get(0),
        )
        .optional()?;
    if already_imported == Some(file.exported_at_ms) {
        return Ok(());
    }

    let transaction = connection.transaction()?;
    transaction.execute(
        "DELETE FROM remote_usage_event WHERE device_id = ?1",
        [&file.device_id],
    )?;
    {
        let mut insert = transaction.prepare(
            "INSERT OR REPLACE INTO remote_usage_event (
                device_id, event_id, adapter_id, occurred_at_ms, processed_tokens
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for event in &file.events {
            if !(0..=MAX_EVENT_TOKENS).contains(&event.tokens) {
                continue;
            }
            insert.execute(params![
                file.device_id,
                event.id,
                event.agent,
                event.at,
                event.tokens
            ])?;
        }
    }
    transaction.execute(
        "INSERT INTO sync_device (device_id, label, exported_at_ms, last_import_ms)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(device_id) DO UPDATE SET
            label = excluded.label,
            exported_at_ms = excluded.exported_at_ms,
            last_import_ms = excluded.last_import_ms",
        params![file.device_id, file.label, file.exported_at_ms, now_ms],
    )?;
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "metrik-sync-{label}-{}-{}",
                std::process::id(),
                Utc::now().timestamp_nanos_opt().unwrap_or_default()
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

    fn open_test_db() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        connection
    }

    fn insert_local_event(connection: &Connection, event_id: &str, at_ms: i64, tokens: i64) {
        connection
            .execute(
                "INSERT INTO usage_event (
                    event_id, adapter_id, event_key, occurred_at_ms, session_id,
                    model, input_uncached_tokens, cache_read_tokens, cache_write_tokens,
                    output_tokens, reasoning_tokens, processed_tokens, quality, payload_hash
                 ) VALUES (?1, 'codex', ?1, ?2, 'session', NULL, ?3, 0, 0, 0, 0, ?3, 'exact', ?1)",
                params![event_id, at_ms, tokens],
            )
            .unwrap();
    }

    #[test]
    fn device_identity_is_created_once_and_stays_stable() {
        let connection = open_test_db();
        let first = device_identity(&connection).unwrap();
        let second = device_identity(&connection).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.0.len(), 16);
    }

    #[test]
    fn hostname_drops_the_domain_suffix_and_rejects_empty() {
        assert_eq!(
            short_hostname("guoxiaoyudeMac-mini.local").as_deref(),
            Some("guoxiaoyudeMac-mini")
        );
        assert_eq!(short_hostname("  KEROS-PC  ").as_deref(), Some("KEROS-PC"));
        assert_eq!(short_hostname("a.b.c").as_deref(), Some("a"));
        assert!(short_hostname("").is_none());
        assert!(short_hostname("   ").is_none());
        assert!(short_hostname(".local").is_none());
    }

    /// 本机能拿到主机名时，旧版在 macOS 上存下的「此设备」要被顶掉；
    /// 用户自己改过的名字不受影响。
    #[test]
    fn the_legacy_fallback_label_is_repaired_but_a_custom_name_is_kept() {
        let connection = open_test_db();
        set_setting(&connection, SETTING_DEVICE_LABEL, LEGACY_FALLBACK_LABEL).unwrap();
        let (_, label) = device_identity(&connection).unwrap();
        match system_hostname() {
            Some(hostname) => {
                assert_eq!(label, hostname);
                assert_ne!(label, LEGACY_FALLBACK_LABEL);
            }
            // 拿不到主机名时不能把名字弄丢，原样留着。
            None => assert_eq!(label, LEGACY_FALLBACK_LABEL),
        }

        let connection = open_test_db();
        set_setting(&connection, SETTING_DEVICE_LABEL, "书房那台").unwrap();
        assert_eq!(device_identity(&connection).unwrap().1, "书房那台");
    }

    #[test]
    fn export_and_import_round_trip_between_two_devices() {
        let shared = TestDirectory::new("roundtrip");
        let now = Utc::now().timestamp_millis();

        let device_a = open_test_db();
        set_setting(
            &device_a,
            SETTING_SYNC_DIR,
            &shared.path().to_string_lossy(),
        )
        .unwrap();
        insert_local_event(&device_a, "event-a", now - 1_000, 111);
        let mut device_a = device_a;
        run_sync(&mut device_a, now);

        let mut device_b = open_test_db();
        set_setting(
            &device_b,
            SETTING_SYNC_DIR,
            &shared.path().to_string_lossy(),
        )
        .unwrap();
        insert_local_event(&device_b, "event-b", now - 2_000, 222);
        run_sync(&mut device_b, now);

        let remote_total: i64 = device_b
            .query_row(
                "SELECT COALESCE(SUM(processed_tokens), 0) FROM remote_usage_event",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remote_total, 111);

        let view = sync_view(&device_b).unwrap();
        assert!(view.enabled);
        assert_eq!(view.devices.len(), 1);
        assert_eq!(view.devices[0].events, 1);
        assert!(view.last_error.is_none());

        // 设备 A 之后也能看到设备 B 的导出。
        let later = now + SYNC_INTERVAL_MS + 1;
        run_sync(&mut device_a, later);
        let remote_on_a: i64 = device_a
            .query_row(
                "SELECT COALESCE(SUM(processed_tokens), 0) FROM remote_usage_event",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remote_on_a, 222);
    }

    #[test]
    fn account_wide_cursor_events_are_not_exported() {
        let shared = TestDirectory::new("cursor");
        let now = Utc::now().timestamp_millis();

        let mut device_a = open_test_db();
        set_setting(
            &device_a,
            SETTING_SYNC_DIR,
            &shared.path().to_string_lossy(),
        )
        .unwrap();
        insert_local_event(&device_a, "event-a", now - 1_000, 111);
        device_a
            .execute(
                "UPDATE usage_event SET adapter_id = 'cursor' WHERE event_id = 'event-a'",
                [],
            )
            .unwrap();
        insert_local_event(&device_a, "event-b", now - 2_000, 222);
        run_sync(&mut device_a, now);

        let mut device_b = open_test_db();
        set_setting(
            &device_b,
            SETTING_SYNC_DIR,
            &shared.path().to_string_lossy(),
        )
        .unwrap();
        run_sync(&mut device_b, now);

        let remote_total: i64 = device_b
            .query_row(
                "SELECT COALESCE(SUM(processed_tokens), 0) FROM remote_usage_event",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remote_total, 222);
    }

    #[test]
    fn reimport_of_unchanged_export_is_skipped_and_updates_replace_old_rows() {
        let shared = TestDirectory::new("replace");
        let now = Utc::now().timestamp_millis();
        let mut local = open_test_db();
        set_setting(&local, SETTING_SYNC_DIR, &shared.path().to_string_lossy()).unwrap();

        let remote_file = shared.path().join("metrik-usage-remote01.json");
        fs::write(
            &remote_file,
            r#"{"version":1,"deviceId":"remote01","label":"laptop","exportedAtMs":1000,
                "events":[{"id":"r1","agent":"claude","at":900,"tokens":10},
                          {"id":"r2","agent":"claude","at":901,"tokens":20}]}"#,
        )
        .unwrap();
        run_sync(&mut local, now);
        let count: i64 = local
            .query_row("SELECT COUNT(*) FROM remote_usage_event", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 2);

        // 同一 exportedAtMs 不重复导入；新导出替换全部旧行。
        fs::write(
            &remote_file,
            r#"{"version":1,"deviceId":"remote01","label":"laptop","exportedAtMs":2000,
                "events":[{"id":"r1","agent":"claude","at":900,"tokens":15}]}"#,
        )
        .unwrap();
        run_sync(&mut local, now + SYNC_INTERVAL_MS + 1);
        let rows: Vec<(String, i64)> = local
            .prepare("SELECT event_id, processed_tokens FROM remote_usage_event")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows, vec![("r1".into(), 15)]);
    }

    #[test]
    fn malformed_remote_file_sets_error_without_blocking_other_imports() {
        let shared = TestDirectory::new("malformed");
        let now = Utc::now().timestamp_millis();
        let mut local = open_test_db();
        set_setting(&local, SETTING_SYNC_DIR, &shared.path().to_string_lossy()).unwrap();
        fs::write(shared.path().join("metrik-usage-bad.json"), "{broken").unwrap();
        fs::write(
            shared.path().join("metrik-usage-good.json"),
            r#"{"version":1,"deviceId":"good","label":"ok","exportedAtMs":1,
                "events":[{"id":"g1","agent":"codex","at":1,"tokens":5}]}"#,
        )
        .unwrap();

        run_sync(&mut local, now);

        let view = sync_view(&local).unwrap();
        assert!(view.last_error.is_some());
        let imported: i64 = local
            .query_row(
                "SELECT COUNT(*) FROM remote_usage_event WHERE device_id = 'good'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(imported, 1);
    }

    /// 共享目录里的文件不可信：伪造的设备标识不能借"移除设备"删到目录外，
    /// 畸形 token 值不能进入账本。
    #[test]
    fn untrusted_remote_files_cannot_escape_the_sync_directory() {
        let shared = TestDirectory::new("untrusted");
        let now = Utc::now().timestamp_millis();
        let mut local = open_test_db();
        set_setting(&local, SETTING_SYNC_DIR, &shared.path().to_string_lossy()).unwrap();
        fs::write(
            shared.path().join("metrik-usage-evil.json"),
            r#"{"version":1,"deviceId":"/../../victim","label":"x","exportedAtMs":1,
                "events":[{"id":"e1","agent":"codex","at":1,"tokens":5}]}"#,
        )
        .unwrap();
        fs::write(
            shared.path().join("metrik-usage-huge.json"),
            r#"{"version":1,"deviceId":"huge","label":"x","exportedAtMs":1,
                "events":[{"id":"h1","agent":"codex","at":1,"tokens":9223372036854775807},
                          {"id":"h2","agent":"codex","at":1,"tokens":7}]}"#,
        )
        .unwrap();

        run_sync(&mut local, now);

        let rows: Vec<(String, i64)> = local
            .prepare("SELECT device_id, processed_tokens FROM remote_usage_event")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows, vec![("huge".into(), 7)]);
        assert!(sync_view(&local).unwrap().last_error.is_some());
        assert!(remove_device(&mut local, "/../../victim").is_err());
    }

    #[test]
    fn disabling_sync_clears_merged_remote_statistics_and_own_export() {
        let shared = TestDirectory::new("disable");
        let now = Utc::now().timestamp_millis();
        let mut local = open_test_db();
        insert_local_event(&local, "mine", now, 7);
        configure(
            &mut local,
            Some(shared.path().to_string_lossy().into_owned()),
        )
        .unwrap();
        let (device_id, _) = device_identity(&local).unwrap();
        assert!(shared.path().join(export_file_name(&device_id)).exists());

        let view = configure(&mut local, None).unwrap();

        assert!(!view.enabled);
        assert!(view.devices.is_empty());
        assert!(!shared.path().join(export_file_name(&device_id)).exists());
        let remote: i64 = local
            .query_row("SELECT COUNT(*) FROM remote_usage_event", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(remote, 0);
        // 本机账本不受影响。
        let mine: i64 = local
            .query_row("SELECT COUNT(*) FROM usage_event", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mine, 1);
    }

    #[test]
    fn remove_device_clears_rows_and_export_file() {
        let shared = TestDirectory::new("remove");
        let now = Utc::now().timestamp_millis();

        let mut device_a = open_test_db();
        set_setting(
            &device_a,
            SETTING_SYNC_DIR,
            &shared.path().to_string_lossy(),
        )
        .unwrap();
        insert_local_event(&device_a, "event-a", now - 1_000, 111);
        let (device_a_id, _) = device_identity(&device_a).unwrap();
        run_sync(&mut device_a, now);
        let export_path = shared.path().join(export_file_name(&device_a_id));
        assert!(export_path.exists());

        let mut device_b = open_test_db();
        set_setting(
            &device_b,
            SETTING_SYNC_DIR,
            &shared.path().to_string_lossy(),
        )
        .unwrap();
        run_sync(&mut device_b, now);
        assert_eq!(sync_view(&device_b).unwrap().devices.len(), 1);

        // B 删除 A：本地行清空，共享文件夹里的 A 导出文件一并移除，
        // 否则下次同步会原样重新导入。
        let view = remove_device(&mut device_b, &device_a_id).unwrap();
        assert!(view.devices.is_empty());
        let remote: i64 = device_b
            .query_row("SELECT COUNT(*) FROM remote_usage_event", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(remote, 0);
        assert!(!export_path.exists());
    }

    #[test]
    fn remove_device_rejects_self() {
        let mut local = open_test_db();
        let (own_id, _) = device_identity(&local).unwrap();
        let error = remove_device(&mut local, &own_id).unwrap_err().to_string();
        assert!(error.contains("本机设备"));
    }

    #[test]
    fn configure_rejects_a_missing_directory() {
        let mut local = open_test_db();
        let missing = std::env::temp_dir().join("metrik-sync-definitely-missing-dir");
        let error =
            configure(&mut local, Some(missing.to_string_lossy().into_owned())).unwrap_err();
        assert!(error.to_string().contains("不存在"));
    }
}
