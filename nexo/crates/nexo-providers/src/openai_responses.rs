//! OpenAI responses: `POST {base}/responses`, an `input` list of typed
//! items, SSE of typed events. Preferred by OpenAI and xAI for their
//! reasoning models; reasoning comes back as opaque items that must be
//! echoed on the next request.

use async_trait::async_trait;
use nexo_core::{
    ErrorKind, EventStream, FinishReason, Message, ModelInfo, ModelRequest, Part, Protocol, ProviderConfig,
    ProviderError, ReasoningConfig, Role, StreamEvent, Usage,
};
use serde_json::{json, Value};

use crate::http;
use crate::openai_chat::{image_url, parse_args, temp, OpenAiChat};
use crate::parse::{stream_error_kind, Feed, Progress};
use crate::sse::SseEvent;

#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiResponses;

pub const ID: &str = "openai-responses";

impl OpenAiResponses {
    pub fn url(cfg: &ProviderConfig) -> String {
        http::openai_url(cfg, "responses")
    }

    pub fn body(cfg: &ProviderConfig, req: &ModelRequest) -> Value {
        let mut input = Vec::new();
        for m in &req.messages {
            input.extend(convert(m, &req.model));
        }
        let mut body = json!({"model": req.model, "input": input, "stream": true});
        // a subscription backend (ChatGPT's) sets the limit itself, rejects a sampling temperature and
        // wants the instructions even when empty
        let subscription = cfg.extra.get("omit_max_tokens").and_then(|v| v.as_bool()).unwrap_or(false);
        if !subscription {
            body["max_output_tokens"] = json!(req.max_tokens);
        }
        body["store"] = json!(cfg.extra.get("store").and_then(|v| v.as_bool()).unwrap_or(false));
        if subscription || !req.system.is_empty() {
            body["instructions"] = json!(req.system);
        }
        let fixed = crate::openai_chat::default_temperature_only(cfg, &req.model);
        if let (Some(t), false, false) = (req.temperature, subscription, fixed) {
            body["temperature"] = temp(t);
        }
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    // `strict: false`: left out, the API takes the schema as strict where it can, and then the
                    // model writes every optional argument on every call (`"wait_until": "", "detach": false`).
                    .map(|t| json!({"type": "function", "name": t.name, "description": t.description, "strict": false, "parameters": t.parameters}))
                    .collect(),
            );
            if req.no_tool_call {
                body["tool_choice"] = json!("none");
            }
        }
        let mut include = Vec::new();
        match req.reasoning {
            Some(ReasoningConfig::Effort(e)) => {
                let effort = e.as_str();
                body["reasoning"] = json!({"effort": effort, "summary": "auto"});
                include.push("reasoning.encrypted_content");
            }
            Some(ReasoningConfig::Budget(_)) => {
                body["reasoning"] = json!({"effort": "medium", "summary": "auto"});
                include.push("reasoning.encrypted_content");
            }
            _ => {}
        }
        if cfg.extra.get("include_reasoning").and_then(|v| v.as_bool()).unwrap_or(false) && include.is_empty() {
            include.push("reasoning.encrypted_content");
        }
        if !include.is_empty() {
            body["include"] = json!(include);
        }
        if let Some(id) = req.session_id.as_deref().filter(|_| cache_key_allowed(cfg)) {
            body["prompt_cache_key"] = json!(id);
        }
        for (k, v) in &req.extra {
            body[k] = v.clone();
        }
        body
    }

    /// The request headers: the protocol's, and on a ChatGPT login the ones Codex sends to tie the
    /// request to its conversation (`session-id`, `thread-id`, `x-client-request-id`), which that
    /// backend routes its cache by. A header set in the provider's config wins.
    pub fn request_headers(cfg: &ProviderConfig, key: Option<&str>, req: &ModelRequest) -> reqwest::header::HeaderMap {
        let mut h = OpenAiChat::headers(cfg, key);
        if let (Some(id), Some("codex")) = (req.session_id.as_deref(), cfg.login.as_deref()) {
            if let Ok(v) = reqwest::header::HeaderValue::from_str(id) {
                for name in ["session-id", "thread-id", "x-client-request-id"] {
                    if !h.contains_key(name) {
                        h.insert(reqwest::header::HeaderName::from_static(name), v.clone());
                    }
                }
            }
        }
        h
    }

    pub fn parse(events: impl futures::Stream<Item = Result<SseEvent, String>> + Send + 'static) -> EventStream {
        crate::parse::run(events, ResponsesParser::default())
    }
}

/// On by default only where the field is known to be accepted (OpenAI itself and a ChatGPT login);
/// `extra.prompt_cache_key` turns it on or off for any other server.
fn cache_key_allowed(cfg: &ProviderConfig) -> bool {
    let known = cfg.login.as_deref() == Some("codex") || crate::openai_chat::is_openai(cfg);
    cfg.extra.get("prompt_cache_key").and_then(|v| v.as_bool()).unwrap_or(known)
}

/// A message as input items.
fn convert(m: &Message, model: &str) -> Vec<Value> {
    let mut out = Vec::new();
    match m.role {
        Role::User => {
            let mut content = Vec::new();
            for p in &m.content {
                match p {
                    Part::ToolResult { call_id, content: c, is_error, .. } => {
                        let text = nexo_core::message::result_text(c);
                        let text = if *is_error && !text.to_ascii_lowercase().starts_with("error") {
                            format!("Error: {text}")
                        } else {
                            text
                        };
                        // with pictures the output is a list of content items, else a string
                        let output = if c.iter().any(|p| matches!(p, Part::Image { .. })) {
                            let mut items = vec![json!({"type": "input_text", "text": text})];
                            for p in c {
                                if let Part::Image { source } = p {
                                    items.push(json!({"type": "input_image", "image_url": image_url(source)}));
                                }
                            }
                            Value::Array(items)
                        } else {
                            Value::String(text)
                        };
                        out.push(json!({"type": "function_call_output", "call_id": call_id, "output": output}));
                    }
                    Part::Text { text } => content.push(json!({"type": "input_text", "text": text})),
                    Part::Image { source } => {
                        content.push(json!({"type": "input_image", "image_url": image_url(source)}))
                    }
                    _ => {}
                }
            }
            if !content.is_empty() {
                out.push(json!({"type": "message", "role": "user", "content": content}));
            }
        }
        Role::Assistant => {
            for p in &m.content {
                match p {
                    // opaque items of another model are not echoed: this one cannot decrypt them
                    p if p.is_foreign_reasoning(model) => {}
                    // only the opaque item is echoed; its text is for the person
                    Part::Reasoning { signature, encrypted: Some(enc), .. } => {
                        let mut item = json!({"type": "reasoning", "encrypted_content": enc, "summary": []});
                        if let Some(id) = signature {
                            item["id"] = json!(id);
                        }
                        out.push(item);
                    }
                    Part::Text { text } if !text.is_empty() => {
                        out.push(json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}));
                    }
                    Part::ToolCall { id, name, args } => {
                        out.push(json!({"type": "function_call", "call_id": id, "name": name, "arguments": args.to_string()}));
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

#[derive(Debug, Default)]
pub struct ResponsesParser {
    /// Open function calls by output index: (call_id, name, fragments).
    calls: Vec<(u64, String, String, String)>,
    /// Reasoning items by output index: their ids.
    reasoning: Vec<(u64, String)>,
    prog: Progress,
    start_sent: bool,
}

impl Feed for ResponsesParser {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        let v: Value = match serde_json::from_str(data.trim()) {
            Ok(v) => v,
            Err(_) => return out,
        };
        let t = v["type"].as_str().unwrap_or("");
        if !t.is_empty() {
            self.prog.started = true;
        }
        if t.is_empty() {
            // a plain error object, sent without event framing by some gateways
            if let Some(e) = v.get("error").filter(|e| e.is_object()) {
                let msg = http::pretty_error(&v.to_string());
                let kind = stream_error_kind(e["type"].as_str().or_else(|| e["code"].as_str()).unwrap_or(""), &msg);
                out.push(StreamEvent::Error(ProviderError::new(kind, msg)));
                self.calls.clear(); // calls cut by the error are dropped, not closed
                self.prog.started = true;
                self.prog.finished = true;
                return out;
            }
        }
        match t {
            "codex.rate_limits" => crate::usage::record_event(&v),
            "response.created" | "response.in_progress" => {
                if !self.start_sent {
                    self.start_sent = true;
                    out.push(StreamEvent::Start {
                        response_id: v["response"]["id"].as_str().map(String::from),
                        model: v["response"]["model"].as_str().map(String::from),
                    });
                }
            }
            // a refusal is the model's answer, and the person reads it
            "response.output_text.delta" | "response.refusal.delta" => {
                if let Some(d) = v["delta"].as_str() {
                    self.prog.content = true;
                    out.push(StreamEvent::TextDelta(d.to_string()));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(d) = v["delta"].as_str() {
                    self.prog.content = true;
                    out.push(StreamEvent::ReasoningDelta(d.to_string()));
                }
            }
            "response.reasoning_summary_text.done" | "response.reasoning_summary_part.done" => {
                // a paragraph break between summary parts
                out.push(StreamEvent::ReasoningDelta("\n".into()));
            }
            "response.output_item.added" => {
                let item = &v["item"];
                let idx = v["output_index"].as_u64().unwrap_or(0);
                match item["type"].as_str() {
                    Some("function_call") => {
                        let id = item["call_id"].as_str().unwrap_or("").to_string();
                        let name = item["name"].as_str().unwrap_or("").to_string();
                        self.prog.saw_tool = true;
                        self.prog.content = true;
                        self.calls.push((idx, id.clone(), name.clone(), String::new()));
                        out.push(StreamEvent::ToolCallStart { id, name });
                    }
                    Some("reasoning") => {
                        if let Some(id) = item["id"].as_str() {
                            self.reasoning.push((idx, id.to_string()));
                        }
                    }
                    _ => {}
                }
            }
            "response.function_call_arguments.delta" => {
                let idx = v["output_index"].as_u64().unwrap_or(0);
                if let (Some(d), Some(c)) = (v["delta"].as_str(), self.calls.iter_mut().find(|c| c.0 == idx)) {
                    c.3.push_str(d);
                    out.push(StreamEvent::ToolCallDelta { id: c.1.clone(), partial_json: d.to_string() });
                }
            }
            "response.function_call_arguments.done" => {
                let idx = v["output_index"].as_u64().unwrap_or(0);
                if let Some(pos) = self.calls.iter().position(|c| c.0 == idx) {
                    let (_, id, name, frags) = self.calls.remove(pos);
                    let args = v["arguments"].as_str().map(String::from).unwrap_or(frags);
                    out.push(StreamEvent::ToolCallEnd { id, name, args: parse_args(&args) });
                }
            }
            "response.output_item.done" => {
                let item = &v["item"];
                let idx = v["output_index"].as_u64().unwrap_or(0);
                match item["type"].as_str() {
                    Some("reasoning") => {
                        let id = item["id"]
                            .as_str()
                            .map(String::from)
                            .or_else(|| self.reasoning.iter().find(|r| r.0 == idx).map(|r| r.1.clone()));
                        let enc = item["encrypted_content"].as_str().map(String::from);
                        if id.is_some() || enc.is_some() {
                            out.push(StreamEvent::ReasoningMeta { signature: id, encrypted: enc });
                        }
                    }
                    Some("function_call") => {
                        // a call whose arguments.done never came
                        if let Some(pos) = self.calls.iter().position(|c| c.0 == idx) {
                            let (_, id, name, frags) = self.calls.remove(pos);
                            let args = item["arguments"].as_str().map(String::from).unwrap_or(frags);
                            out.push(StreamEvent::ToolCallEnd { id, name, args: parse_args(&args) });
                        }
                    }
                    _ => {}
                }
            }
            "response.completed" | "response.incomplete" | "response.failed" => {
                if t == "response.failed" {
                    self.calls.clear(); // calls cut by the failure are dropped, not closed
                } else {
                    out.extend(self.end_calls());
                }
                let r = &v["response"];
                if let Some(u) = r.get("usage").filter(|u| u.is_object()) {
                    out.push(StreamEvent::Usage(usage(u)));
                }
                if !self.prog.finished {
                    self.prog.finished = true;
                    if t == "response.failed" {
                        let msg = r["error"]["message"].as_str().unwrap_or("response failed");
                        let kind = stream_error_kind(r["error"]["code"].as_str().unwrap_or(""), msg);
                        out.push(StreamEvent::Error(ProviderError::new(kind, msg)));
                    } else {
                        let reason = r["incomplete_details"]["reason"].as_str().unwrap_or("");
                        out.push(StreamEvent::Finish(match reason {
                            "max_output_tokens" => FinishReason::Length,
                            "content_filter" => FinishReason::ContentFilter,
                            _ if self.prog.saw_tool => FinishReason::ToolUse,
                            _ => FinishReason::Stop,
                        }));
                    }
                }
            }
            "error" => {
                let msg = v["message"].as_str().or_else(|| v["error"]["message"].as_str()).unwrap_or("error");
                let kind =
                    stream_error_kind(v["code"].as_str().or_else(|| v["error"]["type"].as_str()).unwrap_or(""), msg);
                out.push(StreamEvent::Error(ProviderError::new(kind, msg)));
                self.calls.clear(); // calls cut by the error are dropped, not closed
                self.prog.finished = true;
            }
            _ => {}
        }
        out
    }

    fn fail(&mut self, e: String) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        self.calls.clear(); // calls cut by the transport are dropped, not closed
        if !self.prog.finished {
            self.prog.finished = true;
            out.push(StreamEvent::Error(ProviderError::new(ErrorKind::Network, format!("stream cut: {e}"))));
        }
        out
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        // calls still open without a completed response: their arguments may be cut
        let open = !self.calls.is_empty();
        self.calls.clear();
        self.prog.at_end_with_calls(open).into_iter().collect()
    }
}

impl ResponsesParser {
    fn end_calls(&mut self) -> Vec<StreamEvent> {
        std::mem::take(&mut self.calls)
            .into_iter()
            .map(|(_, id, name, frags)| StreamEvent::ToolCallEnd { id, name, args: parse_args(&frags) })
            .collect()
    }
}

pub fn usage(u: &Value) -> Usage {
    let count = |v: &Value| crate::openai_chat::count(v).unwrap_or(0);
    let input = count(&u["input_tokens"]);
    let cached = count(&u["input_tokens_details"]["cached_tokens"]);
    Usage {
        input: input.saturating_sub(cached),
        output: count(&u["output_tokens"]),
        cache_read: cached,
        cache_write: 0,
        reasoning: count(&u["output_tokens_details"]["reasoning_tokens"]),
    }
}

#[async_trait]
impl Protocol for OpenAiResponses {
    fn id(&self) -> &'static str {
        ID
    }

    async fn stream(
        &self,
        cfg: &ProviderConfig,
        key: Option<&str>,
        req: &ModelRequest,
    ) -> Result<EventStream, ProviderError> {
        let body =
            http::post_stream(&Self::url(cfg), Self::request_headers(cfg, key, req), &Self::body(cfg, req)).await?;
        Ok(Self::parse(http::sse(body)))
    }

    async fn list_models(&self, cfg: &ProviderConfig, key: Option<&str>) -> Result<Vec<ModelInfo>, ProviderError> {
        OpenAiChat.list_models(cfg, key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALL_OPEN: [&str; 3] = [
        r#"{"type":"response.created","response":{"id":"r"}}"#,
        r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"read"}}"#,
        r#"{"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"pa"}"#,
    ];

    #[test]
    fn an_error_drops_open_tool_calls_instead_of_closing_them() {
        for err in [
            r#"{"type":"error","code":"server_error","message":"boom"}"#,
            r#"{"type":"response.failed","response":{"id":"r","error":{"code":"server_error","message":"boom"}}}"#,
        ] {
            let mut p = ResponsesParser::default();
            let mut evs: Vec<StreamEvent> = CALL_OPEN.iter().flat_map(|d| p.feed(d)).collect();
            evs.extend(p.feed(err));
            evs.extend(p.finish());
            assert!(!evs.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { .. })), "{evs:?}");
            assert!(matches!(evs.last(), Some(StreamEvent::Error(_))), "{evs:?}");
        }
    }

    #[test]
    fn a_clean_end_inside_a_tool_call_is_a_truncation_error() {
        let mut p = ResponsesParser::default();
        let mut evs: Vec<StreamEvent> = CALL_OPEN.iter().flat_map(|d| p.feed(d)).collect();
        evs.extend(p.finish());
        assert!(!evs.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { .. } | StreamEvent::Finish(_))), "{evs:?}");
        assert!(matches!(evs.last(), Some(StreamEvent::Error(e)) if e.message.contains("truncated")), "{evs:?}");
    }

    #[test]
    fn a_mid_stream_context_error_without_a_number_is_the_context_kind() {
        let mut p = ResponsesParser::default();
        let evs = p.feed(r#"{"type":"response.failed","response":{"id":"r","error":{"code":"context_length_exceeded","message":"Your input exceeds the context window of this model."}}}"#);
        assert!(matches!(&evs[0], StreamEvent::Error(e) if e.kind == ErrorKind::ContextLength), "{evs:?}");
    }

    #[test]
    fn a_failed_response_and_a_cut_stream_are_errors() {
        let mut p = ResponsesParser::default();
        let evs = p.feed(r#"{"type":"response.failed","response":{"id":"r","status":"failed","error":{"code":"rate_limit_exceeded","message":"slow down"}}}"#);
        assert!(matches!(&evs[0], StreamEvent::Error(e) if e.kind == ErrorKind::RateLimit), "{evs:?}");
        let mut p = ResponsesParser::default();
        p.feed(r#"{"type":"response.created","response":{"id":"r"}}"#);
        assert!(matches!(&p.finish()[0], StreamEvent::Error(e) if e.kind == ErrorKind::Network));
        let mut p = ResponsesParser::default();
        p.feed(r#"{"type":"response.created","response":{"id":"r"}}"#);
        p.feed(r#"{"type":"response.output_text.delta","delta":"hi"}"#);
        assert_eq!(p.finish(), vec![StreamEvent::Finish(FinishReason::Stop)]);
    }

    #[test]
    fn body_uses_items_and_echoes_only_opaque_reasoning() {
        let mut req = ModelRequest::new(
            "gpt-5",
            "sys",
            vec![
                Message::user("hi"),
                Message::new(
                    Role::Assistant,
                    vec![
                        Part::Reasoning {
                            text: "visible".into(),
                            signature: Some("rs_1".into()),
                            encrypted: Some("ENC".into()),
                            model: None,
                        },
                        Part::tool_call("call_1", "read", json!({"path": "a"})),
                    ],
                ),
                Message::tool_results(vec![Part::tool_result("call_1", "text", false)]),
            ],
        );
        req.reasoning = Some(ReasoningConfig::Effort(nexo_core::Effort::High));
        let b = OpenAiResponses::body(&ProviderConfig::new(ID, "https://api.openai.com/v1", None), &req);
        assert_eq!(b["instructions"], "sys");
        assert_eq!(b["store"], false);
        assert_eq!(b["input"][0]["type"], "message");
        assert_eq!(b["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(
            b["input"][1],
            json!({"type": "reasoning", "id": "rs_1", "encrypted_content": "ENC", "summary": []})
        );
        assert_eq!(b["input"][2]["type"], "function_call");
        assert_eq!(b["input"][2]["call_id"], "call_1");
        assert_eq!(b["input"][3], json!({"type": "function_call_output", "call_id": "call_1", "output": "text"}));
        assert_eq!(b["reasoning"]["effort"], "high");
        assert_eq!(b["include"][0], "reasoning.encrypted_content");
        assert_eq!(b["max_output_tokens"], 8192);
    }

    fn with_session(id: &str) -> ModelRequest {
        let mut req = ModelRequest::new("gpt-5", "sys", vec![Message::user("hi")]);
        req.session_id = Some(id.into());
        req
    }

    #[test]
    fn the_cache_key_is_the_session_id_stable_within_a_session_and_apart_between_sessions() {
        let cfg = ProviderConfig::new(ID, "https://api.openai.com/v1", None);
        let mut a = with_session("20261107-101500-aaaa");
        let first = OpenAiResponses::body(&cfg, &a);
        a.messages.push(Message::user("and then"));
        let second = OpenAiResponses::body(&cfg, &a);
        assert_eq!(first["prompt_cache_key"], "20261107-101500-aaaa");
        assert_eq!(first["prompt_cache_key"], second["prompt_cache_key"]);
        let other = OpenAiResponses::body(&cfg, &with_session("20261107-101500-bbbb"));
        assert_ne!(first["prompt_cache_key"], other["prompt_cache_key"]);
        let none = OpenAiResponses::body(&cfg, &ModelRequest::new("gpt-5", "sys", vec![]));
        assert!(none.get("prompt_cache_key").is_none());
        let mut off = cfg.clone();
        off.extra.insert("prompt_cache_key".into(), toml::Value::Boolean(false));
        assert!(OpenAiResponses::body(&off, &a).get("prompt_cache_key").is_none());
        // a third-party server on this protocol gets the field only when its config asks for it
        let mut router = ProviderConfig::new(ID, "https://router.example.com/v1", None);
        assert!(OpenAiResponses::body(&router, &a).get("prompt_cache_key").is_none());
        router.extra.insert("prompt_cache_key".into(), toml::Value::Boolean(true));
        assert_eq!(OpenAiResponses::body(&router, &a)["prompt_cache_key"], "20261107-101500-aaaa");
    }

    #[test]
    fn a_chatgpt_login_sends_the_session_headers_codex_sends() {
        let mut cfg = ProviderConfig::new(ID, "https://chatgpt.com/backend-api/codex", None);
        cfg.login = Some("codex".into());
        let h = OpenAiResponses::request_headers(&cfg, Some("tok"), &with_session("s-1"));
        for name in ["session-id", "thread-id", "x-client-request-id"] {
            assert_eq!(h[name], "s-1", "{name}");
        }
        let h2 = OpenAiResponses::request_headers(&cfg, Some("tok"), &with_session("s-2"));
        assert_ne!(h["session-id"], h2["session-id"]);
        // a config header wins, and a provider that is not a ChatGPT login gets none
        cfg.headers.insert("session-id".into(), "mine".into());
        assert_eq!(OpenAiResponses::request_headers(&cfg, None, &with_session("s-1"))["session-id"], "mine");
        let plain = ProviderConfig::new(ID, "https://api.openai.com/v1", None);
        assert!(OpenAiResponses::request_headers(&plain, None, &with_session("s-1")).get("session-id").is_none());
    }

    #[test]
    fn parses_typed_events() {
        let data = [
            r#"{"type":"response.created","response":{"id":"resp_1","model":"gpt-5"}}"#,
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1"}}"#,
            r#"{"type":"response.reasoning_summary_text.delta","delta":"hm"}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_1","encrypted_content":"ENC"}}"#,
            r#"{"type":"response.output_text.delta","delta":"Hello"}"#,
            r#"{"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","call_id":"call_9","name":"bash"}}"#,
            r#"{"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"command\":"}"#,
            r#"{"type":"response.function_call_arguments.delta","output_index":2,"delta":"\"ls\"}"}"#,
            r#"{"type":"response.function_call_arguments.done","output_index":2,"arguments":"{\"command\":\"ls\"}"}"#,
            r#"{"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":50,"output_tokens":9,"input_tokens_details":{"cached_tokens":20},"output_tokens_details":{"reasoning_tokens":4}}}}"#,
        ];
        let mut p = ResponsesParser::default();
        let evs: Vec<StreamEvent> = data.iter().flat_map(|d| p.feed(d)).collect();
        assert_eq!(evs[0], StreamEvent::Start { response_id: Some("resp_1".into()), model: Some("gpt-5".into()) });
        assert_eq!(evs[1], StreamEvent::ReasoningDelta("hm".into()));
        assert_eq!(
            evs[2],
            StreamEvent::ReasoningMeta { signature: Some("rs_1".into()), encrypted: Some("ENC".into()) }
        );
        assert_eq!(evs[3], StreamEvent::TextDelta("Hello".into()));
        assert_eq!(evs[4], StreamEvent::ToolCallStart { id: "call_9".into(), name: "bash".into() });
        assert_eq!(
            evs[7],
            StreamEvent::ToolCallEnd { id: "call_9".into(), name: "bash".into(), args: json!({"command": "ls"}) }
        );
        assert_eq!(
            evs[8],
            StreamEvent::Usage(Usage { input: 30, output: 9, cache_read: 20, cache_write: 0, reasoning: 4 })
        );
        assert_eq!(evs[9], StreamEvent::Finish(FinishReason::ToolUse));
    }

    #[test]
    fn encrypted_reasoning_of_another_model_is_not_echoed() {
        let history = |made_by: &str| {
            vec![
                Message::user("hi"),
                Message::new(
                    Role::Assistant,
                    vec![
                        Part::Reasoning {
                            text: "v".into(),
                            signature: Some("rs_1".into()),
                            encrypted: Some("ENC".into()),
                            model: Some(made_by.into()),
                        },
                        Part::text("answer"),
                    ],
                ),
            ]
        };
        let cfg = ProviderConfig::new(ID, "https://api.openai.com/v1", None);
        let same = OpenAiResponses::body(&cfg, &ModelRequest::new("gpt-a", "", history("gpt-a")));
        assert!(same["input"].to_string().contains("encrypted_content"));
        let other = OpenAiResponses::body(&cfg, &ModelRequest::new("gpt-b", "", history("gpt-a")));
        let text = other["input"].to_string();
        assert!(!text.contains("encrypted_content") && !text.contains("rs_1"), "{text}");
        assert!(text.contains("answer"));
    }

    #[test]
    fn a_picture_in_a_tool_result_is_an_input_image_in_its_output() {
        let req = ModelRequest::new(
            "gpt-5",
            "sys",
            vec![
                Message::new(Role::Assistant, vec![Part::tool_call("c1", "read", json!({"path": "n.png"}))]),
                Message::tool_results(vec![Part::ToolResult {
                    call_id: "c1".into(),
                    name: Some("read".into()),
                    content: vec![
                        Part::text("n.png: PNG image"),
                        Part::Image {
                            source: nexo_core::message::ImageSource::Base64 {
                                media_type: "image/png".into(),
                                data: "AAAA".into(),
                            },
                        },
                    ],
                    is_error: false,
                }]),
            ],
        );
        let b = OpenAiResponses::body(&ProviderConfig::new(ID, "https://api.openai.com/v1", None), &req);
        assert_eq!(
            b["input"][1],
            json!({"type": "function_call_output", "call_id": "c1", "output": [
                {"type": "input_text", "text": "n.png: PNG image"},
                {"type": "input_image", "image_url": "data:image/png;base64,AAAA"}
            ]})
        );
    }
}
