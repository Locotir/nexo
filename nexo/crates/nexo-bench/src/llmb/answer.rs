//! The answer in a model's final text: the distribution's own reading rules (`parse_answer` of
//! its scoring module, policy `unique_scoreable_json_v2`), and the stricter reading its
//! canonical-step track applies to each reply (`plain_json`).

use std::collections::BTreeSet;
use std::fmt;
use std::sync::OnceLock;

use regex::Regex;
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

/// How the answer was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// The whole text is one JSON object (perhaps in a ```json fence).
    Strict,
    /// Exactly one top-level object inside prose names a scoreable field.
    Embedded,
}

impl Found {
    pub fn as_str(self) -> &'static str {
        match self {
            Found::Strict => "strict_json",
            Found::Embedded => "embedded_json",
        }
    }
}

fn fence() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?is)\A\s*```json\s*\n(?P<body>.*?)\n```\s*\z").unwrap())
}

fn strict(text: &str) -> Result<Map<String, Value>, String> {
    let mut candidate = text.trim();
    if let Some(c) = fence().captures(candidate) {
        candidate = c.name("body").map_or("", |m| m.as_str()).trim();
    }
    match serde_json::from_str::<Value>(candidate) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err("answer must be one JSON object".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// Every JSON object in `text` that no other JSON container (object or array) encloses, in order.
pub fn top_level_objects(text: &str) -> Vec<Map<String, Value>> {
    top_level_values(text)
        .into_iter()
        .filter_map(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .collect()
}

/// Every JSON container (object or array) in `text` that no other container encloses, in order.
pub fn top_level_values(text: &str) -> Vec<Value> {
    let mut containers: Vec<(usize, usize, Value)> = Vec::new();
    for (start, c) in text.char_indices() {
        if c != '[' && c != '{' {
            continue;
        }
        let mut it = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        if let Some(Ok(v)) = it.next() {
            if v.is_object() || v.is_array() {
                containers.push((start, start + it.byte_offset(), v));
            }
        }
    }
    let spans: Vec<(usize, usize)> = containers.iter().map(|(s, e, _)| (*s, *e)).collect();
    containers
        .into_iter()
        .filter(|(s, e, _)| !spans.iter().any(|&(os, oe)| os <= *s && *e <= oe && (os, oe) != (*s, *e)))
        .map(|(_, _, v)| v)
        .collect()
}

fn any_fence() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?s)```[^\n]*\n(.*?)\n[ \t]*```").unwrap())
}

/// The body of every ``` fence in `text` (whatever its tag) that is JSON (as `plain_json` reads
/// it), in order. A model that puts its answer in a fence after prose is read from the fence.
pub fn fenced_json(text: &str) -> Vec<Value> {
    any_fence()
        .captures_iter(text)
        .filter_map(|c| c.get(1))
        .filter_map(|m| serde_json::from_str::<Unique>(m.as_str().trim()).ok().map(|u| u.0))
        .collect()
}

/// The answer object of `text`: the whole text when it is one JSON object, else the one top-level
/// object in it that names a field of `fields`. Err says why there is none.
pub fn parse_answer(text: &str, fields: &[String]) -> Result<(Map<String, Value>, Found), String> {
    if let Ok(m) = strict(text) {
        return Ok((m, Found::Strict));
    }
    let wanted: BTreeSet<&str> = fields.iter().map(String::as_str).collect();
    let mut found: Vec<Map<String, Value>> =
        top_level_objects(text).into_iter().filter(|m| m.keys().any(|k| wanted.contains(k.as_str()))).collect();
    match found.len() {
        1 => Ok((found.remove(0), Found::Embedded)),
        0 => Err("answer does not contain a scoreable JSON object".into()),
        _ => Err("answer contains multiple scoreable JSON objects".into()),
    }
}

fn plain_fence() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?s)\A```(?:json)?\s*\n(.*?)\n```\z").unwrap())
}

/// `text` as JSON, perhaps in a fence: any value, but no object may repeat a key and no number may
/// be infinite. The reading canonical-step applies to a worker's reply.
pub fn plain_json(text: &str) -> Result<Value, String> {
    let candidate = text.trim();
    let body = plain_fence().captures(candidate).and_then(|c| c.get(1)).map_or(candidate, |m| m.as_str());
    serde_json::from_str::<Unique>(body).map(|u| u.0).map_err(|e| e.to_string())
}

/// A JSON value whose objects never repeat a key.
struct Unique(Value);

impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(UniqueVisitor).map(Unique)
    }
}

struct UniqueVisitor;

impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        Number::from_f64(v).map(Value::Number).ok_or_else(|| E::custom("non-finite JSON number"))
    }
    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_string()))
    }
    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut out = Vec::new();
        while let Some(Unique(v)) = seq.next_element()? {
            out.push(v);
        }
        Ok(Value::Array(out))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut out = Map::new();
        while let Some(k) = map.next_key::<String>()? {
            let Unique(v) = map.next_value()?;
            if out.insert(k, v).is_some() {
                return Err(de::Error::custom("duplicate JSON keys are ambiguous"));
            }
        }
        Ok(Value::Object(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fields(f: &[&str]) -> Vec<String> {
        f.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_whole_object_or_a_json_fence_is_strict() {
        let (m, how) = parse_answer("  {\"a\": 1}\n", &fields(&["a"])).unwrap();
        assert_eq!((Value::Object(m), how), (json!({"a": 1}), Found::Strict));
        let (m, how) = parse_answer("```JSON\n{\"a\": [1, 2]}\n```", &fields(&["zzz"])).unwrap();
        assert_eq!((Value::Object(m), how), (json!({"a": [1, 2]}), Found::Strict));
    }

    #[test]
    fn prose_around_one_scoreable_object_is_embedded() {
        let text = "I read the code.\n```\n{\"covered_lines\": [3, 4], \"note\": {\"x\": 1}}\n```\nDone {not json}.";
        let (m, how) = parse_answer(text, &fields(&["covered_lines"])).unwrap();
        assert_eq!(how, Found::Embedded);
        assert_eq!(Value::Object(m), json!({"covered_lines": [3, 4], "note": {"x": 1}}));
        // an object nested in a list is not top level; one without a scoreable field is ignored
        let text = "[{\"a\": 1}] then {\"b\": 2} and {\"a\": 3}";
        let (m, _) = parse_answer(text, &fields(&["a"])).unwrap();
        assert_eq!(Value::Object(m), json!({"a": 3}));
    }

    #[test]
    fn none_or_several_scoreable_objects_is_an_error() {
        assert!(parse_answer("no json here", &fields(&["a"])).unwrap_err().contains("does not contain"));
        assert!(parse_answer("[1, 2]", &fields(&["a"])).is_err());
        let e = parse_answer("{\"a\": 1} or {\"a\": 2}", &fields(&["a"])).unwrap_err();
        assert!(e.contains("multiple"), "{e}");
    }

    #[test]
    fn plain_json_takes_any_value_but_refuses_repeated_keys() {
        assert_eq!(plain_json("```\n[\"x\"]\n```").unwrap(), json!(["x"]));
        assert_eq!(plain_json("```json\n{\"a\": 2.5}\n```").unwrap(), json!({"a": 2.5}));
        assert!(plain_json("{\"a\": 1, \"a\": 2}").unwrap_err().contains("duplicate"));
        assert!(plain_json("The answer is {\"a\": 1}").is_err());
        assert!(plain_json("1e999").is_err());
    }

    #[test]
    fn fences_and_top_level_values_are_read_whatever_encloses_them() {
        let text = "Done. Result:\n```json\n[\"alpha\", \"beta\"]\n```\nand also\n```\n{\"a\": [1]}\n```";
        assert_eq!(fenced_json(text), vec![json!(["alpha", "beta"]), json!({"a": [1]})]);
        assert_eq!(fenced_json("```json\n{\"a\": 1, \"a\": 2}\n```"), Vec::<Value>::new());
        let prose = "The value is [1, 2] (see {\"k\": [3]}) and {\"x\": {\"y\": 1}} ends it";
        assert_eq!(top_level_values(prose), vec![json!([1, 2]), json!({"k": [3]}), json!({"x": {"y": 1}})]);
        assert_eq!(top_level_objects(prose).len(), 2);
    }
}
