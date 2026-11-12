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

use crate::adapters::{antigravity_data_dirs, kimi_code_dir, opencode_data_dir};
use crate::coding_quota;
use std::path::PathBuf;

/// 一个 Agent 的安装探针。两种形态对应两类来源，与各 adapter 的取数方式一致。
pub enum Probe {
    /// 任一路径存在即算装了。路径已按平台解析成绝对路径。
    Paths(Vec<PathBuf>),
    /// 由闭包判定（Qoder 只有账户级 Credits，没有本地日志；Qwen 看
    /// pi auth.json 里有没有百炼 Token Plan 的 key；opencode 兼容"OpenCode
    /// 本体没装、只在 pi 里挂 Go 套餐"的形态）。
    Credential(fn() -> bool),
}

pub struct AgentProbe {
    pub id: &'static str,
    pub probe: Probe,
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_default()
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
            // opencode 卡片有两个来源：本地日志（看数据根）与 Go 套餐配额（看
            // 凭据）。OpenCode 本体没装、只在 pi 里挂 Go 套餐时，凭据落点
            // （环境变量、OpenCode auth.json、pi auth.json）里有 opencode-go
            // key 也算检测到（deepseek 探针同款）。
            id: "opencode",
            probe: Probe::Credential(|| {
                opencode_data_dir().exists() || coding_quota::opencode_go_credential_available()
            }),
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
            // 桌面版、独立 IDE 与 CLI（agy）的数据目录，与 AntigravityAdapter::detected() 同源。
            id: "antigravity",
            probe: Probe::Paths(antigravity_data_dirs()),
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
            // DeepSeek 是配额-only（只拉官方余额，没有本地日志），探针看三处
            // 凭据落点（环境变量、OpenCode auth.json、pi auth.json）里有没有 key。
            id: "deepseek",
            probe: Probe::Credential(coding_quota::deepseek_credential_available),
        },
        AgentProbe {
            id: "dsh",
            probe: Probe::Paths(vec![crate::adapters::dsh_home()]),
        },
        AgentProbe {
            id: "grok",
            probe: Probe::Paths(vec![crate::adapters::grok_home()]),
        },
        AgentProbe {
            // 与 grokbot.rs 的 grokbot_data_dir() 同源：Grok Bot 桌面应用
            // 的 Electron userData 目录（装了就有，登录与否不影响检测）。
            id: "grokbot",
            probe: Probe::Paths(vec![crate::adapters::grokbot_data_dir()]),
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
        AgentProbe {
            // 与 CursorAdapter::detected() 同源：Cursor 的全局状态库。
            id: "cursor",
            probe: Probe::Paths(vec![crate::adapters::cursor_state_db()]),
        },
        AgentProbe {
            // 与 MinimaxAdapter::detected() 同源：数据根受环境变量覆盖。
            id: "minimax",
            probe: Probe::Paths(vec![crate::adapters::minimax_data_dir()]),
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
        // 这两个受环境变量覆盖，必须走与 adapter 相同的解析。opencode 已改
        // 为凭据探针（闭包在 table() 里，内部仍引用 opencode_data_dir()），
        // 不在本断言的守护范围内。
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
        assert_eq!(
            paths(by_id("cursor")),
            vec![crate::adapters::cursor_state_db()]
        );
        assert_eq!(
            paths(by_id("minimax")),
            vec![crate::adapters::minimax_data_dir()]
        );
        assert_eq!(paths(by_id("dsh")), vec![crate::adapters::dsh_home()]);
        assert_eq!(
            paths(by_id("grokbot")),
            vec![crate::adapters::grokbot_data_dir()]
        );
        assert_eq!(paths(by_id("antigravity")), antigravity_data_dirs());
    }
}
