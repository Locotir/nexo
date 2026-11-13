use super::*;

// ------------------------------------------------------------ messages

pub(super) fn has_cache_control(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.contains_key("cache_control") || m.values().any(has_cache_control),
        Value::Array(a) => a.iter().any(has_cache_control),
        _ => false,
    }
}

pub(super) fn strip_cache_control(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter().filter(|(k, _)| *k != "cache_control").map(|(k, v)| (k.clone(), strip_cache_control(v))).collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(strip_cache_control).collect()),
        v => v.clone(),
    }
}

/// Content as a list of blocks (a plain string becomes one text block).
pub(super) fn blocks(content: Option<&Value>) -> Vec<Cow<'_, Value>> {
    match content {
        Some(Value::String(s)) => vec![Cow::Owned(serde_json::json!({"type": "text", "text": s}))],
        Some(Value::Array(a)) => a.iter().map(Cow::Borrowed).collect(),
        _ => vec![],
    }
}

pub(super) fn str_of<'a>(o: &'a Map<String, Value>, k: &str) -> &'a str {
    o.get(k).and_then(Value::as_str).unwrap_or("")
}

pub(super) fn type_of(v: &Value) -> Option<&str> {
    v.get("type").and_then(Value::as_str)
}

/// The text of a content value (string, block list, nested tool results).
pub(super) fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => {
            let mut out: Vec<Cow<str>> = vec![];
            for b in a.iter().filter_map(Value::as_object) {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => out.push(Cow::Borrowed(str_of(b, "text"))),
                    Some("tool_result") => out.push(Cow::Owned(text_of(b.get("content")))),
                    Some(t @ ("image" | "document")) => out.push(Cow::Owned(format!("[{t}]"))),
                    _ => {}
                }
            }
            out.join("\n")
        }
        _ => String::new(),
    }
}

pub(super) const SKIP_KEYS: &[&str] =
    &["type", "id", "tool_use_id", "signature", "data", "cache_control", "source", "is_error", "citations"];

pub(super) fn strings<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Object(m) => {
            for (k, v) in m {
                if !SKIP_KEYS.contains(&k.as_str()) {
                    strings(v, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
        _ => {}
    }
}

/// All the text of a value, without ids, signatures and other metadata.
pub(super) fn flat(v: Option<&Value>) -> String {
    let mut out = vec![];
    if let Some(v) = v {
        strings(v, &mut out);
    }
    out.join("\n")
}

// ------------------------------------------------------------ token estimate

/// Length of a string in canonical JSON (non-ASCII kept as is).
pub(super) fn json_str_len(s: &str) -> usize {
    2 + s
        .chars()
        .map(|c| match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            c if (c as u32) < 0x20 => 6,
            _ => 1,
        })
        .sum::<usize>()
}

pub(super) fn json_obj_len(entries: impl Iterator<Item = (usize, usize)>) -> usize {
    let (mut n, mut len) = (0usize, 2);
    for (k, v) in entries {
        len += k + 1 + v;
        n += 1;
    }
    len + n.saturating_sub(1)
}

/// Length of a value in canonical JSON (sorted keys, no spaces), `cache_control` left out.
pub(super) fn json_len(v: &Value) -> usize {
    match v {
        Value::Null => 4,
        Value::Bool(b) => {
            if *b {
                4
            } else {
                5
            }
        }
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string().len(),
            (_, Some(u)) => u.to_string().len(),
            _ => py::float_repr(n.as_f64().unwrap_or(0.0)).len(),
        },
        Value::String(s) => json_str_len(s),
        Value::Array(a) => 2 + a.iter().map(json_len).sum::<usize>() + a.len().saturating_sub(1),
        Value::Object(m) => {
            json_obj_len(m.iter().filter(|(k, _)| *k != "cache_control").map(|(k, v)| (json_str_len(k), json_len(v))))
        }
    }
}

/// `json_len` of the value without base64 payloads, signatures and reasoning text; and its media blocks.
pub(super) fn stripped_len(v: &Value, media: &mut usize) -> usize {
    match v {
        Value::Object(m) => {
            let ty = m.get("type").and_then(Value::as_str);
            if let (Some("image" | "document"), Some(Value::Object(src))) = (ty, m.get("source")) {
                *media += 1;
                let source = json_obj_len(
                    src.iter()
                        .filter(|(k, _)| *k != "data" && *k != "cache_control")
                        .map(|(k, v)| (json_str_len(k), json_len(v))),
                );
                return json_obj_len(
                    m.iter()
                        .filter(|(k, _)| *k != "cache_control")
                        .map(|(k, v)| (json_str_len(k), if k == "source" { source } else { json_len(v) })),
                );
            }
            if let Some(t @ ("thinking" | "redacted_thinking")) = ty {
                return json_obj_len(std::iter::once((json_str_len("type"), json_str_len(t))));
            }
            json_obj_len(
                m.iter()
                    .filter(|(k, _)| *k != "signature" && *k != "cache_control")
                    .map(|(k, v)| (json_str_len(k), stripped_len(v, media)))
                    .collect::<Vec<_>>()
                    .into_iter(),
            )
        }
        Value::Array(a) => 2 + a.iter().map(|v| stripped_len(v, media)).sum::<usize>() + a.len().saturating_sub(1),
        v => json_len(v),
    }
}

/// Estimated tokens of a block (~4 characters each, images and documents at a nominal size).
pub(super) fn est(b: &Value) -> usize {
    let mut media = 0;
    let len = stripped_len(b, &mut media);
    len / 4 + MEDIA_TOKENS * media
}
