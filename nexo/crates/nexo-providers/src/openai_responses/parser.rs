//! The SSE event feed of a responses stream.

use nexo_core::{ErrorKind, FinishReason, ProviderError, StreamEvent, Usage};

use crate::http;
use serde_json::Value;

use crate::openai_chat::parse_args;
use crate::parse::{stream_error_kind, Feed, Progress};

#[derive(Debug, Default)]
pub struct ResponsesParser {
    /// Open function calls by output index: (call_id, name, fragments).
    calls: Vec<(u64, String, String, String)>,
    /// Reasoning items by output index: their ids.
    reasoning: Vec<(u64, String)>,
    prog: Progress,
    start_sent: bool,
}

impl Feed for ResponsesParser {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        let v: Value = match serde_json::from_str(data.trim()) {
            Ok(v) => v,
            Err(_) => return out,
        };
        let t = v["type"].as_str().unwrap_or("");
        if !t.is_empty() {
            self.prog.started = true;
        }
        if t.is_empty() {
            // a plain error object, sent without event framing by some gateways
            if let Some(e) = v.get("error").filter(|e| e.is_object()) {
                let msg = http::pretty_error(&v.to_string());
                let kind = stream_error_kind(e["type"].as_str().or_else(|| e["code"].as_str()).unwrap_or(""), &msg);
                out.push(StreamEvent::Error(ProviderError::new(kind, msg)));
                self.calls.clear(); // calls cut by the error are dropped, not closed
                self.prog.started = true;
                self.prog.finished = true;
                return out;
            }
        }
        match t {
            "codex.rate_limits" => crate::usage::record_event(&v),
            "response.created" | "response.in_progress" => {
                if !self.start_sent {
                    self.start_sent = true;
                    out.push(StreamEvent::Start {
                        response_id: v["response"]["id"].as_str().map(String::from),
                        model: v["response"]["model"].as_str().map(String::from),
                    });
                }
            }
            // a refusal is the model's answer, and the person reads it
            "response.output_text.delta" | "response.refusal.delta" => {
                if let Some(d) = v["delta"].as_str() {
                    self.prog.content = true;
                    out.push(StreamEvent::TextDelta(d.to_string()));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(d) = v["delta"].as_str() {
                    self.prog.content = true;
                    out.push(StreamEvent::ReasoningDelta(d.to_string()));
                }
            }
            "response.reasoning_summary_text.done" | "response.reasoning_summary_part.done" => {
                // a paragraph break between summary parts
                out.push(StreamEvent::ReasoningDelta("\n".into()));
            }
            "response.output_item.added" => {
                let item = &v["item"];
                let idx = v["output_index"].as_u64().unwrap_or(0);
                match item["type"].as_str() {
                    Some("function_call") => {
                        let id = item["call_id"].as_str().unwrap_or("").to_string();
                        let name = item["name"].as_str().unwrap_or("").to_string();
                        self.prog.saw_tool = true;
                        self.prog.content = true;
                        self.calls.push((idx, id.clone(), name.clone(), String::new()));
                        out.push(StreamEvent::ToolCallStart { id, name });
                    }
                    Some("reasoning") => {
                        if let Some(id) = item["id"].as_str() {
                            self.reasoning.push((idx, id.to_string()));
                        }
                    }
                    _ => {}
                }
            }
            "response.function_call_arguments.delta" => {
                let idx = v["output_index"].as_u64().unwrap_or(0);
                if let (Some(d), Some(c)) = (v["delta"].as_str(), self.calls.iter_mut().find(|c| c.0 == idx)) {
                    c.3.push_str(d);
                    out.push(StreamEvent::ToolCallDelta { id: c.1.clone(), partial_json: d.to_string() });
                }
            }
            "response.function_call_arguments.done" => {
                let idx = v["output_index"].as_u64().unwrap_or(0);
                if let Some(pos) = self.calls.iter().position(|c| c.0 == idx) {
                    let (_, id, name, frags) = self.calls.remove(pos);
                    let args = v["arguments"].as_str().map(String::from).unwrap_or(frags);
                    out.push(StreamEvent::ToolCallEnd { id, name, args: parse_args(&args) });
                }
            }
            "response.output_item.done" => {
                let item = &v["item"];
                let idx = v["output_index"].as_u64().unwrap_or(0);
                match item["type"].as_str() {
                    Some("reasoning") => {
                        let id = item["id"]
                            .as_str()
                            .map(String::from)
                            .or_else(|| self.reasoning.iter().find(|r| r.0 == idx).map(|r| r.1.clone()));
                        let enc = item["encrypted_content"].as_str().map(String::from);
                        if id.is_some() || enc.is_some() {
                            out.push(StreamEvent::ReasoningMeta { signature: id, encrypted: enc });
                        }
                    }
                    Some("function_call") => {
                        // a call whose arguments.done never came
                        if let Some(pos) = self.calls.iter().position(|c| c.0 == idx) {
                            let (_, id, name, frags) = self.calls.remove(pos);
                            let args = item["arguments"].as_str().map(String::from).unwrap_or(frags);
                            out.push(StreamEvent::ToolCallEnd { id, name, args: parse_args(&args) });
                        }
                    }
                    _ => {}
                }
            }
            "response.completed" | "response.incomplete" | "response.failed" => {
                if t == "response.failed" {
                    self.calls.clear(); // calls cut by the failure are dropped, not closed
                } else {
                    out.extend(self.end_calls());
                }
                let r = &v["response"];
                if let Some(u) = r.get("usage").filter(|u| u.is_object()) {
                    out.push(StreamEvent::Usage(usage(u)));
                }
                if !self.prog.finished {
                    self.prog.finished = true;
                    if t == "response.failed" {
                        let msg = r["error"]["message"].as_str().unwrap_or("response failed");
                        let kind = stream_error_kind(r["error"]["code"].as_str().unwrap_or(""), msg);
                        out.push(StreamEvent::Error(ProviderError::new(kind, msg)));
                    } else {
                        let reason = r["incomplete_details"]["reason"].as_str().unwrap_or("");
                        out.push(StreamEvent::Finish(match reason {
                            "max_output_tokens" => FinishReason::Length,
                            "content_filter" => FinishReason::ContentFilter,
                            _ if self.prog.saw_tool => FinishReason::ToolUse,
                            _ => FinishReason::Stop,
                        }));
                    }
                }
            }
            "error" => {
                let msg = v["message"].as_str().or_else(|| v["error"]["message"].as_str()).unwrap_or("error");
                let kind =
                    stream_error_kind(v["code"].as_str().or_else(|| v["error"]["type"].as_str()).unwrap_or(""), msg);
                out.push(StreamEvent::Error(ProviderError::new(kind, msg)));
                self.calls.clear(); // calls cut by the error are dropped, not closed
                self.prog.finished = true;
            }
            _ => {}
        }
        out
    }

    fn fail(&mut self, e: String) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        self.calls.clear(); // calls cut by the transport are dropped, not closed
        if !self.prog.finished {
            self.prog.finished = true;
            out.push(StreamEvent::Error(ProviderError::new(ErrorKind::Network, format!("stream cut: {e}"))));
        }
        out
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        // calls still open without a completed response: their arguments may be cut
        let open = !self.calls.is_empty();
        self.calls.clear();
        self.prog.at_end_with_calls(open).into_iter().collect()
    }
}

impl ResponsesParser {
    fn end_calls(&mut self) -> Vec<StreamEvent> {
        std::mem::take(&mut self.calls)
            .into_iter()
            .map(|(_, id, name, frags)| StreamEvent::ToolCallEnd { id, name, args: parse_args(&frags) })
            .collect()
    }
}

pub fn usage(u: &Value) -> Usage {
    let count = |v: &Value| crate::openai_chat::count(v).unwrap_or(0);
    let input = count(&u["input_tokens"]);
    let cached = count(&u["input_tokens_details"]["cached_tokens"]);
    Usage {
        input: input.saturating_sub(cached),
        output: count(&u["output_tokens"]),
        cache_read: cached,
        cache_write: 0,
        reasoning: count(&u["output_tokens_details"]["reasoning_tokens"]),
    }
}
