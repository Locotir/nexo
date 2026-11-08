//! Outputs and steps that go idle, and the rent that decides when they leave.

use rustc_hash::FxHashSet;
use serde_json::Value;

use crate::hindsight::{Sym, UKind};
use crate::learn::MODEL;

use super::cap::SMALL_RESULT;
use super::dups::clipped;
use super::steps::steps_of;
use super::{Engine, NOTE_TOOL};

/// The form of a name: each run of letters and digits that holds a digit becomes `#`, so `loc_3D60` and
/// `loc_2A10` share `loc_#`. None when no run is free of digits (a bare number or address).
fn form(w: &str) -> Option<String> {
    let (mut out, mut run, mut named) = (String::new(), String::new(), false);
    for c in w.chars().chain(std::iter::once(' ')) {
        if c.is_alphanumeric() {
            run.push(c);
            continue;
        }
        if run.chars().any(|c| c.is_ascii_digit()) {
            out.push('#');
        } else if !run.is_empty() {
            named = true;
            out.push_str(&run);
        }
        run.clear();
        out.push(c);
    }
    out.pop();
    named.then_some(out)
}

impl Engine {
    /// A request has just ended (`k`, the one before the latest): each step of the answered requests
    /// before it (a call and its output) that `k` drew nothing from leaves the view. Drawing on a step is
    /// writing, in a call or in text, a distinctive word of its output that neither the person's requests
    /// nor `k`'s own outputs hold: the agent can only have taken it from that step, or from a reply that
    /// did. While the requests that follow keep using a step, it stays (a second question on the same file
    /// finds the file read); the first request done without it may close it (`rent_close`). A step the
    /// latest request names (`need`, as `pick` takes them) stays too. `k`'s own steps wait for the
    /// request after it. The steps that may leave.
    pub(super) fn close_unused(&mut self, k: usize, j: usize) -> Option<FxHashSet<usize>> {
        let n = self.tr.msgs.len();
        let (ka, kb) = self.goal.span(k, n);
        let said = self.words_in(ka..kb, &[UKind::Text, UKind::Call]);
        let own = self.words_in(ka..kb, &[UKind::Result]);
        let asked = self.words_in(std::iter::once(0).chain(self.goal.turns.iter().map(|t| t.start)), &[UKind::User]);
        let said: FxHashSet<Sym> = said.into_iter().filter(|w| !asked.contains(w)).collect();
        // Only a word of some output shows where the request took what it knew: a request that read
        // nothing and used no output (a bare "[1, 2]", or words of the earlier replies) lets no step go.
        let told = !own.is_empty() || self.steps.iter().any(|s| !s.out && s.read.iter().any(|w| said.contains(w)));
        // close=2: every step leaves once its request is answered, used or not
        let (told, said) = if self.p.close >= 2 { (true, FxHashSet::default()) } else { (told, said) };
        let said: FxHashSet<Sym> = said.into_iter().filter(|w| !own.contains(w)).collect();
        let mut unused: FxHashSet<usize> = FxHashSet::default();
        if told {
            for (i, s) in self.steps.iter().enumerate() {
                if !s.out && !s.read.iter().any(|w| said.contains(w)) {
                    unused.insert(i);
                }
            }
        }
        if let Some((a, work)) = self.work_of(k) {
            for msgs in steps_of(&self.tr, work) {
                let s = self.new_step(a, msgs);
                if self.p.close >= 2 {
                    unused.insert(self.steps.len());
                }
                for &m in &s.msgs {
                    self.step_at.insert(m, self.steps.len());
                }
                self.steps.push(s);
            }
        }
        // what the latest request names stays: the steps recall would bring back for it
        let need = self.asks(j);
        for i in self.pick(&need, FxHashSet::default(), |s| !s.out) {
            unused.remove(&i);
        }
        told.then_some(unused)
    }

    /// The outputs of the request still open (`Params.live`), all but those after the agent's latest
    /// message: one that no agent message written after it drew on (a distinctive word of it that the
    /// person's requests do not hold, in text or in a call) goes idle; one drawn on is in use. The idle
    /// ones that pay for leaving now (the rent rule of `rent_close`) leave whole, each in its place, the
    /// call that asked for it kept. The agent works from what it reads: what it never took up in the calls
    /// and text that followed was not what it needed, and if it is, it can run the call again. The oldest
    /// message an output left from, if any left.
    pub(super) fn live_drop(&mut self, now: usize) -> Option<usize> {
        let (n, nt) = (self.tr.msgs.len(), self.goal.turns.len());
        // with `named`, the outputs of a file the latest request names stay: the request is about that file
        let kept = if self.p.named != 0 { self.named_results() } else { FxHashSet::default() };
        let (a, _) = self.goal.span(nt - 1, n);
        let asked = self.words_in(std::iter::once(0).chain(self.goal.turns.iter().map(|t| t.start)), &[UKind::User]);
        let (mut said, mut wrote, mut told) = (FxHashSet::default(), 0i64, false);
        let mut idle: Vec<(usize, usize, i64)> = Vec::new();
        for mi in (a + 1..n).rev() {
            if self.tr.msgs[mi].get("role").and_then(Value::as_str) == Some("assistant") {
                wrote += 1;
            }
            for &i in self.fz.by_msg(mi) {
                let u = &self.tr.units[i];
                match u.kind {
                    UKind::Text | UKind::Call => {
                        told |= u.kind == UKind::Text || u.tool == NOTE_TOOL;
                        // with `follow`, a name a call went for that the call's own output holds was followed
                        // there: it does not keep the older output that pointed to it in use
                        let got: FxHashSet<Sym> = if self.p.follow != 0 && u.kind == UKind::Call {
                            let res = self.fz.by_msg(mi + 1).iter().map(|&k| &self.tr.units[k]);
                            res.filter(|r| r.kind == UKind::Result).flat_map(|r| r.words.iter().copied()).collect()
                        } else {
                            FxHashSet::default()
                        };
                        said.extend(u.words.iter().copied().filter(|w| !asked.contains(w) && !got.contains(w)));
                    }
                    UKind::Result
                        if wrote >= self.p.live && u.tokens >= SMALL_RESULT && (told || self.p.digest == 0) =>
                    {
                        let gone = self.led.contains(&i)
                            || self.dropped.get(&i).map_or(0, |d| d.len()) == self.fz.chunks[i].len();
                        // an output that sends the lines of a copy left out stays: its stub says this one
                        // sends them, and with both gone they are lost
                        let carries = (0..self.fz.chunks[i].len()).any(|j| self.carrying.contains(&(i, j)));
                        if gone || carries || kept.contains(&i) {
                        } else if u.words.iter().any(|w| said.contains(w)) {
                            self.live_since.remove(&i);
                        } else {
                            idle.push((mi, i, u.tokens));
                        }
                    }
                    _ => {}
                }
            }
        }
        if idle.is_empty() {
            return None;
        }
        idle.reverse();
        // an output is priced at what it sends now: its chunks not dropped yet, less the lines that copies
        // the view keeps point to (were it to leave, those copies would send them)
        let going: FxHashSet<usize> = idle.iter().map(|&(mi, _, _)| mi).collect();
        let pointed = self.pointed(|m| !going.contains(&m));
        for e in idle.iter_mut() {
            e.2 = self.sends_now(e.1, pointed.get(&e.1));
        }
        let sizes = self.sizes_now();
        let mut after = vec![0i64; sizes.len() + 1];
        for mi in (0..sizes.len()).rev() {
            after[mi] = after[mi + 1] + sizes[mi];
        }
        // the cut is priced on the whole view behind it, the messages the previous request did not cache
        // too. Priced only on what it cached, as `rent` does, was measured on the 20 recorded sessions:
        // 1.2 % more input cost and 2 more forgotten words of 112.
        let hz = self.horizon(now, after[0]);
        let r = self.p.ratio;
        let (mut best, mut cut, mut paid, mut saved) = (0.0f64, None, 0.0f64, 0i64);
        for k in (0..idle.len()).rev() {
            let (mi, i, size) = idle[k];
            self.live_since.entry(i).or_insert(now);
            paid += r * size as f64 * hz;
            saved += size;
            let gain = paid - self.p.margin * (1.0 - r) * (after[mi] - saved).max(0) as f64;
            if gain > best {
                (best, cut) = (gain, Some(k));
            }
        }
        let cut = cut?;
        for &(_, i, _) in &idle[cut..] {
            let all = 0..self.fz.chunks[i].len();
            if self.p.lead != 0 {
                let lines = self.leads(i);
                if !lines.is_empty() {
                    self.lead_lines.insert(i, lines);
                }
            }
            self.drop_as(i, all, "live");
            self.led.insert(i);
            self.live_since.remove(&i);
            self.bump("live_dropped", 1);
        }
        self.vis = None;
        // the outputs are in the order of their messages
        Some(idle[cut].0)
    }

    /// The tokens output `i` sends now: its chunks not dropped yet, less the share of its lines that
    /// copies kept in the view point to (`pointed`, from `Engine::pointed`).
    pub(super) fn sends_now(&self, i: usize, pointed: Option<&Vec<bool>>) -> i64 {
        let (chunks, have) = (&self.fz.chunks[i], self.dropped.get(&i));
        let left: i64 =
            (0..chunks.len()).filter(|j| !have.is_some_and(|h| h.contains(j))).map(|j| chunks[j].tokens).sum();
        let held = pointed
            .map_or(0, |p| self.tr.units[i].tokens * p.iter().filter(|x| **x).count() as i64 / (p.len() as i64).max(1));
        (left - held).max(0)
    }

    /// The lines of output `i` that hold a lead: a name no other output holds (nothing printed it again,
    /// so nothing followed it yet), of a form the agent has written in its own text or calls.
    fn leads(&mut self, i: usize) -> Vec<String> {
        for k in self.lead_ix.seen..self.tr.units.len() {
            let u = &self.tr.units[k];
            match u.kind {
                UKind::Text | UKind::Call => {
                    self.lead_ix.forms.extend(u.words.iter().filter_map(|&w| form(self.int.get(w))))
                }
                UKind::Result => {
                    for &w in u.words.iter() {
                        *self.lead_ix.results.entry(w).or_default() += 1;
                    }
                }
                _ => {}
            }
        }
        self.lead_ix.seen = self.tr.units.len();
        let (forms, results) = (&self.lead_ix.forms, &self.lead_ix.results);
        let mine = &self.tr.units[i].words;
        let elsewhere_has = |w: &Sym| results.get(w).copied().unwrap_or(0) > u32::from(mine.contains(w));
        let text = self.tr.unit_text(i).into_owned();
        let mut out = Vec::new();
        for line in text.split('\n') {
            let ws = crate::hindsight::words(line, &mut self.int);
            let lead = |w: &Sym| !elsewhere_has(w) && form(self.int.get(*w)).is_some_and(|f| forms.contains(&f));
            if ws.iter().any(lead) {
                out.push(clipped(line, 200));
            }
        }
        out
    }

    /// How likely a reply within `hz` steps still draws on step `i`: the most likely of its units still in
    /// the view, by the need heads (`learn::Model::need_prob`); without them, of the parts of its outputs
    /// still in the view by the keep head, none for a step with no output left.
    fn step_need(&self, i: usize, hz: f64) -> f64 {
        let mut p: f64 = 0.0;
        let heads = !MODEL.need.is_empty();
        for &mi in &self.steps[i].msgs {
            for &u in self.fz.by_msg(mi) {
                let gone = self.dropped.get(&u);
                if heads {
                    if self.tr.units[u].kind == UKind::User || gone.is_some_and(|g| g.len() == self.fz.chunks[u].len())
                    {
                        continue;
                    }
                    let x = self.fz.unit_vector(&self.tr, u);
                    p = p.max(MODEL.need_prob(&x, hz).unwrap_or(0.0));
                    continue;
                }
                if self.tr.units[u].kind != UKind::Result {
                    continue;
                }
                for j in 0..self.fz.chunks[u].len() {
                    if !gone.is_some_and(|g| g.contains(&j)) {
                        p = p.max(self.prob(&self.fz.vector(&self.tr, u, j)));
                    }
                }
            }
        }
        p
    }

    /// The messages of the thread the latest request is on, as ranges, for the features that ask.
    fn active_ranges(&self) -> Vec<(usize, usize)> {
        let g = &self.goal;
        if g.turns.is_empty() {
            return Vec::new();
        }
        let n = self.tr.msgs.len();
        g.members[g.active()].iter().map(|&j| g.span(j, n)).collect()
    }

    /// Sets the featurizer's context to the step after the history so far (`Featurizer::context`).
    pub(super) fn need_context(&mut self) {
        let t = self.tr.msgs.len();
        let active = self.active_ranges();
        self.fz.set_active(active);
        self.fz.context(&self.tr, t);
    }

    /// The tokens step `i` takes in the view now (`sizes`: those of each message).
    fn step_size(&self, i: usize, sizes: &[i64]) -> i64 {
        let s = &self.steps[i];
        match s.at {
            Some(_) => s.tokens,
            None => s.msgs.iter().map(|&mi| sizes.get(mi).copied().unwrap_or(0)).sum(),
        }
    }

    /// The idle steps that pay for leaving now leave, in one batch: whatever goes out, the view after the
    /// first of them is read again without the cache once. That costs `(1 - ratio)` of the tokens behind
    /// the cut that stay. Keeping a step costs `ratio` of its tokens on each call; a cut is credited with the
    /// calls still to come (`horizon`: `ahead` times the calls so far), never with what keeping has already
    /// cost, which is paid either way. The price of the cut
    /// grows as the view grows behind it, so waiting too long can cost more than leaving early. The cut is
    /// the one with the largest gain, if any gains: the steps before it wait. Whether any step left.
    pub(super) fn rent_close(&mut self, idle: Vec<usize>) -> bool {
        let sizes = self.sizes_now();
        let now = self.tr.steps.len();
        let hz = self.horizon(now, sizes.iter().sum());
        let r = self.p.ratio;
        let going: FxHashSet<usize> = idle.iter().flat_map(|&i| self.steps[i].msgs.iter().copied()).collect();
        let moved = self.moved(|m| !going.contains(&m));
        // a step is worth the chance a reply still copies its output (`step_need`) times the price of the call
        // that would bring it back (the view read once more from the cache): one worth more than the rent it
        // costs to the horizon stays, whatever the cut behind it would cost. On the 656 recorded sessions: in
        // those over 20 requests 6.7 % fewer replies copy a word the view had lost, 2.9 % fewer tokens are read
        // without the cache; the sessions of 5 requests or fewer are sent as before. The same rule with the
        // mean of `step_need` for every step gets half of that: the prediction counts in the longest sessions.
        self.need_context();
        let call = r * sizes.iter().sum::<i64>() as f64;
        let mut cands: Vec<(usize, f64, i64)> = idle
            .into_iter()
            .filter_map(|i| {
                let back: i64 = self.steps[i].msgs.iter().map(|m| moved.get(m).copied().unwrap_or(0)).sum();
                let size = (self.step_size(i, &sizes) - back).max(0);
                let paid = r * size as f64 * hz - self.step_need(i, hz) * call;
                (paid > 0.0).then_some((i, paid, size))
            })
            .collect();
        cands.sort_by_key(|&(i, _, _)| self.steps[i].place());
        let mut after = vec![0i64; sizes.len() + 1];
        for mi in (0..sizes.len()).rev() {
            after[mi] = after[mi + 1] + sizes[mi];
        }
        // the cut is priced as if the view were read again without the cache from the request the steps were
        // made for: their line goes on the message before them that stays (`line_at`), never ahead of the
        // request, so the price is never below what the cut costs. Priced from that message instead, more
        // leaves sooner and more of what the replies use is lost (fixtures: 127 words forgotten, not 120).
        // The price also counts the messages after the previous request, which no cut makes dearer: priced
        // without them, as `rent` does, input cost falls 3.0 % on the 20 recorded sessions but 2 more of
        // 112 forgotten words are lost, so they stay as a margin.
        // what the cache no longer holds (`Params.pockets` 2) is read again closed or not: closing there costs
        // nothing, and what it saves there makes no closing before it cheaper
        let warm = if self.p.pockets >= 2 { self.warm } else { usize::MAX };
        let (mut best, mut cut, mut paid, mut saved, mut first) = (0.0f64, None, 0.0f64, 0i64, sizes.len());
        for n in (0..cands.len()).rev() {
            paid += cands[n].1;
            let s = &self.steps[cands[n].0];
            if s.place() < warm {
                saved += cands[n].2;
            }
            first = first.min(s.place()).min(s.a);
            let rest = (after[first] - after[first.max(warm).min(sizes.len())] - saved).max(0);
            let gain = paid - self.p.margin * (1.0 - r) * rest as f64;
            if gain > best {
                (best, cut) = (gain, Some(n));
            }
        }
        let Some(cut) = cut else { return false };
        for &(i, _, _) in &cands[cut..] {
            (self.steps[i].out, self.steps[i].at, self.steps[i].idle) = (true, None, None);
        }
        true
    }
}
