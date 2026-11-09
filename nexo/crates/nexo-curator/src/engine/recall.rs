//! What the view left out and comes back: the steps a request needs, the outputs the agent asks for again.

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;

use crate::hindsight::{Sym, UKind};
use crate::json::est_str;
use crate::msg::key_of;
use crate::textops::{self};
use crate::tools::{prints_files, read_range, Kind};

use super::files::{basename, command_of, file_key, files_in, same_file};
use super::steps::Step;
use super::{Engine, RECALL_TOOL};

/// The latest calls for the same file or command `asked_back` looks through for the copy a call asks for
/// again, and the latest edits it looks through for one that changed the file since: a copy further back
/// is let be, and one with more edits after it is taken as changed. Both keep a request's work bounded
/// however long the session.
const ASKED_CALLS: usize = 32;

const ASKED_EDITS: usize = 256;

/// A call that read a file, as `Engine::reads_made` keeps it: its unit and the lines it asked for (from 1,
/// `u64::MAX` to the end), the whole file for a command.
pub(super) type ReadMade = (usize, (u64, u64));

/// ``s`` cut to ``n`` characters, with "…" when it was longer.
/// How many times `text` says `word` as a whole word.
pub(super) fn says(text: &str, word: &str) -> usize {
    let alnum = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    text.match_indices(word)
        .filter(|&(k, m)| !alnum(text[..k].chars().next_back()) && !alnum(text[k + m.len()..].chars().next()))
        .count()
}

/// The message (from 0) a `recall` call names: `m00042`, `m42` or `42` is the 42nd.
fn message_named(name: &str) -> Option<usize> {
    let n = name.trim().trim_start_matches(['m', 'M', '#']);
    n.parse::<usize>().ok().filter(|&k| k > 0).map(|k| k - 1)
}

impl Engine {
    /// The steps of the pool that cover the words of `need` not in `base`: for each, the richest step
    /// that holds it (a file read whole, more than an edit that names it) and the latest (what changed).
    /// A word no holder went for or read as its own is only a topic many outputs touch (a header every
    /// file repeats, a word of the prose): its richest holder is then the one that says it most often,
    /// as a search ranks by term frequency, not the largest output that names it once.
    /// A step taken covers its other words too, so a request about one listing takes that listing once.
    pub(super) fn pick(
        &self,
        need: &FxHashSet<Sym>,
        base: &FxHashSet<Sym>,
        pool: impl Fn(&Step) -> bool,
    ) -> Vec<usize> {
        let mut covered: FxHashSet<Sym> = FxHashSet::default();
        let mut want: Vec<Sym> = need.iter().filter(|w| !base.contains(w)).copied().collect();
        // the steps of the pool that hold each word, oldest first: one pass over the steps, not one for each word
        let mut held: FxHashMap<Sym, Vec<usize>> = want.iter().map(|&w| (w, Vec::new())).collect();
        for (i, s) in self.steps.iter().enumerate().filter(|(_, s)| pool(s)) {
            for (w, at) in held.iter_mut() {
                if s.about.contains(w) {
                    at.push(i);
                }
            }
        }
        // with `aim`, the words fewest steps hold come first: the one step that has a rare word is the one
        // the request means, and what it covers needs nothing more
        want.sort_unstable_by_key(|w| (if self.p.aim != 0 { held[w].len() } else { 0 }, *w));
        let mut out = Vec::new();
        for w in want {
            if covered.contains(&w) {
                continue;
            }
            let mut holds: Vec<usize> = held.remove(&w).unwrap_or_default();
            if self.p.aim != 0 && holds.iter().any(|&i| self.steps[i].aimed.contains(&w)) {
                holds.retain(|&i| self.steps[i].aimed.contains(&w));
            }
            let rich = |i: usize| (self.steps[i].read.len(), self.steps[i].about.len(), self.steps[i].msgs[0]);
            let topic = !holds.iter().any(|&i| self.steps[i].aimed.contains(&w) || self.steps[i].read.contains(&w));
            let richest = if topic {
                let word = self.int.get(w).to_lowercase();
                holds.iter().copied().max_by_key(|&i| (says(&self.step_text(i).to_lowercase(), &word), rich(i)))
            } else {
                holds.iter().copied().max_by_key(|&i| rich(i))
            };
            let latest = holds.iter().copied().max_by_key(|&i| self.steps[i].msgs[0]);
            for i in richest.into_iter().chain(latest) {
                if !out.contains(&i) {
                    covered.extend(self.steps[i].about.iter().copied());
                    out.push(i);
                }
            }
        }
        out
    }

    /// The steps out of the view that the latest request needs come back (`pick`): for each word of `need`
    /// that no output in the view holds, the richest and the latest step out of the view that hold it.
    /// They come back at the end of the view, as text in the latest message, so that what the cache holds
    /// stays as it is; they leave again the way they left the first time. Whether any came back.
    pub(super) fn recall(&mut self, need: &FxHashSet<Sym>) -> bool {
        let last = self.tr.msgs.len() - 1;
        let vis = self.visible();
        // the words of `need` that something in the view holds: those of an output it sends, those of a
        // step that is not out. Only a word the request asks is looked up, not every word of the history.
        let mut covered = FxHashSet::default();
        for &mi in vis.iter() {
            for &i in self.fz.by_msg(mi) {
                let u = &self.tr.units[i];
                if u.kind == UKind::Result {
                    covered.extend(u.words.iter().copied().filter(|w| need.contains(w)));
                }
            }
        }
        // only a closed step comes back. An open one the view does not send (it left with the history, or its
        // topic is left out) counts as seen: the ledger keeps the facts of the first, the topic of the second
        // comes back whole when the talk returns to it. Bringing them back here was measured on the 20 recorded
        // sessions: 1.9 % more input cost (up to 29 % in one), and 5 fewer forgotten words of 112 (7 fewer in
        // one, one more in two others).
        for s in self.steps.iter().filter(|s| !s.out) {
            covered.extend(need.iter().filter(|w| s.about.contains(w)).copied());
        }
        let back = self.pick(need, &covered, |s| s.out);
        for &i in &back {
            let text = self.step_text(i);
            let s = &mut self.steps[i];
            (s.out, s.at, s.tokens, s.idle) = (false, Some(last), est_str(&text), None);
            s.backs = s.backs.saturating_add(1);
        }
        if !back.is_empty() {
            self.bump("recalled_steps", back.len() as i64);
        }
        !back.is_empty()
    }

    /// The calls that just arrived (units `new`) that ask again for what the view last sent had left out:
    /// a `recall` of a message the view did not send, or sent with an output taken out whole; a read of a
    /// file over lines an earlier read gave, or a command that prints files run again, when the latest such
    /// output that came back without an error is out of the view and no edit of the file came after it.
    /// Each is the agent bringing an output back by itself (`wanted_back`). Only the reply to the view last
    /// sent is weighed: a message that comes with it further on (a history taken in at once) was not
    /// written from that view.
    pub(super) fn asked_back(&mut self, new: std::ops::Range<usize>) {
        for i in new {
            let u = &self.tr.units[i];
            if u.kind != UKind::Call {
                continue;
            }
            let judged = u.msg > 0 && self.sent_vis.last() == Some(&(u.msg - 1));
            let (msg, tool, b) = (u.msg, u.tool.clone(), self.tr.block(i));
            if tool == RECALL_TOOL {
                let name = b.get("input").and_then(|x| x.get("message")).and_then(Value::as_str).unwrap_or("");
                let named = message_named(name);
                if let Some(mi) = named.filter(|&mi| judged && mi < msg && self.left_out(mi, None)) {
                    self.wanted_back();
                    // into a topic the split left out: two of them are a storm, not a lookup
                    if self.split && self.goal.thread_at(mi) != self.goal.active() {
                        self.hidden_recalls += 1;
                    }
                }
                continue;
            }
            let Some(info) = self.fz.info.get(&i) else { continue };
            let (key, lines, path) = match (info.kind, &info.path) {
                (Kind::Read, Some(p)) => (file_key(p), read_range(b.get("input")), Some(p.clone())),
                (Kind::Exec, _) => {
                    let c = textops::squash_ws(&command_of(b));
                    if c.is_empty() || !prints_files(&c) {
                        continue;
                    }
                    (format!("$ {c}"), (1, u64::MAX), None)
                }
                _ => continue,
            };
            if judged {
                // the latest earlier call for some of those lines whose output came without an error
                let copy = self
                    .reads_made
                    .get(&key)
                    .into_iter()
                    .flatten()
                    .rev()
                    .take(ASKED_CALLS)
                    .filter(|&&(_, (a, b))| a <= lines.1 && lines.0 <= b)
                    .find_map(|&(c, _)| self.result_of(c).filter(|&o| !self.fz.info.get(&o).is_some_and(|x| x.error)));
                if let Some(o) = copy {
                    let mi = self.tr.units[o].msg;
                    if self.left_out(mi, Some(o)) && !self.changed_since(mi, &key, path.as_deref()) {
                        self.wanted_back();
                    }
                }
            }
            self.reads_made.entry(key).or_default().push((i, lines));
        }
    }

    /// The output of call unit `c`: the tool result that answers it in the messages after it, if it came.
    fn result_of(&self, c: usize) -> Option<usize> {
        let id = key_of(self.tr.block(c).get("id"));
        let mi = self.tr.units[c].msg;
        (mi + 1..=mi + 3)
            .flat_map(|m| self.fz.by_msg(m).iter().copied())
            .find(|&r| self.tr.units[r].kind == UKind::Result && key_of(self.tr.block(r).get("tool_use_id")) == id)
    }

    /// Whether the view last sent left message `mi` out: it did not send it (a step brought back as text
    /// is sent, in the message it came back at), or an output of it went whole: output `i`, or any of them
    /// when none is given.
    fn left_out(&self, mi: usize, i: Option<usize>) -> bool {
        if self.step_at.get(&mi).is_some_and(|&k| !self.steps[k].out && self.steps[k].at.is_some()) {
            return false;
        }
        if self.sent_vis.binary_search(&mi).is_err() {
            return true;
        }
        let gone = |i: usize| self.dropped.get(&i).is_some_and(|d| d.len() == self.fz.chunks[i].len());
        match i {
            Some(i) => gone(i),
            None => self.fz.by_msg(mi).iter().any(|&i| self.tr.units[i].kind == UKind::Result && gone(i)),
        }
    }

    /// Whether an edit or a write that did not fail came after message `since` to a file `call` may have
    /// printed: `path` when the call read one file, else a file the command names (its name is in the
    /// command), or any file when it names none (`ls`, `grep x src/`). As `Params.stale` keeps the edits.
    fn changed_since(&self, since: usize, call: &str, path: Option<&str>) -> bool {
        let any = path.is_none() && files_in(call).is_empty();
        let names = |p: &str| match path {
            Some(f) => same_file(p, f),
            None => any || call.contains(basename(p)),
        };
        let mut after = self.edits.iter().rev().take_while(|e| e.1 > since);
        after.by_ref().take(ASKED_EDITS).any(|(p, _, id)| self.edit_ok.get(id) == Some(&true) && names(p))
            || after.next().is_some()
    }

    /// The agent brought a message back by itself while the view had left it out (`asked_back`).
    fn wanted_back(&mut self) {
        self.bump("agent_recalled_steps", 1);
    }
}
