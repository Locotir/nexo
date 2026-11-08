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
        *counts.entry(low.clone()).or_insert(0) += 1;
        if !seen.iter().any(|(_, _, l)| *l == low) {
            seen.push((short(w).into_owned(), at, low));
        }
    }
    let score = |(w, _, low): &(String, usize, String)| {
        let n = counts[low].max(1);
        identifier_weight(w) + 200 / n + w.chars().count().min(14) as i64
    };
    let mut order: Vec<usize> = (0..seen.len()).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(score(&seen[i])), seen[i].1));
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
        "(no words)".to_string()
    } else {
        clip(&words.join(" "))
    }
}

/// The ask a card names its topic by: what follows `asked "` up to the closing quote.
fn card_topic(card: &str) -> Option<&str> {
    let rest = card.split_once("asked \"")?.1;
    let end = rest.find('"').unwrap_or(rest.len());
    Some(&rest[..end])
}

impl Engine {
    /// What the last view carries beyond the conversation, and what it holds back, one item each. The
    /// anchor's pieces (the map of what moved out, the files it pins and the ones the agent wrote, the
    /// agent's findings and plan, the ledger and the standing instructions), the cards of other topics, the
    /// steps brought back; and, held back, the other topics and the steps out of the view.
    pub fn added(&mut self) -> Vec<Added> {
        let mut out: Vec<Added> = Vec::new();
        let n = self.tr.msgs.len();
        if n == 0 {
            return out;
        }
        let mut item = |kind, label: String, tokens: i64, state| out.push(Added { kind, label, tokens, state });
        // the standing instructions, one line each after the header
        for line in self.rules_shown.lines().skip(1) {
            let text = line.split_once(": ").map_or(line, |(_, t)| t);
            item(AddedKind::Rule, label(None, None, None, text), est_str(line), AddedState::In);
        }
        // the anchor
        if !self.base_map.is_empty() {
            let summary = self.base_map.split("\n\n").next().unwrap_or("");
            // named by the requests it lists; without any, by what it is
            let asks: Vec<&str> = summary.lines().filter(|l| l.starts_with("- ")).collect();
            let name = if keywords(&asks.join(" ")).is_empty() {
                "earlier messages".to_string()
            } else {
                label(None, None, None, &asks.join(" "))
            };
            item(AddedKind::Start, name, est_str(summary), AddedState::In);
            if self.p.notes != 0 {
                for (_, line) in &self.findings {
                    item(AddedKind::Memory, label(None, None, None, line), est_str(line), AddedState::In);
                }
                if let Some((_, line)) = &self.plan {
                    item(AddedKind::Goal, label(None, None, None, line), est_str(line), AddedState::In);
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
                item(AddedKind::Memory, label(None, None, None, body), est_str(text), AddedState::In);
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
        let mut held: Vec<Added> = Vec::new();
        let mut apart: FxHashSet<usize> = FxHashSet::default();
        if self.apart() {
            let g = &self.goal;
            for k in g.others() {
                let spans: Vec<(usize, usize)> = g.members[k].iter().map(|&j| g.span(j, n)).collect();
                let tokens: i64 = spans.iter().map(|&(a, b)| self.sizes[a.max(1).min(b)..b].iter().sum::<i64>()).sum();
                for &(a, b) in &spans {
                    apart.extend(a.max(1)..b);
                }
                if tokens > 0 {
                    let ask = g.ask(&self.tr, &self.fz, g.members[k][0]);
                    held.push(Added {
                        kind: AddedKind::Thread,
                        label: label(Some(&ask), None, None, ""),
                        tokens,
                        state: AddedState::Out,
                    });
                }
            }
        }
        // the same call or file left out several times is one item
        let mut outputs: indexmap::IndexMap<String, (i64, usize)> = indexmap::IndexMap::new();
        for mi in 0..n {
            let sent = seen.contains(&mi);
            if !sent && apart.contains(&mi) {
                continue;
            }
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
                let info = self.fz.info.get(&i);
                let path = info.and_then(|x| x.path.as_deref()).filter(|p| !p.is_empty());
                let call =
                    format!("{} {}", u.tool, info.and_then(|x| x.key.as_deref()).unwrap_or("")).trim().to_string();
                let name = label(None, path, Some(&call), "");
                let e = outputs.entry(name).or_insert((0, 0));
                e.0 += tokens;
                e.1 += 1;
            }
        }
        for (name, (tokens, times)) in outputs {
            let name = if times > 1 { format!("{} ×{times}", textops::head(&name, LABEL_CHARS - 5)) } else { name };
            held.push(Added { kind: AddedKind::Recalled, label: name, tokens, state: AddedState::Out });
        }
        held.sort_by_key(|a| std::cmp::Reverse(a.tokens));
        if held.len() > OUT_NAMED {
            let rest = held.split_off(OUT_NAMED);
            held.push(Added {
                kind: AddedKind::Recalled,
                label: format!("{} smaller items", rest.len()),
                tokens: rest.iter().map(|a| a.tokens).sum(),
                state: AddedState::Out,
            });
        }
        out.extend(held);
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
