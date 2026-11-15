//! The shapes of a scan: the periods, the tallies and the per-day digest.

use std::collections::BTreeMap;

/// A call that sends more than this many tokens is long context.
pub const LONG_CTX: u64 = 150_000;
/// A cache miss needs the call before it to have sent at least this much.
pub const MISS_FROM: u64 = 20_000;
/// A row is shown from this share of the usage.
pub const MIN_SHARE: f64 = 0.10;
/// Rows shown at most.
pub const MAX_ROWS: usize = 6;
/// Sessions read at most, newest first.
pub const MAX_FILES: usize = 400;
/// Bytes of sessions read at most.
pub const MAX_BYTES: u64 = 64 << 20;
/// A session file larger than this is left out.
pub const MAX_FILE: u64 = 32 << 20;
/// Files not touched for this many days are not opened.
pub const KEEP_DAYS: i64 = 30;
/// A model must carry this share of the spend to be called out.
pub const MODEL_SHARE: f64 = 0.5;
/// Commands remembered per tool and day.
pub(super) const TOP_KEEP: usize = 64;

/// The window of days the figures cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Day,
    Week,
    Month,
}

impl Period {
    pub const ALL: [Period; 3] = [Period::Day, Period::Week, Period::Month];

    pub fn days(self) -> i64 {
        match self {
            Period::Day => 1,
            Period::Week => 7,
            Period::Month => 30,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Period::Day => "today",
            Period::Week => "7 days",
            Period::Month => "30 days",
        }
    }

    /// The key that picks it.
    pub fn key(self) -> char {
        match self {
            Period::Day => 'd',
            Period::Week => 'w',
            Period::Month => 'm',
        }
    }

    pub fn from_key(c: char) -> Option<Period> {
        Period::ALL.into_iter().find(|p| p.key() == c)
    }
}

/// Counts of model calls. All additive.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tally {
    /// Model calls (the one line that sums a rewritten file is not one).
    pub calls: u64,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cost: f64,
    /// Calls that spent tokens and no money: the model has no price.
    pub unpriced: u64,
    pub long_calls: u64,
    pub long_input: u64,
    /// Calls that re-sent a prompt the cache no longer held, and the tokens they sent.
    pub misses: u64,
    pub resent: u64,
    /// Calls after a compaction, a rewind or a model switch: expected, not misses.
    pub rebuilds: u64,
    /// Money of the lines that say whose call they are, and the part of it that is not the conversation's.
    pub kinded_cost: f64,
    pub side_cost: f64,
    /// Tokens of history the view left out, and what the calls that said so sent.
    pub held: u64,
    pub held_base: u64,
    /// Input tokens and money of the lines that sum a rewritten file: in the totals, but none of
    /// the calls the shares are worked out over.
    pub summed_input: u64,
    pub summed_cost: f64,
}

impl Tally {
    pub fn total_input(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }

    pub fn add(&mut self, o: &Tally) {
        self.calls += o.calls;
        self.input += o.input;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.output += o.output;
        self.reasoning += o.reasoning;
        self.cost += o.cost;
        self.unpriced += o.unpriced;
        self.long_calls += o.long_calls;
        self.long_input += o.long_input;
        self.misses += o.misses;
        self.resent += o.resent;
        self.rebuilds += o.rebuilds;
        self.kinded_cost += o.kinded_cost;
        self.side_cost += o.side_cost;
        self.held += o.held;
        self.held_base += o.held_base;
        self.summed_input += o.summed_input;
        self.summed_cost += o.summed_cost;
    }

    /// Share of the input that came from the cache, 0..=100.
    pub fn cache_hit(&self) -> Option<u64> {
        (self.cache_read * 100).checked_div(self.total_input()).map(|h| h.min(100))
    }
}

/// What one tool's results weighed in a day.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolTally {
    pub runs: u64,
    /// Tokens of the results (about four characters each).
    pub tokens: u64,
    /// Tokens of the results times the calls that carried them: what they cost the context.
    pub weighted: u64,
    /// The program of the command (and its verb, for `cargo test`) where the tool has one, and its weighted tokens.
    pub top: BTreeMap<String, u64>,
}

impl ToolTally {
    pub(super) fn add(&mut self, o: &ToolTally) {
        self.runs += o.runs;
        self.tokens += o.tokens;
        self.weighted += o.weighted;
        for (k, v) in &o.top {
            *self.top.entry(k.clone()).or_default() += v;
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelTally {
    pub calls: u64,
    pub cost: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DayDigest {
    pub tally: Tally,
    pub tools: BTreeMap<String, ToolTally>,
    pub models: BTreeMap<String, ModelTally>,
}

/// One session file, boiled down.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Digest {
    pub id: String,
    pub cwd: String,
    /// Per local day (days since 1970-01-01).
    pub days: BTreeMap<i64, DayDigest>,
}
