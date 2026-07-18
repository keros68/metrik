// 从 LiteLLM 的公开价格表生成 src-tauri/src/pricing_table.rs。
//
// 为什么不在运行时拉取：Metrik 是本地优先的，配额查询之外不该再有网络依赖。
// 价格月级变动而我们发版频繁，构建期生成足够新鲜，且价格留在 git 里可审计。
//
// provider 选择：只取官方第一方 API（openai / anthropic / moonshot / zai / gemini）。
// 适配器解析出的模型名只有在这些官方价目里有完全同名条目时才计价；
// 订阅制 coding plan 的专属模型 ID（kimi-for-coding、k3、coding-plan 的 GLM 等）
// 没有官方按 token 价目，宁可 unpriced 也不借 Bedrock/Azure/Cloudflare 等
// 第三方转售价冒充官方价。
//
// 用法：npm run pricing:update   （改完提交生成的 .rs 文件）
// 也可离线：node scripts/update-pricing.mjs <本地 json 路径>

import { readFileSync, writeFileSync } from "node:fs";

const SOURCE =
  "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const OUT = "src-tauri/src/pricing_table.rs";
const PROVIDERS = new Set(["openai", "anthropic", "moonshot", "zai", "gemini"]);

const localPath = process.argv[2];
let raw;
if (localPath) {
  raw = JSON.parse(readFileSync(localPath, "utf8"));
} else {
  let response;
  try {
    response = await fetch(SOURCE);
  } catch (error) {
    // Node 的 fetch 默认不认 HTTPS_PROXY，走代理的机器会连 DNS 都过不去。
    // npm run pricing:update 已带 --use-env-proxy；直接调 node 时容易漏。
    throw new Error(
      `拉取 LiteLLM 价格表失败（${error.cause?.code ?? error.message}）。` +
        `走代理请用 npm run pricing:update（带 --use-env-proxy），` +
        `或先自行下载再传路径：node scripts/update-pricing.mjs <json 路径>`,
    );
  }
  if (!response.ok) {
    throw new Error(`拉取 LiteLLM 价格表失败: HTTP ${response.status}`);
  }
  raw = await response.json();
}

/// 每百万 token 单价。LiteLLM 存的是每 token 价，乘 1e6 会带出浮点尾数，
/// 四舍五入到 6 位（$0.000001/M 的精度，远超实际需要）。
const perMillion = (value) =>
  value == null ? 0 : Math.round(value * 1e6 * 1e6) / 1e6;

const rows = Object.entries(raw)
  .filter(([, entry]) => {
    if (typeof entry !== "object" || entry === null) return false;
    if (!PROVIDERS.has(entry.litellm_provider)) return false;
    // 没有完整的输入/输出价就不要——半个价格算出来的成本是错的。
    if (entry.input_cost_per_token == null) return false;
    if (entry.output_cost_per_token == null) return false;
    return entry.mode == null || entry.mode === "chat" || entry.mode === "responses";
  })
  .map(([key, entry]) => ({
    // 适配器解析出的是裸模型名（kimi-k2.5、glm-4.6、gemini-3-flash-preview），
    // LiteLLM 的键带 provider 前缀（moonshot/kimi-k2.5），入库前剥掉。
    model: key.startsWith(`${entry.litellm_provider}/`)
      ? key.slice(entry.litellm_provider.length + 1)
      : key,
    input: perMillion(entry.input_cost_per_token),
    // 缓存读价缺失时按未打折的输入价算，宁可高估也不虚报便宜。
    cache_read:
      entry.cache_read_input_token_cost == null
        ? perMillion(entry.input_cost_per_token)
        : perMillion(entry.cache_read_input_token_cost),
    // 缓存写入缺失 = 不额外计费（OpenAI 即如此），记 0。
    cache_write: perMillion(entry.cache_creation_input_token_cost),
    output: perMillion(entry.output_cost_per_token),
  }))
  // 剥前缀后可能撞名（provider/x 与裸 x 同名）：先到先得，绝不两行同名，
  // 否则 price_for 的二分查找行为未定义。
  .filter((row, index, all) => all.findIndex((other) => other.model === row.model) === index)
  // price_for 用二分查找，表必须按模型名有序。
  .sort((a, b) => (a.model < b.model ? -1 : a.model > b.model ? 1 : 0));

if (!rows.length) {
  throw new Error("LiteLLM 价格表里没有匹配到任何第一方 provider 模型，拒绝生成空表");
}

/// Rust 的 f64 字段不接受整数字面量（`input: 5` 编译不过），整数补上 `.0`；
/// 指数写法（1e-7）Rust 认，但统一成定点更好读。
const f64Literal = (value) => {
  const fixed = value.toFixed(6).replace(/0+$/, "").replace(/\.$/, ".0");
  return fixed.includes(".") ? fixed : `${fixed}.0`;
};

const asOf = new Date().toISOString().slice(0, 10);
const body = rows
  .map(
    (row) =>
      `    ("${row.model}", Pricing { input: ${f64Literal(row.input)}, cache_read: ${f64Literal(row.cache_read)}, cache_write: ${f64Literal(row.cache_write)}, output: ${f64Literal(row.output)} }),`,
  )
  .join("\n");

writeFileSync(
  OUT,
  `//! 由 scripts/update-pricing.mjs 生成，请勿手改。
//! 来源：LiteLLM 的 model_prices_and_context_window.json
//! （只取 openai / anthropic / moonshot / zai / gemini 官方第一方 API）。
//! 重新生成：node scripts/update-pricing.mjs
//!
//! 单位：美元 / 百万 token。表按模型名有序，供 price_for 二分查找。

use super::Pricing;

/// 价格表的生成日期，透传给前端做"估算截至"标注。
pub const PRICING_AS_OF: &str = "${asOf}";

// 每行一个模型：rustfmt 会把它拆成每条六行（近千行），生成结果与格式化结果
// 互相打架。这是生成文件，保持一行一条更好读也更好 diff。
#[rustfmt::skip]
pub const PRICING_TABLE: &[(&str, Pricing)] = &[
${body}
];
`,
  "utf8",
);

console.log(`已写入 ${OUT}：${rows.length} 个模型，日期 ${asOf}`);
