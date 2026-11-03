//! The conversation the loop keeps, with what is asked of it on every request kept as it changes.
//!
//! Every change other than an append (a cut, a clear, a replacement, an edit through
//! [`History::edit`]) takes a new `version` from a counter shared by all histories, so
//! `(len, version)` equal to what a reader saw before means only appends happened since.
//! The size of each message is stored, so the token and byte totals of the whole history or of a
//! tail are sums of numbers, not a serialization of the messages. Any doubt: recompute.

use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};

use nexo_core::message::Message;

static VERSIONS: AtomicU64 = AtomicU64::new(1);

fn next_version() -> u64 {
    VERSIONS.fetch_add(1, Ordering::Relaxed)
}

/// The bytes of a message as the model API sees it (its JSON).
pub fn size_of(m: &Message) -> u64 {
    serde_json::to_string(m).map(|s| s.len() as u64).unwrap_or(0)
}

#[derive(Default)]
pub struct History {
    messages: Vec<Message>,
    /// The JSON length of each message.
    sizes: Vec<u64>,
    bytes: u64,
    /// `estimate_tokens` of the whole history: the sum of each message's length over 4.
    tokens: u64,
    version: u64,
}

impl Deref for History {
    type Target = Vec<Message>;
    fn deref(&self) -> &Vec<Message> {
        &self.messages
    }
}

impl History {
    pub fn new(messages: Vec<Message>) -> History {
        let mut h = History::default();
        h.replace(messages);
        h
    }

    /// Changes whenever the history changed in any way other than appends.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn push(&mut self, m: Message) {
        let n = size_of(&m);
        self.sizes.push(n);
        self.bytes += n;
        self.tokens += n / 4;
        self.messages.push(m);
    }

    pub fn truncate(&mut self, len: usize) {
        if len >= self.messages.len() {
            return;
        }
        self.messages.truncate(len);
        self.sizes.truncate(len);
        self.version = next_version();
        self.totals();
    }

    pub fn clear(&mut self) {
        self.replace(Vec::new());
    }

    pub fn replace(&mut self, messages: Vec<Message>) {
        self.sizes = messages.iter().map(size_of).collect();
        self.messages = messages;
        self.version = next_version();
        self.totals();
    }

    /// Any other change, made on the messages themselves.
    #[cfg(test)]
    pub fn edit<R>(&mut self, f: impl FnOnce(&mut Vec<Message>) -> R) -> R {
        let r = f(&mut self.messages);
        self.sizes = self.messages.iter().map(size_of).collect();
        self.version = next_version();
        self.totals();
        r
    }

    fn totals(&mut self) {
        self.bytes = self.sizes.iter().sum();
        self.tokens = self.sizes.iter().map(|n| n / 4).sum();
    }

    /// Same as `estimate_tokens(&history)`.
    pub fn tokens(&self) -> u64 {
        self.tokens
    }

    /// Same as `estimate_tokens(&history[from..])`.
    pub fn tokens_from(&self, from: usize) -> u64 {
        self.sizes[from.min(self.sizes.len())..].iter().map(|n| n / 4).sum()
    }

    /// The JSON bytes of the whole history.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::message::estimate_tokens;

    fn msg(i: usize) -> Message {
        Message::user("x".repeat(i % 37 + i))
    }

    fn exact(h: &History) {
        assert_eq!(h.tokens(), estimate_tokens(h));
        assert_eq!(h.bytes(), h.iter().map(size_of).sum::<u64>());
        for from in [0, 1, h.len() / 2, h.len(), h.len() + 3] {
            assert_eq!(h.tokens_from(from), estimate_tokens(&h[from.min(h.len())..]));
        }
    }

    #[test]
    fn totals_follow_every_kind_of_change() {
        let mut h = History::new((0..10).map(msg).collect());
        exact(&h);
        let v = h.version();
        h.push(msg(11));
        assert_eq!(h.version(), v, "an append keeps the version");
        exact(&h);
        h.truncate(4);
        assert_ne!(h.version(), v);
        exact(&h);
        let v = h.version();
        h.truncate(40);
        assert_eq!(h.version(), v, "a cut past the end changes nothing");
        h.edit(|m| m[1] = msg(300));
        assert_ne!(h.version(), v);
        exact(&h);
        h.clear();
        exact(&h);
    }

    #[test]
    fn two_histories_never_share_a_version() {
        assert_ne!(History::new(Vec::new()).version(), History::new(Vec::new()).version());
    }

    fn next(s: &mut u64) -> u64 {
        *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *s >> 33
    }

    /// A reader that trusts `(len, version)` must never take as unchanged a prefix that changed.
    #[test]
    fn a_thousand_random_edits_are_all_detected() {
        let mut s = 7u64;
        let mut h = History::new((0..50).map(|i| Message::user(format!("m{i}"))).collect());
        let (mut snap, mut seen_version) = (h.to_vec(), h.version());
        let mut detected = [0usize; 3];
        for round in 0..1000 {
            let kind = next(&mut s) % 3;
            match kind {
                0 => {
                    for _ in 0..next(&mut s) % 4 + 1 {
                        h.push(Message::user(format!("new{round}")));
                    }
                }
                1 => {
                    let at = next(&mut s) as usize % h.len().max(1);
                    h.edit(|m| m[at] = Message::user(format!("edit{round}")));
                }
                _ => {
                    let to = next(&mut s) as usize % h.len().max(1);
                    h.truncate(to);
                    // after a cut, appends may bring the length back past the old one
                    for _ in 0..next(&mut s) % 60 {
                        h.push(Message::user(format!("again{round}")));
                    }
                }
            }
            let only_appends = h.version() == seen_version && h.len() >= snap.len();
            if only_appends {
                assert_eq!(&h[..snap.len()], &snap[..], "round {round}: a change was taken for appends");
                assert_eq!(kind, 0, "round {round}: a non-append went undetected");
            } else {
                detected[kind as usize] += 1;
            }
            snap = h.to_vec();
            seen_version = h.version();
        }
        assert!(detected[1] > 100 && detected[2] > 100, "{detected:?}");
    }
}
