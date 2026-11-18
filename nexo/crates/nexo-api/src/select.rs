//! Picking a few values out of a JSON answer (`--select $.price,$.items[*].id`) and comparing them from one poll to
//! the next. A path is the form the rest of the crate uses (`$.a.b[0]`, see [`crate::call::json_get`]) plus the
//! wildcards `[*]` and `.*`, and `['key']` for a key with odd characters. Every pick names the concrete path it came
//! from (`$.items[0].id`), so a value can be followed between polls. The saved form of a pick (the `--state` file)
//! holds only these paths and values.

use serde_json::{json, Value};

/// How many values one pick may return (wildcards can match a whole list).
pub const MAX_PICKED: usize = 100;
/// How many characters of a value a line shows.
const SHOW: usize = 300;
/// The `nexo_api_state` marker of a saved pick.
const STATE_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq)]
enum Seg {
    Key(String),
    Index(usize),
    Any,
}

/// One path: its text and its steps.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    pub text: String,
    segs: Vec<Seg>,
}

fn plain_key(k: &str) -> bool {
    !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl Path {
    /// Reads `$.price`, `$.items[0].id`, `$.items[*].price`, `$.a.*`, `$['odd key']`.
    pub fn parse(text: &str) -> Result<Path, String> {
        let t = text.trim();
        let mut rest =
            t.strip_prefix('$').ok_or_else(|| format!("{t:?} is not a path: it starts with $, like $.price"))?;
        let mut segs = Vec::new();
        while !rest.is_empty() {
            if let Some(r) = rest.strip_prefix('.') {
                let end = r.find(['.', '[']).unwrap_or(r.len());
                let key = &r[..end];
                if key.is_empty() {
                    return Err(format!("{t:?}: a name is missing after a dot"));
                }
                segs.push(if key == "*" { Seg::Any } else { Seg::Key(key.to_string()) });
                rest = &r[end..];
            } else if let Some(r) = rest.strip_prefix('[') {
                let end = r.find(']').ok_or_else(|| format!("{t:?}: a [ has no ]"))?;
                let inner = r[..end].trim();
                let quoted = |q: char| inner.len() >= 2 && inner.starts_with(q) && inner.ends_with(q);
                if inner == "*" {
                    segs.push(Seg::Any);
                } else if let Ok(i) = inner.parse::<usize>() {
                    segs.push(Seg::Index(i));
                } else if quoted('\'') || quoted('"') {
                    segs.push(Seg::Key(inner[1..inner.len() - 1].to_string()));
                } else {
                    return Err(format!("{t:?}: [{inner}] is not an index, * or a quoted key"));
                }
                rest = &r[end + 1..];
            } else {
                return Err(format!("{t:?}: expected . or [ at {rest:?}"));
            }
        }
        if segs.is_empty() {
            return Err("a path must name a field, like $.price (the whole answer is what --select avoids)".to_string());
        }
        Ok(Path { text: t.to_string(), segs })
    }

    pub fn has_wildcard(&self) -> bool {
        self.segs.contains(&Seg::Any)
    }

    /// Whether a concrete path (`$.items[0].id`, as [`pick`] names it) is one this path stands for.
    pub fn matches(&self, concrete: &str) -> bool {
        match Path::parse(concrete) {
            Ok(c) => {
                c.segs.len() == self.segs.len()
                    && self.segs.iter().zip(&c.segs).all(|(a, b)| matches!((a, b), (Seg::Any, _)) || a == b)
            }
            Err(_) => false,
        }
    }
}

/// Splits `--select` arguments at commas (not inside brackets or quotes) and reads each path.
pub fn parse_list(args: &[String]) -> Result<Vec<Path>, String> {
    let mut out: Vec<Path> = Vec::new();
    for arg in args {
        let mut depth = 0usize;
        let mut quote: Option<char> = None;
        let mut cur = String::new();
        let mut parts: Vec<String> = Vec::new();
        for c in arg.chars() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '\'') | (None, '"') => quote = Some(c),
                (None, '[') => depth += 1,
                (None, ']') => depth = depth.saturating_sub(1),
                (None, ',') if depth == 0 => {
                    parts.push(std::mem::take(&mut cur));
                    continue;
                }
                _ => {}
            }
            cur.push(c);
        }
        parts.push(cur);
        for p in parts {
            if p.trim().is_empty() {
                continue;
            }
            let path = Path::parse(&p)?;
            if !out.iter().any(|x| x.text == path.text) {
                out.push(path);
            }
        }
    }
    Ok(out)
}

/// The values at some paths: the concrete path of each, and the value (`None` when nothing is there).
pub type Picked = Vec<(String, Option<Value>)>;

fn key_text(k: &str) -> String {
    if plain_key(k) {
        format!(".{k}")
    } else {
        format!("['{}']", k.replace('\'', ""))
    }
}

fn walk<'a>(v: &'a Value, segs: &[Seg], at: String, out: &mut Vec<(String, &'a Value)>) {
    if out.len() >= MAX_PICKED {
        return;
    }
    let Some((first, rest)) = segs.split_first() else {
        out.push((at, v));
        return;
    };
    match first {
        Seg::Key(k) => {
            if let Some(x) = v.get(k.as_str()) {
                walk(x, rest, format!("{at}{}", key_text(k)), out);
            }
        }
        Seg::Index(i) => {
            if let Some(x) = v.get(*i) {
                walk(x, rest, format!("{at}[{i}]"), out);
            }
        }
        Seg::Any => match v {
            Value::Array(items) => {
                for (i, x) in items.iter().enumerate() {
                    walk(x, rest, format!("{at}[{i}]"), out);
                }
            }
            Value::Object(map) => {
                for (k, x) in map {
                    walk(x, rest, format!("{at}{}", key_text(k)), out);
                }
            }
            _ => {}
        },
    }
}

/// The values of `doc` at `paths`, in the order of the paths. A path that matches nothing gives `(its text, None)`.
pub fn pick(doc: &Value, paths: &[Path]) -> Picked {
    let mut out: Picked = Vec::new();
    for p in paths {
        let mut found = Vec::new();
        walk(doc, &p.segs, "$".to_string(), &mut found);
        if found.is_empty() {
            out.push((p.text.clone(), None));
        }
        for (at, v) in found {
            if out.len() >= MAX_PICKED {
                break;
            }
            out.push((at, Some(v.clone())));
        }
    }
    out
}

/// A value as a line shows it: JSON, cut at 300 characters.
pub fn show(v: &Option<Value>) -> String {
    match v {
        None => "(none)".to_string(),
        Some(v) => {
            let s = v.to_string();
            if s.chars().count() <= SHOW {
                s
            } else {
                let cut: String = s.chars().take(SHOW).collect();
                format!("{cut}…")
            }
        }
    }
}

/// One `path<TAB>value` line per pick.
pub fn lines(p: &Picked) -> String {
    let mut out = String::new();
    for (path, v) in p {
        out.push_str(&format!("{path}\t{}\n", show(v)));
    }
    out
}

/// The picks as a JSON object, `null` for a path that matched nothing.
pub fn object(p: &Picked) -> Value {
    let mut m = serde_json::Map::new();
    for (path, v) in p {
        m.insert(path.clone(), v.clone().unwrap_or(Value::Null));
    }
    Value::Object(m)
}

/// Two values are the same: numbers by value (1 and 1.0 are), the rest as JSON.
pub fn same(a: &Option<Value>, b: &Option<Value>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(Value::Number(x)), Some(Value::Number(y))) => x.as_f64() == y.as_f64(),
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// What differs between two picks: the path, the old and the new value.
pub fn changes(old: &Picked, new: &Picked) -> Vec<(String, Option<Value>, Option<Value>)> {
    let mut out = Vec::new();
    for (path, v) in new {
        let was = old.iter().find(|(p, _)| p == path).and_then(|(_, x)| x.clone());
        if !same(&was, v) {
            out.push((path.clone(), was, v.clone()));
        }
    }
    for (path, v) in old {
        if v.is_some() && !new.iter().any(|(p, _)| p == path) {
            out.push((path.clone(), v.clone(), None));
        }
    }
    out
}

// ---------------------------------------------------------------- the state file

/// The text of a state file: the select list and what was picked, nothing else (no header, no body).
pub fn state_text(select: &[String], p: &Picked) -> String {
    let values: Vec<Value> = p
        .iter()
        .map(|(path, v)| match v {
            Some(v) => json!({ "path": path, "value": v }),
            None => json!({ "path": path }),
        })
        .collect();
    let doc = json!({ "nexo_api_state": STATE_VERSION, "select": select, "values": values });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

/// The picks a state file holds, when it is one and was written for this select list (else `None`: a first poll).
pub fn read_state(text: &str, select: &[String]) -> Option<Picked> {
    let doc: Value = serde_json::from_str(text).ok()?;
    if doc.get("nexo_api_state")?.as_u64()? != STATE_VERSION {
        return None;
    }
    let saved: Vec<&str> = doc.get("select")?.as_array()?.iter().filter_map(Value::as_str).collect();
    if saved.len() != select.len() || !saved.iter().zip(select).all(|(a, b)| *a == b.as_str()) {
        return None;
    }
    let mut out = Vec::new();
    for e in doc.get("values")?.as_array()? {
        out.push((e.get("path")?.as_str()?.to_string(), e.get("value").cloned()));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Value {
        json!({"price": 12.5, "name": "Lamp", "items": [{"id": 1, "price": 3}, {"id": 2, "price": 4}], "odd key": 7})
    }

    fn paths(list: &[&str]) -> Vec<Path> {
        parse_list(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn paths_pick_fields_indexes_and_wildcards() {
        let p = pick(&doc(), &paths(&["$.price", "$.items[0].id", "$.items[*].price", "$.nope", "$['odd key']"]));
        let names: Vec<&str> = p.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["$.price", "$.items[0].id", "$.items[0].price", "$.items[1].price", "$.nope", "$['odd key']"]
        );
        assert_eq!(p[0].1, Some(json!(12.5)));
        assert_eq!(p[3].1, Some(json!(4)));
        assert_eq!(p[4].1, None);
        assert_eq!(p[5].1, Some(json!(7)));
        assert_eq!(lines(&p).lines().next().unwrap(), "$.price\t12.5");
        assert!(lines(&p).contains("$.nope\t(none)\n"));
        assert_eq!(object(&p)["$.nope"], Value::Null);
    }

    #[test]
    fn a_list_is_split_at_commas_outside_brackets() {
        let p = paths(&["$.a,$.b[0]", "$['x,y']", "$.a"]);
        let texts: Vec<&str> = p.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(texts, ["$.a", "$.b[0]", "$['x,y']"]);
        assert!(parse_list(&["price".to_string()]).is_err());
        assert!(parse_list(&["$".to_string()]).is_err());
        assert!(parse_list(&["$.a[".to_string()]).is_err());
    }

    #[test]
    fn a_concrete_path_is_matched_by_its_pattern() {
        let p = Path::parse("$.items[*].price").unwrap();
        assert!(p.matches("$.items[1].price") && !p.matches("$.items[1].id") && !p.matches("$.items.price"));
        assert!(p.has_wildcard());
    }

    #[test]
    fn changes_name_old_and_new_values_and_ignore_the_rest() {
        let a = pick(&doc(), &paths(&["$.price", "$.items[*].price"]));
        let mut d2 = doc();
        d2["name"] = json!("Other");
        assert!(changes(&a, &pick(&d2, &paths(&["$.price", "$.items[*].price"]))).is_empty());
        d2["price"] = json!(12);
        d2["items"].as_array_mut().unwrap().pop();
        let c = changes(&a, &pick(&d2, &paths(&["$.price", "$.items[*].price"])));
        assert_eq!(c.len(), 2, "{c:?}");
        assert_eq!(c[0], ("$.price".to_string(), Some(json!(12.5)), Some(json!(12))));
        assert_eq!(c[1], ("$.items[1].price".to_string(), Some(json!(4)), None));
    }

    #[test]
    fn the_state_round_trips_and_holds_only_picked_values() {
        let sel = vec!["$.price".to_string(), "$.nope".to_string()];
        let mut d = json!({"price": 12.5, "authorization": "Bearer abcdefabcdef"});
        crate::exchange::scrub_json(&mut d);
        let p = pick(&d, &paths(&["$.price", "$.nope"]));
        let text = state_text(&sel, &p);
        assert!(!text.contains("Bearer") && !text.contains("authorization"), "{text}");
        assert_eq!(read_state(&text, &sel), Some(p));
        assert_eq!(read_state(&text, &["$.other".to_string()]), None);
        assert_eq!(read_state("{\"status\":200}", &sel), None);
        assert_eq!(read_state("nope", &sel), None);
    }

    #[test]
    fn a_secret_field_is_picked_masked() {
        let mut d = json!({"access_token": "tkAAAA1111bbbbCCCC", "price": 1});
        crate::exchange::scrub_json(&mut d);
        let p = pick(&d, &paths(&["$.access_token"]));
        assert_eq!(p[0].1, Some(json!("<secret>")));
    }
}
