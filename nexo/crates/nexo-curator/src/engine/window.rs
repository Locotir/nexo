//! The working window: a view aims at `target` tokens (the priced cap), is cut to `cap` whatever the
//! cache costs, and never passes `hard`. What the agent is working from (the sticky set) stays below
//! `hard`; no model and no directive decide it, only rules over the history.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::hindsight::UKind;
use crate::ledger::ERROR_MARKS;
use crate::tools::Kind;

use super::files::file_key;
use super::fixes::TEST_FAILED;
use super::Engine;

/// The token constants below, those of the spec in `lib.rs` and `SMALL_RESULT`, `STUB_TOKENS` and `WROTE_TOKENS` are in
/// accurate tokens (`nexo_core::tokens`), not the old count of 4 characters a token. Most are 1.3 times what they
/// were, the mean ratio of the two counts on real requests. `target`, `eoq` and `apart` are not: the replay fixtures
/// hold dense numbers and ids, where the two counts differ by more, and a plain 1.3 made the window cut earlier
/// than it did (forgotten words 227 to 351). They were set to 1.1, 2.5 and 1.75 times the old values so the 21
/// fixtures give what they gave under the old count (forgotten 231, DeepSeek $ 0.5368, replay $ 0.8459, priced
/// on the old count of 4 characters a token).
/// A result this small saves next to nothing when it leaves: its stub is as long.
const LEAVE_LEAST: i64 = 52;
/// What `still` keeps at least, whatever the sticky set takes.
const STILL_FLOOR: i64 = 5200;
/// The tokens of failing runs (a command whose output shows a failure) held below `cap`, newest first: what the
/// agent fixed is what it reports, and the lines of the failure are what it quotes.
const FAILURE_BUDGET: i64 = 2600;

/// The sticky set as of the last request, by unit.
#[derive(Default)]
pub(super) struct Sticky {
    /// Results that never leave by the window: the latest `file now` of each path, the last `check:`, and
    /// what the latest step returned.
    pub keep: FxHashSet<usize>,
    /// Results that leave only past `hard`: the latest whole read of each file the agent changed or the person
    /// named.
    pub reads: FxHashSet<usize>,
    /// Tokens of everything sticky: the request, the agent's own messages and the results in `keep`.
    pub tokens: i64,
    /// Result units a window cut took parts of, oldest first (for measurement).
    pub cut: Vec<usize>,
    /// Whether the output of each command result shows a failure, once read.
    failing: FxHashMap<usize, bool>,
}

impl Engine {
    /// The sticky set of the view as it stands (see [`Sticky`]). Rules over the history only: the request
    /// and the agent's own words are never results, so they never leave by the window.
    pub(super) fn refresh_sticky(&mut self) {
        let vis = self.visible();
        let t = self.tr.msgs.len();
        let shown: FxHashSet<usize> = vis.iter().copied().collect();
        let (mut keep, mut reads) = (FxHashSet::default(), FxHashSet::default());
        let (mut now_seen, mut read_seen) = (FxHashSet::<String>::default(), FxHashSet::<String>::default());
        let mut check_seen = false;
        let mut tokens = 0;
        let mut failing = std::mem::take(&mut self.stk.failing);
        let mut room = FAILURE_BUDGET;
        for i in (0..self.tr.units.len()).rev() {
            let u = &self.tr.units[i];
            if !shown.contains(&u.msg) {
                continue;
            }
            match u.kind {
                UKind::User | UKind::Text | UKind::Call => tokens += u.tokens,
                UKind::Thinking => {}
                UKind::Result => {
                    let info = self.fz.info.get(&i);
                    let (kind, path, error) =
                        info.map_or((Kind::Other, None, false), |x| (x.kind, x.path.clone(), x.error));
                    let mut sticky = u.msg + 1 >= t;
                    if !error && matches!(kind, Kind::Write | Kind::Exec) && (!check_seen || kind == Kind::Write) {
                        let text = self.tr.unit_text(i);
                        if !check_seen && text.contains("\ncheck: ") {
                            check_seen = true;
                            sticky = true;
                        }
                        if let (Kind::Write, Some(p)) = (kind, &path) {
                            if text.contains("file now") && now_seen.insert(p.clone()) {
                                sticky = true;
                            }
                        }
                    }
                    if !error {
                        let files: Vec<String> = match (kind, &path) {
                            (Kind::Read, Some(p)) => vec![p.clone()],
                            _ => self.shell_reads.get(&i).cloned().unwrap_or_default(),
                        };
                        // the whole body of a file the agent changed or the person named is what it works from
                        let mut latest = false;
                        for f in files {
                            let key = file_key(&f);
                            let work = self.written.contains_key(&key) || self.pins.contains_key(&key);
                            latest |= read_seen.insert(f) && work;
                        }
                        if latest && !sticky {
                            reads.insert(i);
                        }
                    }
                    // the output of a failing run is held below `cap` while the budget lasts
                    if !sticky && kind == Kind::Exec && u.tokens <= room {
                        let shows = *failing.entry(i).or_insert_with(|| {
                            let text = self.tr.unit_text(i);
                            error || ERROR_MARKS.is_match(&text) || TEST_FAILED.is_match(&text)
                        });
                        if shows {
                            reads.insert(i);
                            room -= u.tokens;
                        }
                    }
                    if sticky {
                        keep.insert(i);
                        tokens += u.tokens;
                    }
                }
            }
        }
        // the reads a request names are what it asks about
        for u in self.named_results() {
            if !keep.contains(&u) && shown.contains(&self.tr.units[u].msg) {
                reads.insert(u);
            }
        }
        let cut = std::mem::take(&mut self.stk.cut);
        self.stk = Sticky { keep, reads, tokens, cut, failing };
    }

    /// The tokens of the latest outputs the agent has not written about that a guess may not take (`still`):
    /// what the target leaves after the sticky set, at least [`STILL_FLOOR`], and never more than `still`.
    pub(super) fn still_now(&self) -> i64 {
        if self.p.still <= 0 {
            0
        } else if self.p.target > 0 && self.p.cap > 0 {
            self.p.still.min((self.p.target - self.head - self.stk.tokens).max(STILL_FLOOR))
        } else {
            self.p.still
        }
    }

    /// Whether the sticky set holds result `i` at the given tier: 0 below `cap`, 1 below `hard`.
    fn holds(&self, i: usize, tier: usize, unwritten: usize) -> bool {
        self.stk.keep.contains(&i) || (tier == 0 && (self.stk.reads.contains(&i) || self.tr.units[i].msg >= unwritten))
    }

    /// Past `cap` (or twice the `target`) the oldest results the sticky set does not hold leave, oldest first,
    /// until the view is down to the target, whatever the cache costs; past `hard` the oldest the sticky set holds besides the latest `file now` of each path and
    /// the last check leave too (their note says how to bring them back). The first message changed.
    pub(super) fn window_cut(&mut self) -> Option<usize> {
        if self.p.cap <= 0 {
            return None;
        }
        let sizes = self.sizes_now();
        let mut total: i64 = sizes.iter().sum::<i64>() + self.head;
        // the cut starts at `cap`, or at twice the target when that comes first, and lands on the target: one
        // rewrite buys a stretch of requests with a view that stays small, not a trim at every request
        let (start, land) = match self.p.target {
            t if t > 0 => (self.p.cap.min(2 * t), t),
            _ => (self.p.cap, self.p.cap),
        };
        if total <= start {
            return None;
        }
        self.refresh_sticky();
        let unwritten = self.still_from(&sizes);
        let vis = self.visible();
        let mut first: Option<usize> = None;
        let mut cut = 0;
        for tier in 0..2 {
            let limit = if tier == 0 { land } else { self.p.hard };
            if limit <= 0 {
                continue;
            }
            for &mi in vis.iter() {
                if total <= limit {
                    break;
                }
                let before = self.message(mi).1;
                let pictured = has_picture(&self.tr.msgs[mi]);
                let mut changed = false;
                for &i in self.fz.by_msg(mi).to_vec().iter() {
                    let u = &self.tr.units[i];
                    // a picture costs its nominal tokens whatever its text saves: its result leaves as a whole
                    if u.kind != UKind::Result
                        || (u.saving < LEAVE_LEAST && !pictured)
                        || self.holds(i, tier, unwritten)
                    {
                        continue;
                    }
                    let n = self.fz.chunks[i].len();
                    let mut did = false;
                    for j in 0..n {
                        if self.dropped.get(&i).is_some_and(|d| d.contains(&j)) || self.carrying.contains(&(i, j)) {
                            continue;
                        }
                        self.drop_one(i, j, if tier == 0 { "window_cap" } else { "window_hard" });
                        changed = true;
                        did = true;
                        cut += 1;
                    }
                    if did && !self.stk.cut.contains(&i) {
                        self.stk.cut.push(i);
                    }
                }
                if changed {
                    total += self.message(mi).1 - before;
                    first = Some(first.map_or(mi, |f| f.min(mi)));
                }
            }
            if tier == 1 && total > limit {
                self.bump("window_over_hard", 1);
            }
        }
        if cut > 0 {
            self.bump("window_cuts", cut);
            self.bump("rewrites", 1);
        }
        first
    }

    /// For the replay: the tokens of the sticky set at the last decision.
    pub fn sticky_tokens(&self) -> i64 {
        self.stk.tokens
    }

    /// For the replay: the (message, block) of each result a window cut took parts of.
    pub fn cut_blocks(&self) -> Vec<(usize, usize)> {
        self.stk.cut.iter().map(|&i| (self.tr.units[i].msg, self.tr.units[i].block)).collect()
    }
}

/// Whether a message holds an image or a document, in a tool result or on its own.
fn has_picture(m: &serde_json::Value) -> bool {
    fn walk(v: &serde_json::Value) -> bool {
        match v {
            serde_json::Value::Array(a) => a.iter().any(walk),
            serde_json::Value::Object(o) => {
                matches!(o.get("type").and_then(|t| t.as_str()), Some("image" | "document"))
                    || o.get("content").is_some_and(walk)
            }
            _ => false,
        }
    }
    m.get("content").is_some_and(walk)
}
