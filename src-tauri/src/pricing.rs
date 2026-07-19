//! 成本估算定价表（美元每百万 token）。
//!
//! 数据来源：LiteLLM 的公开价格表（`model_prices_and_context_window.json`），
//! 只取 openai / anthropic / moonshot / zai / gemini 五个官方第一方 API 的
//! provider，构建期由 `scripts/update-pricing.mjs` 生成 `pricing_table.rs`
//! （`npm run pricing:update`）。运行时不联网——价格随发版更新，留在 git 里可审计。
//!
//! ## 匹配规则：精确匹配，绝不前缀猜测
//!
//! 只认表里**完全同名**的模型；仅有一个例外，是把 `-YYYYMMDD` 日期快照后缀剥掉
//! 再试一次（`claude-haiku-4-5-20251001` 与 `claude-haiku-4-5` 是同一模型的两种
//! 写法，同价），这是别名归一化，不是猜价。
//!
//! 这里曾经用「最长前缀匹配」，结果是一场事故：表最新只到 `gpt-5.2`，而实际用的
//! 是 `gpt-5.6-sol`/`gpt-5.5`，于是它们静默命中了 `gpt-5` 的老价格——占 60% 用量
//! 的模型被按低 74% 的价格估算，总成本低估 42%。前缀匹配把"猜价格"伪装成了特性，
//! 正面违反 AGENTS.md 的硬约束。**匹配不上就归入 unpriced，不要再加兜底。**
//!
//! ## 覆盖范围
//!
//! 表内是五个第一方官方 API 的价目（生成时剥掉 LiteLLM 键的 provider 前缀，
//! 按裸模型名匹配）。OpenCode、Antigravity 等直连这些官方 API 的用量因此可以
//! 计价（如 kimi-k2.5、glm-4.6、gemini-3-flash-preview）。
//!
//! 订阅制 coding plan 的专属模型 ID 一律 unpriced：Kimi Code 的 kimi-for-coding、
//! ZCode coding-plan 的 GLM-5.2 等。订阅额度按周期重置、不按 token 卖；
//! LiteLLM 里那些名字的 Bedrock/Azure/Cloudflare 条目是第三方转售价，拿来当
//! 官方价就是猜价格。Kimi 官方文档只说 Extra Usage 按量计费且"接近开放平台
//! 官方 API 价"，但未公布订阅模型 ID 的逐 token 价目——官方公布前不加。
//! 同理，带 -preview 后缀的官方价不补给稳定版名字（gemini-3.1-pro 不计价）。
//!
//! 唯一的窄例外是「同一模型」别名：Kimi Code 订阅用量记的模型是 `kimi-code/k3`，
//! 它就是 Kimi K3 这个模型本身——用 K3 的官方 API 价做估算，正是官方"接近开放
//! 平台官方 API 价"的合理近似（成本页始终标注为估算，不与官方账单混淆）。
//! 不借第三方转售价、不映射到别的模型，例外只此一类，见 SUBSCRIPTION_ALIASES。
//!
//! 缓存口径：OpenAI 的 prompt 缓存写入不计费（LiteLLM 里无该字段 → 记 0）；
//! Anthropic 按 TTL 分级，LiteLLM 给的是最常见的 5 分钟档，1 小时档更贵——
//! 长 TTL 场景会低估。moonshot / zai / gemini 的缓存写入 LiteLLM 同样无字段，
//! 记 0。这是估算，不是账单。

#[path = "pricing_table.rs"]
mod table;

pub use table::PRICING_AS_OF;
use table::PRICING_TABLE;

/// 单个模型的分量单价，单位：美元 / 百万 token。
#[derive(Clone, Copy, Debug)]
pub struct Pricing {
    pub input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
}

/// 手动补充的官方第一方价目（生成表之外）：模型太新、LiteLLM 尚未收录时
/// 按官方定价页临时补齐，收录后即可删。查找时先于生成表命中。
/// 来源：Moonshot 官方定价页（2026-07-18 核对，经多方转载交叉验证）：
/// kimi-k3 输入 $3.00/M、缓存命中 $0.30/M、输出 $15.00/M（1M 上下文旗舰）。
const MANUAL_PRICING: &[(&str, Pricing)] = &[(
    "kimi-k3",
    Pricing {
        input: 3.0,
        cache_read: 0.3,
        cache_write: 0.0,
        output: 15.0,
    },
)];

/// 订阅制 coding plan 的模型 ID → 同一模型的官方第一方 API 价（估算口径）。
/// 仅限"同一模型"：Kimi Code 订阅记的 `kimi-code/k3` 就是 kimi-k3 本身，
/// 官方称 Extra Usage"接近开放平台官方 API 价"；成本页始终标注为估算。
/// 没有官方价的订阅 ID（kimi-for-coding、GLM-5.2 等）继续 unpriced。
const SUBSCRIPTION_ALIASES: &[(&str, &str)] = &[("kimi-code/k3", "kimi-k3")];

/// 返回 `model` 的定价；表里没有则返回 `None`（调用方归入 unpriced，
/// 不得臆造价格）。见模块文档：只精确匹配，日期快照后缀与订阅别名除外。
pub fn price_for(model: &str) -> Option<Pricing> {
    exact(model)
        .or_else(|| exact(strip_date_suffix(model)?))
        .or_else(|| subscription_alias(model).and_then(exact))
}

fn exact(model: &str) -> Option<Pricing> {
    MANUAL_PRICING
        .iter()
        .find(|(name, _)| *name == model)
        .map(|(_, pricing)| *pricing)
        .or_else(|| {
            PRICING_TABLE
                .binary_search_by(|(name, _)| (*name).cmp(model))
                .ok()
                .map(|index| PRICING_TABLE[index].1)
        })
}

/// 订阅别名只按全名命中（`kimi-code/k3` → `kimi-k3`），不做任何前缀猜测。
fn subscription_alias(model: &str) -> Option<&str> {
    SUBSCRIPTION_ALIASES
        .iter()
        .find(|(alias, _)| *alias == model)
        .map(|(_, target)| *target)
}

/// `claude-haiku-4-5-20251001` → `claude-haiku-4-5`。只认 8 位数字结尾，
/// 所以 `gpt-5.6-sol` 这种非日期后缀不会被剥掉去碰运气。
fn strip_date_suffix(model: &str) -> Option<&str> {
    let (base, date) = model.rsplit_once('-')?;
    (date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit())).then_some(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_nonempty_so_binary_search_is_valid() {
        assert!(PRICING_TABLE.len() > 50, "生成的价格表异常地小");
        assert!(
            PRICING_TABLE.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "价格表必须按模型名严格有序，否则 binary_search 会漏查",
        );
    }

    #[test]
    fn dated_snapshot_falls_back_to_the_undated_alias() {
        let base = price_for("claude-haiku-4-5").expect("priced");
        // LiteLLM 恰好也收录了这个日期版；剥后缀的回退对它未收录的新快照才关键。
        let dated = price_for("claude-haiku-4-5-20251001").expect("priced");
        assert_eq!(base.input, dated.input);
        assert_eq!(base.output, dated.output);

        // 表里没有的未来快照，靠剥后缀命中别名。
        let future = price_for("claude-opus-4-8-20260401").expect("priced");
        assert_eq!(future.input, price_for("claude-opus-4-8").unwrap().input);
    }

    #[test]
    fn new_generation_never_borrows_an_older_models_price() {
        // 这是回归测试：前缀匹配曾让 gpt-5.6-sol 命中 gpt-5 的价格，低估 74%。
        let sol = price_for("gpt-5.6-sol").expect("priced");
        let five = price_for("gpt-5").expect("priced");
        assert_ne!(
            sol.input, five.input,
            "gpt-5.6-sol 必须用自己的价格，不能退回 gpt-5",
        );

        // 非日期后缀不得被剥掉去撞别的模型。
        assert!(strip_date_suffix("gpt-5.6-sol").is_none());
        assert!(strip_date_suffix("gpt-5-mini").is_none());
    }

    #[test]
    fn subscription_only_model_ids_stay_unpriced() {
        // 订阅 coding plan 的专属 ID 没有官方按 token 价目：不得借第三方
        // 转售价或同系模型的价格蒙混（Kimi Code 订阅、ZCode coding plan）。
        assert!(price_for("kimi-code/kimi-for-coding").is_none());
        assert!(price_for("kimi-for-coding").is_none());
        assert!(price_for("GLM-5.2").is_none());
        assert!(price_for("glm-5-turbo").is_none());
        // 有 -preview 后缀的官方价也不补给稳定版名字。
        assert!(price_for("gemini-3.1-pro").is_none());
    }

    #[test]
    fn kimi_k3_priced_from_official_rates_including_subscription_alias() {
        // 手动补充的官方价（LiteLLM 未收录时临时补齐）：K3 输入 $3、
        // 缓存 $0.3、输出 $15（2026-07-18 官方定价页核对）。
        let direct = price_for("kimi-k3").expect("kimi-k3 priced");
        assert_eq!(direct.input, 3.0);
        assert_eq!(direct.cache_read, 0.3);
        assert_eq!(direct.output, 15.0);
        // 窄例外：Kimi Code 订阅的 kimi-code/k3 就是 K3 本身，按同一官方价估算。
        let aliased = price_for("kimi-code/k3").expect("alias priced");
        assert_eq!(aliased.input, direct.input);
        assert_eq!(aliased.output, direct.output);
        // 其他订阅 ID 仍不得蒙混（见上一条测试）。
        assert!(price_for("kimi-code/k4").is_none());
    }

    #[test]
    fn first_party_api_models_are_priced_by_bare_name() {
        // OpenCode / Antigravity 等直连官方 API 的用量按第一方价目计价
        // （LiteLLM 键的 provider 前缀在生成时已剥掉）。
        assert!(price_for("kimi-k2.5").is_some());
        assert!(price_for("glm-4.6").is_some());
        assert!(price_for("gemini-3-flash-preview").is_some());
        assert!(price_for("gemini-2.5-pro").is_some());
    }

    #[test]
    fn unknown_model_is_unpriced() {
        assert!(price_for("unknown").is_none());
        assert!(price_for("").is_none());
        // 未知模型带日期后缀也不能靠剥后缀蒙混过关。
        assert!(price_for("totally-made-up-20260101").is_none());
    }
}
