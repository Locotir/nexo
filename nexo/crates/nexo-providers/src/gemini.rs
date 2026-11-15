//! Gemini: `POST {base}/v1beta/models/{model}:streamGenerateContent?alt=sse`,
//! `contents` of `parts`, whole `GenerateContentResponse` objects per event.
//! Function calls arrive whole and without ids: ids are synthesised here.

mod contents;
mod parser;
mod schema;
#[cfg(test)]
mod tests;

pub use parser::{fresh_call_id, GeminiParser};
pub use schema::clean_schema;

use async_trait::async_trait;
use contents::{contents, declaration};
use nexo_core::{
    Effort, EventStream, ModelInfo, ModelRequest, Protocol, ProviderConfig, ProviderError, ReasoningConfig,
};
use serde_json::{json, Value};

use crate::http;
use crate::openai_chat::temp;
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
