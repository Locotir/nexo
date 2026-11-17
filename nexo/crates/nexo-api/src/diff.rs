//! What changed between two JSON answers of one endpoint (`nexo api watch`): the paths that were added, removed or
//! changed, with the old and new values. An array of objects that share an id-like field is matched by that field
//! (`$.items[id=3].name`), so a list that is reordered or grows at the front shows only what really differs; any other
//! array is compared by position.

use serde_json::Value;
use std::collections::HashSet;

/// What happened at one path.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Added(Value),
    Removed(Value),
    Changed(Value, Value),
}

/// One difference.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// `$.items[id=3].name`.
    pub path: String,
    pub kind: Kind,
}

/// The field names that identify the items of a list, in the order they are tried (compared in lower case).
const ID_KEYS: [&str; 7] = ["id", "_id", "uuid", "guid", "key", "slug", "code"];

/// The longest value a line shows, in characters.
const SHOW: usize = 80;

fn key_path(parent: &str, key: &str) -> String {
    let plain = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if plain {
        format!("{parent}.{key}")
    } else {
        format!("{parent}[{}]", Value::String(key.to_string()))
    }
}

/// The text an id value is shown as in a path; `None` for anything but a string or a number.
fn id_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The key of an object, matched without regard to case.
fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object()?.iter().find(|(k, _)| k.to_ascii_lowercase() == key).map(|(_, v)| v)
}

/// The first id-like field that every item of both lists has, with a distinct scalar value in each list.
fn id_key(old: &[Value], new: &[Value]) -> Option<&'static str> {
    if old.is_empty() && new.is_empty() {
        return None;
    }
    ID_KEYS.iter().copied().find(|key| {
        [old, new].iter().all(|list| {
            let mut seen = HashSet::new();
            list.iter().all(|item| field(item, key).and_then(id_text).map_or(false, |t| seen.insert(t)))
        })
    })
}

fn walk(path: &str, old: &Value, new: &Value, out: &mut Vec<Change>) {
    if old == new {
        return;
    }
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in a {
                match b.get(k) {
                    Some(w) => walk(&key_path(path, k), v, w, out),
                    None => out.push(Change { path: key_path(path, k), kind: Kind::Removed(v.clone()) }),
                }
            }
            for (k, w) in b {
                if !a.contains_key(k) {
                    out.push(Change { path: key_path(path, k), kind: Kind::Added(w.clone()) });
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => match id_key(a, b) {
            Some(key) => {
                let id = |v: &Value| field(v, key).and_then(id_text).unwrap_or_default();
                let label = |v: &Value| format!("{path}[{key}={}]", id(v));
                for v in a {
                    match b.iter().find(|w| id(w) == id(v)) {
                        Some(w) => walk(&label(v), v, w, out),
                        None => out.push(Change { path: label(v), kind: Kind::Removed(v.clone()) }),
                    }
                }
                for w in b {
                    if !a.iter().any(|v| id(v) == id(w)) {
                        out.push(Change { path: label(w), kind: Kind::Added(w.clone()) });
                    }
                }
            }
            None => {
                for i in 0..a.len().max(b.len()) {
                    let here = format!("{path}[{i}]");
                    match (a.get(i), b.get(i)) {
                        (Some(v), Some(w)) => walk(&here, v, w, out),
                        (Some(v), None) => out.push(Change { path: here, kind: Kind::Removed(v.clone()) }),
                        (None, Some(w)) => out.push(Change { path: here, kind: Kind::Added(w.clone()) }),
                        (None, None) => {}
                    }
                }
            }
        },
        _ => out.push(Change { path: path.to_string(), kind: Kind::Changed(old.clone(), new.clone()) }),
    }
}

/// Every difference between two answers, paths rooted at `$`.
pub fn diff(old: &Value, new: &Value) -> Vec<Change> {
    let mut out = Vec::new();
    walk("$", old, new, &mut out);
    out
}

fn show(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() <= SHOW {
        return s;
    }
    let cut: String = s.chars().take(SHOW).collect();
    format!("{cut}…")
}

/// One line: `+ $.x = 1`, `- $.x (was 1)`, `~ $.x: 1 -> 2`.
pub fn render(c: &Change) -> String {
    match &c.kind {
        Kind::Added(v) => format!("+ {} = {}", c.path, show(v)),
        Kind::Removed(v) => format!("- {} (was {})", c.path, show(v)),
        Kind::Changed(a, b) => format!("~ {}: {} -> {}", c.path, show(a), show(b)),
    }
}

/// The lines of the first `max` changes, and a last line that counts the rest.
pub fn render_all(changes: &[Change], max: usize) -> String {
    let mut out = String::new();
    for c in changes.iter().take(max) {
        out.push_str(&render(c));
        out.push('\n');
    }
    if changes.len() > max {
        out.push_str(&format!("… {} more changes\n", changes.len() - max));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines(old: Value, new: Value) -> Vec<String> {
        diff(&old, &new).iter().map(render).collect()
    }

    #[test]
    fn equal_answers_have_no_changes() {
        assert!(diff(&json!({"a": [1, {"b": 2}]}), &json!({"a": [1, {"b": 2}]})).is_empty());
    }

    #[test]
    fn added_removed_and_changed_fields_are_told_with_their_values() {
        let l =
            lines(json!({"a": 1, "gone": "x", "n": {"deep": true}}), json!({"a": 2, "new": [1], "n": {"deep": false}}));
        assert_eq!(
            l,
            vec![
                "~ $.a: 1 -> 2".to_string(),
                "- $.gone (was \"x\")".to_string(),
                "~ $.n.deep: true -> false".to_string(),
                "+ $.new = [1]".to_string(),
            ]
        );
    }

    #[test]
    fn a_change_of_type_is_a_change() {
        assert_eq!(lines(json!({"a": 1}), json!({"a": "1"})), vec!["~ $.a: 1 -> \"1\"".to_string()]);
    }

    #[test]
    fn lists_of_objects_are_matched_by_their_id() {
        let old = json!({"items": [{"id": 1, "n": "a"}, {"id": 2, "n": "b"}, {"id": 3, "n": "c"}]});
        let new = json!({"items": [{"id": 4, "n": "d"}, {"id": 3, "n": "c"}, {"id": 1, "n": "A"}]});
        assert_eq!(
            lines(old, new),
            vec![
                "~ $.items[id=1].n: \"a\" -> \"A\"".to_string(),
                "- $.items[id=2] (was {\"id\":2,\"n\":\"b\"})".to_string(),
                "+ $.items[id=4] = {\"id\":4,\"n\":\"d\"}".to_string(),
            ]
        );
    }

    #[test]
    fn a_list_with_repeated_or_missing_ids_is_compared_by_position() {
        let l = lines(json!([{"id": 1}, {"id": 1}]), json!([{"id": 1}, {"id": 2}, 7]));
        assert_eq!(l, vec!["~ $[1].id: 1 -> 2".to_string(), "+ $[2] = 7".to_string()]);
        let l = lines(json!({"t": [1, 2, 3]}), json!({"t": [1, 2]}));
        assert_eq!(l, vec!["- $.t[2] (was 3)".to_string()]);
    }

    #[test]
    fn an_empty_list_that_fills_shows_each_item_by_id() {
        let l = lines(json!({"x": []}), json!({"x": [{"id": "a"}]}));
        assert_eq!(l, vec!["+ $.x[id=a] = {\"id\":\"a\"}".to_string()]);
    }

    #[test]
    fn odd_keys_are_quoted_and_long_values_cut_and_extra_changes_counted() {
        let l = lines(json!({"a b": "x"}), json!({"a b": "y".repeat(200)}));
        assert!(l[0].starts_with("~ $[\"a b\"]: \"x\" -> \"yyy") && l[0].ends_with('…'), "{l:?}");
        let many: Vec<Change> =
            (0..5).map(|i| Change { path: format!("$.k{i}"), kind: Kind::Added(json!(i)) }).collect();
        let out = render_all(&many, 3);
        assert_eq!(out.lines().count(), 4);
        assert!(out.ends_with("… 2 more changes\n"));
    }
}
