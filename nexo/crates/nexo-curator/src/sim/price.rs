//! The prefix cache and the token estimates the replay prices a view sequence with.

use serde_json::{json, Value};

/// A rough token count of a message (4 characters per token, as the engine estimates).
pub fn tokens_of(v: &Value) -> i64 {
    let text = match v.get("content") {
        Some(Value::String(s)) => s.len(),
        Some(c) => c.to_string().len(),
        None => v.to_string().len(),
    };
    (text as i64 + 3) / 4
}

/// The content blocks of a message: a text given as a string is one text block, as the API reads it.
pub fn blocks_of(m: &Value) -> Vec<Value> {
    match m.get("content") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) => vec![json!({"type": "text", "text": s})],
        _ => Vec::new(),
    }
}

/// The tokens of `now` a prefix cache holds after `prev` was sent: the messages sent unchanged at its
/// start and, of the first one that changed, the blocks it still starts with. The cache matches the
/// tokens the request starts with, not whole messages (a provider stores them in units of some tens of
/// tokens), so a message that only grew keeps what it had.
pub fn cached(prev: &[Value], now: &[Value]) -> i64 {
    let same = now.iter().zip(prev).take_while(|(a, b)| a == b).count();
    let hit: i64 = now[..same].iter().map(tokens_of).sum();
    hit + match (now.get(same), prev.get(same)) {
        (Some(a), Some(b)) => grown(a, b),
        _ => 0,
    }
}

/// A provider's prefix cache over time: the requests sent in the last `life` seconds, each with when it
/// was sent (`life` 0: only the last request, as a local server with one slot). With `slots` k, at most
/// the last k requests, whatever their age.
pub struct Cache {
    pub life: f64,
    /// The most requests kept; 0 means no limit by count (one when `life` is 0 too).
    pub slots: usize,
    pub sent: Vec<(Vec<Value>, f64)>,
}

impl Cache {
    pub fn new(life: f64, slots: usize) -> Cache {
        Cache { life, slots, sent: Vec::new() }
    }

    /// Sends `now` (after a system prompt of `system` tokens) at second `at`: the tokens it reads from the
    /// cache, the most any request still held starts it with.
    pub fn send(&mut self, now: &[Value], system: i64, at: f64) -> i64 {
        let life = self.life;
        if life > 0.0 {
            self.sent.retain(|(_, t)| at - t <= life);
        }
        let cap = match (self.slots, life > 0.0) {
            (0, true) => usize::MAX,
            (0, false) => 1,
            (k, _) => k,
        };
        if self.sent.len() > cap {
            self.sent.drain(..self.sent.len() - cap);
        }
        let hit = self.sent.iter().map(|(v, _)| cached(v, now)).max().map_or(0, |h| h + system);
        self.sent.push((now.to_vec(), at));
        hit
    }
}

/// What a token costs, in dollars per million: sent without the cache (`miss`), read from it (`hit`), and
/// written by the model (`out`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prices {
    pub miss: f64,
    pub hit: f64,
    pub out: f64,
}

impl Prices {
    /// deepseek-flash as the host prices it (`nexo-providers/src/presets.rs`).
    pub const LIVE: Prices = Prices { miss: 0.3, hit: 0.006, out: 1.2 };
    /// The replay's pair: input only.
    pub const LEGACY: Prices = Prices { miss: 0.28, hit: 0.028, out: 0.0 };

    pub fn named(name: &str) -> Option<Prices> {
        match name {
            "live" => Some(Prices::LIVE),
            "legacy" => Some(Prices::LEGACY),
            _ => None,
        }
    }

    /// The input cost of `miss` and `hit` tokens, as the replay's `dollars`.
    pub fn dollars(&self, miss: i64, hit: i64) -> f64 {
        (miss as f64 * self.miss + hit as f64 * self.hit) / 1e6
    }

    /// The cost of `out` tokens written.
    pub fn output(&self, out: i64) -> f64 {
        out as f64 * self.out / 1e6
    }
}

/// The tokens of `now` a prefix cache holds when `before` was sent in its place: the blocks both start
/// with, when they come from the same side.
pub fn grown(now: &Value, before: &Value) -> i64 {
    let (x, y) = (blocks_of(now), blocks_of(before));
    let kept = x.iter().zip(&y).take_while(|(p, q)| p == q).count();
    if now.get("role") == before.get("role") && kept > 0 {
        tokens_of(&json!({ "content": x[..kept] }))
    } else {
        0
    }
}
