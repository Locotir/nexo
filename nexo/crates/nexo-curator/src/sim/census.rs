//! A census of a message list by kind: where the tokens of a view (or of the plain history) are.

use super::price::tokens_of;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Tools whose input writes or edits a file (nexo's and the fixtures' names, any case).
pub const WRITE_TOOLS: &[&str] =
    &["edit_file", "write_file", "apply_patch", "multi_edit", "multiedit", "edit", "write"];

/// The kind of one content block of a message sent by `role`.
pub fn kind_of(role: &str, block: &Value) -> &'static str {
    let text = |v: Option<&Value>| match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.get("text").and_then(Value::as_str)).collect::<String>(),
        _ => String::new(),
    };
    let made = |s: &str| s.trim_start().starts_with(crate::mechanical::MARK);
    match block.get("type").and_then(Value::as_str) {
        Some("text") if made(&text(block.get("text"))) => "curator",
        Some("text") if role == "user" => "person",
        Some("text") => "reply text",
        Some("tool_use") => {
            let name = block.get("name").and_then(Value::as_str).unwrap_or("").to_lowercase();
            if WRITE_TOOLS.contains(&name.as_str()) {
                "write args"
            } else {
                "other args"
            }
        }
        Some("tool_result") if made(&text(block.get("content"))) => "curator",
        Some("tool_result") => "results",
        Some("thinking" | "redacted_thinking") => "reasoning",
        _ => "other",
    }
}

/// Adds the tokens of `msgs` to `acc`, by kind.
pub fn add(acc: &mut BTreeMap<&'static str, i64>, msgs: &[Value]) {
    for m in msgs {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("");
        let blocks = match m.get("content") {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::String(s)) => vec![json!({"type": "text", "text": s})],
            _ => Vec::new(),
        };
        for b in blocks {
            *acc.entry(kind_of(role, &b)).or_insert(0) += tokens_of(&json!({"content": [b.clone()]}));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        let k = |r, b: Value| kind_of(r, &b);
        assert_eq!(k("assistant", json!({"type": "tool_use", "name": "edit_file", "input": {}})), "write args");
        assert_eq!(k("assistant", json!({"type": "tool_use", "name": "Write", "input": {}})), "write args");
        assert_eq!(k("assistant", json!({"type": "tool_use", "name": "read_file", "input": {}})), "other args");
        assert_eq!(k("user", json!({"type": "tool_result", "content": "x"})), "results");
        assert_eq!(k("assistant", json!({"type": "thinking", "thinking": "x"})), "reasoning");
        assert_eq!(k("user", json!({"type": "text", "text": "hi"})), "person");
        assert_eq!(k("assistant", json!({"type": "text", "text": "hi"})), "reply text");
        assert_eq!(k("user", json!({"type": "text", "text": "[curator] the map"})), "curator");
        let mut acc = BTreeMap::new();
        add(&mut acc, &[json!({"role": "user", "content": "hello there"})]);
        assert!(acc["person"] > 0);
    }
}
