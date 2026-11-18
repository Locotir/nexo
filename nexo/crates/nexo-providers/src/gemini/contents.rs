//! Our messages and tools as the API takes them.

use nexo_core::{ImageSource, Message, Part, Role};
use serde_json::{json, Value};

use super::schema::clean_schema;

/// A tool as a function declaration. A tool with no arguments goes without `parameters`: the API
/// answers 400 to an object schema whose `properties` is empty ("should be non-empty for OBJECT type").
pub(super) fn declaration(t: &nexo_core::ToolSpec) -> Value {
    let mut d = json!({"name": t.name, "description": t.description});
    let params = clean_schema(&t.parameters);
    if params["properties"].as_object().is_some_and(|p| !p.is_empty()) || params["type"] != "object" {
        d["parameters"] = params;
    }
    d
}

/// The messages as `contents`, with tool results named after their calls.
pub(super) fn contents(messages: &[Message], model: &str) -> Vec<Value> {
    let mut names: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for m in messages {
        for (id, name, _) in m.tool_calls() {
            names.insert(id, name);
        }
    }
    let mut out: Vec<Value> = Vec::new();
    for m in messages {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "model",
        };
        let mut parts = Vec::new();
        let mut pending_sig: Option<String> = None;
        for p in &m.content {
            match p {
                Part::Text { text } => {
                    if !text.is_empty() {
                        let mut part = json!({"text": text});
                        if let Some(s) = pending_sig.take() {
                            part["thoughtSignature"] = json!(s);
                        }
                        parts.push(part);
                    }
                }
                // a signature made by another model is not valid for this one
                p if p.is_foreign_reasoning(model) => {}
                Part::Reasoning { signature, .. } => {
                    // thoughts are not sent back; their signature rides on the next part
                    if signature.is_some() {
                        pending_sig = signature.clone();
                    }
                }
                Part::ToolCall { name, args, .. } => {
                    let mut part = json!({"functionCall": {"name": name, "args": args}});
                    if let Some(s) = pending_sig.take() {
                        part["thoughtSignature"] = json!(s);
                    }
                    parts.push(part);
                }
                Part::ToolResult { call_id, name, content, is_error } => {
                    let fname = name
                        .clone()
                        .or_else(|| names.get(call_id.as_str()).map(|s| s.to_string()))
                        .unwrap_or_else(|| call_id.clone());
                    let text = nexo_core::message::result_text(content);
                    let response = if *is_error { json!({"error": text}) } else { json!({"result": text}) };
                    parts.push(json!({"functionResponse": {"name": fname, "response": response}}));
                    // the pictures of the result ride in the same turn, after the response
                    for p in content {
                        if let Part::Image { source: ImageSource::Base64 { media_type, data } } = p {
                            parts.push(json!({"inlineData": {"mimeType": media_type, "data": data}}));
                        }
                    }
                }
                Part::Image { source } => match source {
                    ImageSource::Base64 { media_type, data } => {
                        parts.push(json!({"inlineData": {"mimeType": media_type, "data": data}}))
                    }
                    ImageSource::Url { url } => parts.push(json!({"fileData": {"fileUri": url}})),
                },
                Part::RedactedReasoning { .. } => {}
            }
        }
        if parts.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(prev) if prev["role"] == role => {
                if let Some(a) = prev["parts"].as_array_mut() {
                    a.extend(parts);
                }
            }
            _ => out.push(json!({"role": role, "parts": parts})),
        }
    }
    // A call made by another model, or by another provider before a switch, has no signature of
    // this one. The first call of each step still needs a value, and only the first: the calls
    // that follow it in parallel carry none.
    if needs_call_signature(model) {
        for c in out.iter_mut().filter(|c| c["role"] == "model") {
            let first_call =
                c["parts"].as_array_mut().and_then(|p| p.iter_mut().find(|p| p.get("functionCall").is_some()));
            if let Some(call) = first_call.filter(|c| c.get("thoughtSignature").is_none()) {
                call["thoughtSignature"] = json!(NO_SIGNATURE);
            }
        }
    }
    out
}

/// The documented stand-in for a function call that has no thought signature (Google, "Thought
/// signatures": a dummy value skips the validation; the Vertex name of the same switch).
const NO_SIGNATURE: &str = "skip_thought_signature_validator";

/// Whether the model checks the signature of the first function call of each step: Gemini 3 and
/// later do and answer 400 to a call without one, earlier models do not check and may refuse an
/// unknown value. The version is read after `gemini-`, so a proxy prefix such as `models/` is fine.
/// The `-latest` aliases (`gemini-flash-latest`) have no number and point at the newest model of
/// their line, which is past version 3, so they count as such.
fn needs_call_signature(model: &str) -> bool {
    let Some(i) = model.find("gemini-") else { return false };
    let rest = &model[i + 7..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return rest.ends_with("-latest");
    }
    digits.parse::<u32>().is_ok_and(|v| v >= 3)
}
