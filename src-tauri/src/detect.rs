//! 本机装了哪些 Agent。
//!
//! 只看安装痕迹，不看有没有用量：装了但最近没用过的也该算"检测到"，否则
//! 设置里会把它挪进"未检测到"，用户会以为我们不支持。
//!
//! **检测只用于排序与分组，从不用于过滤。** 探针会误判——换了非默认配置
//! 目录、便携安装、或者我们压根没有可靠探针（Antigravity 只有运行期的 RPC
//! 端点，没有稳定的安装痕迹）。若误判能把一个 Agent 从列表里抹掉，用户就
//! 再也勾不上它了，所以未检测到的一律仍可展开手动勾选。
//!
//! 路径取自各 adapter 的 `discover()`，不另猜一套：改了那边这里要跟着改，
//! `probe_paths_match_adapter_roots` 会在偏离时失败。

use crate::coding_quota;
use std::path::PathBuf;

/// 一个 Agent 的安装探针。三种形态对应三类来源，与各 adapter 的取数方式一致。
pub enum Probe {
    /// 任一路径存在即算装了。路径已按平台解析成绝对路径。
    Paths(Vec<PathBuf>),
    /// 由用户配置的凭据提供（Qoder 只有账户级 Credits，没有本地日志；Qwen 看
    /// pi auth.json 里有没有百炼 Token Plan 的 key）。
    Credential(fn() -> bool),
    /// 没有便宜且可靠的安装痕迹。此类 Agent 只能靠"本周期有用量"反推，
    /// 由调用方补上，这里如实返回"未知"而不是猜一个路径。
    Unknown,
}

pub struct AgentProbe {
    pub id: &'static str,
    pub probe: Probe,
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_default()
}

/// OpenCode 的数据根：与 `OpencodeAdapter::detected()` 同一套解析。
fn opencode_data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|value| value.is_absolute())
        .unwrap_or_else(|| home().join(".local").join("share"))
        .join("opencode")
}

/// Kimi 的数据根：与 `KimiAdapter::detected()` 同一套解析。
fn kimi_code_dir() -> PathBuf {
    std::env::var_os("KIMI_CODE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home().join(".kimi-code"))
}

/// Grok Build 的数据根：与 `GrokAdapter::detected()` / `grok_home()` 同源。
fn grok_home_dir() -> PathBuf {
    std::env::var_os("GROK_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home().join(".grok"))
}

pub fn table() -> Vec<AgentProbe> {
    let home = home();
    vec![
        AgentProbe {
            id: "codex",
            probe: Probe::Paths(vec![home.join(".codex")]),
        },
        AgentProbe {
            id: "claude",
            probe: Probe::Paths(vec![home.join(".claude")]),
        },
        AgentProbe {
            id: "zcode",
            probe: Probe::Paths(vec![home.join(".zcode")]),
        },
        AgentProbe {
            id: "opencode",
            probe: Probe::Paths(vec![opencode_data_dir()]),
        },
        AgentProbe {
            id: "kimi",
            probe: Probe::Paths(vec![
                kimi_code_dir(),
                home.join(".kimi"),
                // 桌面版没装 CLI 时也能被探到（安装痕迹 = kimi-desktop 数据目录）。
                dirs::config_dir()
                    .map(|config| config.join("kimi-desktop"))
                    .unwrap_or_else(|| home.join(".kimi-desktop-absent")),
            ]),
        },
        AgentProbe {
            // Antigravity 只在 IDE 运行时才有 language server 端点，扫进程与
            // 试端口都太贵，不适合每次开设置页都跑一遍。
            id: "antigravity",
            probe: Probe::Unknown,
        },
        AgentProbe {
            id: "workbuddy",
            probe: Probe::Paths(vec![home.join(".codebuddy"), home.join(".workbuddy")]),
        },
        AgentProbe {
            id: "qoder",
            probe: Probe::Credential(|| coding_quota::qoder_cookie_source().is_some()),
        },
        AgentProbe {
            id: "grok",
            probe: Probe::Paths(vec![grok_home_dir()]),
        },
        AgentProbe {
            // pi 与其同格式分支 Oh My Pi 各有一个数据根；与 PiAdapter::detected()
            // 的扫描根同源（取其父目录）。
            id: "pi",
            probe: Probe::Paths(vec![
                home.join(".pi").join("agent"),
                home.join(".omp").join("agent"),
            ]),
        },
        AgentProbe {
            // 百炼 Token Plan 没有可编程的额度接口，控制台 cookie 只能活几天，
            // 官方额度源已移除；卡片只承载 pi 归属过来的用量，探针看 pi 配了没配
            // 百炼 Token Plan 的 key。
            id: "qwen",
            probe: Probe::Credential(coding_quota::pi_auth_has_qwen_token_plan),
        },
        AgentProbe {
            // Hermes 的数据根：与 HermesAdapter::detected() 同源（扫描的是
            // ~/.hermes/state.db，探针看库文件本身）。
            id: "hermes",
            probe: Probe::Paths(vec![home.join(".hermes").join("state.db")]),
        },
    ]
}

/// 本机检测到的 Agent id。返回顺序即表的顺序。
pub fn installed_agents() -> Vec<&'static str> {
    table()
        .into_iter()
        .filter(|entry| match &entry.probe {
            Probe::Paths(paths) => paths.iter().any(|path| path.exists()),
            Probe::Credential(check) => check(),
            Probe::Unknown => false,
        })
        .map(|entry| entry.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::AGENT_IDS;

    #[test]
    fn every_agent_has_exactly_one_probe() {
        let ids: Vec<&str> = table().into_iter().map(|entry| entry.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "有 Agent 写了两个探针");

        let mut expected: Vec<&str> = AGENT_IDS.to_vec();
        expected.sort_unstable();
        assert_eq!(
            sorted, expected,
            "探针表与 AGENT_IDS 不一致：新增 Agent 时两处要同步"
        );
    }

    /// 探针路径必须与各 adapter 真正扫描的根一致。adapter 改了目录而这里没跟，
    /// 会让用户看到"未检测到"却明明有数据。
    #[test]
    fn probe_paths_match_adapter_roots() {
        let home = home();
        let by_id = |wanted: &str| {
            table()
                .into_iter()
                .find(|entry| entry.id == wanted)
                .map(|entry| entry.probe)
                .expect("表里应有该 Agent")
        };
        let paths = |probe: Probe| match probe {
            Probe::Paths(paths) => paths,
            _ => panic!("该 Agent 应当是路径探针"),
        };

        // 与 codex.rs / claude.rs / zcode.rs 里的根同源，取其父目录。
        assert_eq!(paths(by_id("codex")), vec![home.join(".codex")]);
        assert_eq!(paths(by_id("claude")), vec![home.join(".claude")]);
        assert_eq!(paths(by_id("zcode")), vec![home.join(".zcode")]);
        assert_eq!(
            paths(by_id("workbuddy")),
            vec![home.join(".codebuddy"), home.join(".workbuddy")]
        );
        // 这两个受环境变量覆盖，必须走与 adapter 相同的解析。
        assert_eq!(paths(by_id("opencode")), vec![opencode_data_dir()]);
        assert_eq!(
            paths(by_id("kimi")),
            vec![
                kimi_code_dir(),
                home.join(".kimi"),
                dirs::config_dir()
                    .map(|config| config.join("kimi-desktop"))
                    .unwrap_or_else(|| home.join(".kimi-desktop-absent")),
            ]
        );
        // grok 同样受 GROK_HOME 覆盖。
        assert_eq!(paths(by_id("grok")), vec![grok_home_dir()]);
        // pi 与 OMP 的数据根与 PiAdapter::detected() 同源。
        assert_eq!(
            paths(by_id("pi")),
            vec![
                home.join(".pi").join("agent"),
                home.join(".omp").join("agent")
            ]
        );
        // hermes 探针与 HermesAdapter::detected() 同源：state.db 文件本身。
        assert_eq!(
            paths(by_id("hermes")),
            vec![home.join(".hermes").join("state.db")]
        );
    }

    #[test]
    fn unknown_probe_never_claims_installed() {
        let antigravity = table()
            .into_iter()
            .find(|entry| entry.id == "antigravity")
            .expect("表里应有 antigravity");
        assert!(matches!(antigravity.probe, Probe::Unknown));
        assert!(!installed_agents().contains(&"antigravity"));
    }
}
