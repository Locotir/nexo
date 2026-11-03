//! GitHub Copilot: one endpoint that speaks three protocols, chosen by the
//! model. Claude models take Anthropic messages (`{base}/v1/messages`),
//! GPT-5 and later, Grok and MAI Code take OpenAI responses
//! (`{base}/responses`), everything else chat completions
//! (`{base}/chat/completions`). The token is the GitHub login's
//! (`nexo auth login copilot`), sent as a bearer token with the headers the
//! Copilot API expects.

use async_trait::async_trait;
use nexo_core::{EventStream, Message, ModelInfo, ModelRequest, Part, Protocol, ProviderConfig, ProviderError, Role};

use crate::anthropic::Anthropic;
use crate::openai_chat::OpenAiChat;
use crate::openai_responses::OpenAiResponses;

#[derive(Debug, Default, Clone, Copy)]
pub struct Copilot;

pub const ID: &str = "copilot";
pub const BASE_URL: &str = "https://api.githubcopilot.com";
const API_VERSION: &str = "2026-08-01";

/// Which protocol Copilot serves `model` with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Messages,
    Responses,
    Chat,
}

pub fn route(model: &str) -> Route {
    let m = model.to_ascii_lowercase();
    if m.starts_with("claude") {
        return Route::Messages;
    }
    let gpt5 = m
        .strip_prefix("gpt-")
        .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse::<u32>().ok())
        .is_some_and(|n| n >= 5);
    if (gpt5 && !m.starts_with("gpt-5-mini")) || m.starts_with("grok-") || m.starts_with("mai-code-") {
        Route::Responses
    } else {
        Route::Chat
    }
}

/// The configuration each request goes out with: no version segment on the
/// OpenAI routes, a bearer token on the Anthropic one, and Copilot's headers.
pub fn shape(cfg: &ProviderConfig, req: Option<&ModelRequest>, route: Route) -> ProviderConfig {
    let mut c = cfg.clone();
    c.extra.insert("versionless".into(), toml::Value::Boolean(true));
    let mut set = |k: &str, v: &str| {
        c.headers.entry(k.to_string()).or_insert_with(|| v.to_string());
    };
    set("Openai-Intent", "conversation-edits");
    set("X-GitHub-Api-Version", API_VERSION);
    if let Some(req) = req {
        set("x-initiator", if by_agent(&req.messages) { "agent" } else { "user" });
        if req.messages.iter().any(|m| m.content.iter().any(|p| matches!(p, Part::Image { .. }))) {
            set("Copilot-Vision-Request", "true");
        }
    }
    if route == Route::Messages {
        c.extra.insert("bearer".into(), toml::Value::Boolean(true));
        set("anthropic-beta", "interleaved-thinking-2025-05-14");
    }
    c
}

/// A request the person did not just type (the last message is not theirs,
/// or carries only tool results) is the agent's: Copilot counts them apart.
fn by_agent(messages: &[Message]) -> bool {
    match messages.last() {
        Some(m) if m.role == Role::User => !m.content.iter().any(|p| !matches!(p, Part::ToolResult { .. })),
        _ => true,
    }
}

#[async_trait]
impl Protocol for Copilot {
    fn id(&self) -> &'static str {
        ID
    }

    async fn stream(
        &self,
        cfg: &ProviderConfig,
        key: Option<&str>,
        req: &ModelRequest,
    ) -> Result<EventStream, ProviderError> {
        let r = route(&req.model);
        let c = shape(cfg, Some(req), r);
        match r {
            Route::Messages => Anthropic.stream(&c, key, req).await,
            Route::Responses => OpenAiResponses.stream(&c, key, req).await,
            Route::Chat => OpenAiChat.stream(&c, key, req).await,
        }
    }

    async fn list_models(&self, cfg: &ProviderConfig, key: Option<&str>) -> Result<Vec<ModelInfo>, ProviderError> {
        OpenAiChat.list_models(&shape(cfg, None, Route::Chat), key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_by_model() {
        assert_eq!(route("claude-sonnet-5.5"), Route::Messages);
        assert_eq!(route("gpt-5.6-sol"), Route::Responses);
        assert_eq!(route("gpt-6"), Route::Responses);
        assert_eq!(route("gpt-5-mini"), Route::Chat);
        assert_eq!(route("gpt-4.1"), Route::Chat);
        assert_eq!(route("grok-code-fast-1"), Route::Responses);
        assert_eq!(route("gemini-3.8-flash"), Route::Chat);
    }

    #[test]
    fn requests_go_without_a_version_and_with_copilot_headers() {
        let cfg = ProviderConfig::new(ID, BASE_URL, None);
        let mut req = ModelRequest::new("gpt-4.1", "", vec![Message::user("hi")]);
        let c = shape(&cfg, Some(&req), Route::Chat);
        assert_eq!(OpenAiChat::url(&c), "https://api.githubcopilot.com/chat/completions");
        assert_eq!(OpenAiResponses::url(&c), "https://api.githubcopilot.com/responses");
        assert_eq!(c.headers["x-initiator"], "user");
        assert_eq!(c.headers["Openai-Intent"], "conversation-edits");
        let a = shape(&cfg, Some(&req), Route::Messages);
        assert_eq!(Anthropic::url(&a), "https://api.githubcopilot.com/v1/messages");
        let h = Anthropic::headers(&a, Some("tok"));
        assert_eq!(h["authorization"], "Bearer tok");
        assert!(h.get("x-api-key").is_none());
        req.messages.push(Message::assistant("ok"));
        assert_eq!(shape(&cfg, Some(&req), Route::Chat).headers["x-initiator"], "agent");
    }
}
