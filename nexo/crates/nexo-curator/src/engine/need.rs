//! The names-kept rule (`Params.need`, off by default): a part of an output the agent has written about leaves
//! when no recent reply names it, each name it holds lives in another place of the view or stays on the first
//! line of the part that holds it, and the batch it leaves with pays for the view sent again.
//!
//! Why: of the tokens a session sends, most are outputs the agent read once and wrote about, and the cache
//! rent on them is paid on every call after. A part whose names all live elsewhere (the person's words, the
//! agent's own text and calls, another output) adds nothing a reader could miss by name; a part with names of
//! its own keeps the first line that holds each of them, as lines of the original text (the line-stay hook of
//! `frontier.rs`, `Trimmed.lines`), never a summary and never a new kind of text. The rule only waits and
//! prices: a part that was named in the last `need` replies is the agent's working material and stays.
//!
//! Simulated, not replayed on the view (`night-1110/need/STUDY.md`, sections 2 and 3, the recorded sessions
//! priced like the replay, from plain history): medium sessions cost 0.8519 of plain at 2 idle replies, 0.7685
//! with none and 0.8860 at 4; 0 forgotten words by construction; copied lines lost 185 in 42 replies at 2, 415
//! in 80 with no gate, 92 in 27 at 4. How much of that stacks on what the view already closes is not measured.
//!
//! The count of the places that hold each name is kept apart from the request: a message adds its names when it
//! enters, and is counted again only when what it holds changed (it left the view, or a part of it left),
//! which a signature of the message tells without reading its words. A part that left on lines adds the names
//! of those lines. What the counter does not know about (a message that went as a card or a pointer) counts as
//! holding nothing, so the error is always toward keeping a line, never toward losing a name.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use rustc_hash::FxHasher;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hindsight::{Sym, UKind};
use crate::sorted::{FxHashMap, FxHashSet};

use super::cap::SMALL_RESULT;
use super::frontier::{file_of, MARK_TOKENS};
use super::{Engine, NOTE_TOOL, RECALL_TOOL};

/// A part of an output: its unit and its place in it.
type Part = (usize, usize);

/// What the rule keeps between requests.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Need {
    /// How many places of the view hold each name: a part of an output it still sends, and each of the person's
    /// and the agent's texts and calls.
    pub(super) places: FxHashMap<Sym, u32>,
    /// By message, what it added to `places` (one name per place) and the signature of the message then.
    held: Vec<(u64, Vec<Sym>)>,
    /// Of a part that left on lines, the names those lines hold.
    kept: FxHashMap<Part, Vec<Sym>>,
    /// The parts that may leave once the rest of the rule agrees, by message.
    wait: BTreeMap<usize, Vec<Part>>,
    /// The messages before this one have been looked at for parts that wait.
    listed: usize,
}

/// A part chosen for the batch: its message, its place, the names only it holds, the lines that stay for them
/// and the tokens that leave with it.
struct Pick {
    msg: usize,
    part: Part,
    own: Vec<Sym>,
    stay: Vec<usize>,
    save: i64,
}

impl Engine {
    /// Takes the parts that pay out of the view, once the agent has written about their output and no recent
    /// reply names them. Returns the first message that changed.
    pub(super) fn need_cut(&mut self) -> Option<usize> {
        if self.p.need == 0 {
            return None;
        }
        self.need_count();
        self.need_list();
        if self.need.wait.is_empty() {
            return None;
        }
        // the last `need` replies of the agent: no part they name leaves, and no part after them
        let n = self.tr.msgs.len();
        let idle = self.p.need as usize;
        let agent = |e: &Self, m: usize| e.tr.msgs[m].get("role").and_then(Value::as_str) == Some("assistant");
        let window: Vec<usize> = (0..n).rev().filter(|&m| agent(self, m)).take(idle).collect();
        if window.len() < idle {
            return None;
        }
        let from = window[window.len() - 1];
        self.refresh_sticky();
        let named = if self.p.named != 0 { self.named_results() } else { FxHashSet::default() };
        let mut elig: Vec<(usize, Part)> = Vec::new();
        let mut gone: Vec<(usize, Part)> = Vec::new();
        {
            // what the agent and the person said from the first of those replies on: each name, and the
            // file's own name of a path
            let mut said: FxHashSet<&str> = FxHashSet::default();
            for u in (from..n).flat_map(|m| self.fz.by_msg(m)).map(|&i| &self.tr.units[i]) {
                if u.kind != UKind::Result {
                    for &w in u.words.iter().chain(&u.alias) {
                        let s = self.int.get(w);
                        said.insert(s);
                        said.extend(file_of(s));
                    }
                }
            }
            let took_up = |w: &Sym| {
                let s = self.int.get(*w);
                said.contains(s) || file_of(s).is_some_and(|t| said.contains(t))
            };
            for (&mi, parts) in &self.need.wait {
                if mi >= from {
                    break;
                }
                for &(i, j) in parts {
                    if self.dropped.get(&i).is_some_and(|d| d.contains(&j)) {
                        gone.push((mi, (i, j)));
                    } else if self.stk.keep.contains(&i)
                        || self.stk.reads.contains(&i)
                        || named.contains(&i)
                        || self.led.contains(&i)
                        || self.carrying.contains(&(i, j))
                        || self.fz.chunks[i][j].words.iter().any(took_up)
                    {
                        // it stays now and is looked at again at the next request
                    } else {
                        elig.push((mi, (i, j)));
                    }
                }
            }
        }
        for (mi, part) in gone {
            self.need_unlist(mi, part);
        }
        // the batch in order, the oldest first: a name two parts share leaves the first with no line and
        // keeps the second's, since it is then the only place that holds it
        let mut taken: FxHashMap<Sym, u32> = FxHashMap::default();
        let mut plan: Vec<Pick> = Vec::new();
        for (msg, part) in elig {
            let (i, j) = part;
            let (held, tokens) = {
                let c = &self.fz.chunks[i][j];
                (c.words.clone(), c.tokens)
            };
            let places = |w: &Sym| {
                self.need.places.get(w).copied().unwrap_or(0).saturating_sub(taken.get(w).copied().unwrap_or(0))
            };
            let own: Vec<Sym> = held.iter().copied().filter(|w| places(w) <= 1).collect();
            let (stay, size) = if own.is_empty() {
                (Vec::new(), MARK_TOKENS)
            } else {
                match self.stay_lines(i, j, &own) {
                    Some(x) => x,
                    None => continue,
                }
            };
            if tokens <= size {
                continue;
            }
            for w in held.iter().filter(|w| !own.contains(w)) {
                *taken.entry(*w).or_default() += 1;
            }
            plan.push(Pick { msg, part, own, stay, save: tokens - size });
        }
        let k = self.need_cut_at(&plan)?;
        let (mut parts, mut names, mut tokens) = (0, 0, 0);
        for p in plan.into_iter().filter(|p| p.msg >= k) {
            let (i, j) = p.part;
            self.drop_one(i, j, "need");
            if !p.stay.is_empty() {
                self.trimmed.lines.insert(p.part, p.stay);
            }
            parts += 1;
            names += p.own.len() as i64;
            tokens += p.save;
            self.need.kept.insert(p.part, p.own);
            self.need_unlist(p.msg, p.part);
        }
        self.bump("need_parts", parts);
        self.bump("need_names", names);
        self.bump("need_tokens", tokens);
        // the counts go back down now, not at the next request
        self.need_count();
        Some(k)
    }

    /// The message to cut at: the one where the rent saved on what leaves from it on, over the calls still to
    /// come (`ratio` of the tokens each), is more than what is sent again from it on at the price the cache
    /// does not take off; the one that gains most. Nothing when none does. With no pricing ahead
    /// (`ahead=0`), the oldest of the batch.
    fn need_cut_at(&mut self, plan: &[Pick]) -> Option<usize> {
        let first = plan.first()?.msg;
        if self.p.ahead <= 0.0 {
            return Some(first);
        }
        let sizes = self.sizes_now();
        let vis = self.visible();
        let (r, calls) = (self.p.ratio, self.calls_left());
        let mut best: Option<(usize, f64)> = None;
        let mut at = first;
        for p in plan {
            if p.msg < at {
                continue;
            }
            at = p.msg;
            let leave: i64 = plan.iter().filter(|q| q.msg >= at).map(|q| q.save).sum();
            let sent: i64 = vis.iter().filter(|&&m| m >= at).map(|&m| sizes[m]).sum();
            let gain = r * calls * leave as f64 - (1.0 - r) * (sent - leave).max(0) as f64;
            if gain > 0.0 && best.is_none_or(|(_, g)| gain > g) {
                best = Some((at, gain));
            }
            at += 1;
        }
        best.map(|(k, _)| k)
    }

    /// Lists the parts of the outputs the agent has written about since the last look: those before its latest
    /// text or note. The first and the last part of an output stay (what it printed first and its verdict), and
    /// so does an error, a failed output and what a `recall` brought back. The sticky set is asked at the cut,
    /// since it changes as the history grows.
    fn need_list(&mut self) {
        let Some(w) = self
            .tr
            .units
            .iter()
            .rev()
            .find(|u| u.kind == UKind::Text || (u.kind == UKind::Call && u.tool == NOTE_TOOL))
            .map(|u| u.msg)
        else {
            return;
        };
        let mut found: Vec<(usize, Part)> = Vec::new();
        for mi in self.need.listed..w {
            for &i in self.fz.by_msg(mi) {
                let u = &self.tr.units[i];
                let chunks = &self.fz.chunks[i];
                if u.kind != UKind::Result
                    || u.tool == RECALL_TOOL
                    || u.tokens < SMALL_RESULT
                    || chunks.len() < 3
                    || self.fz.info.get(&i).is_some_and(|x| x.error)
                {
                    continue;
                }
                for (j, c) in chunks.iter().enumerate().take(chunks.len() - 1).skip(1) {
                    if !c.error {
                        found.push((mi, (i, j)));
                    }
                }
            }
        }
        for (mi, part) in found {
            self.need.wait.entry(mi).or_default().push(part);
        }
        self.need.listed = self.need.listed.max(w);
    }

    fn need_unlist(&mut self, mi: usize, part: Part) {
        if let Some(v) = self.need.wait.get_mut(&mi) {
            v.retain(|p| *p != part);
            if v.is_empty() {
                self.need.wait.remove(&mi);
            }
        }
    }

    /// Brings `need.places` up to the view: a message is counted again only when its signature changed.
    pub(super) fn need_count(&mut self) {
        let n = self.tr.msgs.len();
        let mut inview = vec![false; n];
        for &m in self.visible().iter() {
            inview[m] = true;
        }
        if self.need.held.len() < n {
            self.need.held.resize(n, (u64::MAX, Vec::new()));
        }
        for (mi, &shown) in inview.iter().enumerate() {
            let sig = self.need_signature(mi, shown);
            if self.need.held[mi].0 == sig {
                continue;
            }
            let fresh = if shown && !self.shut.contains_key(&mi) && !self.frozen.contains_key(&mi) {
                self.need_names_of(mi)
            } else {
                Vec::new()
            };
            let old = std::mem::replace(&mut self.need.held[mi], (sig, fresh.clone())).1;
            for w in old {
                match self.need.places.get_mut(&w) {
                    Some(c) if *c > 1 => *c -= 1,
                    Some(_) => {
                        self.need.places.remove(&w);
                    }
                    None => {}
                }
            }
            for w in fresh {
                *self.need.places.entry(w).or_default() += 1;
            }
        }
    }

    /// What tells that message `mi` holds the same names as at the last count: whether it is in the view, shut
    /// or frozen, and which parts of its outputs left.
    fn need_signature(&self, mi: usize, shown: bool) -> u64 {
        let mut h = FxHasher::default();
        (shown, self.shut.contains_key(&mi), self.frozen.contains_key(&mi), self.shut_out.contains(&mi)).hash(&mut h);
        let (mut units, mut left) = (0usize, 0u64);
        for &i in self.fz.by_msg(mi) {
            units += 1;
            for &j in self.dropped.get(&i).into_iter().flat_map(|d| d.iter()) {
                left = left
                    .wrapping_add(((i as u64) << 20 ^ j as u64).wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
            }
        }
        h.write_usize(units);
        h.write_u64(left);
        // never the value a message that was not counted yet holds
        h.finish() >> 1
    }

    /// The names the places of message `mi` hold, one for each place: a part of an output it still sends (or
    /// the names of the lines a part that left kept), and each text and call of the person and the agent.
    fn need_names_of(&self, mi: usize) -> Vec<Sym> {
        let mut out: Vec<Sym> = Vec::new();
        for &i in self.fz.by_msg(mi) {
            let (u, have) = (&self.tr.units[i], self.dropped.get(&i));
            match u.kind {
                UKind::Thinking => {}
                UKind::Result => {
                    for (j, c) in self.fz.chunks[i].iter().enumerate() {
                        if have.is_some_and(|h| h.contains(&j)) {
                            if let Some(k) = self.need.kept.get(&(i, j)) {
                                out.extend_from_slice(k);
                            }
                        } else {
                            out.extend_from_slice(&c.words);
                        }
                    }
                }
                _ if have.is_none_or(|h| h.is_empty()) => out.extend_from_slice(&u.words),
                _ => {}
            }
        }
        out
    }
}
