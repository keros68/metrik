/// 模型名的展示写法。只影响界面文字：入库、计价匹配、CSV 导出一律用日志里
/// 记的原始 ID（`claude-fable-5-1`），改了就匹配不上价格表，模型会变成未计价。

/// Anthropic 的版本号用连字符（`claude-fable-5-1`），同一张列表里其他厂商都用
/// 点号（`glm-5.3`、`gpt-5.6`、`kimi-k2.5`），并排看很跳。这里把 Claude 版本号
/// 的连字符换成点号统一写法。
///
/// 只认「连字符 + 数字 + 连字符 + 数字」这一段，且两侧要么到头要么接连字符：
/// - `claude-fable-5-1` → `claude-fable-5.1`
/// - `claude-opus-4-1-20250805` → `claude-opus-4.1-20250805`（日期快照原样保留，
///   它是模型身份的一部分，不能省）
/// - `claude-3-7-sonnet-20250219` → `claude-3.7-sonnet-20250219`（老式命名把版本
///   写在系列名前面，同样处理）
/// - `claude-opus-5`、`claude-3-opus-20240229`、`claude-mythos-preview` 不变
///
/// 限定 `claude-` 开头：`grok-4-1-fast` 那样的名字是 xAI 官方写法，不该被改。
/// 这是规则转换不是名字表，Anthropic 出新模型不用来这里加一行。
export function claudeVersionWithDots(model) {
  if (!model.startsWith("claude-")) return model;
  return model.replace(/-(\d+)-(\d+)(?=-|$)/g, "-$1.$2");
}

/// 界面上的模型名：本地确实缺模型名的记 "unknown"（未标注模型）；
/// "synced-remote" 是同步事件（导出本就不含模型名，见 sync 架构约束），
/// 不是某个叫这个名字的模型，必须说人话。
export function modelDisplayName(model) {
  if (model === "synced-remote") return "其他设备同步（无模型名）";
  if (model === "unknown") return "未标注模型";
  return claudeVersionWithDots(model);
}
