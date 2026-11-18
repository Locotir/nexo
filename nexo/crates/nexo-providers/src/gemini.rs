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

/// Gemini 3 and later set the depth with `thinkingLevel`; Gemini 2.5 takes a `thinkingBudget` in tokens.
pub fn takes_thinking_level(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.find("gemini-").is_some_and(|i| {
        let v: String = m[i + 7..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        v.parse::<f32>().is_ok_and(|v| v >= 3.0)
    })
}

/// The levels Gemini 3 takes, as the ones every model of the line has in common: `gemini-3-pro` has `low` and
/// `high` only, and none of them can switch thinking off.
pub fn level_names(model: &str) -> &'static [&'static str] {
    if model.to_ascii_lowercase().contains("gemini-3-pro") {
        &["low", "high"]
    } else {
        &["low", "medium", "high"]
    }
}

/// The `thinkingConfig` of a reasoning setting. Off, minimal and low are `low` on Gemini 3 (it cannot think less),
/// and Gemini 2.5 Pro takes 128 tokens at the least (a budget of 0 is a 400 there).
fn thinking_config(model: &str, r: ReasoningConfig) -> Value {
    let e = match r {
        ReasoningConfig::Effort(e) => e,
        ReasoningConfig::Budget(b) if !takes_thinking_level(model) => {
            return json!({"thinkingBudget": b, "includeThoughts": true})
        }
        ReasoningConfig::Budget(b) => Effort::from_budget(b),
        ReasoningConfig::Off => {
            return if takes_thinking_level(model) {
                json!({"thinkingLevel": "low"})
            } else if model.to_ascii_lowercase().contains("pro") {
                json!({"thinkingBudget": 128})
            } else {
                json!({"thinkingBudget": 0})
            };
        }
    };
    if takes_thinking_level(model) {
        let l = match e {
            Effort::Minimal | Effort::Low => "low",
            Effort::Medium if level_names(model).contains(&"medium") => "medium",
            _ => "high",
        };
        return json!({"thinkingLevel": l, "includeThoughts": true});
    }
    let b = match e {
        Effort::Minimal => 512,
        Effort::Low => 1024,
        Effort::Medium => 8192,
        Effort::High | Effort::Xhigh | Effort::Max => 24576,
    };
    json!({"thinkingBudget": b, "includeThoughts": true})
}

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
        if let Some(t) = req.reasoning.map(|r| thinking_config(&req.model, r)) {
            gen["thinkingConfig"] = t;
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
        let body = http::post_stream(
            &Self::url(cfg, &req.model),
            Self::headers(cfg, key),
            &Self::body(cfg, req),
            cfg.idle_timeout(),
        )
        .await?;
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
