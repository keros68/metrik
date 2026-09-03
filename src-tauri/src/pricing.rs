//! 成本估算定价表（美元每百万 token）。
//!
//! 数据来源：LiteLLM 的公开价格表（`model_prices_and_context_window.json`），
//! 只取 openai / anthropic / moonshot / zai / gemini / xai 六个官方第一方 API 的
//! provider，构建期由 `scripts/update-pricing.mjs` 生成 `pricing_table.rs`
//! （`npm run pricing:update`）。运行时不联网——价格随发版更新，留在 git 里可审计。
//! `.github/workflows/pricing-refresh.yml` 每周跑一次并开 PR，人工核对 diff 后合并。
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
//! 正面违反 docs/PRODUCT-CONSTRAINTS.md 的数据真实性约束。**匹配不上就归入 unpriced，不要再加兜底。**
//!
//! ## 覆盖范围
//!
//! 表内是六个第一方官方 API 的价目（生成时剥掉 LiteLLM 键的 provider 前缀，
//! 按裸模型名匹配）。OpenCode、Antigravity 等直连这些官方 API 的用量因此可以
//! 计价（如 kimi-k2.5、glm-4.6、gemini-3-flash-preview）。
//!
//! 生成表覆盖不到的官方 API（DeepSeek、阿里云百炼的 Qwen）按官方定价页手动
//! 补在 MANUAL_PRICING，规则不变：只认官方第一方价目，不借第三方转售价。
//!
//! ## 分时定价
//!
//! DeepSeek 按时段分峰谷，谷段是峰段的 5 折。所以 `price_for` 收事件
//! 时间戳：成本必须逐事件计价，先按模型汇总再乘单价会把时段信息抹掉。
//! 表里存峰段（标准）价，谷段在查表时打折，见 OFF_PEAK_HALF_PRICE。
//!
//! 峰段除了钟点还有一根**星期**轴：官方两个窗口只在周一至周五生效，周六周日
//! 全天谷段（2026-08-23 北京时间起）。而周几要按**北京时间**数，不是 UTC ——
//! 北京早 8 小时，UTC 周五 16:00 起北京已是周六。只看钟点的话，周末落在窗口
//! 里的用量会按峰段价估算，正好高一倍，而所有断言照样绿。
//!
//! 订阅制 coding plan 的专属模型 ID 一律 unpriced：Kimi Code 的 kimi-for-coding 等。
//! 订阅额度按周期重置、不按 token 卖；
//! LiteLLM 里那些名字的 Bedrock/Azure/Cloudflare 条目是第三方转售价，拿来当
//! 官方价就是猜价格。Kimi 官方文档只说 Extra Usage 按量计费且"接近开放平台
//! 官方 API 价"，但未公布订阅模型 ID 的逐 token 价目——官方公布前不加。
//! 同理，带 -preview 后缀的官方价不补给稳定版名字（gemini-3.1-pro 不计价）。
//!
//! 唯一的窄例外是「同一模型」别名：Kimi Code 订阅记的 `kimi-code/k3` 就是
//! Kimi K3 本身；ZCode coding-plan 记的 `GLM-5.2` 只是 glm-5.2 的大小写变体。
//! 都按同一模型的官方第一方 API 价估算（成本页始终标注为估算，不与官方账单
//! 混淆）。不借第三方转售价、不映射到别的模型，见 SUBSCRIPTION_ALIASES。
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

impl Pricing {
    /// 谷段 5 折：四个分量同比例打折（DeepSeek 官方谷段价正是峰段的一半）。
    fn halved(self) -> Self {
        Self {
            input: self.input / 2.0,
            cache_read: self.cache_read / 2.0,
            cache_write: self.cache_write / 2.0,
            output: self.output / 2.0,
        }
    }
}

/// 手动补充的官方第一方价目（生成表之外）：模型太新、LiteLLM 尚未收录时
/// 按官方定价页临时补齐，**收录后即删**——两处都留会让手工值一直压过生成值，
/// 官方调价后刷新生成表也不生效。查找时先于生成表命中。
///
/// glm-5.2 / glm-5.3 / glm-5.3-flash / kimi-k3 / kimi-k2.7-code 已在 2026-09-03
/// 的刷新里被 LiteLLM 收录，数值与手工值一致，故从这里删掉；下面的测试仍按
/// 各自官方定价页的数值断言，钉的是价格本身，不是它存在哪张表里。
///
/// 来源：
/// - glm-5-turbo：z.ai 官方定价页 docs.z.ai/guides/overview/pricing
///   （2026-07-20 核对；同页 glm-5/glm-5.1 数值与 LiteLLM 生成表完全一致，
///   佐证来源可信）。缓存写入官方标注限时免费 → 记 0。
/// - deepseek-v4-pro / deepseek-v4-flash：DeepSeek 官方定价页
///   api-docs.deepseek.com/quick_start/pricing（2026-08-20 核对）。存的是峰段
///   标准价，谷段由 OFF_PEAK_HALF_PRICE 打 5 折。缓存写入官方不单独计费 → 记 0。
/// - qwen3.8-max：阿里云百炼（2026-08-20 核对）。输入 $2/M、输出 $6/M 是官方
///   公布价；缓存两项官方尚未逐模型列出，按百炼计费文档的通用规则推算——
///   命中按输入价 10%（$0.2/M）、显式缓存写入按 125%（$2.5/M）。这两项是
///   规则推算不是逐模型报价，官方列出后应替换。
const MANUAL_PRICING: &[(&str, Pricing)] = &[
    (
        "deepseek-v4-flash",
        Pricing {
            input: 0.44,
            cache_read: 0.014,
            cache_write: 0.0,
            output: 1.32,
        },
    ),
    (
        "deepseek-v4-pro",
        Pricing {
            input: 1.32,
            cache_read: 0.044,
            cache_write: 0.0,
            output: 3.96,
        },
    ),
    (
        "glm-5-turbo",
        Pricing {
            input: 1.2,
            cache_read: 0.24,
            cache_write: 0.0,
            output: 4.0,
        },
    ),
    (
        "qwen3.8-max",
        Pricing {
            input: 2.0,
            cache_read: 0.2,
            cache_write: 2.5,
            output: 6.0,
        },
    ),
];

/// 官方按时段分价的模型：谷段单价 = 表内标准价 × 0.5。
/// DeepSeek 定价页（2026-08-23 核对）：峰段 01:00–04:00 与 06:00–10:00 UTC、
/// **且只在周一至周五**，其余时段为谷段、5 折。表内存峰段标准价，与其他模型
/// 「存官方标价」一致。
const OFF_PEAK_HALF_PRICE: &[&str] = &["deepseek-v4-flash", "deepseek-v4-pro"];

/// 「周末全天谷段」这条规则生效的时刻：北京时间 2026-08-23 00:00，
/// 即 2026-08-22 16:00 UTC。**此前的事件不按这条规则重算** —— 否则一次升级
/// 会把用户看过的历史成本悄悄改成另一个数。
const WEEKEND_OFF_PEAK_FROM_MS: i64 = 1_787_414_400_000;

/// 订阅制 coding plan 的模型 ID → 同一模型的官方第一方 API 价（估算口径）。
/// 仅限"同一模型"，且要有官方佐证，不是看名字像就归一：
/// - Kimi Code 订阅记的 `kimi-code/k3` 就是 kimi-k3 本身，
///   官方称 Extra Usage"接近开放平台官方 API 价"；成本页始终标注为估算。
///   `kimi-code/k3-256k` 是官方文档写明的同一模型的 256K 上下文版（"The 256K
///   context version of Kimi K3…delivers the same results"，2026-08-31 核对），
///   Hermes 记的裸名 `k3` 同样是它——按同一官方 API 价估算。官方明说 k3 (1M)
///   的订阅额度消耗约为 k3-256k 的两倍，但那讲的是套餐额度，不是逐 token 价；
///   官方未公布 256k 档单独的 API 价目，Kimi 官方又历来按上下文档分价
///   （kimi-latest-8k/32k/128k），官方公布后再分档。
/// - `kimi-for-coding` 是 Kimi Code 文档模型表里的固定模型 ID，版本一栏写明
///   "Kimi K2.7 Code"（2026-08-31 核对），按 kimi-k2.7-code 官方 API 价估算。
///   其高速变体 kimi-for-coding-highspeed 官方价目单列（标准版两倍），未收录。
/// - ZCode coding-plan 记的 `GLM-5.2`、`GLM-5.3`、`GLM-5.3-Flash` 只是
///   glm-5.2、glm-5.3、glm-5.3-flash 的大小写变体，同一模型。
/// - Grok Build 订阅记的 `grok-4.5-build`：docs.x.ai 模型页里 grok-4.5 的官方
///   别名就含 `grok-build-latest`（Build 产品线 = grok-4.5，2026-08-19 核对），
///   故按 grok-4.5 官方 API 价估算。
/// - Codex 自动评审记的 `codex-auto-review`：OpenAI 官方页
///   alignment.openai.com/auto-review 明写「Auto-review uses GPT-5.4 Thinking
///   (low reasoning)」（2026-08-20 核对），故按 gpt-5.4 官方 API 价估算。
///
/// 没有官方价的订阅 ID 继续unpriced；kimi-for-coding 已有官方佐证（K2.7
/// Code），见上。
const SUBSCRIPTION_ALIASES: &[(&str, &str)] = &[
    ("GLM-5.2", "glm-5.2"),
    ("GLM-5.3", "glm-5.3"),
    ("GLM-5.3-Flash", "glm-5.3-flash"),
    ("kimi-code/k3", "kimi-k3"),
    ("kimi-code/k3-256k", "kimi-k3"),
    ("k3", "kimi-k3"),
    ("kimi-for-coding", "kimi-k2.7-code"),
    ("grok-4.5-build", "grok-4.5"),
    ("codex-auto-review", "gpt-5.4"),
];

/// 返回 `model` 在 `occurred_at_ms` 时刻的定价；表里没有则返回 `None`
/// （调用方归入 unpriced，不得臆造价格）。见模块文档：只精确匹配，日期快照
/// 后缀与订阅别名除外。时间戳只对 OFF_PEAK_HALF_PRICE 里的模型有影响，
/// 其余模型全天一价。
pub fn price_for(model: &str, occurred_at_ms: i64) -> Option<Pricing> {
    let (canonical, price) = resolve(model)?;
    Some(if in_off_peak(canonical, occurred_at_ms) {
        price.halved()
    } else {
        price
    })
}

/// 归一到表里的规范名，连同标准价一起返回。规范名（而不是调用方传进来的
/// 别名）才是查分时定价的依据。
fn resolve(model: &str) -> Option<(&str, Pricing)> {
    if let Some(pricing) = exact(model) {
        return Some((model, pricing));
    }
    if let Some(base) = strip_date_suffix(model) {
        if let Some(pricing) = exact(base) {
            return Some((base, pricing));
        }
    }
    let target = subscription_alias(model)?;
    exact(target).map(|pricing| (target, pricing))
}

/// DeepSeek 峰段是 01:00–04:00 与 06:00–10:00 UTC（左闭右开）、且只在周一至
/// 周五；北京时间的周六周日全天谷段。其余时段为谷段。
fn in_off_peak(canonical: &str, occurred_at_ms: i64) -> bool {
    if !OFF_PEAK_HALF_PRICE.contains(&canonical) {
        return false;
    }
    if occurred_at_ms >= WEEKEND_OFF_PEAK_FROM_MS && in_beijing_weekend(occurred_at_ms) {
        return true;
    }
    // Unix 纪元起点就是 UTC 00:00，UTC 又没有夏令时，整除即可，不必引 chrono。
    let hour = occurred_at_ms.div_euclid(3_600_000).rem_euclid(24);
    !((1..4).contains(&hour) || (6..10).contains(&hour))
}

/// 这一刻在**北京时间**里是不是周六或周日。
///
/// 周几必须按北京时间数：官方那句规则整句写在北京时区里，而北京早 8 小时——
/// UTC 周五 16:00 起那边已是周六，UTC 周日 16:00 起那边已是周一。按 UTC 数的
/// 话，每周有 16 小时判反。（现行两个窗口都够不着这 16 小时，所以今天两种
/// 读法算出来的价一模一样；哪天窗口往后挪一点，按 UTC 数的那份就开始错钱，
/// 而对着已公布窗口写的断言一条都不会红。）
///
/// UTC+8 是固定偏移、无夏令时，加 8 小时再整除即可，同样不必引 chrono。
fn in_beijing_weekend(occurred_at_ms: i64) -> bool {
    let day = (occurred_at_ms + 8 * 3_600_000).div_euclid(86_400_000);
    // 1970-01-01 是星期四，所以 +4 之后 0 = 周日、6 = 周六。
    matches!((day + 4).rem_euclid(7), 0 | 6)
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

    /// 峰谷之外的模型全天一价，用哪个时刻都一样；固定一个（2026-08-20 12:30
    /// UTC）省得每条断言各写各的。
    const ANY_TIME_MS: i64 = 1_787_229_000_000;

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
        let base = price_for("claude-haiku-4-5", ANY_TIME_MS).expect("priced");
        // LiteLLM 恰好也收录了这个日期版；剥后缀的回退对它未收录的新快照才关键。
        let dated = price_for("claude-haiku-4-5-20251001", ANY_TIME_MS).expect("priced");
        assert_eq!(base.input, dated.input);
        assert_eq!(base.output, dated.output);

        // 表里没有的未来快照，靠剥后缀命中别名。
        let future = price_for("claude-opus-4-8-20260401", ANY_TIME_MS).expect("priced");
        assert_eq!(
            future.input,
            price_for("claude-opus-4-8", ANY_TIME_MS).unwrap().input
        );
    }

    #[test]
    fn new_generation_never_borrows_an_older_models_price() {
        // 这是回归测试：前缀匹配曾让 gpt-5.6-sol 命中 gpt-5 的价格，低估 74%。
        let sol = price_for("gpt-5.6-sol", ANY_TIME_MS).expect("priced");
        let five = price_for("gpt-5", ANY_TIME_MS).expect("priced");
        assert_ne!(
            sol.input, five.input,
            "gpt-5.6-sol 必须用自己的价格，不能退回 gpt-5",
        );

        // 非日期后缀不得被剥掉去撞别的模型。
        assert!(strip_date_suffix("gpt-5.6-sol").is_none());
        assert!(strip_date_suffix("gpt-5-mini").is_none());
    }

    #[test]
    fn claude_fable_5_1_keeps_its_own_cache_read_rate() {
        // 官方定价：输入 $10、输出 $50 与 Fable 5 同价，但缓存命中是 $0.25，
        // 不是 Fable 5 的 $1.0。两代同价容易让人以为整行都能沿用，缓存命中
        // 占实际用量的大头，这一栏抄错成本就系统性高估四倍。
        let fable_5_1 = price_for("claude-fable-5-1", ANY_TIME_MS).expect("priced");
        assert_eq!(fable_5_1.input, 10.0);
        assert_eq!(fable_5_1.cache_read, 0.25);
        assert_eq!(fable_5_1.output, 50.0);
        let fable_5 = price_for("claude-fable-5", ANY_TIME_MS).expect("priced");
        assert_eq!(fable_5.cache_read, 1.0);
    }

    #[test]
    fn subscription_only_model_ids_stay_unpriced() {
        // 订阅 coding plan 的专属 ID 没有官方按 token 价目：不得借第三方
        // 转售价或同系模型的价格蒙混（Kimi Code 订阅等）。
        assert!(price_for("kimi-code/kimi-for-coding", ANY_TIME_MS).is_none());
        // 有 -preview 后缀的官方价也不补给稳定版名字。
        assert!(price_for("gemini-3.1-pro", ANY_TIME_MS).is_none());
    }

    #[test]
    fn glm_priced_from_official_rates_including_case_alias() {
        // z.ai 官方定价页（2026-07-20 核对）：glm-5.2 输入 $1.4、缓存 $0.26、
        // 输出 $4.4；glm-5-turbo 输入 $1.2、缓存 $0.24、输出 $4.0。
        let direct = price_for("glm-5.2", ANY_TIME_MS).expect("glm-5.2 priced");
        assert_eq!(direct.input, 1.4);
        assert_eq!(direct.cache_read, 0.26);
        assert_eq!(direct.output, 4.4);
        let turbo = price_for("glm-5-turbo", ANY_TIME_MS).expect("glm-5-turbo priced");
        assert_eq!(turbo.input, 1.2);
        assert_eq!(turbo.output, 4.0);
        // ZCode coding-plan 记的大写 GLM-5.2 是同一模型的大小写变体，同价。
        let aliased = price_for("GLM-5.2", ANY_TIME_MS).expect("alias priced");
        assert_eq!(aliased.input, direct.input);
        assert_eq!(aliased.output, direct.output);

        // glm-5.3 官方定价与 5.2 相同（2026-08-20 核对）；claude/pi 适配器记
        // 小写裸名，ZCode 记大写，两种写法都要计价。
        let five_three = price_for("glm-5.3", ANY_TIME_MS).expect("glm-5.3 priced");
        assert_eq!(five_three.input, 1.4);
        assert_eq!(five_three.cache_read, 0.26);
        assert_eq!(five_three.output, 4.4);
        let five_three_upper = price_for("GLM-5.3", ANY_TIME_MS).expect("alias priced");
        assert_eq!(five_three_upper.input, five_three.input);
        assert_eq!(five_three_upper.output, five_three.output);

        // glm-5.3-flash 官方标准价（2026-08-31 核对）：输入 $0.15、缓存 $0.03、
        // 输出 $0.50。限时五折是折扣不是标价，不进表。ZCode 记的大写
        // GLM-5.3-Flash 是同一模型的大小写变体。
        let flash = price_for("glm-5.3-flash", ANY_TIME_MS).expect("glm-5.3-flash priced");
        assert_eq!(flash.input, 0.15);
        assert_eq!(flash.cache_read, 0.03);
        assert_eq!(flash.output, 0.5);
        let flash_upper = price_for("GLM-5.3-Flash", ANY_TIME_MS).expect("alias priced");
        assert_eq!(flash_upper.input, flash.input);
        assert_eq!(flash_upper.output, flash.output);
    }

    #[test]
    fn kimi_k3_priced_from_official_rates_including_subscription_alias() {
        // Moonshot 官方定价页（2026-07-18 核对）：K3 输入 $3、
        // 缓存 $0.3、输出 $15。
        let direct = price_for("kimi-k3", ANY_TIME_MS).expect("kimi-k3 priced");
        assert_eq!(direct.input, 3.0);
        assert_eq!(direct.cache_read, 0.3);
        assert_eq!(direct.output, 15.0);
        // 窄例外：Kimi Code 订阅的 kimi-code/k3 就是 K3 本身，按同一官方价估算。
        // 256K 上下文版与 Hermes 记的裸名 k3 同为 K3（官方文档佐证，见别名表）。
        let aliased = price_for("kimi-code/k3", ANY_TIME_MS).expect("alias priced");
        assert_eq!(aliased.input, direct.input);
        assert_eq!(aliased.output, direct.output);
        let capped = price_for("kimi-code/k3-256k", ANY_TIME_MS).expect("alias priced");
        assert_eq!(capped.input, direct.input);
        assert_eq!(capped.output, direct.output);
        let bare = price_for("k3", ANY_TIME_MS).expect("alias priced");
        assert_eq!(bare.input, direct.input);
        assert_eq!(bare.output, direct.output);
        // 其他订阅 ID 仍不得蒙混（见上一条测试）。
        assert!(price_for("kimi-code/k4", ANY_TIME_MS).is_none());
        assert!(price_for("kimi-code/kimi-for-coding", ANY_TIME_MS).is_none());

        // kimi-for-coding 官方文档写明是 Kimi K2.7 Code（2026-08-31 核对），
        // 按 kimi-k2.7-code 官方 API 价估算：页面只标人民币（缓存命中 ¥1.30 /
        // 未命中 ¥6.50 / 输出 ¥27），输入输出官方注明与 K2.6 同价、缓存按
        // ¥1.30/¥1.10 等比。
        let k27 = price_for("kimi-for-coding", ANY_TIME_MS).expect("kimi-for-coding priced");
        assert_eq!(k27.input, 0.95);
        assert_eq!(k27.cache_read, 0.19);
        assert_eq!(k27.output, 4.0);
        let k27_api = price_for("kimi-k2.7-code", ANY_TIME_MS).expect("kimi-k2.7-code priced");
        assert_eq!(k27_api.input, k27.input);
        assert_eq!(k27_api.cache_read, k27.cache_read);
        assert_eq!(k27_api.output, k27.output);
    }

    #[test]
    fn grok_45_build_priced_from_official_api_rates() {
        // Grok Build 订阅记的 grok-4.5-build 按 grok-4.5 官方 API 价估算：
        // docs.x.ai 模型页 grok-4.5 的官方别名含 grok-build-latest（Build
        // 产品线 = grok-4.5；in $2 / cache $0.30 / out $6，2026-08-19 与
        // LiteLLM 交叉一致）。直连 xAI API 的裸名也照表计价。
        let aliased = price_for("grok-4.5-build", ANY_TIME_MS).expect("alias priced");
        assert_eq!(aliased.input, 2.0);
        assert_eq!(aliased.cache_read, 0.3);
        assert_eq!(aliased.output, 6.0);
        let direct = price_for("grok-4.5", ANY_TIME_MS).expect("api name priced");
        assert_eq!(direct.input, aliased.input);
        assert_eq!(direct.output, aliased.output);
        // 未佐证的其他订阅变体不得蒙混：前缀匹配是事故，不重演。
        assert!(price_for("grok-4.6-build", ANY_TIME_MS).is_none());
    }

    #[test]
    fn first_party_api_models_are_priced_by_bare_name() {
        // OpenCode / Antigravity 等直连官方 API 的用量按第一方价目计价
        // （LiteLLM 键的 provider 前缀在生成时已剥掉）。
        assert!(price_for("kimi-k2.5", ANY_TIME_MS).is_some());
        assert!(price_for("glm-4.6", ANY_TIME_MS).is_some());
        assert!(price_for("gemini-3-flash-preview", ANY_TIME_MS).is_some());
        assert!(price_for("gemini-2.5-pro", ANY_TIME_MS).is_some());
        // xAI 两个易混淆的裸名都要在表：4.6 的缓存价与 4.5 不同，
        // grok-build-0.1 是独立定价的代码快模型（别名 grok-code-fast 族）。
        // 钉在测试里：LiteLLM 若将来掉条目，重新生成会静默失价。
        let forty_six = price_for("grok-4.6", ANY_TIME_MS).expect("grok-4.6 priced");
        assert_eq!(forty_six.cache_read, 0.5);
        let build_code = price_for("grok-build-0.1", ANY_TIME_MS).expect("grok-build-0.1 priced");
        assert_eq!(build_code.input, 1.0);
        assert_eq!(build_code.output, 2.0);
    }

    #[test]
    fn deepseek_switches_between_peak_and_off_peak_rates() {
        // DeepSeek 官方定价页（2026-08-20 核对）：峰段 01:00–04:00 与
        // 06:00–10:00 UTC 是标准价，其余时段 5 折。表内存峰段价。
        let peak = price_for("deepseek-v4-pro", 1_787_193_000_000).expect("peak priced");
        assert_eq!(peak.input, 1.32);
        assert_eq!(peak.cache_read, 0.044);
        assert_eq!(peak.output, 3.96);
        let off_peak = price_for("deepseek-v4-pro", 1_787_229_000_000).expect("off-peak priced");
        assert_eq!(off_peak.input, 0.66);
        assert_eq!(off_peak.cache_read, 0.022);
        assert_eq!(off_peak.output, 1.98);

        // 边界左闭右开：09:30 还在峰段，10:30 已经出峰段。
        assert_eq!(
            price_for("deepseek-v4-pro", 1_787_218_200_000)
                .unwrap()
                .input,
            1.32,
        );
        assert_eq!(
            price_for("deepseek-v4-pro", 1_787_221_800_000)
                .unwrap()
                .input,
            0.66,
        );

        // 分时只对 DeepSeek 生效，别的模型全天一价。
        let glm_peak = price_for("glm-5.3", 1_787_193_000_000).expect("priced");
        let glm_off = price_for("glm-5.3", 1_787_229_000_000).expect("priced");
        assert_eq!(glm_peak.input, glm_off.input);
    }

    #[test]
    fn deepseek_weekend_is_off_peak_on_the_beijing_calendar() {
        // 2026-08-23 07:00 UTC = 北京周日 15:00：钟点落在 06:00–10:00 UTC 这个
        // 峰段窗口里，但周末全天谷段，所以要出 5 折价。只看钟点的实现在这里
        // 会给出 1.32，正好高一倍。
        let sunday = price_for("deepseek-v4-pro", 1_787_468_400_000).expect("priced");
        assert_eq!(sunday.input, 0.66);
        assert_eq!(sunday.output, 1.98);

        // 2026-08-24 02:00 UTC = 北京周一 10:00：周末规则不能漏到周一头上，
        // 这一刻在 01:00–04:00 窗口里，仍是峰段价。
        assert_eq!(
            price_for("deepseek-v4-pro", 1_787_536_800_000)
                .unwrap()
                .input,
            1.32,
        );

        // 生效时刻之前不改写历史：2026-08-22 01:30 UTC = 北京周六 09:30，
        // 也在窗口里，但那时这条规则还没生效，按当时的口径仍是峰段价。
        assert_eq!(
            price_for("deepseek-v4-pro", 1_787_362_200_000)
                .unwrap()
                .input,
            1.32,
        );
    }

    #[test]
    fn beijing_weekend_boundaries_are_counted_on_the_shifted_clock() {
        // 两种读法唯一分歧的那 16 小时，一头一个：
        // 2026-08-28 16:00 UTC 是 UTC 的周五，北京已是周六 00:00 → 周末。
        assert!(in_beijing_weekend(1_787_932_800_000));
        // 早一分钟还是北京的周五。
        assert!(!in_beijing_weekend(1_787_932_740_000));
        // 2026-08-30 16:00 UTC 是 UTC 的周日，北京已是周一 00:00 → 不是周末。
        assert!(!in_beijing_weekend(1_788_105_600_000));
        // 早一分钟还是北京的周日。
        assert!(in_beijing_weekend(1_788_105_540_000));
    }

    #[test]
    fn qwen_and_codex_auto_review_are_priced_from_official_sources() {
        // 阿里云百炼 qwen3.8-max：输入 $2、输出 $6 是官方公布价；缓存两项按
        // 官方计费规则推算（命中 10%、显式缓存写入 125%）。
        let qwen = price_for("qwen3.8-max", ANY_TIME_MS).expect("qwen3.8-max priced");
        assert_eq!(qwen.input, 2.0);
        assert_eq!(qwen.cache_read, 0.2);
        assert_eq!(qwen.cache_write, 2.5);
        assert_eq!(qwen.output, 6.0);

        // codex-auto-review 按 OpenAI 官方声明的 GPT-5.4 计价，不是自成一档。
        let review = price_for("codex-auto-review", ANY_TIME_MS).expect("alias priced");
        let gpt = price_for("gpt-5.4", ANY_TIME_MS).expect("gpt-5.4 priced");
        assert_eq!(review.input, gpt.input);
        assert_eq!(review.cache_read, gpt.cache_read);
        assert_eq!(review.output, gpt.output);
    }

    #[test]
    fn unknown_model_is_unpriced() {
        assert!(price_for("unknown", ANY_TIME_MS).is_none());
        assert!(price_for("", ANY_TIME_MS).is_none());
        // 未知模型带日期后缀也不能靠剥后缀蒙混过关。
        assert!(price_for("totally-made-up-20260101", ANY_TIME_MS).is_none());
    }
}
