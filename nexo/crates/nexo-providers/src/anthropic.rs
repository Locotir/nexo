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
        if let Some(b) = budget.filter(|_| !broken_round) {
            let b = b.min(req.max_tokens.saturating_sub(1).max(1024));
            body["thinking"] = json!({"type": "enabled", "budget_tokens": b});
            if body["temperature"].is_number() {
                body.as_object_mut().map(|o| o.remove("temperature")); // not allowed with thinking
            }
        }
        for (k, v) in &req.extra {
            body[k] = v.clone();
        }
        body
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
                if !text.is_empty() {
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
                let text = nexo_core::message::result_text(c);
                let mut r = json!({"type": "tool_result", "tool_use_id": call_id, "content": text});
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
                            "max_tokens" => FinishReason::Length,
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
                self.prog.finished = true;
            }
            _ => {} // ping
        }
        out
    }

    fn fail(&mut self, e: String) -> Vec<StreamEvent> {
        let mut out = self.end_calls();
        if !self.prog.finished {
            self.prog.finished = true;
            out.push(StreamEvent::Error(ProviderError::new(ErrorKind::Network, format!("stream cut: {e}"))));
        }
        out
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = self.end_calls();
        if !self.prog.finished && self.prog.content && !self.usage_sent && !self.usage.is_empty() {
            self.usage_sent = true;
            out.push(StreamEvent::Usage(self.usage));
        }
        out.extend(self.prog.at_end());
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
        let body = http::post_stream(&Self::url(cfg), Self::headers(cfg, key), &Self::body(cfg, req)).await?;
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
}
