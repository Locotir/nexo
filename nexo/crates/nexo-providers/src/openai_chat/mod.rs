//! OpenAI chat completions: the protocol most vendors serve
//! (`POST {base}/chat/completions`, SSE of `choices[0].delta` chunks).

mod messages;
mod parser;
mod request;
mod stream;
#[cfg(test)]
mod tests;
mod think;
mod wire;

pub(crate) use messages::image_url;
pub use parser::ChatParser;
pub(crate) use request::{default_temperature_only, is_openai};
pub(crate) use stream::terminated;
pub use wire::usage;
pub(crate) use wire::{count, parse_args, temp};

use async_trait::async_trait;
use nexo_core::{EventStream, ModelInfo, ModelRequest, Protocol, ProviderConfig, ProviderError};

use crate::http;

#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiChat;

pub const ID: &str = "openai-chat";

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
        // Together answers with the bare array
        let mut out: Vec<ModelInfo> = v["data"]
            .as_array()
            .or_else(|| v.as_array())
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
