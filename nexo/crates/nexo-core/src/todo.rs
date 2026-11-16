//! The todo list of a request, as its last `todo` call left it.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::message::{Message, Part};
use serde_json::Value;

/// The todo list of a request as its last `todo` call left it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Todo {
    pub done: usize,
    pub doing: usize,
    pub pending: usize,
    pub doing_text: Option<String>,
    pub hash: u64,
}

/// The last `todo` call in `history`, if it listed items.
pub fn todo_of(history: &[Message]) -> Option<Todo> {
    let items = history.iter().rev().flat_map(|m| m.content.iter().rev()).find_map(|p| match p {
        Part::ToolCall { name, args, .. } if name == "todo" => args.get("items").and_then(Value::as_array),
        _ => None,
    })?;
    let mut t = Todo::default();
    let mut h = DefaultHasher::new();
    for i in items {
        let status = i.get("status").and_then(Value::as_str).unwrap_or("pending").trim().to_ascii_lowercase();
        let text = i.get("text").and_then(Value::as_str).unwrap_or("").trim().to_string();
        (&status, &text).hash(&mut h);
        match status.as_str() {
            "done" | "completed" | "complete" => t.done += 1,
            "in_progress" | "in-progress" | "doing" | "active" => {
                t.doing += 1;
                t.doing_text.get_or_insert(text);
            }
            _ => t.pending += 1,
        }
    }
    t.hash = h.finish();
    Some(t)
}
