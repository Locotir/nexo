//! Our messages as the protocol takes them.

use nexo_core::{ImageSource, Message, Part, Role};
use serde_json::{json, Value};

/// The message as the protocol takes it, and whether a thinking block of another model was left out.
pub(super) fn convert(m: &Message, model: &str) -> (Value, bool) {
    let mut dropped = false;
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let mut content = Vec::new();
    for p in &m.content {
        match p {
            Part::Text { text } => {
                // a text block of blanks only is a 400 ("text content blocks must contain non-whitespace text")
                if !text.trim().is_empty() {
                    content.push(json!({"type": "text", "text": text}));
                }
            }
            p if p.is_foreign_reasoning(model) => dropped = true,
            Part::Reasoning { text, signature, .. } => {
                // the protocol only takes a thinking block back with its signature
                if let Some(sig) = signature {
                    content.push(json!({"type": "thinking", "thinking": text, "signature": sig}));
                }
            }
            Part::RedactedReasoning { data, .. } => content.push(json!({"type": "redacted_thinking", "data": data})),
            Part::ToolCall { id, name, args } => {
                content.push(json!({"type": "tool_use", "id": id, "name": name, "input": args}))
            }
            Part::ToolResult { call_id, content: c, is_error, .. } => {
                // a result with pictures is a list of blocks, the rest a plain string
                let body = if c.iter().any(|p| matches!(p, Part::Image { .. })) {
                    let blocks: Vec<Value> = c
                        .iter()
                        .filter_map(|p| match p {
                            Part::Text { text } => Some(json!({"type": "text", "text": text})),
                            Part::Image { source } => Some(json!({"type": "image", "source": image_source(source)})),
                            _ => None,
                        })
                        .collect();
                    Value::Array(blocks)
                } else {
                    Value::String(nexo_core::message::result_text(c))
                };
                let mut r = json!({"type": "tool_result", "tool_use_id": call_id, "content": body});
                if *is_error {
                    r["is_error"] = json!(true);
                }
                content.push(r);
            }
            Part::Image { source } => content.push(json!({"type": "image", "source": image_source(source)})),
        }
    }
    if content.is_empty() {
        content.push(json!({"type": "text", "text": "(empty)"}));
    }
    (json!({"role": role, "content": content}), dropped)
}

pub(super) fn image_source(s: &ImageSource) -> Value {
    match s {
        ImageSource::Base64 { media_type, data } => json!({"type": "base64", "media_type": media_type, "data": data}),
        ImageSource::Url { url } => json!({"type": "url", "url": url}),
    }
}
