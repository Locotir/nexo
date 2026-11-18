//! Anthropic messages: `POST {base}/v1/messages`, content blocks, SSE with
//! named events and explicit cache breakpoints. Served compatibly by
//! several other vendors (`…/anthropic` base URLs).

mod family;
mod messages;
mod parser;
mod request;
#[cfg(test)]
mod tests;

pub use family::{effort_levels, modern, off_thinks_less};
pub use parser::MessagesParser;
pub use request::MODERN_MAX_OUTPUT;

use async_trait::async_trait;
use nexo_core::{EventStream, ModelInfo, ModelRequest, Protocol, ProviderConfig, ProviderError};

use crate::http;

#[derive(Debug, Default, Clone, Copy)]
pub struct Anthropic;

pub const ID: &str = "anthropic";
pub const VERSION: &str = "2023-06-01";

/// The beta that lets the API drop a thinking block whose history was edited, instead of a 400.
pub const BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";

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
        let body = http::post_stream(&Self::url(cfg), headers, &json, cfg.idle_timeout()).await?;
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
