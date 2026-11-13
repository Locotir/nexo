//! Big messages in pieces. Chrome takes at most 1 MiB per message from a native host, and a screenshot or a page
//! read is bigger, so a message over the limit travels as `{"t":"chunk","cid":7,"i":0,"n":3,"d":"..."}` envelopes:
//! slices of its JSON text, which the receiver joins and parses again. The extension does the same in JavaScript.

use serde_json::{json, Value};
use std::collections::HashMap;

/// Most bytes of a message sent whole, and the size of a slice. Escaping quotes in a slice at most doubles it, so a
/// chunk stays far below Chrome's 1 MiB.
pub const CHUNK_BYTES: usize = 200_000;
/// A message that would take more chunks than this is refused.
pub const MAX_CHUNKS: usize = 1024;
/// Messages being reassembled at once.
pub const MAX_INFLIGHT: usize = 8;
/// The most bytes one reassembled message may take.
pub const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// The JSON of `msg` as the bodies to send: the message itself when it fits, else its chunks, numbered `cid`.
pub fn encode(msg: &Value, cid: u64) -> Vec<Vec<u8>> {
    let whole = serde_json::to_vec(msg).unwrap_or_default();
    if whole.len() <= CHUNK_BYTES {
        return vec![whole];
    }
    // The JSON text is valid UTF-8, so slices are cut on character boundaries.
    let text = String::from_utf8(whole).unwrap_or_default();
    let mut pieces: Vec<&str> = Vec::new();
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let mut cut = rest.len().min(CHUNK_BYTES);
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let (head, tail) = rest.split_at(cut);
        pieces.push(head);
        rest = tail;
    }
    let n = pieces.len();
    pieces
        .into_iter()
        .enumerate()
        .map(|(i, d)| {
            serde_json::to_vec(&json!({"t": "chunk", "cid": cid, "i": i, "n": n, "d": d})).unwrap_or_default()
        })
        .collect()
}

/// Why a chunk was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum ChunkError {
    /// Missing or malformed field, an index outside the count, or the same piece twice.
    Malformed(&'static str),
    /// Too many chunks, too many messages at once, or too many bytes.
    TooBig,
    /// The joined text is not JSON.
    NotJson,
}

impl std::fmt::Display for ChunkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChunkError::Malformed(why) => write!(f, "bad chunk: {why}"),
            ChunkError::TooBig => f.write_str("chunked message is too big"),
            ChunkError::NotJson => f.write_str("chunked message is not JSON"),
        }
    }
}

impl std::error::Error for ChunkError {}

struct Partial {
    slots: Vec<Option<String>>,
    got: usize,
    bytes: usize,
}

/// Joins chunks back into messages. Anything that is not a chunk passes through unchanged.
#[derive(Default)]
pub struct Reassembler {
    parts: HashMap<u64, Partial>,
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// `Ok(Some(message))` for a whole message (a plain one, or the last chunk of a split one), `Ok(None)` while
    /// pieces are still missing.
    pub fn push(&mut self, msg: Value) -> Result<Option<Value>, ChunkError> {
        if msg.get("t").and_then(Value::as_str) != Some("chunk") {
            return Ok(Some(msg));
        }
        let cid = msg.get("cid").and_then(Value::as_u64).ok_or(ChunkError::Malformed("no cid"))?;
        let i = msg.get("i").and_then(Value::as_u64).ok_or(ChunkError::Malformed("no index"))? as usize;
        let n = msg.get("n").and_then(Value::as_u64).ok_or(ChunkError::Malformed("no count"))? as usize;
        let d = msg.get("d").and_then(Value::as_str).ok_or(ChunkError::Malformed("no data"))?;
        if n == 0 || i >= n {
            return Err(ChunkError::Malformed("index outside the count"));
        }
        if n > MAX_CHUNKS {
            return Err(ChunkError::TooBig);
        }
        if !self.parts.contains_key(&cid) && self.parts.len() >= MAX_INFLIGHT {
            return Err(ChunkError::TooBig);
        }
        let part = self.parts.entry(cid).or_insert_with(|| Partial { slots: vec![None; n], got: 0, bytes: 0 });
        let refused = if part.slots.len() != n {
            Some(ChunkError::Malformed("the count changed"))
        } else if part.slots[i].is_some() {
            Some(ChunkError::Malformed("a piece came twice"))
        } else if part.bytes + d.len() > MAX_MESSAGE {
            Some(ChunkError::TooBig)
        } else {
            None
        };
        if let Some(e) = refused {
            self.parts.remove(&cid);
            return Err(e);
        }
        part.bytes += d.len();
        part.slots[i] = Some(d.to_string());
        part.got += 1;
        if part.got < n {
            return Ok(None);
        }
        let done =
            self.parts.remove(&cid).map(|p| p.slots.into_iter().flatten().collect::<String>()).unwrap_or_default();
        serde_json::from_str(&done).map(Some).map_err(|_| ChunkError::NotJson)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(bodies: &[Vec<u8>]) -> Vec<Value> {
        bodies.iter().map(|b| serde_json::from_slice(b).unwrap()).collect()
    }

    #[test]
    fn a_small_message_goes_whole_and_comes_back_unchanged() {
        let msg = json!({"t": "res", "id": 1, "r": {"ok": true}});
        let out = encode(&msg, 1);
        assert_eq!(out.len(), 1);
        let mut re = Reassembler::new();
        assert_eq!(re.push(parse(&out).remove(0)).unwrap(), Some(msg));
    }

    #[test]
    fn a_big_message_splits_under_chromes_limit_and_joins_back_in_any_order() {
        // Multi-byte characters, so a cut inside one would corrupt the text.
        let text: String = "añ€😀\"\\".repeat(150_000);
        let msg = json!({"t": "res", "id": 9, "r": {"data": text}});
        let out = encode(&msg, 7);
        assert!(out.len() >= 3, "{} chunks", out.len());
        assert!(out.iter().all(|b| b.len() < crate::frame::MAX_TO_CHROME), "every chunk fits in 1 MiB");
        let mut pieces = parse(&out);
        pieces.reverse();
        let mut re = Reassembler::new();
        let mut done = None;
        for (i, p) in pieces.into_iter().enumerate() {
            let r = re.push(p).unwrap();
            if i + 1 < out.len() {
                assert!(r.is_none());
            } else {
                done = r;
            }
        }
        assert_eq!(done.unwrap(), msg);
    }

    #[test]
    fn two_messages_can_interleave() {
        let a = json!({"k": "a".repeat(CHUNK_BYTES * 2 + 5)});
        let b = json!({"k": "b".repeat(CHUNK_BYTES * 2 + 5)});
        let (pa, pb) = (parse(&encode(&a, 1)), parse(&encode(&b, 2)));
        let mut re = Reassembler::new();
        let mut got = Vec::new();
        for (x, y) in pa.into_iter().zip(pb) {
            got.extend(re.push(x).unwrap());
            got.extend(re.push(y).unwrap());
        }
        assert_eq!(got, vec![a, b]);
    }

    #[test]
    fn a_repeated_or_out_of_range_piece_is_refused_and_forgotten() {
        let msg = json!({"k": "z".repeat(CHUNK_BYTES * 2)});
        let pieces = parse(&encode(&msg, 1));
        let mut re = Reassembler::new();
        assert_eq!(re.push(pieces[0].clone()).unwrap(), None);
        assert_eq!(re.push(pieces[0].clone()), Err(ChunkError::Malformed("a piece came twice")));
        // The partial message was dropped, so it can start again.
        assert_eq!(re.push(pieces[0].clone()).unwrap(), None);
        let bad = json!({"t": "chunk", "cid": 5, "i": 3, "n": 3, "d": "x"});
        assert_eq!(re.push(bad), Err(ChunkError::Malformed("index outside the count")));
        assert!(matches!(re.push(json!({"t": "chunk", "cid": 5})), Err(ChunkError::Malformed(_))));
    }

    #[test]
    fn limits_hold_on_chunk_count_messages_in_flight_and_bad_json() {
        let mut re = Reassembler::new();
        let too_many = json!({"t": "chunk", "cid": 1, "i": 0, "n": MAX_CHUNKS + 1, "d": "x"});
        assert_eq!(re.push(too_many), Err(ChunkError::TooBig));
        for cid in 0..MAX_INFLIGHT as u64 {
            assert_eq!(re.push(json!({"t": "chunk", "cid": cid, "i": 0, "n": 2, "d": "x"})).unwrap(), None);
        }
        let one_more = json!({"t": "chunk", "cid": 999, "i": 0, "n": 2, "d": "x"});
        assert_eq!(re.push(one_more), Err(ChunkError::TooBig));
        let mut re = Reassembler::new();
        assert_eq!(re.push(json!({"t": "chunk", "cid": 1, "i": 0, "n": 2, "d": "{not"})).unwrap(), None);
        assert_eq!(re.push(json!({"t": "chunk", "cid": 1, "i": 1, "n": 2, "d": " json"})), Err(ChunkError::NotJson));
    }
}
