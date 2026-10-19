//! Minimal, metadata-only OTLP decoding for opt-in local response-speed samples.
//!
//! Codex's sampling span ends before draining tools. Its parent request span and
//! the receiving-stream sibling can close later, so joins must survive batches.
//! Never use a whole turn, a tool span, or a single SSE event as request timing.

use anyhow::{bail, Context, Result};
use prost::Message;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

const MAX_PENDING_SPANS: usize = 4096;
const MAX_EMITTED_IDS: usize = 4096;
const PENDING_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_MODEL_BYTES: usize = 200;

#[derive(Clone, Debug, PartialEq)]
pub struct SpeedSample {
    pub id: String,
    pub agent: String,
    pub model: String,
    pub occurred_at_ms: i64,
    pub output_tokens: i64,
    pub duration_ms: f64,
}

#[derive(Clone, Default)]
pub struct Decoder {
    spans: BTreeMap<SpanKey, CachedSpan>,
    span_order: VecDeque<SpanKey>,
    emitted: BTreeSet<String>,
    emitted_order: VecDeque<String>,
}

impl Decoder {
    pub fn decode(
        &mut self,
        agent: &str,
        signal: &str,
        content_type: &str,
        body: &[u8],
    ) -> Result<Vec<SpeedSample>> {
        let content_type = content_type.split(';').next().unwrap_or_default().trim();
        let json = content_type.eq_ignore_ascii_case("application/json");
        let protobuf = content_type.eq_ignore_ascii_case("application/x-protobuf");
        if !json && !protobuf {
            bail!("unsupported OTLP content type");
        }
        match (agent, signal) {
            ("claude", "logs") => {
                let samples = if json {
                    claude_json(body)?
                } else {
                    claude_protobuf(body)?
                };
                deduplicate_samples(samples)
            }
            ("codex", "traces") => {
                let spans = if json {
                    codex_json(body)?
                } else {
                    codex_protobuf(body)?
                };
                self.expire_spans();
                // Merge the entire batch before joining: a conflicting duplicate
                // in the same export must not escape as a successful sample.
                for span in spans {
                    self.cache_span(span);
                }
                Ok(self.codex_samples())
            }
            _ => bail!("unsupported timing signal"),
        }
    }

    fn expire_spans(&mut self) {
        self.expire_spans_at(Instant::now());
    }

    fn expire_spans_at(&mut self, now: Instant) {
        while let Some(key) = self.span_order.front() {
            let expired = self.spans.get(key).is_none_or(|entry| {
                now.saturating_duration_since(entry.inserted) >= PENDING_LIFETIME
            });
            if !expired {
                break;
            }
            self.spans.remove(key);
            self.span_order.pop_front();
        }
    }

    fn cache_span(&mut self, span: TimingSpan) {
        if let Some(entry) = self.spans.get_mut(&span.key) {
            if entry.value.as_ref() != Some(&span) {
                // Poison conflicting identities instead of choosing whichever
                // export arrived first. Only whitelisted fields are retained.
                entry.value = None;
            }
            return;
        }
        while self.spans.len() >= MAX_PENDING_SPANS {
            if let Some(oldest) = self.span_order.pop_front() {
                self.spans.remove(&oldest);
            }
        }
        self.span_order.push_back(span.key);
        self.spans.insert(
            span.key,
            CachedSpan {
                inserted: Instant::now(),
                value: Some(span),
            },
        );
    }

    fn codex_samples(&mut self) -> Vec<SpeedSample> {
        let mut candidates = Vec::new();
        for sampling in self.spans.values().filter_map(|entry| entry.value.as_ref()) {
            if sampling.kind != SpanKind::Sampling || sampling.failed {
                continue;
            }
            let Some(request_key) = sampling.parent else {
                continue;
            };
            let Some(request) = self.span(&request_key) else {
                continue;
            };
            let SpanKind::Request { model } = &request.kind else {
                continue;
            };
            if request.failed
                || request.start_ns > sampling.start_ns
                || request.end_ns < sampling.end_ns
                || sampling.end_ns <= sampling.start_ns
            {
                continue;
            }
            // The verified Codex implementation creates exactly one sampling
            // span and receiving stream per try_run_sampling_request invocation.
            // Require that shape instead of pairing nearby timestamps.
            if self
                .children(request_key)
                .filter(|span| span.kind == SpanKind::Sampling)
                .count()
                != 1
            {
                continue;
            }
            let Some(receiving) = exactly_one(
                self.children(request_key)
                    .filter(|span| span.kind == SpanKind::Receiving),
            ) else {
                continue;
            };
            let Some(completed) = exactly_one(
                self.children(receiving.key)
                    .filter(|span| matches!(span.kind, SpanKind::Completed { .. })),
            ) else {
                continue;
            };
            let SpanKind::Completed { output_tokens } = completed.kind else {
                continue;
            };
            if receiving.failed
                || completed.failed
                || receiving.start_ns < sampling.start_ns
                || receiving.start_ns > sampling.end_ns
                || receiving.end_ns < completed.end_ns
                || completed.start_ns < receiving.start_ns
                || completed.end_ns > sampling.end_ns
            {
                continue;
            }
            let occurred_at_ms = (sampling.end_ns / 1_000_000) as i64;
            if occurred_at_ms <= 0 {
                continue;
            }
            let id = sample_id(&[b"codex", &sampling.key.trace, &sampling.key.span]);
            if self.emitted.contains(&id) {
                continue;
            }
            candidates.push(SpeedSample {
                id,
                agent: "codex".into(),
                model: model.clone(),
                occurred_at_ms,
                output_tokens,
                duration_ms: (sampling.end_ns - sampling.start_ns) as f64 / 1_000_000.0,
            });
        }
        for sample in &candidates {
            while self.emitted.len() >= MAX_EMITTED_IDS {
                if let Some(oldest) = self.emitted_order.pop_front() {
                    self.emitted.remove(&oldest);
                }
            }
            self.emitted.insert(sample.id.clone());
            self.emitted_order.push_back(sample.id.clone());
        }
        candidates
    }

    fn span(&self, key: &SpanKey) -> Option<&TimingSpan> {
        self.spans.get(key)?.value.as_ref()
    }

    fn children(&self, parent: SpanKey) -> impl Iterator<Item = &TimingSpan> {
        self.spans
            .values()
            .filter_map(|entry| entry.value.as_ref())
            .filter(move |span| span.parent == Some(parent))
    }
}

fn exactly_one<T>(mut values: impl Iterator<Item = T>) -> Option<T> {
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SpanKey {
    trace: [u8; 16],
    span: [u8; 8],
}

#[derive(Clone)]
struct CachedSpan {
    inserted: Instant,
    value: Option<TimingSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TimingSpan {
    key: SpanKey,
    parent: Option<SpanKey>,
    start_ns: u64,
    end_ns: u64,
    failed: bool,
    kind: SpanKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SpanKind {
    Request { model: String },
    Sampling,
    Receiving,
    Completed { output_tokens: i64 },
}

#[derive(Clone, Debug)]
enum Scalar {
    String(String),
    Integer(i64),
    Double(f64),
    Invalid,
}

type Attributes = BTreeMap<String, Scalar>;

fn wanted_attribute(key: &str) -> bool {
    matches!(
        key,
        "event.name"
            | "event.timestamp"
            | "model"
            | "duration_ms"
            | "output_tokens"
            | "request_id"
            | "otel.name"
            | "gen_ai.usage.output_tokens"
    )
}

fn string_attribute<'a>(attributes: &'a Attributes, key: &str) -> Option<&'a str> {
    match attributes.get(key)? {
        Scalar::String(value) => Some(value),
        _ => None,
    }
}

fn positive_integer(value: &Scalar) -> Option<i64> {
    let value = match value {
        Scalar::Integer(value) => *value,
        Scalar::Double(value)
            if value.is_finite()
                && value.fract() == 0.0
                && *value > 0.0
                && *value < i64::MAX as f64 =>
        {
            *value as i64
        }
        Scalar::String(value) if decimal_digits(value) => value.parse().ok()?,
        _ => return None,
    };
    (value > 0).then_some(value)
}

fn positive_duration(value: &Scalar) -> Option<f64> {
    let value = match value {
        Scalar::Integer(value) => *value as f64,
        Scalar::Double(value) => *value,
        Scalar::String(value) if !value.is_empty() && value.trim() == value => {
            value.parse().ok()?
        }
        _ => return None,
    };
    (value.is_finite() && value > 0.0).then_some(value)
}

fn decimal_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_model(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_MODEL_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn sample_id(parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part);
    }
    format!("{:x}", hash.finalize())
}

fn claude_sample(
    attributes: &Attributes,
    event_name: Option<&str>,
    body_name: Option<&str>,
    time_ns: u64,
) -> Option<SpeedSample> {
    let attribute_name = string_attribute(attributes, "event.name");
    let names: Vec<_> = [attribute_name, event_name, body_name]
        .into_iter()
        .flatten()
        .filter(|name| !name.is_empty())
        .collect();
    if names.is_empty()
        || names
            .iter()
            .any(|name| !matches!(*name, "api_request" | "claude_code.api_request"))
    {
        return None;
    }
    let model = string_attribute(attributes, "model")?;
    let request_id = string_attribute(attributes, "request_id")?;
    if !valid_model(model)
        || request_id.is_empty()
        || request_id.len() > 512
        || request_id.trim() != request_id
        || request_id.chars().any(char::is_control)
    {
        return None;
    }
    let output_tokens = positive_integer(attributes.get("output_tokens")?)?;
    let duration_ms = positive_duration(attributes.get("duration_ms")?)?;
    let occurred_at_ms = if time_ns > 0 {
        (time_ns / 1_000_000) as i64
    } else {
        chrono::DateTime::parse_from_rfc3339(string_attribute(attributes, "event.timestamp")?)
            .ok()?
            .timestamp_millis()
    };
    (occurred_at_ms > 0).then(|| SpeedSample {
        id: sample_id(&[b"claude", request_id.as_bytes()]),
        agent: "claude".into(),
        model: model.into(),
        occurred_at_ms,
        output_tokens,
        duration_ms,
    })
}

fn deduplicate_samples(samples: Vec<SpeedSample>) -> Result<Vec<SpeedSample>> {
    let mut unique = BTreeMap::new();
    for sample in samples {
        if let Some(existing) = unique.get(&sample.id) {
            if existing != &sample {
                bail!("conflicting timing sample identity");
            }
        } else {
            unique.insert(sample.id.clone(), sample);
        }
    }
    Ok(unique.into_values().collect())
}

fn timing_kind(name: &str, attributes: &Attributes) -> Option<SpanKind> {
    match name {
        "try_run_sampling_request" => {
            let model = string_attribute(attributes, "model")?;
            valid_model(model).then(|| SpanKind::Request {
                model: model.into(),
            })
        }
        "codex.sampling" => Some(SpanKind::Sampling),
        "receiving_stream" => Some(SpanKind::Receiving),
        "completed" => Some(SpanKind::Completed {
            output_tokens: positive_integer(attributes.get("gen_ai.usage.output_tokens")?)?,
        }),
        // Verified on Codex 0.163.0-alpha.10.2: native OTLP can retain the
        // original span name and omit otel.name. record_responses writes this
        // usage field only for Completed(Some(token_usage)), so the positive
        // output field identifies completion; other handlers have no usage.
        "handle_responses" => Some(SpanKind::Completed {
            output_tokens: positive_integer(attributes.get("gen_ai.usage.output_tokens")?)?,
        }),
        _ => None,
    }
}

fn array_field<'a>(value: &'a Value, field: &str) -> Result<&'a [Value]> {
    match value.get(field) {
        Some(Value::Array(values)) => Ok(values),
        None => Ok(&[]),
        _ => bail!("invalid OTLP array field"),
    }
}

fn json_root(body: &[u8]) -> Result<Value> {
    let root: Value = serde_json::from_slice(body).context("invalid OTLP JSON")?;
    if !root.is_object() {
        bail!("invalid OTLP JSON root");
    }
    Ok(root)
}

fn json_attributes(record: &Value) -> Option<Attributes> {
    let mut attributes = Attributes::new();
    let Some(values) = record.get("attributes") else {
        return Some(attributes);
    };
    for entry in values.as_array()? {
        let key = entry.get("key")?.as_str()?;
        if !wanted_attribute(key) {
            continue;
        }
        let value = entry
            .get("value")
            .map(json_scalar)
            .unwrap_or(Scalar::Invalid);
        // OTLP requires unique attribute keys; even identical duplicate keys
        // are malformed and must not silently select one numeric value.
        if attributes.insert(key.into(), value).is_some() {
            return None;
        }
    }
    Some(attributes)
}

fn json_scalar(value: &Value) -> Scalar {
    let Some(object) = value.as_object().filter(|object| object.len() == 1) else {
        return Scalar::Invalid;
    };
    if let Some(value) = object.get("stringValue").and_then(Value::as_str) {
        Scalar::String(value.into())
    } else if let Some(value) = object.get("intValue") {
        value
            .as_i64()
            .or_else(|| value.as_str()?.parse().ok())
            .map(Scalar::Integer)
            .unwrap_or(Scalar::Invalid)
    } else if let Some(value) = object.get("doubleValue").and_then(Value::as_f64) {
        Scalar::Double(value)
    } else {
        Scalar::Invalid
    }
}

fn json_nanos(value: &Value) -> Option<u64> {
    if let Some(value) = value.as_u64() {
        Some(value)
    } else {
        let value = value.as_str()?;
        decimal_digits(value).then(|| value.parse().ok()).flatten()
    }
}

fn claude_json(body: &[u8]) -> Result<Vec<SpeedSample>> {
    let root = json_root(body)?;
    let mut samples = Vec::new();
    for resource in array_field(&root, "resourceLogs")? {
        for scope in array_field(resource, "scopeLogs")? {
            for record in array_field(scope, "logRecords")? {
                let Some(attributes) = json_attributes(record) else {
                    continue;
                };
                let time_ns = match record.get("timeUnixNano") {
                    Some(value) => match json_nanos(value) {
                        Some(value) => value,
                        None => continue,
                    },
                    None => 0,
                };
                if let Some(sample) = claude_sample(
                    &attributes,
                    record.get("eventName").and_then(Value::as_str),
                    record
                        .get("body")
                        .and_then(|body| body.get("stringValue"))
                        .and_then(Value::as_str),
                    time_ns,
                ) {
                    samples.push(sample);
                }
            }
        }
    }
    Ok(samples)
}

fn codex_json(body: &[u8]) -> Result<Vec<TimingSpan>> {
    let root = json_root(body)?;
    let mut spans = Vec::new();
    for resource in array_field(&root, "resourceSpans")? {
        for scope in array_field(resource, "scopeSpans")? {
            for record in array_field(scope, "spans")? {
                if let Some(span) = json_timing_span(record) {
                    spans.push(span);
                }
            }
        }
    }
    Ok(spans)
}

fn json_timing_span(record: &Value) -> Option<TimingSpan> {
    let attributes = json_attributes(record)?;
    let kind = timing_kind(record.get("name")?.as_str()?, &attributes)?;
    let trace = hex_id(record.get("traceId")?.as_str()?)?;
    let span = hex_id(record.get("spanId")?.as_str()?)?;
    let parent = match record.get("parentSpanId").and_then(Value::as_str) {
        Some("") | None => None,
        Some(value) => Some(SpanKey {
            trace,
            span: hex_id(value)?,
        }),
    };
    let start_ns = json_nanos(record.get("startTimeUnixNano")?)?;
    let end_ns = json_nanos(record.get("endTimeUnixNano")?)?;
    if start_ns == 0 || end_ns < start_ns {
        return None;
    }
    Some(TimingSpan {
        key: SpanKey { trace, span },
        parent,
        start_ns,
        end_ns,
        failed: record
            .get("status")
            .and_then(|status| status.get("code"))
            .is_some_and(|code| {
                code.as_i64() == Some(2) || code.as_str() == Some("STATUS_CODE_ERROR")
            }),
        kind,
    })
}

fn hex_id<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != N * 2 || !value.is_ascii() {
        return None;
    }
    let mut decoded = [0; N];
    for (index, byte) in decoded.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    decoded.iter().any(|byte| *byte != 0).then_some(decoded)
}

fn bytes_id<const N: usize>(value: &[u8]) -> Option<[u8; N]> {
    let value: [u8; N] = value.try_into().ok()?;
    value.iter().any(|byte| *byte != 0).then_some(value)
}

fn proto_attributes(values: Vec<proto::KeyValue>) -> Option<Attributes> {
    let mut attributes = Attributes::new();
    for entry in values {
        if !wanted_attribute(&entry.key) {
            continue;
        }
        let value = match entry.value.and_then(|value| value.value) {
            Some(proto::any_value::Value::String(value)) => Scalar::String(value),
            Some(proto::any_value::Value::Int(value)) => Scalar::Integer(value),
            Some(proto::any_value::Value::Double(value)) => Scalar::Double(value),
            _ => Scalar::Invalid,
        };
        if attributes.insert(entry.key, value).is_some() {
            return None;
        }
    }
    Some(attributes)
}

fn claude_protobuf(body: &[u8]) -> Result<Vec<SpeedSample>> {
    let request = proto::LogsRequest::decode(body).context("invalid OTLP protobuf logs")?;
    let mut samples = Vec::new();
    for resource in request.resource_logs {
        for scope in resource.scope_logs {
            for record in scope.log_records {
                let Some(attributes) = proto_attributes(record.attributes) else {
                    continue;
                };
                let body_name = match record.body.and_then(|value| value.value) {
                    Some(proto::any_value::Value::String(value)) => Some(value),
                    _ => None,
                };
                if let Some(sample) = claude_sample(
                    &attributes,
                    Some(&record.event_name),
                    body_name.as_deref(),
                    record.time_unix_nano,
                ) {
                    samples.push(sample);
                }
            }
        }
    }
    Ok(samples)
}

fn codex_protobuf(body: &[u8]) -> Result<Vec<TimingSpan>> {
    let request = proto::TracesRequest::decode(body).context("invalid OTLP protobuf traces")?;
    let mut spans = Vec::new();
    for resource in request.resource_spans {
        for scope in resource.scope_spans {
            for record in scope.spans {
                if let Some(span) = proto_timing_span(record) {
                    spans.push(span);
                }
            }
        }
    }
    Ok(spans)
}

fn proto_timing_span(record: proto::Span) -> Option<TimingSpan> {
    let attributes = proto_attributes(record.attributes)?;
    let kind = timing_kind(&record.name, &attributes)?;
    let trace = bytes_id(&record.trace_id)?;
    let span = bytes_id(&record.span_id)?;
    let parent = if record.parent_span_id.is_empty() {
        None
    } else {
        Some(SpanKey {
            trace,
            span: bytes_id(&record.parent_span_id)?,
        })
    };
    if record.start_time_unix_nano == 0 || record.end_time_unix_nano < record.start_time_unix_nano {
        return None;
    }
    Some(TimingSpan {
        key: SpanKey { trace, span },
        parent,
        start_ns: record.start_time_unix_nano,
        end_ns: record.end_time_unix_nano,
        failed: record.status.is_some_and(|status| status.code == 2),
        kind,
    })
}

// Only the wire fields used above are defined. Unknown resource attributes,
// span events, prompts and tool output are skipped by prost and never cached.
// Field numbers: opentelemetry-proto v1.9.0 common/logs/trace v1 messages.
mod proto {
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct AnyValue {
        #[prost(oneof = "any_value::Value", tags = "1, 2, 3, 4")]
        pub value: Option<any_value::Value>,
    }

    pub mod any_value {
        #[derive(Clone, PartialEq, prost::Oneof)]
        pub enum Value {
            #[prost(string, tag = "1")]
            String(String),
            #[prost(bool, tag = "2")]
            Bool(bool),
            #[prost(int64, tag = "3")]
            Int(i64),
            #[prost(double, tag = "4")]
            Double(f64),
        }
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct KeyValue {
        #[prost(string, tag = "1")]
        pub key: String,
        #[prost(message, optional, tag = "2")]
        pub value: Option<AnyValue>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct LogsRequest {
        #[prost(message, repeated, tag = "1")]
        pub resource_logs: Vec<ResourceLogs>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ResourceLogs {
        #[prost(message, repeated, tag = "2")]
        pub scope_logs: Vec<ScopeLogs>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ScopeLogs {
        #[prost(message, repeated, tag = "2")]
        pub log_records: Vec<LogRecord>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct LogRecord {
        #[prost(fixed64, tag = "1")]
        pub time_unix_nano: u64,
        #[prost(message, optional, tag = "5")]
        pub body: Option<AnyValue>,
        #[prost(message, repeated, tag = "6")]
        pub attributes: Vec<KeyValue>,
        #[prost(string, tag = "12")]
        pub event_name: String,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct TracesRequest {
        #[prost(message, repeated, tag = "1")]
        pub resource_spans: Vec<ResourceSpans>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ResourceSpans {
        #[prost(message, repeated, tag = "2")]
        pub scope_spans: Vec<ScopeSpans>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ScopeSpans {
        #[prost(message, repeated, tag = "2")]
        pub spans: Vec<Span>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Span {
        #[prost(bytes = "vec", tag = "1")]
        pub trace_id: Vec<u8>,
        #[prost(bytes = "vec", tag = "2")]
        pub span_id: Vec<u8>,
        #[prost(bytes = "vec", tag = "4")]
        pub parent_span_id: Vec<u8>,
        #[prost(string, tag = "5")]
        pub name: String,
        #[prost(fixed64, tag = "7")]
        pub start_time_unix_nano: u64,
        #[prost(fixed64, tag = "8")]
        pub end_time_unix_nano: u64,
        #[prost(message, repeated, tag = "9")]
        pub attributes: Vec<KeyValue>,
        #[prost(message, optional, tag = "15")]
        pub status: Option<Status>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Status {
        #[prost(int32, tag = "3")]
        pub code: i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BASE: u64 = 1_700_000_000_000_000_000;
    const TRACE: &str = "11111111111111111111111111111111";

    fn attribute(key: &str, value: Value) -> Value {
        json!({"key": key, "value": value})
    }

    fn span(id: u8, parent: Option<u8>, name: &str, start: u64, end: u64) -> Value {
        json!({
            "traceId": TRACE,
            "spanId": format!("{id:016x}"),
            "parentSpanId": parent.map(|id| format!("{id:016x}")).unwrap_or_default(),
            "name": name,
            "startTimeUnixNano": (BASE + start * 1_000_000).to_string(),
            "endTimeUnixNano": (BASE + end * 1_000_000).to_string(),
            "attributes": []
        })
    }

    fn codex_spans() -> Vec<Value> {
        let mut request = span(1, None, "try_run_sampling_request", 0, 5000);
        request["attributes"] = json!([attribute("model", json!({"stringValue":"gpt-test"}))]);
        let sampling = span(2, Some(1), "codex.sampling", 100, 2100);
        // Both of these parent spans remain open during a subsequent tool wait.
        let receiving = span(3, Some(1), "receiving_stream", 200, 4900);
        let mut completed = span(4, Some(3), "completed", 2000, 2099);
        completed["attributes"] = json!([attribute(
            "gen_ai.usage.output_tokens",
            json!({"intValue":"40"})
        )]);
        vec![request, sampling, receiving, completed]
    }

    fn traces(spans: Vec<Value>) -> Vec<u8> {
        serde_json::to_vec(&json!({"resourceSpans":[{"scopeSpans":[{"spans":spans}]}]})).unwrap()
    }

    fn decode_spans(decoder: &mut Decoder, spans: Vec<Value>) -> Vec<SpeedSample> {
        decoder
            .decode("codex", "traces", "application/json", &traces(spans))
            .unwrap()
    }

    fn claude_record() -> Value {
        json!({
            "timeUnixNano": BASE.to_string(),
            "body":{"stringValue":"claude_code.api_request"},
            "attributes":[
                attribute("event.name", json!({"stringValue":"api_request"})),
                attribute("model", json!({"stringValue":"claude-test"})),
                attribute("request_id", json!({"stringValue":"request-private-id"})),
                attribute("output_tokens", json!({"intValue":"120"})),
                attribute("duration_ms", json!({"doubleValue":1500.25})),
                attribute("user.email", json!({"stringValue":"private@example.invalid"})),
                attribute("prompt", json!({"stringValue":"never retain this"}))
            ]
        })
    }

    fn logs(records: Vec<Value>) -> Vec<u8> {
        serde_json::to_vec(&json!({"resourceLogs":[{"scopeLogs":[{"logRecords":records}]}]}))
            .unwrap()
    }

    fn decode_logs(records: Vec<Value>) -> Vec<SpeedSample> {
        Decoder::default()
            .decode(
                "claude",
                "logs",
                "application/json; charset=utf-8",
                &logs(records),
            )
            .unwrap()
    }

    #[test]
    fn codex_uses_sampling_lifetime_and_exact_tree_not_tool_or_event_duration() {
        let samples = decode_spans(&mut Decoder::default(), codex_spans());
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].duration_ms, 2000.0);
        assert_eq!(samples[0].output_tokens, 40);
        assert_eq!(samples[0].occurred_at_ms, (BASE / 1_000_000 + 2100) as i64);
        assert_eq!(samples[0].model, "gpt-test");
        assert_eq!(samples[0].id.len(), 64);
    }

    #[test]
    fn codex_joins_out_of_order_batches_and_ignores_retries() {
        let spans = codex_spans();
        let mut decoder = Decoder::default();
        assert!(decode_spans(&mut decoder, vec![spans[3].clone()]).is_empty());
        assert!(decode_spans(&mut decoder, vec![spans[1].clone()]).is_empty());
        let samples = decode_spans(&mut decoder, vec![spans[2].clone(), spans[0].clone()]);
        assert_eq!(samples.len(), 1);
        assert!(decode_spans(&mut decoder, spans.clone()).is_empty());
        assert_eq!(
            decode_spans(&mut Decoder::default(), spans)[0].id,
            samples[0].id
        );
    }

    #[test]
    fn codex_native_wire_handler_usage_needs_no_otel_name() {
        let mut spans = codex_spans();
        spans[3]["name"] = json!("handle_responses");
        // The real exporter writes a decimal string inside intValue. Seven
        // other same-named event handlers carry no output usage at all.
        spans[3]["attributes"] = json!([attribute(
            "gen_ai.usage.output_tokens",
            json!({"intValue":"5"})
        )]);
        for id in 5..12 {
            spans.push(span(id, Some(3), "handle_responses", 300, 324));
        }
        let samples = decode_spans(&mut Decoder::default(), spans);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].output_tokens, 5);
        assert_eq!(samples[0].duration_ms, 2000.0);
    }

    #[test]
    fn codex_never_pairs_adjacent_unrelated_requests_or_traces() {
        for mutate in [0, 1, 2] {
            let mut spans = codex_spans();
            match mutate {
                0 => spans[3]["traceId"] = json!("22222222222222222222222222222222"),
                1 => spans[3]["parentSpanId"] = json!("0000000000000009"),
                _ => spans[0]["name"] = json!("whole_turn"),
            }
            assert!(decode_spans(&mut Decoder::default(), spans).is_empty());
        }
        let terminal_only = codex_spans().pop().unwrap();
        assert!(decode_spans(&mut Decoder::default(), vec![terminal_only]).is_empty());
    }

    #[test]
    fn codex_rejects_conflicting_duplicate_and_ambiguous_completion() {
        let mut spans = codex_spans();
        let mut conflict = spans[3].clone();
        conflict["attributes"][0]["value"]["intValue"] = json!("41");
        spans.push(conflict);
        assert!(decode_spans(&mut Decoder::default(), spans).is_empty());

        let mut spans = codex_spans();
        let mut second = spans[3].clone();
        second["spanId"] = json!("0000000000000005");
        spans.push(second);
        assert!(decode_spans(&mut Decoder::default(), spans).is_empty());
    }

    #[test]
    fn codex_requires_valid_ids_model_timestamps_and_success() {
        for (index, field, value) in [
            (1, "spanId", json!("short")),
            (1, "traceId", json!("00000000000000000000000000000000")),
            (1, "startTimeUnixNano", json!(-1)),
            (1, "endTimeUnixNano", json!("NaN")),
            (1, "endTimeUnixNano", json!(BASE.to_string())),
            (1, "status", json!({"code":2})),
        ] {
            let mut spans = codex_spans();
            spans[index][field] = value;
            assert!(decode_spans(&mut Decoder::default(), spans).is_empty());
        }
        let mut spans = codex_spans();
        spans[0]["attributes"][0]["value"]["stringValue"] = json!("bad\nmodel");
        assert!(decode_spans(&mut Decoder::default(), spans).is_empty());
    }

    #[test]
    fn claude_pairs_request_output_duration_and_hashes_identity() {
        let record = claude_record();
        let samples = decode_logs(vec![record.clone(), record]);
        assert_eq!(samples.len(), 1);
        let sample = &samples[0];
        assert_eq!(sample.output_tokens, 120);
        assert_eq!(sample.duration_ms, 1500.25);
        assert_eq!(sample.occurred_at_ms, (BASE / 1_000_000) as i64);
        assert_eq!(sample.model, "claude-test");
        assert_eq!(sample.id.len(), 64);
        let retained = format!("{sample:?}");
        assert!(!retained.contains("private"));
        assert!(!retained.contains("never retain"));
    }

    #[test]
    fn claude_accepts_numeric_string_fields_and_iso_timestamp_fallback() {
        let mut record = claude_record();
        record.as_object_mut().unwrap().remove("timeUnixNano");
        record["attributes"][3]["value"] = json!({"stringValue":"120"});
        record["attributes"][4]["value"] = json!({"stringValue":"1500.25"});
        record["attributes"].as_array_mut().unwrap().push(attribute(
            "event.timestamp",
            json!({"stringValue":"2023-11-14T22:13:20.000Z"}),
        ));
        let samples = decode_logs(vec![record]);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].occurred_at_ms, (BASE / 1_000_000) as i64);
    }

    #[test]
    fn claude_rejects_errors_missing_ids_bad_numeric_fields_and_model_controls() {
        for (index, value) in [
            (0, json!({"stringValue":"api_error"})),
            (1, json!({"stringValue":"claude\u{0}test"})),
            (2, json!({"stringValue":""})),
            (3, json!({"doubleValue":1.5})),
            (3, json!({"stringValue":"-1"})),
            (3, json!({"stringValue":"9223372036854775808"})),
            (4, json!({"stringValue":"NaN"})),
            (4, json!({"stringValue":"inf"})),
            (4, json!({"intValue":"0"})),
        ] {
            let mut record = claude_record();
            record["attributes"][index]["value"] = value;
            assert!(decode_logs(vec![record]).is_empty());
        }
        let mut record = claude_record();
        record["timeUnixNano"] = json!(1.5);
        assert!(decode_logs(vec![record]).is_empty());
    }

    #[test]
    fn claude_conflicting_duplicate_request_is_rejected_without_payload_in_error() {
        let record = claude_record();
        let mut conflict = record.clone();
        conflict["attributes"][3]["value"] = json!({"intValue":"121"});
        let error = Decoder::default()
            .decode(
                "claude",
                "logs",
                "application/json",
                &logs(vec![record, conflict]),
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "conflicting timing sample identity");
    }

    fn proto_value(value: &Value) -> proto::AnyValue {
        let value = match json_scalar(value) {
            Scalar::String(value) => proto::any_value::Value::String(value),
            Scalar::Integer(value) => proto::any_value::Value::Int(value),
            Scalar::Double(value) => proto::any_value::Value::Double(value),
            Scalar::Invalid => proto::any_value::Value::Bool(false),
        };
        proto::AnyValue { value: Some(value) }
    }

    fn proto_attrs(record: &Value) -> Vec<proto::KeyValue> {
        record["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| proto::KeyValue {
                key: entry["key"].as_str().unwrap().into(),
                value: Some(proto_value(&entry["value"])),
            })
            .collect()
    }

    #[test]
    fn protobuf_and_json_claude_contracts_match() {
        let record = claude_record();
        let request = proto::LogsRequest {
            resource_logs: vec![proto::ResourceLogs {
                scope_logs: vec![proto::ScopeLogs {
                    log_records: vec![proto::LogRecord {
                        time_unix_nano: BASE,
                        body: Some(proto_value(&record["body"])),
                        attributes: proto_attrs(&record),
                        event_name: String::new(),
                    }],
                }],
            }],
        };
        let protobuf = Decoder::default()
            .decode(
                "claude",
                "logs",
                "application/x-protobuf",
                &request.encode_to_vec(),
            )
            .unwrap();
        assert_eq!(protobuf, decode_logs(vec![record]));
    }

    #[test]
    fn protobuf_and_json_codex_contracts_match() {
        let spans = codex_spans();
        let proto_spans = spans
            .iter()
            .map(|span| proto::Span {
                trace_id: hex_id::<16>(span["traceId"].as_str().unwrap())
                    .unwrap()
                    .to_vec(),
                span_id: hex_id::<8>(span["spanId"].as_str().unwrap())
                    .unwrap()
                    .to_vec(),
                parent_span_id: hex_id::<8>(span["parentSpanId"].as_str().unwrap())
                    .map(|id| id.to_vec())
                    .unwrap_or_default(),
                name: span["name"].as_str().unwrap().into(),
                start_time_unix_nano: json_nanos(&span["startTimeUnixNano"]).unwrap(),
                end_time_unix_nano: json_nanos(&span["endTimeUnixNano"]).unwrap(),
                attributes: proto_attrs(span),
                status: None,
            })
            .collect();
        let request = proto::TracesRequest {
            resource_spans: vec![proto::ResourceSpans {
                scope_spans: vec![proto::ScopeSpans { spans: proto_spans }],
            }],
        };
        let protobuf = Decoder::default()
            .decode(
                "codex",
                "traces",
                "application/x-protobuf",
                &request.encode_to_vec(),
            )
            .unwrap();
        assert_eq!(protobuf, decode_spans(&mut Decoder::default(), spans));
    }

    #[test]
    fn pending_cache_is_bounded_and_does_not_retain_unknown_attributes() {
        let mut decoder = Decoder::default();
        let mut span = json_timing_span(&codex_spans()[1]).unwrap();
        for index in 1..=MAX_PENDING_SPANS + 5 {
            span.key.span = (index as u64).to_be_bytes();
            decoder.cache_span(span.clone());
        }
        assert_eq!(decoder.spans.len(), MAX_PENDING_SPANS);
        assert_eq!(decoder.span_order.len(), MAX_PENDING_SPANS);
        // Advance the comparison clock rather than subtracting a day from an
        // Instant: CI machines may have booted less than one day ago.
        decoder.expire_spans_at(Instant::now() + PENDING_LIFETIME + Duration::from_secs(1));
        assert!(decoder.spans.is_empty());
        assert_eq!(decoder.emitted.len(), 0);
    }
}
