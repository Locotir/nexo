//! The SSE event feed of a Gemini stream.

use std::sync::atomic::{AtomicU64, Ordering};

use nexo_core::{ErrorKind, FinishReason, ProviderError, StreamEvent, Usage};
use serde_json::{json, Value};

use crate::parse::{stream_error_kind, Feed, Progress};

/// function calls must not repeat across turns, or an interface keyed by id
/// attaches a result to an older card.
static CALLS: AtomicU64 = AtomicU64::new(0);

/// A call id no earlier turn or session used: the time and a counter.
pub fn fresh_call_id() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("call_{:x}_{}", t, CALLS.fetch_add(1, Ordering::Relaxed) + 1)
}

#[derive(Debug, Default)]
pub struct GeminiParser {
    prog: Progress,
    usage: Option<Usage>,
    /// Usage was already reported (a later copy is not sent again).
    usage_sent: bool,
    /// A `Finish` was emitted (not an error).
    stopped: bool,
}

impl Feed for GeminiParser {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        let v: Value = match serde_json::from_str(data.trim()) {
            Ok(v) => v,
            Err(_) => return out,
        };
        if !v.is_object() {
            return out;
        }
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            let msg = e["message"].as_str().unwrap_or("error").to_string();
            let kind = stream_error_kind(e["status"].as_str().or_else(|| e["code"].as_str()).unwrap_or(""), &msg);
            out.push(StreamEvent::Error(ProviderError::new(kind, msg)));
            self.prog.finished = true;
            self.prog.started = true;
            return out;
        }
        if !self.prog.started {
            self.prog.started = true;
            out.push(StreamEvent::Start {
                response_id: v["responseId"].as_str().map(String::from),
                model: v["modelVersion"].as_str().map(String::from),
            });
        }
        if let Some(reason) = v["promptFeedback"]["blockReason"].as_str() {
            out.push(StreamEvent::Error(ProviderError::new(
                ErrorKind::BadRequest,
                format!("prompt blocked: {reason}"),
            )));
            self.prog.finished = true;
            return out;
        }
        let cand = &v["candidates"][0];
        if let Some(parts) = cand["content"]["parts"].as_array() {
            for p in parts {
                if let Some(sig) = p["thoughtSignature"].as_str() {
                    out.push(StreamEvent::ReasoningMeta { signature: Some(sig.to_string()), encrypted: None });
                }
                if let Some(fc) = p.get("functionCall").filter(|f| f.is_object()) {
                    self.prog.saw_tool = true;
                    self.prog.content = true;
                    let id =
                        fc["id"].as_str().or_else(|| p["id"].as_str()).map(String::from).unwrap_or_else(fresh_call_id);
                    let name = fc["name"].as_str().unwrap_or("").to_string();
                    // a call with no arguments has no `args`, or a null one
                    let args = fc.get("args").filter(|a| !a.is_null()).cloned().unwrap_or_else(|| json!({}));
                    out.push(StreamEvent::ToolCallStart { id: id.clone(), name: name.clone() });
                    out.push(StreamEvent::ToolCallEnd { id, name, args });
                } else if let Some(t) = p["text"].as_str() {
                    if !t.is_empty() {
                        self.prog.content = true;
                        if p["thought"].as_bool().unwrap_or(false) {
                            out.push(StreamEvent::ReasoningDelta(t.to_string()));
                        } else {
                            out.push(StreamEvent::TextDelta(t.to_string()));
                        }
                    }
                }
            }
        }
        if let Some(u) = v.get("usageMetadata").filter(|u| u.is_object()) {
            let prompt = u["promptTokenCount"].as_u64().unwrap_or(0);
            let cached = u["cachedContentTokenCount"].as_u64().unwrap_or(0);
            let thoughts = u["thoughtsTokenCount"].as_u64().unwrap_or(0);
            self.usage = Some(Usage {
                input: prompt.saturating_sub(cached),
                output: u["candidatesTokenCount"].as_u64().unwrap_or(0) + thoughts,
                cache_read: cached,
                cache_write: 0,
                cache_write_1h: 0,
                reasoning: thoughts,
            });
        }
        if let Some(r) = cand["finishReason"].as_str() {
            if !self.prog.finished {
                self.prog.finished = true;
                if let Some(u) = self.usage.take() {
                    self.usage_sent = true;
                    out.push(StreamEvent::Usage(u));
                }
                self.stopped = true;
                let filtered = matches!(r, "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII");
                // a broken call, or a non-content stop with nothing said, is a failed attempt, not an empty reply
                if matches!(r, "MALFORMED_FUNCTION_CALL" | "UNEXPECTED_TOOL_CALL")
                    || (!self.prog.content && !filtered && r != "MAX_TOKENS")
                {
                    out.push(StreamEvent::Error(ProviderError::new(
                        ErrorKind::Server,
                        format!("the model ended without a usable reply ({r})"),
                    )));
                    return out;
                }
                out.push(StreamEvent::Finish(match r {
                    "MAX_TOKENS" => FinishReason::Length,
                    _ if filtered => FinishReason::ContentFilter,
                    _ if self.prog.saw_tool => FinishReason::ToolUse,
                    _ => FinishReason::Stop,
                }));
            }
        }
        out
    }

    fn fail(&mut self, e: String) -> Vec<StreamEvent> {
        if self.prog.finished {
            return Vec::new();
        }
        self.prog.finished = true;
        vec![StreamEvent::Error(ProviderError::new(ErrorKind::Network, format!("stream cut: {e}")))]
    }

    /// Some responses end without a `finishReason`; what is pending is flushed.
    fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        // usage can come in a chunk after the one with finishReason
        if !self.usage_sent && (self.stopped || (!self.prog.finished && self.prog.content)) {
            if let Some(u) = self.usage.take() {
                self.usage_sent = true;
                out.push(StreamEvent::Usage(u));
            }
        }
        out.extend(self.prog.at_end());
        out
    }
}
