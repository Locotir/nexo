//! What a protocol adapter is: a request in, a stream of events out.
//!
//! An adapter implements [`Protocol`] once per *wire protocol* (OpenAI chat
//! completions, OpenAI responses, Anthropic messages, Gemini); a *vendor* is
//! only a [`crate::config::ProviderConfig`] naming the protocol, a base URL
//! and where the key is. See `docs/providers-base.md`.

use std::collections::BTreeMap;
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::ProviderConfig;
use crate::error::ProviderError;
use crate::message::Message;
use crate::pricing::Pricing;
use crate::stream::StreamEvent;

/// A tool as the model sees it: a name, what it does and a JSON schema of its arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// A JSON schema object (`{"type": "object", "properties": ...}`).
    pub parameters: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Minimal,
    Low,
    Medium,
    High,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Minimal => "minimal",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }

    pub fn parse(s: &str) -> Option<Effort> {
        match s.trim().to_ascii_lowercase().as_str() {
            "minimal" => Some(Effort::Minimal),
            "low" => Some(Effort::Low),
            "medium" => Some(Effort::Medium),
            "high" => Some(Effort::High),
            _ => None,
        }
    }
}

/// How much the model should reason, in whichever unit the protocol takes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningConfig {
    Off,
    Effort(Effort),
    Budget(u32),
}

/// One model call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
    #[serde(default)]
    pub stop: Vec<String>,
    /// The reply must be text: the tools stay declared, but the protocol is told that none may be called.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_tool_call: bool,
    /// Ask the protocol to mark cache breakpoints where it has explicit ones.
    #[serde(default = "yes")]
    pub cache: bool,
    /// What every request of one conversation shares (the session id; a sub-agent run has its own).
    /// Protocols that route their prompt cache by an id send it, so the prefix of one conversation
    /// is found again on the next request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Free-form, forwarded by adapters that know a key (e.g. `"store"`).
    #[serde(default)]
    pub extra: BTreeMap<String, Value>,
}

fn yes() -> bool {
    true
}

impl ModelRequest {
    pub fn new(model: impl Into<String>, system: impl Into<String>, messages: Vec<Message>) -> ModelRequest {
        ModelRequest {
            model: model.into(),
            system: system.into(),
            messages,
            tools: Vec::new(),
            max_tokens: 8192,
            temperature: None,
            reasoning: None,
            stop: Vec::new(),
            no_tool_call: false,
            cache: true,
            session_id: None,
            extra: BTreeMap::new(),
        }
    }
}

/// What a vendor says about one of its models.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<Pricing>,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub vision: bool,
    /// The reasoning levels the model takes (`low`, `medium`, `high`, ...); empty when it has none
    /// or its reasoning cannot be set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<String>,
}

/// What answers at a provider's address, found by asking it (`/model` setup).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerProbe {
    /// `ollama`, `vllm`, `llama.cpp`, `lmstudio`, `openrouter`, `anthropic`, `gemini`, `openai-compatible`.
    pub kind: String,
    /// The server's version, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The protocol to talk to it and the base URL that protocol takes.
    pub protocol: String,
    pub base_url: String,
    /// The server asked for a key (it answered 401/403 without one).
    pub needs_key: bool,
    /// Its models, with whatever it says of each.
    pub models: Vec<ModelInfo>,
    /// What was asked and what answered, one line each.
    pub trail: Vec<String>,
}

/// What a check of one model found: its facts (from the server, the catalogue, the
/// preset) and how a real request went.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelCheck {
    pub info: ModelInfo,
    /// Where the facts came from, one line each (`context 131072 from Ollama /api/show`).
    pub sources: Vec<String>,
    /// The test request: time to the end of the reply, the text it gave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
    /// The model called the test tool.
    #[serde(default)]
    pub called_tool: bool,
    /// The model sent reasoning.
    #[serde(default)]
    pub reasoned: bool,
    /// Why the test request failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ------------------------------------------------------------ what the model setup shares

/// The servers people run on their own machine, by preset, at their usual address.
pub const LOCAL: &[(&str, &str)] = &[
    ("ollama", "http://localhost:11434"),
    ("lmstudio", "http://localhost:1234"),
    ("vllm", "http://localhost:8000"),
    ("llamacpp", "http://localhost:8080"),
];

/// What a person types as an address, as a URL: `localhost:11434` gets
/// `http://`, `api.example.com` gets `https://`, a trailing `/` goes.
pub fn normalize_url(input: &str) -> String {
    let t = input.trim().trim_end_matches('/');
    if t.contains("://") {
        return t.to_string();
    }
    let host = t.split(['/', ':']).next().unwrap_or("");
    let local = host == "localhost"
        || host.starts_with("127.")
        || host.starts_with("192.168.")
        || host.starts_with("10.")
        || host == "0.0.0.0"
        || host.ends_with(".local")
        || host.starts_with('[');
    if local {
        format!("http://{t}")
    } else {
        format!("https://{t}")
    }
}

/// The name a person knows a server kind by.
pub fn kind_label(kind: &str) -> &str {
    match kind {
        "ollama" => "Ollama",
        "llama.cpp" => "llama.cpp",
        "lmstudio" => "LM Studio",
        "vllm" => "vLLM",
        "openrouter" => "OpenRouter",
        "anthropic" => "Anthropic",
        "gemini" => "Gemini",
        "mock" => "mock",
        "subscription" => "subscription",
        _ => "OpenAI-compatible",
    }
}

pub type EventStream = Pin<Box<dyn Stream<Item = StreamEvent> + Send>>;

/// A wire protocol. `key` is the resolved secret (None for keyless servers).
/// The stream it returns always ends with `Done`, after a `Finish` or an
/// `Error`; it never reorders, trims or rewrites the messages it is given.
#[async_trait]
pub trait Protocol: Send + Sync {
    /// `openai-chat`, `openai-responses`, `anthropic`, `gemini`, ...
    fn id(&self) -> &'static str;

    async fn stream(
        &self,
        provider: &ProviderConfig,
        key: Option<&str>,
        req: &ModelRequest,
    ) -> Result<EventStream, ProviderError>;

    /// The vendor's model list, where the protocol has one.
    async fn list_models(
        &self,
        _provider: &ProviderConfig,
        _key: Option<&str>,
    ) -> Result<Vec<ModelInfo>, ProviderError> {
        Err(ProviderError::new(crate::error::ErrorKind::Other, "this protocol has no model list"))
    }
}
