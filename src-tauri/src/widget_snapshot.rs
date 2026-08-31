//! Sanitised data bridge for the native macOS WidgetKit extension.
//!
//! The widget never reads Metrik's SQLite ledger directly. The host app publishes a
//! compact, versioned JSON snapshot containing only derived totals and official quota
//! metadata. The signed publisher stores those JSON bytes through the Widget extension's
//! standard preferences domain, rather than making the extension open a shared file directly.
//! This keeps storage ownership in the shared core and gives WidgetKit a stable contract
//! that can evolve independently from the database schema.

use crate::domain::UsageSnapshot;
use anyhow::{Context, Result};
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const SNAPSHOT_FILE_NAME: &str = "widget-snapshot.json";
pub const AGENT_FILTER_SETTING_KEY: &str = "macos_widget_agents";

pub fn normalize_agent_filter(agent_filter: &[String]) -> Vec<String> {
    agent_filter.iter().fold(Vec::new(), |mut selected, id| {
        if crate::domain::AGENT_IDS.contains(&id.as_str()) && !selected.contains(id) {
            selected.push(id.clone());
        }
        selected
    })
}

/// Resolve the single authoritative macOS selection. Once a valid saved value exists,
/// a window-provided value is only a stale rendering hint and must never replace it.
/// The boolean says whether the caller should persist a one-time initial selection.
pub fn resolve_agent_filter(
    saved: Option<&str>,
    requested: Option<&[String]>,
) -> (Option<Vec<String>>, bool) {
    let saved = saved
        .and_then(|encoded| serde_json::from_str::<Vec<String>>(encoded).ok())
        .map(|agents| normalize_agent_filter(&agents))
        .filter(|agents| !agents.is_empty());
    if saved.is_some() {
        return (saved, false);
    }
    let initial = requested
        .map(normalize_agent_filter)
        .filter(|agents| !agents.is_empty());
    let should_persist = initial.is_some();
    (initial, should_persist)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WidgetSnapshot<'a> {
    schema_version: u8,
    generated_at: &'a str,
    total_tokens: i64,
    agents: Vec<WidgetAgent<'a>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WidgetAgent<'a> {
    id: &'a str,
    label: &'static str,
    tokens: i64,
    windows: Vec<WidgetQuotaWindow<'a>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WidgetQuotaWindow<'a> {
    key: &'a str,
    label: &'a str,
    available: bool,
    remaining_percent: f64,
    resets_in_minutes: Option<f64>,
    stale: bool,
    reset_expired: bool,
    quality: &'a str,
}

fn agent_label(id: &str) -> &'static str {
    match id {
        "codex" => "ChatGPT",
        "claude" => "Claude",
        "zcode" => "GLM",
        "opencode" => "OpenCode",
        "kimi" => "Kimi",
        "antigravity" => "Antigravity",
        "workbuddy" => "WorkBuddy",
        "qoder" => "Qoder",
        "pi" => "Pi",
        "qwen" => "Qwen",
        "hermes" => "Hermes",
        _ => "Agent",
    }
}

fn make_payload<'a>(
    snapshot: &'a UsageSnapshot,
    agent_filter: Option<&[String]>,
) -> WidgetSnapshot<'a> {
    // agent_filter 是用户在设置里勾选的小组件 Agent（顺序即显示顺序）。
    // None 表示不过滤（启动播种、CLI 导出）；Some 必须严格匹配选择，不能在
    // 空值或未知值时擅自回退成全量。前端设置本身保证正常交互至少保留一项。
    let selected: Vec<&crate::domain::AgentSummary> = match agent_filter {
        Some(filter) => normalize_agent_filter(filter)
            .iter()
            .filter_map(|id| snapshot.agents.iter().find(|agent| &agent.id == id))
            .collect(),
        None => snapshot.agents.iter().collect(),
    };
    let agents = selected
        .iter()
        .map(|agent| {
            let windows = snapshot
                .agent_quotas
                .iter()
                .find(|quota| quota.agent == agent.id)
                .map(|quota| {
                    quota
                        .windows
                        .iter()
                        .map(|window| WidgetQuotaWindow {
                            key: &window.key,
                            label: &window.label,
                            available: window.view.available,
                            remaining_percent: window.view.remaining_percent,
                            resets_in_minutes: window.view.resets_in_minutes,
                            stale: window.view.stale,
                            reset_expired: window.view.reset_expired,
                            quality: &window.view.quality,
                        })
                        .collect()
                })
                .unwrap_or_default();
            WidgetAgent {
                id: &agent.id,
                label: agent_label(&agent.id),
                tokens: agent.tokens,
                windows,
            }
        })
        .collect();

    WidgetSnapshot {
        schema_version: 1,
        generated_at: &snapshot.generated_at,
        total_tokens: snapshot.total_tokens,
        agents,
    }
}

fn publisher_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("METRIK_WIDGET_PUBLISHER") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }

    let executable = std::env::current_exe().ok()?;
    let contents = executable.parent().and_then(Path::parent)?;
    let helper = contents.join("Helpers").join("metrik-widget-publish");
    helper.is_file().then_some(helper)
}

fn publish_with_helper(helper: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let mut child = Command::new(helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot start WidgetKit publisher {}", helper.display()))?;
    child
        .stdin
        .take()
        .context("WidgetKit publisher stdin is unavailable")?
        .write_all(bytes)
        .context("cannot send the snapshot to the WidgetKit publisher")?;
    let output = child
        .wait_with_output()
        .context("cannot wait for the WidgetKit publisher")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("WidgetKit publisher failed: {}", detail.trim());
    }
    let path = String::from_utf8(output.stdout)
        .context("WidgetKit publisher returned a non-UTF-8 path")?;
    let path = PathBuf::from(path.trim());
    if path.as_os_str().is_empty() {
        anyhow::bail!("WidgetKit publisher returned an empty path");
    }
    Ok(path)
}

fn fallback_directory() -> Result<PathBuf> {
    let base = dirs::data_dir().context("cannot locate the application support directory")?;
    Ok(base.join("Metrik").join("Widget Preview"))
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("widget snapshot has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("cannot create widget group directory {}", parent.display()))?;

    let temporary = parent.join(format!(".{SNAPSHOT_FILE_NAME}.{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("cannot create widget snapshot {}", temporary.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("cannot write widget snapshot {}", temporary.display()))?;
    file.sync_all()
        .with_context(|| format!("cannot sync widget snapshot {}", temporary.display()))?;
    drop(file);

    fs::rename(&temporary, path)
        .with_context(|| format!("cannot publish widget snapshot {}", path.display()))?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("cannot secure widget snapshot {}", path.display()))?;
    Ok(())
}

pub fn persist(snapshot: &UsageSnapshot, agent_filter: Option<&[String]>) -> Result<PathBuf> {
    let bytes = serde_json::to_vec(&make_payload(snapshot, agent_filter))?;
    let path = if let Some(helper) = publisher_path() {
        publish_with_helper(&helper, &bytes)?
    } else {
        // Cargo tests and unbundled development builds have no signed helper.
        // Keep a readable preview copy without pretending it is an App Group.
        let path = fallback_directory()?.join(SNAPSHOT_FILE_NAME);
        write_atomically(&path, &bytes)?;
        path
    };
    // 这里刻意不 reload 时间线：persist 挂在数据轮询上（索引期可达每秒数次），
    // 而 reloadTimelines 消耗 WidgetKit 的应用级刷新配额，配额耗尽后 chronod 会把
    // 所有刷新推迟一小时以上，小组件反而冻在旧快照。常规刷新由时间线策略
    // （.after(5min)，不占配额）完成；即时 reload 只在用户显式改勾选时触发。
    Ok(path)
}

/// 用户显式更改 Agent 选择后调用，让小组件立刻反映新选择。次数受用户操作频率
/// 限制，不会耗尽 WidgetKit 刷新配额。
pub fn reload_timelines() {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let Some(contents) = executable.parent().and_then(Path::parent) else {
        return;
    };
    let helper = contents.join("Helpers").join("metrik-widget-reload");
    if helper.is_file() {
        let _ = std::process::Command::new(helper).status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentSummary, CostSummary, IndexingView};

    #[test]
    fn labels_match_the_public_agent_names() {
        assert_eq!(agent_label("codex"), "ChatGPT");
        assert_eq!(agent_label("zcode"), "GLM");
        assert_eq!(agent_label("opencode"), "OpenCode");
        assert_eq!(agent_label("pi"), "Pi");
    }

    fn snapshot_with_agents(ids: &[&str]) -> UsageSnapshot {
        UsageSnapshot {
            generated_at: "2026-08-12T20:00:00Z".to_owned(),
            period: "today".to_owned(),
            is_demo: false,
            total_tokens: ids.len() as i64,
            comparison_percent: 0.0,
            comparison_available: false,
            series: Vec::new(),
            agent_quotas: Vec::new(),
            agents: ids
                .iter()
                .map(|id| AgentSummary {
                    id: (*id).to_owned(),
                    tokens: 1,
                    input_uncached: 1,
                    cache_read: 0,
                    cache_write: 0,
                    output: 0,
                    share: 0.0,
                    detected: true,
                })
                .collect(),
            models: Vec::new(),
            sources: Vec::new(),
            cost: CostSummary {
                available: false,
                total_usd: 0.0,
                unpriced_tokens: 0,
                pricing_as_of: String::new(),
                by_agent: Vec::new(),
            },
            indexing: IndexingView { pending: 0 },
        }
    }

    fn payload_agent_ids<'a>(payload: &'a WidgetSnapshot<'a>) -> Vec<&'a str> {
        payload.agents.iter().map(|agent| agent.id).collect()
    }

    #[test]
    fn filter_keeps_only_selected_agents_in_selection_order() {
        let snapshot = snapshot_with_agents(&["codex", "claude", "kimi"]);
        let filter = vec!["kimi".to_owned(), "codex".to_owned()];
        let payload = make_payload(&snapshot, Some(&filter));
        assert_eq!(payload_agent_ids(&payload), ["kimi", "codex"]);
    }

    #[test]
    fn empty_or_unknown_filter_stays_empty_instead_of_showing_every_agent() {
        let snapshot = snapshot_with_agents(&["codex", "claude"]);
        let filter = vec!["no-such-agent".to_owned()];
        let payload = make_payload(&snapshot, Some(&filter));
        assert!(payload_agent_ids(&payload).is_empty());
    }

    #[test]
    fn filter_deduplicates_agents_without_changing_order() {
        let snapshot = snapshot_with_agents(&["codex", "claude"]);
        let filter = vec!["claude".to_owned(), "claude".to_owned(), "codex".to_owned()];
        let payload = make_payload(&snapshot, Some(&filter));
        assert_eq!(payload_agent_ids(&payload), ["claude", "codex"]);
    }

    #[test]
    fn filter_normalization_rejects_unknown_agents() {
        let filter = vec![
            "kimi".to_owned(),
            "no-such-agent".to_owned(),
            "kimi".to_owned(),
        ];
        assert_eq!(normalize_agent_filter(&filter), ["kimi"]);
    }

    #[test]
    fn saved_selection_wins_over_a_stale_window_request() {
        let requested = vec!["codex".to_owned(), "claude".to_owned()];
        let (resolved, should_persist) =
            resolve_agent_filter(Some(r#"["zcode","kimi","codex"]"#), Some(&requested));
        assert_eq!(resolved.unwrap(), ["zcode", "kimi", "codex"]);
        assert!(!should_persist);
    }

    #[test]
    fn first_valid_window_selection_seeds_missing_saved_state_once() {
        let requested = vec!["kimi".to_owned(), "codex".to_owned()];
        let (resolved, should_persist) = resolve_agent_filter(None, Some(&requested));
        assert_eq!(resolved.unwrap(), ["kimi", "codex"]);
        assert!(should_persist);
    }

    #[test]
    fn no_filter_publishes_every_agent() {
        let snapshot = snapshot_with_agents(&["codex", "claude", "kimi"]);
        let payload = make_payload(&snapshot, None);
        assert_eq!(payload_agent_ids(&payload), ["codex", "claude", "kimi"]);
    }
}
