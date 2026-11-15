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
