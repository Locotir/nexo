//! What the curator put in a request beyond the plain conversation, and what it held back: a list of
//! named items with their sizes. Read-only: it reads the state the decisions left and changes none.

use rustc_hash::FxHashSet;
use serde_json::{json, Value};

use crate::hindsight::UKind;
use crate::json::est_str;
use crate::mechanical::MARK;
use crate::textops::{self};

use super::Engine;

/// What kind of thing an item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddedKind {
    /// A tool output kept in the history: one a step brought back, or one the view left out or cut.
    Recalled,
    /// The start of the request: the map of what moved out, a file it pins or one the agent wrote.
    Start,
    /// A standing instruction the user gave.
    Rule,
    /// A topic of the conversation, as a card or left out.
    Thread,
    /// The plan the agent wrote down.
    Goal,
    /// A fact the agent or the ledger kept.
    Memory,
}

impl AddedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AddedKind::Recalled => "recalled",
            AddedKind::Start => "start",
            AddedKind::Rule => "rule",
            AddedKind::Thread => "thread",
            AddedKind::Goal => "goal",
            AddedKind::Memory => "memory",
        }
    }
}

/// Whether an item is in the request, in it but shortened, or held back (its tokens are then what was held back).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddedState {
    In,
    Trimmed,
    Out,
}

impl AddedState {
    pub fn as_str(self) -> &'static str {
        match self {
            AddedState::In => "in",
            AddedState::Trimmed => "trimmed",
            AddedState::Out => "out",
        }
    }
}

/// One item: what it is, a short name, its tokens (estimated) and where it stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Added {
    pub kind: AddedKind,
    pub label: String,
    pub tokens: i64,
    pub state: AddedState,
}

impl Added {
    pub fn to_json(&self) -> Value {
        json!({"kind": self.kind.as_str(), "label": self.label, "tokens": self.tokens.max(0), "state": self.state.as_str()})
    }
}

/// The most items held back that a list names one by one; the rest are one item.
const OUT_NAMED: usize = 24;

/// The longest label, in characters.
const LABEL_CHARS: usize = 48;

/// The label of a text with no words to name it by.
const NO_WORDS: &str = "(no words)";

/// The longest keyword, in characters.
const KEYWORD_CHARS: usize = 24;

/// Words too common to name a block.
const COMMON: &[&str] = &[
    "the", "and", "for", "that", "this", "with", "from", "are", "was", "were", "not", "you", "your", "all", "any",
    "can", "has", "have", "had", "but", "its", "into", "out", "use", "using", "used", "will", "would", "should",
    "could", "then", "than", "them", "they", "when", "what", "which", "who", "how", "why", "also", "just", "only",
    "each", "every", "more", "most", "some", "such", "one", "two", "turn", "finding", "plan", "rule", "always",
    "never", "please", "make", "sure", "need", "needs", "keep", "dont", "don't", "does", "did", "get", "got",
];

/// A word longer than a name needs, by its end (the end of a path is the file).
fn short(w: &str) -> std::borrow::Cow<'_, str> {
    let n = w.chars().count();
    if n <= KEYWORD_CHARS {
        std::borrow::Cow::Borrowed(w)
    } else {
        std::borrow::Cow::Owned(format!("…{}", w.chars().skip(n - KEYWORD_CHARS + 1).collect::<String>()))
    }
}

fn clip(s: &str) -> String {
    let s = textops::squash_ws(s);
    if textops::len(&s) > LABEL_CHARS {
        format!("{}…", textops::head(&s, LABEL_CHARS - 1))
    } else {
        s
    }
}

/// How much a token looks like a name of something (an identifier, a path, a file) and not a word.
fn identifier_weight(w: &str) -> i64 {
    let inner_dot = w.trim_matches('.').contains('.');
    let camel = w.chars().zip(w.chars().skip(1)).any(|(a, b)| a.is_lowercase() && b.is_uppercase());
    let digit = w.chars().any(|c| c.is_ascii_digit()) && w.chars().any(char::is_alphabetic);
    if w.contains('_') || w.contains('/') || w.contains("::") || inner_dot || camel {
        300
    } else if digit || w.contains('-') {
        200
    } else {
        0
    }
}

/// The three most distinctive words of `text`, in the order they come: identifiers and paths first, then
/// the words said least often, the longer first. Deterministic and read from the text alone.
pub fn keywords(text: &str) -> Vec<String> {
    let mut seen: Vec<(String, usize, String)> = Vec::new();
    let mut counts: rustc_hash::FxHashMap<String, i64> = rustc_hash::FxHashMap::default();
    let word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '/' | '.' | '-');
    for (at, raw) in text.split(|c: char| !word(c)).enumerate() {
        let w = raw.trim_matches(|c: char| matches!(c, '.' | '-' | '/')).trim_matches('.');
        if w.chars().count() < 3 || w.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let low = w.to_lowercase();
        if COMMON.contains(&low.as_str()) {
            continue;
        }
        // the first time a word is said, it is a candidate
        let times = counts.entry(low.clone()).or_insert(0);
        *times += 1;
        if *times == 1 {
            seen.push((short(w).into_owned(), at, low));
        }
    }
    let score = |(w, _, low): &(String, usize, String)| {
        let n = counts[low].max(1);
        identifier_weight(w) + 200 / n + w.chars().count().min(14) as i64
    };
    let scores: Vec<i64> = seen.iter().map(score).collect();
    let mut order: Vec<usize> = (0..seen.len()).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(scores[i]), seen[i].1));
    order.truncate(3);
    order.sort_by_key(|&i| seen[i].1);
    order.into_iter().map(|i| seen[i].0.clone()).collect()
}

/// The name of an item: the thread's name if it has one, else the file's path, else the tool and its
/// command, else the three most distinctive words of its text (`keywords`).
pub fn label(thread: Option<&str>, path: Option<&str>, call: Option<&str>, text: &str) -> String {
    for named in [thread, path, call].into_iter().flatten() {
        if !named.trim().is_empty() {
            return clip(named);
        }
    }
    let words = keywords(text);
    if words.is_empty() {
        NO_WORDS.to_string()
    } else {
        clip(&words.join(" "))
    }
}

/// The labels of texts alone (`label(None, None, None, text)`), kept while they stay under a size: the same
/// findings, requests and facts are named at every request, and naming one reads all of its words.
#[derive(Default)]
pub(super) struct LabelMemo {
    map: rustc_hash::FxHashMap<String, String>,
    bytes: usize,
}

impl LabelMemo {
    const MAX_BYTES: usize = 4 << 20;

    fn of(&mut self, text: &str) -> String {
        if let Some(l) = self.map.get(text) {
            return l.clone();
        }
        let l = label(None, None, None, text);
        if self.bytes > Self::MAX_BYTES {
            self.map.clear();
            self.bytes = 0;
        }
        self.bytes += text.len() + l.len();
        self.map.insert(text.to_string(), l.clone());
        l
    }
}

/// What the outputs held back add up to, by name in the order the names first came: (tokens, outputs).
type Outputs = indexmap::IndexMap<std::sync::Arc<str>, (i64, usize)>;

/// The item of the outputs held back under one name.
fn out_item(name: &str, tokens: i64, times: usize) -> Added {
    let label =
        if times > 1 { format!("{} ×{times}", textops::head(name, LABEL_CHARS - 5)) } else { name.to_string() };
    Added { kind: AddedKind::Recalled, label, tokens, state: AddedState::Out }
}

/// The item that stands for the `n` outputs too small to list.
fn smaller(n: usize, tokens: i64) -> Added {
    Added { kind: AddedKind::Recalled, label: format!("{n} smaller items"), tokens, state: AddedState::Out }
}

/// What the outputs of messages `1..upto` hold when none of them is sent, kept between requests: it is
/// read from the messages alone, so it grows as `upto` does and is made again only if `upto` goes back.
#[derive(Default)]
pub(super) struct Below {
    upto: usize,
    names: Outputs,
    /// The names by tokens, the most first, then by where they stand in `names`.
    order: std::collections::BTreeSet<(std::cmp::Reverse<i64>, usize)>,
    total: i64,
}

impl Below {
    /// Takes in what the messages up to `upto` add (`fresh`, in the order they came).
    fn take(&mut self, fresh: Outputs, upto: usize) {
        for (name, (tokens, times)) in fresh {
            let at = self.names.get_index_of(&name).unwrap_or(self.names.len());
            let e = self.names.entry(name).or_insert((0, 0));
            self.order.remove(&(std::cmp::Reverse(e.0), at));
            *e = (e.0 + tokens, e.1 + times);
            self.order.insert((std::cmp::Reverse(e.0), at));
            self.total += tokens;
        }
        self.upto = upto;
    }
}

/// The ask a card names its topic by: what follows `asked "` up to the closing quote.
fn card_topic(card: &str) -> Option<&str> {
    let rest = card.split_once("asked \"")?.1;
    let end = rest.find('"').unwrap_or(rest.len());
    Some(&rest[..end])
}

impl Engine {
    /// The messages whose outputs `added` weighs, as ranges in order: all of them, or, when the other topics
    /// are apart, only the ones no other topic holds (the first message, what came before the first request,
    /// and the active topic's own). A request is of one topic and they follow each other, so the rest is
    /// exactly what the other topics hold, and nothing is read once for each message of the session.
    pub(crate) fn counted(&self) -> Vec<(usize, usize)> {
        let (g, n) = (&self.goal, self.tr.msgs.len());
        let Some(first) = g.turns.first().filter(|_| self.apart()) else { return vec![(0, n)] };
        let mut out = vec![(0, first.start.max(1).min(n))];
        out.extend(g.members[g.active()].iter().map(|&j| g.span(j, n)).map(|(a, b)| (a.max(1).min(b), b)));
        out
    }

    /// `counted` the long way: every message that is sent or that no other topic holds.
    #[cfg(test)]
    pub(crate) fn counted_whole(&mut self) -> Vec<usize> {
        let (g, n) = (&self.goal, self.tr.msgs.len());
        let mut apart: FxHashSet<usize> = FxHashSet::default();
        if self.apart() {
            for k in g.others() {
                for (a, b) in g.members[k].iter().map(|&j| g.span(j, n)) {
                    apart.extend(a.max(1)..b);
                }
            }
        }
        let seen: FxHashSet<usize> = self.visible().iter().copied().collect();
        (0..n).filter(|mi| seen.contains(mi) || !apart.contains(mi)).collect()
    }

    /// What the last view carries beyond the conversation, and what it holds back, one item each. The
    /// anchor's pieces (the map of what moved out, the files it pins and the ones the agent wrote, the
    /// agent's findings and plan, the ledger and the standing instructions), the cards of other topics, the
    /// steps brought back; and, held back, the other topics and the steps out of the view.
    pub fn added(&mut self) -> Vec<Added> {
        self.added_by(true)
    }

    /// `added` with nothing kept from the requests before: every message is read again.
    #[cfg(test)]
    pub(crate) fn added_whole(&mut self) -> Vec<Added> {
        self.added_by(false)
    }

    /// The first message the view may send when the topics are not apart: the outputs before it are all
    /// held back whole, so what they add up to is kept between requests (`Below`). `None` when the view
    /// sends every message or leaves other topics out.
    pub(super) fn kept_from(&self) -> Option<usize> {
        (!self.apart() && !self.goal.turns.is_empty()).then(|| self.start.max(1).min(self.tr.msgs.len()))
    }

    /// What the outputs of message `mi` hold back, added to `to` under the name of each: nothing of one
    /// sent whole, the lines cut of one sent cut, all of one not sent.
    fn held_of(&mut self, mi: usize, sent: bool, to: &mut Outputs) {
        for &i in self.fz.by_msg(mi) {
            let u = &self.tr.units[i];
            if u.kind != UKind::Result {
                continue;
            }
            let tokens = match (sent, self.dropped.get(&i)) {
                (true, None) => 0,
                (true, Some(cut)) => cut.iter().filter_map(|&j| self.fz.chunks[i].get(j)).map(|c| c.tokens).sum(),
                (false, _) => u.tokens,
            };
            if tokens <= 0 {
                continue;
            }
            // a result's name does not change: it is made once
            let name = match self.out_names.get(&i) {
                Some(n) => n.clone(),
                None => {
                    let info = self.fz.info.get(&i);
                    let path = info.and_then(|x| x.path.as_deref()).filter(|p| !p.is_empty());
                    let call =
                        format!("{} {}", u.tool, info.and_then(|x| x.key.as_deref()).unwrap_or("")).trim().to_string();
                    let n: std::sync::Arc<str> = label(None, path, Some(&call), "").into();
                    self.out_names.insert(i, n.clone());
                    n
                }
            };
            let e = to.entry(name).or_insert((0, 0));
            e.0 += tokens;
            e.1 += 1;
        }
    }

    /// The outputs held back when the topics are not apart, the largest first and the small ones as one
    /// item, as `added` lists them. The messages before `from` are not sent, and what their outputs hold
    /// does not change: it is kept (`Below`) and only the messages `from` moved past are read. The first
    /// message and those from `from` on are read at each request, and the two are put together in the
    /// order the names first came.
    fn held_kept(&mut self, from: usize, seen: &FxHashSet<usize>) -> Vec<(Added, Option<usize>)> {
        let mut below = std::mem::take(&mut self.below);
        if from < below.upto {
            below = Below::default();
        }
        let mut fresh = Outputs::new();
        for mi in below.upto.max(1)..from {
            self.held_of(mi, false, &mut fresh);
        }
        below.take(fresh, from);
        let n = self.tr.msgs.len();
        let mut over = Outputs::new();
        if n > 0 {
            self.held_of(0, seen.contains(&0), &mut over);
        }
        let first = over.len();
        for mi in from..n {
            self.held_of(mi, seen.contains(&mi), &mut over);
        }
        // (tokens, where its name first came, name, times): a name of the first message comes before the
        // kept ones, a name only of the messages from `from` on after them
        let mut all: Vec<(i64, (u8, usize), &str, usize)> = Vec::new();
        let (mut count, mut total) = (below.names.len(), below.total);
        for (k, (name, &(tokens, times))) in over.iter().enumerate() {
            total += tokens;
            match below.names.get_full(name) {
                Some((at, _, &(t, c))) => {
                    all.push((tokens + t, if k < first { (0, k) } else { (1, at) }, name, times + c))
                }
                None => {
                    count += 1;
                    all.push((tokens, if k < first { (0, k) } else { (2, k) }, name, times));
                }
            }
        }
        // of the kept ones no request touched, no more than the list shows can be in it
        let untouched = below.order.iter().filter_map(|&(_, at)| {
            let (name, &(tokens, times)) = below.names.get_index(at)?;
            (!over.contains_key(name)).then_some((tokens, (1, at), &**name, times))
        });
        all.extend(untouched.take(OUT_NAMED));
        all.sort_by_key(|&(tokens, at, ..)| (std::cmp::Reverse(tokens), at));
        all.truncate(OUT_NAMED);
        let shown: i64 = all.iter().map(|a| a.0).sum();
        let mut held: Vec<(Added, Option<usize>)> =
            all.into_iter().map(|(tokens, _, name, times)| (out_item(name, tokens, times), None)).collect();
        if count > OUT_NAMED {
            held.push((smaller(count - OUT_NAMED, total - shown), None));
        }
        self.below = below;
        held
    }

    fn added_by(&mut self, kept: bool) -> Vec<Added> {
        let mut out: Vec<Added> = Vec::new();
        let n = self.tr.msgs.len();
        if n == 0 {
            return out;
        }
        let mut item = |kind, label: String, tokens: i64, state| out.push(Added { kind, label, tokens, state });
        // the standing instructions, one line each after the header
        for line in self.rules_shown.lines().skip(1) {
            let text = line.split_once(": ").map_or(line, |(_, t)| t);
            item(AddedKind::Rule, self.label_memo.of(text), est_str(line), AddedState::In);
        }
        // the anchor
        if !self.base_map.is_empty() {
            let summary = self.base_map.split("\n\n").next().unwrap_or("");
            // named by the requests it lists; without any, by what it is
            let asks: Vec<&str> = summary.lines().filter(|l| l.starts_with("- ")).collect();
            let asks = asks.join(" ");
            let name = match self.label_memo.of(&asks) {
                l if l == NO_WORDS => "earlier messages".to_string(),
                l => l,
            };
            item(AddedKind::Start, name, est_str(summary), AddedState::In);
            if self.p.notes != 0 {
                for (_, line) in &self.findings {
                    item(AddedKind::Memory, self.label_memo.of(line), est_str(line), AddedState::In);
                }
                if let Some((_, line)) = &self.plan {
                    item(AddedKind::Goal, self.label_memo.of(line), est_str(line), AddedState::In);
                }
            }
            let clip_at = 4 * self.p.pin.div_euclid(2);
            for (_, path, text) in self.pins.values() {
                let cut = clip_at > 0 && textops::len(text) >= clip_at as usize;
                let state = if cut { AddedState::Trimmed } else { AddedState::In };
                item(AddedKind::Start, label(None, Some(path), None, ""), est_str(text), state);
            }
            for w in self.wrote.values().filter(|w| w.mi < self.start) {
                let cut = w.text.contains("lines cut …") || w.text.contains("characters cut …");
                let state = if cut { AddedState::Trimmed } else { AddedState::In };
                item(AddedKind::Start, label(None, Some(&w.path), None, ""), est_str(&w.text), state);
            }
            let head = format!("{MARK} Facts from the messages that moved out");
            if let Some(at) = self.base_map.rfind(&head) {
                let text = &self.base_map[at..];
                let body = text.split_once('\n').map_or("", |(_, rest)| rest);
                item(AddedKind::Memory, self.label_memo.of(body), est_str(text), AddedState::In);
            }
        }
        // the cards of the other topics, as they were put in the messages the view sends
        let seen: FxHashSet<usize> = self.visible().iter().copied().collect();
        let mut cards: Vec<(&usize, &String)> = self.frozen.iter().filter(|(mi, _)| seen.contains(mi)).collect();
        cards.sort();
        for (_, card) in cards {
            for line in card.lines().filter(|l| l.starts_with("• messages")) {
                item(AddedKind::Thread, label(card_topic(line), None, None, line), est_str(line), AddedState::In);
            }
        }
        // the steps brought back
        for s in self.steps.iter().filter(|s| !s.out && s.at.is_some()) {
            let call = step_call(&s.did);
            item(AddedKind::Recalled, label(None, None, call.as_deref(), ""), s.tokens, AddedState::In);
        }
        // held back: the other topics when they are apart, and the outputs the view does not send whole
        // (the label of a topic is made for the ones that are shown: `Some(ask)`)
        let mut held: Vec<(Added, Option<usize>)> = Vec::new();
        if self.apart() {
            let g = &self.goal;
            for k in g.others() {
                let spans = g.members[k].iter().map(|&j| g.span(j, n));
                let tokens: i64 = spans.map(|(a, b)| self.sizes[a.max(1).min(b)..b].iter().sum::<i64>()).sum();
                if tokens > 0 {
                    held.push((
                        Added { kind: AddedKind::Thread, label: String::new(), tokens, state: AddedState::Out },
                        Some(g.members[k][0]),
                    ));
                }
            }
        }
        match self.kept_from() {
            Some(from) if kept => held = self.held_kept(from, &seen),
            _ => {
                // the same call or file left out several times is one item
                let mut outputs: Outputs = Outputs::new();
                for mi in self.counted().into_iter().flat_map(|(a, b)| a..b) {
                    self.held_of(mi, seen.contains(&mi), &mut outputs);
                }
                for (name, (tokens, times)) in outputs {
                    held.push((out_item(&name, tokens, times), None));
                }
                held.sort_by_key(|a| std::cmp::Reverse(a.0.tokens));
                if held.len() > OUT_NAMED {
                    let rest = held.split_off(OUT_NAMED);
                    held.push((smaller(rest.len(), rest.iter().map(|a| a.0.tokens).sum()), None));
                }
            }
        }
        out.extend(held.into_iter().map(|(mut a, ask)| {
            if let Some(j) = ask {
                a.label = label(Some(&self.goal.ask(&self.tr, &self.fz, j)), None, None, "");
            }
            a
        }));
        out
    }
}

/// What a step's calls did, for its name: the first, and how many more.
fn step_call(did: &[String]) -> Option<String> {
    let first = did.first()?;
    Some(if did.len() > 1 { format!("{first} +{}", did.len() - 1) } else { first.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_is_the_thread_then_the_path_then_the_command_then_keywords() {
        let text = "Rename retry_backoff in client.rs so the jitter stays below the cap";
        assert_eq!(
            label(Some("Fix the retry loop"), Some("src/client.rs"), Some("read a"), text),
            "Fix the retry loop"
        );
        assert_eq!(label(None, Some("src/client.rs"), Some("read a"), text), "src/client.rs");
        assert_eq!(label(Some("  "), None, Some("bash cargo test"), text), "bash cargo test");
        assert_eq!(label(None, None, None, text), "Rename retry_backoff client.rs");
        assert_eq!(label(None, None, None, "the and for"), "(no words)");
    }

    #[test]
    fn keywords_prefer_identifiers_and_the_rare_and_never_change() {
        let text = "tokens tokens tokens parse_header in lexer/mod.rs and tokens again";
        let k = keywords(text);
        assert_eq!(k, ["parse_header", "lexer/mod.rs", "again"]);
        assert_eq!(keywords(text), k, "the same text names the same words");
        assert!(keywords("12 345 ab").is_empty());
    }

    #[test]
    fn a_long_label_is_cut() {
        let l = label(Some(&"x".repeat(200)), None, None, "");
        assert_eq!(l.chars().count(), LABEL_CHARS);
        assert!(l.ends_with('…'));
    }

    #[test]
    fn a_card_names_its_topic_by_what_was_asked() {
        let card = "• messages 3–9: asked \"Add a retry to the uploader\", then \"and log it\". Did (2): read a.rs";
        assert_eq!(card_topic(card), Some("Add a retry to the uploader"));
        assert_eq!(card_topic("• … and 3 older topics."), None);
    }
}
