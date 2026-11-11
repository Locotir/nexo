//! Gemini: `POST {base}/v1beta/models/{model}:streamGenerateContent?alt=sse`,
//! `contents` of `parts`, whole `GenerateContentResponse` objects per event.
//! Function calls arrive whole and without ids: ids are synthesised here.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use nexo_core::{
    Effort, ErrorKind, EventStream, FinishReason, ImageSource, Message, ModelInfo, ModelRequest, Part, Protocol,
    ProviderConfig, ProviderError, ReasoningConfig, Role, StreamEvent, Usage,
};
use serde_json::{json, Map, Value};

use crate::http;
use crate::openai_chat::temp;
use crate::parse::{stream_error_kind, Feed, Progress};
use crate::sse::SseEvent;

#[derive(Debug, Default, Clone, Copy)]
pub struct Gemini;

pub const ID: &str = "gemini";

impl Gemini {
    pub fn url(cfg: &ProviderConfig, model: &str) -> String {
        http::join_versioned(&cfg.base_url, "v1beta", &format!("models/{model}:streamGenerateContent?alt=sse"))
    }

    pub fn headers(cfg: &ProviderConfig, key: Option<&str>) -> reqwest::header::HeaderMap {
        let mut own =
            vec![("content-type", "application/json".to_string()), ("accept", "text/event-stream".to_string())];
        if let Some(k) = key {
            if cfg.extra.get("bearer").and_then(|v| v.as_bool()).unwrap_or(false) {
                own.push(("authorization", format!("Bearer {k}")));
            } else {
                own.push(("x-goog-api-key", k.to_string()));
            }
        }
        http::headers(cfg, &own)
    }

    pub fn body(_cfg: &ProviderConfig, req: &ModelRequest) -> Value {
        let mut body = json!({"contents": Value::Array(contents(&req.messages, &req.model))});
        if !req.system.is_empty() {
            body["systemInstruction"] = json!({"parts": [{"text": req.system}]});
        }
        if !req.tools.is_empty() {
            let decls: Vec<Value> = req.tools.iter().map(declaration).collect();
            body["tools"] = json!([{"functionDeclarations": decls}]);
            if req.no_tool_call {
                body["toolConfig"] = json!({"functionCallingConfig": {"mode": "NONE"}});
            }
        }
        let mut gen = json!({"maxOutputTokens": req.max_tokens});
        if let Some(t) = req.temperature {
            gen["temperature"] = temp(t);
        }
        if !req.stop.is_empty() {
            gen["stopSequences"] = json!(req.stop);
        }
        match req.reasoning {
            Some(ReasoningConfig::Off) => gen["thinkingConfig"] = json!({"thinkingBudget": 0}),
            Some(ReasoningConfig::Budget(b)) => {
                gen["thinkingConfig"] = json!({"thinkingBudget": b, "includeThoughts": true})
            }
            Some(ReasoningConfig::Effort(e)) => {
                let b = match e {
                    Effort::Minimal => 512,
                    Effort::Low => 1024,
                    Effort::Medium => 8192,
                    Effort::High => 24576,
                };
                gen["thinkingConfig"] = json!({"thinkingBudget": b, "includeThoughts": true});
            }
            None => {}
        }
        body["generationConfig"] = gen;
        for (k, v) in &req.extra {
            body[k] = v.clone();
        }
        body
    }

    pub fn parse(events: impl futures::Stream<Item = Result<SseEvent, String>> + Send + 'static) -> EventStream {
        crate::parse::run(events, GeminiParser::default())
    }
}

/// A tool as a function declaration. A tool with no arguments goes without `parameters`: the API
/// answers 400 to an object schema whose `properties` is empty ("should be non-empty for OBJECT type").
fn declaration(t: &nexo_core::ToolSpec) -> Value {
    let mut d = json!({"name": t.name, "description": t.description});
    let params = clean_schema(&t.parameters);
    if params["properties"].as_object().is_some_and(|p| !p.is_empty()) || params["type"] != "object" {
        d["parameters"] = params;
    }
    d
}

/// The messages as `contents`, with tool results named after their calls.
fn contents(messages: &[Message], model: &str) -> Vec<Value> {
    let mut names: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for m in messages {
        for (id, name, _) in m.tool_calls() {
            names.insert(id, name);
        }
    }
    let mut out: Vec<Value> = Vec::new();
    for m in messages {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "model",
        };
        let mut parts = Vec::new();
        let mut pending_sig: Option<String> = None;
        for p in &m.content {
            match p {
                Part::Text { text } => {
                    if !text.is_empty() {
                        let mut part = json!({"text": text});
                        if let Some(s) = pending_sig.take() {
                            part["thoughtSignature"] = json!(s);
                        }
                        parts.push(part);
                    }
                }
                // a signature made by another model is not valid for this one
                p if p.is_foreign_reasoning(model) => {}
                Part::Reasoning { signature, .. } => {
                    // thoughts are not sent back; their signature rides on the next part
                    if signature.is_some() {
                        pending_sig = signature.clone();
                    }
                }
                Part::ToolCall { name, args, .. } => {
                    let mut part = json!({"functionCall": {"name": name, "args": args}});
                    if let Some(s) = pending_sig.take() {
                        part["thoughtSignature"] = json!(s);
                    }
                    parts.push(part);
                }
                Part::ToolResult { call_id, name, content, is_error } => {
                    let fname = name
                        .clone()
                        .or_else(|| names.get(call_id.as_str()).map(|s| s.to_string()))
                        .unwrap_or_else(|| call_id.clone());
                    let text = nexo_core::message::result_text(content);
                    let response = if *is_error { json!({"error": text}) } else { json!({"result": text}) };
                    parts.push(json!({"functionResponse": {"name": fname, "response": response}}));
                    // the pictures of the result ride in the same turn, after the response
                    for p in content {
                        if let Part::Image { source: ImageSource::Base64 { media_type, data } } = p {
                            parts.push(json!({"inlineData": {"mimeType": media_type, "data": data}}));
                        }
                    }
                }
                Part::Image { source } => match source {
                    ImageSource::Base64 { media_type, data } => {
                        parts.push(json!({"inlineData": {"mimeType": media_type, "data": data}}))
                    }
                    ImageSource::Url { url } => parts.push(json!({"fileData": {"fileUri": url}})),
                },
                Part::RedactedReasoning { .. } => {}
            }
        }
        if parts.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(prev) if prev["role"] == role => {
                if let Some(a) = prev["parts"].as_array_mut() {
                    a.extend(parts);
                }
            }
            _ => out.push(json!({"role": role, "parts": parts})),
        }
    }
    out
}

/// The schema without the keywords the API rejects.
pub fn clean_schema(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            let mut m = Map::new();
            for (k, v) in o {
                // the children of `properties` are names chosen by the tool, not keywords
                if k == "properties" {
                    if let Value::Object(props) = v {
                        let kept = props.iter().map(|(n, s)| (n.clone(), clean_schema(s))).collect();
                        m.insert(k.clone(), Value::Object(kept));
                        continue;
                    }
                }
                if matches!(
                    k.as_str(),
                    "additionalProperties" | "$schema" | "$id" | "default" | "examples" | "const" | "patternProperties"
                ) {
                    continue;
                }
                m.insert(k.clone(), clean_schema(v));
            }
            Value::Object(m)
        }
        Value::Array(a) => Value::Array(a.iter().map(clean_schema).collect()),
        other => other.clone(),
    }
}

/// Calls made in this process: the ids synthesised for Gemini's unnumbered
/// function calls must not repeat across turns, or an interface keyed by id
/// attaches a result to an older card.
static CALLS: AtomicU64 = AtomicU64::new(0);

/// A call id no earlier turn or session used: the time and a counter.
pub fn fresh_call_id() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("call_{:x}_{}", t, CALLS.fetch_add(1, Ordering::Relaxed) + 1)
}

#[derive(Debug, Default)]
pub struct GeminiParser {
    prog: Progress,
    usage: Option<Usage>,
    /// Usage was already reported (a later copy is not sent again).
    usage_sent: bool,
    /// A `Finish` was emitted (not an error).
    stopped: bool,
}

impl Feed for GeminiParser {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        let v: Value = match serde_json::from_str(data.trim()) {
            Ok(v) => v,
            Err(_) => return out,
        };
        if !v.is_object() {
            return out;
        }
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            let msg = e["message"].as_str().unwrap_or("error").to_string();
            let kind = stream_error_kind(e["status"].as_str().or_else(|| e["code"].as_str()).unwrap_or(""), &msg);
            out.push(StreamEvent::Error(ProviderError::new(kind, msg)));
            self.prog.finished = true;
            self.prog.started = true;
            return out;
        }
        if !self.prog.started {
            self.prog.started = true;
            out.push(StreamEvent::Start {
                response_id: v["responseId"].as_str().map(String::from),
                model: v["modelVersion"].as_str().map(String::from),
            });
        }
        if let Some(reason) = v["promptFeedback"]["blockReason"].as_str() {
            out.push(StreamEvent::Error(ProviderError::new(
                ErrorKind::BadRequest,
                format!("prompt blocked: {reason}"),
            )));
            self.prog.finished = true;
            return out;
        }
        let cand = &v["candidates"][0];
        if let Some(parts) = cand["content"]["parts"].as_array() {
            for p in parts {
                if let Some(sig) = p["thoughtSignature"].as_str() {
                    out.push(StreamEvent::ReasoningMeta { signature: Some(sig.to_string()), encrypted: None });
                }
                if let Some(fc) = p.get("functionCall").filter(|f| f.is_object()) {
                    self.prog.saw_tool = true;
                    self.prog.content = true;
                    let id =
                        fc["id"].as_str().or_else(|| p["id"].as_str()).map(String::from).unwrap_or_else(fresh_call_id);
                    let name = fc["name"].as_str().unwrap_or("").to_string();
                    // a call with no arguments has no `args`, or a null one
                    let args = fc.get("args").filter(|a| !a.is_null()).cloned().unwrap_or_else(|| json!({}));
                    out.push(StreamEvent::ToolCallStart { id: id.clone(), name: name.clone() });
                    out.push(StreamEvent::ToolCallEnd { id, name, args });
                } else if let Some(t) = p["text"].as_str() {
                    if !t.is_empty() {
                        self.prog.content = true;
                        if p["thought"].as_bool().unwrap_or(false) {
                            out.push(StreamEvent::ReasoningDelta(t.to_string()));
                        } else {
                            out.push(StreamEvent::TextDelta(t.to_string()));
                        }
                    }
                }
            }
        }
        if let Some(u) = v.get("usageMetadata").filter(|u| u.is_object()) {
            let prompt = u["promptTokenCount"].as_u64().unwrap_or(0);
            let cached = u["cachedContentTokenCount"].as_u64().unwrap_or(0);
            let thoughts = u["thoughtsTokenCount"].as_u64().unwrap_or(0);
            self.usage = Some(Usage {
                input: prompt.saturating_sub(cached),
                output: u["candidatesTokenCount"].as_u64().unwrap_or(0) + thoughts,
                cache_read: cached,
                cache_write: 0,
                reasoning: thoughts,
            });
        }
        if let Some(r) = cand["finishReason"].as_str() {
            if !self.prog.finished {
                self.prog.finished = true;
                if let Some(u) = self.usage.take() {
                    self.usage_sent = true;
                    out.push(StreamEvent::Usage(u));
                }
                self.stopped = true;
                let filtered = matches!(r, "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII");
                // a broken call, or a non-content stop with nothing said, is a failed attempt, not an empty reply
                if matches!(r, "MALFORMED_FUNCTION_CALL" | "UNEXPECTED_TOOL_CALL")
                    || (!self.prog.content && !filtered && r != "MAX_TOKENS")
                {
                    out.push(StreamEvent::Error(ProviderError::new(
                        ErrorKind::Server,
                        format!("the model ended without a usable reply ({r})"),
                    )));
                    return out;
                }
                out.push(StreamEvent::Finish(match r {
                    "MAX_TOKENS" => FinishReason::Length,
                    _ if filtered => FinishReason::ContentFilter,
                    _ if self.prog.saw_tool => FinishReason::ToolUse,
                    _ => FinishReason::Stop,
                }));
            }
        }
        out
    }

    fn fail(&mut self, e: String) -> Vec<StreamEvent> {
        if self.prog.finished {
            return Vec::new();
        }
        self.prog.finished = true;
        vec![StreamEvent::Error(ProviderError::new(ErrorKind::Network, format!("stream cut: {e}")))]
    }

    /// Some responses end without a `finishReason`; what is pending is flushed.
    fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        // usage can come in a chunk after the one with finishReason
        if !self.usage_sent && (self.stopped || (!self.prog.finished && self.prog.content)) {
            if let Some(u) = self.usage.take() {
                self.usage_sent = true;
                out.push(StreamEvent::Usage(u));
            }
        }
        out.extend(self.prog.at_end());
        out
    }
}

#[async_trait]
impl Protocol for Gemini {
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
            http::post_stream(&Self::url(cfg, &req.model), Self::headers(cfg, key), &Self::body(cfg, req)).await?;
        Ok(Self::parse(http::sse(body)))
    }

    async fn list_models(&self, cfg: &ProviderConfig, key: Option<&str>) -> Result<Vec<ModelInfo>, ProviderError> {
        let url = http::join_versioned(&cfg.base_url, "v1beta", "models?pageSize=200");
        let v = http::get_json(&url, Self::headers(cfg, key)).await?;
        Ok(v["models"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        let name = m["name"].as_str()?;
                        let methods = m["supportedGenerationMethods"].as_array();
                        if methods.is_some_and(|ms| !ms.iter().any(|x| x == "generateContent")) {
                            return None;
                        }
                        Some(ModelInfo {
                            id: name.trim_start_matches("models/").to_string(),
                            display: m["displayName"].as_str().map(String::from),
                            context_window: m["inputTokenLimit"].as_u64().map(|n| n as u32),
                            max_output: m["outputTokenLimit"].as_u64().map(|n| n as u32),
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
    fn body_uses_contents_and_names_function_responses() {
        let mut req = ModelRequest::new(
            "gemini-2.5-pro",
            "sys",
            vec![
                Message::user("hi"),
                Message::new(
                    Role::Assistant,
                    vec![
                        Part::Reasoning {
                            text: "t".into(),
                            signature: Some("TS".into()),
                            encrypted: None,
                            model: None,
                        },
                        Part::tool_call("call_1", "read", json!({"path": "a"})),
                    ],
                ),
                Message::tool_results(vec![Part::tool_result("call_1", "contents", false)]),
            ],
        );
        req.tools = vec![ToolSpec {
            name: "read".into(),
            description: "d".into(),
            parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}, "additionalProperties": false}),
        }];
        let b = Gemini::body(&ProviderConfig::new(ID, "https://generativelanguage.googleapis.com", None), &req);
        assert_eq!(b["systemInstruction"]["parts"][0]["text"], "sys");
        assert_eq!(b["contents"][0], json!({"role": "user", "parts": [{"text": "hi"}]}));
        assert_eq!(b["contents"][1]["role"], "model");
        assert_eq!(b["contents"][1]["parts"][0]["functionCall"]["name"], "read");
        assert_eq!(b["contents"][1]["parts"][0]["thoughtSignature"], "TS");
        assert_eq!(
            b["contents"][2]["parts"][0]["functionResponse"],
            json!({"name": "read", "response": {"result": "contents"}})
        );
        assert!(b["tools"][0]["functionDeclarations"][0]["parameters"].get("additionalProperties").is_none());
        assert_eq!(b["generationConfig"]["maxOutputTokens"], 8192);
        assert_eq!(
            Gemini::url(&ProviderConfig::new(ID, "https://generativelanguage.googleapis.com", None), "gemini-2.5-pro"),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn parses_thoughts_text_whole_calls_and_usage() {
        let data = [
            r#"{"candidates":[{"content":{"parts":[{"text":"plan","thought":true}],"role":"model"}}],"responseId":"r1","modelVersion":"gemini-2.5-pro"}"#,
            r#"{"candidates":[{"content":{"parts":[{"text":"Hello "}],"role":"model"}}]}"#,
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{"path":"a"}},"thoughtSignature":"TS"}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":10,"cachedContentTokenCount":40,"thoughtsTokenCount":6}}"#,
        ];
        let mut p = GeminiParser::default();
        let evs: Vec<StreamEvent> = data.iter().flat_map(|d| p.feed(d)).collect();
        assert_eq!(evs[0], StreamEvent::Start { response_id: Some("r1".into()), model: Some("gemini-2.5-pro".into()) });
        assert_eq!(evs[1], StreamEvent::ReasoningDelta("plan".into()));
        assert_eq!(evs[2], StreamEvent::TextDelta("Hello ".into()));
        assert_eq!(evs[3], StreamEvent::ReasoningMeta { signature: Some("TS".into()), encrypted: None });
        let id = match &evs[4] {
            StreamEvent::ToolCallStart { id, name } if name == "read" => id.clone(),
            other => panic!("{other:?}"),
        };
        assert!(id.starts_with("call_"));
        assert_eq!(
            evs[5],
            StreamEvent::ToolCallEnd { id: id.clone(), name: "read".into(), args: json!({"path": "a"}) }
        );
        assert_eq!(
            evs[6],
            StreamEvent::Usage(Usage { input: 60, output: 16, cache_read: 40, cache_write: 0, reasoning: 6 })
        );
        assert_eq!(evs[7], StreamEvent::Finish(FinishReason::ToolUse));
        // a second response never reuses an id
        let mut p2 = GeminiParser::default();
        let evs2: Vec<StreamEvent> = data.iter().flat_map(|d| p2.feed(d)).collect();
        assert!(matches!(&evs2[4], StreamEvent::ToolCallStart { id: id2, .. } if id2 != &id));
    }

    #[test]
    fn errors_in_the_stream_are_classified_and_a_cut_prompt_is_not_a_reply() {
        let mut p = GeminiParser::default();
        let evs = p.feed(r#"{"error":{"code":429,"message":"Quota exceeded","status":"RESOURCE_EXHAUSTED"}}"#);
        assert!(matches!(&evs[0], StreamEvent::Error(e) if e.kind == ErrorKind::RateLimit && e.is_retryable()));
        let mut p = GeminiParser::default();
        p.feed(r#"{"candidates":[{"content":{"parts":[],"role":"model"}}]}"#);
        assert!(matches!(&p.finish()[0], StreamEvent::Error(e) if e.kind == ErrorKind::Network));
    }

    #[test]
    fn a_malformed_call_or_an_empty_finish_is_a_retryable_error() {
        for r in ["MALFORMED_FUNCTION_CALL", "OTHER"] {
            let mut p = GeminiParser::default();
            let mut evs = p.feed(&format!(r#"{{"candidates":[{{"finishReason":"{r}"}}]}}"#));
            evs.extend(p.finish());
            assert!(!evs.iter().any(|e| matches!(e, StreamEvent::Finish(_))), "{evs:?}");
            assert!(evs
                .iter()
                .any(|e| matches!(e, StreamEvent::Error(e) if e.is_retryable() && e.message.contains(r))));
        }
        let mut p = GeminiParser::default();
        let evs = p.feed(r#"{"candidates":[{"finishReason":"SAFETY"}]}"#);
        assert!(evs.contains(&StreamEvent::Finish(FinishReason::ContentFilter)), "a block stays a block");
    }

    #[test]
    fn clean_schema_keeps_properties_named_like_keywords() {
        let s = json!({"type": "object", "additionalProperties": false,
            "properties": {"default": {"type": "string", "default": "x"}, "const": {"type": "integer"}},
            "required": ["default"]});
        let c = clean_schema(&s);
        assert_eq!(c["properties"]["default"], json!({"type": "string"}));
        assert_eq!(c["properties"]["const"], json!({"type": "integer"}));
        assert!(c.get("additionalProperties").is_none());
    }

    #[test]
    fn usage_after_the_finish_reason_is_still_reported_once() {
        let late = [
            r#"{"candidates":[{"content":{"parts":[{"text":"Hi"}],"role":"model"},"finishReason":"STOP"}]}"#,
            r#"{"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":2}}"#,
        ];
        let mut p = GeminiParser::default();
        let mut evs: Vec<StreamEvent> = late.iter().flat_map(|d| p.feed(d)).collect();
        evs.extend(p.finish());
        let usages: Vec<_> = evs.iter().filter(|e| matches!(e, StreamEvent::Usage(_))).collect();
        assert_eq!(usages, vec![&StreamEvent::Usage(Usage { input: 10, output: 2, ..Default::default() })]);
        // usage sent with the finish is not sent again when a later chunk repeats it
        let mut p = GeminiParser::default();
        let mut evs = p.feed(r#"{"candidates":[{"content":{"parts":[{"text":"Hi"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":2}}"#);
        evs.extend(p.feed(late[1]));
        evs.extend(p.finish());
        assert_eq!(evs.iter().filter(|e| matches!(e, StreamEvent::Usage(_))).count(), 1);
    }

    #[test]
    fn thought_signature_of_another_model_is_not_sent_back() {
        let history = |made_by: &str| {
            vec![
                Message::user("hi"),
                Message::new(
                    Role::Assistant,
                    vec![
                        Part::Reasoning {
                            text: "t".into(),
                            signature: Some("TS".into()),
                            encrypted: None,
                            model: Some(made_by.into()),
                        },
                        Part::tool_call("call_1", "read", json!({})),
                    ],
                ),
            ]
        };
        let cfg = ProviderConfig::new(ID, "https://generativelanguage.googleapis.com", None);
        let same = Gemini::body(&cfg, &ModelRequest::new("gem-a", "", history("gem-a")));
        assert_eq!(same["contents"][1]["parts"][0]["thoughtSignature"], "TS");
        let other = Gemini::body(&cfg, &ModelRequest::new("gem-b", "", history("gem-a")));
        assert!(!other.to_string().contains("thoughtSignature"));
        assert_eq!(other["contents"][1]["parts"][0]["functionCall"]["name"], "read");
    }

    #[test]
    fn a_picture_in_a_tool_result_rides_as_inline_data_after_the_response() {
        let msgs = vec![
            Message::new(Role::Assistant, vec![Part::tool_call("c1", "read", json!({"path": "n.png"}))]),
            Message::tool_results(vec![Part::ToolResult {
                call_id: "c1".into(),
                name: Some("read".into()),
                content: vec![
                    Part::text("n.png: PNG image"),
                    Part::Image { source: ImageSource::Base64 { media_type: "image/png".into(), data: "AAAA".into() } },
                ],
                is_error: false,
            }]),
        ];
        let c = contents(&msgs, "gemini-x");
        assert_eq!(c[1]["parts"][0]["functionResponse"]["response"]["result"], "n.png: PNG image");
        assert_eq!(c[1]["parts"][1], json!({"inlineData": {"mimeType": "image/png", "data": "AAAA"}}));
    }
}
