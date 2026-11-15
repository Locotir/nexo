//! Our messages as `input` items.

use nexo_core::{Message, ModelRequest, Part, Role};
use serde_json::{json, Value};

use crate::openai_chat::image_url;

/// The request's messages as input items. Reasoning items are passed back only with the calls of the current
/// turn (since the latest person message): the API drops the ones of earlier turns, so sending them is input
/// for nothing.
pub(super) fn input_items(req: &ModelRequest) -> Vec<Value> {
    let turn = nexo_core::message::latest_person(&req.messages).unwrap_or(0);
    let mut input = Vec::new();
    for (i, m) in req.messages.iter().enumerate() {
        input.extend(convert(m, &req.model, i >= turn));
    }
    pair_reasoning(&mut input);
    input
}

/// A reasoning item is valid only right before the item it led to (an assistant message or a call):
/// one left at the end of the input, before another reasoning item or before anything else is dropped.
/// The other way round needs nothing: a call is sent without an `id`, so it does not name a reasoning
/// item, and goes alone when the curator left its reasoning out.
pub(super) fn pair_reasoning(input: &mut Vec<Value>) {
    let leads = |v: &Value| v["type"] == "function_call" || (v["type"] == "message" && v["role"] == "assistant");
    let keep: Vec<bool> =
        (0..input.len()).map(|i| input[i]["type"] != "reasoning" || input.get(i + 1).is_some_and(leads)).collect();
    let mut k = keep.into_iter();
    input.retain(|_| k.next().unwrap_or(true));
}

/// A message as input items; `echo` says whether its reasoning is passed back (it is, in the current turn).
pub(super) fn convert(m: &Message, model: &str, echo: bool) -> Vec<Value> {
    let mut out = Vec::new();
    match m.role {
        Role::User => {
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
                        // with pictures the output is a list of content items, else a string
                        let output = if c.iter().any(|p| matches!(p, Part::Image { .. })) {
                            let mut items = vec![json!({"type": "input_text", "text": text})];
                            for p in c {
                                if let Part::Image { source } = p {
                                    items.push(json!({"type": "input_image", "image_url": image_url(source)}));
                                }
                            }
                            Value::Array(items)
                        } else {
                            Value::String(text)
                        };
                        out.push(json!({"type": "function_call_output", "call_id": call_id, "output": output}));
                    }
                    Part::Text { text } => content.push(json!({"type": "input_text", "text": text})),
                    Part::Image { source } => {
                        content.push(json!({"type": "input_image", "image_url": image_url(source)}))
                    }
                    _ => {}
                }
            }
            if !content.is_empty() {
                out.push(json!({"type": "message", "role": "user", "content": content}));
            }
        }
        Role::Assistant => {
            for p in &m.content {
                match p {
                    // opaque items of another model are not echoed: this one cannot decrypt them
                    p if p.is_foreign_reasoning(model) => {}
                    // nor are the ones of an earlier turn
                    Part::Reasoning { .. } if !echo => {}
                    // only the opaque item is echoed; its text is for the person
                    Part::Reasoning { signature, encrypted: Some(enc), .. } => {
                        let mut item = json!({"type": "reasoning", "encrypted_content": enc, "summary": []});
                        if let Some(id) = signature {
                            item["id"] = json!(id);
                        }
                        out.push(item);
                    }
                    Part::Text { text } if !text.is_empty() => {
                        out.push(json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}));
                    }
                    Part::ToolCall { id, name, args } => {
                        out.push(json!({"type": "function_call", "call_id": id, "name": name, "arguments": args.to_string()}));
                    }
                    _ => {}
                }
            }
        }
    }
    out
}
