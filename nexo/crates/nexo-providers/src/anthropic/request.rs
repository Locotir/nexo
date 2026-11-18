//! The request: url, headers and the JSON body of `/v1/messages`.

use nexo_core::{Effort, EventStream, ModelRequest, Part, ProviderConfig, ReasoningConfig, Role};
use serde_json::{json, Value};

use super::family::{adaptive_thinking, modern, off_thinking, takes_temperature, takes_xhigh};
use super::messages::convert;
use super::parser::MessagesParser;
use super::{Anthropic, VERSION};
use crate::http;
use crate::openai_chat::temp;
use crate::sse::SseEvent;

/// The reply size asked of a Claude 5.5 model when the person set no smaller cap.
pub(super) const MODERN_MAX_TOKENS: u32 = 64_000;
/// The largest reply of a Claude 5.5 model.
pub const MODERN_MAX_OUTPUT: u32 = 128_000;

/// The `output_config.effort` of a reasoning setting; `None` is off. `model` lowers `xhigh` to `high` where the
/// model has no such level (it is a 400 there).
pub(super) fn effort_name(r: ReasoningConfig, model: &str) -> Option<&'static str> {
    let e = match r {
        ReasoningConfig::Off => return None,
        ReasoningConfig::Effort(e) => e,
        ReasoningConfig::Budget(b) => Effort::from_budget(b),
    };
    Some(match e {
        Effort::Minimal | Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::Xhigh if takes_xhigh(model) => "xhigh",
        Effort::Xhigh => "high",
        Effort::Max => "max",
    })
}

impl Anthropic {
    pub fn url(cfg: &ProviderConfig) -> String {
        http::join_versioned(&cfg.base_url, "v1", "messages")
    }

    pub fn headers(cfg: &ProviderConfig, key: Option<&str>) -> reqwest::header::HeaderMap {
        let version = cfg.extra.get("api_version").and_then(|v| v.as_str()).unwrap_or(VERSION).to_string();
        let mut own = vec![
            ("content-type", "application/json".to_string()),
            ("accept", "text/event-stream".to_string()),
            ("anthropic-version", version),
        ];
        if let Some(k) = key {
            if cfg.extra.get("bearer").and_then(|v| v.as_bool()).unwrap_or(false) {
                own.push(("authorization", format!("Bearer {k}")));
            } else {
                own.push(("x-api-key", k.to_string()));
            }
        }
        http::headers(cfg, &own)
    }

    pub fn body(cfg: &ProviderConfig, req: &ModelRequest) -> Value {
        let cache = req.cache && cfg.extra.get("cache").and_then(|v| v.as_bool()).unwrap_or(true);
        // every breakpoint takes the same lifetime, so the rule that a longer one comes first holds
        let long = cache && req.cache_long && cfg.extra.get("cache_ttl_1h").and_then(|v| v.as_bool()).unwrap_or(true);
        let bp = if long { json!({"type": "ephemeral", "ttl": "1h"}) } else { json!({"type": "ephemeral"}) };
        let mut body = json!({"model": req.model, "max_tokens": req.max_tokens, "stream": true});
        if !req.system.is_empty() {
            let mut sys = json!({"type": "text", "text": req.system});
            if cache {
                sys["cache_control"] = bp.clone();
            }
            body["system"] = json!([sys]);
        }
        if !req.tools.is_empty() {
            let mut tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.parameters}))
                .collect();
            if cache {
                if let Some(last) = tools.last_mut() {
                    last["cache_control"] = bp.clone();
                }
            }
            body["tools"] = Value::Array(tools);
            if req.no_tool_call {
                body["tool_choice"] = json!({"type": "none"});
            }
        }
        let mut messages: Vec<Value> = Vec::new();
        // a tool round of this request lost its thinking block because another model made it
        let mut broken_round = false;
        let round_start = req
            .messages
            .iter()
            .rposition(|m| m.role == Role::User && !m.content.iter().any(Part::is_tool_result))
            .unwrap_or(0);
        for (i, m) in req.messages.iter().enumerate() {
            let (v, dropped) = convert(m, &req.model);
            broken_round |= dropped && i > round_start && m.has_tool_calls();
            match messages.last_mut() {
                // the protocol wants strict alternation: same roles are joined
                Some(prev) if prev["role"] == v["role"] => {
                    if let (Some(a), Some(b)) = (prev["content"].as_array_mut(), v["content"].as_array()) {
                        a.extend(b.iter().cloned());
                    }
                }
                _ => messages.push(v),
            }
        }
        if cache {
            // the first block of the first message (the person's opening request) never changes, while what a
            // context policy adds after it (a map of what it left out) does: the cache reads only up to a breakpoint
            // it finds, so without one here a change to that addition rewrites the opening request too
            if let Some(b) = messages
                .first_mut()
                .filter(|m| m["role"] == "user")
                .and_then(|m| m["content"].as_array_mut())
                .and_then(|c| c.first_mut())
            {
                b["cache_control"] = bp.clone();
            }
            if let Some(c) = messages.last_mut().and_then(|m| m["content"].as_array_mut()) {
                // a text that ends a message of several blocks rides on this request only (the time and facts line,
                // a supervisor note): the breakpoint goes before it, so the next request, which no longer carries
                // it, still finds this one's prefix in the cache
                let trailing = c.len() > 1 && c.last().is_some_and(|b| b["type"] == "text");
                let at = if trailing { c.len() - 2 } else { c.len().saturating_sub(1) };
                if let Some(b) = c.get_mut(at) {
                    b["cache_control"] = bp;
                }
            }
        }
        body["messages"] = Value::Array(messages);
        if modern(&req.model) {
            Self::modern_fields(cfg, req, &mut body);
        } else {
            if let Some(t) = req.temperature.filter(|_| takes_temperature(&req.model)) {
                body["temperature"] = temp(t);
            }
            let budget = match req.reasoning {
                Some(ReasoningConfig::Budget(b)) => Some(b.max(1024)),
                Some(ReasoningConfig::Effort(Effort::Minimal)) => Some(1024),
                Some(ReasoningConfig::Effort(Effort::Low)) => Some(2048),
                Some(ReasoningConfig::Effort(Effort::Medium)) => Some(8192),
                Some(ReasoningConfig::Effort(Effort::High)) => Some(16384),
                Some(ReasoningConfig::Effort(Effort::Xhigh)) => Some(24576),
                Some(ReasoningConfig::Effort(Effort::Max)) => Some(32768),
                _ => None,
            };
            // SetModel can land between the rounds of one turn (the command loop in
            // `nexo-agent/src/turn.rs` keeps serving while the turn task runs). The protocol
            // wants the thinking block of a tool round back, so when the model that made it is
            // gone, this request goes without thinking rather than invalid.
            // the budget must stay below max_tokens and at least 1024: a reply too small for that goes without
            if adaptive_thinking(&req.model) {
                // no budget on these models: the depth is the effort, and no reasoning set leaves it to the model
                if let Some(e) = req.reasoning.and_then(|r| effort_name(r, &req.model)).filter(|_| !broken_round) {
                    body["thinking"] = json!({"type": "adaptive"});
                    body["output_config"] = json!({"effort": e});
                } else if let (Some(ReasoningConfig::Off), Some(t)) = (req.reasoning, off_thinking(&req.model)) {
                    // Claude 5 thinks when `thinking` is left out: off must say so
                    body["thinking"] = json!({"type": t});
                }
            } else if let Some(b) = budget.filter(|_| !broken_round && req.max_tokens > 1024) {
                let b = b.min(req.max_tokens - 1);
                body["thinking"] = json!({"type": "enabled", "budget_tokens": b});
                if body["temperature"].is_number() {
                    body.as_object_mut().map(|o| o.remove("temperature")); // not allowed with thinking
                }
            }
        }
        // a stop sequence is no sampling parameter: the 5.5 line takes it like every other model, and the other
        // protocols send it whatever the model
        if !req.stop.is_empty() {
            body["stop_sequences"] = json!(req.stop);
        }
        for (k, v) in &req.extra {
            // the models of the 5.5 line reject sampling parameters whatever their value
            if modern(&req.model) && matches!(k.as_str(), "temperature" | "top_p" | "top_k") {
                continue;
            }
            if !takes_temperature(&req.model) && k == "temperature" {
                continue;
            }
            body[k] = v.clone();
        }
        body
    }

    /// The fields of the Claude 5.5 line: `thinking` is adaptive (a manual budget, `enabled` and sampling
    /// parameters are all a 400), the depth is `output_config.effort`, and the reply gets room for the
    /// reasoning, which counts inside `max_tokens`.
    fn modern_fields(cfg: &ProviderConfig, req: &ModelRequest, body: &mut Value) {
        // a round whose thinking block another model made keeps its settings here: the 5.5 API drops a block it
        // cannot read by itself, and the None branch below would strip every block of the history (a history edit:
        // the messages cache and every later block are lost)
        let effort = req.reasoning.map_or(Some("medium"), |r| effort_name(r, &req.model));
        match effort {
            Some(e) => {
                body["output_config"] = json!({"effort": e});
                let mut thinking = json!({"type": "adaptive"});
                // thinking blocks are valid only for the history they were made in; the curator rewrites
                // history, so a block whose prefix changed is dropped by the API instead of failing the call
                if cfg.extra.get("block_binding").and_then(|v| v.as_bool()).unwrap_or(true) {
                    thinking["block_binding"] = json!({"prefix_mismatch_behavior": "drop_block"});
                }
                body["thinking"] = thinking;
            }
            None => {
                // reasoning off: Sonnet has a lowest setting, Haiku can be disabled; Opus 5.5 always thinks and takes nothing
                if let Some(t) = off_thinking(&req.model) {
                    body["thinking"] = json!({"type": t});
                }
                // those take no `block_binding`: the blocks of earlier turns stay out
                if let Some(ms) = body["messages"].as_array_mut() {
                    for m in ms {
                        if let Some(c) = m["content"].as_array_mut() {
                            c.retain(|b| !matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking")));
                            if c.is_empty() {
                                c.push(json!({"type": "text", "text": "(empty)"}));
                            }
                        }
                    }
                }
            }
        }
        // a large file written in one call must not be cut, and thinking shares the room
        let floor = if req.max_tokens >= nexo_core::config::DEFAULT_MAX_TOKENS { MODERN_MAX_TOKENS } else { 4096 };
        body["max_tokens"] = json!(req.max_tokens.max(floor).min(MODERN_MAX_OUTPUT));
    }

    pub fn parse(events: impl futures::Stream<Item = Result<SseEvent, String>> + Send + 'static) -> EventStream {
        crate::parse::run(events, MessagesParser::default())
    }
}
