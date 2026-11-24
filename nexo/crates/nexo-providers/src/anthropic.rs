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
        use nexo_core::ErrorKind;
        use std::collections::HashSet;

        let invalid = || ProviderError::new(ErrorKind::Parse, "invalid Anthropic model pagination");
        let base = http::join_versioned(&cfg.base_url, "v1", "models");
        let mut url = reqwest::Url::parse(&base).map_err(|_| invalid())?;
        url.query_pairs_mut().append_pair("limit", "100");
        let mut models = Vec::new();
        let mut ids = HashSet::new();
        let mut cursors = HashSet::new();
        // A broken or hostile endpoint must not produce an unbounded traversal.
        for _ in 0..1000 {
            let v = http::get_json(url.as_str(), Self::headers(cfg, key)).await?;
            let data = v["data"].as_array().ok_or_else(invalid)?;
            for model in data {
                let id = model["id"].as_str().filter(|id| !id.is_empty()).ok_or_else(invalid)?;
                if !ids.insert(id.to_owned()) {
                    return Err(invalid());
                }
            }
            let more = v["has_more"].as_bool().ok_or_else(invalid)?;
            models.extend(crate::probe::anthropic_models(&v));
            if !more {
                return Ok(models);
            }
            let cursor = v["last_id"].as_str().filter(|id| !id.is_empty()).ok_or_else(invalid)?;
            if data.last().and_then(|m| m["id"].as_str()) != Some(cursor) || !cursors.insert(cursor.to_owned()) {
                return Err(invalid());
            }
            url.query_pairs_mut().clear().append_pair("limit", "100").append_pair("after_id", cursor);
        }
        Err(invalid())
    }
}
