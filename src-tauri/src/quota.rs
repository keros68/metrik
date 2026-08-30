//! 官方额度来源的统一取数层。
//!
//! 此前每家厂商都在 `engine::build_snapshot` 里单独硬接：Codex 走 app-server、
//! Claude 走 OAuth 再回落 statusLine 钩子、GLM/Kimi/Qoder/WorkBuddy 走 HTTP。
//! 三段代码形状相同却各写一遍，加一家厂商的成本是"再改一次 engine 的取数
//! 流程"——这才是覆盖面上不去的结构性原因。
//!
//! 这里把共同形状抽出来：声明缓存节奏与超时（各家限流策略不同，故随 provider
//! 声明而非写死），实现一个 `fetch`，在 `registry()` 里列一行。engine 只调
//! `refresh_all`。
//!
//! 落库语义对所有来源一致，且不得放宽：**拿到窗口才整体替换该 adapter 的行**
//! ——来源里消失的窗口（如套餐变更后没有 5 小时窗）不得滞留冒充当前额度；
//! 拿不到就保留旧行（会随时效在展示层变陈旧），绝不写零值或估算。

use crate::adapters;
use crate::app_server;
use crate::claude_hook::ClaudeHook;
use crate::claude_oauth::{self, ClaudeOauth};
use crate::coding_quota;
use crate::domain::QuotaSample;
use crate::storage;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 缓存节奏与单次取数超时。
#[derive(Clone, Copy)]
pub struct QuotaPolicy {
    /// 拿到窗口后多久内不再重拉。
    fresh_ttl: Duration,
    /// 拉空或失败后多久内不再重试（限流友好）。
    empty_ttl: Duration,
    timeout: Duration,
}

impl QuotaPolicy {
    const fn new(fresh_secs: u64, empty_secs: u64, timeout_secs: u64) -> Self {
        Self {
            fresh_ttl: Duration::from_secs(fresh_secs),
            empty_ttl: Duration::from_secs(empty_secs),
            timeout: Duration::from_secs(timeout_secs),
        }
    }
}

/// 一次取数的结果。`samples` 为空不算错误——多数来源在没有凭据时就是空。
pub struct QuotaOutcome {
    pub samples: Vec<QuotaSample>,
    /// 失败原因；只有声明要留档的 provider 会被上层记下来。
    pub failure: Option<String>,
}

/// 取数前从账本读出的开关快照。provider 不直接碰数据库——落库与设置读写
/// 都留在这一层，provider 只负责"从外部拿到窗口"。
pub struct ProviderEnv {
    settings: HashMap<String, String>,
}

impl ProviderEnv {
    pub fn load(connection: &Connection) -> Result<Self> {
        let mut statement = connection
            .prepare("SELECT key, value FROM app_setting")
            .context("failed to prepare app_setting query")?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("failed to read app_setting")?;
        let mut settings = HashMap::new();
        for row in rows {
            let (key, value) = row.context("failed to read an app_setting row")?;
            settings.insert(key, value);
        }
        Ok(Self { settings })
    }

    fn flag(&self, key: &str) -> bool {
        self.settings.get(key).map(String::as_str) == Some("1")
    }
}

pub trait QuotaProvider: Send + Sync {
    /// 写入 `quota_snapshot` 用的 adapter_id。
    fn adapter_id(&self) -> &'static str;

    fn policy(&self) -> QuotaPolicy;

    /// 主来源的前置条件（开关、凭据、可执行文件）。false 时不发起拉取，
    /// 直接走 `fallback`。
    fn is_available(&self, _env: &ProviderEnv) -> bool {
        true
    }

    fn fetch(&self, timeout: Duration) -> Result<Vec<QuotaSample>>;

    /// 主来源拿不到时的本地兜底（目前只有 Claude 的 statusLine 钩子文件）。
    /// 读本地文件，成本可忽略，故不进缓存。
    fn fallback(&self) -> Vec<QuotaSample> {
        Vec::new()
    }

    /// 失败原因是否留档给用户看。默认不留：多数来源在没配凭据时失败是常态，
    /// 留档只会制造噪声。Claude 直连是例外——它是用户显式开启的，失败必须
    /// 有交代。
    fn record_failure(&self, _connection: &Connection, _message: &str) -> Result<()> {
        Ok(())
    }

    fn clear_failure(&self, _connection: &Connection) -> Result<()> {
        Ok(())
    }
}

/// adapter 每次快照都重建，故缓存不能放 provider 里，必须由调用方跨快照持有。
pub type QuotaCache = Mutex<HashMap<&'static str, (Instant, Vec<QuotaSample>)>>;

pub fn registry() -> Vec<Box<dyn QuotaProvider>> {
    let mut providers: Vec<Box<dyn QuotaProvider>> = vec![
        Box::new(CodexQuota),
        Box::new(ClaudeQuota),
        Box::new(GrokQuota),
    ];
    for (adapter_id, fetch) in [
        (
            "zcode",
            coding_quota::fetch_zcode_quota as fn(Duration) -> Result<Vec<QuotaSample>>,
        ),
        ("kimi", coding_quota::fetch_kimi_quota),
        ("kimiwork", coding_quota::fetch_kimiwork_quota),
        ("qoder", coding_quota::fetch_qoder_quota),
        ("workbuddy", coding_quota::fetch_workbuddy_quota),
    ] {
        providers.push(Box::new(HttpQuota { adapter_id, fetch }));
    }
    providers
}

/// Grok Build：本地统一日志里的 credits 快照（非网络 live）。
/// 日志由 Grok CLI 在会话中自行写入；Metrik 只读尾部，不碰 OAuth。
struct GrokQuota;

impl QuotaProvider for GrokQuota {
    fn adapter_id(&self) -> &'static str {
        "grok"
    }

    fn policy(&self) -> QuotaPolicy {
        // 本地文件读取便宜；日志可能数分钟才刷新一次，fresh 不必太短。
        QuotaPolicy::new(120, 300, 2)
    }

    fn is_available(&self, _env: &ProviderEnv) -> bool {
        adapters::grok_home_exists()
    }

    fn fetch(&self, timeout: Duration) -> Result<Vec<QuotaSample>> {
        // 环境变量在这里解析一次；适配器本体接根目录参数，测试不碰全局状态。
        adapters::fetch_grok_quota_snapshot(&adapters::grok_home(), timeout)
    }
}

/// 取数并落库。engine 只调这一个。
pub fn refresh_all(connection: &Connection, cache: &QuotaCache, force: bool) -> Result<()> {
    let env = ProviderEnv::load(connection)?;
    for provider in registry() {
        let provider = provider.as_ref();
        let mut samples = Vec::new();
        if provider.is_available(&env) {
            let outcome = cached_fetch(cache, provider, force);
            match &outcome.failure {
                Some(message) => provider.record_failure(connection, message)?,
                // 缓存命中失败时返回的是"空且无错误"：既不留档也不清除，
                // 上一条记录继续有效。
                None if !outcome.samples.is_empty() => provider.clear_failure(connection)?,
                None => {}
            }
            samples = outcome.samples;
        }
        if samples.is_empty() {
            samples = provider.fallback();
        }
        if samples.is_empty() {
            continue;
        }
        connection
            .execute(
                "DELETE FROM quota_snapshot WHERE adapter_id = ?1",
                [provider.adapter_id()],
            )
            .with_context(|| format!("failed to clear {} quota rows", provider.adapter_id()))?;
        for sample in &samples {
            storage::upsert_quota(connection, sample)?;
        }
    }
    prune_unmanaged_quota_rows(connection)?;
    Ok(())
}

/// 清除注册表已不存在的 adapter 的残留配额行。来源被移除后（如 pi 额度源、
/// Qwen Token Plan 的控制台 cookie 源），旧版本写入的行若不清理，卡片/胶囊会继续显示一个不再有来源的配额。
/// 只删“不再有 provider”的 adapter；仍在注册表里的（含 kimiwork 这类内部源）
/// 即使本轮拉空也保留旧行（陈旧展示由展示层负责）。
pub fn prune_unmanaged_quota_rows(connection: &Connection) -> Result<()> {
    let managed: Vec<&str> = registry().iter().map(|p| p.adapter_id()).collect();
    let placeholders = vec!["?"; managed.len()].join(",");
    let sql = format!("DELETE FROM quota_snapshot WHERE adapter_id NOT IN ({placeholders})");
    let mut statement = connection.prepare(&sql)?;
    statement.execute(rusqlite::params_from_iter(managed))?;
    Ok(())
}

/// 按 provider 的节奏取数并跨快照缓存。`force`（手动刷新）跳过新鲜缓存立即
/// 重拉；失败路径与常规一致——写入空哨兵、保留库中旧行，绝不因强制刷新而删数据。
fn cached_fetch(cache: &QuotaCache, provider: &dyn QuotaProvider, force: bool) -> QuotaOutcome {
    let policy = provider.policy();
    let Ok(mut guard) = cache.lock() else {
        // 锁中毒只可能来自别处的 panic，此时当作"这次没数据"：保留库中旧行，
        // 也不拿一句内部错误去污染用户能看到的失败记录。
        return QuotaOutcome {
            samples: Vec::new(),
            failure: None,
        };
    };
    if !force {
        if let Some((captured, cached)) = guard.get(provider.adapter_id()) {
            let ttl = if cached.is_empty() {
                policy.empty_ttl
            } else {
                policy.fresh_ttl
            };
            if captured.elapsed() < ttl {
                return QuotaOutcome {
                    samples: cached.clone(),
                    failure: None,
                };
            }
        }
    }
    match provider.fetch(policy.timeout) {
        Ok(samples) => {
            guard.insert(provider.adapter_id(), (Instant::now(), samples.clone()));
            QuotaOutcome {
                samples,
                failure: None,
            }
        }
        Err(error) => {
            // 来源不可用时不该让每次周期性刷新都付一次完整超时，空哨兵用更长的 TTL。
            guard.insert(provider.adapter_id(), (Instant::now(), Vec::new()));
            QuotaOutcome {
                samples: Vec::new(),
                failure: Some(error.to_string()),
            }
        }
    }
}

/// Codex：本机 ChatGPT / Codex app-server 是权威来源。它是本地进程，
/// 比走网络的几家更快也更值得频繁重试，故 TTL 与超时都更短。
struct CodexQuota;

impl QuotaProvider for CodexQuota {
    fn adapter_id(&self) -> &'static str {
        "codex"
    }

    fn policy(&self) -> QuotaPolicy {
        QuotaPolicy::new(60, 240, 4)
    }

    fn fetch(&self, timeout: Duration) -> Result<Vec<QuotaSample>> {
        app_server::read_codex_quota(timeout)
    }
}

/// Claude：用户显式开启 OAuth 直连时优先（账户级合并额度，含网页版消耗，
/// 不依赖终端状态栏）；未开启或拉取失败时回落到 statusLine 钩子文件。
struct ClaudeQuota;

impl QuotaProvider for ClaudeQuota {
    fn adapter_id(&self) -> &'static str {
        "claude"
    }

    fn policy(&self) -> QuotaPolicy {
        QuotaPolicy::new(120, 300, 6)
    }

    fn is_available(&self, env: &ProviderEnv) -> bool {
        env.flag(claude_oauth::SETTING_KEY)
    }

    fn fetch(&self, timeout: Duration) -> Result<Vec<QuotaSample>> {
        ClaudeOauth::detected().fetch_quota_samples(timeout)
    }

    fn fallback(&self) -> Vec<QuotaSample> {
        ClaudeHook::detected().quota_samples()
    }

    fn record_failure(&self, connection: &Connection, message: &str) -> Result<()> {
        claude_oauth::record_failure(connection, message)
    }

    fn clear_failure(&self, connection: &Connection) -> Result<()> {
        claude_oauth::clear_failure(connection)
    }
}

/// 走网络的官方配额（GLM / Kimi / Qoder / WorkBuddy）：一次实时 GET，
/// 凭据由各自的 fetch 自行从本机配置读取，没有凭据时返回错误而不是零值。
struct HttpQuota {
    adapter_id: &'static str,
    fetch: fn(Duration) -> Result<Vec<QuotaSample>>,
}

impl QuotaProvider for HttpQuota {
    fn adapter_id(&self) -> &'static str {
        self.adapter_id
    }

    fn policy(&self) -> QuotaPolicy {
        QuotaPolicy::new(120, 300, 6)
    }

    fn fetch(&self, timeout: Duration) -> Result<Vec<QuotaSample>> {
        (self.fetch)(timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_ledger() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        connection
    }

    #[test]
    fn prune_removes_rows_of_removed_quota_sources_only() {
        use crate::domain::QuotaSample;
        let connection = memory_ledger();
        // pi 曾是配额源（0.17.1）、Qwen 控制台 cookie 源后来也移除；
        // kimiwork 仍在注册表。
        for adapter in ["pi", "qwen", "kimiwork"] {
            storage::upsert_quota(
                &connection,
                &QuotaSample {
                    adapter_id: adapter,
                    window_key: "five_hour".into(),
                    remaining_percent: 99.0,
                    resets_at_ms: None,
                    collected_at_ms: 0,
                    source_label: "x".into(),
                    quality: "official_live",
                },
            )
            .unwrap();
        }
        prune_unmanaged_quota_rows(&connection).unwrap();
        let left: Vec<String> = {
            let mut s = connection
                .prepare("SELECT adapter_id FROM quota_snapshot ORDER BY adapter_id")
                .unwrap();
            s.query_map([], |row| row.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(
            left,
            vec!["kimiwork".to_string()],
            "pi 与 qwen 残留应被清除、kimiwork 保留"
        );
    }

    #[test]
    fn registry_covers_every_quota_adapter_exactly_once() {
        let mut ids: Vec<&str> = registry()
            .iter()
            .map(|provider| provider.adapter_id())
            .collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "两个 provider 写了同一个 adapter_id");
        assert_eq!(
            ids,
            vec![
                "claude",
                "codex",
                "grok",
                "kimi",
                "kimiwork",
                "qoder",
                "workbuddy",
                "zcode"
            ]
        );
    }

    #[test]
    fn env_reads_the_opt_in_flag_as_written() {
        let connection = memory_ledger();
        let env = ProviderEnv::load(&connection).unwrap();
        assert!(!env.flag(claude_oauth::SETTING_KEY));

        storage::set_app_setting(&connection, claude_oauth::SETTING_KEY, "1").unwrap();
        assert!(ProviderEnv::load(&connection)
            .unwrap()
            .flag(claude_oauth::SETTING_KEY));

        // 关闭写的是 "0"，不是删除键——不能把它也当成开启。
        storage::set_app_setting(&connection, claude_oauth::SETTING_KEY, "0").unwrap();
        assert!(!ProviderEnv::load(&connection)
            .unwrap()
            .flag(claude_oauth::SETTING_KEY));
    }

    /// Claude 直连是用户显式开启的来源，未开启时不该发起拉取——否则会把
    /// 一条"没有凭据"的失败记录摆到用户面前。
    #[test]
    fn claude_primary_waits_for_the_opt_in_flag() {
        let connection = memory_ledger();
        let env = ProviderEnv::load(&connection).unwrap();
        assert!(!ClaudeQuota.is_available(&env));

        storage::set_app_setting(&connection, claude_oauth::SETTING_KEY, "1").unwrap();
        let env = ProviderEnv::load(&connection).unwrap();
        assert!(ClaudeQuota.is_available(&env));

        // 其余来源没有开关，永远可用。
        assert!(CodexQuota.is_available(&env));
    }

    #[test]
    fn a_fresh_cache_entry_short_circuits_the_fetch() {
        struct Counting {
            calls: std::sync::atomic::AtomicUsize,
        }
        impl QuotaProvider for Counting {
            fn adapter_id(&self) -> &'static str {
                "codex"
            }
            fn policy(&self) -> QuotaPolicy {
                QuotaPolicy::new(60, 240, 4)
            }
            fn fetch(&self, _timeout: Duration) -> Result<Vec<QuotaSample>> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err(anyhow::anyhow!("来源不可用"))
            }
        }

        let provider = Counting {
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let cache: QuotaCache = Mutex::new(HashMap::new());

        // 第一次真的去拉，失败原因如实带回。
        let first = cached_fetch(&cache, &provider, false);
        assert!(first.samples.is_empty());
        assert_eq!(first.failure.as_deref(), Some("来源不可用"));

        // 空哨兵在 TTL 内挡住重拉，且不再报错——上一条记录继续有效。
        let second = cached_fetch(&cache, &provider, false);
        assert!(second.failure.is_none());
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // force 跳过缓存，立即重试。
        let third = cached_fetch(&cache, &provider, true);
        assert_eq!(third.failure.as_deref(), Some("来源不可用"));
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
