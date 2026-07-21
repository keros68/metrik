use super::{discover_jsonl, AgentAdapter, ParsedScan, ScanDiagnostics, SourceCandidate};
use crate::domain::{ParsedSource, TokenVector, UsageEvent};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

/// 腾讯 CodeBuddy Code / WorkBuddy 的会话转录（JSONL），两个产品同一格式：
/// `~/.codebuddy/projects/<key>/*.jsonl` 与 `~/.workbuddy/projects/**/*.jsonl`。
/// 每行 `{type,role,status,sessionId,timestamp(毫秒),message{model,usage},providerData{...}}`；
/// 只计 `status == "completed"` 的 assistant message 与 function_call。
///
/// 口径陷阱（据 tokscale 解析器与其真实夹具核实）：`inputTokens`/`input_tokens`
/// 是**含缓存**的 prompt 总量，真正未缓存输入是 `cachedMissTokens`——存在时必须
/// 优先取它，否则缓存读会被重复计入 processed。usage 字段蛇形/驼峰别名并存。
///
/// 身份 = `providerData.messageId` → `providerData.traceId` → 顶层 `id`（后者是
/// 客户端本地序号，须叠加 sessionId 限定作用域）。同一身份渐进更新，分量取最大值
/// ——与 Claude adapter 同构。老版本 WorkBuddy 的 `workbuddy.db`（SQLite，仅单一
/// used 总数）不解析，见 coverage_gaps。
pub struct WorkbuddyAdapter {
    roots: Vec<PathBuf>,
    legacy_db: Option<PathBuf>,
}

#[derive(Clone)]
struct MessageUsage {
    timestamp: i64,
    session_id: String,
    event_key: String,
    model: Option<String>,
    tokens: TokenVector,
}

#[derive(Deserialize, Default)]
struct BuddyRecord {
    id: Option<String>,
    /// epoch 毫秒。
    timestamp: Option<i64>,
    #[serde(rename = "type")]
    record_type: Option<String>,
    role: Option<String>,
    status: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    message: Option<BuddyMessage>,
    #[serde(rename = "providerData")]
    provider_data: Option<BuddyProviderData>,
}

#[derive(Deserialize, Default)]
struct BuddyMessage {
    model: Option<String>,
    usage: Option<BuddyUsage>,
}

#[derive(Deserialize, Default)]
struct BuddyProviderData {
    model: Option<String>,
    #[serde(rename = "requestModelId")]
    request_model_id: Option<String>,
    #[serde(rename = "messageId")]
    message_id: Option<String>,
    #[serde(rename = "traceId")]
    trace_id: Option<String>,
    usage: Option<BuddyUsage>,
    #[serde(rename = "rawUsage")]
    raw_usage: Option<BuddyUsage>,
}

/// 每类分量的别名互斥地取第一个出现者；蛇形与驼峰并存于不同版本的日志。
#[derive(Deserialize, Default)]
struct BuddyUsage {
    #[serde(rename = "cachedMissTokens")]
    cached_miss_tokens: Option<i64>,
    #[serde(rename = "cacheMissTokens")]
    cache_miss_tokens: Option<i64>,
    input_tokens: Option<i64>,
    #[serde(rename = "inputTokens")]
    input_tokens_camel: Option<i64>,
    prompt_tokens: Option<i64>,
    output_tokens: Option<i64>,
    #[serde(rename = "outputTokens")]
    output_tokens_camel: Option<i64>,
    completion_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    #[serde(rename = "cacheReadInputTokens")]
    cache_read_camel: Option<i64>,
    #[serde(rename = "cacheTokens")]
    cache_tokens: Option<i64>,
    prompt_cache_hit_tokens: Option<i64>,
    cached_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
    #[serde(rename = "cacheCreationInputTokens")]
    cache_creation_camel: Option<i64>,
    #[serde(rename = "cachedWriteTokens")]
    cached_write_tokens: Option<i64>,
    prompt_cache_write_tokens: Option<i64>,
    completion_thinking_tokens: Option<i64>,
    #[serde(rename = "completionThinkingTokens")]
    completion_thinking_camel: Option<i64>,
    #[serde(rename = "reasoningTokens")]
    reasoning_tokens: Option<i64>,
}

fn first_positive(values: &[Option<i64>]) -> i64 {
    values.iter().find_map(|value| *value).unwrap_or(0).max(0)
}

impl BuddyUsage {
    fn to_vector(&self) -> TokenVector {
        let cache_read = first_positive(&[
            self.cache_read_input_tokens,
            self.cache_read_camel,
            self.cache_tokens,
            self.prompt_cache_hit_tokens,
            self.cached_tokens,
        ]);
        // 未缓存输入：
        // - `cachedMissTokens` 本就是未缓存量，存在时直接用；
        // - 其余别名（蛇形 input_tokens、驼峰 inputTokens、prompt_tokens）**都是
        //   含缓存的 prompt 总量**，要扣掉 cache_read 才是未缓存部分。
        //
        // 蛇形这条与 Anthropic 的同名字段语义相反，是真机实测定的：CodeBuddy CLI
        // 转录里 `total_tokens == input_tokens + output_tokens` 恒成立（16/16 行），
        // 而 cache_read_input_tokens 不进这个等式——它是 input_tokens 的子集。
        // 早先按 Anthropic 风格当作"不含缓存"会把缓存读重复计一遍。
        let input_uncached = if let Some(miss) = self.cached_miss_tokens.or(self.cache_miss_tokens)
        {
            miss.max(0)
        } else {
            (first_positive(&[
                self.input_tokens,
                self.input_tokens_camel,
                self.prompt_tokens,
            ]) - cache_read)
                .max(0)
        };
        let output = first_positive(&[
            self.output_tokens,
            self.output_tokens_camel,
            self.completion_tokens,
        ]);
        TokenVector {
            input_uncached,
            cache_read,
            cache_write: first_positive(&[
                self.cache_creation_input_tokens,
                self.cache_creation_camel,
                self.cached_write_tokens,
                self.prompt_cache_write_tokens,
            ]),
            output,
            // reasoning 是 output 子项，超出 output 的值不可信，夹回去。
            reasoning_output: first_positive(&[
                self.completion_thinking_tokens,
                self.completion_thinking_camel,
                self.reasoning_tokens,
            ])
            .min(output),
        }
    }
}

impl WorkbuddyAdapter {
    pub fn detected() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        let legacy_db = home.join(".workbuddy").join("workbuddy.db");
        Self {
            roots: vec![
                home.join(".codebuddy").join("projects"),
                home.join(".workbuddy").join("projects"),
            ],
            legacy_db: legacy_db.exists().then_some(legacy_db),
        }
    }

    #[cfg(test)]
    fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            legacy_db: None,
        }
    }
}

impl AgentAdapter for WorkbuddyAdapter {
    fn id(&self) -> &'static str {
        "workbuddy"
    }

    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate> {
        discover_jsonl(&self.roots, self.id(), cutoff_ms)
    }

    fn coverage_gaps(&self) -> Vec<String> {
        // 老版本 WorkBuddy 把用量写进 SQLite（单一 used 总数，无法拆分量）；
        // 存在即说明有本版本读不到的历史。
        self.legacy_db
            .as_ref()
            .map(|path| format!("旧版数据库暂不支持读取：{}", path.display()))
            .into_iter()
            .collect()
    }

    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64) -> Result<ParsedScan> {
        let file = File::open(&candidate.path)
            .with_context(|| format!("failed to open {}", candidate.path.display()))?;
        let reader = BufReader::with_capacity(256 * 1024, file);
        let fallback_session = candidate
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("unknown-session")
            .to_owned();
        let mut messages: HashMap<String, MessageUsage> = HashMap::new();
        let mut diagnostics = ScanDiagnostics::default();
        let track_skipped_lines = candidate.mtime_ns / 1_000_000 >= cutoff_ms;

        for (line_index, line) in reader.lines().enumerate() {
            let line = match line {
                Ok(line) => line,
                Err(_) => {
                    if track_skipped_lines {
                        diagnostics.unreadable_lines += 1;
                    }
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<BuddyRecord>(&line) else {
                if track_skipped_lines {
                    diagnostics.malformed_lines += 1;
                }
                continue;
            };
            // assistant 回复与工具调用都带 usage，都要计。
            //
            // status 只对 `message` 有约束：它在生成中会先落一行、完成后重写，
            // 未完成的不计。`function_call` 的 status 恒为 null（真机实测：16 条
            // 带 usage 的行里 13 条是 function_call、status 全空），对它要求
            // "completed" 会漏掉 81% 的用量。
            let countable = match record.record_type.as_deref() {
                Some("message") => {
                    record.role.as_deref() == Some("assistant")
                        && record.status.as_deref() == Some("completed")
                }
                Some("function_call") => true,
                _ => false,
            };
            if !countable {
                continue;
            }
            let Some(timestamp) = record.timestamp.filter(|value| *value > 0) else {
                continue;
            };
            if timestamp < cutoff_ms {
                continue;
            }
            let message = record.message.unwrap_or_default();
            let provider = record.provider_data.unwrap_or_default();
            let Some(usage) = message.usage.or(provider.usage).or(provider.raw_usage) else {
                continue;
            };
            let tokens = usage.to_vector();
            let session_id = record
                .session_id
                .unwrap_or_else(|| fallback_session.clone());
            let model = provider
                .model
                .or(provider.request_model_id)
                .or(message.model);
            // messageId/traceId 是 provider 生成的全局身份；顶层 id 是客户端本地
            // 序号（如 "assistant-1"），跨会话会撞，必须叠加会话限定作用域。
            let key = if let Some(message_id) = provider.message_id {
                format!("message:{message_id}")
            } else if let Some(trace_id) = provider.trace_id {
                format!("trace:{trace_id}")
            } else if let Some(local_id) = record.id {
                format!("local:{session_id}:{local_id}")
            } else {
                format!("fallback:{session_id}:{timestamp}:{line_index}")
            };

            if let Some(stored) = messages.get_mut(&key) {
                stored.tokens.component_max(&tokens);
                stored.model = stored.model.clone().or(model);
                if timestamp >= stored.timestamp {
                    stored.timestamp = timestamp;
                }
            } else {
                messages.insert(
                    key.clone(),
                    MessageUsage {
                        timestamp,
                        session_id,
                        event_key: key,
                        model,
                        tokens,
                    },
                );
            }
        }

        let mut events: Vec<UsageEvent> = messages
            .into_values()
            .filter(|message| message.tokens.processed() > 0)
            .map(|message| {
                UsageEvent::new(
                    self.id(),
                    message.event_key,
                    message.timestamp,
                    message.session_id,
                    message.model,
                    message.tokens,
                    "exact",
                )
            })
            .collect();
        events.sort_by_key(|event| event.occurred_at_ms);

        let logical_key = events
            .first()
            .map(|event| event.session_id.clone())
            .unwrap_or(fallback_session);
        Ok(ParsedScan {
            source: ParsedSource {
                source_id: candidate.source_id.clone(),
                adapter_id: self.id(),
                locator: candidate.path.clone(),
                logical_key,
                size: candidate.size,
                mtime_ns: candidate.mtime_ns,
                events,
                quotas: vec![],
            },
            diagnostics,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn parse_lines(name: &str, lines: &[String]) -> ParsedScan {
        parse_lines_with_cutoff(name, lines, i64::MIN)
    }

    fn parse_lines_with_cutoff(name: &str, lines: &[String], cutoff_ms: i64) -> ParsedScan {
        let temp = std::env::temp_dir().join(format!(
            "metrik-workbuddy-{name}-{}.jsonl",
            std::process::id()
        ));
        let mut file = File::create(&temp).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        drop(file);
        let metadata = temp.metadata().unwrap();
        let candidate = SourceCandidate {
            source_id: "source".into(),
            path: temp.clone(),
            size: metadata.len(),
            mtime_ns: i64::MAX,
        };
        let parsed = WorkbuddyAdapter::with_roots(vec![])
            .parse(&candidate, cutoff_ms)
            .unwrap();
        std::fs::remove_file(temp).ok();
        parsed
    }

    /// tokscale 仓库里的真实夹具行（蛇形 usage、providerData 带模型）。
    #[test]
    fn real_fixture_line_parses_with_snake_case_usage() {
        let parsed = parse_lines(
            "fixture",
            &[r#"{"id":"assistant-1","timestamp":1780000000100,"type":"message","role":"assistant","status":"completed","sessionId":"session-1","cwd":"/Users/alice/repo","providerData":{"model":"glm-5.2","messageId":"msg-1"},"message":{"usage":{"input_tokens":24486,"output_tokens":3,"cache_read_input_tokens":14720}}}"#.to_string()],
        );
        assert_eq!(parsed.source.events.len(), 1);
        let event = &parsed.source.events[0];
        assert_eq!(event.event_key, "message:msg-1");
        assert_eq!(event.model.as_deref(), Some("glm-5.2"));
        // 蛇形 input_tokens 同样含缓存：未缓存部分 = input - cache_read。
        assert_eq!(event.tokens.input_uncached, 24486 - 14720);
        assert_eq!(event.tokens.cache_read, 14720);
        assert_eq!(event.tokens.output, 3);
        // processed 与来源的 total_tokens 口径一致（input 已含 cache_read）。
        assert_eq!(event.tokens.processed(), 24486 + 3);
    }

    /// 真机转录行（本机 WorkBuddy 桌面版实测，2026-07-21）：
    /// `total_tokens == input_tokens + output_tokens` 恒成立，cache_read 是
    /// input_tokens 的子集；function_call 带 usage 但 status 为空。
    #[test]
    fn real_desktop_transcript_rows_are_counted_with_total_matching() {
        let lines = vec![
            r#"{"id":"a-1","timestamp":1784603600000,"type":"message","role":"assistant","status":"completed","sessionId":"d180683e","providerData":{"messageId":"65a92f2e6275"},"message":{"usage":{"input_tokens":32514,"output_tokens":855,"total_tokens":33369,"cache_read_input_tokens":19008}}}"#.to_string(),
            // function_call 的 status 是 null——必须照计，否则漏掉大部分用量。
            r#"{"id":"f-1","timestamp":1784603601000,"type":"function_call","sessionId":"d180683e","providerData":{"messageId":"4a0fe9346f49"},"message":{"usage":{"input_tokens":33785,"output_tokens":372,"total_tokens":34157,"cache_read_input_tokens":32512}}}"#.to_string(),
        ];
        let parsed = parse_lines("real-desktop", &lines);
        assert_eq!(parsed.source.events.len(), 2, "function_call 也要计入");
        for (event, (input, output, cache)) in parsed
            .source
            .events
            .iter()
            .zip([(32514, 855, 19008), (33785, 372, 32512)])
        {
            assert_eq!(event.tokens.cache_read, cache);
            assert_eq!(event.tokens.input_uncached, input - cache);
            assert_eq!(event.tokens.output, output);
            // 与来源自报的 total_tokens 对齐，不重复计缓存读。
            assert_eq!(event.tokens.processed(), input + output);
        }
    }

    /// 驼峰 inputTokens 含缓存但缺 cachedMissTokens 时：扣缓存读，不重复计入。
    #[test]
    fn camel_input_tokens_without_miss_subtracts_cache_read() {
        let parsed = parse_lines(
            "camel-inclusive",
            &[r#"{"id":"a-1","timestamp":1780000000100,"type":"message","role":"assistant","status":"completed","sessionId":"s-1","providerData":{"messageId":"m-1"},"message":{"usage":{"inputTokens":140732,"outputTokens":635,"cacheTokens":76032}}}"#.to_string()],
        );
        let event = &parsed.source.events[0];
        assert_eq!(event.tokens.input_uncached, 140732 - 76032);
        assert_eq!(event.tokens.cache_read, 76032);
        assert_eq!(event.tokens.processed(), 140732 + 635);
    }

    /// 口径陷阱：inputTokens 含缓存，cachedMissTokens 才是未缓存输入。
    #[test]
    fn cached_miss_tokens_wins_over_inclusive_input_tokens() {
        let parsed = parse_lines(
            "miss",
            &[r#"{"id":"a-1","timestamp":1780000000100,"type":"message","role":"assistant","status":"completed","sessionId":"s-1","message":{"usage":{"inputTokens":140732,"outputTokens":635,"cacheTokens":76032,"cachedWriteTokens":0,"cachedMissTokens":64700}}}"#.to_string()],
        );
        let event = &parsed.source.events[0];
        assert_eq!(event.tokens.input_uncached, 64700);
        assert_eq!(event.tokens.cache_read, 76032);
        assert_eq!(event.tokens.output, 635);
        // processed = 未缓存 + 缓存读 + 输出，不含 inputTokens 里的重复缓存量。
        assert_eq!(event.tokens.processed(), 64700 + 76032 + 635);
    }

    /// 同一 messageId 渐进更新：分量取最大值，只出一个事件（跨会话同 ID 也合并）。
    #[test]
    fn progressive_updates_keep_component_maximum() {
        let mut lines = Vec::new();
        for (session, output) in [("s-1", 10), ("s-1", 40), ("s-2", 25)] {
            lines.push(format!(
                r#"{{"id":"a-1","timestamp":1780000000100,"type":"message","role":"assistant","status":"completed","sessionId":"{session}","providerData":{{"messageId":"msg-a"}},"message":{{"usage":{{"input_tokens":100,"output_tokens":{output}}}}}}}"#
            ));
        }
        let parsed = parse_lines("progressive", &lines);
        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].tokens.output, 40);
        assert_eq!(parsed.source.events[0].event_key, "message:msg-a");
    }

    /// 顶层 id 是客户端本地序号：不同会话的同名 id 必须是两个事件。
    #[test]
    fn local_ids_are_scoped_by_session() {
        let mut lines = Vec::new();
        for session in ["s-1", "s-2"] {
            lines.push(format!(
                r#"{{"id":"assistant-1","timestamp":1780000000100,"type":"message","role":"assistant","status":"completed","sessionId":"{session}","message":{{"usage":{{"input_tokens":50,"output_tokens":5}}}}}}"#
            ));
        }
        let parsed = parse_lines("local-scope", &lines);
        assert_eq!(parsed.source.events.len(), 2);
    }

    /// 未完成状态与非 assistant 行不计；function_call 计入。
    #[test]
    fn only_completed_assistant_and_function_calls_count() {
        let lines = vec![
            r#"{"id":"u-1","timestamp":1780000000100,"type":"message","role":"user","status":"completed","sessionId":"s-1","message":{"usage":{"input_tokens":999}}}"#.to_string(),
            r#"{"id":"a-1","timestamp":1780000000200,"type":"message","role":"assistant","status":"in_progress","sessionId":"s-1","message":{"usage":{"input_tokens":888}}}"#.to_string(),
            r#"{"id":"f-1","timestamp":1780000000300,"type":"function_call","status":"completed","sessionId":"s-1","providerData":{"messageId":"msg-f"},"message":{"usage":{"input_tokens":30,"output_tokens":4}}}"#.to_string(),
        ];
        let parsed = parse_lines("filter", &lines);
        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].event_key, "message:msg-f");
        assert_eq!(parsed.source.events[0].tokens.processed(), 34);
    }

    /// 时间边界：cutoff 之前的事件不计入。
    #[test]
    fn events_before_cutoff_are_skipped() {
        let lines = vec![
            r#"{"id":"a-1","timestamp":1000,"type":"message","role":"assistant","status":"completed","sessionId":"s-1","providerData":{"messageId":"m-old"},"message":{"usage":{"input_tokens":10,"output_tokens":1}}}"#.to_string(),
            r#"{"id":"a-2","timestamp":2000,"type":"message","role":"assistant","status":"completed","sessionId":"s-1","providerData":{"messageId":"m-new"},"message":{"usage":{"input_tokens":20,"output_tokens":2}}}"#.to_string(),
        ];
        let parsed = parse_lines_with_cutoff("cutoff", &lines, 1500);
        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.source.events[0].event_key, "message:m-new");
    }

    /// 部分输入：坏行进诊断、好行保留，覆盖降级为 partial 而不是伪装精确。
    #[test]
    fn malformed_line_is_reported_while_valid_usage_is_retained() {
        let lines = vec![
            "{broken-json".to_string(),
            r#"{"id":"a-1","timestamp":1780000000100,"type":"message","role":"assistant","status":"completed","sessionId":"s-1","providerData":{"messageId":"m-1"},"message":{"usage":{"input_tokens":10,"output_tokens":1}}}"#.to_string(),
        ];
        let parsed = parse_lines("diagnostics", &lines);
        assert_eq!(parsed.source.events.len(), 1);
        assert_eq!(parsed.diagnostics.malformed_lines, 1);
        assert!(parsed.diagnostics.is_partial());
    }

    /// reasoning 是 output 子项：夹到 output 以内，不参与 processed 叠加。
    #[test]
    fn reasoning_tokens_are_clamped_to_output() {
        let parsed = parse_lines(
            "reasoning",
            &[r#"{"id":"a-1","timestamp":1780000000100,"type":"message","role":"assistant","status":"completed","sessionId":"s-1","providerData":{"messageId":"m-1"},"message":{"usage":{"input_tokens":10,"output_tokens":20,"reasoningTokens":50}}}"#.to_string()],
        );
        let event = &parsed.source.events[0];
        assert_eq!(event.tokens.reasoning_output, 20);
        assert_eq!(event.tokens.processed(), 30);
    }
}
