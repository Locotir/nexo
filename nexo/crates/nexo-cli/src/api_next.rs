//! What a `nexo api call` answer makes possible: at most three ready-to-run next commands with the value filled in
//! (from the scrubbed answer, so never a secret; a secret link says where the secret comes from instead).

use crate::api_call::Out;
use nexo_api::exchange;
use nexo_api::links::{self, Kind, Next, MAX_NEXT};
use nexo_api::spec::Api;
use serde_json::{json, Value};
use std::path::Path;

/// A word the shell takes as one argument.
fn quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// A name from the capture as one line of plain text (it ends up in a comment or a sentence, never in a command).
fn line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

fn name_of(api: &Api, id: &str) -> String {
    api.endpoints
        .iter()
        .find(|e| e.id == id)
        .map(|e| format!("{} {}", e.method, e.path))
        .unwrap_or_else(|| id.to_string())
}

/// `nexo api call MAP ENDPOINT -p name=value`, or for an auth link where the secret comes from.
fn command(api: &Api, map: &Path, n: &Next) -> String {
    if n.kind == Kind::Auth {
        return line(&format!("auth from {} ({}) for {}", name_of(api, &n.source), n.from, n.to.join(", ")));
    }
    let mut c = format!("nexo api call {} {}", quote(&map.display().to_string()), quote(&n.to[0]));
    for (name, value) in &n.params {
        c.push_str(&format!(" -p {}", quote(&format!("{name}={value}"))));
    }
    c
}

/// Adds the next steps to the answer of a call: lines after the text, or a `next` array in the JSON.
pub fn add(out: &mut Out, api: &Api, ep: &str, map: &Path, body: &str, location: Option<&str>, as_json: bool) {
    if out.code != 0 {
        return;
    }
    let mut v = serde_json::from_str::<Value>(body).ok();
    if let Some(v) = v.as_mut() {
        exchange::scrub_json(v);
    }
    let loc = location.map(exchange::clean_url);
    let steps = links::next(api, ep, v.as_ref(), loc.as_deref());
    let shown: Vec<&Next> = steps.list.iter().take(MAX_NEXT).collect();
    let left = steps.list.len().saturating_sub(MAX_NEXT) + steps.skipped;
    if as_json {
        let list: Vec<Value> = shown
            .iter()
            .map(|n| json!({ "kind": n.kind.name(), "from": n.from, "command": command(api, map, n) }))
            .collect();
        if let Ok(Value::Object(mut o)) = serde_json::from_str::<Value>(&out.text) {
            o.insert("next".to_string(), Value::Array(list));
            if left > 0 {
                o.insert("next_ignored".to_string(), json!(left));
            }
            out.text = format!("{}\n", serde_json::to_string_pretty(&Value::Object(o)).unwrap_or_default());
        }
        return;
    }
    if shown.is_empty() && left == 0 {
        return;
    }
    out.text.push_str("next:\n");
    for n in &shown {
        out.text.push_str(&format!("  {}   # {}\n", command(api, map, n), line(&n.from)));
    }
    if left > 0 {
        out.text.push_str(&format!("  ({left} more links not used: no value in this answer, or past {MAX_NEXT})\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_with_spaces_or_quotes_are_quoted_and_plain_ones_are_not() {
        assert_eq!(quote("item_id=42"), "item_id=42");
        assert_eq!(quote("name=a b"), "'name=a b'");
        assert_eq!(quote("x=it's"), "'x=it'\\''s'");
        assert_eq!(quote(""), "''");
    }
}
