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
        let hit = cached(&self.msgs, &now);
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
