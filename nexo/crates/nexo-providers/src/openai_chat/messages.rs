//! Our messages as the protocol's messages.

use nexo_core::{ImageSource, Message, Part, Role};
use serde_json::{json, Value};

/// One of ours as the protocol's messages (a tool-results message becomes
/// one `tool` message per result).
pub(super) fn convert(m: &Message, send_reasoning: bool) -> Vec<Value> {
    match m.role {
        Role::User => {
            let mut out = Vec::new();
            let mut content = Vec::new();
            for p in &m.content {
                match p {
                    Part::ToolResult { call_id, content: c, is_error, .. } => {
                        let text = nexo_core::message::result_text(c);
                        let text = if *is_error && !text.to_ascii_lowercase().starts_with("error") {
                            format!("Error: {text}")
                        } else {
                            text
                        };
                        out.push(json!({"role": "tool", "tool_call_id": call_id, "content": text}));
                        // a tool message takes text only: its pictures follow in the user message
                        for img in c.iter().filter_map(|p| match p {
                            Part::Image { source } => Some(source),
                            _ => None,
                        }) {
                            content.push(json!({"type": "image_url", "image_url": {"url": image_url(img)}}));
                        }
                    }
                    Part::Text { text } => content.push(json!({"type": "text", "text": text})),
                    Part::Image { source } => {
                        content.push(json!({"type": "image_url", "image_url": {"url": image_url(source)}}))
                    }
                    _ => {}
                }
            }
            if !content.is_empty() {
                let only_text = content.iter().all(|c| c["type"] == "text");
                let content = if only_text {
                    Value::String(content.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n\n"))
                } else {
                    Value::Array(content)
                };
                out.push(json!({"role": "user", "content": content}));
            }
            out
        }
        Role::Assistant => {
            let mut msg = json!({"role": "assistant"});
            let text = m.text();
            let calls: Vec<Value> = m
                .tool_calls()
                .map(|(id, name, args)| json!({"id": id, "type": "function", "function": {"name": name, "arguments": args.to_string()}}))
                .collect();
            // null content is only valid next to tool calls; a message with neither sends ""
            msg["content"] = if !text.is_empty() {
                Value::String(text)
            } else if calls.is_empty() {
                Value::String(String::new())
            } else {
                Value::Null
            };
            if !calls.is_empty() {
                msg["tool_calls"] = Value::Array(calls);
            }
            if send_reasoning {
                let r = m.reasoning();
                if !r.is_empty() {
                    msg["reasoning_content"] = Value::String(r);
                }
            }
            vec![msg]
        }
    }
}

pub(crate) fn image_url(source: &ImageSource) -> String {
    match source {
        ImageSource::Base64 { media_type, data } => format!("data:{media_type};base64,{data}"),
        ImageSource::Url { url } => url.clone(),
    }
}
