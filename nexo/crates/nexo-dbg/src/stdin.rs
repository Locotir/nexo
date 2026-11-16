//! Program input as bytes: the escapes `run ... <<< "text"` and `stdin <text>` read, and the `stdin` command itself.
//! The input travels as bytes up to the pipe that feeds the program; across the session socket (JSON text) a run's
//! input goes as hex (`stdin --hex`).

use crate::engine::{rows, Session};
use crate::{usage, Result};
use nexo_bin::Output;
use serde_json::json;

/// The text with its escapes read: `\n` `\t` `\0` `\\` `\"` and `\xNN` (two hex digits, any byte). A backslash
/// before anything else stays as typed (a path like `C:\dir` keeps working); `\x` without two hex digits is an error.
pub fn decode(s: &str) -> std::result::Result<Vec<u8>, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' || i + 1 >= b.len() {
            out.push(b[i]);
            i += 1;
            continue;
        }
        let (byte, used) = match b[i + 1] {
            b'n' => (b'\n', 2),
            b't' => (b'\t', 2),
            b'0' => (0, 2),
            b'\\' => (b'\\', 2),
            b'"' => (b'"', 2),
            b'x' => {
                let h = s.get(i + 2..i + 4).and_then(|h| u8::from_str_radix(h, 16).ok());
                match h {
                    Some(v) if b[i + 2].is_ascii_hexdigit() && b[i + 3].is_ascii_hexdigit() => (v, 4),
                    _ => {
                        let seen: String = s[i..].chars().take(4).collect();
                        return Err(format!("{seen:?}: \\x takes two hex digits (\\xff, \\x00)"));
                    }
                }
            }
            _ => (b'\\', 1),
        };
        out.push(byte);
        i += used;
    }
    Ok(out)
}

fn from_hex(s: &str) -> std::result::Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return Err("--hex takes an even number of hex digits".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| "--hex takes hex digits only".to_string()))
        .collect()
}

/// `stdin <text> [--raw] [--exact] | stdin --hex <digits> [--raw] | stdin --close`.
pub fn exec(sess: &mut Session, words: &[String]) -> Result<Output> {
    let flag = |n: &str| words[1..].iter().any(|w| w == n);
    if flag("--close") {
        sess.t.stdin(None)?;
        return Ok(rows("stdin", vec!["standard input closed".into()], json!({})));
    }
    let pos: Vec<&str> = words[1..].iter().filter(|w| !w.starts_with("--")).map(String::as_str).collect();
    if pos.is_empty() {
        return usage(
            "stdin <text> (a newline is added; --raw sends exactly the text; escapes: \\n \\t \\0 \\\\ \\\" \\xNN, --exact reads none) | stdin --close",
        );
    }
    let text = pos.join(" ");
    let mut bytes = if flag("--hex") {
        from_hex(&text).or_else(usage)?
    } else if flag("--exact") {
        text.into_bytes()
    } else {
        decode(&text).or_else(usage)?
    };
    if !flag("--raw") {
        bytes.push(b'\n');
    }
    let n = sess.t.stdin(Some(&bytes))?;
    Ok(rows("stdin", vec![format!("sent {n} bytes")], json!({})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_become_bytes() {
        assert_eq!(decode("A\\x00\\xff\\n").unwrap(), [b'A', 0, 0xff, b'\n']);
        assert_eq!(decode("a\\0b\\\\c\\\"d\\te").unwrap(), b"a\0b\\c\"d\te");
        assert_eq!(decode("\\xAb").unwrap(), [0xab]);
    }

    #[test]
    fn other_backslashes_and_utf8_stay() {
        assert_eq!(decode("C:\\dir\\").unwrap(), b"C:\\dir\\");
        assert_eq!(decode("h\u{e9}").unwrap(), "h\u{e9}".as_bytes());
    }

    #[test]
    fn bad_hex_escape_is_an_error() {
        for t in ["\\x", "\\x4", "\\xzz", "a\\x4g"] {
            let e = decode(t).unwrap_err();
            assert!(e.contains("two hex digits"), "{t}: {e}");
        }
    }

    #[test]
    fn hex_roundtrip() {
        let b = [0u8, 1, 0x7f, 0x80, 0xff];
        assert_eq!(from_hex(&crate::rsp::hex(&b)).unwrap(), b);
        assert!(from_hex("abc").is_err());
    }
}
