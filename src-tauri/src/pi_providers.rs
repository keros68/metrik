//! pi（badlogic/pi-mono）是 harness，不是计量身份：它没有自己的 coding plan，
//! 用量计费发生在它调用的 provider 上。本模块把 pi 会话日志里的 provider id
//! 映射到 Metrik 的计量 Agent。
//!
//! 映射依据 pi 内置目录（models-store.json 的 provider 名单，2026-08 提取）：
//! GLM Coding Plan（z.ai / BigModel CN）→ `zcode`（GLM 卡片，与 zcode 桌面端
//! 同一账户额度）；Qwen Token Plan 各变体 → `qwen`；OpenCode Go（`opencode-go`，
//! 2026-09 真机补录）→ `opencode`；其余 provider（Anthropic、OpenAI 等）由 pi
//! 自带凭据直连计费，不经过既有客户端的额度，留在 `pi` 名下。
//!
//! 归属发生在 adapter 层（写入 `usage_event.adapter_id`），一次入库、处处一致：
//! 图表、模型榜、会话流、成本估算与同步导出自动跟随，无需各查询点单独判断。
//!
//! GLM 配额同样如此：`resolve_glm_credentials` 把 pi auth.json 里的 GLM key
//! 并入候选，读数归 GLM 卡片——与用量归属对齐。

/// 把 pi 的 provider id 映射到计量 Agent id。
pub fn credited_agent(pi_provider: Option<&str>) -> &'static str {
    let Some(provider) = pi_provider.map(str::trim).filter(|value| !value.is_empty()) else {
        // 无 provider 的记录（旧格式/工具内嵌调用）：无法归户，留在 pi。
        return "pi";
    };
    match provider {
        // GLM Coding Plan：z.ai 国际端与 BigModel 国内端是同一套餐的两个区域。
        "zai" | "zai-coding" | "zai-coding-cn" => "zcode",
        // 百炼个人 Token Plan：pi 目录里的三个变体打同一个套餐额度。
        "qwen-token-plan" | "qwen-token-plan-cn" | "qwen-token-plan-individual" => "qwen",
        // OpenCode Go：opencode.ai 网关计费的同一套餐，与跑在哪个 harness 无关；
        // 配额源（coding_quota 的 Go 配额）同挂 opencode 卡片。
        "opencode-go" => "opencode",
        _ => "pi",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glm_and_qwen_coding_plans_credit_their_own_cards() {
        assert_eq!(credited_agent(Some("zai")), "zcode");
        assert_eq!(credited_agent(Some("zai-coding")), "zcode");
        assert_eq!(credited_agent(Some("zai-coding-cn")), "zcode");
        assert_eq!(credited_agent(Some("qwen-token-plan")), "qwen");
        assert_eq!(credited_agent(Some("qwen-token-plan-cn")), "qwen");
        assert_eq!(credited_agent(Some("qwen-token-plan-individual")), "qwen");
        // 真机（2026-09）：pi 会话日志里的 provider id 就是 opencode-go。
        assert_eq!(credited_agent(Some("opencode-go")), "opencode");
    }

    #[test]
    fn unknown_providers_and_missing_values_stay_on_pi() {
        // pi 直连的 Anthropic/OpenAI 等不经过既有客户端的额度，留在 pi 名下。
        assert_eq!(credited_agent(Some("anthropic")), "pi");
        assert_eq!(credited_agent(Some("openai")), "pi");
        assert_eq!(credited_agent(Some("  ")), "pi");
        assert_eq!(credited_agent(None), "pi");
    }
}
