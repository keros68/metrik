//! Hermes（Nous Research 的 agent CLI）是 harness，不是计量身份：它没有自己的
//! coding plan，用量计费发生在它配置的上游 provider 上（本机核实：GLM Coding
//! Plan、Kimi Code、ChatGPT/Codex 订阅、DeepSeek/MiMo/SenseNova 等直连 API）。
//! 本模块把一次用量记录的路由映射到 Metrik 的计量 Agent，规则与 pi_providers
//! 一致：走别家套餐的记到对应卡片，其余留在 hermes 名下。
//!
//! 分类依据是 **billing_base_url**，不是 provider 名：hermes 的 provider 字段是
//! 用户自己起的名字（本机同一上游分别被记作 `custom`、`custom:kimi`、`zai`、
//! `xiaomi`），不可靠；base_url 是实际请求的路由，稳定。只认各官方 coding
//! plan 的专属端点——plain API 端点（如 bigmodel 无 `/coding` 的按量端点）不
//! 消耗套餐额度，不归属到对应卡片。
//!
//! 归属发生在 adapter 层（写入 `usage_event.adapter_id`），一次入库、处处一致，
//! 与 pi 相同；账本层的分量最大值合并按事件键的 `hermes:` 前缀识别（hermes 的
//! 用量行是累计值，每次扫描都会重新观察到更大的数，见 storage）。

/// 把 hermes 一次用量记录的路由（billing_provider, billing_base_url）映射到
/// 计量 Agent id。
pub fn credited_agent(
    billing_provider: Option<&str>,
    billing_base_url: Option<&str>,
) -> &'static str {
    let route = billing_base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    let Some(route) = route else {
        // 没有路由的记录（早期版本只写了 model）：provider 字段同样是用户起的
        // 名字，认不得，留在 hermes。
        return "hermes";
    };

    // GLM Coding Plan：bigmodel 国内端与 z.ai 国际端的 coding 专属端点，
    // 与 pi 的 zai* provider 同一套餐额度 → GLM 卡片。
    if (route.contains("open.bigmodel.cn") || route.contains("api.z.ai"))
        && route.contains("/coding")
    {
        return "zcode";
    }
    // Kimi Code 订阅端点（模型 k3 / kimi-for-coding）→ Kimi 卡片。
    if route.contains("api.kimi.com") && route.contains("/coding") {
        return "kimi";
    }
    // ChatGPT 后端的 Codex 订阅通道（billing_mode 常为 subscription_included）
    // → Codex 卡片。
    if route.contains("chatgpt.com") && route.contains("/codex") {
        return "codex";
    }
    let _ = billing_provider;
    "hermes"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_plan_endpoints_credit_their_own_cards() {
        // 本机真实路由（2026-08-31 核对）。
        assert_eq!(
            credited_agent(
                Some("custom"),
                Some("https://open.bigmodel.cn/api/coding/paas/v4"),
            ),
            "zcode"
        );
        assert_eq!(
            credited_agent(
                Some("custom:zai"),
                Some("https://open.bigmodel.cn/api/coding/paas/v4")
            ),
            "zcode"
        );
        assert_eq!(
            credited_agent(Some("zai"), Some("https://api.z.ai/api/coding/paas/v4")),
            "zcode"
        );
        assert_eq!(
            credited_agent(Some("custom:kimi"), Some("https://api.kimi.com/coding/v1")),
            "kimi"
        );
        assert_eq!(
            credited_agent(Some("kimi"), Some("https://api.kimi.com/coding")),
            "kimi"
        );
        assert_eq!(
            credited_agent(
                Some("openai-codex"),
                Some("https://chatgpt.com/backend-api/codex/"),
            ),
            "codex"
        );
    }

    #[test]
    fn plain_api_endpoints_and_unknown_routes_stay_on_hermes() {
        // 不带 /coding 的 bigmodel 按量端点不消耗套餐，不归属 GLM 卡。
        assert_eq!(
            credited_agent(Some("custom"), Some("https://open.bigmodel.cn/api/paas/v4")),
            "hermes"
        );
        // 直连 API：DeepSeek、MiMo Token Plan、SenseNova、StepFun。
        assert_eq!(
            credited_agent(
                Some("custom"),
                Some("https://token-plan-cn.xiaomimimo.com/v1")
            ),
            "hermes"
        );
        assert_eq!(
            credited_agent(Some("custom"), Some("https://api.stepfun.com/step_plan/v1")),
            "hermes"
        );
        // 早期记录没有路由：provider 名不可靠，一律留 hermes。
        assert_eq!(credited_agent(Some("custom"), None), "hermes");
        assert_eq!(credited_agent(Some(""), Some("  ")), "hermes");
        assert_eq!(credited_agent(None, None), "hermes");
    }
}
