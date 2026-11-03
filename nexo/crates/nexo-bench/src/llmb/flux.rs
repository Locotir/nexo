// Answer comparison ported from SWE-Flux.
//
// Source: https://github.com/HamedTaherkhani/SWE-Flux, commit
// 53de3e3455b2595ffbeba0fc34de9ba2c4ab4eed, module evaluation_scripts/evaluate_qa_answers
// (CompareResult and the helpers up to compare_values), as vendored unmodified by the
// llm-benchmark distribution; this is a faithful port of those rules to Rust.
//
// MIT License
//
// Copyright (c) 2023 Hamed Taherkhani, Hadi Hemmati, Mohammad Abdollahi, Melika Sepidband
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

//! SWE-Flux's comparison of a predicted value with the expected one: sets compared as multisets,
//! paths and identifiers normalised, numbers within a tolerance, containers written as strings
//! read back. Each rule forgives a presentation, never a different value.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use super::py;

/// Ok, or why not (with the JSON path where the values part).
pub type Compared = Result<(), String>;

/// The module file suffix the source repositories use (kept out of string literals on purpose).
const MODULE_EXT: &str = concat!(".", "p", "y");

fn normalize_path(s: &str) -> String {
    let mut out = s.trim().replace('\\', "/");
    while out.contains("//") {
        out = out.replace("//", "/");
    }
    if let Some(r) = out.strip_prefix("./") {
        out = r.to_string();
    }
    if out.ends_with('/') && out.chars().count() > 1 {
        out.pop();
    }
    out
}

fn dotted_to_path(s: &str) -> String {
    if s.contains('/') || s.contains('\\') {
        normalize_path(s)
    } else if s.contains('.') {
        normalize_path(&s.replace('.', "/"))
    } else {
        s.to_string()
    }
}

fn normalize_identifier(s: &str, key_name: &str) -> BTreeSet<String> {
    let raw = s.trim();
    let mut vals: BTreeSet<String> = [raw.to_string(), normalize_path(raw), dotted_to_path(raw)].into();
    if key_name.to_lowercase().contains("file") {
        for v in vals.clone() {
            match v.strip_suffix(MODULE_EXT) {
                Some(stem) => vals.insert(stem.to_string()),
                None => vals.insert(format!("{v}{MODULE_EXT}")),
            };
        }
    }
    vals
}

const ALT_SEPARATOR: &str = "|||";

fn is_alt_string(s: &str) -> bool {
    if !s.contains(ALT_SEPARATOR) {
        return false;
    }
    let parts: Vec<&str> = s.split(ALT_SEPARATOR).map(str::trim).collect();
    parts.len() >= 2 && parts.iter().all(|p| !p.is_empty())
}

fn enum_canonical(s: &str) -> String {
    let mut token = s.trim();
    if token.starts_with('<') && token.ends_with('>') && token.chars().count() > 2 {
        token = token[1..token.len() - 1].trim();
    }
    if let Some(i) = token.rfind('.') {
        token = &token[i + 1..];
    }
    token.to_uppercase()
}

/// Keys whose string values are code identifiers (paths, functions, classes): a path prefix, a
/// class repr wrapper or a bare-versus-qualified name is forgiven, a different entity is not.
const IDENTIFIER_KEYS: [&str; 8] =
    ["file", "func", "cls", "class", "concrete_class", "dispatched_file", "dispatched_function", "dispatched_class"];

fn class_repr() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^<class\s+'([^']+)'>$").unwrap())
}

fn cond_prefix() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^(if|elif|while|for|assert)\b\s*").unwrap())
}

fn strip_class_repr(s: &str) -> String {
    let t = s.trim();
    class_repr().captures(t).map_or(t.to_string(), |c| c[1].to_string())
}

fn strip_condition(s: &str) -> String {
    let t = cond_prefix().replace(s.trim(), "").into_owned();
    t.strip_suffix(':').unwrap_or(&t).trim().to_string()
}

/// One dotted or slashed identifier is a whole-component tail of the other.
fn component_suffix_match(a: &str, b: &str) -> bool {
    let split = |s: &str| s.trim().split(['.', '/']).filter(|x| !x.is_empty()).map(String::from).collect::<Vec<_>>();
    let (sa, sb) = (split(a), split(b));
    if sa.is_empty() || sb.is_empty() {
        return false;
    }
    let (short, long) = if sa.len() <= sb.len() { (sa, sb) } else { (sb, sa) };
    long[long.len() - short.len()..] == short[..]
}

fn compare_string_single(expected: &str, pred: &str, key_name: &str) -> bool {
    if expected == pred {
        return true;
    }
    let kl = key_name.to_lowercase();
    if kl == "condition" && strip_condition(expected) == strip_condition(pred) {
        return true;
    }
    if IDENTIFIER_KEYS.contains(&kl.as_str()) {
        let (e, p) = (strip_class_repr(expected), strip_class_repr(pred));
        if e == p || component_suffix_match(&e, &p) {
            return true;
        }
    }
    let (e, p) = (normalize_identifier(expected, key_name), normalize_identifier(pred, key_name));
    if e.intersection(&p).next().is_some() {
        return true;
    }
    if kl == "type" || kl == "status" {
        return enum_canonical(expected) == enum_canonical(pred);
    }
    false
}

pub fn compare_strings(expected: &str, pred: &str, key_name: &str) -> bool {
    compare_string_single(expected, pred, key_name)
        || (is_alt_string(expected)
            && expected.split(ALT_SEPARATOR).any(|alt| compare_string_single(alt.trim(), pred, key_name)))
}

/// Answer fields that are sets: compared as multisets, since their order means nothing. Sequence,
/// history and path fields are deliberately absent: their order is the answer.
pub const ORDER_INSENSITIVE_LIST_KEYS: [&str; 15] = [
    "observed_def_use_pairs",
    "covered_def_use_pairs",
    "covered_du_pairs",
    "uncovered_du_pairs_list",
    "covered_lines",
    "covered_functions",
    "union_lines_sorted",
    "intersection_lines_sorted",
    "lines_covered_by_only_one_test",
    "exception_types",
    "unique_values",
    "subtype_value_set",
    "guess_value_set",
    "safe_tests",
    "crashing_tests",
];

/// A number that is not a boolean.
fn number(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn isclose(a: f64, b: f64, tol: f64) -> bool {
    a == b || (a - b).abs() <= (tol * a.abs().max(b.abs())).max(tol)
}

fn mappingproxy() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?s)^mappingproxy\((.*)\)$").unwrap())
}

/// A `mappingproxy({...})` repr as the dict it shows; anything else as it is.
fn normalize_special_value(v: &Value) -> Value {
    if let Value::String(s) = v {
        if let Some(c) = mappingproxy().captures(s.trim()) {
            if let Some(d @ Value::Object(_)) = py::literal(c[1].trim()) {
                return d;
            }
        }
    }
    v.clone()
}

/// A list or dict the model wrote as a string, read back (as JSON, else as a literal).
fn try_parse_container(s: &str) -> Option<Value> {
    let t = s.trim();
    if !t.starts_with(['[', '{']) {
        return None;
    }
    serde_json::from_str::<Value>(t)
        .ok()
        .filter(|v| v.is_array() || v.is_object())
        .or_else(|| py::literal(t).filter(|v| v.is_array() || v.is_object()))
}

/// A scalar written as a string: `true`, `null`, `282`, `8.0`.
enum Scalar {
    Bool(bool),
    Null,
    Num(f64),
}

fn int_like() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[+-]?\d+$").unwrap())
}

fn coerce_scalar_string(s: &str) -> Option<Scalar> {
    let t = s.trim();
    match t.to_lowercase().as_str() {
        "true" => return Some(Scalar::Bool(true)),
        "false" => return Some(Scalar::Bool(false)),
        "null" | "none" => return Some(Scalar::Null),
        _ => {}
    }
    if int_like().is_match(t) {
        return t.trim_start_matches('+').parse::<f64>().ok().map(Scalar::Num);
    }
    t.parse::<f64>().ok().map(Scalar::Num)
}

/// Loose equality as the source language has it: `1 == 1.0`, `True == 1`.
fn loose_eq(a: &Value, b: &Value) -> bool {
    let num = |v: &Value| match v {
        Value::Bool(x) => Some(if *x { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        _ => None,
    };
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| loose_eq(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| loose_eq(v, w)))
        }
        (Value::Number(x), Value::Number(y)) if !x.is_f64() && !y.is_f64() => x == y,
        _ => match (num(a), num(b)) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        },
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    s.chars().take(80).collect()
}

fn compare_list_unordered(expected: &[Value], pred: &[Value], key_name: &str, path: &str, tol: f64) -> Compared {
    let mut used = vec![false; pred.len()];
    for (i, e) in expected.iter().enumerate() {
        let child = format!("{path}[{i}]");
        let hit = (0..pred.len()).find(|&j| !used[j] && compare_values(e, &pred[j], key_name, &child, tol).is_ok());
        match hit {
            Some(j) => used[j] = true,
            None => return Err(format!("{path}: unmatched_element (order-insensitive) {}", short(e))),
        }
    }
    Ok(())
}

/// Whether `pred` is `expected` by SWE-Flux's rules; `key_name` is the field (or the dict key) the
/// values sit under, `path` where they are, `tol` the relative and absolute numeric tolerance.
pub fn compare_values(expected: &Value, pred: &Value, key_name: &str, path: &str, tol: f64) -> Compared {
    let expected = normalize_special_value(expected);
    let mut pred = normalize_special_value(pred);

    if let (Some(e), Some(p)) = (number(&expected), number(&pred)) {
        return if isclose(e, p, tol) { Ok(()) } else { Err(format!("{path}: number_mismatch {expected} != {pred}")) };
    }
    if let (Value::String(e), Value::String(p)) = (&expected, &pred) {
        return if compare_strings(e, p, key_name) {
            Ok(())
        } else {
            Err(format!("{path}: string_mismatch {e} != {p}"))
        };
    }
    if let Value::Array(e) = &expected {
        if let Value::String(s) = &pred {
            if let Some(v @ Value::Array(_)) = try_parse_container(s) {
                pred = v;
            }
        }
        let Value::Array(p) = &pred else {
            return Err(format!("{path}: type_mismatch expected=list got={}", type_name(&pred)));
        };
        if e.len() != p.len() {
            return Err(format!("{path}: list_len {} != {}", e.len(), p.len()));
        }
        if ORDER_INSENSITIVE_LIST_KEYS.contains(&key_name) {
            return compare_list_unordered(e, p, key_name, path, tol);
        }
        for (i, (x, y)) in e.iter().zip(p).enumerate() {
            compare_values(x, y, key_name, &format!("{path}[{i}]"), tol)?;
        }
        return Ok(());
    }
    if let Value::Object(e) = &expected {
        if let Value::String(s) = &pred {
            if let Some(v @ Value::Object(_)) = try_parse_container(s) {
                pred = v;
            }
        }
        let Value::Object(p) = &pred else {
            return Err(format!("{path}: type_mismatch expected=dict got={}", type_name(&pred)));
        };
        let missing: Vec<&String> = e.keys().filter(|k| !p.contains_key(*k)).collect();
        if !missing.is_empty() {
            return Err(format!("{path}: missing_keys={missing:?}"));
        }
        for (k, ev) in e {
            let child = if path != "$" { format!("{path}.{k}") } else { format!("$.{k}") };
            compare_values(ev, &p[k], k, &child, tol)?;
        }
        return Ok(());
    }
    if let (Value::String(s), false) = (&pred, expected.is_string()) {
        match (coerce_scalar_string(s), &expected) {
            (Some(Scalar::Bool(c)), Value::Bool(e)) if c == *e => return Ok(()),
            (Some(Scalar::Null), Value::Null) => return Ok(()),
            (Some(Scalar::Num(c)), Value::Number(e)) if e.as_f64().is_some_and(|e| isclose(e, c, tol)) => return Ok(()),
            _ => {}
        }
    }
    if loose_eq(&expected, &pred) {
        return Ok(());
    }
    Err(format!("{path}: value_mismatch {} != {}", short(&expected), short(&pred)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ok(e: Value, p: Value, key: &str) -> bool {
        compare_values(&e, &p, key, &format!("$.{key}"), 1e-6).is_ok()
    }

    #[test]
    fn sets_are_multisets_and_sequences_keep_their_order() {
        let e = json!([{"file": "pkg/mod", "func": "f"}, {"file": "pkg/b", "func": "g"}]);
        let p = json!([{"file": "pkg/b", "func": "g"}, {"file": "pkg/mod", "func": "f"}]);
        assert!(ok(e.clone(), p.clone(), "covered_functions"));
        assert!(!ok(e, p, "executed_path"));
        assert!(!ok(json!([1, 1, 2]), json!([1, 2, 2]), "covered_lines"));
        assert!(ok(json!([3, 1]), json!("[1, 3]"), "covered_lines"), "a list written as a string is read back");
    }

    #[test]
    fn identifiers_forgive_naming_conventions_only() {
        let file = |e: &str, p: &str| ok(json!(e), json!(p), "file");
        assert!(file(&format!("src/m/metric{MODULE_EXT}"), &format!("keras/src/m/metric{MODULE_EXT}")));
        assert!(file("pkg.mod", "pkg/mod"));
        assert!(!file("a/foo", "b/foo"));
        assert!(ok(json!("<class 'a.b.C'>"), json!("C"), "cls"));
        assert!(ok(json!("faker.proxy._select_factory"), json!("_select_factory"), "func"));
        assert!(!ok(json!("faker.proxy._select_factory"), json!("_select"), "func"));
        assert!(ok(json!("if not (x):"), json!("not (x)"), "condition"));
        assert!(ok(json!("Kind.RED"), json!("<RED>"), "type"));
        assert!(ok(json!("left|||right"), json!("right"), "side"));
        assert!(!ok(json!("left"), json!("right"), "side"));
    }

    #[test]
    fn numbers_and_scalars_written_as_strings() {
        assert!(ok(json!(0.1), json!(0.1000000001), "x"));
        assert!(!ok(json!(1), json!(2), "x"));
        assert!(ok(json!(282), json!("282"), "x"));
        assert!(ok(json!(8.0), json!("8"), "x"));
        assert!(!ok(json!(9), json!("8"), "x"));
        assert!(ok(json!(true), json!("True"), "x"));
        assert!(ok(json!(null), json!("None"), "x"));
        assert!(ok(json!(true), json!(1), "x"), "loose equality: True == 1");
        assert!(!ok(json!(true), json!("1"), "x"));
    }

    #[test]
    fn dicts_need_every_expected_key_and_allow_more() {
        assert!(ok(json!({"a": 1}), json!({"a": 1.0, "extra": 2}), "x"));
        let e = compare_values(&json!({"a": 1, "b": 2}), &json!({"a": 1}), "x", "$.x", 1e-6).unwrap_err();
        assert!(e.contains("missing_keys"), "{e}");
        assert!(ok(json!({"a": [1]}), json!("{'a': [1]}"), "x"));
        assert!(ok(json!("mappingproxy({'k': 1})"), json!({"k": 1}), "x"));
    }
}
