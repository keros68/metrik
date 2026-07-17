//! 成本估算定价表（美元每百万 token）。
//!
//! 数据来源：LiteLLM 的公开价格表（`model_prices_and_context_window.json`），
//! 只取 openai 与 anthropic 两个 provider，构建期由
//! `scripts/update-pricing.mjs` 生成 `pricing_table.rs`（`npm run pricing:update`）。
//! 运行时不联网——价格随发版更新，留在 git 里可审计。
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
//! GLM(zcode)、Kimi、OpenCode 用的模型不在表里，一律 unpriced：它们走订阅制
//! coding plan，按 token 估价本就无意义；LiteLLM 里那些名字只有 Bedrock/Azure 等
//! 第三方转售价，拿来当官方价就是猜价格。
//!
//! 缓存口径：OpenAI 的 prompt 缓存写入不计费（LiteLLM 里无该字段 → 记 0）；
//! Anthropic 按 TTL 分级，LiteLLM 给的是最常见的 5 分钟档，1 小时档更贵——
//! 长 TTL 场景会低估。这是估算，不是账单。

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

/// 返回 `model` 的定价；表里没有则返回 `None`（调用方归入 unpriced，
/// 不得臆造价格）。见模块文档：只精确匹配，日期快照后缀除外。
pub fn price_for(model: &str) -> Option<Pricing> {
    exact(model).or_else(|| exact(strip_date_suffix(model)?))
}

fn exact(model: &str) -> Option<Pricing> {
    PRICING_TABLE
        .binary_search_by(|(name, _)| (*name).cmp(model))
        .ok()
        .map(|index| PRICING_TABLE[index].1)
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
    fn subscription_billed_agents_stay_unpriced_rather_than_borrow_a_resale_rate() {
        // GLM/Kimi 走订阅 coding plan；LiteLLM 只有第三方转售价，不能冒充官方价。
        assert!(price_for("kimi-code/k3").is_none());
        assert!(price_for("GLM-5.2").is_none());
        assert!(price_for("glm-5-turbo").is_none());
    }

    #[test]
    fn unknown_model_is_unpriced() {
        assert!(price_for("unknown").is_none());
        assert!(price_for("").is_none());
        // 未知模型带日期后缀也不能靠剥后缀蒙混过关。
        assert!(price_for("totally-made-up-20260101").is_none());
    }
}
