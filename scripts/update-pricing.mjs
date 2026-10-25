// 从 LiteLLM 与 models.dev 两份公开价格表生成 src-tauri/src/pricing_table.rs。
//
// 为什么不在运行时拉取：Metrik 是本地优先的，配额查询之外不该再有网络依赖。
// 价格月级变动而我们发版频繁，构建期生成足够新鲜，且价格留在 git 里可审计。
//
// 两个来源：LiteLLM 为主；models.dev 只补 LiteLLM 尚未收录的模型（新模型
// 通常先进 models.dev）。两边都收录但数值不同时沿用 LiteLLM，并把分歧写进
// 生成文件的注释——分歧变化会让文件出现 diff，每周刷新的 PR 里人工回官方页核对。
//
// provider 选择：只取官方第一方 API（openai / anthropic / moonshot / zai / gemini / xai / minimax）。
// 适配器解析出的模型名只有在这些官方价目里有完全同名条目时才计价；
// 订阅制 coding plan 的专属模型 ID（kimi-for-coding、coding-plan 的 GLM 等）
// 没有官方按 token 价目，宁可 unpriced 也不借 Bedrock/Azure/Cloudflare 等
// 第三方转售价冒充官方价。
// 注意：两份表都未收录的新模型官方价手动补在 pricing.rs 的 MANUAL_PRICING
// （如 deepseek-v4-pro），本脚本只生成 pricing_table.rs，不会覆盖它。
//
// 代际裁剪：解析视界固定 182 天（docs/ARCHITECTURE.md），更老的日志不会再
// 进台账，为它们保价只在每次刷新时制造分歧核对与 diff 噪音。表只收
// ACTIVE_FAMILIES 里「视界内还会出现在受支持 agent 日志里」的代际——
// 大致是 2025 年末的一代（gpt-5.2、claude-4-5 系、kimi-k2.5、gemini-2.5、
// MiniMax-M2.1+）及之后。新代际发布时在这里加一行；整代滑出视界、且
// 不再有受支持 agent 在用之后删一行。裁掉的老代际按 unpriced 报告，
// 重扫不会重算它们的历史成本。
//
// 用法：npm run pricing:update   （改完提交生成的 .rs 文件）
// .github/workflows/pricing-refresh.yml 每半个月替你跑一次，有变化就开 PR 等人核对。
// 也可离线：node scripts/update-pricing.mjs <LiteLLM json 路径> <models.dev json 路径>

import { readFileSync, writeFileSync } from "node:fs";

const LITELLM_SOURCE =
  "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const MODELS_DEV_SOURCE = "https://models.dev/api.json";
const OUT = "src-tauri/src/pricing_table.rs";
const LITELLM_PROVIDERS = new Set([
  "openai",
  "anthropic",
  "moonshot",
  "zai",
  "gemini",
  "xai",
  "minimax",
]);
// models.dev 的 provider ID 与 LiteLLM 不同名，覆盖的是同一组官方 API：
// google = Gemini API，moonshotai = api.moonshot.ai，zai = api.z.ai，minimax = api.minimax.io。
// zai-coding-plan、minimax-coding-plan 这类订阅 provider 的价目是 0 占位，不取。
const MODELS_DEV_PROVIDERS = ["openai", "anthropic", "moonshotai", "zai", "google", "xai", "minimax"];
const asOf = new Date().toISOString().slice(0, 10);

// 官方已公布、但尚未开始计费的价格只能在生效日后进入估算表。
const PRICING_START_DATES = new Map();

// 滑出 182 天解析视界的整代模型：表里不收（见 ACTIVE_FAMILIES）。
let prunedGenerations = 0;
const activeOnly = (model) => {
  if (inActiveFamily(model)) return true;
  prunedGenerations += 1;
  return false;
};

// 模型存在不等于价格已公开。LiteLLM 偶尔会先填入一个推测值；官方价目可核验前
// 保持 unpriced，不能把第三方数值当成第一方价格。
// gpt-5.5-cyber 曾在此列：2026-10-25 官方定价页已公布（$12.5/$1.25/$75，与
// LiteLLM 一致），解禁。新出现同类情况加回这里即可。
const UNVERIFIED_PRICING = new Set();

// 视界内还可能出现在受支持 agent 日志里的代际（见文件头部说明）。
// 按模型名（已剥 provider 前缀）匹配，两源同规则。
const ACTIVE_FAMILIES = [
  // OpenAI：gpt-5.2（2025-12）起整代保留。gpt-5 本代（2025-08）被 5.5
  // （2026-04-23）接替只早窗口起点 3 天，且一直是 Codex 的默认模型——
  // 窗口内的日志还在，保留；5.1 及更早整代裁掉。chat-latest 是 Codex 记的
  // 对话别名；daybreak 是 Cyber 线（2026）。
  /^gpt-5(-|$)/,
  /^gpt-5\.[2-9]/,
  /^gpt-6/,
  /^chat-latest$/,
  /^(gpt-)?daybreak-/,
  // Anthropic：2025 年秋的 4-5 一代（sonnet-4-5 / opus-4-5 / haiku-4-5）
  // 仍在服役，再往前整代裁掉。
  /^claude-(opus-4-[5-9]|opus-5|sonnet-4-[5-9]|sonnet-5|haiku-4-[5-9]|haiku-5|fable-|mythos)/,
  // Gemini：2.5 三兄弟仍在 Antigravity/OpenCode 的窗口内；3 起整代保留；
  // -latest 别名跟着当前代走。audio/image/robotics/deep-research 不是
  // coding agent 会记的模型，不收。
  /^gemini-(2\.5-(pro|flash|flash-lite)$|3|4|flash-latest$|pro-latest$|flash-lite-latest$)/,
  // GLM：4.6（2025-10）起；4.5 家族已整代滑出。
  /^glm-(4\.[6-9]|5)/,
  // Kimi：k2.5（2025-10）起；moonshot-v1-* 是上古 openai 兼容端点。
  /^kimi-k[23]/,
  // xAI：表里只有 4.20（2026-03）以后的代际，整族保留。
  /^grok-/,
  // MiniMax：M2.1（2025-11）起；M2 本体已被 M2.1 接管且滑出视界。
  /^MiniMax-M2\.|^MiniMax-M[3-9]/,
];
const inActiveFamily = (model) => ACTIVE_FAMILIES.some((family) => family.test(model));

async function load(name, url, localPath) {
  if (localPath) return JSON.parse(readFileSync(localPath, "utf8"));
  let response;
  try {
    response = await fetch(url);
  } catch (error) {
    // Node 的 fetch 默认不认 HTTPS_PROXY，走代理的机器会连 DNS 都过不去。
    // npm run pricing:update 已带 --use-env-proxy；直接调 node 时容易漏。
    throw new Error(
      `拉取 ${name} 价格表失败（${error.cause?.code ?? error.message}）。` +
        `走代理请用 npm run pricing:update（带 --use-env-proxy），` +
        `或先自行下载再传路径：node scripts/update-pricing.mjs <LiteLLM json> <models.dev json>`,
    );
  }
  if (!response.ok) {
    throw new Error(`拉取 ${name} 价格表失败: HTTP ${response.status}`);
  }
  return response.json();
}

const [litellm, modelsDev] = await Promise.all([
  load("LiteLLM", LITELLM_SOURCE, process.argv[2]),
  load("models.dev", MODELS_DEV_SOURCE, process.argv[3]),
]);

/// 四舍五入到 6 位小数（$0.000001/M 的精度，远超实际需要），去掉浮点尾数。
const round6 = (value) => Math.round(value * 1e6) / 1e6;

/// LiteLLM 存的是每 token 价，换算成每百万 token。
const perMillion = (value) => (value == null ? 0 : round6(value * 1e6));

const SAFE_MODEL_NAME = /^[A-Za-z0-9._:@\/-]+$/;
let rejectedNames = 0;

/// 两个来源共用的准入规则。
function admissible(model, input, output) {
  if (UNVERIFIED_PRICING.has(model)) return false;
  const pricingStarts = PRICING_START_DATES.get(model);
  if (pricingStarts != null && asOf < pricingStarts) return false;
  // 没有完整且为正的 token 输入/输出价就不要——半个价格算出来的成本是错的；
  // Lyria 这类按图片或时长收费的模型会把 token 价填成 0，占位值不能当免费。
  if (input == null || output == null || input <= 0 || output <= 0) return false;
  // 模型名原样写进 Rust 字符串字面量；含引号、反斜杠等字符的上游键直接丢弃，
  // 不让外部数据改写生成的源码。
  if (!SAFE_MODEL_NAME.test(model)) {
    rejectedNames += 1;
    return false;
  }
  return true;
}

const litellmRows = Object.entries(litellm)
  .filter(([, entry]) => typeof entry === "object" && entry !== null)
  .filter(([, entry]) => LITELLM_PROVIDERS.has(entry.litellm_provider))
  .filter(([, entry]) => entry.mode == null || entry.mode === "chat" || entry.mode === "responses")
  .map(([key, entry]) => ({
    // 适配器解析出的是裸模型名（kimi-k2.5、glm-4.6、gemini-3-flash-preview），
    // LiteLLM 的键带 provider 前缀（moonshot/kimi-k2.5），入库前剥掉。
    model: key.startsWith(`${entry.litellm_provider}/`)
      ? key.slice(entry.litellm_provider.length + 1)
      : key,
    entry,
  }))
  .filter(({ model, entry }) =>
    admissible(model, entry.input_cost_per_token, entry.output_cost_per_token),
  )
  .filter(({ model }) => activeOnly(model))
  .map(({ model, entry }) => ({
    model,
    input: perMillion(entry.input_cost_per_token),
    // 缓存读价缺失时按未打折的输入价算，宁可高估也不虚报便宜。
    cache_read:
      entry.cache_read_input_token_cost == null
        ? perMillion(entry.input_cost_per_token)
        : perMillion(entry.cache_read_input_token_cost),
    // 缓存写入缺失 = 不额外计费（OpenAI 即如此），记 0。
    cache_write: perMillion(entry.cache_creation_input_token_cost),
    output: perMillion(entry.output_cost_per_token),
    source: "litellm",
  }));

// models.dev 的 cost 已是每百万 token；只取 cost 顶层的基础档，
// context_over_200k / tiers 这类长上下文加价档与 LiteLLM 口径一致地忽略。
const modelsDevRows = MODELS_DEV_PROVIDERS.flatMap((provider) =>
  Object.entries(modelsDev[provider]?.models ?? {}).map(([model, entry]) => ({ model, entry })),
)
  // 只收输出纯文本的模型：图像、语音、实时音频模型按其他单位计费，
  // 其 token 价不能代表实际花费（对应 LiteLLM 侧的 mode 过滤）。
  .filter(({ entry }) => {
    const output = entry.modalities?.output;
    return Array.isArray(output) && output.length === 1 && output[0] === "text";
  })
  .filter(({ model, entry }) => admissible(model, entry.cost?.input, entry.cost?.output))
  .filter(({ model }) => activeOnly(model))
  .map(({ model, entry: { cost } }) => ({
    model,
    input: round6(cost.input),
    cache_read: round6(cost.cache_read ?? cost.input),
    cache_write: round6(cost.cache_write ?? 0),
    output: round6(cost.output),
    source: "models.dev",
  }));

// 剥前缀后可能撞名（provider/x 与裸 x 同名），models.dev 也可能与 LiteLLM 重名：
// 先到先得，LiteLLM 在前，绝不两行同名，否则 price_for 的二分查找行为未定义。
const rows = [];
const byModel = new Map();
const conflicts = new Map();
const FIELDS = ["input", "cache_read", "cache_write", "output"];
for (const row of [...litellmRows, ...modelsDevRows]) {
  const existing = byModel.get(row.model);
  if (!existing) {
    byModel.set(row.model, row);
    rows.push(row);
    continue;
  }
  if (existing.source === row.source) continue;
  const differing = FIELDS.filter((field) => existing[field] !== row[field]);
  if (differing.length) conflicts.set(row.model, { litellm: existing, modelsDev: row, differing });
}
// price_for 用二分查找，表必须按模型名有序。
const byName = (a, b) => (a.model < b.model ? -1 : a.model > b.model ? 1 : 0);
rows.sort(byName);

if (rejectedNames) {
  console.log(`跳过 ${rejectedNames} 个含非法字符的模型名`);
}
if (prunedGenerations) {
  console.log(`视界外整代裁剪 ${prunedGenerations} 个模型`);
}

if (!litellmRows.length || !modelsDevRows.length) {
  // 任一来源整体失效（格式改版、provider 改名）都该停下查原因，
  // 不能让生成表悄悄退化成单源。
  throw new Error(
    `价格来源没有匹配到任何第一方 provider 模型（LiteLLM ${litellmRows.length}，` +
      `models.dev ${modelsDevRows.length}），拒绝生成`,
  );
}

/// Rust 的 f64 字段不接受整数字面量（`input: 5` 编译不过），整数补上 `.0`；
/// 指数写法（1e-7）Rust 认，但统一成定点更好读。
const f64Literal = (value) => {
  const fixed = value.toFixed(6).replace(/0+$/, "").replace(/\.$/, ".0");
  return fixed.includes(".") ? fixed : `${fixed}.0`;
};

const body = rows
  .map(
    (row) =>
      `    ("${row.model}", Pricing { input: ${f64Literal(row.input)}, cache_read: ${f64Literal(row.cache_read)}, cache_write: ${f64Literal(row.cache_write)}, output: ${f64Literal(row.output)} }),` +
      (row.source === "models.dev" ? " // models.dev" : ""),
  )
  .join("\n");

const prices = (row) => FIELDS.map((field) => f64Literal(row[field])).join(" / ");
const conflictNotes = [...conflicts.keys()]
  .sort()
  .map((model) => {
    const { litellm: kept, modelsDev: other, differing } = conflicts.get(model);
    return `//! - ${model}（${differing.join("、")}）：LiteLLM ${prices(kept)}；models.dev ${prices(other)}`;
  })
  .join("\n");
const conflictSection = conflictNotes
  ? `//!
//! 两源分歧（表内采用 LiteLLM 值；字段顺序 input / cache_read / cache_write / output）。
//! 涉及实际计价的条目应回官方定价页核对，确认后在 MANUAL_PRICING 覆盖或无需处理：
${conflictNotes}
`
  : "";

writeFileSync(
  OUT,
  `//! 由 scripts/update-pricing.mjs 生成，请勿手改。
//! 来源：LiteLLM 的 model_prices_and_context_window.json 为主，models.dev 的
//! api.json 补 LiteLLM 未收录的模型（行尾标 \`// models.dev\`）。只取 openai /
//! anthropic / moonshot / zai / gemini / xai / minimax 官方第一方 API。
//! 重新生成：npm run pricing:update
//!
//! 单位：美元 / 百万 token。表按模型名有序，供 price_for 二分查找。
${conflictSection}
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
