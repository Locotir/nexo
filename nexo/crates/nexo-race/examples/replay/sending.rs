use super::*;

/// The messages one way of sending a session sent last, with the tokens and distinctive words of each,
/// kept up to date by what changed at their end (the past a request keeps is not looked at again).
#[derive(Default)]
pub(super) struct Sending {
    pub(super) msgs: Vec<Value>,
    pub(super) tokens: Vec<i64>,
    pub(super) words: Vec<Vec<String>>,
    pub(super) bag: Bag,
}

impl Sending {
    /// Keeps the first `base` messages and adds `new`; returns the tokens a prefix cache holds.
    pub(super) fn extend(&mut self, base: usize, new: Vec<Value>) -> i64 {
        let base = base.min(self.msgs.len());
        let mut hit: i64 = self.tokens[..base].iter().sum();
        if let (Some(before), Some(now)) = (self.msgs.get(base), new.first()) {
            hit += grown(now, before);
        }
        self.cut(base);
        for m in new {
            self.push(m);
        }
        hit
    }

    /// Sends `now` in place of the last messages; returns the tokens a prefix cache holds, and whether the
    /// start of `now` differs from what was sent before (the past was rewritten).
    pub(super) fn send(&mut self, now: Vec<Value>) -> (i64, bool) {
        let hit = match marks() {
            Some(head) => marked(&self.msgs, &now, head),
            None => cached(&self.msgs, &now),
        };
        let same = now.iter().zip(&self.msgs).take_while(|(a, b)| a == b).count();
        let rewrote = same < self.msgs.len();
        self.cut(same);
        for m in now.into_iter().skip(same) {
            self.push(m);
        }
        (hit, rewrote)
    }

    pub(super) fn cut(&mut self, keep: usize) {
        for w in self.words.drain(keep.min(self.msgs.len())..) {
            self.bag.sub(&w);
        }
        self.msgs.truncate(keep);
        self.tokens.truncate(keep);
    }

    pub(super) fn push(&mut self, m: Value) {
        let words = distinctive_words(&text_of(&m));
        self.bag.add(&words);
        self.words.push(words);
        self.tokens.push(tokens_of(&m));
        self.msgs.push(m);
    }

    pub(super) fn total(&self) -> i64 {
        self.tokens.iter().sum()
    }
}

/// `REPLAY_MARKS`: the cache is read only at breakpoints, as on the Anthropic API. `last`: one at the end of the
/// last message; `head`: one after the first block of the first message too. Unset: any unchanged prefix is read.
fn marks() -> Option<bool> {
    static MARKS: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
    *MARKS.get_or_init(|| match std::env::var("REPLAY_MARKS").as_deref() {
        Err(_) => None,
        Ok("last") => Some(false),
        Ok("head") => Some(true),
        Ok(v) => {
            eprintln!("REPLAY_MARKS wants \"last\" or \"head\", not {v:?}");
            std::process::exit(2);
        }
    })
}

/// The tokens of `now` read from the cache when the last request left breakpoints only at the end of its last
/// message and, with `head`, after the first block of its first message: all it sent if `now` starts with all of
/// it (its last message may have grown), else that first block if it is unchanged, else nothing.
fn marked(prev: &[Value], now: &[Value], head: bool) -> i64 {
    let same = now.iter().zip(prev).take_while(|(a, b)| a == b).count();
    let whole = same == prev.len()
        || (same + 1 == prev.len()
            && now.get(same).is_some_and(|m| {
                let (x, y) = (blocks_of(m), blocks_of(&prev[same]));
                m.get("role") == prev[same].get("role") && x.len() >= y.len() && x[..y.len()] == y[..]
            }));
    if whole {
        return prev.iter().map(tokens_of).sum();
    }
    let first = |m: Option<&Value>| m.filter(|m| m["role"] == "user").and_then(|m| blocks_of(m).first().cloned());
    match (head, first(now.first()), first(prev.first())) {
        (true, Some(a), Some(b)) if a == b => tokens_of(&json!({ "content": [a] })),
        _ => 0,
    }
}

/// One way of sending a session, summed over its requests.
#[derive(Default)]
pub(super) struct Way {
    pub(super) miss: i64,
    pub(super) hit: i64,
    /// The most tokens one request sent.
    pub(super) peak: i64,
    /// Distinctive words the replies took from the history that this way had not sent with the request.
    pub(super) lost: usize,
    /// Replies with at least one such word.
    pub(super) lost_replies: usize,
    /// Requests that rewrote what an earlier one had sent (a compaction, a cut).
    pub(super) rewrites: usize,
}

impl Way {
    pub(super) fn count(&mut self, all: i64, hit: i64) {
        self.miss += all - hit;
        self.hit += hit;
        self.peak = self.peak.max(all);
    }

    pub(super) fn json(&self) -> Value {
        json!({
            "usd": dollars(self.miss, self.hit), "miss": self.miss, "hit": self.hit, "peak": self.peak,
            "lost": self.lost, "lost_replies": self.lost_replies, "rewrites": self.rewrites,
        })
    }
}
