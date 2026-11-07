//! A small JSON Schema checker for `--json-schema`, and the reading of a model's JSON answer.
//!
//! It covers the keywords people write for answers: `type`, `enum`, `const`, `properties`,
//! `required`, `additionalProperties`, `items`, `minItems`, `maxItems`, `minLength`, `maxLength`,
//! `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`, `anyOf`, `oneOf`, `allOf`, `not`
//! and local `$ref` (`#/...`). Other keywords are ignored.

use serde_json::Value;

/// Reads the schema from a file path or from inline JSON.
pub fn load(arg: &str) -> Result<Value, String> {
    let text = if arg.trim_start().starts_with(['{', '[']) || matches!(arg.trim(), "true" | "false") {
        arg.to_string()
    } else {
        std::fs::read_to_string(arg).map_err(|e| format!("--json-schema {arg}: {e}"))?
    };
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("--json-schema: not valid JSON: {e}"))?;
    if v.is_object() || v.is_boolean() {
        Ok(v)
    } else {
        Err("--json-schema: a schema is a JSON object".into())
    }
}

/// The JSON value in a model's reply: the whole reply, a fenced block, or the outermost braces.
pub fn extract(reply: &str) -> Result<Value, String> {
    let t = reply.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Ok(v);
    }
    if let Some(start) = t.find("```") {
        let rest = &t[start + 3..];
        let body = rest.split_once('\n').map(|(_, b)| b).unwrap_or(rest);
        if let Some(end) = body.find("```") {
            if let Ok(v) = serde_json::from_str::<Value>(body[..end].trim()) {
                return Ok(v);
            }
        }
    }
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let (Some(a), Some(b)) = (t.find(open), t.rfind(close)) {
            if a < b {
                if let Ok(v) = serde_json::from_str::<Value>(&t[a..=b]) {
                    return Ok(v);
                }
            }
        }
    }
    Err("the answer is not JSON".into())
}

/// Every way `value` breaks `schema`, each as "path: problem" (empty: it validates).
pub fn validate(schema: &Value, value: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    check(schema, schema, value, "$", &mut errors, 0);
    errors
}

fn type_ok(t: &str, v: &Value) -> bool {
    match t {
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.fract() == 0.0),
        _ => true,
    }
}

fn check(root: &Value, schema: &Value, v: &Value, path: &str, errs: &mut Vec<String>, depth: u32) {
    let Some(s) = schema.as_object() else {
        if schema == &Value::Bool(false) {
            errs.push(format!("{path}: no value is allowed here"));
        }
        return;
    };
    if depth > 32 {
        return;
    }
    if let Some(r) = s.get("$ref").and_then(Value::as_str) {
        match r.strip_prefix('#').and_then(|p| root.pointer(p)) {
            Some(target) => check(root, target, v, path, errs, depth + 1),
            None => errs.push(format!("{path}: the schema's $ref {r} cannot be resolved")),
        }
        return;
    }
    if let Some(t) = s.get("type") {
        let names: Vec<&str> = match t {
            Value::String(x) => vec![x.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        if !names.is_empty() && !names.iter().any(|n| type_ok(n, v)) {
            errs.push(format!("{path}: expected {}, got {}", names.join(" or "), kind(v)));
            return;
        }
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array) {
        if !e.contains(v) {
            errs.push(format!("{path}: {v} is not one of the allowed values"));
        }
    }
    if let Some(c) = s.get("const") {
        if c != v {
            errs.push(format!("{path}: must equal {c}"));
        }
    }
    if let Some(n) = v.as_f64() {
        let num = |k: &str| s.get(k).and_then(Value::as_f64);
        if let Some(m) = num("minimum").filter(|m| n < *m) {
            errs.push(format!("{path}: {n} is below the minimum {m}"));
        }
        if let Some(m) = num("maximum").filter(|m| n > *m) {
            errs.push(format!("{path}: {n} is above the maximum {m}"));
        }
        if let Some(m) = num("exclusiveMinimum").filter(|m| n <= *m) {
            errs.push(format!("{path}: {n} must be above {m}"));
        }
        if let Some(m) = num("exclusiveMaximum").filter(|m| n >= *m) {
            errs.push(format!("{path}: {n} must be below {m}"));
        }
    }
    if let Some(t) = v.as_str() {
        let len = t.chars().count() as u64;
        if let Some(m) = s.get("minLength").and_then(Value::as_u64).filter(|m| len < *m) {
            errs.push(format!("{path}: shorter than {m} characters"));
        }
        if let Some(m) = s.get("maxLength").and_then(Value::as_u64).filter(|m| len > *m) {
            errs.push(format!("{path}: longer than {m} characters"));
        }
    }
    if let Some(a) = v.as_array() {
        let len = a.len() as u64;
        if let Some(m) = s.get("minItems").and_then(Value::as_u64).filter(|m| len < *m) {
            errs.push(format!("{path}: fewer than {m} items"));
        }
        if let Some(m) = s.get("maxItems").and_then(Value::as_u64).filter(|m| len > *m) {
            errs.push(format!("{path}: more than {m} items"));
        }
        if let Some(items) = s.get("items") {
            for (i, x) in a.iter().enumerate() {
                check(root, items, x, &format!("{path}[{i}]"), errs, depth + 1);
            }
        }
    }
    if let Some(o) = v.as_object() {
        if let Some(req) = s.get("required").and_then(Value::as_array) {
            for k in req.iter().filter_map(Value::as_str) {
                if !o.contains_key(k) {
                    errs.push(format!("{path}: missing the required property \"{k}\""));
                }
            }
        }
        let props = s.get("properties").and_then(Value::as_object);
        for (k, x) in o {
            match props.and_then(|p| p.get(k)) {
                Some(ps) => check(root, ps, x, &format!("{path}.{k}"), errs, depth + 1),
                None => match s.get("additionalProperties") {
                    Some(Value::Bool(false)) => errs.push(format!("{path}: unexpected property \"{k}\"")),
                    Some(extra @ Value::Object(_)) => check(root, extra, x, &format!("{path}.{k}"), errs, depth + 1),
                    _ => {}
                },
            }
        }
    }
    let passes = |sub: &Value| {
        let mut e = Vec::new();
        check(root, sub, v, path, &mut e, depth + 1);
        e.is_empty()
    };
    if let Some(all) = s.get("allOf").and_then(Value::as_array) {
        for sub in all {
            check(root, sub, v, path, errs, depth + 1);
        }
    }
    if let Some(any) = s.get("anyOf").and_then(Value::as_array) {
        if !any.iter().any(passes) {
            errs.push(format!("{path}: matches none of the allowed shapes (anyOf)"));
        }
    }
    if let Some(one) = s.get("oneOf").and_then(Value::as_array) {
        let n = one.iter().filter(|x| passes(x)).count();
        if n != 1 {
            errs.push(format!("{path}: matches {n} of the shapes, exactly one is required (oneOf)"));
        }
    }
    if let Some(not) = s.get("not") {
        if passes(not) {
            errs.push(format!("{path}: matches a shape that is not allowed"));
        }
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_json_from_prose_and_fences() {
        assert_eq!(extract("{\"a\":1}").unwrap(), json!({"a": 1}));
        assert_eq!(extract("Sure:\n```json\n{\"a\": 2}\n```\n").unwrap(), json!({"a": 2}));
        assert_eq!(extract("here it is {\"a\": 3} done").unwrap(), json!({"a": 3}));
        assert!(extract("no json here").is_err());
    }

    #[test]
    fn validates_the_common_keywords() {
        let s = json!({
            "type": "object",
            "required": ["name", "n"],
            "additionalProperties": false,
            "properties": {
                "name": {"type": "string", "minLength": 1},
                "n": {"type": "integer", "minimum": 0},
                "tags": {"type": "array", "items": {"enum": ["a", "b"]}}
            }
        });
        assert!(validate(&s, &json!({"name": "x", "n": 3, "tags": ["a"]})).is_empty());
        let e = validate(&s, &json!({"name": "", "n": -1, "tags": ["c"], "z": 1}));
        assert_eq!(e.len(), 4, "{e:?}");
        let e = validate(&s, &json!({"n": 1.5}));
        assert!(e.iter().any(|m| m.contains("\"name\"")) && e.iter().any(|m| m.contains("expected integer")), "{e:?}");
    }

    #[test]
    fn follows_local_refs_and_combinators() {
        let s = json!({"$defs": {"id": {"type": "string"}}, "anyOf": [{"$ref": "#/$defs/id"}, {"type": "null"}]});
        assert!(validate(&s, &json!("x")).is_empty());
        assert!(validate(&s, &json!(null)).is_empty());
        assert!(!validate(&s, &json!(3)).is_empty());
    }
}
