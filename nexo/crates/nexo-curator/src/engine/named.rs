//! Steps that stay in the view because the latest request names a file they read.

use std::sync::Arc;

use rustc_hash::FxHashSet;

use crate::goal::action;
use crate::hindsight::{Sym, UKind};
use crate::json::est_str;
use crate::tools::Kind;

use super::files::{files_in, name_parts, same_file};
use super::{Engine, NOTE_TOOL};

impl Engine {
    /// The files each step read, each by the parts of its name the other files read do not share
    /// (`assembly-B.txt` by `B` next to `assembly-A.txt`; `auth.rs` by `auth` next to `db.rs`; a file read
    /// alone, by all of them). A part two files have (`assembly`, `test`) names neither; a file with no
    /// part of its own keeps them all.
    fn step_files(&mut self) -> Arc<Vec<Vec<Vec<String>>>> {
        for s in &self.steps[self.sf.seen()..] {
            self.sf.push(s.did.iter().flat_map(|d| files_in(d)).collect());
        }
        self.sf.all()
    }

    /// The outputs of the calls that read a file the latest request names, each file by the parts of its
    /// name the other files read do not share (as in `step_files`): «and now Function C» names
    /// `assembly-C.txt` next to `assembly-A.txt`. It looks at every call, the request still open too.
    /// Newest first, as `named_take` bounds them.
    pub(super) fn named_results(&self) -> FxHashSet<usize> {
        let mut out = FxHashSet::default();
        let Some(start) = self.goal.turns.last().map(|t| t.start) else { return out };
        let asked: FxHashSet<String> = name_parts(&self.unit_text_at(start, UKind::User)).into_iter().collect();
        let cands: Vec<(Vec<usize>, bool)> = self
            .call_files()
            .into_iter()
            .rev()
            .filter(|(_, files)| files.iter().any(|f| f.iter().all(|p| asked.contains(p))))
            .map(|(mi, _)| {
                let res = self.fz.by_msg(mi + 1).iter().copied().filter(|&u| self.tr.units[u].kind == UKind::Result);
                (res.collect(), true)
            })
            .collect();
        for ((units, _), take) in cands.iter().zip(self.named_take(&cands)) {
            if take {
                out.extend(units.iter().copied());
            }
        }
        out
    }

    /// The files result `i` holds as they were: a read tool's file, or those a shell command that only
    /// printed files printed (`shell_reads`).
    fn read_paths(&self, i: usize) -> Vec<&str> {
        match self.fz.info.get(&i) {
            Some(x) if x.kind == Kind::Read => x.path.as_deref().filter(|p| !p.is_empty()).into_iter().collect(),
            _ => self.shell_reads.get(&i).map(|ps| ps.iter().map(String::as_str).collect()).unwrap_or_default(),
        }
    }

    /// Of the reads a request names (`cands`: the result units of each, newest first), the ones `named`
    /// keeps or brings back, newest first up to `Params.named` tokens: none a later edit of its file made
    /// out of date once a newer read of that file is taken (it shows what the file no longer holds; with
    /// no newer read, it is still the best picture of the file, the edits on top), and none whose words a
    /// newer one taken holds already (a second read of the same lines). The newest one still in the view
    /// stays whatever its size (`in_view`): keeping it adds nothing to send. Whether each is taken.
    fn named_take(&self, cands: &[(Vec<usize>, bool)]) -> Vec<bool> {
        let budget = self.p.named;
        let (mut spent, mut covered, mut fresh) = (0i64, FxHashSet::default(), Vec::<&str>::new());
        let mut out = Vec::with_capacity(cands.len());
        for (units, in_view) in cands {
            let paths: Vec<(usize, Vec<&str>)> =
                units.iter().map(|&u| (u, self.read_paths(u))).filter(|(_, ps)| !ps.is_empty()).collect();
            let newer = |p: &&str| fresh.iter().any(|f| same_file(f, p));
            let stale = !paths.is_empty()
                && paths.iter().all(|(u, ps)| ps.iter().all(newer) && self.read_superseded(*u, true).is_some());
            let words: FxHashSet<Sym> = units.iter().flat_map(|&u| self.tr.units[u].words.iter().copied()).collect();
            let copy = !words.is_empty() && words.iter().all(|w| covered.contains(w));
            let tokens: i64 = units.iter().map(|&u| self.tr.units[u].tokens).sum();
            let fits = spent + tokens <= budget || (*in_view && spent == 0);
            let take = !stale && !copy && fits;
            if take {
                spent += tokens;
                covered.extend(words);
                fresh.extend(paths.into_iter().flat_map(|(_, ps)| ps));
            }
            out.push(take);
        }
        out
    }

    /// The calls that read files, each with its message and its files, each file by the parts of its name
    /// the files the other calls read do not share (as in `step_files`); a file with no part of its own
    /// keeps them all.
    fn call_files(&self) -> Vec<(usize, Vec<Vec<String>>)> {
        let shared = |p: &String| self.part_files.get(p).copied().unwrap_or(0) > 1;
        let own = |f: &Vec<String>| {
            let own: Vec<String> = f.iter().filter(|p| !shared(p)).cloned().collect();
            if own.is_empty() {
                f.clone()
            } else {
                own
            }
        };
        self.calls_raw.iter().map(|(mi, fs)| (*mi, fs.iter().map(own).collect())).collect()
    }

    /// Brings `calls_raw` and the part counts up to the units that came since the last request.
    pub(super) fn index_calls(&mut self) {
        for u in self.calls_seen..self.tr.units.len() {
            if self.tr.units[u].kind != UKind::Call || self.tr.units[u].tool == NOTE_TOOL {
                continue;
            }
            let files = files_in(&action(&self.tr, u));
            if files.is_empty() {
                continue;
            }
            for f in &files {
                if self.files_known.insert(f.clone()) {
                    let mut parts = f.clone();
                    parts.sort();
                    parts.dedup();
                    for p in parts {
                        *self.part_files.entry(p).or_default() += 1;
                    }
                }
            }
            self.calls_raw.push((self.tr.units[u].msg, files));
        }
        self.calls_seen = self.tr.units.len();
    }

    /// `Params.named`: a step that read a file the latest request names (`step_files`) is not idle. «Fix
    /// the login in auth» keeps `auth.rs` read; the next request, on the README, lets it go. When a request
    /// has just begun (`turned`), the steps out of the view that read a file it names come back, the way
    /// `recall` brings them: «and now the logout in auth» finds `auth.rs` read again. Whether any came back.
    pub(super) fn named_keep(&mut self, turned: bool) -> bool {
        let Some(start) = self.goal.turns.last().map(|t| t.start) else { return false };
        let asked: FxHashSet<String> = name_parts(&self.unit_text_at(start, UKind::User)).into_iter().collect();
        let files = self.step_files();
        let last = self.tr.msgs.len() - 1;
        // `mission`: the files the thread's first message names that no later request of it named yet
        let thread = self.goal.active();
        let parts: Vec<FxHashSet<String>> = if self.p.mission != 0 {
            let turns: Vec<usize> = self.goal.turns.iter().filter(|t| t.thread == thread).map(|t| t.start).collect();
            turns.iter().map(|&s| name_parts(&self.unit_text_at(s, UKind::User)).into_iter().collect()).collect()
        } else {
            Vec::new()
        };
        let listed = |f: &Vec<String>| parts.len() > 1 && f.iter().all(|p| parts[0].contains(p));
        let taken = |f: &Vec<String>| parts[1..].iter().any(|r| f.iter().all(|p| r.contains(p)));
        // only a list being worked through: a later request already took up one of the files it names
        let through = files.iter().flatten().any(|f| listed(f) && taken(f));
        let pending = |f: &Vec<String>| through && listed(f) && !taken(f);
        let mut back = 0;
        let mut named: Vec<(usize, Vec<usize>, bool)> = Vec::new();
        for (i, own) in files.iter().enumerate() {
            if !own.iter().any(|own| own.iter().all(|p| asked.contains(p))) {
                if !self.steps[i].out && own.iter().any(pending) {
                    self.steps[i].idle = None;
                }
                continue;
            }
            // an output that left unused (`live_drop`) is gone from the view as much as a step that left
            let res: Vec<usize> = self.steps[i]
                .msgs
                .iter()
                .flat_map(|&mi| self.fz.by_msg(mi).iter().copied())
                .filter(|&u| self.tr.units[u].kind == UKind::Result)
                .collect();
            let gone = self.steps[i].at.is_none() && !res.is_empty() && res.iter().all(|u| self.led.contains(u));
            named.push((i, res, !self.steps[i].out && !gone));
        }
        // reads by parts (`sed -n 40,400p`, `400,800p`) are not copies of each other: each may stay or come
        // back, newest first, within the budget (`named_take`)
        named.reverse();
        let cands: Vec<(Vec<usize>, bool)> = named.iter().map(|(_, res, in_view)| (res.clone(), *in_view)).collect();
        for ((i, _, in_view), take) in named.into_iter().zip(self.named_take(&cands)) {
            if !take {
                continue;
            }
            if in_view {
                self.steps[i].idle = None;
            } else if turned {
                let text = self.step_text(i);
                let s = &mut self.steps[i];
                (s.out, s.at, s.tokens, s.idle) = (false, Some(last), est_str(&text), None);
                back += 1;
            }
        }
        if back > 0 {
            self.bump("recalled_steps", back);
        }
        back > 0
    }
}
