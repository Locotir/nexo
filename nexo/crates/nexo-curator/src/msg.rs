//! The Messages API shape: content blocks, their text, and the truth value of JSON values.

use std::borrow::Cow;

use serde_json::{Map, Value};

use crate::json::float_repr;
use crate::textops::repr_str;

/// `{"type": "text", "text": s}`
/// The line before the person's own words when the harness put anything in their message (`nexo_core::agent::PERSON_MARK`).
pub const PERSON_MARK: &str = "[the person's message]";

/// The person's own words in the text of a message they sent: what follows the last [`PERSON_MARK`] line, else the
/// whole text.
pub fn person_words(text: &str) -> &str {
    let line = format!("\n{PERSON_MARK}\n");
    match text.rfind(&line) {
        Some(i) => &text[i + line.len()..],
        None => text.strip_prefix(PERSON_MARK).and_then(|r| r.strip_prefix('\n')).unwrap_or(text),
    }
}

/// What follows the time in the line nexo puts first in a request after the person stopped the one before it
/// (`nexo_core::agent::STOP_WORDS`).
const STOP_WORDS: &str = "the person stopped the request above";

/// The time and the rest of a line `[HH:MM:SS...]`.
fn timed(line: &str) -> Option<(&str, &str)> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    let (t, rest) = inner.split_at_checked(8)?;
    let b = t.as_bytes();
    let hms = b[2] == b':' && b[5] == b':' && [0, 1, 3, 4, 6, 7].iter().all(|&i| b[i].is_ascii_digit());
    hms.then_some((t, rest))
}

/// The line that says the person stopped the request before, when `text` starts with one, and its time
/// (`nexo_core::agent::stop_line_of`).
pub fn stop_line_of(text: &str) -> Option<(&str, &str)> {
    let first = text.split('\n').next()?;
    let (at, rest) = timed(first)?;
    rest.strip_prefix(' ')?.starts_with(STOP_WORDS).then_some((first, at))
}

/// What a part's header adds when the person stopped the request it was typed during
/// (`nexo_core::agent::BEFORE_STOP`).
const BEFORE_STOP: &str = "before the person stopped it";

/// Whether `line` heads one part of requests nexo joined into one (`nexo_core::agent::part_header`).
fn is_part_header(line: &str) -> bool {
    timed(line).is_some_and(|(_, rest)| rest.is_empty() || rest.starts_with(", typed while"))
}

/// The parts of the person's words when nexo joined requests typed one after another into one, oldest first, each
/// with the line that says when it was typed; None for one request.
fn headed_parts(words: &str) -> Option<Vec<(&str, &str)>> {
    let mut lines = words.split_inclusive('\n');
    let first = lines.next()?;
    let mut header = first.trim_end_matches(['\n', '\r']);
    if !is_part_header(header) {
        return None;
    }
    let mut parts = Vec::new();
    let (mut start, mut at) = (first.len(), first.len());
    for line in lines {
        let bare = line.trim_end_matches(['\n', '\r']);
        if is_part_header(bare) {
            parts.push((header, words[start..at].trim_matches('\n')));
            (header, start) = (bare, at + line.len());
        }
        at += line.len();
    }
    parts.push((header, words[start..].trim_matches('\n')));
    Some(parts)
}

/// The parts of the person's words when nexo joined requests typed one after another into one, oldest first, each
/// without the line that says when it was typed; None for one request (`nexo_core::agent::joined_parts`).
pub fn joined_parts(words: &str) -> Option<Vec<&str>> {
    headed_parts(words).map(|parts| parts.into_iter().map(|(_, part)| part).collect())
}

/// What the person asks now in the text of a message they sent: their words ([`person_words`]) without the line that
/// says they stopped the request before, and of requests joined into one those still asked. A part typed while the
/// model worked on a request the person then stopped, followed by one typed after the stop, was asked of the stopped
/// request: the part after the stop is what they want now. Parts queued while the model worked, with no stop between
/// them and now (or a stop made to run them at once), are each asked now, all of them.
pub fn request_words(text: &str) -> Cow<'_, str> {
    let words = person_words(text);
    let words = stop_line_of(words).map_or(words, |(line, _)| words[line.len()..].trim_start());
    let Some(parts) = headed_parts(words) else { return Cow::Borrowed(words) };
    let after: Vec<&str> = parts.iter().filter(|(h, _)| !h.contains(BEFORE_STOP)).map(|&(_, p)| p).collect();
    let asked = if after.is_empty() { parts.iter().map(|&(_, p)| p).collect() } else { after };
    match asked.as_slice() {
        [one] => Cow::Borrowed(one),
        _ => Cow::Owned(asked.join("\n\n")),
    }
}

/// Whether nexo marked the text of a message: it starts with the line of a stop, or the person's words are requests
/// joined into one. Its request is then [`request_words`], not the whole text.
pub fn is_marked(text: &str) -> bool {
    stop_line_of(text).is_some() || joined_parts(person_words(text)).is_some()
}

/// The request in the whole text of a message the person sent, as the curator reads it: [`request_words`] when nexo
/// marked the message, else the text as it is.
pub fn asked_now(text: &str) -> Cow<'_, str> {
    if is_marked(text) {
        request_words(text)
    } else {
        Cow::Borrowed(text)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    const DURING: &str = "[16:13:05, typed while you worked on the request above]";
    const DURING_LATER: &str = "[16:13:07, typed while you worked on the request above]";
    const STOPPED_HEADER: &str =
        "[16:13:05, typed while you worked on the request above, before the person stopped it]";

    #[test]
    fn requests_queued_while_the_model_worked_are_all_asked_now_without_their_headers() {
        let text = format!("{DURING}\nfix parse in lexer.rs\n\n{DURING_LATER}\nand update the README");
        assert_eq!(request_words(&text), "fix parse in lexer.rs\n\nand update the README");
        assert_eq!(asked_now(&text), "fix parse in lexer.rs\n\nand update the README");
    }

    #[test]
    fn a_part_typed_before_a_stop_gives_way_to_the_one_typed_after_it() {
        let stop = "[16:13:08 the person stopped the request above (Esc) while you waited for your reply; it got no \
                    answer]";
        let words = format!("{STOPPED_HEADER}\ny documenta cada función\n\n[16:13:10]\nDetente, cambiamos de tema");
        assert_eq!(request_words(&format!("{stop}\n\n{PERSON_MARK}\n{words}")), "Detente, cambiamos de tema");
        assert_eq!(request_words(&words), "Detente, cambiamos de tema");
    }

    #[test]
    fn parts_stopped_to_run_them_at_once_are_all_asked_now() {
        let later = STOPPED_HEADER.replace("16:13:05", "16:13:07");
        let text = format!("{STOPPED_HEADER}\nfix parse in lexer.rs\n\n{later}\nand update the README");
        assert_eq!(request_words(&text), "fix parse in lexer.rs\n\nand update the README");
    }

    #[test]
    fn one_request_is_read_as_it_is() {
        assert_eq!(request_words("[12:00:00 is when it broke]\nfix it"), "[12:00:00 is when it broke]\nfix it");
        assert!(matches!(request_words("fix it"), Cow::Borrowed("fix it")));
    }
}
