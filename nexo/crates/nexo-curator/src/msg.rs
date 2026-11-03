//! The Messages API shape: content blocks, their text, and the truth value of JSON values.

use std::borrow::Cow;

use serde_json::{Map, Value};

use crate::json::float_repr;
use crate::py::repr_str;

/// `{"type": "text", "text": s}`
pub fn text_block(s: &str) -> Value {
    let mut m = Map::new();
    m.insert("type".into(), Value::String("text".into()));
    m.insert("text".into(), Value::String(s.into()));
    Value::Object(m)
}

/// Content as a list of blocks (a plain string becomes one text block).
pub fn blocks(content: Option<&Value>) -> Cow<'_, [Value]> {
    match content {
        Some(Value::String(s)) => Cow::Owned(vec![text_block(s)]),
        Some(Value::Array(a)) => Cow::Borrowed(a.as_slice()),
        // `list(dict)` is its keys
        Some(Value::Object(m)) => Cow::Owned(m.keys().map(|k| Value::String(k.clone())).collect()),
        _ => Cow::Owned(Vec::new()),
    }
}

/// `b.get(key)` as a str, "" when missing or not a string.
pub fn get_str<'a>(b: &'a Value, key: &str) -> &'a str {
    b.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The text of a content value (string, block list, nested tool results).
pub fn text_of(content: Option<&Value>) -> Cow<'_, str> {
    match content {
        Some(Value::String(s)) => Cow::Borrowed(s.as_str()),
        Some(Value::Array(a)) => {
            let mut out: Vec<Cow<'_, str>> = Vec::new();
            for b in a {
                if !b.is_object() {
                    continue;
                }
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => out.push(Cow::Borrowed(get_str(b, "text"))),
                    Some("tool_result") => out.push(text_of(b.get("content"))),
                    Some(t @ ("image" | "document")) => out.push(Cow::Owned(format!("[{t}]"))),
                    _ => {}
                }
            }
            if out.len() == 1 {
                return out.pop().unwrap_or(Cow::Borrowed(""));
            }
            Cow::Owned(out.join("\n"))
        }
        _ => Cow::Borrowed(""),
    }
}

const SKIP_KEYS: [&str; 9] =
    ["type", "id", "tool_use_id", "signature", "data", "cache_control", "source", "is_error", "citations"];

fn strings<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Object(m) => {
            for (k, x) in m {
                if !SKIP_KEYS.contains(&k.as_str()) {
                    strings(x, out);
                }
            }
        }
        Value::Array(a) => {
            for x in a {
                strings(x, out);
            }
        }
        _ => {}
    }
}

/// All the text in a value, without ids, signatures and other metadata.
pub fn flat(v: Option<&Value>) -> String {
    let mut out = Vec::new();
    if let Some(v) = v {
        strings(v, &mut out);
    }
    out.join("\n")
}

/// The truth value of a JSON value: absent, null, false, 0, "" and empty arrays and maps are false.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
    }
}

/// `repr` of a decoded JSON value.
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => repr_str(s),
        _ => py_str(v),
    }
}

/// `str` of a decoded JSON value.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                float_repr(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => s.clone(),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter().map(|(k, x)| format!("{}: {}", repr_str(k), py_repr(x))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// A hashable key for a JSON value used as a dict key (`b.get("id")`): missing is `None`.
pub fn key_of(v: Option<&Value>) -> String {
    match v {
        None => "null".into(),
        Some(v) => v.to_string(),
    }
}
