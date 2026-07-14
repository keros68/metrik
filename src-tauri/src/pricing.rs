//! 静态成本估算定价表（美元每百万 token）。
//!
//! 数据来源：OpenAI 官方定价页（<https://openai.com/api/pricing/>）与
//! Anthropic 官方定价页（<https://www.anthropic.com/pricing>），调研/核对日期
//! 2026-07-13（见 `PRICING_AS_OF`）。
//!
//! - OpenAI 的 prompt 缓存写入不计费，所以 `cache_write` 一律记 0。
//! - Anthropic 的缓存写入价按 5 分钟 TTL 场景的 1.25× 基础输入价**近似**给出
//!   （官方按 TTL 分级定价，这里不区分 TTL，只取最常见档位的近似值，估算存在
//!   系统性偏差，不是账单精确值）。
//! - GLM（智谱）官方定价页公开的人民币价格与第三方转载相互矛盾，未能核实到
//!   稳定口径，故不纳入定价表；GLM、opencode 使用的模型以及任何未匹配到前缀
//!   的模型一律归入 unpriced，不猜价格。
//!
//! 匹配规则：对事件的 `model` 字符串做**最长前缀匹配**，例如
//! `claude-sonnet-4-5-20250929` 命中 `claude-sonnet-4-5`；
//! `gpt-5.2-codex` 优先于 `gpt-5.2`、`gpt-5.1-codex`/`gpt-5.1`、`gpt-5` 命中，
//! 因为按前缀长度降序比较，不依赖表内顺序。

/// 定价表最后核实日期，透传给前端做"估算截至"标注。
pub const PRICING_AS_OF: &str = "2026-07-13";

/// 单个模型的分量单价，单位：美元 / 百万 token。
#[derive(Clone, Copy, Debug)]
pub struct Pricing {
    pub input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
}

const PRICING_TABLE: &[(&str, Pricing)] = &[
    // --- OpenAI（cache_write 免费，记 0） ---
    (
        "gpt-5.2-codex",
        Pricing {
            input: 1.75,
            cache_read: 0.175,
            cache_write: 0.0,
            output: 14.0,
        },
    ),
    (
        "gpt-5.2",
        Pricing {
            input: 1.75,
            cache_read: 0.175,
            cache_write: 0.0,
            output: 14.0,
        },
    ),
    (
        "gpt-5.1-codex",
        Pricing {
            input: 1.25,
            cache_read: 0.125,
            cache_write: 0.0,
            output: 10.0,
        },
    ),
    (
        "gpt-5.1",
        Pricing {
            input: 1.25,
            cache_read: 0.125,
            cache_write: 0.0,
            output: 10.0,
        },
    ),
    (
        "gpt-5-codex",
        Pricing {
            input: 1.25,
            cache_read: 0.125,
            cache_write: 0.0,
            output: 10.0,
        },
    ),
    (
        "gpt-5-mini",
        Pricing {
            input: 0.25,
            cache_read: 0.025,
            cache_write: 0.0,
            output: 2.0,
        },
    ),
    (
        "gpt-5",
        Pricing {
            input: 1.25,
            cache_read: 0.125,
            cache_write: 0.0,
            output: 10.0,
        },
    ),
    // --- Anthropic（cache_write 为 5 分钟 TTL 近似值） ---
    (
        "claude-fable-5",
        Pricing {
            input: 10.0,
            cache_read: 1.0,
            cache_write: 12.5,
            output: 50.0,
        },
    ),
    (
        "claude-sonnet-5",
        Pricing {
            input: 3.0,
            cache_read: 0.3,
            cache_write: 3.75,
            output: 15.0,
        },
    ),
    (
        "claude-opus-4-8",
        Pricing {
            input: 5.0,
            cache_read: 0.5,
            cache_write: 6.25,
            output: 25.0,
        },
    ),
    (
        "claude-opus-4-5",
        Pricing {
            input: 5.0,
            cache_read: 0.5,
            cache_write: 6.25,
            output: 25.0,
        },
    ),
    (
        "claude-sonnet-4-5",
        Pricing {
            input: 3.0,
            cache_read: 0.3,
            cache_write: 3.75,
            output: 15.0,
        },
    ),
    (
        "claude-sonnet-4",
        Pricing {
            input: 3.0,
            cache_read: 0.3,
            cache_write: 3.75,
            output: 15.0,
        },
    ),
    (
        "claude-3-7-sonnet",
        Pricing {
            input: 3.0,
            cache_read: 0.3,
            cache_write: 3.75,
            output: 15.0,
        },
    ),
    (
        "claude-3-5-sonnet",
        Pricing {
            input: 3.0,
            cache_read: 0.3,
            cache_write: 3.75,
            output: 15.0,
        },
    ),
    (
        "claude-haiku-4-5",
        Pricing {
            input: 1.0,
            cache_read: 0.1,
            cache_write: 1.25,
            output: 5.0,
        },
    ),
    (
        "claude-opus-4-1",
        Pricing {
            input: 15.0,
            cache_read: 1.5,
            cache_write: 18.75,
            output: 75.0,
        },
    ),
    (
        "claude-opus-4",
        Pricing {
            input: 15.0,
            cache_read: 1.5,
            cache_write: 18.75,
            output: 75.0,
        },
    ),
];

/// 对 `model` 做最长前缀匹配，返回定价；未匹配到任何前缀则返回 `None`
/// （调用方应将这些 token 归入 unpriced，不得臆造价格）。
pub fn price_for(model: &str) -> Option<Pricing> {
    PRICING_TABLE
        .iter()
        .filter(|(prefix, _)| model.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, pricing)| *pricing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_exact_and_dated_suffix() {
        let base = price_for("claude-sonnet-4-5").expect("priced");
        let dated = price_for("claude-sonnet-4-5-20250929").expect("priced");
        assert_eq!(base.input, dated.input);
        assert_eq!(base.output, dated.output);
    }

    #[test]
    fn kimi_models_stay_unpriced_rather_than_borrow_another_vendors_rate() {
        // Kimi 是订阅制，公开单价未经核实：宁可归入"未计价"也不臆造价格。
        assert!(price_for("kimi-code/kimi-for-coding").is_none());
        assert!(price_for("kimi-for-coding").is_none());
    }

    #[test]
    fn prefers_longest_prefix_gpt_5_2_codex() {
        let pricing = price_for("gpt-5.2-codex").expect("priced");
        assert_eq!(pricing.input, 1.75);
        assert_eq!(pricing.output, 14.0);
    }

    #[test]
    fn prefers_longest_prefix_gpt_5_1_codex_over_gpt_5_1() {
        let pricing = price_for("gpt-5.1-codex-20260101").expect("priced");
        assert_eq!(pricing.input, 1.25);
        assert_eq!(pricing.output, 10.0);
    }

    #[test]
    fn gpt_5_mini_does_not_match_bare_gpt_5() {
        let pricing = price_for("gpt-5-mini").expect("priced");
        assert_eq!(pricing.input, 0.25);
        assert_eq!(pricing.output, 2.0);
    }

    #[test]
    fn unknown_model_is_unpriced() {
        assert!(price_for("glm-4.7").is_none());
        assert!(price_for("unknown").is_none());
        assert!(price_for("").is_none());
    }
}
