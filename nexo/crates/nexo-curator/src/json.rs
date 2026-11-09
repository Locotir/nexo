//! The token estimate of text and of JSON values written compactly (no spaces after `,` and `:`,
//! non-ASCII kept as is, floats in their shortest round-trip form), and a loose equality of decoded JSON
//! (numbers by value, `true == 1`, maps without order).

use serde_json::{Map, Number, Value};

/// Nominal cost of an image or document block.
pub const MEDIA_TOKENS: i64 = 1500;

pub use nexo_core::tokens::{text_cost, NEWLINE, TOKEN};

/// The cost of a JSON string: its text, as the quotes cost nothing.
fn str_cost(s: &str) -> usize {
    text_cost(s)
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

fn num_cost(n: &Number) -> usize {
    text_cost(&if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        float_repr(n.as_f64().unwrap_or(0.0))
    })
}

/// The cost of `json.dumps(strip_cache_control(strip_media(v)))` and the number of media blocks.
fn walk(v: &Value, strip: bool, n: &mut usize, media: &mut i64) {
    match v {
        Value::Null | Value::Bool(_) => *n += text_cost("null"),
        Value::Number(x) => *n += num_cost(x),
        Value::String(s) => *n += str_cost(s),
        Value::Array(a) => {
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
                    *n += str_cost("type") + str_cost(t);
                    return;
                }
            }
            object(m, strip, n, media, false);
        }
    }
}

fn object(m: &Map<String, Value>, strip: bool, n: &mut usize, media: &mut i64, media_block: bool) {
    for (key, val) in m {
        if key == "cache_control" || (strip && key == "signature") {
            continue;
        }
        *n += str_cost(key);
        if media_block && key == "source" {
            if let Value::Object(src) = val {
                for (sk, sv) in src {
                    if sk == "data" || sk == "cache_control" {
                        continue;
                    }
                    *n += str_cost(sk);
                    walk(sv, false, n, media);
                }
                continue;
            }
        }
        walk(val, strip, n, media);
    }
}

/// Estimated tokens ([`text_cost`] of the JSON), with images and documents at a nominal size.
pub fn est(v: &Value) -> i64 {
    let mut n = 0;
    let mut media = 0;
    walk(v, true, &mut n, &mut media);
    (n / TOKEN) as i64 + MEDIA_TOKENS * media
}

/// `est` of a string.
pub fn est_str(s: &str) -> i64 {
    (str_cost(s) / TOKEN) as i64
}

/// `est` and the media count.
pub fn est_media(v: &Value) -> (i64, i64) {
    let mut n = 0;
    let mut media = 0;
    walk(v, true, &mut n, &mut media);
    ((n / TOKEN) as i64 + MEDIA_TOKENS * media, media)
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
        // four words of one token each: the quotes, colons and commas cost nothing
        assert_eq!(est(&json!({"role": "user", "content": "hi"})), 4);
        let img = json!({"type": "image", "source": {"type": "base64", "data": "AAAA"}});
        // the keys and words of {"type":"image","source":{"type":"base64"}}: four words and "base64" (a word and a number); the data is not counted
        assert_eq!(est(&img), 6 + 1500);
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
        assert_eq!(est(&json!({"type": "thinking", "thinking": "x", "signature": "s"})), 2);
        // a key (103), a line break (37), a control character as punctuation (73), one wide character (80)
        assert_eq!(est(&json!({"a": "\n\u{1}é"})), 2);
    }
}
