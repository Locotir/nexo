//! Sizes of JSON values written compactly (no spaces after `,` and `:`, non-ASCII kept as is, floats
//! in their shortest round-trip form), the token estimate built on them, and a loose equality of
//! decoded JSON (numbers by value, `true == 1`, maps without order).

use serde_json::{Map, Number, Value};

/// Nominal cost of an image or document block.
pub const MEDIA_TOKENS: i64 = 1500;

/// Characters of a JSON string, quotes included.
pub fn str_chars(s: &str) -> usize {
    let mut n = 2;
    for &b in s.as_bytes() {
        if b & 0xc0 != 0x80 {
            n += 1;
        }
        if b < 0x20 {
            n += match b {
                b'\n' | b'\r' | b'\t' | 0x08 | 0x0c => 1,
                _ => 5,
            };
        } else if b == b'"' || b == b'\\' {
            n += 1;
        }
    }
    n
}

/// The shortest text of a float that reads back as it (`1.0`, `1e-05`, `1e+16`).
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let e = format!("{x:e}");
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let (sign, mant) = match mant.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mant),
    };
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let mut out = String::from(sign);
    if (-4..16).contains(&exp) {
        if exp >= 0 {
            let p = exp as usize + 1;
            if digits.len() <= p {
                out.push_str(&digits);
                out.push_str(&"0".repeat(p - digits.len()));
                out.push_str(".0");
            } else {
                out.push_str(&digits[..p]);
                out.push('.');
                out.push_str(&digits[p..]);
            }
        } else {
            out.push_str("0.");
            out.push_str(&"0".repeat((-exp - 1) as usize));
            out.push_str(&digits);
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exp.abs()));
    }
    out
}

fn num_chars(n: &Number) -> usize {
    if let Some(i) = n.as_i64() {
        i.to_string().len()
    } else if let Some(u) = n.as_u64() {
        u.to_string().len()
    } else {
        float_repr(n.as_f64().unwrap_or(0.0)).len()
    }
}

/// The characters the estimate gives a reasoning block of type `kind`: a few. Its opaque data (signature,
/// encrypted or withheld content) is counted apart by [`opaque_tokens`], and only while it is sent back.
pub fn thinking_chars(kind: &str) -> usize {
    8 + str_chars(kind) + 1
}

/// The tokens of the opaque data of the reasoning blocks in message `m` (a signature, encrypted content,
/// the data of a withheld block), at the estimate's four characters a token. A request sends it back only
/// for the messages after the person's latest one ([`is_person`]); earlier turns' reasoning is dropped by
/// the providers that take it, so it adds nothing there.
pub fn opaque_tokens(m: &Value) -> i64 {
    let Some(Value::Array(content)) = m.get("content") else { return 0 };
    let chars: usize = content
        .iter()
        .filter(|b| matches!(b.get("type").and_then(Value::as_str), Some("thinking" | "redacted_thinking")))
        .flat_map(|b| ["signature", "encrypted", "data"].into_iter().filter_map(|k| b.get(k)?.as_str()))
        .map(|s| s.chars().count())
        .sum();
    (chars / 4) as i64
}

/// Whether `m` is something the person said: a user message with text and no tool result.
pub fn is_person(m: &Value) -> bool {
    if m.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    match m.get("content") {
        Some(Value::String(_)) => true,
        Some(Value::Array(a)) => {
            let is =
                |b: &Value, names: &[&str]| b.get("type").and_then(Value::as_str).is_some_and(|t| names.contains(&t));
            a.iter().any(|b| is(b, &["text", "image"])) && !a.iter().any(|b| is(b, &["tool_result"]))
        }
        _ => false,
    }
}

/// Characters of `json.dumps(strip_cache_control(strip_media(v)))` and the number of media blocks.
fn walk(v: &Value, strip: bool, n: &mut usize, media: &mut i64) {
    match v {
        Value::Null => *n += 4,
        Value::Bool(b) => *n += if *b { 4 } else { 5 },
        Value::Number(x) => *n += num_chars(x),
        Value::String(s) => *n += str_chars(s),
        Value::Array(a) => {
            *n += 2 + a.len().saturating_sub(1);
            for x in a {
                walk(x, strip, n, media);
            }
        }
        Value::Object(m) => {
            if strip {
                let t = m.get("type").and_then(Value::as_str);
                if matches!(t, Some("image" | "document")) && m.get("source").is_some_and(Value::is_object) {
                    *media += 1;
                    object(m, false, n, media, true);
                    return;
                }
                if let Some(t @ ("thinking" | "redacted_thinking")) = t {
                    *n += thinking_chars(t);
                    return;
                }
            }
            object(m, strip, n, media, false);
        }
    }
}

fn object(m: &Map<String, Value>, strip: bool, n: &mut usize, media: &mut i64, media_block: bool) {
    let mut k = 0;
    *n += 2;
    for (key, val) in m {
        if key == "cache_control" || (strip && key == "signature") {
            continue;
        }
        k += 1;
        *n += str_chars(key) + 1;
        if media_block && key == "source" {
            if let Value::Object(src) = val {
                let mut j = 0;
                *n += 2;
                for (sk, sv) in src {
                    if sk == "data" || sk == "cache_control" {
                        continue;
                    }
                    j += 1;
                    *n += str_chars(sk) + 1;
                    walk(sv, false, n, media);
                }
                *n += j.max(1) - 1;
                continue;
            }
        }
        walk(val, strip, n, media);
    }
    *n += k.max(1) - 1;
}

/// Estimated tokens (~4 characters each), with images and documents at a nominal size.
pub fn est(v: &Value) -> i64 {
    let mut n = 0;
    let mut media = 0;
    walk(v, true, &mut n, &mut media);
    (n / 4) as i64 + MEDIA_TOKENS * media
}

/// `est` of a string.
pub fn est_str(s: &str) -> i64 {
    (str_chars(s) / 4) as i64
}

/// `est` and the media count.
pub fn est_media(v: &Value) -> (i64, i64) {
    let mut n = 0;
    let mut media = 0;
    walk(v, true, &mut n, &mut media);
    ((n / 4) as i64 + MEDIA_TOKENS * media, media)
}

/// A deep copy without `cache_control` keys.
pub fn strip_cc(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter().filter(|(k, _)| *k != "cache_control").map(|(k, v)| (k.clone(), strip_cc(v))).collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(strip_cc).collect()),
        x => x.clone(),
    }
}

fn num_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Compare an incoming value after stripping cache controls against an already stripped value,
/// without allocating a normalized copy. Scalar semantics are exactly those of `py_eq`.
pub(crate) fn py_eq_stripped(incoming: &Value, stripped: &Value) -> bool {
    match (incoming, stripped) {
        (Value::Array(a), Value::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| py_eq_stripped(x, y)),
        (Value::Object(a), Value::Object(b)) => {
            a.iter().filter(|(k, _)| *k != "cache_control").count() == b.len()
                && a.iter()
                    .filter(|(k, _)| *k != "cache_control")
                    .all(|(k, v)| b.get(k).is_some_and(|w| py_eq_stripped(v, w)))
        }
        _ => py_eq(incoming, stripped),
    }
}

/// Loose equality of decoded JSON (`1 == 1.0 == true`, maps compared without order).
pub fn py_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(p), Some(q)) => p == q,
            _ => match (x.as_u64(), y.as_u64()) {
                (Some(p), Some(q)) => p == q,
                _ => x.as_f64() == y.as_f64(),
            },
        },
        _ => match (num_f64(a), num_f64(b)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stripped_comparison_matches_normalization_and_python_equality() {
        let values = vec![
            json!(null),
            json!(true),
            json!(1),
            json!(1.0),
            json!(0),
            json!("1"),
            json!([{"cache_control": {"x": 1}, "n": [true, 2.0]}]),
            json!([{"n": [1, 2], "cache_control": false}]),
            json!({"cache_control": 1, "nested": {"cache_control": 2, "x": [1, false]}}),
            json!({"nested": {"x": [1.0, 0]}}),
            json!({"nested": {"x": [1, 1]}}),
            json!({}),
            json!([]),
        ];
        for a in &values {
            for b in &values {
                assert_eq!(py_eq_stripped(a, &strip_cc(b)), py_eq(&strip_cc(a), &strip_cc(b)), "{a} vs {b}");
            }
        }
    }

    #[test]
    fn reprs() {
        assert_eq!(float_repr(1.0), "1.0");
        assert_eq!(float_repr(-0.0), "-0.0");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(1.5e-5), "1.5e-05");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(float_repr(123.456), "123.456");
        assert_eq!(float_repr(1234567890123456.0), "1234567890123456.0");
    }

    #[test]
    fn sizes() {
        // {"role":"user","content":"hi"} is 30 characters
        assert_eq!(est(&json!({"role": "user", "content": "hi"})), 7);
        let img = json!({"type": "image", "source": {"type": "base64", "data": "AAAA"}});
        // {"type":"image","source":{"type":"base64"}}
        assert_eq!(est(&img), 43 / 4 + 1500);
        // an image inside a tool result counts too, at the same size, whatever its data
        let result = |parts: Value| json!({"type": "tool_result", "tool_use_id": "t", "content": parts});
        let text = json!({"type": "text", "text": "shot"});
        let image = |data: String| json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": data}});
        let (big, small) = (image("A".repeat(100_000)), image("A".into()));
        let bare = est(&result(json!([text])));
        let rise = est(&result(json!([text, big]))) - bare;
        assert!((1500..1600).contains(&rise), "the nominal size and the block's few characters, not its data: {rise}");
        assert_eq!(est(&result(json!([text, big]))), est(&result(json!([text, small]))));
        assert_eq!(est_media(&result(json!([text, big, small]))).1, 2);
        assert_eq!(est(&json!({"type": "thinking", "thinking": "x", "signature": "s"})), 19 / 4);
        assert_eq!(est(&json!({"a": "\n\u{1}é"})), 17 / 4);
    }

    #[test]
    fn opaque_reasoning_and_the_person_message() {
        let m = json!({"role": "assistant", "content": [
            {"type": "thinking", "thinking": "x", "encrypted": "E".repeat(400), "signature": "rs_1"},
            {"type": "redacted_thinking", "data": "D".repeat(40)},
            {"type": "text", "text": "T".repeat(4000)}]});
        assert_eq!(opaque_tokens(&m), (400 + 4 + 40) / 4);
        assert_eq!(opaque_tokens(&json!({"role": "user", "content": "hi"})), 0);
        assert!(is_person(&json!({"role": "user", "content": "hi"})));
        assert!(is_person(&json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})));
        assert!(!is_person(
            &json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "c", "content": "x"}]})
        ));
        assert!(!is_person(&m));
    }
}
