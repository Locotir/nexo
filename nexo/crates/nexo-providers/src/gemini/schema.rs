//! The tool schema as the API takes it: the OpenAPI subset of function declarations.
//!
//! Tool schemas come from MCP servers and plugins as full JSON Schema. The function declaration
//! takes only a subset, and one name or shape it does not know makes the whole request fail with
//! INVALID_ARGUMENT, so every turn on that tool set breaks. The cleaner keeps an allow-list of
//! keywords and rewrites the common JSON Schema forms into ones the subset has.

use serde_json::{json, Map, Value};

/// Keywords copied through unchanged. The structural ones (`properties`, `items`, `anyOf`) and the
/// ones that need a rewrite (`type`, `enum`, `format`, `required`) are handled in `norm`.
const KEPT: &[&str] = &[
    "title",
    "description",
    "nullable",
    "minItems",
    "maxItems",
    "minProperties",
    "maxProperties",
    "minLength",
    "maxLength",
    "pattern",
    "minimum",
    "maximum",
    "propertyOrdering",
];

/// How deep a schema may nest before the rest is replaced by a plain object. This stops runaway
/// input; reference cycles are caught on their own.
const MAX_DEPTH: usize = 64;

/// The schema as the function declaration takes it.
pub fn clean_schema(v: &Value) -> Value {
    norm(v, v, &mut Vec::new(), 0)
}

/// The stand-in for a schema we cannot express: a reference that loops or points nowhere.
fn any_object() -> Value {
    json!({"type": "object"})
}

fn is_null_schema(v: &Value) -> bool {
    v.get("type").and_then(Value::as_str) == Some("null")
}

/// Adds the keys of `from` that `into` does not have yet. `properties` and `required` of two
/// object schemas are joined instead, which is what `allOf` means for objects.
fn fill(into: &mut Map<String, Value>, from: &Map<String, Value>) {
    for (k, v) in from {
        match (k.as_str(), into.get_mut(k)) {
            ("properties", Some(Value::Object(mine))) => {
                if let Value::Object(theirs) = v {
                    for (n, s) in theirs {
                        mine.entry(n.clone()).or_insert_with(|| s.clone());
                    }
                }
            }
            ("required", Some(Value::Array(mine))) => {
                if let Value::Array(theirs) = v {
                    for n in theirs {
                        if !mine.contains(n) {
                            mine.push(n.clone());
                        }
                    }
                }
            }
            (_, Some(_)) => {}
            (_, None) => {
                into.insert(k.clone(), v.clone());
            }
        }
    }
}

/// One schema node. `root` is where local `$ref`s point into and `stack` the references being
/// expanded right now, so a cycle ends in a plain object instead of looping.
fn norm(node: &Value, root: &Value, stack: &mut Vec<String>, depth: usize) -> Value {
    let Value::Object(o) = node else {
        // `true` and `false` schemas, or junk: no constraint we can express
        return json!({});
    };
    if depth > MAX_DEPTH {
        return any_object();
    }
    if let Some(r) = o.get("$ref") {
        // a local reference is inlined, with the keys beside it (a description, say) on top
        let target = r
            .as_str()
            .filter(|r| !stack.iter().any(|s| s == r))
            .and_then(|r| r.strip_prefix('#').map(|p| (r, p)))
            .and_then(|(r, p)| root.pointer(p).map(|t| (r.to_string(), t)));
        let Some((r, Value::Object(t))) = target else { return any_object() };
        let mut merged = t.clone();
        for (k, v) in o {
            if k != "$ref" {
                merged.insert(k.clone(), v.clone());
            }
        }
        stack.push(r);
        let out = norm(&Value::Object(merged), root, stack, depth + 1);
        stack.pop();
        return out;
    }

    let mut out = Map::new();
    let mut nullable = false;
    let mut types: Vec<String> = Vec::new();
    let mut enum_values: Option<Vec<Value>> = None;
    let mut format: Option<&Value> = None;
    for (k, v) in o {
        match k.as_str() {
            // the children of `properties` are names chosen by the tool, not keywords
            "properties" => {
                if let Value::Object(props) = v {
                    let kept = props.iter().map(|(n, s)| (n.clone(), norm(s, root, stack, depth + 1))).collect();
                    out.insert(k.clone(), Value::Object(kept));
                }
            }
            // the tuple form (an array of schemas) has no counterpart
            "items" if v.is_object() || v.is_boolean() => {
                out.insert(k.clone(), norm(v, root, stack, depth + 1));
            }
            "type" => match v {
                Value::String(t) => types.push(t.clone()),
                Value::Array(a) => types.extend(a.iter().filter_map(|t| t.as_str().map(str::to_string))),
                _ => {}
            },
            "enum" => enum_values = v.as_array().cloned(),
            "format" => format = Some(v),
            "required" if v.is_array() => {
                out.insert(k.clone(), v.clone());
            }
            k if KEPT.contains(&k) => {
                out.insert(k.to_string(), v.clone());
            }
            // `$ref`, `$defs`, `not`, `exclusiveMinimum`, `default`, `const` and the rest do not exist here
            _ => {}
        }
    }

    // `type` as a list: `null` is `nullable`. One type left is the type; a real choice between
    // types becomes an `anyOf` once the alternatives are known. The node's own type goes in
    // before anything merged from members, so it wins.
    nullable |= types.iter().any(|t| t == "null");
    types.retain(|t| t != "null");
    let mut seen = std::collections::HashSet::new();
    types.retain(|t| seen.insert(t.clone()));
    if let [only] = types.as_slice() {
        out.insert("type".into(), Value::String(only.clone()));
    }

    // `allOf` of objects is one object with all the properties and required names
    let all_of = o.get("allOf").and_then(Value::as_array).into_iter().flatten();
    for m in all_of {
        if let Value::Object(m) = norm(m, root, stack, depth + 1) {
            fill(&mut out, &m);
        }
    }

    // `oneOf` becomes `anyOf`; a null member turns into `nullable` on this node
    let alternatives = o.get("anyOf").or_else(|| o.get("oneOf")).and_then(Value::as_array);
    let mut members = Vec::new();
    for m in alternatives.into_iter().flatten() {
        if is_null_schema(m) {
            nullable = true;
        } else {
            members.push(norm(m, root, stack, depth + 1));
        }
    }
    match members.len() {
        0 => {}
        1 => {
            if let Value::Object(m) = &members[0] {
                nullable |= m.get("nullable") == Some(&Value::Bool(true));
                fill(&mut out, m);
            }
        }
        _ => {
            out.insert("anyOf".into(), Value::Array(members));
        }
    }

    if types.len() > 1 && !out.contains_key("anyOf") {
        let alts = types.iter().map(|t| json!({"type": t})).collect();
        out.insert("anyOf".into(), Value::Array(alts));
    }

    let ty = out.get("type").and_then(Value::as_str).map(str::to_string);

    // only the formats the API lists, and only on the type that has them
    if let (Some(Value::String(f)), Some(ty)) = (format, ty.as_deref()) {
        let listed = match ty {
            "string" => matches!(f.as_str(), "enum" | "date-time"),
            "integer" | "number" => matches!(f.as_str(), "int32" | "int64" | "float" | "double"),
            _ => false,
        };
        if listed {
            out.insert("format".into(), Value::String(f.clone()));
        }
    }

    // `enum` is for strings only. Other values would be a 400, so the model gets them as a fact
    // in the description instead.
    if let Some(mut values) = enum_values {
        if values.iter().any(Value::is_null) {
            nullable = true;
            values.retain(|v| !v.is_null());
        }
        let strings = values.iter().all(Value::is_string);
        if !values.is_empty() && strings && matches!(ty.as_deref(), None | Some("string")) {
            out.entry("type").or_insert_with(|| json!("string"));
            out.insert("enum".into(), Value::Array(values));
        } else if !values.is_empty() {
            let list: Vec<String> = values.iter().map(|v| v.to_string()).collect();
            let note = format!("Allowed values: {}.", list.join(", "));
            let text = match out.get("description").and_then(Value::as_str) {
                Some(d) if !d.is_empty() => format!("{d} {note}"),
                _ => note,
            };
            out.insert("description".into(), Value::String(text));
        }
    }

    if nullable {
        out.insert("nullable".into(), Value::Bool(true));
    }
    Value::Object(out)
}
