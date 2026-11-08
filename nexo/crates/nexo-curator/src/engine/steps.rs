//! The steps of answered requests: their words, how a request closes and the lines that say what left.

use indexmap::IndexMap;
use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;

use crate::goal::action;
use crate::hindsight::{Interner, Sym, Tracer, UKind};
use crate::mechanical::MARK;
use crate::msg::blocks;

use super::{Engine, NOTE_TOOL};

/// Output messages a closed request names by their `m#####` number before the rest are counted.
const RECALL_NAMED: usize = 6;

/// The distinct calls a closed request names on its line.
const CLOSED_SHOWN: usize = 8;

/// A step of an answered request (`Params.close`): a call and its output.
#[derive(Clone, Debug)]
pub(super) struct Step {
    /// The request message, and the messages of the step.
    pub(super) a: usize,
    pub(super) msgs: Vec<usize>,
    /// The distinctive words of its output.
    pub(super) read: FxHashSet<Sym>,
    /// Those, the ones of its calls, and its topic words (`topics`): what a request may name it by.
    pub(super) about: FxHashSet<Sym>,
    /// The words its calls named (paths, names, their parts, the topic words of what they did): what it
    /// went for, where `about` also holds what it only came across (a file a listing printed).
    pub(super) aimed: FxHashSet<Sym>,
    /// What its calls did, for the line that says it left.
    pub(super) did: Vec<String>,
    pub(super) out: bool,
    /// The message it was brought back at, as text, and the tokens of that text.
    pub(super) at: Option<usize>,
    pub(super) tokens: i64,
    /// The model call since which no request has used it (`None`: in use, or out).
    pub(super) idle: Option<usize>,
}

impl Step {
    /// Where the step is in the view: the message it came back at, or its own first message.
    pub(super) fn place(&self) -> usize {
        self.at.unwrap_or(self.msgs[0])
    }
}

/// The topic words of a text: its words of three letters or more, in lower case, digits apart, and its
/// single capital letters, which label things ("function B", "table C", "assembly-A.txt"). Unlike the
/// distinctive words, which show what a reply copied, these show what a text is about ("helm",
/// "invoice", "boost"); how rare each is in the session (`asks`) tells which point anywhere.
pub(super) fn topics(text: &str, int: &mut Interner) -> FxHashSet<Sym> {
    let mut out = FxHashSet::default();
    for w in text.split(|c: char| !c.is_alphanumeric()) {
        if w.len() == 1 && w.chars().all(|c| c.is_ascii_uppercase()) {
            out.insert(int.intern(w));
        } else if w.chars().count() >= 3 && !w.chars().all(|c| c.is_ascii_digit()) {
            out.insert(int.intern(&w.to_lowercase()));
        }
    }
    out
}

/// The steps of a request's work: each agent message with the tool results that answer it. A step that
/// only writes notes is left out: the notes are what the agent took from the others, and they stay.
pub(super) fn steps_of(tr: &Tracer, work: Vec<usize>) -> Vec<Vec<usize>> {
    let mut steps: Vec<Vec<usize>> = Vec::new();
    for mi in work {
        let agent = tr.msgs[mi].get("role").and_then(Value::as_str) == Some("assistant");
        match steps.last_mut() {
            Some(s) if !agent => s.push(mi),
            _ => steps.push(vec![mi]),
        }
    }
    steps.retain(|s| {
        let calls: Vec<String> = blocks(tr.msgs[s[0]].get("content"))
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            .map(|b| b.get("name").and_then(Value::as_str).unwrap_or("").to_string())
            .collect();
        calls.is_empty() || calls.iter().any(|c| c != NOTE_TOOL)
    });
    steps
}

impl Engine {
    /// The calls and outputs of request `j` (the messages between the request and its reply), if the agent
    /// answered it: its last message holds text and no call. `None` for a request still open or with no
    /// calls.
    pub(super) fn work_of(&self, j: usize) -> Option<(usize, Vec<usize>)> {
        let (a, b) = self.goal.span(j, self.tr.msgs.len());
        let has = |mi: usize, kind: &str| {
            blocks(self.tr.msgs[mi].get("content")).iter().any(|x| x.get("type").and_then(Value::as_str) == Some(kind))
        };
        let last = b - 1;
        let answered = last > a
            && self.tr.msgs[last].get("role").and_then(Value::as_str) == Some("assistant")
            && !has(last, "tool_use")
            && self.fz.by_msg(last).iter().any(|&i| self.tr.units[i].kind == UKind::Text);
        let work: Vec<usize> = (a + 1..last).filter(|&mi| has(mi, "tool_use") || has(mi, "tool_result")).collect();
        (answered && !work.is_empty()).then_some((a, work))
    }

    /// The distinctive words of the units of `msgs` whose kind is one of `kinds`.
    pub(super) fn words_in(&self, msgs: impl Iterator<Item = usize>, kinds: &[UKind]) -> FxHashSet<Sym> {
        let mut out = FxHashSet::default();
        for mi in msgs {
            for &i in self.fz.by_msg(mi) {
                let u = &self.tr.units[i];
                if kinds.contains(&u.kind) {
                    out.extend(u.words.iter().copied());
                }
            }
        }
        out
    }

    /// The words of request `j` that point at some steps and not at all: its distinctive words and its
    /// topic words that at most half of the steps hold and that at most half of the requests say. A word
    /// every step holds (the project's folder) or every request repeats (the frame of the session: a
    /// format, a name the person always gives) says nothing of which steps to bring, unless exactly one
    /// step holds it: then it points at that step however often the person says it.
    pub(super) fn asks(&mut self, j: usize) -> FxHashSet<Sym> {
        while self.requests.len() <= j {
            let a = self.goal.turns[self.requests.len()].start;
            let mut words = self.words_in(std::iter::once(a), &[UKind::User]);
            let text = self.unit_text_at(a, UKind::User);
            words.extend(topics(&text, &mut self.int));
            for &w in &words {
                *self.requests_df.entry(w).or_default() += 1;
            }
            self.requests.push(words);
        }
        let (n, m) = (self.steps.len(), self.requests.len());
        let df = |map: &FxHashMap<Sym, usize>, w: &Sym| map.get(w).copied().unwrap_or(0);
        self.requests[j]
            .iter()
            .filter(|w| {
                df(&self.step_df, w) == 1 || (2 * df(&self.step_df, w) <= n && 2 * df(&self.requests_df, w) <= m + 1)
            })
            .copied()
            .collect()
    }

    /// A step of request message `a`, made of messages `msgs`: what it read, what it is about, what it did.
    pub(super) fn new_step(&mut self, a: usize, msgs: Vec<usize>) -> Step {
        let mut called = self.words_in(msgs.iter().copied(), &[UKind::Call]);
        // a request names a file by its name more often than by its path
        let paths: Vec<String> = called.iter().map(|&w| self.int.get(w).to_string()).collect();
        for p in paths {
            match p.rsplit_once('/') {
                Some((_, name)) if name.chars().count() >= 4 => {
                    called.insert(self.int.intern(name));
                }
                _ => {}
            }
        }
        let read = self.words_in(msgs.iter().copied(), &[UKind::Result]);
        let did = self.fz.by_msg(msgs[0]).iter().filter(|&&i| self.tr.units[i].kind == UKind::Call);
        let did: Vec<String> = did.map(|&i| action(&self.tr, i)).collect();
        let mut about: FxHashSet<Sym> = called.union(&read).copied().collect();
        for mi in &msgs {
            let text = self.unit_text_at(*mi, UKind::Result);
            about.extend(topics(&text, &mut self.int));
        }
        let mut aimed = called;
        for d in &did {
            aimed.extend(topics(d, &mut self.int));
        }
        about.extend(aimed.iter().copied());
        for &w in &about {
            *self.step_df.entry(w).or_default() += 1;
        }
        Step { a, msgs, read, about, aimed, did, out: false, at: None, tokens: 0, idle: None }
    }

    /// Step `i` as text: what each call did, and each output whole.
    pub(super) fn step_text(&self, i: usize) -> String {
        let mut out = vec![format!("{MARK} Brought back from earlier in this conversation:")];
        for &mi in &self.steps[i].msgs {
            for &u in self.fz.by_msg(mi) {
                match self.tr.units[u].kind {
                    UKind::Call => out.push(format!("$ {}", action(&self.tr, u))),
                    UKind::Result => out.push(self.tr.unit_text(u).into_owned()),
                    _ => {}
                }
            }
        }
        out.join("\n")
    }

    /// The message the line that says step `i` left goes on (`gone`: the messages out of the view): the
    /// last one before the step that the view still sends (one before
    /// `start` is not sent either), so that all the cache holds up to it stays as it is. The steps of a request that left side by side share it. A message of the agent's there (a
    /// step that said something and called nothing) is no place for it: the request takes the line.
    /// When the request itself is no longer sent (it is before `start`; the first message always is), the
    /// line goes on the first message from `start` on that the view sends and that is not the agent's,
    /// before the step if one is, else after it (and then it does not say the reply follows): a line on a
    /// message not sent is lost.
    fn line_at(&self, i: usize, gone: &FxHashSet<usize>) -> usize {
        let s = &self.steps[i];
        let agent =
            |mi: usize| self.tr.msgs.get(mi).and_then(|m| m.get("role")).and_then(Value::as_str) == Some("assistant");
        let sent = |mi: usize| !gone.contains(&mi);
        if s.a == 0 || s.a >= self.start {
            let mut mi = s.msgs[0].saturating_sub(1);
            while mi > s.a && (!sent(mi) || mi < self.start) {
                mi -= 1;
            }
            return if mi <= s.a || agent(mi) { s.a } else { mi };
        }
        (self.start..self.tr.msgs.len()).find(|&mi| sent(mi) && !agent(mi)).unwrap_or(0)
    }

    /// The steps out of the view leave it, and each run of them side by side says which calls left, on
    /// the message just before it (`line_at`), so that the agent can run them again. The reply that closed
    /// the request stays: it is what the agent took from that work, the way a note is. The steps brought
    /// back go as text in the message they came back at.
    /// Returns the oldest message it changed, if any.
    pub(super) fn shut_lines(&mut self) -> Option<usize> {
        let mut out = FxHashSet::default();
        // the last step of each request: the line of the run that ends with it says the reply follows
        let mut last: FxHashMap<usize, usize> = FxHashMap::default();
        for (i, s) in self.steps.iter().enumerate() {
            if s.out || s.at.is_some() {
                out.extend(s.msgs.iter().copied());
            }
            last.insert(s.a, i);
        }
        // each run, by the message its line goes on and its request: the calls that left, the messages
        // that carry their outputs (what a `recall` is for), and whether it reaches the request's last step
        type Run<'s> = (IndexMap<&'s str, usize>, Vec<usize>, bool);
        let mut runs: IndexMap<(usize, usize), Run> = IndexMap::new();
        let mut brought: Vec<(usize, usize)> = Vec::new();
        for (i, s) in self.steps.iter().enumerate() {
            if let (false, Some(at)) = (s.out, s.at) {
                brought.push((at, i));
            }
            // a step brought back still left its place: the line counts it, so it reads the same as when it
            // was written and what the cache holds before the latest messages stays as it is
            if !s.out && s.at.is_none() {
                continue;
            }
            // a step that left with the messages before `start` has no place in the view to name it from
            if s.msgs.iter().all(|&m| m < self.start) {
                continue;
            }
            let at = self.line_at(i, &out);
            // the reply follows only a line before the step, and only when the agent replied
            let next = s.msgs.last().map_or(0, |m| m + 1);
            let replied = self.tr.msgs.get(next).is_some_and(|m| {
                m.get("role").and_then(Value::as_str) == Some("assistant")
                    && !blocks(m.get("content"))
                        .iter()
                        .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            });
            let (d, outs, end) = runs.entry((at, s.a)).or_default();
            for &m in &s.msgs {
                if self.tr.msgs[m].get("role").and_then(Value::as_str) != Some("assistant") {
                    outs.push(m);
                }
            }
            for what in &s.did {
                *d.entry(what.as_str()).or_insert(0) += 1;
            }
            *end |= last.get(&s.a) == Some(&i) && at < s.msgs[0] && replied;
        }
        // what came back to a message was there before the request it starts was closed: it goes first, so
        // the line that closes the request comes after it and the message only grows
        let mut shut: FxHashMap<usize, Vec<String>> = FxHashMap::default();
        brought.sort();
        for (at, i) in brought {
            shut.entry(at).or_default().push(self.step_text(i));
        }
        for ((at, _), (d, mut outs, end)) in runs {
            let calls: usize = d.values().sum();
            let mut said: Vec<String> = d
                .iter()
                .take(CLOSED_SHOWN)
                .map(|(what, &n)| if n > 1 { format!("{what} (×{n})") } else { what.to_string() })
                .collect();
            if d.len() > CLOSED_SHOWN {
                said.push(format!("… {} more", d.len() - CLOSED_SHOWN));
            }
            outs.sort_unstable();
            outs.dedup();
            let back = if self.recall_tool && !outs.is_empty() {
                let v = &outs;
                let name = |m: &usize| format!("m{:05}", m + 1);
                let mut names: Vec<String> = v.iter().take(RECALL_NAMED).map(name).collect();
                if v.len() > RECALL_NAMED {
                    names.push(format!("… {} more, to {}", v.len() - RECALL_NAMED, name(&v[v.len() - 1])));
                }
                format!(
                    " {} {}; recall brings {} back as it was.",
                    if v.len() == 1 { "The output is" } else { "The outputs are" },
                    names.join(", "),
                    if v.len() == 1 { "it" } else { "any of them" }
                )
            } else {
                String::new()
            };
            shut.entry(at).or_default().push(format!(
                "{MARK} {calls} tool call{} made for this request and {} output are left out of this context: {}.{}{back}",
                if calls == 1 { "" } else { "s" },
                if calls == 1 { "its" } else { "their" },
                said.join(" | "),
                if end { " The reply that closed the request follows." } else { "" }
            ));
        }
        let moved: Vec<usize> = out.symmetric_difference(&self.shut_out).copied().collect();
        let (going, coming): (Vec<usize>, Vec<usize>) = moved.iter().partition(|m| out.contains(m));
        self.tag_msgs(going, true, "close");
        self.tag_msgs(coming, false, "close");
        let relined: Vec<usize> =
            shut.keys().chain(self.shut.keys()).filter(|a| shut.get(a) != self.shut.get(a)).copied().collect();
        // the view changes only where it sends a message before or after: a step that leaves or comes back
        // from where the view does not send it (before `start`, in a topic left out) changes nothing there
        let before = self.visible();
        self.shut_out = out;
        self.vis = None;
        let after = self.visible();
        let sent: FxHashSet<usize> = before.iter().chain(after.iter()).copied().collect();
        let changed = moved.iter().chain(&relined).filter(|m| sent.contains(m)).min().copied();
        for mi in moved.into_iter().chain(relined) {
            self.built.remove(&mi);
        }
        let closed = self.steps.iter().filter(|s| s.out).count() as i64;
        self.stats.insert("closed_steps", closed);
        self.shut = shut;
        self.vis = None;
        changed
    }
}
