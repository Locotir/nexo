//! Anthropic messages: `POST {base}/v1/messages`, content blocks, SSE with
//! named events and explicit cache breakpoints. Served compatibly by
//! several other vendors (`…/anthropic` base URLs).

use async_trait::async_trait;
use nexo_core::{
    Effort, ErrorKind, EventStream, FinishReason, ImageSource, Message, ModelInfo, ModelRequest, Part, Protocol,
    ProviderConfig, ProviderError, ReasoningConfig, Role, StreamEvent, Usage,
};
use serde_json::{json, Value};

use crate::http;
use crate::openai_chat::{parse_args, temp};
use crate::parse::{stream_error_kind, Feed, Progress};
use crate::sse::SseEvent;

#[derive(Debug, Default, Clone, Copy)]
pub struct Anthropic;

pub const ID: &str = "anthropic";
pub const VERSION: &str = "2023-06-01";

/// The beta that lets the API drop a thinking block whose history was edited, instead of a 400.
pub const BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";
/// The reply size asked of a Claude 5.5 model when the person set no smaller cap.
const MODERN_MAX_TOKENS: u32 = 64_000;
/// The largest reply of a Claude 5.5 model.
pub const MODERN_MAX_OUTPUT: u32 = 128_000;

/// The family (`sonnet`, `opus`, `haiku`) and version of a name like `claude-sonnet-5-5`, also as a
/// cloud id (`anthropic.claude-sonnet-5-5-v1:0`, `claude-sonnet-5-5@20260101`).
fn family_version(model: &str) -> Option<(&'static str, u32, u32)> {
    let m = model.to_ascii_lowercase();
    let rest = &m[m.find("claude-")? + "claude-".len()..];
    let mut t = rest.split(['-', '.', '@', ':']);
    let fam = match t.next()? {
        "sonnet" => "sonnet",
        "opus" => "opus",
        "haiku" => "haiku",
        _ => return None,
    };
    let major: u32 = t.next()?.parse().ok()?;
    // a date after the major (`claude-sonnet-5-20260101`) is not a minor version
    let minor: u32 = t.next().filter(|s| s.len() <= 2).and_then(|s| s.parse().ok()).unwrap_or(0);
    Some((fam, major, minor))
}

fn family(model: &str) -> Option<&'static str> {
    family_version(model).map(|f| f.0)
}

/// Claude 5.5 and later: adaptive thinking, effort, no sampling parameters.
pub fn modern(model: &str) -> bool {
    family_version(model).is_some_and(|(_, major, minor)| (major, minor) >= (5, 5))
}

/// Reasoning off is a request for less thinking: before 5.5 it is no thinking, and on 5.5 Sonnet has a lowest
/// setting; the other 5.5 models take nothing for it and think at the server's default effort.
pub fn off_thinks_less(model: &str) -> bool {
    !modern(model) || family(model) == Some("sonnet")
}

impl Anthropic {
    pub fn url(cfg: &ProviderConfig) -> String {
        http::join_versioned(&cfg.base_url, "v1", "messages")
    }

    pub fn headers(cfg: &ProviderConfig, key: Option<&str>) -> reqwest::header::HeaderMap {
        let version = cfg.extra.get("api_version").and_then(|v| v.as_str()).unwrap_or(VERSION).to_string();
        let mut own = vec![
            ("content-type", "application/json".to_string()),
            ("accept", "text/event-stream".to_string()),
            ("anthropic-version", version),
        ];
        if let Some(k) = key {
            if cfg.extra.get("bearer").and_then(|v| v.as_bool()).unwrap_or(false) {
                own.push(("authorization", format!("Bearer {k}")));
            } else {
                own.push(("x-api-key", k.to_string()));
            }
        }
        http::headers(cfg, &own)
    }

    pub fn body(cfg: &ProviderConfig, req: &ModelRequest) -> Value {
        let cache = req.cache && cfg.extra.get("cache").and_then(|v| v.as_bool()).unwrap_or(true);
        let bp = json!({"type": "ephemeral"});
        let mut body = json!({"model": req.model, "max_tokens": req.max_tokens, "stream": true});
        if !req.system.is_empty() {
            let mut sys = json!({"type": "text", "text": req.system});
            if cache {
                sys["cache_control"] = bp.clone();
            }
            body["system"] = json!([sys]);
        }
        if !req.tools.is_empty() {
            let mut tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.parameters}))
                .collect();
            if cache {
                if let Some(last) = tools.last_mut() {
                    last["cache_control"] = bp.clone();
                }
            }
            body["tools"] = Value::Array(tools);
            if req.no_tool_call {
                body["tool_choice"] = json!({"type": "none"});
            }
        }
        let mut messages: Vec<Value> = Vec::new();
        // a tool round of this request lost its thinking block because another model made it
        let mut broken_round = false;
        let round_start = req
            .messages
            .iter()
            .rposition(|m| m.role == Role::User && !m.content.iter().any(Part::is_tool_result))
            .unwrap_or(0);
        for (i, m) in req.messages.iter().enumerate() {
            let (v, dropped) = convert(m, &req.model);
            broken_round |= dropped && i > round_start && m.has_tool_calls();
            match messages.last_mut() {
                // the protocol wants strict alternation: same roles are joined
                Some(prev) if prev["role"] == v["role"] => {
                    if let (Some(a), Some(b)) = (prev["content"].as_array_mut(), v["content"].as_array()) {
                        a.extend(b.iter().cloned());
                    }
                }
                _ => messages.push(v),
            }
        }
        if cache {
            if let Some(last) = messages.last_mut().and_then(|m| m["content"].as_array_mut()).and_then(|c| c.last_mut())
            {
                last["cache_control"] = bp;
            }
        }
        body["messages"] = Value::Array(messages);
        if modern(&req.model) {
            Self::modern_fields(cfg, req, &mut body);
        } else {
            if let Some(t) = req.temperature {
                body["temperature"] = temp(t);
            }
            if !req.stop.is_empty() {
                body["stop_sequences"] = json!(req.stop);
            }
            let budget = match req.reasoning {
                Some(ReasoningConfig::Budget(b)) => Some(b.max(1024)),
                Some(ReasoningConfig::Effort(Effort::Minimal)) => Some(1024),
                Some(ReasoningConfig::Effort(Effort::Low)) => Some(2048),
                Some(ReasoningConfig::Effort(Effort::Medium)) => Some(8192),
                Some(ReasoningConfig::Effort(Effort::High)) => Some(16384),
                _ => None,
            };
            // SetModel can land between the rounds of one turn (the command loop in
            // `nexo-agent/src/turn.rs` keeps serving while the turn task runs). The protocol
            // wants the thinking block of a tool round back, so when the model that made it is
            // gone, this request goes without thinking rather than invalid.
            // the budget must stay below max_tokens and at least 1024: a reply too small for that goes without
            if let Some(b) = budget.filter(|_| !broken_round && req.max_tokens > 1024) {
                let b = b.min(req.max_tokens - 1);
                body["thinking"] = json!({"type": "enabled", "budget_tokens": b});
                if body["temperature"].is_number() {
                    body.as_object_mut().map(|o| o.remove("temperature")); // not allowed with thinking
                }
            }
        }
        for (k, v) in &req.extra {
            // the models of the 5.5 line reject sampling parameters whatever their value
            if modern(&req.model) && matches!(k.as_str(), "temperature" | "top_p" | "top_k") {
                continue;
            }
            body[k] = v.clone();
        }
        body
    }

    /// The fields of the Claude 5.5 line: `thinking` is adaptive (a manual budget, `enabled` and sampling
    /// parameters are all a 400), the depth is `output_config.effort`, and the reply gets room for the
    /// reasoning, which counts inside `max_tokens`.
    fn modern_fields(cfg: &ProviderConfig, req: &ModelRequest, body: &mut Value) {
        let effort = match req.reasoning {
            None => Some("medium"),
            Some(ReasoningConfig::Off) => None,
            Some(ReasoningConfig::Effort(Effort::Minimal | Effort::Low)) => Some("low"),
            Some(ReasoningConfig::Effort(Effort::Medium)) => Some("medium"),
            Some(ReasoningConfig::Effort(Effort::High)) => Some("high"),
            Some(ReasoningConfig::Budget(b)) => Some(match b {
                0..=2048 => "low",
                2049..=12_000 => "medium",
                _ => "high",
            }),
        };
        match effort {
            Some(e) => {
                body["output_config"] = json!({"effort": e});
                let mut thinking = json!({"type": "adaptive"});
                // thinking blocks are valid only for the history they were made in; the curator rewrites
                // history, so a block whose prefix changed is dropped by the API instead of failing the call
                if cfg.extra.get("block_binding").and_then(|v| v.as_bool()).unwrap_or(true) {
                    thinking["block_binding"] = json!({"prefix_mismatch_behavior": "drop_block"});
                }
                body["thinking"] = thinking;
            }
            None => {
                // reasoning off: Sonnet has a lowest setting; the others always think and take nothing
                if family(&req.model) == Some("sonnet") {
                    body["thinking"] = json!({"type": "between_tools"});
                }
                // `between_tools` takes no `block_binding`: the blocks of earlier turns stay out
                if let Some(ms) = body["messages"].as_array_mut() {
                    for m in ms {
                        if let Some(c) = m["content"].as_array_mut() {
                            c.retain(|b| !matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking")));
                            if c.is_empty() {
                                c.push(json!({"type": "text", "text": "(empty)"}));
                            }
                        }
                    }
                }
            }
        }
        // a large file written in one call must not be cut, and thinking shares the room
        let floor = if req.max_tokens >= nexo_core::config::DEFAULT_MAX_TOKENS { MODERN_MAX_TOKENS } else { 4096 };
        body["max_tokens"] = json!(req.max_tokens.max(floor).min(MODERN_MAX_OUTPUT));
    }

    pub fn parse(events: impl futures::Stream<Item = Result<SseEvent, String>> + Send + 'static) -> EventStream {
        crate::parse::run(events, MessagesParser::default())
    }
}

/// The message as the protocol takes it, and whether a thinking block of another model was left out.
fn convert(m: &Message, model: &str) -> (Value, bool) {
    let mut dropped = false;
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let mut content = Vec::new();
    for p in &m.content {
        match p {
            Part::Text { text } => {
                // a text block of blanks only is a 400 ("text content blocks must contain non-whitespace text")
                if !text.trim().is_empty() {
                    content.push(json!({"type": "text", "text": text}));
                }
            }
            p if p.is_foreign_reasoning(model) => dropped = true,
            Part::Reasoning { text, signature, .. } => {
                // the protocol only takes a thinking block back with its signature
                if let Some(sig) = signature {
                    content.push(json!({"type": "thinking", "thinking": text, "signature": sig}));
                }
            }
            Part::RedactedReasoning { data, .. } => content.push(json!({"type": "redacted_thinking", "data": data})),
            Part::ToolCall { id, name, args } => {
                content.push(json!({"type": "tool_use", "id": id, "name": name, "input": args}))
            }
            Part::ToolResult { call_id, content: c, is_error, .. } => {
                // a result with pictures is a list of blocks, the rest a plain string
                let body = if c.iter().any(|p| matches!(p, Part::Image { .. })) {
                    let blocks: Vec<Value> = c
                        .iter()
                        .filter_map(|p| match p {
                            Part::Text { text } => Some(json!({"type": "text", "text": text})),
                            Part::Image { source } => Some(json!({"type": "image", "source": image_source(source)})),
                            _ => None,
                        })
                        .collect();
                    Value::Array(blocks)
                } else {
                    Value::String(nexo_core::message::result_text(c))
                };
                let mut r = json!({"type": "tool_result", "tool_use_id": call_id, "content": body});
                if *is_error {
                    r["is_error"] = json!(true);
                }
                content.push(r);
            }
            Part::Image { source } => content.push(json!({"type": "image", "source": image_source(source)})),
        }
    }
    if content.is_empty() {
        content.push(json!({"type": "text", "text": "(empty)"}));
    }
    (json!({"role": role, "content": content}), dropped)
}

fn image_source(s: &ImageSource) -> Value {
    match s {
        ImageSource::Base64 { media_type, data } => json!({"type": "base64", "media_type": media_type, "data": data}),
        ImageSource::Url { url } => json!({"type": "url", "url": url}),
    }
}

#[derive(Debug, Default)]
pub struct MessagesParser {
    /// Open blocks by index: (kind, id, name, fragments).
    blocks: Vec<(u64, String, String, String, String)>,
    usage: Usage,
    prog: Progress,
    usage_sent: bool,
}

impl Feed for MessagesParser {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        let v: Value = match serde_json::from_str(data.trim()) {
            Ok(v) => v,
            Err(_) => return out,
        };
        if v["type"].is_string() {
            self.prog.started = true;
        } else if let Some(e) = v.get("error").filter(|e| e.is_object()) {
            // a plain error object, sent without event framing by some gateways
            let msg = http::pretty_error(&v.to_string());
            let kind = stream_error_kind(e["type"].as_str().unwrap_or(""), &msg);
            out.push(StreamEvent::Error(ProviderError::new(kind, msg)));
            self.blocks.clear(); // calls cut by the error are dropped, not closed
            self.prog.started = true;
            self.prog.finished = true;
            return out;
        }
        match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                let m = &v["message"];
                out.push(StreamEvent::Start {
                    response_id: m["id"].as_str().map(String::from),
                    model: m["model"].as_str().map(String::from),
                });
                self.take_usage(&m["usage"]);
            }
            "content_block_start" => {
                let idx = v["index"].as_u64().unwrap_or(0);
                let b = &v["content_block"];
                let kind = b["type"].as_str().unwrap_or("").to_string();
                match kind.as_str() {
                    "tool_use" => {
                        let id = b["id"].as_str().unwrap_or("").to_string();
                        let name = b["name"].as_str().unwrap_or("").to_string();
                        self.prog.saw_tool = true;
                        self.prog.content = true;
                        out.push(StreamEvent::ToolCallStart { id: id.clone(), name: name.clone() });
                        self.blocks.push((idx, kind, id, name, String::new()));
                    }
                    "redacted_thinking" => {
                        out.push(StreamEvent::RedactedReasoning { data: b["data"].as_str().unwrap_or("").to_string() });
                    }
                    "thinking" => {
                        // the part is created by the first delta; an empty one by the signature
                        self.blocks.push((idx, kind, String::new(), String::new(), String::new()));
                    }
                    "text" => {
                        if let Some(t) = b["text"].as_str().filter(|t| !t.is_empty()) {
                            self.prog.content = true;
                            out.push(StreamEvent::TextDelta(t.to_string()));
                        }
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let idx = v["index"].as_u64().unwrap_or(0);
                let d = &v["delta"];
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        if let Some(t) = d["text"].as_str() {
                            self.prog.content = true;
                            out.push(StreamEvent::TextDelta(t.to_string()));
                        }
                    }
                    "thinking_delta" => {
                        if let Some(t) = d["thinking"].as_str() {
                            self.prog.content = true;
                            out.push(StreamEvent::ReasoningDelta(t.to_string()));
                        }
                    }
                    "signature_delta" => {
                        if let Some(s) = d["signature"].as_str() {
                            out.push(StreamEvent::ReasoningMeta { signature: Some(s.to_string()), encrypted: None });
                        }
                    }
                    "input_json_delta" => {
                        if let (Some(j), Some(b)) =
                            (d["partial_json"].as_str(), self.blocks.iter_mut().find(|b| b.0 == idx))
                        {
                            b.4.push_str(j);
                            out.push(StreamEvent::ToolCallDelta { id: b.2.clone(), partial_json: j.to_string() });
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let idx = v["index"].as_u64().unwrap_or(0);
                if let Some(pos) = self.blocks.iter().position(|b| b.0 == idx) {
                    let (_, kind, id, name, frags) = self.blocks.remove(pos);
                    if kind == "tool_use" {
                        out.push(StreamEvent::ToolCallEnd { id, name, args: parse_args(&frags) });
                    }
                }
            }
            "message_delta" => {
                self.take_usage(&v["usage"]);
                if let Some(r) = v["delta"]["stop_reason"].as_str() {
                    out.extend(self.end_calls());
                    if !self.prog.finished {
                        self.prog.finished = true;
                        out.push(StreamEvent::Finish(match r {
                            "max_tokens" | "model_context_window_exceeded" => FinishReason::Length,
                            "tool_use" => FinishReason::ToolUse,
                            "refusal" => FinishReason::ContentFilter,
                            _ if self.prog.saw_tool => FinishReason::ToolUse,
                            _ => FinishReason::Stop,
                        }));
                    }
                }
                if !self.usage_sent {
                    self.usage_sent = true;
                    out.push(StreamEvent::Usage(self.usage));
                }
            }
            "message_stop" => {
                out.extend(self.end_calls());
                if !self.prog.finished {
                    self.prog.finished = true;
                    out.push(StreamEvent::Finish(if self.prog.saw_tool {
                        FinishReason::ToolUse
                    } else {
                        FinishReason::Stop
                    }));
                }
                if !self.usage_sent && !self.usage.is_empty() {
                    self.usage_sent = true;
                    out.push(StreamEvent::Usage(self.usage));
                }
            }
            "error" => {
                let msg = http::pretty_error(&v.to_string());
                let kind = stream_error_kind(v["error"]["type"].as_str().unwrap_or(""), &msg);
                out.push(StreamEvent::Error(ProviderError::new(kind, msg)));
                self.blocks.clear(); // calls cut by the error are dropped, not closed
                self.prog.finished = true;
            }
            _ => {} // ping
        }
        out
    }

    fn fail(&mut self, e: String) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        self.blocks.clear(); // calls cut by the transport are dropped, not closed
        if !self.prog.finished {
            self.prog.finished = true;
            out.push(StreamEvent::Error(ProviderError::new(ErrorKind::Network, format!("stream cut: {e}"))));
        }
        out
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        if !self.prog.finished && self.prog.content && !self.usage_sent && !self.usage.is_empty() {
            self.usage_sent = true;
            out.push(StreamEvent::Usage(self.usage));
        }
        // calls still open without a stop: their arguments may be cut
        let open = self.blocks.iter().any(|b| b.1 == "tool_use");
        self.blocks.clear();
        out.extend(self.prog.at_end_with_calls(open));
        out
    }
}

impl MessagesParser {
    /// Counts arrive in parts (input at the start, output at the end) and
    /// the later ones are cumulative: each field present replaces the old.
    fn take_usage(&mut self, u: &Value) {
        if !u.is_object() {
            return;
        }
        if let Some(n) = u["input_tokens"].as_u64() {
            self.usage.input = n;
        }
        if let Some(n) = u["output_tokens"].as_u64() {
            self.usage.output = n;
        }
        if let Some(n) = u["cache_read_input_tokens"].as_u64() {
            self.usage.cache_read = n;
        }
        if let Some(n) = u["cache_creation_input_tokens"].as_u64() {
            self.usage.cache_write = n;
        }
    }

    fn end_calls(&mut self) -> Vec<StreamEvent> {
        std::mem::take(&mut self.blocks)
            .into_iter()
            .filter(|b| b.1 == "tool_use")
            .map(|(_, _, id, name, frags)| StreamEvent::ToolCallEnd { id, name, args: parse_args(&frags) })
            .collect()
    }
}

#[async_trait]
impl Protocol for Anthropic {
    fn id(&self) -> &'static str {
        ID
    }

    async fn stream(
        &self,
        cfg: &ProviderConfig,
        key: Option<&str>,
        req: &ModelRequest,
    ) -> Result<EventStream, ProviderError> {
        let json = Self::body(cfg, req);
        let mut headers = Self::headers(cfg, key);
        if json["thinking"]["block_binding"].is_object() {
            let have = headers.get("anthropic-beta").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            if !have.split(',').any(|b| b.trim() == BINDING_BETA) {
                let v = if have.is_empty() { BINDING_BETA.to_string() } else { format!("{have},{BINDING_BETA}") };
                if let Ok(v) = reqwest::header::HeaderValue::from_str(&v) {
                    headers.insert("anthropic-beta", v);
                }
            }
        }
        let body = http::post_stream(&Self::url(cfg), headers, &json).await?;
        Ok(Self::parse(http::sse(body)))
    }

    async fn list_models(&self, cfg: &ProviderConfig, key: Option<&str>) -> Result<Vec<ModelInfo>, ProviderError> {
        let url = http::join_versioned(&cfg.base_url, "v1", "models?limit=100");
        let v = http::get_json(&url, Self::headers(cfg, key)).await?;
        Ok(v["data"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        Some(ModelInfo {
                            id: m["id"].as_str()?.to_string(),
                            display: m["display_name"].as_str().map(String::from),
                            ..Default::default()
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::ToolSpec;

    #[test]
    fn a_stream_cut_after_message_start_is_an_error_not_an_empty_reply() {
        let mut p = MessagesParser::default();
        p.feed(r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":5}}}"#);
        let end = p.finish();
        assert!(matches!(&end[0], StreamEvent::Error(e) if e.kind == ErrorKind::Network), "{end:?}");
        let mut p = MessagesParser::default();
        let evs = p.feed(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#);
        assert!(matches!(&evs[0], StreamEvent::Error(e) if e.kind == ErrorKind::Overloaded));
        assert!(p.finish().is_empty(), "nothing more after an error");
    }

    const CALL_OPEN: [&str; 3] = [
        r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":5}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_1","name":"read","input":{}}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"pa"}}"#,
    ];

    #[test]
    fn an_error_drops_open_tool_calls_instead_of_closing_them() {
        let mut p = MessagesParser::default();
        let mut evs: Vec<StreamEvent> = CALL_OPEN.iter().flat_map(|d| p.feed(d)).collect();
        evs.extend(p.feed(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#));
        evs.extend(p.finish());
        assert!(!evs.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { .. })), "{evs:?}");
        assert!(matches!(evs.last(), Some(StreamEvent::Error(e)) if e.kind == ErrorKind::Overloaded));
    }

    #[test]
    fn a_clean_end_inside_a_tool_call_is_a_truncation_error() {
        let mut p = MessagesParser::default();
        let mut evs: Vec<StreamEvent> = CALL_OPEN.iter().flat_map(|d| p.feed(d)).collect();
        evs.extend(p.finish());
        assert!(!evs.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { .. } | StreamEvent::Finish(_))), "{evs:?}");
        assert!(matches!(evs.last(), Some(StreamEvent::Error(e)) if e.message.contains("truncated")), "{evs:?}");
    }

    #[test]
    fn a_context_window_stop_is_a_length_finish() {
        let mut p = MessagesParser::default();
        let mut evs: Vec<StreamEvent> = CALL_OPEN.iter().flat_map(|d| p.feed(d)).collect();
        evs.extend(p.feed(r#"{"type":"message_delta","delta":{"stop_reason":"model_context_window_exceeded"},"usage":{"output_tokens":3}}"#));
        assert!(evs.contains(&StreamEvent::Finish(FinishReason::Length)), "{evs:?}");
    }

    #[test]
    fn the_thinking_budget_stays_below_max_tokens() {
        let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
        let mut req = ModelRequest::new("claude-x", "", vec![Message::user("hi")]);
        req.reasoning = Some(ReasoningConfig::Budget(4000));
        req.max_tokens = 1000;
        assert!(Anthropic::body(&cfg, &req).get("thinking").is_none(), "no room for the 1024 minimum");
        req.max_tokens = 1024;
        assert!(Anthropic::body(&cfg, &req).get("thinking").is_none());
        req.max_tokens = 2000;
        assert_eq!(Anthropic::body(&cfg, &req)["thinking"]["budget_tokens"], 1999);
        req.max_tokens = 1025;
        assert_eq!(Anthropic::body(&cfg, &req)["thinking"]["budget_tokens"], 1024);
        req.max_tokens = 8192;
        assert_eq!(Anthropic::body(&cfg, &req)["thinking"]["budget_tokens"], 4000);
    }

    #[test]
    fn body_has_system_field_blocks_and_cache_breakpoints() {
        let mut req = ModelRequest::new(
            "claude-x",
            "sys",
            vec![
                Message::user("hi"),
                Message::new(
                    Role::Assistant,
                    vec![
                        Part::Reasoning { text: "t".into(), signature: Some("S".into()), encrypted: None, model: None },
                        Part::Reasoning { text: "unsigned".into(), signature: None, encrypted: None, model: None },
                        Part::tool_call("tu_1", "read", json!({"path": "a"})),
                    ],
                ),
                Message::tool_results(vec![Part::tool_result("tu_1", "boom", true)]),
            ],
        );
        req.tools =
            vec![ToolSpec { name: "read".into(), description: "d".into(), parameters: json!({"type": "object"}) }];
        req.reasoning = Some(ReasoningConfig::Budget(4000));
        let b = Anthropic::body(&ProviderConfig::new(ID, "https://api.anthropic.com", None), &req);
        assert_eq!(b["system"][0]["text"], "sys");
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(b["tools"][0]["cache_control"]["type"], "ephemeral");
        assert!(b.get("tool_choice").is_none());
        req.no_tool_call = true;
        let b2 = Anthropic::body(&ProviderConfig::new(ID, "https://api.anthropic.com", None), &req);
        assert_eq!(b2["tool_choice"], json!({"type": "none"}));
        assert_eq!(b2["tools"], b["tools"], "the tools stay declared");
        let msgs = b["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["content"][0], json!({"type": "thinking", "thinking": "t", "signature": "S"}));
        assert_eq!(msgs[1]["content"][1]["type"], "tool_use", "an unsigned thinking block is not sent");
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[2]["content"][0]["is_error"], true);
        assert_eq!(msgs[2]["content"][0]["cache_control"]["type"], "ephemeral", "the last block is a breakpoint");
        assert_eq!(b["thinking"]["budget_tokens"], 4000);
        assert_eq!(b["max_tokens"], 8192);
        assert_eq!(
            Anthropic::url(&ProviderConfig::new(ID, "https://api.deepseek.com/anthropic", None)),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
    }

    #[test]
    fn parses_named_events_with_cache_usage() {
        let data = [
            r#"{"type":"message_start","message":{"id":"msg_1","model":"m","usage":{"input_tokens":10,"cache_read_input_tokens":90,"cache_creation_input_tokens":5,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SIG"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hi"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"tu_1","name":"read","input":{}}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\""}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":": \"a\"}"}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":12}}"#,
            r#"{"type":"message_stop"}"#,
        ];
        let mut p = MessagesParser::default();
        let evs: Vec<StreamEvent> = data.iter().flat_map(|d| p.feed(d)).collect();
        assert!(matches!(evs[0], StreamEvent::Start { .. }));
        assert_eq!(evs[1], StreamEvent::ReasoningDelta("hmm".into()));
        assert_eq!(evs[2], StreamEvent::ReasoningMeta { signature: Some("SIG".into()), encrypted: None });
        assert_eq!(evs[3], StreamEvent::TextDelta("Hi".into()));
        assert_eq!(evs[4], StreamEvent::ToolCallStart { id: "tu_1".into(), name: "read".into() });
        assert_eq!(
            evs[7],
            StreamEvent::ToolCallEnd { id: "tu_1".into(), name: "read".into(), args: json!({"path": "a"}) }
        );
        assert_eq!(evs[8], StreamEvent::Finish(FinishReason::ToolUse));
        assert_eq!(
            evs[9],
            StreamEvent::Usage(Usage { input: 10, output: 12, cache_read: 90, cache_write: 5, reasoning: 0 })
        );
        assert_eq!(evs.len(), 10);
    }

    fn reasoning_history(made_by: &str) -> Vec<Message> {
        vec![
            Message::user("hi"),
            Message::new(
                Role::Assistant,
                vec![
                    Part::Reasoning {
                        text: "t".into(),
                        signature: Some("S".into()),
                        encrypted: None,
                        model: Some(made_by.into()),
                    },
                    Part::RedactedReasoning { data: "R".into(), model: Some(made_by.into()) },
                    Part::tool_call("tu_1", "read", json!({})),
                ],
            ),
            Message::tool_results(vec![Part::tool_result("tu_1", "ok", false)]),
        ]
    }

    #[test]
    fn thinking_of_another_model_is_not_sent_back() {
        let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
        let mut req = ModelRequest::new("model-a", "sys", reasoning_history("model-a"));
        req.reasoning = Some(ReasoningConfig::Budget(4000));
        let same = Anthropic::body(&cfg, &req);
        assert_eq!(same["messages"][1]["content"][0]["signature"], "S");
        assert_eq!(same["messages"][1]["content"][1]["type"], "redacted_thinking");
        assert_eq!(same["thinking"]["budget_tokens"], 4000);

        req.model = "model-b".into();
        let other = Anthropic::body(&cfg, &req);
        let text = other["messages"].to_string();
        assert!(!text.contains("\"signature\"") && !text.contains("redacted_thinking"), "{text}");
        assert_eq!(other["messages"][1]["content"][0]["type"], "tool_use");
        assert!(other.get("thinking").is_none(), "a tool round without its thinking block goes without thinking");

        // sessions from before the model was recorded keep today's behaviour
        let mut old = reasoning_history("x");
        for p in old[1].content.iter_mut() {
            if let Part::Reasoning { model, .. } | Part::RedactedReasoning { model, .. } = p {
                *model = None;
            }
        }
        let mut req = ModelRequest::new("model-b", "sys", old);
        req.reasoning = Some(ReasoningConfig::Budget(4000));
        let b = Anthropic::body(&cfg, &req);
        assert_eq!(b["messages"][1]["content"][0]["signature"], "S");
        assert_eq!(b["thinking"]["budget_tokens"], 4000);
    }

    #[test]
    fn a_picture_in_a_tool_result_is_an_image_block_inside_it() {
        let (m, _) = convert(
            &Message::tool_results(vec![Part::ToolResult {
                call_id: "c1".into(),
                name: Some("read".into()),
                content: vec![
                    Part::text("n.png: PNG image"),
                    Part::Image { source: ImageSource::Base64 { media_type: "image/png".into(), data: "AAAA".into() } },
                ],
                is_error: false,
            }]),
            "claude-x",
        );
        assert_eq!(
            m["content"][0]["content"],
            json!([
                {"type": "text", "text": "n.png: PNG image"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}
            ])
        );
    }

    fn modern_req(model: &str) -> ModelRequest {
        let mut req = ModelRequest::new(model, "sys", vec![Message::user("hi")]);
        req.tools =
            vec![ToolSpec { name: "read".into(), description: "d".into(), parameters: json!({"type": "object"}) }];
        req.max_tokens = 16_384;
        req.temperature = Some(0.0);
        req.extra.insert("top_p".into(), json!(0.9));
        req
    }

    #[test]
    fn the_5_5_models_are_told_apart_from_the_older_ones() {
        for m in [
            "claude-sonnet-5-5",
            "claude-opus-5-5",
            "claude-haiku-5-5",
            "anthropic.claude-sonnet-5-5-v1:0",
            "claude-haiku-5-5@20260101",
            "claude-sonnet-5.5",
            "claude-opus-6",
        ] {
            assert!(modern(m), "{m}");
        }
        for m in [
            "claude-sonnet-5",
            "claude-sonnet-5-20260101",
            "claude-opus-4-8",
            "claude-sonnet-4-5",
            "claude-haiku-4-5",
            "claude-3-5-sonnet-latest",
            "gpt-5-5",
            "claude-x",
        ] {
            assert!(!modern(m), "{m}");
        }
    }

    #[test]
    fn sonnet_5_5_gets_adaptive_thinking_effort_and_no_rejected_field() {
        let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
        for model in ["claude-sonnet-5-5", "claude-haiku-5-5", "claude-opus-5-5"] {
            let mut req = modern_req(model);
            let b = Anthropic::body(&cfg, &req);
            assert_eq!(b["thinking"]["type"], "adaptive", "{model}");
            assert_eq!(b["thinking"]["block_binding"]["prefix_mismatch_behavior"], "drop_block");
            assert_eq!(b["output_config"], json!({"effort": "medium"}), "default is medium");
            for k in ["temperature", "top_p", "top_k"] {
                assert!(b.get(k).is_none(), "{model} sends {k}");
            }
            assert!(!b.to_string().contains("budget_tokens") && !b.to_string().contains("\"enabled\""));
            assert_eq!(b["max_tokens"], 64_000, "a default-sized cap is raised so a big file write is not cut");
            assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
            assert_eq!(b["tools"][0]["cache_control"]["type"], "ephemeral");
            assert_eq!(b["messages"][0]["content"][0]["cache_control"]["type"], "ephemeral");
            for (e, want) in
                [(Effort::Minimal, "low"), (Effort::Low, "low"), (Effort::Medium, "medium"), (Effort::High, "high")]
            {
                req.reasoning = Some(ReasoningConfig::Effort(e));
                assert_eq!(Anthropic::body(&cfg, &req)["output_config"]["effort"], want);
            }
            req.reasoning = Some(ReasoningConfig::Budget(30_000));
            assert_eq!(Anthropic::body(&cfg, &req)["output_config"]["effort"], "high");
        }
    }

    #[test]
    fn a_5_5_reply_cap_is_raised_only_when_the_person_left_it_at_the_default_and_never_past_the_largest() {
        let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
        let mut req = modern_req("claude-sonnet-5-5");
        req.max_tokens = 8192;
        assert_eq!(Anthropic::body(&cfg, &req)["max_tokens"], 8192);
        req.max_tokens = 1024;
        assert_eq!(Anthropic::body(&cfg, &req)["max_tokens"], 4096, "thinking needs room");
        req.max_tokens = 500_000;
        assert_eq!(Anthropic::body(&cfg, &req)["max_tokens"], 128_000);
    }

    #[test]
    fn turning_reasoning_off_on_a_5_5_model_uses_what_it_accepts() {
        let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
        let mut req = modern_req("claude-sonnet-5-5");
        req.reasoning = Some(ReasoningConfig::Off);
        req.messages = vec![
            Message::user("hi"),
            Message::new(
                Role::Assistant,
                vec![
                    Part::Reasoning { text: "t".into(), signature: Some("sig".into()), encrypted: None, model: None },
                    Part::text("ok"),
                ],
            ),
            Message::user("again"),
        ];
        let b = Anthropic::body(&cfg, &req);
        assert_eq!(b["thinking"], json!({"type": "between_tools"}), "no other field is allowed with it");
        assert!(b.get("output_config").is_none());
        assert!(!b["messages"].to_string().contains("signature"), "no block replayed after an edit risk");
        req.model = "claude-opus-5-5".into();
        let b = Anthropic::body(&cfg, &req);
        assert!(b.get("thinking").is_none(), "Opus 5.5 always thinks and takes neither disabled nor between_tools");
    }

    #[test]
    fn thinking_blocks_of_a_5_5_conversation_go_back_with_their_signature() {
        let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
        let mut req = modern_req("claude-sonnet-5-5");
        req.messages = vec![
            Message::user("hi"),
            Message::new(
                Role::Assistant,
                vec![
                    Part::Reasoning {
                        text: String::new(),
                        signature: Some("sig".into()),
                        encrypted: None,
                        model: None,
                    },
                    Part::text("ok"),
                ],
            ),
            Message::user("again"),
        ];
        let b = Anthropic::body(&cfg, &req);
        assert_eq!(b["messages"][1]["content"][0], json!({"type": "thinking", "thinking": "", "signature": "sig"}));
    }

    #[test]
    fn the_binding_control_can_be_turned_off_and_older_models_keep_their_fields() {
        let mut cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
        cfg.extra.insert("block_binding".into(), toml::Value::Boolean(false));
        let b = Anthropic::body(&cfg, &modern_req("claude-sonnet-5-5"));
        assert_eq!(b["thinking"], json!({"type": "adaptive"}));
        let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
        let mut req = modern_req("claude-sonnet-4-5");
        let b = Anthropic::body(&cfg, &req);
        assert_eq!(b["temperature"], 0.0);
        assert_eq!(b["max_tokens"], 16_384);
        assert!(b.get("thinking").is_none() && b.get("output_config").is_none());
        req.reasoning = Some(ReasoningConfig::Effort(Effort::Medium));
        let b = Anthropic::body(&cfg, &req);
        assert_eq!(b["thinking"], json!({"type": "enabled", "budget_tokens": 8192}));
        assert!(b.get("temperature").is_none());
    }

    #[test]
    fn usage_counts_cache_writes_and_reads_and_prices_them() {
        let mut p = MessagesParser::default();
        let mut evs = p.feed(r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":10,"cache_creation_input_tokens":2000,"cache_read_input_tokens":500}}}"#);
        evs.extend(
            p.feed(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":40}}"#),
        );
        let u = evs.iter().find_map(|e| if let StreamEvent::Usage(u) = e { Some(*u) } else { None }).unwrap();
        assert_eq!((u.input, u.output, u.cache_read, u.cache_write), (10, 40, 500, 2000));
        let price = nexo_core::Pricing { input: 2.0, output: 10.0, cache_read: 0.10, cache_write: 2.50 };
        let want = (10.0 * 2.0 + 40.0 * 10.0 + 500.0 * 0.10 + 2000.0 * 2.50) / 1e6;
        assert!((price.cost(&u) - want).abs() < 1e-12);
    }
}
