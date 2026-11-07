//! OpenAI chat completions: the protocol most vendors serve
//! (`POST {base}/chat/completions`, SSE of `choices[0].delta` chunks).

use async_trait::async_trait;
use nexo_core::{
    ErrorKind, EventStream, FinishReason, ImageSource, Message, ModelInfo, ModelRequest, Part, Protocol,
    ProviderConfig, ProviderError, ReasoningConfig, Role, StreamEvent, Usage,
};
use serde_json::{json, Value};

use crate::http;
use crate::parse::{stream_error_kind, Feed, Progress};
use crate::sse::SseEvent;

#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiChat;

pub const ID: &str = "openai-chat";

impl OpenAiChat {
    pub fn url(cfg: &ProviderConfig) -> String {
        http::openai_url(cfg, "chat/completions")
    }

    pub fn headers(cfg: &ProviderConfig, key: Option<&str>) -> reqwest::header::HeaderMap {
        let mut own =
            vec![("content-type", "application/json".to_string()), ("accept", "text/event-stream".to_string())];
        if let Some(k) = key {
            own.push(("authorization", format!("Bearer {k}")));
        }
        http::headers(cfg, &own)
    }

    /// The request body, exactly as the protocol wants it.
    pub fn body(cfg: &ProviderConfig, req: &ModelRequest) -> Value {
        let mut messages = Vec::new();
        if !req.system.is_empty() {
            let role = cfg.extra.get("system_role").and_then(|v| v.as_str()).unwrap_or("system");
            messages.push(json!({"role": role, "content": req.system}));
        }
        let send_reasoning = cfg.extra.get("send_reasoning").and_then(|v| v.as_bool()).unwrap_or(false);
        for m in &req.messages {
            messages.extend(convert(m, send_reasoning));
        }
        let mut body = json!({"model": req.model, "messages": messages, "stream": true, "stream_options": {"include_usage": true}});
        let max_key = cfg.extra.get("max_tokens_key").and_then(|v| v.as_str()).map(String::from).unwrap_or_else(|| {
            if needs_completion_key(&req.model) {
                "max_completion_tokens".into()
            } else {
                "max_tokens".into()
            }
        });
        body[max_key] = json!(req.max_tokens);
        if let Some(t) = req.temperature {
            body["temperature"] = temp(t);
        }
        if !req.stop.is_empty() {
            body["stop"] = json!(req.stop);
        }
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.parameters}}))
                    .collect(),
            );
            if req.no_tool_call {
                body["tool_choice"] = json!("none");
            }
        }
        match req.reasoning {
            Some(ReasoningConfig::Effort(e)) => {
                body["reasoning_effort"] = json!(e.as_str());
            }
            Some(ReasoningConfig::Budget(_)) | Some(ReasoningConfig::Off) | None => {}
        }
        // OpenAI's own endpoint routes its prompt cache by this; other vendors may reject the field
        if let Some(id) = req.session_id.as_deref().filter(|_| is_openai(cfg)) {
            body["prompt_cache_key"] = json!(id);
        }
        for (k, v) in &req.extra {
            body[k] = v.clone();
        }
        body
    }

    /// Parses the events of one response.
    pub fn parse(events: impl futures::Stream<Item = Result<SseEvent, String>> + Send + 'static) -> EventStream {
        crate::parse::run(events, ChatParser::default())
    }
}

/// Whether the provider is OpenAI itself (`api.openai.com`).
pub(crate) fn is_openai(cfg: &ProviderConfig) -> bool {
    reqwest::Url::parse(&cfg.base_url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.eq_ignore_ascii_case("api.openai.com")))
        .unwrap_or(false)
}

/// Newer OpenAI models reject `max_tokens` (`o*`, `gpt-5*`).
fn needs_completion_key(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    (m.starts_with('o') && m[1..].starts_with(|c: char| c.is_ascii_digit())) || m.starts_with("gpt-5")
}

/// One of ours as the protocol's messages (a tool-results message becomes
/// one `tool` message per result).
fn convert(m: &Message, send_reasoning: bool) -> Vec<Value> {
    match m.role {
        Role::User => {
            let mut out = Vec::new();
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
                        out.push(json!({"role": "tool", "tool_call_id": call_id, "content": text}));
                        // a tool message takes text only: its pictures follow in the user message
                        for img in c.iter().filter_map(|p| match p {
                            Part::Image { source } => Some(source),
                            _ => None,
                        }) {
                            content.push(json!({"type": "image_url", "image_url": {"url": image_url(img)}}));
                        }
                    }
                    Part::Text { text } => content.push(json!({"type": "text", "text": text})),
                    Part::Image { source } => {
                        content.push(json!({"type": "image_url", "image_url": {"url": image_url(source)}}))
                    }
                    _ => {}
                }
            }
            if !content.is_empty() {
                let only_text = content.iter().all(|c| c["type"] == "text");
                let content = if only_text {
                    Value::String(content.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n\n"))
                } else {
                    Value::Array(content)
                };
                out.push(json!({"role": "user", "content": content}));
            }
            out
        }
        Role::Assistant => {
            let mut msg = json!({"role": "assistant"});
            let text = m.text();
            let calls: Vec<Value> = m
                .tool_calls()
                .map(|(id, name, args)| json!({"id": id, "type": "function", "function": {"name": name, "arguments": args.to_string()}}))
                .collect();
            // null content is only valid next to tool calls; a message with neither sends ""
            msg["content"] = if !text.is_empty() {
                Value::String(text)
            } else if calls.is_empty() {
                Value::String(String::new())
            } else {
                Value::Null
            };
            if !calls.is_empty() {
                msg["tool_calls"] = Value::Array(calls);
            }
            if send_reasoning {
                let r = m.reasoning();
                if !r.is_empty() {
                    msg["reasoning_content"] = Value::String(r);
                }
            }
            vec![msg]
        }
    }
}

pub(crate) fn image_url(source: &ImageSource) -> String {
    match source {
        ImageSource::Base64 { media_type, data } => format!("data:{media_type};base64,{data}"),
        ImageSource::Url { url } => url.clone(),
    }
}

/// Folds chat-completion chunks into events.
#[derive(Debug, Default)]
pub struct ChatParser {
    /// Open tool calls by chunk index: (index, id, name, argument fragments).
    calls: Vec<(usize, String, String, String)>,
    prog: Progress,
    /// The latest usage: servers that send running counts end with the final ones.
    usage: Option<Usage>,
}

impl Feed for ChatParser {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        let data = data.trim();
        if data == "[DONE]" {
            out.extend(self.close());
            return out;
        }
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return out, // keep-alive noise some gateways send
        };
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            let msg = http::pretty_error(&v.to_string());
            let kind = stream_error_kind(e["type"].as_str().or_else(|| e["code"].as_str()).unwrap_or(""), &msg);
            out.push(StreamEvent::Error(ProviderError::new(kind, msg)));
            self.calls.clear(); // calls cut by the error are dropped, not closed
            self.prog.finished = true;
            return out;
        }
        if !self.prog.started {
            self.prog.started = true;
            out.push(StreamEvent::Start {
                response_id: v["id"].as_str().map(String::from),
                model: v["model"].as_str().map(String::from),
            });
        }
        if let Some(choice) = v["choices"].as_array().and_then(|c| c.first()) {
            let delta = &choice["delta"];
            if let Some(r) = delta["reasoning_content"].as_str().or_else(|| delta["reasoning"].as_str()) {
                if !r.is_empty() {
                    self.prog.content = true;
                    out.push(StreamEvent::ReasoningDelta(r.to_string()));
                }
            }
            match &delta["content"] {
                Value::String(t) if !t.is_empty() => {
                    self.prog.content = true;
                    out.push(StreamEvent::TextDelta(t.clone()));
                }
                Value::Array(parts) => {
                    // Mistral-style typed parts
                    for p in parts {
                        match (p["type"].as_str(), p["text"].as_str()) {
                            (Some("thinking"), _) => {
                                if let Some(arr) = p["thinking"].as_array() {
                                    for t in arr {
                                        if let Some(s) = t["text"].as_str() {
                                            self.prog.content = true;
                                            out.push(StreamEvent::ReasoningDelta(s.to_string()));
                                        }
                                    }
                                }
                            }
                            (_, Some(t)) if !t.is_empty() => {
                                self.prog.content = true;
                                out.push(StreamEvent::TextDelta(t.to_string()));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            if let Some(calls) = delta["tool_calls"].as_array() {
                for c in calls {
                    let id = c["id"].as_str().unwrap_or("");
                    let name = c["function"]["name"].as_str().unwrap_or("");
                    let args = c["function"]["arguments"].as_str().unwrap_or("");
                    let idx = match c["index"].as_u64() {
                        Some(i) => i as usize,
                        // no index (some servers): a known id continues its call, a new id or
                        // a name starts one, a bare fragment goes to the last one
                        None if !id.is_empty() => self.calls.iter().position(|k| k.1 == id).unwrap_or(self.calls.len()),
                        None if !name.is_empty() => self.calls.len(),
                        None => self.calls.len().saturating_sub(1),
                    };
                    let known = self.calls.iter().position(|k| k.0 == idx);
                    match known {
                        None => {
                            let id = if id.is_empty() { format!("call_{idx}") } else { id.to_string() };
                            self.calls.push((idx, id.clone(), name.to_string(), String::new()));
                            self.prog.saw_tool = true;
                            self.prog.content = true;
                            out.push(StreamEvent::ToolCallStart { id: id.clone(), name: name.to_string() });
                            if !args.is_empty() {
                                if let Some(k) = self.calls.last_mut() {
                                    k.3.push_str(args);
                                }
                                out.push(StreamEvent::ToolCallDelta { id, partial_json: args.to_string() });
                            }
                        }
                        Some(i) => {
                            if !name.is_empty() && self.calls[i].2.is_empty() {
                                self.calls[i].2 = name.to_string();
                            }
                            if !args.is_empty() {
                                self.calls[i].3.push_str(args);
                                out.push(StreamEvent::ToolCallDelta {
                                    id: self.calls[i].1.clone(),
                                    partial_json: args.to_string(),
                                });
                            }
                        }
                    }
                }
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                out.extend(self.end_calls());
                if !self.prog.finished {
                    self.prog.finished = true;
                    out.push(StreamEvent::Finish(match reason {
                        "length" => FinishReason::Length,
                        "tool_calls" | "function_call" => FinishReason::ToolUse,
                        "content_filter" => FinishReason::ContentFilter,
                        _ if self.prog.saw_tool => FinishReason::ToolUse,
                        _ => FinishReason::Stop,
                    }));
                }
            }
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.usage = Some(usage(u));
        }
        out
    }

    /// The transport failed: what is open is closed and the error reported.
    fn fail(&mut self, e: String) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        self.calls.clear(); // calls cut by the transport are dropped, not closed
        if !self.prog.finished {
            out.push(StreamEvent::Error(ProviderError::new(ErrorKind::Network, format!("stream cut: {e}"))));
            self.prog.finished = true;
        }
        out
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        // calls still open without a finish_reason or [DONE]: their arguments may be cut
        let open = !self.calls.is_empty();
        self.calls.clear();
        let mut out: Vec<StreamEvent> = self.prog.at_end_with_calls(open).into_iter().collect();
        out.extend(self.usage.take().map(StreamEvent::Usage));
        out
    }
}

impl ChatParser {
    fn end_calls(&mut self) -> Vec<StreamEvent> {
        let calls = std::mem::take(&mut self.calls);
        calls
            .into_iter()
            .map(|(_, id, name, args)| StreamEvent::ToolCallEnd { id, name, args: parse_args(&args) })
            .collect()
    }

    /// `[DONE]`: the response is complete whatever came before it.
    fn close(&mut self) -> Vec<StreamEvent> {
        let mut out = self.end_calls();
        if !self.prog.finished {
            self.prog.finished = true;
            self.prog.started = true;
            out.push(StreamEvent::Finish(if self.prog.saw_tool { FinishReason::ToolUse } else { FinishReason::Stop }));
        }
        out.extend(self.usage.take().map(StreamEvent::Usage));
        out
    }
}

/// A temperature as the JSON number the person typed (an `f32` widened
/// to `f64` would carry noise: 0.2 becomes 0.20000000298).
pub(crate) fn temp(t: f32) -> Value {
    json!((t as f64 * 1000.0).round() / 1000.0)
}

/// Arguments as JSON; an empty or broken fragment gives an empty object.
pub(crate) fn parse_args(s: &str) -> Value {
    let s = s.trim();
    if s.is_empty() {
        return json!({});
    }
    serde_json::from_str(s).unwrap_or_else(|_| json!({"_raw": s}))
}

/// Usage from the protocol's counts, with every vendor's cache field.
pub fn usage(u: &Value) -> Usage {
    let prompt = u["prompt_tokens"].as_u64().unwrap_or(0);
    let cached = u["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .or_else(|| u["prompt_cache_hit_tokens"].as_u64())
        .unwrap_or(0);
    Usage {
        input: prompt.saturating_sub(cached),
        output: u["completion_tokens"].as_u64().unwrap_or(0),
        cache_read: cached,
        cache_write: 0,
        reasoning: u["completion_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0),
    }
}

#[async_trait]
impl Protocol for OpenAiChat {
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
        let v = http::get_json(&http::openai_url(cfg, "models"), Self::headers(cfg, key)).await?;
        let mut out: Vec<ModelInfo> = v["data"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| m["id"].as_str())
                    .map(|id| ModelInfo { id: id.to_string(), ..Default::default() })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }
}

/// Pins a stream and stops it after the first `Done`, which every adapter's
/// stream ends with.
pub(crate) fn terminated(s: impl futures::Stream<Item = StreamEvent> + Send + 'static) -> EventStream {
    Box::pin(TerminatedStream { inner: Box::pin(s), done: false })
}

struct TerminatedStream {
    inner: EventStream,
    done: bool,
}

impl futures::Stream for TerminatedStream {
    type Item = StreamEvent;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<StreamEvent>> {
        if self.done {
            return std::task::Poll::Ready(None);
        }
        match self.inner.as_mut().poll_next(cx) {
            std::task::Poll::Ready(Some(StreamEvent::Done)) => {
                self.done = true;
                std::task::Poll::Ready(Some(StreamEvent::Done))
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::ToolSpec;

    fn cfg() -> ProviderConfig {
        ProviderConfig::new(ID, "https://api.deepseek.com", Some("DEEPSEEK_API_KEY"))
    }

    #[test]
    fn tool_calls_without_an_index_are_told_apart_by_id() {
        let chunks = [
            r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"id":"a","type":"function","function":{"name":"read","arguments":"{\"path\":\"a\"}"}},{"id":"b","type":"function","function":{"name":"ls","arguments":"{}"}}]}}]}"#,
            r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ];
        let mut p = ChatParser::default();
        let evs: Vec<StreamEvent> = chunks.iter().flat_map(|c| p.feed(c)).collect();
        let ends: Vec<_> = evs.iter().filter(|e| matches!(e, StreamEvent::ToolCallEnd { .. })).collect();
        assert_eq!(ends.len(), 2, "{evs:?}");
        assert!(matches!(ends[0], StreamEvent::ToolCallEnd { id, args, .. } if id == "a" && args["path"] == "a"));
        assert!(matches!(ends[1], StreamEvent::ToolCallEnd { id, name, .. } if id == "b" && name == "ls"));
    }

    #[test]
    fn a_null_error_field_is_not_an_error_and_an_empty_assistant_sends_empty_content() {
        let mut p = ChatParser::default();
        let evs = p.feed(r#"{"id":"x","error":null,"choices":[{"index":0,"delta":{"content":"ok"}}]}"#);
        assert!(evs.iter().all(|e| !matches!(e, StreamEvent::Error(_))));
        assert!(evs.contains(&StreamEvent::TextDelta("ok".into())));
        let mut p = ChatParser::default();
        let evs = p.feed(r#"{"error":{"message":"Rate limit reached","type":"rate_limit_error"}}"#);
        assert!(matches!(&evs[0], StreamEvent::Error(e) if e.kind == ErrorKind::RateLimit));
        let req = ModelRequest::new(
            "m",
            "",
            vec![Message::user("hi"), Message::new(Role::Assistant, vec![]), Message::user("more")],
        );
        let b = OpenAiChat::body(&cfg(), &req);
        assert_eq!(b["messages"][1]["content"], "");
    }

    #[test]
    fn the_cache_key_goes_only_to_openai_itself() {
        let mut req = ModelRequest::new("gpt-5", "", vec![Message::user("hi")]);
        req.session_id = Some("sess-1".into());
        let openai = OpenAiChat::body(&ProviderConfig::new(ID, "https://api.openai.com/v1", None), &req);
        assert_eq!(openai["prompt_cache_key"], "sess-1");
        for base in ["https://api.deepseek.com", "https://openrouter.ai/api/v1", "http://localhost:11434/v1"] {
            let b = OpenAiChat::body(&ProviderConfig::new(ID, base, None), &req);
            assert!(b.get("prompt_cache_key").is_none(), "{base}");
        }
        req.session_id = None;
        assert!(OpenAiChat::body(&ProviderConfig::new(ID, "https://api.openai.com/v1", None), &req)
            .get("prompt_cache_key")
            .is_none());
    }

    #[test]
    fn the_end_of_the_stream_without_a_finish_is_read_from_what_came() {
        let mut p = ChatParser::default();
        assert!(matches!(&p.finish()[0], StreamEvent::Error(e) if e.kind == ErrorKind::Parse), "nothing read at all");
        let mut p = ChatParser::default();
        p.feed(r#"{"id":"x","choices":[{"index":0,"delta":{"content":"partial"}}]}"#);
        assert_eq!(p.finish(), vec![StreamEvent::Finish(FinishReason::Stop)], "content without a finish is complete");
        let mut p = ChatParser::default();
        p.feed(r#"{"id":"x","choices":[{"index":0,"delta":{"role":"assistant"}}]}"#);
        assert!(
            matches!(&p.finish()[0], StreamEvent::Error(e) if e.kind == ErrorKind::Network),
            "a start without content"
        );
    }

    #[test]
    fn body_places_system_tools_and_results_the_openai_way() {
        let mut req = ModelRequest::new(
            "deepseek-chat",
            "be brief",
            vec![
                Message::user("read a.rs"),
                Message::new(
                    Role::Assistant,
                    vec![
                        Part::Reasoning { text: "secret".into(), signature: None, encrypted: None, model: None },
                        Part::text("ok"),
                        Part::tool_call("c1", "read", json!({"path": "a.rs"})),
                    ],
                ),
                Message::tool_results(vec![Part::tool_result("c1", "fn main() {}", false)]),
            ],
        );
        req.tools =
            vec![ToolSpec { name: "read".into(), description: "reads".into(), parameters: json!({"type": "object"}) }];
        req.temperature = Some(0.2);
        req.no_tool_call = true;
        assert_eq!(OpenAiChat::body(&cfg(), &req)["tool_choice"], "none");
        req.no_tool_call = false;
        let b = OpenAiChat::body(&cfg(), &req);
        assert!(b.get("tool_choice").is_none());
        assert_eq!(b["messages"][0], json!({"role": "system", "content": "be brief"}));
        assert_eq!(b["messages"][1], json!({"role": "user", "content": "read a.rs"}));
        assert_eq!(b["messages"][2]["role"], "assistant");
        assert!(b["messages"][2].get("reasoning_content").is_none(), "readable reasoning is not sent back");
        assert_eq!(b["messages"][2]["tool_calls"][0]["function"]["arguments"], "{\"path\":\"a.rs\"}");
        assert_eq!(b["messages"][3], json!({"role": "tool", "tool_call_id": "c1", "content": "fn main() {}"}));
        assert_eq!(b["tools"][0]["type"], "function");
        assert_eq!(b["tools"][0]["function"]["name"], "read");
        assert_eq!(b["stream_options"]["include_usage"], true);
        assert_eq!(b["max_tokens"], 8192);
        assert_eq!(b["temperature"], 0.2);
        assert_eq!(OpenAiChat::url(&cfg()), "https://api.deepseek.com/v1/chat/completions");
    }

    #[test]
    fn newer_openai_models_take_max_completion_tokens() {
        let req = ModelRequest::new("o3-mini", "", vec![Message::user("hi")]);
        let b = OpenAiChat::body(&cfg(), &req);
        assert_eq!(b["max_completion_tokens"], 8192);
        assert!(b.get("max_tokens").is_none());
        assert!(b.get("messages").unwrap().as_array().unwrap().len() == 1, "no empty system message");
    }

    #[test]
    fn parses_fragmented_tool_calls_reasoning_and_deepseek_usage() {
        let chunks = [
            r#"{"id":"x","model":"deepseek-reasoner","choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"think"}}]}"#,
            r#"{"id":"x","choices":[{"index":0,"delta":{"content":"Hi"}}]}"#,
            r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":""}}]}}]}"#,
            r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"pa"}}]}}]}"#,
            r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a\"}"}}]}}]}"#,
            r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"id":"x","choices":[],"usage":{"prompt_tokens":100,"completion_tokens":7,"prompt_cache_hit_tokens":64,"prompt_cache_miss_tokens":36,"completion_tokens_details":{"reasoning_tokens":3}}}"#,
            "[DONE]",
        ];
        let mut p = ChatParser::default();
        let evs: Vec<StreamEvent> = chunks.iter().flat_map(|c| p.feed(c)).collect();
        assert!(matches!(evs[0], StreamEvent::Start { .. }));
        assert_eq!(evs[1], StreamEvent::ReasoningDelta("think".into()));
        assert_eq!(evs[2], StreamEvent::TextDelta("Hi".into()));
        assert_eq!(evs[3], StreamEvent::ToolCallStart { id: "call_1".into(), name: "read".into() });
        assert_eq!(
            evs[6],
            StreamEvent::ToolCallEnd { id: "call_1".into(), name: "read".into(), args: json!({"path": "a"}) }
        );
        assert_eq!(evs[7], StreamEvent::Finish(FinishReason::ToolUse));
        assert_eq!(
            evs[8],
            StreamEvent::Usage(Usage { input: 36, output: 7, cache_read: 64, cache_write: 0, reasoning: 3 })
        );
        assert_eq!(evs.len(), 9, "[DONE] adds nothing after a finish");
    }

    #[test]
    fn an_error_chunk_becomes_an_error_event() {
        let mut p = ChatParser::default();
        let evs = p.feed(r#"{"error":{"message":"overloaded","type":"server_error"}}"#);
        assert!(matches!(&evs[0], StreamEvent::Error(e) if e.message.contains("overloaded")));
    }

    #[test]
    fn running_usage_reports_the_last_figures_once() {
        let chunks = [
            r#"{"id":"x","choices":[{"index":0,"delta":{"content":"a"}}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            r#"{"id":"x","choices":[{"index":0,"delta":{"content":"b"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#,
            r#"{"id":"x","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
            "[DONE]",
        ];
        let mut p = ChatParser::default();
        let mut evs: Vec<StreamEvent> = chunks.iter().flat_map(|c| p.feed(c)).collect();
        evs.extend(p.finish());
        let usages: Vec<_> = evs.iter().filter(|e| matches!(e, StreamEvent::Usage(_))).collect();
        assert_eq!(usages, vec![&StreamEvent::Usage(Usage { input: 10, output: 5, ..Default::default() })]);
        // a stream without [DONE] reports it at its end
        let mut p = ChatParser::default();
        let mut evs: Vec<StreamEvent> = chunks[..3].iter().flat_map(|c| p.feed(c)).collect();
        evs.extend(p.finish());
        assert_eq!(evs.last(), Some(&StreamEvent::Usage(Usage { input: 10, output: 5, ..Default::default() })));
    }

    const CALL_OPEN: &str = r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"read","arguments":"{\"pa"}}]}}]}"#;

    #[test]
    fn an_error_drops_open_tool_calls_instead_of_closing_them() {
        let mut p = ChatParser::default();
        let mut evs = p.feed(CALL_OPEN);
        evs.extend(p.feed(r#"{"error":{"message":"overloaded","type":"server_error"}}"#));
        evs.extend(p.finish());
        assert!(!evs.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { .. })), "{evs:?}");
        assert!(matches!(evs.last(), Some(StreamEvent::Error(_))), "{evs:?}");
    }

    #[test]
    fn a_clean_end_inside_a_tool_call_is_a_truncation_error() {
        let mut p = ChatParser::default();
        let mut evs = p.feed(CALL_OPEN);
        evs.extend(p.finish());
        assert!(!evs.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { .. } | StreamEvent::Finish(_))), "{evs:?}");
        assert!(matches!(evs.last(), Some(StreamEvent::Error(e)) if e.message.contains("truncated")), "{evs:?}");
    }

    #[test]
    fn a_picture_in_a_tool_result_follows_the_tool_message_as_an_image_url() {
        let req = ModelRequest::new(
            "gpt-5",
            "",
            vec![
                Message::new(Role::Assistant, vec![Part::tool_call("c1", "read", json!({"path": "n.png"}))]),
                Message::tool_results(vec![Part::ToolResult {
                    call_id: "c1".into(),
                    name: Some("read".into()),
                    content: vec![
                        Part::text("n.png: PNG image"),
                        Part::Image {
                            source: ImageSource::Base64 { media_type: "image/png".into(), data: "AAAA".into() },
                        },
                    ],
                    is_error: false,
                }]),
            ],
        );
        let b = OpenAiChat::body(&cfg(), &req);
        assert_eq!(b["messages"][0]["role"], "assistant");
        assert_eq!(b["messages"][1], json!({"role": "tool", "tool_call_id": "c1", "content": "n.png: PNG image"}));
        assert_eq!(
            b["messages"][2],
            json!({"role": "user", "content": [{"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}]})
        );
    }
}
