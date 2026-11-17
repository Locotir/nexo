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

/// How many references one schema may inline. Definitions that refer to each other several times
/// grow exponentially when inlined, and the depth limit does not bound that; past the budget the
/// further references become plain objects, which keeps the request small and the cleaning fast.
/// Only references are counted, so the plain properties of a tool are never starved by a big one.
const MAX_EXPANSIONS: usize = 2000;

/// What the cleaning of one schema carries along: where local `$ref`s point into, the references
/// being expanded right now (so a cycle ends in a plain object instead of looping) and how many
/// references were inlined so far.
struct Cx<'a> {
    root: &'a Value,
    stack: Vec<String>,
    expansions: usize,
}

/// The schema as the function declaration takes it.
pub fn clean_schema(v: &Value) -> Value {
    let mut cx = Cx { root: v, stack: Vec::new(), expansions: 0 };
    let mut out = norm(v, &mut cx, 0);
    // the top level is the caller's to judge: a tool with no arguments goes without `parameters`
    prune(&mut out);
    out
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

/// One schema node.
fn norm(node: &Value, cx: &mut Cx, depth: usize) -> Value {
    let Value::Object(o) = node else {
        // `true` and `false` schemas, or junk: no constraint we can express
        return json!({});
    };
    if depth > MAX_DEPTH {
        return any_object();
    }
    if let Some(r) = o.get("$ref") {
        cx.expansions += 1;
        if cx.expansions > MAX_EXPANSIONS {
            return any_object();
        }
        let root = cx.root;
        // a local reference is inlined, with the keys beside it (a description, say) on top
        let target = r
            .as_str()
            .filter(|r| !cx.stack.iter().any(|s| s == r))
            .and_then(|r| r.strip_prefix('#').map(|p| (r, p)))
            .and_then(|(r, p)| root.pointer(p).map(|t| (r.to_string(), t)));
        let Some((r, Value::Object(t))) = target else { return any_object() };
        let mut merged = t.clone();
        for (k, v) in o {
            if k != "$ref" {
                merged.insert(k.clone(), v.clone());
            }
        }
        cx.stack.push(r);
        let out = norm(&Value::Object(merged), cx, depth + 1);
        cx.stack.pop();
        return out;
    }

    let mut out = Map::new();
    let mut nullable = false;
    let mut types: Vec<String> = Vec::new();
    let mut enum_values: Option<Vec<Value>> = None;
    let mut format: Option<&Value> = None;
    let mut tuple: Vec<Value> = Vec::new();
    for (k, v) in o {
        match k.as_str() {
            // the children of `properties` are names chosen by the tool, not keywords
            "properties" => {
                if let Value::Object(props) = v {
                    let kept = props.iter().map(|(n, s)| (n.clone(), norm(s, cx, depth + 1))).collect();
                    out.insert(k.clone(), Value::Object(kept));
                }
            }
            "items" if v.is_object() || v.is_boolean() => {
                out.insert(k.clone(), norm(v, cx, depth + 1));
            }
            // the tuple form (an array of schemas) has no counterpart; what the positions have in
            // common is kept below
            "items" => {
                if let Value::Array(members) = v {
                    tuple = members.iter().map(|m| norm(m, cx, depth + 1)).collect();
                }
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
        if let Value::Object(m) = norm(m, cx, depth + 1) {
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
            members.push(norm(m, cx, depth + 1));
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

    // The API answers 400 to an array without `items`. A tuple is widened to the choice of its
    // positions and the model is told the order; anything else gets the scalar types.
    if ty.as_deref() == Some("array") && out.get("items").is_none_or(untyped) {
        let items = widen(&tuple);
        if tuple.len() > 1 {
            let kinds: Vec<&str> = tuple.iter().filter_map(|m| m.get("type").and_then(Value::as_str)).collect();
            if kinds.len() == tuple.len() {
                let note = format!("Items by position: {}.", kinds.join(", "));
                let text = match out.get("description").and_then(Value::as_str) {
                    Some(d) if !d.is_empty() => format!("{d} {note}"),
                    _ => note,
                };
                out.insert("description".into(), Value::String(text));
            }
        }
        out.insert("items".into(), items);
    }

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

/// A schema that says nothing about the type of a value.
fn untyped(v: &Value) -> bool {
    v.get("type").is_none() && v.get("anyOf").is_none()
}

/// One schema for the members of a tuple: the member itself, the choice between distinct ones,
/// or, with no members, the choice between the scalar types (an object has no usable form here).
fn widen(members: &[Value]) -> Value {
    let mut distinct: Vec<Value> = Vec::new();
    for m in members.iter().filter(|m| !untyped(m)) {
        if !distinct.contains(m) {
            distinct.push(m.clone());
        }
    }
    match distinct.len() {
        0 => json!({"anyOf": [{"type": "string"}, {"type": "number"}, {"type": "boolean"}]}),
        1 => distinct.remove(0),
        _ => json!({"anyOf": distinct}),
    }
}

/// Whether the API would reject this node: an object with no properties ("should be non-empty
/// for OBJECT type"), an array of such objects, a choice with nothing left to choose from.
/// Everywhere below the top, such a node is cut out of its parent and the parent's description
/// names what is missing, so the model knows it cannot pass it. The top level is left to the
/// caller, which sends a tool with no arguments without `parameters`.
fn prune(node: &mut Value) -> bool {
    let Value::Object(o) = node else { return false };
    let mut left_out: Vec<String> = Vec::new();
    if let Some(Value::Object(props)) = o.get_mut("properties") {
        props.retain(|name, schema| {
            let unusable = prune(schema);
            if unusable {
                left_out.push(name.clone());
            }
            !unusable
        });
    }
    if !left_out.is_empty() {
        if let Some(Value::Array(required)) = o.get_mut("required") {
            required.retain(|r| r.as_str().is_none_or(|r| !left_out.iter().any(|n| n == r)));
        }
        let note = format!("Not offered here, the API cannot take their schema: {}.", left_out.join(", "));
        let text = match o.get("description").and_then(Value::as_str) {
            Some(d) if !d.is_empty() => format!("{d} {note}"),
            _ => note,
        };
        o.insert("description".into(), Value::String(text));
    }
    if o.get_mut("items").is_some_and(prune) {
        return true;
    }
    if let Some(Value::Array(members)) = o.get_mut("anyOf") {
        members.retain_mut(|m| !prune(m));
        if members.is_empty() {
            return true;
        }
    }
    let no_properties = o.get("properties").and_then(Value::as_object).is_none_or(|p| p.is_empty());
    o.get("type").and_then(Value::as_str) == Some("object") && no_properties && !o.contains_key("anyOf")
}
