//! The SSE event feed of a messages stream.

use nexo_core::{ErrorKind, FinishReason, ProviderError, StreamEvent, Usage};
use serde_json::Value;

use crate::http;
use crate::openai_chat::parse_args;
use crate::parse::{stream_error_kind, Feed, Progress};

#[derive(Debug, Default)]
pub struct MessagesParser {
    /// Open blocks by index: (kind, id, name, fragments).
    blocks: Vec<(u64, String, String, String, String)>,
    usage: Usage,
    prog: Progress,
    usage_sent: bool,
}

impl Feed for MessagesParser {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        let v: Value = match serde_json::from_str(data.trim()) {
            Ok(v) => v,
            Err(_) => return out,
        };
        if v["type"].is_string() {
            self.prog.started = true;
        } else if let Some(e) = v.get("error").filter(|e| e.is_object()) {
            // a plain error object, sent without event framing by some gateways
            let msg = http::pretty_error(&v.to_string());
            let kind = stream_error_kind(e["type"].as_str().unwrap_or(""), &msg);
            out.push(StreamEvent::Error(ProviderError::new(kind, msg)));
            self.blocks.clear(); // calls cut by the error are dropped, not closed
            self.prog.started = true;
            self.prog.finished = true;
            return out;
        }
        match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                let m = &v["message"];
                out.push(StreamEvent::Start {
                    response_id: m["id"].as_str().map(String::from),
                    model: m["model"].as_str().map(String::from),
                });
                self.take_usage(&m["usage"]);
            }
            "content_block_start" => {
                let idx = v["index"].as_u64().unwrap_or(0);
                let b = &v["content_block"];
                let kind = b["type"].as_str().unwrap_or("").to_string();
                match kind.as_str() {
                    "tool_use" => {
                        let id = b["id"].as_str().unwrap_or("").to_string();
                        let name = b["name"].as_str().unwrap_or("").to_string();
                        self.prog.saw_tool = true;
                        self.prog.content = true;
                        out.push(StreamEvent::ToolCallStart { id: id.clone(), name: name.clone() });
                        self.blocks.push((idx, kind, id, name, String::new()));
                    }
                    "redacted_thinking" => {
                        out.push(StreamEvent::RedactedReasoning { data: b["data"].as_str().unwrap_or("").to_string() });
                    }
                    "thinking" => {
                        // the part is created by the first delta; an empty one by the signature
                        self.blocks.push((idx, kind, String::new(), String::new(), String::new()));
                    }
                    "text" => {
                        if let Some(t) = b["text"].as_str().filter(|t| !t.is_empty()) {
                            self.prog.content = true;
                            out.push(StreamEvent::TextDelta(t.to_string()));
                        }
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let idx = v["index"].as_u64().unwrap_or(0);
                let d = &v["delta"];
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        if let Some(t) = d["text"].as_str() {
                            self.prog.content = true;
                            out.push(StreamEvent::TextDelta(t.to_string()));
                        }
                    }
                    "thinking_delta" => {
                        if let Some(t) = d["thinking"].as_str() {
                            self.prog.content = true;
                            out.push(StreamEvent::ReasoningDelta(t.to_string()));
                        }
                    }
                    "signature_delta" => {
                        if let Some(s) = d["signature"].as_str() {
                            out.push(StreamEvent::ReasoningMeta { signature: Some(s.to_string()), encrypted: None });
                        }
                    }
                    "input_json_delta" => {
                        if let (Some(j), Some(b)) =
                            (d["partial_json"].as_str(), self.blocks.iter_mut().find(|b| b.0 == idx))
                        {
                            b.4.push_str(j);
                            out.push(StreamEvent::ToolCallDelta { id: b.2.clone(), partial_json: j.to_string() });
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let idx = v["index"].as_u64().unwrap_or(0);
                if let Some(pos) = self.blocks.iter().position(|b| b.0 == idx) {
                    let (_, kind, id, name, frags) = self.blocks.remove(pos);
                    if kind == "tool_use" {
                        out.push(StreamEvent::ToolCallEnd { id, name, args: parse_args(&frags) });
                    }
                }
            }
            "message_delta" => {
                self.take_usage(&v["usage"]);
                if let Some(r) = v["delta"]["stop_reason"].as_str() {
                    out.extend(self.end_calls());
                    if !self.prog.finished {
                        self.prog.finished = true;
                        out.push(StreamEvent::Finish(match r {
                            "max_tokens" | "model_context_window_exceeded" => FinishReason::Length,
                            "tool_use" => FinishReason::ToolUse,
                            "refusal" => FinishReason::ContentFilter,
                            _ if self.prog.saw_tool => FinishReason::ToolUse,
                            _ => FinishReason::Stop,
                        }));
                    }
                }
                if !self.usage_sent {
                    self.usage_sent = true;
                    out.push(StreamEvent::Usage(self.usage));
                }
            }
            "message_stop" => {
                out.extend(self.end_calls());
                if !self.prog.finished {
                    self.prog.finished = true;
                    out.push(StreamEvent::Finish(if self.prog.saw_tool {
                        FinishReason::ToolUse
                    } else {
                        FinishReason::Stop
                    }));
                }
                if !self.usage_sent && !self.usage.is_empty() {
                    self.usage_sent = true;
                    out.push(StreamEvent::Usage(self.usage));
                }
            }
            "error" => {
                let msg = http::pretty_error(&v.to_string());
                let kind = stream_error_kind(v["error"]["type"].as_str().unwrap_or(""), &msg);
                out.push(StreamEvent::Error(ProviderError::new(kind, msg)));
                self.blocks.clear(); // calls cut by the error are dropped, not closed
                self.prog.finished = true;
            }
            _ => {} // ping
        }
        out
    }

    fn fail(&mut self, e: String) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        self.blocks.clear(); // calls cut by the transport are dropped, not closed
        if !self.prog.finished {
            self.prog.finished = true;
            out.push(StreamEvent::Error(ProviderError::new(ErrorKind::Network, format!("stream cut: {e}"))));
        }
        out
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        if !self.prog.finished && self.prog.content && !self.usage_sent && !self.usage.is_empty() {
            self.usage_sent = true;
            out.push(StreamEvent::Usage(self.usage));
        }
        // calls still open without a stop: their arguments may be cut
        let open = self.blocks.iter().any(|b| b.1 == "tool_use");
        self.blocks.clear();
        out.extend(self.prog.at_end_with_calls(open));
        out
    }
}

impl MessagesParser {
    /// Counts arrive in parts (input at the start, output at the end) and
    /// the later ones are cumulative: each field present replaces the old.
    fn take_usage(&mut self, u: &Value) {
        if !u.is_object() {
            return;
        }
        if let Some(n) = u["input_tokens"].as_u64() {
            self.usage.input = n;
        }
        if let Some(n) = u["output_tokens"].as_u64() {
            self.usage.output = n;
        }
        if let Some(n) = u["cache_read_input_tokens"].as_u64() {
            self.usage.cache_read = n;
        }
        if let Some(n) = u["cache_creation_input_tokens"].as_u64() {
            self.usage.cache_write = n;
        }
    }

    fn end_calls(&mut self) -> Vec<StreamEvent> {
        std::mem::take(&mut self.blocks)
            .into_iter()
            .filter(|b| b.1 == "tool_use")
            .map(|(_, _, id, name, frags)| StreamEvent::ToolCallEnd { id, name, args: parse_args(&frags) })
            .collect()
    }
}
