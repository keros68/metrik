//! Explicit, reversible opt-in for the clients' local timing exporters.
//!
//! The ownership file is a write-ahead journal of individual changed settings,
//! never a backup of a configuration that can contain credentials. Writing that
//! journal before replacing the client file also makes an interrupted install
//! recoverable: uninstall only restores values that still match our writes.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use toml_edit::{Document, Item, Table};

const CLAUDE_ENDPOINT: &str = "http://127.0.0.1:46319/claude/v1/logs";
const CODEX_ENDPOINT: &str = "http://127.0.0.1:46319/codex/v1/traces";
const MAX_CONFIG_BYTES: usize = 2 * 1024 * 1024;
const MAX_OWNERSHIP_BYTES: usize = 64 * 1024;
static CONFIG_LOCK: Mutex<()> = Mutex::new(());
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigStatus {
    pub configured: bool,
    pub conflict: bool,
}

pub struct ConfigManager {
    state_dir: PathBuf,
    claude_settings: Option<PathBuf>,
    codex_config: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Agent {
    Claude,
    Codex,
}

impl Agent {
    fn parse(agent: &str) -> Result<Self> {
        match agent {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => bail!("Unsupported timing client"),
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Self::Claude => CLAUDE_ENDPOINT,
            Self::Codex => CODEX_ENDPOINT,
        }
    }

    fn ownership_name(self) -> &'static str {
        match self {
            Self::Claude => "response-speed-claude.json",
            Self::Codex => "response-speed-codex.json",
        }
    }

    fn container(self) -> &'static str {
        match self {
            Self::Claude => "env",
            Self::Codex => "otel",
        }
    }

    fn desired(self) -> Vec<(&'static str, SettingValue)> {
        match self {
            Self::Claude => [
                ("CLAUDE_CODE_ENABLE_TELEMETRY", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "http/json"),
                ("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT", CLAUDE_ENDPOINT),
            ]
            .into_iter()
            .map(|(key, value)| (key, SettingValue::json(json!(value))))
            .collect(),
            Self::Codex => vec![(
                "trace_exporter",
                SettingValue {
                    value: json!({"otlp-http": {"endpoint": CODEX_ENDPOINT, "protocol": "json"}}),
                    toml: Some(format!(
                        "trace_exporter = {{ otlp-http = {{ endpoint = \"{CODEX_ENDPOINT}\", protocol = \"json\" }} }}\n"
                    )),
                },
            )],
        }
    }
}

// Some(JSON null) and an absent key have different restoration semantics.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct SettingValue {
    value: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    toml: Option<String>,
}

impl SettingValue {
    fn json(value: Value) -> Self {
        Self { value, toml: None }
    }
}

#[derive(Deserialize, Serialize)]
struct Change {
    key: String,
    previous: Option<SettingValue>,
    installed: SettingValue,
}

#[derive(Deserialize, Serialize)]
struct Ownership {
    version: u8,
    agent: Agent,
    config_path: PathBuf,
    original_missing: bool,
    created_container: bool,
    changes: Vec<Change>,
}

enum Settings {
    Claude(Value),
    Codex(Box<Document>),
}

struct Loaded {
    path: PathBuf,
    original: Option<Vec<u8>>,
    settings: Settings,
}

impl ConfigManager {
    pub fn detected(state_dir: PathBuf) -> Self {
        let directory = |variable: &str, default: &str| {
            std::env::var_os(variable)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .or_else(|| dirs::home_dir().map(|home| home.join(default)))
        };
        Self {
            state_dir,
            claude_settings: directory("CLAUDE_CONFIG_DIR", ".claude")
                .map(|directory| directory.join("settings.json")),
            codex_config: directory("CODEX_HOME", ".codex")
                .map(|directory| directory.join("config.toml")),
        }
    }

    #[cfg(test)]
    pub fn with_paths(state_dir: PathBuf, claude_settings: PathBuf, codex_config: PathBuf) -> Self {
        Self {
            state_dir,
            claude_settings: Some(claude_settings),
            codex_config: Some(codex_config),
        }
    }

    pub fn inspect(&self, agent: &str, endpoint: &str) -> Result<ConfigStatus> {
        let _guard = lock()?;
        let agent = checked_agent(agent, endpoint)?;
        let loaded = load(self.config_path(agent)?, agent)?;
        let ownership = self.read_ownership(agent)?;
        let ownership_conflict = ownership.as_ref().is_some_and(|ownership| {
            ownership.config_path != loaded.path
                || ownership.changes.iter().any(|change| {
                    !same_value(
                        loaded.settings.get(&change.key).as_ref(),
                        Some(&change.installed),
                    )
                })
        });
        let setting_conflict = loaded.settings.conflict(agent);
        Ok(ConfigStatus {
            configured: loaded.settings.configured(agent) && !setting_conflict,
            conflict: setting_conflict || ownership_conflict,
        })
    }

    pub fn enable(&self, agent: &str, endpoint: &str) -> Result<()> {
        let _guard = lock()?;
        let agent = checked_agent(agent, endpoint)?;
        let mut loaded = load(self.config_path(agent)?, agent)?;
        if loaded.settings.conflict(agent) {
            bail!("An existing timing exporter conflicts with the local collector");
        }
        if let Some(ownership) = self.read_ownership(agent)? {
            if ownership.config_path == loaded.path
                && loaded.settings.configured(agent)
                && ownership.changes.iter().all(|change| {
                    same_value(
                        loaded.settings.get(&change.key).as_ref(),
                        Some(&change.installed),
                    )
                })
            {
                return Ok(());
            }
            bail!("Timing settings changed; disable the previous installation before enabling");
        }

        let mut ownership = Ownership {
            version: 1,
            agent,
            config_path: loaded.path.clone(),
            original_missing: loaded.original.is_none(),
            created_container: !loaded.settings.has_container(agent),
            changes: Vec::new(),
        };
        for (key, installed) in agent.desired() {
            let previous = loaded.settings.get(key);
            if !same_value(previous.as_ref(), Some(&installed)) {
                loaded.settings.set(key, Some(&installed))?;
                ownership.changes.push(Change {
                    key: key.into(),
                    previous,
                    installed,
                });
            }
        }
        if ownership.changes.is_empty() {
            // A manually configured local exporter is not silently adopted.
            return Ok(());
        }
        let bytes = loaded.settings.bytes()?;
        ensure_unchanged(&loaded.path, loaded.original.as_deref())?;
        fs::create_dir_all(&self.state_dir)
            .map_err(|_| anyhow!("Cannot create timing ownership directory"))?;
        let journal = serde_json::to_vec(&ownership)
            .map_err(|_| anyhow!("Cannot encode timing ownership"))?;
        record_and_apply(&self.ownership_path(agent), &journal, || {
            loaded.replace(Some(&bytes))
        })
    }

    pub fn disable(&self, agent: &str) -> Result<()> {
        let _guard = lock()?;
        let agent = Agent::parse(agent)?;
        let Some(ownership) = self.read_ownership(agent)? else {
            return Ok(());
        };
        // Restore the file originally changed, even if CODEX_HOME or
        // CLAUDE_CONFIG_DIR has since been pointed at a different directory.
        let mut loaded = load(&ownership.config_path, agent)?;
        let mut changed = false;
        for change in &ownership.changes {
            if same_value(
                loaded.settings.get(&change.key).as_ref(),
                Some(&change.installed),
            ) {
                loaded.settings.set(&change.key, change.previous.as_ref())?;
                changed = true;
            }
        }
        if changed {
            if ownership.created_container {
                loaded.settings.remove_empty_container(agent);
            }
            let bytes = loaded.settings.bytes()?;
            if ownership.original_missing && loaded.settings.empty() {
                loaded.replace(None)?;
            } else {
                loaded.replace(Some(&bytes))?;
            }
        }
        // If deletion fails, the unchanged journal can be retried. It never
        // restores a second time over a subsequent user modification.
        fs::remove_file(self.ownership_path(agent))
            .map_err(|_| anyhow!("Timing settings restored; ownership cleanup can be retried"))?;
        Ok(())
    }

    fn config_path(&self, agent: Agent) -> Result<&Path> {
        match agent {
            Agent::Claude => self.claude_settings.as_deref(),
            Agent::Codex => self.codex_config.as_deref(),
        }
        .ok_or_else(|| anyhow!("Cannot locate timing client settings"))
    }

    fn ownership_path(&self, agent: Agent) -> PathBuf {
        self.state_dir.join(agent.ownership_name())
    }

    fn read_ownership(&self, agent: Agent) -> Result<Option<Ownership>> {
        let Some(bytes) = read_optional(&self.ownership_path(agent), MAX_OWNERSHIP_BYTES)? else {
            return Ok(None);
        };
        let ownership: Ownership = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow!("Invalid timing ownership metadata"))?;
        let desired = agent.desired();
        let mut keys = std::collections::HashSet::new();
        if ownership.version != 1
            || ownership.agent != agent
            || !ownership.config_path.is_absolute()
            || ownership.changes.is_empty()
            || ownership.changes.len() > desired.len()
            || ownership.changes.iter().any(|change| {
                !keys.insert(change.key.as_str())
                    || !desired.iter().any(|(key, value)| {
                        *key == change.key && same_value(Some(value), Some(&change.installed))
                    })
            })
        {
            bail!("Invalid timing ownership metadata");
        }
        Ok(Some(ownership))
    }
}

fn lock() -> Result<MutexGuard<'static, ()>> {
    CONFIG_LOCK
        .lock()
        .map_err(|_| anyhow!("Timing configuration is unavailable"))
}

fn checked_agent(agent: &str, endpoint: &str) -> Result<Agent> {
    let agent = Agent::parse(agent)?;
    if endpoint != agent.endpoint() {
        bail!("Timing exporter requires the local collector endpoint");
    }
    Ok(agent)
}

fn same_value(left: Option<&SettingValue>, right: Option<&SettingValue>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.value == right.value,
        (None, None) => true,
        _ => false,
    }
}

impl Settings {
    fn has_container(&self, agent: Agent) -> bool {
        match self {
            Self::Claude(value) => value.get(agent.container()).is_some(),
            Self::Codex(document) => document.get(agent.container()).is_some(),
        }
    }

    fn get(&self, key: &str) -> Option<SettingValue> {
        match self {
            Self::Claude(value) => Some(SettingValue::json(value.get("env")?.get(key)?.clone())),
            Self::Codex(document) => {
                let item = document.get("otel")?.as_table_like()?.get(key)?;
                let mut field = Document::new();
                field.as_table_mut().insert(key, item.clone());
                Some(SettingValue {
                    value: toml_value(item),
                    toml: Some(field.to_string()),
                })
            }
        }
    }

    fn configured(&self, agent: Agent) -> bool {
        agent
            .desired()
            .into_iter()
            .all(|(key, value)| same_value(self.get(key).as_ref(), Some(&value)))
    }

    fn conflict(&self, agent: Agent) -> bool {
        match self {
            Self::Claude(value) => {
                if !value.is_object() || value.get("env").is_some_and(|env| !env.is_object()) {
                    return true;
                }
                if claude_additional_exports_conflict(value) {
                    return true;
                }
                let get = |key| self.get(key).map(|value| value.value);
                if agent
                    .desired()
                    .into_iter()
                    .any(|(key, _)| get(key).is_some_and(|value| !value.is_string()))
                {
                    return true;
                }
                let endpoint = get("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT");
                if endpoint
                    .as_ref()
                    .is_some_and(|value| value != CLAUDE_ENDPOINT)
                {
                    return true;
                }
                match get("OTEL_LOGS_EXPORTER").as_ref().and_then(Value::as_str) {
                    None | Some("none") => false,
                    Some("otlp") => endpoint
                        .as_ref()
                        .is_none_or(|value| value != CLAUDE_ENDPOINT),
                    _ => true,
                }
            }
            Self::Codex(document) => {
                if document
                    .get("otel")
                    .is_some_and(|otel| !otel.is_table_like())
                {
                    return true;
                }
                let current = self.get("trace_exporter");
                let desired = agent.desired();
                current.as_ref().is_some_and(|current| {
                    current.value != "none" && !same_value(Some(current), Some(&desired[0].1))
                })
            }
        }
    }

    fn set(&mut self, key: &str, value: Option<&SettingValue>) -> Result<()> {
        match self {
            Self::Claude(settings) => {
                let settings = settings
                    .as_object_mut()
                    .ok_or_else(|| anyhow!("Invalid timing client settings"))?;
                if !settings.contains_key("env") {
                    settings.insert("env".into(), json!({}));
                }
                let env = settings
                    .get_mut("env")
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| anyhow!("Invalid timing client environment"))?;
                if let Some(value) = value {
                    env.insert(key.into(), value.value.clone());
                } else {
                    env.remove(key);
                }
            }
            Self::Codex(document) => {
                if !document.contains_key("otel") {
                    document["otel"] = Item::Table(Table::new());
                }
                // A user may have reformatted our inline exporter as nested
                // TOML tables. Their header comments belong to the current
                // file, so retain them when replacing that subtree with a
                // scalar (or removing it), rather than restoring old comments.
                let comments = document["otel"]
                    .get(key)
                    .map(table_comments)
                    .unwrap_or_default();
                if !comments.is_empty() {
                    if let Some(parent) = document["otel"].as_table_mut() {
                        let prefix = parent
                            .decor()
                            .prefix()
                            .and_then(|raw| raw.as_str())
                            .unwrap_or("");
                        let prefix = format!("{prefix}{comments}");
                        parent.decor_mut().set_prefix(prefix);
                        // An empty implicit parent is not rendered at all,
                        // including its comments, once its final child goes.
                        parent.set_implicit(false);
                    }
                }
                let table = document["otel"]
                    .as_table_like_mut()
                    .ok_or_else(|| anyhow!("Invalid timing client settings"))?;
                if let Some(value) = value {
                    let mut field = value
                        .toml
                        .as_deref()
                        .ok_or_else(|| anyhow!("Invalid timing ownership value"))?
                        .parse::<Document>()
                        .map_err(|_| anyhow!("Invalid timing ownership value"))?;
                    let item = field
                        .as_table_mut()
                        .remove(key)
                        .ok_or_else(|| anyhow!("Invalid timing ownership value"))?;
                    if toml_value(&item) != value.value {
                        bail!("Invalid timing ownership value");
                    }
                    let replacing_table = table.get(key).is_some_and(Item::is_table);
                    if let Some(existing) = table.get_mut(key) {
                        *existing = item;
                    } else {
                        table.insert(key, item);
                    }
                    if replacing_table {
                        // A key parsed from [otel.trace_exporter.*] has no
                        // assignment separator. Use normal spacing when it
                        // becomes a trace_exporter = value assignment again.
                        if let Some(decor) = table.key_decor_mut(key) {
                            decor.set_suffix(" ");
                        }
                    }
                } else {
                    table.remove(key);
                }
            }
        }
        Ok(())
    }

    fn remove_empty_container(&mut self, agent: Agent) {
        match self {
            Self::Claude(value) => {
                if value
                    .get("env")
                    .and_then(Value::as_object)
                    .is_some_and(|env| env.is_empty())
                {
                    value
                        .as_object_mut()
                        .expect("object containing env")
                        .remove("env");
                }
            }
            Self::Codex(document) => {
                if document
                    .get("otel")
                    .and_then(Item::as_table_like)
                    .is_some_and(|otel| otel.is_empty())
                    && document
                        .get("otel")
                        .is_none_or(|otel| table_comments(otel).is_empty())
                {
                    document.remove(agent.container());
                }
            }
        }
    }

    fn bytes(&self) -> Result<Vec<u8>> {
        match self {
            Self::Claude(value) => {
                let mut bytes = serde_json::to_vec_pretty(value)
                    .map_err(|_| anyhow!("Cannot encode timing client settings"))?;
                bytes.push(b'\n');
                Ok(bytes)
            }
            Self::Codex(document) => Ok(document.to_string().into_bytes()),
        }
    }

    fn empty(&self) -> bool {
        match self {
            Self::Claude(value) => value.as_object().is_some_and(|value| value.is_empty()),
            // Keep comments added by the user even when no TOML values remain.
            Self::Codex(document) => document.to_string().trim().is_empty(),
        }
    }
}

fn claude_additional_exports_conflict(settings: &Value) -> bool {
    let get = |key: &str| settings.get("env").and_then(|env| env.get(key));
    let telemetry_enabled =
        get("CLAUDE_CODE_ENABLE_TELEMETRY").and_then(Value::as_str) == Some("1");
    if !telemetry_enabled
        && ["OTEL_METRICS_EXPORTER", "OTEL_TRACES_EXPORTER"]
            .into_iter()
            .any(|key| match get(key) {
                None => false,
                Some(Value::String(value)) => value
                    .split(',')
                    .map(str::trim)
                    .any(|exporter| !exporter.is_empty() && exporter != "none"),
                Some(_) => true,
            })
    {
        // Enabling the global switch must not revive an unrelated dormant
        // exporter. Leave its configuration untouched for the user to resolve.
        return true;
    }
    // Detailed beta tracing sends logs and traces directly to this endpoint,
    // bypassing even our per-signal logs endpoint. It cannot be represented as
    // a working local logs installation, including when telemetry was active.
    get("ENABLE_BETA_TRACING_DETAILED").and_then(Value::as_str) == Some("1")
        && get("BETA_TRACING_ENDPOINT").is_some_and(|value| value.as_str() != Some(""))
}

fn table_comments(item: &Item) -> String {
    let Some(table) = item.as_table() else {
        return String::new();
    };
    let mut comments = String::new();
    for raw in [table.decor().prefix(), table.decor().suffix()]
        .into_iter()
        .flatten()
    {
        for line in raw.as_str().unwrap_or("").lines() {
            if let Some(index) = line.find('#') {
                comments.push_str(&line[index..]);
                comments.push('\n');
            }
        }
    }
    for (_, child) in table.iter() {
        comments.push_str(&table_comments(child));
    }
    comments
}

fn toml_value(item: &Item) -> Value {
    if let Some(table) = item.as_table_like() {
        return Value::Object(
            table
                .iter()
                .map(|(key, item)| (key.to_owned(), toml_value(item)))
                .collect(),
        );
    }
    if let Some(value) = item.as_str() {
        json!(value)
    } else {
        // Exporter schemas contain strings and tables. Unknown TOML types are
        // treated as foreign settings, never overwritten during opt-in.
        json!({"unsupportedTomlValue": item.to_string()})
    }
}

fn load(path: &Path, agent: Agent) -> Result<Loaded> {
    let path = resolved_path(path)?;
    let original = read_optional(&path, MAX_CONFIG_BYTES)?;
    let text = original
        .as_deref()
        .map(std::str::from_utf8)
        .transpose()
        .map_err(|_| anyhow!("Invalid timing client settings encoding"))?
        .unwrap_or("")
        .trim_start_matches('\u{feff}');
    let settings = match agent {
        Agent::Claude => Settings::Claude(if original.is_none() {
            json!({})
        } else {
            serde_json::from_str(text)
                .map_err(|_| anyhow!("Invalid timing client JSON settings"))?
        }),
        Agent::Codex => Settings::Codex(Box::new(
            text.parse::<Document>()
                .map_err(|_| anyhow!("Invalid timing client TOML settings"))?,
        )),
    };
    Ok(Loaded {
        path,
        original,
        settings,
    })
}

fn resolved_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|_| anyhow!("Cannot resolve timing client settings"))?
            .join(path)
    };
    let mut ancestor = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => {
                // Resolve existing parent symlinks as well as the client file.
                // New and existing Windows paths then use the same extended
                // absolute representation in the ownership journal.
                let mut resolved = fs::canonicalize(ancestor)
                    .map_err(|_| anyhow!("Cannot resolve timing client settings"))?;
                for component in missing.into_iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    ancestor
                        .file_name()
                        .ok_or_else(|| anyhow!("Invalid timing settings path"))?,
                );
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| anyhow!("Invalid timing settings path"))?;
            }
            Err(_) => bail!("Cannot resolve timing client settings"),
        }
    }
}

fn read_optional(path: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    match fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.len() > limit as u64 => {
            bail!("Timing configuration file is unavailable or too large");
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!("Cannot read timing configuration file"),
    }
    let bytes = fs::read(path).map_err(|_| anyhow!("Cannot read timing configuration file"))?;
    if bytes.len() > limit {
        bail!("Timing configuration file is too large");
    }
    Ok(Some(bytes))
}

fn ensure_unchanged(path: &Path, original: Option<&[u8]>) -> Result<()> {
    if read_optional(path, MAX_CONFIG_BYTES)?.as_deref() != original {
        bail!("Timing client settings changed during the operation; retry");
    }
    Ok(())
}

impl Loaded {
    fn replace(&self, bytes: Option<&[u8]>) -> Result<()> {
        ensure_unchanged(&self.path, self.original.as_deref())?;
        if let Some(bytes) = bytes {
            write_atomic(&self.path, bytes, false)
        } else {
            fs::remove_file(&self.path).map_err(|_| anyhow!("Cannot remove timing client settings"))
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| anyhow!("Invalid timing settings path"))?;
    fs::create_dir_all(directory)
        .map_err(|_| anyhow!("Cannot create timing settings directory"))?;
    let permissions = if private {
        None
    } else {
        fs::metadata(path)
            .ok()
            .map(|metadata| metadata.permissions())
    };
    if permissions
        .as_ref()
        .is_some_and(|permissions| permissions.readonly())
    {
        bail!("Timing client settings are read-only");
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let temporary = PathBuf::from(temporary);
    let mut created = false;
    let result = (|| -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        created = true;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|_| anyhow!("Cannot atomically write timing configuration"))
}

fn record_and_apply(
    journal_path: &Path,
    journal: &[u8],
    apply: impl FnOnce() -> Result<()>,
) -> Result<()> {
    write_atomic(journal_path, journal, true)?;
    if let Err(error) = apply() {
        // A crash or a failed cleanup remains safe: disable compares every
        // installed value before restoring anything from this journal.
        let _ = fs::remove_file(journal_path);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
        manager: ConfigManager,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "metrik-speed-config-{}-{}",
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            let manager = ConfigManager::with_paths(
                root.join("state"),
                root.join("claude/settings.json"),
                root.join("codex/config.toml"),
            );
            Self { root, manager }
        }

        fn path(&self, agent: Agent) -> &Path {
            self.manager.config_path(agent).unwrap()
        }

        fn write(&self, agent: Agent, text: &str) {
            fs::create_dir_all(self.path(agent).parent().unwrap()).unwrap();
            fs::write(self.path(agent), text).unwrap();
        }

        fn read(&self, agent: Agent) -> String {
            fs::read_to_string(self.path(agent)).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn claude_roundtrip_preserves_statusline_other_signals_and_private_settings() {
        let fixture = Fixture::new();
        let original = json!({
            "statusLine": {"type": "command", "command": "existing status line"},
            "env": {"CLAUDE_CODE_ENABLE_TELEMETRY": "1", "OTEL_LOGS_EXPORTER": "none", "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL": "grpc",
                "OTEL_METRICS_EXPORTER": "console", "OTEL_EXPORTER_OTLP_ENDPOINT": "https://existing.invalid",
                "UNRELATED_PRIVATE_VALUE": "fixture-secret"},
            "permissions": {"allow": ["Read"]}
        });
        fixture.write(Agent::Claude, &original.to_string());
        assert!(
            !fixture
                .manager
                .inspect("claude", CLAUDE_ENDPOINT)
                .unwrap()
                .configured
        );
        fixture.manager.enable("claude", CLAUDE_ENDPOINT).unwrap();
        let installed: Value = serde_json::from_str(&fixture.read(Agent::Claude)).unwrap();
        assert_eq!(installed["statusLine"], original["statusLine"]);
        assert_eq!(installed["env"]["OTEL_METRICS_EXPORTER"], "console");
        assert_eq!(
            installed["env"]["OTEL_EXPORTER_OTLP_ENDPOINT"],
            "https://existing.invalid"
        );
        let ownership = fs::read_to_string(fixture.manager.ownership_path(Agent::Claude)).unwrap();
        assert!(!ownership.contains("fixture-secret"));
        assert!(!ownership.contains("statusLine"));
        assert!(!ownership.contains("existing.invalid"));
        assert!(
            fixture
                .manager
                .inspect("claude", CLAUDE_ENDPOINT)
                .unwrap()
                .configured
        );
        fixture.manager.enable("claude", CLAUDE_ENDPOINT).unwrap();
        fixture.manager.disable("claude").unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&fixture.read(Agent::Claude)).unwrap(),
            original
        );
        assert!(!fixture.manager.ownership_path(Agent::Claude).exists());
    }

    #[test]
    fn claude_global_opt_in_never_activates_dormant_other_signal_exporters() {
        for (key, exporter) in [
            ("OTEL_METRICS_EXPORTER", "otlp"),
            ("OTEL_METRICS_EXPORTER", "prometheus"),
            ("OTEL_METRICS_EXPORTER", "console"),
            ("OTEL_TRACES_EXPORTER", "otlp"),
            ("OTEL_TRACES_EXPORTER", "none, console"),
        ] {
            for telemetry in [None, Some("0"), Some("1")] {
                let fixture = Fixture::new();
                let mut original = json!({"env": {
                    "OTEL_EXPORTER_OTLP_ENDPOINT": "https://existing.invalid",
                    "CLAUDE_CODE_ENHANCED_TELEMETRY_BETA": "1"
                }});
                original["env"][key] = json!(exporter);
                if let Some(telemetry) = telemetry {
                    original["env"]["CLAUDE_CODE_ENABLE_TELEMETRY"] = json!(telemetry);
                }
                let text = original.to_string();
                fixture.write(Agent::Claude, &text);
                let status = fixture.manager.inspect("claude", CLAUDE_ENDPOINT).unwrap();
                if telemetry == Some("1") {
                    assert!(!status.conflict);
                    fixture.manager.enable("claude", CLAUDE_ENDPOINT).unwrap();
                    fixture.manager.disable("claude").unwrap();
                    assert_eq!(
                        serde_json::from_str::<Value>(&fixture.read(Agent::Claude)).unwrap(),
                        original
                    );
                } else {
                    assert!(status.conflict);
                    assert!(!status.configured);
                    let error = fixture
                        .manager
                        .enable("claude", CLAUDE_ENDPOINT)
                        .unwrap_err()
                        .to_string();
                    assert!(!error.contains("existing.invalid"));
                    assert_eq!(fixture.read(Agent::Claude), text);
                    assert!(!fixture.manager.ownership_path(Agent::Claude).exists());
                }
            }
        }
    }

    #[test]
    fn claude_disabled_other_signals_and_inactive_beta_settings_are_preserved() {
        let fixture = Fixture::new();
        let original = json!({"env": {
            "CLAUDE_CODE_ENABLE_TELEMETRY": "0",
            "OTEL_METRICS_EXPORTER": "none", "OTEL_TRACES_EXPORTER": "",
            "ENABLE_BETA_TRACING_DETAILED": "0", "BETA_TRACING_ENDPOINT": "https://existing.invalid"
        }});
        fixture.write(Agent::Claude, &original.to_string());
        assert!(
            !fixture
                .manager
                .inspect("claude", CLAUDE_ENDPOINT)
                .unwrap()
                .conflict
        );
        fixture.manager.enable("claude", CLAUDE_ENDPOINT).unwrap();
        fixture.manager.disable("claude").unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&fixture.read(Agent::Claude)).unwrap(),
            original
        );
    }

    #[test]
    fn claude_detailed_beta_endpoint_cannot_masquerade_as_local_logs_collection() {
        for telemetry in ["0", "1"] {
            let fixture = Fixture::new();
            let original = json!({"env": {
                "CLAUDE_CODE_ENABLE_TELEMETRY": telemetry,
                "OTEL_LOGS_EXPORTER": "otlp", "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL": "http/json",
                "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT": CLAUDE_ENDPOINT,
                "ENABLE_BETA_TRACING_DETAILED": "1", "BETA_TRACING_ENDPOINT": "https://existing.invalid"
            }});
            let text = original.to_string();
            fixture.write(Agent::Claude, &text);
            assert_eq!(
                fixture.manager.inspect("claude", CLAUDE_ENDPOINT).unwrap(),
                ConfigStatus {
                    configured: false,
                    conflict: true
                }
            );
            assert!(fixture.manager.enable("claude", CLAUDE_ENDPOINT).is_err());
            assert_eq!(fixture.read(Agent::Claude), text);
            assert!(!fixture.manager.ownership_path(Agent::Claude).exists());
        }
    }

    #[test]
    fn codex_roundtrip_preserves_comments_and_unrelated_otel_configuration() {
        let fixture = Fixture::new();
        let original = "# user comment\nmodel = \"fixture-model\"\n[otel]\n# keep this comment\nexporter = \"console\"\ntrace_exporter = \"none\" # disabled intentionally\nlog_user_prompt = false\n[model_providers.example]\nname = \"fixture-secret\"\n";
        fixture.write(Agent::Codex, original);
        fixture.manager.enable("codex", CODEX_ENDPOINT).unwrap();
        let installed = fixture.read(Agent::Codex);
        assert!(installed.contains("# user comment"));
        assert!(installed.contains("# keep this comment"));
        assert!(installed.contains("exporter = \"console\""));
        assert!(installed.contains("log_user_prompt = false"));
        assert!(
            fixture
                .manager
                .inspect("codex", CODEX_ENDPOINT)
                .unwrap()
                .configured
        );
        assert!(
            !fs::read_to_string(fixture.manager.ownership_path(Agent::Codex))
                .unwrap()
                .contains("fixture-secret")
        );
        fixture.manager.disable("codex").unwrap();
        assert_eq!(fixture.read(Agent::Codex), original);
    }

    #[test]
    fn foreign_exporters_are_reported_without_mutating_either_file() {
        for (agent, text) in [
            (Agent::Claude, r#"{"env":{"OTEL_LOGS_EXPORTER":"otlp","OTEL_EXPORTER_OTLP_ENDPOINT":"https://foreign.invalid"}}"#),
            (Agent::Claude, r#"{"env":{"OTEL_LOGS_EXPORTER":"console"}}"#),
            (Agent::Claude, r#"{"env":{"OTEL_EXPORTER_OTLP_LOGS_ENDPOINT":"https://foreign.invalid"}}"#),
            (Agent::Codex, "[otel]\ntrace_exporter = { otlp-http = { endpoint = \"https://foreign.invalid\", protocol = \"json\" } }\n"),
            (Agent::Codex, "[otel]\ntrace_exporter = \"console\"\n"),
        ] {
            let fixture = Fixture::new();
            fixture.write(agent, text);
            let name = if agent == Agent::Claude { "claude" } else { "codex" };
            assert!(fixture.manager.inspect(name, agent.endpoint()).unwrap().conflict);
            assert!(fixture.manager.enable(name, agent.endpoint()).is_err());
            assert_eq!(fixture.read(agent), text);
            assert!(!fixture.manager.ownership_path(agent).exists());
        }
    }

    #[test]
    fn uninstall_preserves_later_user_edits_and_restores_only_owned_keys() {
        let fixture = Fixture::new();
        fixture.write(
            Agent::Claude,
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}}"#,
        );
        fixture.manager.enable("claude", CLAUDE_ENDPOINT).unwrap();
        let mut edited: Value = serde_json::from_str(&fixture.read(Agent::Claude)).unwrap();
        edited["env"]["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"] = json!("https://user-changed.invalid");
        edited["env"]["NEW_SETTING"] = json!("keep");
        edited["statusLine"] = json!({"command": "later command"});
        fixture.write(Agent::Claude, &edited.to_string());
        assert!(
            fixture
                .manager
                .inspect("claude", CLAUDE_ENDPOINT)
                .unwrap()
                .conflict
        );
        assert!(fixture.manager.enable("claude", CLAUDE_ENDPOINT).is_err());
        fixture.manager.disable("claude").unwrap();
        let restored: Value = serde_json::from_str(&fixture.read(Agent::Claude)).unwrap();
        assert_eq!(restored["env"]["CLAUDE_CODE_ENABLE_TELEMETRY"], "0");
        assert_eq!(
            restored["env"]["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"],
            "https://user-changed.invalid"
        );
        assert_eq!(restored["env"]["NEW_SETTING"], "keep");
        assert_eq!(restored["statusLine"], edited["statusLine"]);
        assert!(restored["env"].get("OTEL_LOGS_EXPORTER").is_none());
        assert!(restored["env"]
            .get("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL")
            .is_none());
    }

    #[test]
    fn codex_user_replacement_is_not_removed_or_reclaimed() {
        let fixture = Fixture::new();
        fixture.manager.enable("codex", CODEX_ENDPOINT).unwrap();
        let edited = "[otel]\ntrace_exporter = \"console\"\nexporter = \"none\"\n";
        fixture.write(Agent::Codex, edited);
        fixture.manager.disable("codex").unwrap();
        assert_eq!(fixture.read(Agent::Codex), edited);
        assert!(!fixture.manager.ownership_path(Agent::Codex).exists());
    }

    #[test]
    fn newly_created_configs_are_removed_unless_user_added_settings() {
        for (agent, name) in [(Agent::Claude, "claude"), (Agent::Codex, "codex")] {
            let fixture = Fixture::new();
            let status = fixture.manager.inspect(name, agent.endpoint()).unwrap();
            assert_eq!(
                status,
                ConfigStatus {
                    configured: false,
                    conflict: false
                }
            );
            assert!(!fixture.path(agent).exists());
            fixture.manager.enable(name, agent.endpoint()).unwrap();
            assert_eq!(
                fixture.manager.inspect(name, agent.endpoint()).unwrap(),
                ConfigStatus {
                    configured: true,
                    conflict: false
                }
            );
            fixture.manager.disable(name).unwrap();
            assert!(!fixture.path(agent).exists());
            fixture.manager.enable(name, agent.endpoint()).unwrap();
            let text = fixture.read(agent);
            if agent == Agent::Claude {
                let mut value: Value = serde_json::from_str(&text).unwrap();
                value["userAdded"] = json!(true);
                fixture.write(agent, &value.to_string());
            } else {
                fixture.write(
                    agent,
                    &format!("# user added comment\nuser_added = true\n{text}"),
                );
            }
            fixture.manager.disable(name).unwrap();
            assert!(fixture.path(agent).exists());
            let restored = fixture.read(agent);
            assert!(restored.contains(if agent == Agent::Claude {
                "userAdded"
            } else {
                "user_added"
            }));
            assert!(!restored.contains(agent.endpoint()));
        }
    }

    #[test]
    fn manually_configured_local_exporters_are_not_adopted_on_enable() {
        for (agent, name) in [(Agent::Claude, "claude"), (Agent::Codex, "codex")] {
            let fixture = Fixture::new();
            fixture.manager.enable(name, agent.endpoint()).unwrap();
            fs::remove_file(fixture.manager.ownership_path(agent)).unwrap();
            let manual = fixture.read(agent);
            fixture.manager.enable(name, agent.endpoint()).unwrap();
            fixture.manager.disable(name).unwrap();
            assert_eq!(fixture.read(agent), manual);
            assert!(!fixture.manager.ownership_path(agent).exists());
        }
    }

    #[test]
    fn invalid_json_toml_and_container_types_do_not_leak_or_modify_settings() {
        for (agent, text) in [
            (Agent::Claude, "{\"fixture-secret\": "),
            (Agent::Claude, r#"{"env":"fixture-secret"}"#),
            (Agent::Codex, "fixture-secret = ["),
            (Agent::Codex, "otel = \"fixture-secret\"\n"),
            (
                Agent::Codex,
                "[otel]\ntrace_exporter = [\"fixture-secret\"]\n",
            ),
            (
                Agent::Codex,
                "[[otel.trace_exporter]]\nendpoint = \"fixture-secret\"\n",
            ),
        ] {
            let fixture = Fixture::new();
            fixture.write(agent, text);
            let name = if agent == Agent::Claude {
                "claude"
            } else {
                "codex"
            };
            let error = fixture
                .manager
                .enable(name, agent.endpoint())
                .unwrap_err()
                .to_string();
            assert!(!error.contains("fixture-secret"));
            assert_eq!(fixture.read(agent), text);
            assert!(!fixture.manager.ownership_path(agent).exists());
        }
    }

    #[test]
    fn failed_journal_write_never_changes_the_client_configuration() {
        let fixture = Fixture::new();
        fixture.write(Agent::Claude, r#"{"statusLine":{"command":"keep"}}"#);
        let before = fixture.read(Agent::Claude);
        fs::write(&fixture.manager.state_dir, b"not a directory").unwrap();
        assert!(fixture.manager.enable("claude", CLAUDE_ENDPOINT).is_err());
        assert_eq!(fixture.read(Agent::Claude), before);
    }

    #[test]
    fn failed_client_write_removes_the_prepared_journal_and_temporary_files() {
        let fixture = Fixture::new();
        fixture.write(Agent::Claude, r#"{"userAdded":true}"#);
        let original = fixture.read(Agent::Claude);
        let permissions = fs::metadata(fixture.path(Agent::Claude))
            .unwrap()
            .permissions();
        let mut readonly = permissions.clone();
        readonly.set_readonly(true);
        fs::set_permissions(fixture.path(Agent::Claude), readonly).unwrap();
        let result = fixture.manager.enable("claude", CLAUDE_ENDPOINT);
        fs::set_permissions(fixture.path(Agent::Claude), permissions).unwrap();
        assert!(result.is_err());
        assert_eq!(fixture.read(Agent::Claude), original);
        assert!(!fixture.manager.ownership_path(Agent::Claude).exists());
        assert_eq!(fs::read_dir(&fixture.manager.state_dir).unwrap().count(), 0);
        assert_eq!(
            fs::read_dir(fixture.path(Agent::Claude).parent().unwrap())
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn atomic_rename_failure_cleans_up_only_its_temporary_file() {
        let fixture = Fixture::new();
        let directory = fixture.root.join("cannot-replace-a-directory");
        fs::create_dir(&directory).unwrap();
        assert!(write_atomic(&directory, b"fixture", false).is_err());
        assert!(directory.is_dir());
        assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 1);
    }

    #[test]
    fn prepared_journal_can_be_disabled_before_any_client_write() {
        let fixture = Fixture::new();
        fixture.write(
            Agent::Claude,
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"},"userAdded":true}"#,
        );
        let original = fixture.read(Agent::Claude);
        fixture.manager.enable("claude", CLAUDE_ENDPOINT).unwrap();
        fixture.write(Agent::Claude, &original); // Simulate interruption before rename.
        fixture.manager.disable("claude").unwrap();
        assert_eq!(fixture.read(Agent::Claude), original);
        assert!(!fixture.manager.ownership_path(Agent::Claude).exists());
    }

    #[test]
    fn changed_config_directory_does_not_adopt_or_mutate_the_new_configuration() {
        let fixture = Fixture::new();
        fixture.manager.enable("codex", CODEX_ENDPOINT).unwrap();
        let new_config = fixture.root.join("other-codex/config.toml");
        fs::create_dir_all(new_config.parent().unwrap()).unwrap();
        fs::write(&new_config, "model = \"keep\"\n").unwrap();
        let changed = ConfigManager::with_paths(
            fixture.manager.state_dir.clone(),
            fixture.path(Agent::Claude).to_owned(),
            new_config.clone(),
        );
        assert!(changed.inspect("codex", CODEX_ENDPOINT).unwrap().conflict);
        assert!(changed.enable("codex", CODEX_ENDPOINT).is_err());
        changed.disable("codex").unwrap();
        assert!(!fixture.path(Agent::Codex).exists());
        assert_eq!(
            fs::read_to_string(new_config).unwrap(),
            "model = \"keep\"\n"
        );
    }

    #[test]
    fn restoring_toml_compares_values_instead_of_user_formatting() {
        let fixture = Fixture::new();
        fixture.write(Agent::Codex, "# keep\n[otel]\ntrace_exporter = \"none\"\n");
        fixture.manager.enable("codex", CODEX_ENDPOINT).unwrap();
        fixture.write(Agent::Codex, &format!("# keep\n[otel.trace_exporter.otlp-http]\nprotocol = 'json'\nendpoint = '{CODEX_ENDPOINT}'\n"));
        assert!(
            fixture
                .manager
                .inspect("codex", CODEX_ENDPOINT)
                .unwrap()
                .configured
        );
        fixture.manager.disable("codex").unwrap();
        assert_eq!(
            fixture.read(Agent::Codex),
            "# keep\n[otel]\ntrace_exporter = \"none\"\n"
        );
    }

    #[test]
    fn uninstall_keeps_current_toml_comments_without_resurrecting_deleted_comments() {
        let fixture = Fixture::new();
        fixture.write(
            Agent::Codex,
            "# old comment\n[otel]\ntrace_exporter = \"none\"\n",
        );
        fixture.manager.enable("codex", CODEX_ENDPOINT).unwrap();
        fixture.write(Agent::Codex, &format!("# current comment\n[otel.trace_exporter.otlp-http]\nprotocol = 'json'\nendpoint = '{CODEX_ENDPOINT}'\n"));
        fixture.manager.disable("codex").unwrap();
        let restored = fixture.read(Agent::Codex);
        assert!(restored.contains("# current comment"));
        assert!(!restored.contains("# old comment"));
        assert_eq!(
            restored.parse::<Document>().unwrap()["otel"]["trace_exporter"].as_str(),
            Some("none")
        );
    }

    #[test]
    fn comments_added_to_a_new_nested_exporter_keep_the_client_file_on_uninstall() {
        let fixture = Fixture::new();
        fixture.manager.enable("codex", CODEX_ENDPOINT).unwrap();
        fixture.write(Agent::Codex, &format!("# user added\n[otel.trace_exporter.otlp-http]\nprotocol = 'json'\nendpoint = '{CODEX_ENDPOINT}'\n"));
        fixture.manager.disable("codex").unwrap();
        let restored = fixture.read(Agent::Codex);
        assert!(restored.contains("# user added"));
        assert!(!restored.contains(CODEX_ENDPOINT));
        assert!(restored.parse::<Document>().unwrap()["otel"]
            .as_table()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn endpoint_validation_does_not_accept_remote_or_other_signal_destinations() {
        let fixture = Fixture::new();
        for endpoint in [
            "https://foreign.invalid",
            CODEX_ENDPOINT,
            "http://localhost:46319/claude/v1/logs",
        ] {
            assert!(fixture.manager.enable("claude", endpoint).is_err());
        }
        assert!(!fixture.path(Agent::Claude).exists());
        assert!(!fixture.manager.state_dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn ownership_is_private_and_configuration_symlink_is_preserved() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let fixture = Fixture::new();
        let target = fixture.root.join("shared-settings.json");
        fs::write(&target, b"{}").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        fs::create_dir_all(fixture.path(Agent::Claude).parent().unwrap()).unwrap();
        symlink(&target, fixture.path(Agent::Claude)).unwrap();
        fixture.manager.enable("claude", CLAUDE_ENDPOINT).unwrap();
        assert!(fs::symlink_metadata(fixture.path(Agent::Claude))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::metadata(fixture.manager.ownership_path(Agent::Claude))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fixture.manager.disable("claude").unwrap();
        assert!(fs::symlink_metadata(fixture.path(Agent::Claude))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            serde_json::from_str::<Value>(&fs::read_to_string(target).unwrap()).unwrap(),
            json!({})
        );
    }
}
