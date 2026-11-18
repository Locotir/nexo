//! The request: url, headers, body, and the per-vendor rules for what the body carries.

use nexo_core::{Effort, ModelRequest, ProviderConfig, ReasoningConfig};
use serde_json::{json, Value};

use super::messages::convert;
use super::parser::ChatParser;
use super::wire::temp;
use super::OpenAiChat;
use crate::http;
use crate::sse::SseEvent;
use nexo_core::EventStream;

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
        // DeepSeek thinking mode answers 400 to a tool loop whose assistant turns lack `reasoning_content`
        // and ignores the field otherwise, so it replays by default there. Strict servers keep it out unless
        // `send_reasoning` says so; the key always wins.
        let send_reasoning = cfg
            .extra
            .get("send_reasoning")
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| host_is(cfg, "api.deepseek.com"));
        for m in &req.messages {
            messages.extend(convert(m, send_reasoning));
        }
        if host_is(cfg, "api.mistral.ai") {
            for m in &mut messages {
                if let Some(calls) = m.get_mut("tool_calls").and_then(|v| v.as_array_mut()) {
                    for c in calls {
                        c["id"] = json!(mistral_id(c["id"].as_str().unwrap_or("")));
                    }
                }
                if let Some(id) = m["tool_call_id"].as_str().map(mistral_id) {
                    m["tool_call_id"] = json!(id);
                }
            }
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
        if let Some(t) = req.temperature.filter(|_| !default_temperature_only(cfg, &req.model)) {
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
        if let Some(r) = req.reasoning {
            reasoning_fields(cfg, r, &mut body);
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

/// How a reasoning setting goes into the body. A level is `reasoning_effort`, except on OpenRouter, which documents
/// `reasoning: {effort}`. Off is a field only where the vendor documents one: DeepSeek's `thinking` switch, and
/// `reasoning_effort: none` on Mistral, Groq and Ollama (the models that take it list `off`); elsewhere nothing
/// is sent, and the levels a model lists leave `off` out so that it is never offered there.
fn reasoning_fields(cfg: &ProviderConfig, r: ReasoningConfig, body: &mut Value) {
    let effort = match r {
        ReasoningConfig::Effort(e) => e,
        ReasoningConfig::Budget(b) => Effort::from_budget(b),
        ReasoningConfig::Off => {
            if host_is(cfg, "api.deepseek.com") {
                body["thinking"] = json!({"type": "disabled"});
            } else if host_is(cfg, "api.mistral.ai") || host_is(cfg, "api.groq.com") || is_ollama(cfg) {
                body["reasoning_effort"] = json!("none");
            }
            return;
        }
    };
    if host_is(cfg, "openrouter.ai") {
        body["reasoning"] = json!({"effort": effort.as_str()});
    } else {
        body["reasoning_effort"] = json!(effort.as_str());
    }
}

/// Whether the server is Ollama (its port, or its name in the address).
fn is_ollama(cfg: &ProviderConfig) -> bool {
    reqwest::Url::parse(&cfg.base_url)
        .ok()
        .is_some_and(|u| u.port() == Some(11434) || u.host_str().is_some_and(|h| h.contains("ollama")))
}

/// Whether the provider is OpenAI itself (`api.openai.com`).
pub(crate) fn is_openai(cfg: &ProviderConfig) -> bool {
    host_is(cfg, "api.openai.com")
}

fn host_is(cfg: &ProviderConfig, host: &str) -> bool {
    reqwest::Url::parse(&cfg.base_url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.eq_ignore_ascii_case(host)))
        .unwrap_or(false)
}

/// Mistral answers 400 to a tool call id that is not exactly nine letters and digits, and a
/// conversation carries the ids other providers made (`call_0`, `toolu_...`). The same id always
/// maps to the same nine characters, so a call and its result still meet.
pub(crate) fn mistral_id(id: &str) -> String {
    if id.len() == 9 && id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return id.to_string();
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    (0..9)
        .map(|_| {
            let c = DIGITS[(h % 62) as usize] as char;
            h /= 62;
            c
        })
        .collect()
}

/// OpenAI's reasoning models (`o*`, `gpt-5*`) answer 400 to a temperature other than their default,
/// so on OpenAI itself none is sent. The `-chat` models of the GPT-5 line are not reasoning models.
pub(crate) fn default_temperature_only(cfg: &ProviderConfig, model: &str) -> bool {
    is_openai(cfg) && needs_completion_key(model) && !model.to_ascii_lowercase().contains("-chat")
}

/// Newer OpenAI models reject `max_tokens` (`o*`, `gpt-5*`).
fn needs_completion_key(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    (m.starts_with('o') && m[1..].starts_with(|c: char| c.is_ascii_digit())) || m.starts_with("gpt-5")
}
