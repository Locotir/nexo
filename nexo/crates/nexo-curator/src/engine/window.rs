//! The working window: a view aims at `target` tokens (the priced cap), is cut to `cap` whatever the
//! cache costs, and never passes `hard`. What the agent is working from (the sticky set) stays below
//! `hard`; no model and no directive decide it, only rules over the history.

use rustc_hash::FxHashSet;

use crate::hindsight::UKind;
use crate::tools::Kind;

use super::files::file_key;
use super::Engine;

/// A result this small saves next to nothing when it leaves: its stub is as long.
const LEAVE_LEAST: i64 = 40;
/// What `still` keeps at least, whatever the sticky set takes.
const STILL_FLOOR: i64 = 4000;

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
        self.stk = Sticky { keep, reads, tokens, cut };
    }

    /// The tokens of the latest outputs the agent has not written about that a guess may not take (`still`):
    /// what the window leaves after the sticky set, at least [`STILL_FLOOR`], and never more than `still`.
    pub(super) fn still_now(&self) -> i64 {
        if self.p.still <= 0 {
            0
        } else if self.p.target > 0 && self.p.cap > 0 {
            self.p.still.min((self.p.cap - self.head - self.stk.tokens).max(STILL_FLOOR))
        } else {
            self.p.still
        }
    }

    /// Whether the sticky set holds result `i` at the given tier: 0 below `cap`, 1 below `hard`.
    fn holds(&self, i: usize, tier: usize, unwritten: usize) -> bool {
        self.stk.keep.contains(&i) || (tier == 0 && (self.stk.reads.contains(&i) || self.tr.units[i].msg >= unwritten))
    }

    /// Past `cap` the oldest results the sticky set does not hold leave, oldest first, whatever the cache
    /// costs; past `hard` the oldest the sticky set holds besides the latest `file now` of each path and
    /// the last check leave too (their note says how to bring them back). The first message changed.
    pub(super) fn window_cut(&mut self) -> Option<usize> {
        if self.p.cap <= 0 {
            return None;
        }
        let sizes = self.sizes_now();
        let mut total: i64 = sizes.iter().sum::<i64>() + self.head;
        if total <= self.p.cap {
            return None;
        }
        // a cut lands on the target, not on the cap, so the next requests do not cut again
        let land = if self.p.target > 0 { self.p.target } else { self.p.cap };
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
