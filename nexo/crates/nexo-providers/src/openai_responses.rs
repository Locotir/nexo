//! OpenAI responses: `POST {base}/responses`, an `input` list of typed
//! items, SSE of typed events. Preferred by OpenAI and xAI for their
//! reasoning models; reasoning comes back as opaque items that must be
//! echoed on the next request.

mod input;
mod parser;
#[cfg(test)]
mod tests;

use async_trait::async_trait;
use input::input_items;
use nexo_core::{EventStream, ModelInfo, ModelRequest, Protocol, ProviderConfig, ProviderError, ReasoningConfig};
pub use parser::{usage, ResponsesParser};
use serde_json::{json, Value};

use crate::catalogue::Catalogue;
use crate::http;
use crate::openai_chat::{temp, OpenAiChat};
use crate::sse::SseEvent;

#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiResponses;

pub const ID: &str = "openai-responses";

impl OpenAiResponses {
    pub fn url(cfg: &ProviderConfig) -> String {
        http::openai_url(cfg, "responses")
    }

    pub fn body(cfg: &ProviderConfig, req: &ModelRequest) -> Value {
        Self::body_in(cfg, req, crate::catalogue::cached().as_deref())
    }

    /// `body` with the model catalogue given, so a test does not depend on the one cached on disk.
    fn body_in(cfg: &ProviderConfig, req: &ModelRequest, catalogue: Option<&Catalogue>) -> Value {
        let input = input_items(req);
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
                // OpenAI's ladder ends at xhigh; "max" is a 400 here.
                let effort = if e == nexo_core::Effort::Max { "xhigh" } else { e.as_str() };
                body["reasoning"] = json!({"effort": effort, "summary": "auto"});
                include.push("reasoning.encrypted_content");
            }
            Some(ReasoningConfig::Budget(b)) => {
                body["reasoning"] = json!({"effort": nexo_core::Effort::from_budget(b).as_str(), "summary": "auto"});
                include.push("reasoning.encrypted_content");
            }
            _ => {}
        }
        if cfg.extra.get("include_reasoning").and_then(|v| v.as_bool()).unwrap_or(false) && include.is_empty() {
            include.push("reasoning.encrypted_content");
        }
        // With no reasoning setting, a model that reasons by default still produces reasoning items, and with
        // `store: false` they come back only when asked for (encrypted); without them the next round of a tool
        // loop starts without the model's earlier reasoning. Sent only where the catalogue says the model
        // reasons, because a model that does not reason answers 400 to the field.
        if include.is_empty()
            && req.reasoning.is_none()
            && !body["store"].as_bool().unwrap_or(false)
            && reasons_by_default(cfg, &req.model, catalogue)
        {
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

/// Whether the catalogue says `model` reasons, on a server known to take `reasoning.encrypted_content`
/// (OpenAI itself and a ChatGPT login). Any other server, or a catalogue that does not say, is no.
fn reasons_by_default(cfg: &ProviderConfig, model: &str, catalogue: Option<&Catalogue>) -> bool {
    let preset = if cfg.login.as_deref() == Some("codex") {
        "codex"
    } else if crate::openai_chat::is_openai(cfg) {
        "openai"
    } else {
        return false;
    };
    catalogue.and_then(|c| c.lookup(preset, model)).and_then(|e| e.reasoning) == Some(true)
}

/// On by default only where the field is known to be accepted (OpenAI itself and a ChatGPT login);
/// `extra.prompt_cache_key` turns it on or off for any other server.
fn cache_key_allowed(cfg: &ProviderConfig) -> bool {
    let known = cfg.login.as_deref() == Some("codex") || crate::openai_chat::is_openai(cfg);
    cfg.extra.get("prompt_cache_key").and_then(|v| v.as_bool()).unwrap_or(known)
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
        let body = http::post_stream(
            &Self::url(cfg),
            Self::request_headers(cfg, key, req),
            &Self::body(cfg, req),
            cfg.idle_timeout(),
        )
        .await?;
        Ok(Self::parse(http::sse(body)))
    }

    async fn list_models(&self, cfg: &ProviderConfig, key: Option<&str>) -> Result<Vec<ModelInfo>, ProviderError> {
        OpenAiChat.list_models(cfg, key).await
    }
}
