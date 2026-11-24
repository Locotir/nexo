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
use nexo_core::{ErrorKind, EventStream, ModelInfo, ModelRequest, Part, Protocol, ProviderConfig, ProviderError};

use crate::http;

#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiChat;

pub const ID: &str = "openai-chat";

// Chat tool messages accept only text. Moving an image to a user message
// loses its tool-result association and may reorder it around parallel calls.
fn validate_tool_image_order(req: &ModelRequest) -> Result<(), ProviderError> {
    for message in &req.messages {
        for part in &message.content {
            if let Part::ToolResult { content, .. } = part {
                if crate::result_parts(content).iter().any(|p| matches!(p, Part::Image { .. })) {
                    return Err(ProviderError::new(ErrorKind::BadRequest,
                        "OpenAI Chat cannot preserve images in tool results; use a multimodal tool-result protocol instead"));
                }
            }
        }
    }
    Ok(())
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
        validate_tool_image_order(req)?;
        let body =
            http::post_stream(&Self::url(cfg), Self::headers(cfg, key), &Self::body(cfg, req), cfg.idle_timeout())
                .await?;
        Ok(Self::parse(http::sse(body)))
    }

    async fn list_models(&self, cfg: &ProviderConfig, key: Option<&str>) -> Result<Vec<ModelInfo>, ProviderError> {
        let v = http::get_json(&http::openai_url(cfg, "models"), Self::headers(cfg, key)).await?;
        Ok(crate::probe::openai_models(&v))
    }
}
