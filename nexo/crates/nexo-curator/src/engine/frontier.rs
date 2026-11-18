//! The frontier trim (`Params.trim`): what the reply to an output did not take up leaves that output on the
//! next request, while it is still the last message the cache holds.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hindsight::{words, Sym, UKind};
use crate::json::est_str;
use crate::msg::key_of;
use crate::sorted::{FxHashMap, FxHashSet};
use crate::tools::{prints_files, Kind};

use super::cap::SMALL_RESULT;
use super::files::command_of;
use super::Engine;

/// The tokens of the line that says lines were omitted.
pub(super) const MARK_TOKENS: i64 = 7;

/// What the trim remembers of the parts it took (and what the names-kept rule of `need.rs` takes by the same
/// hook: the lines of a part that stay).
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Trimmed {
    /// Of a part that left holding names of its own, the lines of its output that stay: the first that
    /// holds each of them.
    pub(super) lines: FxHashMap<(usize, usize), Vec<usize>>,
    /// Those names, while those lines are all that holds them.
    only: FxHashSet<Sym>,
    /// The units looked at so far for who says or prints one of them.
    seen: usize,
}

/// A part to take: its output, its place in it, the names only it holds and its lines that stay for them.
type Take = (usize, usize, Vec<Sym>, Vec<usize>);

/// The file's own name in a path, when it is long enough to be a name by itself.
pub(super) fn file_of(w: &str) -> Option<&str> {
    w.rsplit_once('/').map(|(_, t)| t).filter(|t| t.len() >= 4)
}

impl Engine {
    /// Trims message `mi`, the last one of the view sent before, once the agent has replied to it. Of each
    /// of its outputs the first and the last part stay, and so do a part that reports an error, an output
    /// that failed or that the sticky set holds (a failing run, a read the agent works from, what a
    /// `recall` brought back), and every part with a name the reply (or the person) said, whole or as the
    /// file's own name: that is what the agent took up. Of the parts between, one that names nothing the
    /// rest of the view does not hold leaves (a run printed again, progress lines): no name is lost with
    /// it. From 2 on, a part with names of its own leaves too, but for the first line that holds each of
    /// them, when those lines are half of the part at most (a listing, all names, stays). 1 and 2 take
    /// only from what a process printed; 3 takes from every output, a file read too. The rewrite is the
    /// cheapest there is, as the cache holds everything before that message: it is made when the rent the
    /// parts would pay over the calls still to come (`ratio` of their tokens each) is more than sending
    /// what stays of the message, and the reply behind it, once more at the full price. Returns the
    /// message, if it changed.
    pub(super) fn frontier_trim(&mut self, mi: usize) -> Option<usize> {
        self.names_said();
        let n = self.tr.msgs.len();
        if mi + 1 >= n {
            return None;
        }
        let level = self.p.trim;
        let small = |i: usize| self.tr.units[i].tokens < SMALL_RESULT || self.fz.chunks[i].len() < 3;
        let outs: Vec<usize> = self
            .fz
            .by_msg(mi)
            .iter()
            .copied()
            .filter(|&i| self.tr.units[i].kind == UKind::Result && !small(i))
            .filter(|i| !self.fz.info.get(i).is_some_and(|x| x.error) && !self.led.contains(i))
            .filter(|&i| level >= 3 || self.process_output(i))
            .collect();
        if outs.is_empty() {
            return None;
        }
        self.refresh_sticky();
        let named = if self.p.named != 0 { self.named_results() } else { FxHashSet::default() };
        let mut cands: Vec<(usize, usize)> = Vec::new();
        {
            // what the agent (or the person) said after it: each name, and the file's own name of a path
            let mut said: FxHashSet<&str> = FxHashSet::default();
            for u in (mi + 1..n).flat_map(|m| self.fz.by_msg(m)).map(|&i| &self.tr.units[i]) {
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
            for i in outs {
                if self.stk.keep.contains(&i) || self.stk.reads.contains(&i) || named.contains(&i) {
                    continue;
                }
                let (chunks, have) = (&self.fz.chunks[i], self.dropped.get(&i));
                for (j, c) in chunks.iter().enumerate().take(chunks.len() - 1).skip(1) {
                    let out = have.is_some_and(|h| h.contains(&j)) || self.carrying.contains(&(i, j));
                    if !out && !c.error && !c.words.iter().any(took_up) {
                        cands.push((i, j));
                    }
                }
            }
        }
        if cands.is_empty() {
            return None;
        }
        // the places of the view that hold each name: a part of an output it still sends, or what the
        // person and the agent wrote
        let mut places: FxHashMap<Sym, u32> = FxHashMap::default();
        for &m in self.visible().iter() {
            for &i in self.fz.by_msg(m) {
                let (u, have) = (&self.tr.units[i], self.dropped.get(&i));
                match u.kind {
                    UKind::Thinking => {}
                    UKind::Result => {
                        for (j, c) in self.fz.chunks[i].iter().enumerate() {
                            if !have.is_some_and(|h| h.contains(&j)) {
                                for &w in c.words.iter() {
                                    *places.entry(w).or_default() += 1;
                                }
                            }
                        }
                    }
                    _ if have.is_none_or(|h| h.is_empty()) => {
                        for &w in u.words.iter() {
                            *places.entry(w).or_default() += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut plan: Vec<Take> = Vec::new();
        for (i, j) in cands {
            let held: Vec<Sym> = self.fz.chunks[i][j].words.clone();
            let own: Vec<Sym> = held.iter().copied().filter(|w| places.get(w).is_none_or(|&h| h <= 1)).collect();
            let mut stay: Vec<usize> = Vec::new();
            if !own.is_empty() {
                if level < 2 {
                    continue;
                }
                match self.stay_lines(i, j, &own) {
                    Some((lines, _)) => stay = lines,
                    None => continue,
                }
            }
            // the names the part shares are held one place less; its own stay held, on those lines
            for w in held.iter().filter(|w| !own.contains(w)) {
                if let Some(h) = places.get_mut(w) {
                    *h -= 1;
                }
            }
            plan.push((i, j, own, stay));
        }
        if plan.is_empty() {
            return None;
        }
        // priced on the message as it would go: the cache keeps its blocks before the first one that changes
        let before = self.message(mi).1;
        for (i, j, _, stay) in &plan {
            self.dropped.entry(*i).or_default().insert(*j);
            if !stay.is_empty() {
                self.trimmed.lines.insert((*i, *j), stay.clone());
            }
        }
        let after = self.message(mi).1;
        for (i, j, _, _) in &plan {
            if let Some(d) = self.dropped.get_mut(i) {
                d.remove(j);
                if d.is_empty() {
                    self.dropped.remove(i);
                }
            }
        }
        let first = plan.iter().map(|t| self.tr.units[t.0].block).min().unwrap_or(0);
        let whole = |i: &&usize| self.tr.units[**i].block < first && !self.dropped.contains_key(*i);
        let lead: i64 = self.fz.by_msg(mi).iter().filter(whole).map(|&i| self.tr.units[i].tokens).sum();
        // the reply was read from the cache behind that message: it goes again too
        let agent = |m: &usize| self.tr.msgs[*m].get("role").and_then(Value::as_str) == Some("assistant");
        let reply: i64 = (mi + 1..n).take_while(agent).map(|m| self.sizes[m]).sum();
        let r = self.p.ratio;
        let to_come = self.calls_left();
        let again = ((after - lead).max(0) + reply) as f64;
        let pays = self.p.ahead <= 0.0 || r * to_come * (before - after) as f64 > (1.0 - r) * again;
        if !pays || after >= before {
            for (i, j, _, _) in &plan {
                self.trimmed.lines.remove(&(*i, *j));
            }
            // (the message as it was priced is not the message that goes)
            self.built.remove(&mi);
            return None;
        }
        let (parts, mut names) = (plan.len() as i64, 0);
        for (i, j, own, _) in plan {
            self.drop_one(i, j, "trim");
            names += own.len() as i64;
            self.trimmed.only.extend(own);
        }
        self.bump("trim_parts", parts);
        self.bump("trim_names", names);
        self.bump("trim_tokens", before - after);
        Some(mi)
    }

    /// Of part `j` of output `i`, the lines that stay when the part leaves holding the names `own` (only it
    /// holds them): the first line that holds each of them, between two lines that say the rest left. With
    /// the tokens those lines and the two marks take. None when some name has no line, or when those lines
    /// are half of the part or more (a listing, all names, stays whole).
    pub(super) fn stay_lines(&mut self, i: usize, j: usize, own: &[Sym]) -> Option<(Vec<usize>, i64)> {
        let c = &self.fz.chunks[i][j];
        let text = self.tr.unit_text(i);
        let (mut found, mut size): (FxHashSet<Sym>, i64) = (FxHashSet::default(), 2 * MARK_TOKENS);
        let mut stay: Vec<usize> = Vec::new();
        for (l, line) in text.split('\n').enumerate().take(c.end.max(0) as usize).skip(c.start.max(0) as usize) {
            let holds = words(line, &mut self.int);
            let new = found.len();
            found.extend(own.iter().filter(|w| holds.binary_search(w).is_ok()));
            if found.len() > new {
                stay.push(l);
                size += est_str(line) + 1;
            }
        }
        (found.len() == own.len() && 2 * size <= c.tokens).then_some((stay, size))
    }

    /// Whether output `i` is what a process printed: a command's, and not one that only prints files (that
    /// is the material itself, as a read returns it).
    fn process_output(&self, i: usize) -> bool {
        if !self.fz.info.get(&i).is_some_and(|x| x.kind == Kind::Exec) {
            return false;
        }
        let (u, id) = (&self.tr.units[i], key_of(self.tr.block(i).get("tool_use_id")));
        let calls = (u.msg.saturating_sub(3)..u.msg).rev().flat_map(|m| self.fz.by_msg(m).iter().copied());
        let call =
            calls.filter(|&c| self.tr.units[c].kind == UKind::Call).find(|&c| key_of(self.tr.block(c).get("id")) == id);
        call.is_some_and(|c| !prints_files(&command_of(self.tr.block(c))))
    }

    /// Counts the names on the lines a trim kept that the agent then says while those lines are all that
    /// holds them (`trim_names_said`): what the lines were kept for, and what a trim that took the whole
    /// part would have cost. A name some output prints again, or the person says, is held there from
    /// then on.
    fn names_said(&mut self) {
        let mut said = 0;
        for k in self.trimmed.seen..self.tr.units.len() {
            if self.trimmed.only.is_empty() {
                break;
            }
            let u = &self.tr.units[k];
            let agent = matches!(u.kind, UKind::Text | UKind::Call | UKind::Thinking);
            for w in u.words.iter() {
                if self.trimmed.only.remove(w) && agent {
                    said += 1;
                }
            }
        }
        self.trimmed.seen = self.tr.units.len();
        if said > 0 {
            self.bump("trim_names_said", said);
        }
    }
}
