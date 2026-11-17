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
    Xhigh,
    Max,
}

/// Every level a person can name, lowest first; `off` is the floor.
pub const LEVEL_ORDER: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// The level among `levels` that stands closest to `wanted`, when `wanted` is not listed itself (the lower one on a
/// tie; `off` only for `off`). None when it is listed, when the list is empty, or when a name is not on the ladder (`on`, a budget).
pub fn nearest_level(levels: &[String], wanted: &str) -> Option<String> {
    let rank = |l: &str| LEVEL_ORDER.iter().position(|x| *x == l);
    let w = rank(wanted)?;
    if levels.iter().any(|l| l == wanted) {
        return None;
    }
    let mut ranked = Vec::new();
    for l in levels {
        let r = rank(l)?;
        // asking for some reasoning never lands on none
        if r > 0 || w == 0 {
            ranked.push((r, l));
        }
    }
    ranked.into_iter().min_by_key(|(r, _)| (r.abs_diff(w), *r)).map(|(_, l)| l.clone())
}

/// The least thinking a side request can ask for among the `levels` a model lists: `off` where the model lists it and
/// off thinks less than no level (`off_less`), else its lowest listed rung, never `off` when the model does not list
/// it. A model with no list gets `off` only where that does something, else nothing is sent (None).
pub fn lowest_level(levels: &[String], off_less: bool) -> Option<ReasoningConfig> {
    let has = |l: &str| levels.iter().any(|x| x == l);
    if levels.is_empty() {
        return off_less.then_some(ReasoningConfig::Off);
    }
    if off_less && (has("off") || has("none")) {
        return Some(ReasoningConfig::Off);
    }
    LEVEL_ORDER.iter().skip(1).find(|l| has(l)).and_then(|l| Effort::parse(l)).map(ReasoningConfig::Effort)
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Minimal => "minimal",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }

    /// The level that stands for a thinking budget in tokens.
    pub fn from_budget(b: u32) -> Effort {
        match b {
            0..=2048 => Effort::Low,
            2049..=12_000 => Effort::Medium,
            _ => Effort::High,
        }
    }

    pub fn parse(s: &str) -> Option<Effort> {
        match s.trim().to_ascii_lowercase().as_str() {
            "minimal" => Some(Effort::Minimal),
            "low" => Some(Effort::Low),
            "medium" => Some(Effort::Medium),
            "high" => Some(Effort::High),
            "xhigh" => Some(Effort::Xhigh),
            "max" => Some(Effort::Max),
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
    /// Write the cache for the longest lifetime the protocol sells (an hour) instead of the short one: the agent
    /// sets it when the pauses of the conversation make the dearer write pay. Protocols with one lifetime ignore it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cache_long: bool,
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
            cache_long: false,
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
    /// The model list answered with the key and refused without it: the key is proved. A public list
    /// proves nothing about a key.
    #[serde(default)]
    pub key_proved: bool,
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

/// Whether an address names this machine (`localhost`, `127.*`, `[::1]`, `0.0.0.0`); the setup and the
/// local scan share the test, so nothing else is ever asked by the scan.
pub fn on_this_machine(url: &str) -> bool {
    let url = url.trim().to_ascii_lowercase();
    let host = url.split("://").last().unwrap_or("");
    ["localhost", "127.", "[::1]", "0.0.0.0"].iter().any(|h| host.starts_with(h))
}

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
