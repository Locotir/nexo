//! The native-messaging host of the nexo browser extension. Chrome frames every message as a 4-byte
//! little-endian length followed by that many bytes of JSON, in both directions, on the host's stdio.
//!
//! For now the host only answers: `ping` gets `pong`, and `navigate` / `title` requests are checked and
//! handed back to the extension as commands to run in the active tab. Driving it from nexo comes later.

use serde_json::{json, Value};
use std::io::{self, Read, Write};

/// Chrome refuses a message from the host above 1 MiB.
pub const MAX_OUT: usize = 1024 * 1024;
/// A message from the browser can be larger; this bounds what the host will allocate for one.
pub const MAX_IN: usize = 64 * 1024 * 1024;

/// Reads one frame. `None` when the stream ended cleanly before a frame started (Chrome closed the port).
pub fn read_message<R: Read>(r: &mut R) -> io::Result<Option<Value>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < len.len() {
        match r.read(&mut len[got..])? {
            0 if got == 0 => return Ok(None),
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => got += n,
        }
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_IN {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("message of {len} bytes is too large")));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Writes one frame and flushes it.
pub fn write_message<W: Write>(w: &mut W, msg: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(msg)?;
    if body.len() > MAX_OUT {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "message is over Chrome's 1 MiB limit"));
    }
    w.write_all(&(body.len() as u32).to_le_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

/// Only pages a person would open: no `javascript:`, `file:` or `chrome:` targets.
fn allowed_url(url: &str) -> bool {
    url == "about:blank" || url.starts_with("http://") || url.starts_with("https://")
}

fn error(id: &Value, text: &str) -> Value {
    json!({ "type": "error", "id": id, "error": text })
}

/// The reply to one message from the extension, if any.
pub fn handle(msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    match msg.get("type").and_then(Value::as_str) {
        Some("ping") => Some(json!({ "type": "pong", "id": id })),
        Some("navigate") => match msg.get("url").and_then(Value::as_str) {
            Some(url) if allowed_url(url) => Some(json!({ "type": "navigate", "id": id, "url": url })),
            Some(_) => Some(error(&id, "only http, https and about:blank can be opened")),
            None => Some(error(&id, "navigate needs a url")),
        },
        Some("title") => Some(json!({ "type": "title", "id": id })),
        // answers to commands the host sent; nothing consumes them yet
        Some("result" | "pong" | "error") => None,
        Some(other) => Some(error(&id, &format!("unknown message type {other}"))),
        None => Some(error(&id, "a message needs a type")),
    }
}

/// Serves frames until the input ends.
pub fn serve<R: Read, W: Write>(input: &mut R, output: &mut W) -> io::Result<()> {
    while let Some(msg) = read_message(input)? {
        if let Some(reply) = handle(&msg) {
            write_message(output, &reply)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(v: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        write_message(&mut out, v).unwrap();
        out
    }

    #[test]
    fn frame_has_a_little_endian_length_prefix() {
        let bytes = frame(&json!({"a": 1}));
        let n = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        assert_eq!(n, bytes.len() - 4);
        assert_eq!(serde_json::from_slice::<Value>(&bytes[4..]).unwrap(), json!({"a": 1}));
    }

    #[test]
    fn ping_gets_pong_over_the_wire() {
        let mut input = io::Cursor::new(frame(&json!({"type": "ping", "id": 7})));
        let mut output = Vec::new();
        serve(&mut input, &mut output).unwrap();
        let reply = read_message(&mut io::Cursor::new(output)).unwrap().unwrap();
        assert_eq!(reply, json!({"type": "pong", "id": 7}));
    }

    #[test]
    fn navigate_is_checked_and_handed_back() {
        let ok = handle(&json!({"type": "navigate", "id": 1, "url": "https://example.com/"})).unwrap();
        assert_eq!(ok, json!({"type": "navigate", "id": 1, "url": "https://example.com/"}));
        for bad in ["javascript:alert(1)", "file:///etc/passwd", "chrome://settings"] {
            let r = handle(&json!({"type": "navigate", "id": 2, "url": bad})).unwrap();
            assert_eq!(r["type"], "error", "{bad}");
        }
        assert_eq!(handle(&json!({"type": "navigate"})).unwrap()["type"], "error");
    }

    #[test]
    fn results_get_no_reply_and_unknown_types_get_an_error() {
        assert!(handle(&json!({"type": "result", "ok": true})).is_none());
        assert_eq!(handle(&json!({"type": "dance"})).unwrap()["type"], "error");
        assert_eq!(handle(&json!({})).unwrap()["type"], "error");
    }

    #[test]
    fn clean_end_is_none_and_a_cut_frame_is_an_error() {
        assert!(read_message(&mut io::Cursor::new(Vec::new())).unwrap().is_none());
        assert!(read_message(&mut io::Cursor::new(vec![9, 0])).is_err());
        assert!(read_message(&mut io::Cursor::new(vec![9, 0, 0, 0, b'{'])).is_err());
    }

    #[test]
    fn oversized_frames_are_refused() {
        let mut big = (MAX_IN as u32 + 1).to_le_bytes().to_vec();
        big.push(0);
        assert!(read_message(&mut io::Cursor::new(big)).is_err());
        let huge = json!({"s": "x".repeat(MAX_OUT)});
        assert!(write_message(&mut Vec::new(), &huge).is_err());
    }
}
