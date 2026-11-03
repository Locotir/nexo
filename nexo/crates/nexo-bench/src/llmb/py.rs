//! Two formats the distribution was written with: the canonical JSON its digests are taken over
//! (sorted keys, `", "` and `": "` separators, non-ASCII kept), and the literal syntax a model may
//! use for a list or a dict it writes as a string (`['a', 'b']`, `{'k': None}`, tuples).

use serde_json::{Map, Number, Value};

/// `v` as the canonical JSON the manifest's digests are taken over.
pub fn dumps_sorted(v: &Value) -> String {
    let mut out = String::new();
    write(v, &mut out);
    out
}

fn write(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => string(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write(x, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                string(k, out);
                out.push_str(": ");
                write(&m[k], out);
            }
            out.push('}');
        }
    }
}

fn string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `text` read as a literal (strings, numbers, `True`/`False`/`None`, lists, tuples, dicts with
/// string keys); None when it is not one, or is one JSON cannot hold (a set, a non-string key).
pub fn literal(text: &str) -> Option<Value> {
    let mut p = Parser { s: text.as_bytes(), i: 0, src: text };
    let v = p.value()?;
    p.ws();
    (p.i == p.s.len()).then_some(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    src: &'a str,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Option<Value> {
        self.ws();
        match *self.s.get(self.i)? {
            b'[' => {
                self.i += 1;
                Some(Value::Array(self.items(b']')?))
            }
            b'(' => {
                self.i += 1;
                Some(Value::Array(self.items(b')')?))
            }
            b'{' => {
                self.i += 1;
                self.dict()
            }
            b'\'' | b'"' => self.string().map(Value::String),
            b'-' | b'+' | b'0'..=b'9' | b'.' => self.number(),
            _ => {
                for (word, v) in [("True", Value::Bool(true)), ("False", Value::Bool(false)), ("None", Value::Null)] {
                    if self.src[self.i..].starts_with(word) {
                        let end = self.i + word.len();
                        if self.s.get(end).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
                            return None;
                        }
                        self.i = end;
                        return Some(v);
                    }
                }
                None
            }
        }
    }

    fn items(&mut self, close: u8) -> Option<Vec<Value>> {
        let mut out = Vec::new();
        loop {
            if self.eat(close) {
                return Some(out);
            }
            out.push(self.value()?);
            if !self.eat(b',') {
                return self.eat(close).then_some(out);
            }
        }
    }

    fn dict(&mut self) -> Option<Value> {
        let mut m = Map::new();
        if self.eat(b'}') {
            return Some(Value::Object(m));
        }
        loop {
            self.ws();
            let key = match self.s.get(self.i) {
                Some(b'\'' | b'"') => self.string()?,
                _ => return None,
            };
            if !self.eat(b':') {
                // `{'a', 'b'}` is a set
                return None;
            }
            let v = self.value()?;
            m.insert(key, v);
            if !self.eat(b',') {
                return self.eat(b'}').then_some(Value::Object(m));
            }
            if self.eat(b'}') {
                return Some(Value::Object(m));
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        let quote = self.s[self.i];
        self.i += 1;
        let mut out = String::new();
        let rest = &self.src[self.i..];
        let mut chars = rest.char_indices();
        while let Some((at, c)) = chars.next() {
            if c as u32 == quote as u32 {
                self.i += at + 1;
                return Some(out);
            }
            if c == '\n' {
                return None;
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            let (_, e) = chars.next()?;
            match e {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '0' => out.push('\0'),
                'a' => out.push('\u{7}'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'v' => out.push('\u{b}'),
                '\\' | '\'' | '"' => out.push(e),
                '\n' => {}
                'x' | 'u' | 'U' => {
                    let n = match e {
                        'x' => 2,
                        'u' => 4,
                        _ => 8,
                    };
                    let hex: String = (0..n).filter_map(|_| chars.next().map(|(_, c)| c)).collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                other => {
                    out.push('\\');
                    out.push(other);
                }
            }
        }
        None
    }

    fn number(&mut self) -> Option<Value> {
        let start = self.i;
        while self.i < self.s.len() && matches!(self.s[self.i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-' | b'_') {
            self.i += 1;
        }
        let t: String = self.src[start..self.i].chars().filter(|c| *c != '_').collect();
        let digits = t.trim_start_matches(['+', '-']);
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(n) = t.trim_start_matches('+').parse::<i64>() {
                return Some(Value::Number(n.into()));
            }
        }
        let f: f64 = t.parse().ok()?;
        Number::from_f64(f).map(Value::Number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_sorts_keys_and_keeps_non_ascii() {
        let v = json!({"b": [1, "é\n\"x\""], "a": {"z": null, "c": true}});
        assert_eq!(dumps_sorted(&v), r#"{"a": {"c": true, "z": null}, "b": [1, "é\n\"x\""]}"#);
        assert_eq!(dumps_sorted(&json!("\u{1}")), r#""\u0001""#);
    }

    #[test]
    fn literals_read_like_the_language_writes_them() {
        assert_eq!(literal("['a', \"b\", (1, 2.5), None, True]"), Some(json!(["a", "b", [1, 2.5], null, true])));
        assert_eq!(literal("{'k': -3, 'x': {'y': []},}"), Some(json!({"k": -3, "x": {"y": []}})));
        assert_eq!(literal("{'a', 'b'}"), None);
        assert_eq!(literal("{1: 'a'}"), None);
        assert_eq!(literal("[1, 2"), None);
        assert_eq!(literal("Nonesuch"), None);
        assert_eq!(literal("'it\\'s'"), Some(json!("it's")));
    }
}
