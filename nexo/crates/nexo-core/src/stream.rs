//! What a provider's response reduces to, whatever the protocol: a stream
//! of these events, which [`Collector`] folds into one assistant message.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ops::{Add, AddAssign};

use crate::error::ProviderError;
use crate::message::{Message, Part, Role};

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Start {
        response_id: Option<String>,
        model: Option<String>,
    },
    TextDelta(String),
    ReasoningDelta(String),
    /// Opaque reasoning data to keep with the current reasoning part.
    ReasoningMeta {
        signature: Option<String>,
        encrypted: Option<String>,
    },
    /// Reasoning the provider withheld (only its opaque data is kept).
    RedactedReasoning {
        data: String,
    },
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallDelta {
        id: String,
        partial_json: String,
    },
    /// The adapter joins the fragments and parses them; `args` is the result.
    ToolCallEnd {
        id: String,
        name: String,
        args: Value,
    },
    Usage(Usage),
    Finish(FinishReason),
    Error(ProviderError),
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolUse,
    ContentFilter,
    Error,
}

/// Tokens of one request, as every vendor reports them (zero where a vendor
/// has no such count).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens *not* served from the cache.
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Reasoning tokens (already counted in `output` where the vendor does so).
    pub reasoning: u64,
}

impl Usage {
    pub fn total_input(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }

    pub fn is_empty(&self) -> bool {
        *self == Usage::default()
    }
}

impl Add for Usage {
    type Output = Usage;
    fn add(self, o: Usage) -> Usage {
        Usage {
            input: self.input + o.input,
            output: self.output + o.output,
            cache_read: self.cache_read + o.cache_read,
            cache_write: self.cache_write + o.cache_write,
            reasoning: self.reasoning + o.reasoning,
        }
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, o: Usage) {
        *self = *self + o;
    }
}

/// Folds the events of one response into the assistant message they describe.
#[derive(Debug, Default)]
pub struct Collector {
    parts: Vec<Part>,
    /// Open tool calls: id -> (name, json fragments).
    open: Vec<(String, String, String)>,
    pub usage: Usage,
    pub finish: Option<FinishReason>,
    pub error: Option<ProviderError>,
    pub response_id: Option<String>,
    pub model: Option<String>,
    /// The model the request was sent to; stamped on the reasoning parts it produces.
    request_model: Option<String>,
}

impl Collector {
    pub fn new() -> Collector {
        Collector::default()
    }

    /// A collector that records `model` on the reasoning it collects.
    pub fn for_model(model: impl Into<String>) -> Collector {
        Collector { request_model: Some(model.into()), ..Collector::default() }
    }

    pub fn push(&mut self, ev: StreamEvent) {
        match ev {
            StreamEvent::Start { response_id, model } => {
                self.response_id = response_id;
                self.model = model;
            }
            StreamEvent::TextDelta(t) => match self.parts.last_mut() {
                Some(Part::Text { text }) => text.push_str(&t),
                _ => self.parts.push(Part::text(t)),
            },
            StreamEvent::ReasoningDelta(t) => match self.parts.last_mut() {
                Some(Part::Reasoning { text, .. }) => text.push_str(&t),
                _ => self.parts.push(Part::Reasoning {
                    text: t,
                    signature: None,
                    encrypted: None,
                    model: self.request_model.clone(),
                }),
            },
            StreamEvent::ReasoningMeta { signature, encrypted } => match self.parts.last_mut() {
                Some(Part::Reasoning { signature: s, encrypted: e, .. }) => {
                    if signature.is_some() {
                        *s = signature;
                    }
                    if encrypted.is_some() {
                        *e = encrypted;
                    }
                }
                _ => self.parts.push(Part::Reasoning {
                    text: String::new(),
                    signature,
                    encrypted,
                    model: self.request_model.clone(),
                }),
            },
            StreamEvent::RedactedReasoning { data } => {
                self.parts.push(Part::RedactedReasoning { data, model: self.request_model.clone() })
            }
            StreamEvent::ToolCallStart { id, name } => self.open.push((id, name, String::new())),
            StreamEvent::ToolCallDelta { id, partial_json } => {
                if let Some(o) = self.open.iter_mut().find(|o| o.0 == id) {
                    o.2.push_str(&partial_json);
                } else {
                    self.open.push((id, String::new(), partial_json));
                }
            }
            StreamEvent::ToolCallEnd { id, name, args } => {
                self.open.retain(|o| o.0 != id);
                self.parts.push(Part::ToolCall { id, name, args });
            }
            StreamEvent::Usage(u) => self.usage += u,
            StreamEvent::Finish(f) => self.finish = Some(f),
            StreamEvent::Error(e) => self.error = Some(e),
            StreamEvent::Done => {}
        }
    }

    /// The message so far (tool calls still open are closed with what arrived).
    pub fn message(&self) -> Message {
        let mut parts: Vec<Part> = self.parts.iter().filter(|p| !is_blank(p)).cloned().collect();
        for (id, name, json) in &self.open {
            let args = serde_json::from_str(json).unwrap_or_else(|_| Value::Object(Default::default()));
            parts.push(Part::ToolCall { id: id.clone(), name: name.clone(), args });
        }
        Message { role: Role::Assistant, content: parts }
    }

    pub fn finish_reason(&self) -> FinishReason {
        match self.finish {
            Some(f) => f,
            None if self.error.is_some() => FinishReason::Error,
            None if self.message().has_tool_calls() => FinishReason::ToolUse,
            None => FinishReason::Stop,
        }
    }
}

fn is_blank(p: &Part) -> bool {
    match p {
        Part::Text { text } => text.is_empty(),
        Part::Reasoning { text, signature, encrypted, .. } => {
            text.is_empty() && signature.is_none() && encrypted.is_none()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_text_reasoning_and_tool_calls_in_order() {
        let mut c = Collector::new();
        for ev in [
            StreamEvent::Start { response_id: Some("r1".into()), model: None },
            StreamEvent::ReasoningDelta("let me ".into()),
            StreamEvent::ReasoningDelta("see".into()),
            StreamEvent::ReasoningMeta { signature: Some("s".into()), encrypted: None },
            StreamEvent::TextDelta("Read".into()),
            StreamEvent::TextDelta("ing".into()),
            StreamEvent::ToolCallStart { id: "c1".into(), name: "read".into() },
            StreamEvent::ToolCallDelta { id: "c1".into(), partial_json: "{\"path\":".into() },
            StreamEvent::ToolCallDelta { id: "c1".into(), partial_json: "\"a\"}".into() },
            StreamEvent::ToolCallEnd { id: "c1".into(), name: "read".into(), args: serde_json::json!({"path": "a"}) },
            StreamEvent::Usage(Usage { input: 10, output: 5, ..Default::default() }),
            StreamEvent::Finish(FinishReason::ToolUse),
            StreamEvent::Done,
        ] {
            c.push(ev);
        }
        let m = c.message();
        assert_eq!(m.reasoning(), "let me see");
        assert_eq!(m.text(), "Reading");
        assert_eq!(m.tool_calls().count(), 1);
        assert_eq!(c.finish_reason(), FinishReason::ToolUse);
        assert_eq!(c.usage.total_input(), 10);
        match &m.content[0] {
            Part::Reasoning { signature, .. } => assert_eq!(signature.as_deref(), Some("s")),
            _ => panic!(),
        }
    }

    #[test]
    fn an_open_call_is_closed_with_what_arrived() {
        let mut c = Collector::new();
        c.push(StreamEvent::ToolCallStart { id: "c".into(), name: "bash".into() });
        c.push(StreamEvent::ToolCallDelta { id: "c".into(), partial_json: "{\"command\":\"ls\"}".into() });
        assert_eq!(c.message().tool_calls().next().unwrap().2["command"], "ls");
        assert_eq!(c.finish_reason(), FinishReason::ToolUse);
    }
}
