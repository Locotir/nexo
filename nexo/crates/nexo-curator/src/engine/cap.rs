//! The size cap: what leaves the view when it is past the cap, priced against the cache it breaks.

use std::collections::BinaryHeap;

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;

use crate::hindsight::{Set, Sym, UKind};
use crate::json::est_str;
use crate::learn::MODEL;
use crate::ledger::{fold_forms, lines_of, said_lines, ON_DISK, POOL, SAID};
use crate::msg::get_str;
use crate::tools::{full_write, shell_writes, Kind};

use super::files::command_of;
use super::{Engine, NOTE_TOOL};

/// Tool outputs shorter than this (in tokens) are never left out in place: the stub that would stand for
/// them is a third of their size or more.
pub(super) const SMALL_RESULT: i64 = 100;

type Proposal = (usize, usize, Vec<usize>, i64);

/// A worth that orders as a number (the parts [`Engine::cover`] weighs are never NaN).
#[derive(PartialEq)]
struct Score(f64);

impl Eq for Score {}

impl PartialOrd for Score {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Score {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&o.0)
    }
}

impl Engine {
    /// Tokens of the outputs after the agent's latest text or note: what it has read and not yet written about.
    /// The last message where the agent wrote: text of its own, or a note.
    fn last_written(&self) -> Option<usize> {
        self.tr
            .units
            .iter()
            .rev()
            .find(|u| u.kind == UKind::Text || (u.kind == UKind::Call && u.tool == NOTE_TOOL))
            .map(|u| u.msg)
    }

    /// The first message whose outputs `Params.still` keeps from a guess: the latest outputs since the agent
    /// last wrote, newest first, while they fit the budget. `usize::MAX` keeps none.
    pub(super) fn still_from(&self, sizes: &[i64]) -> usize {
        let still = self.still_now();
        if still <= 0 {
            return usize::MAX;
        }
        let first = self.last_written().map_or(0, |m| m + 1);
        let (mut from, mut kept) = (sizes.len(), 0);
        for mi in (first..sizes.len()).rev() {
            if self.fz.by_msg(mi).iter().any(|&i| self.tr.units[i].kind == UKind::Result) {
                kept += sizes[mi];
                if kept > still {
                    break;
                }
            }
            from = mi;
        }
        from
    }

    /// The outputs the cap leaves out of its count (`Params.hold`): the ones the agent has not written about
    /// yet. With a budget for them (`Params.still`), only the latest that fit it: past it the older ones count,
    /// or a session where the agent never writes would grow past any cap.
    pub(super) fn held(&self, sizes: &[i64]) -> i64 {
        let from = if self.p.still > 0 { self.still_from(sizes).min(sizes.len()) } else { 0 };
        let mut held = 0;
        for mi in (from..sizes.len()).rev() {
            let units = self.fz.by_msg(mi).iter().map(|&i| &self.tr.units[i]);
            let mut units = units.filter(|u| u.kind == UKind::Text || u.kind == UKind::Call);
            if units.any(|u| u.kind == UKind::Text || u.tool == NOTE_TOOL) {
                break;
            }
            if self.fz.by_msg(mi).iter().any(|&i| self.tr.units[i].kind == UKind::Result) {
                held += sizes[mi];
            }
        }
        held
    }

    /// The cap past which the view is cut, the share of it a cut brings the view down to, and whether
    /// the cut is priced with `base` instead of `Params.eoq`. A cut is an order of the economic order
    /// quantity: it costs reading again without the cache what the view keeps, and the tokens it lets
    /// grow cost `ratio` of that on every request until the next cut. `base` is what no cut takes (the
    /// first message and the protected tail). When even the deepest cut leaves no room for one more
    /// step, the cap as priced would cut every request a little and the provider would read the whole
    /// view again each time (a long text pasted into the latest request does it): the cut is then priced
    /// with what it leaves, so the view grows as sent until the cut makes room.
    /// The requests a cut still saves rent for: `ahead` of the session so far (at least 4; none with `ahead=0`), and no more
    /// than until the cap would take what it cut for free.
    pub(super) fn horizon(&self, k: usize, view: i64) -> f64 {
        if self.p.ahead <= 0.0 {
            0.0
        } else {
            (self.p.ahead * k as f64).max(4.0).min(self.room(k, view))
        }
    }

    /// The steps the view has room to grow by before the cap cuts it (all of them without a cap).
    pub(super) fn room(&self, k: usize, view: i64) -> f64 {
        let g = self.raw_tokens as f64 / k.max(1) as f64;
        if self.cap > 0 && g > 0.0 {
            (self.cap - view).max(0) as f64 / g
        } else {
            f64::INFINITY
        }
    }

    pub(super) fn eoq_cap(&self, k: usize, base: i64) -> (i64, f64, bool) {
        let r = self.p.ratio;
        if r <= 0.0 {
            return (0, self.floor, false);
        }
        let g = self.raw_tokens as f64 / k.max(1) as f64;
        let cap_of = |e: f64| e + if r < 1.0 { (2.0 * (1.0 - r) * e * g / r).sqrt() } else { 0.0 };
        let mut e = self.p.eoq as f64;
        let stuck = base as f64 + g > cap_of(e);
        if stuck {
            e = base as f64;
        }
        let cap = (cap_of(e) as i64).max(1);
        (cap, e / cap as f64, stuck)
    }

    pub(super) fn prob(&self, x: &[f64]) -> f64 {
        if self.p.turn_head {
            MODEL.turn_prob(x)
        } else {
            MODEL.prob(x)
        }
    }

    /// The parts of the results in the view whose every line a later output in the view sends too, as
    /// (result, part), each with the parts that send those lines: the agent read the same lines again, and
    /// the later copy is the one it asked for. Only the lines a part still sends count: a part already left
    /// out carries nothing.
    fn copies(&self, vis: &[usize]) -> FxHashMap<(usize, usize), Vec<(usize, usize)>> {
        // each line sent below, with the latest part that sends it (`usize::MAX`: a line no part holds,
        // which never leaves)
        let mut later: FxHashMap<String, (usize, usize)> = FxHashMap::default();
        let mut out = FxHashMap::default();
        for &mi in vis.iter().rev() {
            for &i in self.fz.by_msg(mi).iter().rev() {
                if self.tr.units[i].kind != UKind::Result {
                    continue;
                }
                let text = self.tr.unit_text(i);
                let lines: Vec<&str> = text.split('\n').map(str::trim).collect();
                let have = self.dropped.get(&i);
                let mut owner: Vec<Option<usize>> = vec![Some(usize::MAX); lines.len()];
                for (j, c) in self.fz.chunks[i].iter().enumerate() {
                    let (a, b) = (c.start.max(0) as usize, (c.end.max(0) as usize).min(lines.len()));
                    let gone = have.is_some_and(|h| h.contains(&j));
                    owner[a.min(b)..b].iter_mut().for_each(|o| *o = (!gone).then_some(j));
                    if gone {
                        continue;
                    }
                    let own = &lines[a.min(b)..b];
                    if own.iter().any(|l| !l.is_empty()) && own.iter().all(|l| l.is_empty() || later.contains_key(*l)) {
                        let mut by: Vec<(usize, usize)> =
                            own.iter().filter(|l| !l.is_empty()).map(|l| later[*l]).collect();
                        by.sort_unstable();
                        by.dedup();
                        out.insert((i, j), by);
                    }
                }
                for (l, o) in lines.iter().zip(owner) {
                    if let (false, Some(j)) = (l.is_empty(), o) {
                        later.entry(l.to_string()).or_insert((i, j));
                    }
                }
            }
        }
        out
    }

    pub(super) fn evict(&mut self, t: usize, k: usize, sizes: &[i64], free: bool) -> Option<usize> {
        self.fz.context(&self.tr, t);
        let vis = self.visible();
        let copies = if self.p.sup > 0 { self.copies(&vis) } else { FxHashMap::default() };
        // a part that sends the lines of a copy left out (now or before) stays: they would be gone
        let mut carried = self.carrying.clone();
        // past the cap, a read a later edit made out of date goes with the copies, before any guess
        let stale = if free { self.stale_reads(&vis) } else { FxHashSet::default() };
        // what the agent read and has not written about yet is what it is working from: no guess takes it
        let still = self.still_from(sizes)..usize::MAX;
        // a part whose lines a later copy points to only moves down to that copy if it leaves: what it
        // saves is the share of its lines no copy points to; the lines it sends as a pointer to an earlier
        // output (`dedup`) cost next to nothing already, and leaving saves none of them either
        let mut pointed = self.pointed(|_| true);
        for (u, lines) in self.pointing(&vis) {
            let n = self.shown.get(&u).map_or(0, Vec::len);
            let held = pointed.entry(u).or_insert_with(|| vec![false; n]);
            held.iter_mut().take(lines.1).skip(lines.0).for_each(|l| *l = true);
        }
        // the latest reply's reasoning goes back with its calls (a provider refuses a call without it), so no
        // tail short enough to leave that reply open lets it go
        let last_reply =
            (0..t).rev().find(|&m| self.tr.msgs[m].get("role").and_then(Value::as_str) == Some("assistant"));
        let mut proposal: Vec<Proposal> = Vec::new();
        for &mi in vis.iter() {
            if mi as i64 >= t as i64 - self.p.protect {
                break;
            }
            for &i in self.fz.by_msg(mi) {
                let u = &self.tr.units[i];
                if u.kind == UKind::Thinking && Some(mi) == last_reply {
                    continue;
                }
                // a short output is a measurement or a verdict: its stub saves next to nothing, and running
                // the tool again to see it costs a whole request
                if u.saving == 0 || (u.kind == UKind::Result && u.tokens < SMALL_RESULT) {
                    continue;
                }
                let chunks = &self.fz.chunks[i];
                let have = self.dropped.get(&i);
                if have.map_or(0, |h| h.len()) == chunks.len() {
                    continue;
                }
                let more: Vec<usize> = (0..chunks.len())
                    .filter(|j| {
                        !have.is_some_and(|h| h.contains(j))
                            && (copies.contains_key(&(i, *j))
                                || (!carried.contains(&(i, *j))
                                    && (stale.contains(&i)
                                        || (self.guess
                                            && !still.contains(&mi)
                                            && self.prob(&self.fz.vector(&self.tr, i, *j)) < self.p.threshold))))
                    })
                    .collect();
                let free = |j: usize| -> i64 {
                    let Some(p) = pointed.get(&i) else { return chunks[j].tokens };
                    let n = p.len() as i64;
                    let at = |x: i64| (if x < 0 { (n + x).max(0) } else { x.min(n) }) as usize;
                    let (a, b) = (at(chunks[j].start), at(chunks[j].end));
                    let span = b.saturating_sub(a);
                    let held = p[a..b.max(a)].iter().filter(|x| **x).count();
                    if span == 0 {
                        chunks[j].tokens
                    } else {
                        chunks[j].tokens * (span - held) as i64 / span as i64
                    }
                };
                let more: Vec<usize> = more.into_iter().filter(|&j| free(j) > 0).collect();
                for j in &more {
                    carried.extend(copies.get(&(i, *j)).into_iter().flatten().copied());
                }
                if !more.is_empty() {
                    let tokens = more.iter().map(|&j| free(j)).sum();
                    proposal.push((self.tr.units[i].msg, i, more, tokens));
                }
            }
        }
        if proposal.is_empty() {
            self.held.clear();
            self.held_at = k;
            return None;
        }
        let best_at = if free { 0 } else { self.rent(t, k, sizes, &proposal)? };
        for (_, i, more, _) in &proposal[best_at..] {
            for &j in more {
                let rule = if copies.contains_key(&(*i, j)) {
                    "sup"
                } else if stale.contains(i) {
                    "stale"
                } else if free {
                    "cap_guess"
                } else {
                    "rent"
                };
                self.drop_one(*i, j, rule);
            }
            self.bump("dropped_chunks", more.len() as i64);
            for j in more {
                if let Some(by) = copies.get(&(*i, *j)) {
                    self.carrying.extend(by.iter().copied());
                    self.copied.insert((*i, *j));
                    self.bump("sup_chunks", 1);
                }
                self.held.remove(&(*i, *j));
            }
        }
        if free {
            self.held.clear();
            self.held_at = k;
        }
        self.bump("rewrites", 1);
        Some(proposal[best_at].0)
    }

    /// Past the cap: lets parts of results go, the safest first, until the view is down to ``target``.
    /// A part whose every line a later output in the view sends too is lost by nobody, and neither is a
    /// read a later edit of its file made out of date (`Params.stale`); after those, the parts least
    /// likely to be needed. What the oldest messages hold is no less needed for being old: the file read
    /// first is often the one the work is about.
    pub(super) fn need(&mut self, t: usize, sizes: &[i64], target: f64, only_copies: bool) -> Option<usize> {
        self.fz.context(&self.tr, t);
        let vis = self.visible();
        let stale = self.stale_reads(&vis);
        // with the need heads, a result is priced by the chance a reply to the horizon draws on it, per token
        // it holds (its parts go together, the least needed per token first); without them, part by part by
        // the keep head
        let hz = self.horizon(self.tr.steps.len(), sizes.iter().sum());
        let mut unit_need: FxHashMap<usize, f64> = FxHashMap::default();
        if !MODEL.need.is_empty() && !only_copies {
            self.need_context();
            for &i in vis.iter().flat_map(|&mi| self.fz.by_msg(mi)) {
                if self.tr.units[i].kind == UKind::Result {
                    if let Some(p) = MODEL.need_prob(&self.fz.unit_vector(&self.tr, i), hz) {
                        unit_need.insert(i, p / self.tr.units[i].tokens.max(1) as f64);
                    }
                }
            }
        }
        // the lines each visible output still sends, the latest first, with the part that sends them
        let mut later: FxHashMap<String, (usize, usize)> = FxHashMap::default();
        let mut carriers: FxHashMap<(usize, usize), Vec<(usize, usize)>> = FxHashMap::default();
        let mut parts: Vec<(f64, usize, usize, usize, i64, bool)> = Vec::new();
        for &mi in vis.iter().rev() {
            for &i in self.fz.by_msg(mi).iter().rev() {
                let u = &self.tr.units[i];
                if u.kind != UKind::Result {
                    continue;
                }
                let text = self.tr.unit_text(i).into_owned();
                let lines: Vec<&str> = text.split('\n').collect();
                let have = self.dropped.get(&i).cloned().unwrap_or_default();
                let chunks = &self.fz.chunks[i];
                let open = (mi as i64) < t as i64 - self.p.protect && u.saving > 0 && u.tokens >= SMALL_RESULT;
                for (j, c) in chunks.iter().enumerate() {
                    if have.contains(&j) {
                        continue;
                    }
                    let own = &lines[c.start.max(0) as usize..(c.end.max(0) as usize).min(lines.len())];
                    if open {
                        let copy = own.iter().all(|l| l.trim().is_empty() || later.contains_key(l.trim()));
                        if copy {
                            let mut by: Vec<(usize, usize)> =
                                own.iter().filter_map(|l| later.get(l.trim()).copied()).collect();
                            by.sort_unstable();
                            by.dedup();
                            carriers.insert((i, j), by);
                        }
                        if only_copies && !copy && !stale.contains(&i) {
                            for l in own.iter().map(|l| l.trim()).filter(|l| !l.is_empty()) {
                                later.entry(l.to_string()).or_insert((i, j));
                            }
                            continue;
                        }
                        let score = if copy || stale.contains(&i) {
                            -1.0
                        } else if let Some(&p) = unit_need.get(&i) {
                            p
                        } else {
                            self.prob(&self.fz.vector(&self.tr, i, j))
                        };
                        parts.push((score, mi, i, j, c.tokens, copy));
                    }
                    for l in own.iter().map(|l| l.trim()).filter(|l| !l.is_empty()) {
                        later.entry(l.to_string()).or_insert((i, j));
                    }
                }
            }
        }
        // the copies first: a part that sends a copy's lines may itself be one to leave, and it stays once
        // the copy has gone
        parts.sort_by(|a, b| a.0.total_cmp(&b.0).then(b.5.cmp(&a.5)).then(a.1.cmp(&b.1)));
        let mut carried = self.carrying.clone();
        let mut total = sizes.iter().sum::<i64>() as f64;
        let mut first: Option<usize> = None;
        for (score, mi, i, j, tokens, copy) in parts {
            if total <= target {
                break;
            }
            if !copy && carried.contains(&(i, j)) {
                continue;
            }
            let (rule, stat) = match (copy, score < 0.0) {
                (true, _) => ("cap_copies", "need_copies"),
                (false, true) => ("cap_stale", "need_stale"),
                _ => ("cap_need", "need_chunks"),
            };
            self.drop_one(i, j, rule);
            self.held.remove(&(i, j));
            self.bump(stat, 1);
            if copy {
                let by = carriers.remove(&(i, j)).unwrap_or_default();
                carried.extend(by.iter().copied());
                self.carrying.extend(by);
                self.copied.insert((i, j));
            }
            total -= tokens as f64;
            first = Some(first.map_or(mi, |f| f.min(mi)));
        }
        if first.is_some() {
            self.bump("rewrites", 1);
        }
        first
    }

    /// Past the cap: keeps the parts of results that tell the most per token and lets the rest go, until
    /// the view is down to ``target``. A part is worth the words it holds that nothing kept so far holds,
    /// each weighed by [`Featurizer::worth`] (rare words more, the words the current step asks about
    /// more again), so a second copy of something kept is worth nothing, whatever its age. Greedy by worth
    /// per token: the parts kept tell at least 1 − 1/e of what the best choice of the same size tells.
    pub(super) fn cover(&mut self, t: usize, sizes: &[i64], target: f64) -> Option<usize> {
        self.fz.context(&self.tr, t);
        let vis = self.visible();
        let mut known: FxHashSet<Sym> = FxHashSet::default();
        let mut parts: Vec<(usize, usize, usize, i64)> = Vec::new();
        let mut open_tokens = 0i64;
        for &mi in vis.iter() {
            for &i in self.fz.by_msg(mi) {
                let u = &self.tr.units[i];
                let open = u.kind == UKind::Result
                    && (mi as i64) < t as i64 - self.p.protect
                    && u.saving > 0
                    && u.tokens >= SMALL_RESULT;
                let have = self.dropped.get(&i);
                for (j, c) in self.fz.chunks[i].iter().enumerate() {
                    if have.is_some_and(|h| h.contains(&j)) {
                        continue;
                    }
                    // a part that sends the lines of a copy left out stays, as what is known
                    if open && !self.carrying.contains(&(i, j)) {
                        parts.push((mi, i, j, c.tokens));
                        open_tokens += c.tokens;
                    } else if u.kind == UKind::Result {
                        // what a request or a reply names is asked about, not told: only material covers a word
                        known.extend(c.terms.iter().copied());
                    }
                }
            }
        }
        let fixed = sizes.iter().sum::<i64>() - open_tokens;
        let mut room = target - fixed as f64;
        if open_tokens as f64 <= room {
            return None;
        }
        let worth = |known: &FxHashSet<Sym>, (_, i, j, tokens): &(usize, usize, usize, i64)| -> f64 {
            let gain: f64 =
                self.fz.chunks[*i][*j].terms.iter().filter(|w| !known.contains(w)).map(|&w| self.fz.worth(w)).sum();
            gain / (*tokens).max(1) as f64
        };
        // lazy greedy: a part's worth only falls as more is kept, so a stale top is weighed again; of two
        // parts worth the same, the later one stays
        let mut heap: BinaryHeap<(Score, usize)> =
            parts.iter().enumerate().map(|(n, p)| (Score(worth(&known, p)), n)).collect();
        let mut keep = vec![false; parts.len()];
        while let Some((Score(s), n)) = heap.pop() {
            let now = worth(&known, &parts[n]);
            if now < s && heap.peek().is_some_and(|(Score(top), _)| now < *top) {
                heap.push((Score(now), n));
                continue;
            }
            let tokens = parts[n].3 as f64;
            if tokens <= room {
                room -= tokens;
                keep[n] = true;
                known.extend(self.fz.chunks[parts[n].1][parts[n].2].terms.iter().copied());
            }
        }
        let mut first: Option<usize> = None;
        for (n, &(mi, i, j, _)) in parts.iter().enumerate() {
            if keep[n] {
                continue;
            }
            self.drop_one(i, j, "cap_cover");
            self.held.remove(&(i, j));
            self.bump("cover_chunks", 1);
            first = Some(first.map_or(mi, |f| f.min(mi)));
        }
        if first.is_some() {
            self.bump("rewrites", 1);
        }
        first
    }

    fn rent(&mut self, t: usize, k: usize, sizes: &[i64], proposal: &[Proposal]) -> Option<usize> {
        // a cut is credited with the rent it saves from now on, not the rent already paid: that is gone either
        // way. Credited with the rent since birth, cuts waited for a large view behind them and then broke the
        // cache over it: on the 656 recorded sessions, the forward credit forgets 14 % fewer words, rewrites
        // 31 % less and peaks 7 % lower, for 0.8 % more input cost.
        let hz = self.horizon(k, sizes.iter().sum());
        let mut held = FxHashMap::default();
        for (_, i, more, _) in proposal {
            let chunks = &self.fz.chunks[*i];
            for &j in more {
                held.insert((*i, j), self.p.ratio * chunks[j].tokens as f64 * hz);
            }
        }
        self.held = held;
        self.held_at = k;
        let mut cum = Vec::with_capacity(sizes.len() + 1);
        cum.push(0i64);
        for s in sizes {
            cum.push(cum[cum.len() - 1] + s);
        }
        let sent = self.tr.steps.last().map_or(t, |&s| s.min(t));
        // what the cache no longer holds (`Params.pockets` 2) is read again cut or not: a cut there costs nothing,
        // and what it saves there makes no cut before it cheaper
        let warm = if self.p.pockets >= 2 { self.warm } else { usize::MAX };
        let (mut best_at, mut saved, mut paid, mut bought) = (None, 0i64, 0.0f64, (0.0f64, 0.0f64));
        for n in (0..proposal.len()).rev() {
            let (f, i, more, tokens) = &proposal[n];
            if *f < warm {
                saved += tokens;
            }
            paid += more.iter().map(|j| self.held[&(*i, *j)]).sum::<f64>();
            if n > 0 && proposal[n - 1].0 == *f {
                continue;
            }
            let rest = (cum[(*f).max(warm).min(sent)] - cum[(*f).min(sent)] - saved).max(0);
            let price = self.p.margin * (1.0 - self.p.ratio) * rest as f64;
            let need = price;
            if paid >= need && paid - bought.0 >= need - bought.1 {
                best_at = Some(n);
                bought = (paid, need);
            }
        }
        best_at
    }

    /// The tokens the view may take in the model's window (0: no limit).
    pub fn set_budget(&mut self, tokens: i64) {
        self.budget = tokens.max(0);
    }

    /// The window: a view over [`Engine::set_budget`] would be refused by the provider, so the oldest
    /// messages leave until it fits (they go to the ledger, as past the cap), the protected tail first
    /// kept and then given up but for the latest message. What one message alone holds past the
    /// window is paged when the view is rendered (`Curator::view`). Whether any message left.
    pub(super) fn fit(&mut self) -> bool {
        if self.budget <= 0 {
            return false;
        }
        let sizes = self.sizes_now();
        if sizes.iter().sum::<i64>() <= self.budget {
            return false;
        }
        let mut left = self.leave(&sizes, self.budget as f64);
        let sizes = self.sizes_now();
        if sizes.iter().sum::<i64>() > self.budget {
            let protect = std::mem::replace(&mut self.p.protect, 1);
            left |= self.leave(&sizes, self.budget as f64);
            self.p.protect = protect;
        }
        self.bump("window_fits", 1);
        left
    }

    pub(super) fn leave(&mut self, sizes: &[i64], target: f64) -> bool {
        let t = self.tr.msgs.len() as i64;
        let mut total: i64 = sizes.iter().sum();
        let mut new_start = self.start;
        let mut mi = self.start;
        while (mi as i64) < (t - self.p.protect).min(t - 1) && total as f64 > target {
            total -= sizes[mi];
            mi += 1;
            if self.tr.msgs[mi].get("role").and_then(Value::as_str) == Some("assistant") {
                new_start = mi;
            }
        }
        if new_start == self.start {
            return false;
        }
        // a message that left the view is not built again: its built form goes with it
        for mi in self.start.max(1)..new_start {
            self.built.remove(&mi);
        }
        for mi in self.start..new_start {
            self.gone_tokens += self.sizes[mi];
            for &i in self.fz.by_msg(mi) {
                if self.tr.units[i].kind != UKind::Call {
                    continue;
                }
                let Some(info) = self.fz.info.get(&i) else { continue };
                match (info.kind, &info.path) {
                    (Kind::Write, Some(p)) if !p.is_empty() => *self.gone.entry(p.clone()).or_insert(0) += 1,
                    (Kind::Exec, _) => {
                        self.gone_commands += 1;
                        for w in shell_writes(&command_of(self.tr.block(i))) {
                            *self.gone.entry(w).or_insert(0) += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
        self.bump("left_messages", (new_start - self.start) as i64);
        self.tag_msgs(self.start..new_start, true, "leave");
        if self.p.ledger != 0 {
            // the pool keeps its latest `POOL` lines: the messages that left are read from the newest back,
            // and the read stops once they alone fill it (a long history leaving at once is not read whole)
            let mut batches: Vec<Vec<(usize, String, Set, f64)>> = Vec::new();
            let mut filled = 0;
            for mi in (self.start..new_start).rev() {
                if filled >= POOL {
                    break;
                }
                let mut batch = Vec::new();
                for &i in self.fz.by_msg(mi) {
                    if self.tr.units[i].kind == UKind::Thinking {
                        continue;
                    }
                    let text = self.tr.unit_text(i).into_owned();
                    // what was said weighs twice; the body of a file read or written is on disk and weighs less
                    let info = self.fz.info.get(&i);
                    let kind = self.tr.units[i].kind;
                    // an edit of a part of a file is the agent's own decision (what it renamed, what it set), which
                    // it reports again; a file written whole is on disk and pinned
                    let whole = kind == UKind::Call
                        && info.is_some_and(|x| x.kind == Kind::Write && x.path.is_some())
                        && full_write(get_str(self.tr.block(i), "name"), self.tr.block(i).get("input")).is_some();
                    let edit = kind == UKind::Call
                        && !whole
                        && info.is_some_and(|x| x.kind == Kind::Write && x.path.is_some());
                    let said = matches!(kind, UKind::Text | UKind::User)
                        || (kind == UKind::Call && get_str(self.tr.block(i), "name") == NOTE_TOOL)
                        || edit;
                    let w = match kind {
                        _ if said => SAID,
                        UKind::Result if info.is_some_and(|x| x.kind == Kind::Read && !x.error && x.path.is_some()) => {
                            ON_DISK
                        }
                        _ if whole => ON_DISK,
                        _ => 1.0,
                    };
                    // what was said keeps its rows of numbers whole: they are the values it stated
                    let mut lines = if said {
                        said_lines(&text, &mut self.int)
                    } else {
                        fold_forms(lines_of(&text, &mut self.int))
                    };
                    if kind == UKind::Call {
                        let old = self.superseded(i, mi);
                        if !old.is_empty() {
                            lines.retain(|(ln, _)| !old.contains(ln));
                        }
                    }
                    for (ln, ws) in lines {
                        batch.push((mi, ln, ws, w));
                    }
                }
                filled += batch.len();
                batches.push(batch);
            }
            for batch in batches.into_iter().rev() {
                self.pool.extend(batch);
            }
            if self.pool.len() > POOL {
                self.pool.drain(..self.pool.len() - POOL);
            }
        }
        self.harvest(self.start, new_start);
        self.start = new_start;
        // a line on a message that left goes back on its request: the view is read again whole anyway
        if !self.shut.is_empty() {
            self.shut_lines();
        }
        self.vis = None;
        let kept;
        (self.base_map, kept) = self.map();
        self.rules_shown = self.rules_block();
        self.kept_tokens = kept + est_str(&self.rules_shown);
        if self.p.ledger != 0 {
            // the ledger takes what the rules, the findings and the pinned files leave of 80% of the view
            let budget = self.p.ledger.min((0.8 * target) as i64 - self.kept_tokens);
            let text = self.ledger_text(budget);
            if !text.is_empty() {
                self.base_map += "\n\n";
                self.base_map += &text;
            }
        }
        self.compose();
        true
    }
}
