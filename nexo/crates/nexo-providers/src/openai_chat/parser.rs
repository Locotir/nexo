//! Folds chat-completion chunks into events.

use nexo_core::{ErrorKind, FinishReason, ProviderError, StreamEvent, Usage};
use serde_json::Value;

use super::think::ThinkSplit;
use super::wire::{args_text, parse_args, usage};
use crate::http;
use crate::parse::{stream_error_kind, Feed, Progress};

/// Folds chat-completion chunks into events.
#[derive(Debug, Default)]
pub struct ChatParser {
    /// Open tool calls: (index, id given to nexo, name, argument fragments, id as the server sent it).
    calls: Vec<(usize, String, String, String, String)>,
    prog: Progress,
    /// The latest usage: servers that send running counts end with the final ones.
    usage: Option<Usage>,
    /// A leading `<think>` block in the text.
    think: ThinkSplit,
}
impl Feed for ChatParser {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        let data = data.trim();
        if data == "[DONE]" {
            out.extend(self.close());
            return out;
        }
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return out, // keep-alive noise some gateways send
        };
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            let msg = http::pretty_error(&v.to_string());
            let kind = stream_error_kind(e["type"].as_str().or_else(|| e["code"].as_str()).unwrap_or(""), &msg);
            out.push(StreamEvent::Error(ProviderError::new(kind, msg)));
            self.calls.clear(); // calls cut by the error are dropped, not closed
            self.prog.finished = true;
            return out;
        }
        if !self.prog.started {
            self.prog.started = true;
            out.push(StreamEvent::Start {
                response_id: v["id"].as_str().map(String::from),
                model: v["model"].as_str().map(String::from),
            });
        }
        // only choice 0 is the reply; a choice with another index (a gateway that sends several) is not mixed in
        if let Some(choice) =
            v["choices"].as_array().and_then(|c| c.iter().find(|c| c["index"].as_u64().unwrap_or(0) == 0))
        {
            // a server that ignores `stream: true` answers with one body whose `message` is the whole reply
            let delta = if choice["delta"].is_object() { &choice["delta"] } else { &choice["message"] };
            if let Some(r) = delta["reasoning_content"].as_str().or_else(|| delta["reasoning"].as_str()) {
                if !r.is_empty() {
                    self.prog.content = true;
                    out.push(StreamEvent::ReasoningDelta(r.to_string()));
                }
            }
            match &delta["content"] {
                Value::String(t) if !t.is_empty() => {
                    let pieces = self.think.push(t);
                    self.say(&mut out, pieces);
                }
                Value::Array(parts) => {
                    // Mistral-style typed parts
                    for p in parts {
                        match (p["type"].as_str(), p["text"].as_str()) {
                            (Some("thinking"), _) => {
                                if let Some(arr) = p["thinking"].as_array() {
                                    for t in arr {
                                        if let Some(s) = t["text"].as_str() {
                                            self.prog.content = true;
                                            out.push(StreamEvent::ReasoningDelta(s.to_string()));
                                        }
                                    }
                                }
                            }
                            (_, Some(t)) if !t.is_empty() => {
                                self.prog.content = true;
                                out.push(StreamEvent::TextDelta(t.to_string()));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            // a refusal is the model's answer, and the person reads it (OpenAI sends it apart from `content`)
            if let Some(r) = delta["refusal"].as_str().filter(|r| !r.is_empty()) {
                self.prog.content = true;
                out.push(StreamEvent::TextDelta(r.to_string()));
            }
            if let Some(calls) = delta["tool_calls"].as_array() {
                let held = self.think.flush();
                self.say(&mut out, held);
                for c in calls {
                    let id = c["id"].as_str().unwrap_or("");
                    let name = c["function"]["name"].as_str().unwrap_or("");
                    let args = args_text(&c["function"]["arguments"]);
                    let args = args.as_str();
                    let idx = match c["index"].as_u64() {
                        Some(i) => i as usize,
                        // no index (some servers): a known id continues its call, a new id or
                        // a name starts one, a bare fragment goes to the last one
                        None if !id.is_empty() => {
                            self.calls.iter().rposition(|k| k.4 == id).map_or(self.calls.len(), |i| self.calls[i].0)
                        }
                        None if !name.is_empty() => self.calls.len(),
                        None => self.calls.len().saturating_sub(1),
                    };
                    // a new id on an index already open is another call (some servers number every parallel call 0)
                    let known = self.calls.iter().rposition(|k| k.0 == idx && (id.is_empty() || k.4 == id));
                    match known {
                        None => {
                            // a made-up id must not repeat in the next turn, or a result lands on an older card
                            // (and a second call that reuses the id of the first would answer to its result)
                            let used = self.calls.iter().any(|k| k.1 == id);
                            let sent = id;
                            let id =
                                if id.is_empty() || used { crate::gemini::fresh_call_id() } else { id.to_string() };
                            self.calls.push((idx, id.clone(), name.to_string(), String::new(), sent.to_string()));
                            self.prog.saw_tool = true;
                            self.prog.content = true;
                            out.push(StreamEvent::ToolCallStart { id: id.clone(), name: name.to_string() });
                            if !args.is_empty() {
                                if let Some(k) = self.calls.last_mut() {
                                    k.3.push_str(args);
                                }
                                out.push(StreamEvent::ToolCallDelta { id, partial_json: args.to_string() });
                            }
                        }
                        Some(i) => {
                            if !name.is_empty() && self.calls[i].2.is_empty() {
                                self.calls[i].2 = name.to_string();
                            }
                            if !args.is_empty() {
                                self.calls[i].3.push_str(args);
                                out.push(StreamEvent::ToolCallDelta {
                                    id: self.calls[i].1.clone(),
                                    partial_json: args.to_string(),
                                });
                            }
                        }
                    }
                }
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                let held = self.think.flush();
                self.say(&mut out, held);
                // Z.ai ends a reply it failed to make with `network_error`, OpenRouter with `error`: a failed
                // attempt the client can send again, not an empty answer; the calls it cut are dropped
                if matches!(reason, "network_error" | "error") {
                    self.calls.clear();
                    if !self.prog.finished {
                        self.prog.finished = true;
                        out.push(StreamEvent::Error(ProviderError::new(
                            ErrorKind::Network,
                            format!("the provider ended the reply with an error ({reason})"),
                        )));
                    }
                    return out;
                }
                out.extend(self.end_calls());
                if !self.prog.finished {
                    self.prog.finished = true;
                    out.push(StreamEvent::Finish(match reason {
                        // `model_length` is Mistral's word, `model_context_window_exceeded` Z.ai's
                        "length" | "model_length" | "model_context_window_exceeded" => FinishReason::Length,
                        "tool_calls" | "function_call" if self.prog.saw_tool => FinishReason::ToolUse,
                        // `sensitive` is Z.ai's word for a reply it blocked
                        "content_filter" | "sensitive" => FinishReason::ContentFilter,
                        _ if self.prog.saw_tool => FinishReason::ToolUse,
                        _ => FinishReason::Stop,
                    }));
                }
            }
        }
        // `usage` is the standard place; Groq repeats it in `x_groq` and Moonshot streams it inside the choice
        let object = |u: &&Value| u.is_object();
        let found = v
            .get("usage")
            .filter(object)
            .or_else(|| v["x_groq"].get("usage").filter(object))
            .or_else(|| v["choices"][0].get("usage").filter(object));
        if let Some(u) = found {
            self.usage = Some(usage(u));
        }
        out
    }

    /// The transport failed: what is open is closed and the error reported.
    fn fail(&mut self, e: String) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        self.calls.clear(); // calls cut by the transport are dropped, not closed
        if !self.prog.finished {
            out.push(StreamEvent::Error(ProviderError::new(ErrorKind::Network, format!("stream cut: {e}"))));
            self.prog.finished = true;
        }
        out
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        // calls still open without a finish_reason or [DONE]: their arguments may be cut
        let open = !self.calls.is_empty();
        self.calls.clear();
        let held = self.think.flush();
        let mut out = Vec::new();
        self.say(&mut out, held);
        out.extend(self.prog.at_end_with_calls(open));
        out.extend(self.usage.take().map(StreamEvent::Usage));
        out
    }
}

impl ChatParser {
    /// Text and reasoning pieces as events.
    fn say(&mut self, out: &mut Vec<StreamEvent>, pieces: Vec<(bool, String)>) {
        for (reasoning, s) in pieces {
            self.prog.content = true;
            out.push(if reasoning { StreamEvent::ReasoningDelta(s) } else { StreamEvent::TextDelta(s) });
        }
    }

    fn end_calls(&mut self) -> Vec<StreamEvent> {
        let calls = std::mem::take(&mut self.calls);
        calls
            .into_iter()
            .map(|(_, id, name, args, _)| StreamEvent::ToolCallEnd { id, name, args: parse_args(&args) })
            .collect()
    }

    /// `[DONE]`: the response is complete whatever came before it.
    fn close(&mut self) -> Vec<StreamEvent> {
        let held = self.think.flush();
        let mut out = Vec::new();
        self.say(&mut out, held);
        out.extend(self.end_calls());
        if !self.prog.finished {
            self.prog.finished = true;
            self.prog.started = true;
            out.push(StreamEvent::Finish(if self.prog.saw_tool { FinishReason::ToolUse } else { FinishReason::Stop }));
        }
        out.extend(self.usage.take().map(StreamEvent::Usage));
        out
    }
}
