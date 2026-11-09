//! The tail of the view: what the session settled that the action about to be made supposes without naming it,
//! and the lines of the history that the person's request names (the index, `index.rs`).
//!
//! It goes as text at the end of the last message, which is new on every call, so it costs the cache nothing,
//! and it is not part of the history: the next call builds it again from what is due then.

use serde_json::Value;

use crate::hindsight::UKind;
use crate::json::{est, est_str};
use crate::msg::{blocks, get_str, text_block, text_of};
use crate::standing::CALL_KEYS;

use super::notes::note_text;
use super::view::add_blocks;
use super::Engine;

/// Standing facts in one tail.
const FACTS: usize = 12;

/// Lines of the history one tail brings back, at most for each key of the request and in all, and the tokens they
/// may take (the standing facts count apart).
const PER_KEY: usize = 8;
const LINES: usize = 40;
const LINE_TOKENS: i64 = 3000;

/// Characters of a line shown.
const SHOWN: usize = 300;

impl Engine {
    /// The standing facts in message ``m`` as it arrives (see `standing.rs`).
    pub(super) fn take_standing(&mut self, m: &Value) {
        let mi = self.tr.msgs.len();
        let bs = blocks(m.get("content"));
        let texts: Vec<String> = bs.iter().filter_map(note_text).collect();
        let results: Vec<String> = bs
            .iter()
            .filter(|b| get_str(b, "type") == "tool_result")
            .map(|b| text_of(b.get("content")).into_owned())
            .collect();
        let calls: Vec<String> = bs
            .iter()
            .filter(|b| get_str(b, "type") == "tool_use")
            .flat_map(|b| {
                CALL_KEYS.iter().filter_map(|k| b.get("input").and_then(|i| i.get(*k)).and_then(Value::as_str))
            })
            .map(str::to_string)
            .collect();
        let user = m.get("role").and_then(Value::as_str) == Some("user");
        self.standing.see(mi, user, &texts, &results, &calls);
    }

    /// Indexes the lines of the units that just arrived (``new``), and keeps the person's text as the request the
    /// tail answers.
    pub(super) fn take_index(&mut self, new: std::ops::Range<usize>) {
        for i in new {
            let kind = self.tr.units[i].kind;
            if kind == UKind::Thinking {
                continue;
            }
            let text = self.tr.unit_text(i);
            self.index.add(i, &text);
            if kind == UKind::User && !text.trim().is_empty() {
                self.index.want(i, &text);
            }
        }
    }

    /// The lines of the history the person's latest request names that the view does not hold whole, as text:
    /// the best first, grouped by message (`m#####`, as `recall` names it).
    fn index_lines(&mut self) -> Vec<String> {
        if self.index.is_empty() {
            return Vec::new();
        }
        let vis = self.visible();
        let hide0 = self.m0_card(self.apart());
        // the index is taken out for the lookup: deciding whether a message is whole in the view reads the engine
        let index = std::mem::take(&mut self.index);
        let mut whole: rustc_hash::FxHashMap<usize, bool> = rustc_hash::FxHashMap::default();
        let picks = index.pick(PER_KEY, LINES, &mut |(u, _)| {
            let mi = self.tr.units[u as usize].msg;
            *whole.entry(mi).or_insert_with(|| {
                vis.binary_search(&mi).is_ok() && if mi == 0 { !hide0 } else { self.message(mi).0.is_none() }
            })
        });
        self.index = index;
        let mut texts: rustc_hash::FxHashMap<u32, Vec<&str>> = rustc_hash::FxHashMap::default();
        let units: rustc_hash::FxHashSet<u32> = picks.iter().map(|p| p.0).collect();
        let owned: Vec<(u32, String)> =
            units.into_iter().map(|u| (u, self.tr.unit_text(u as usize).into_owned())).collect();
        for (u, t) in &owned {
            texts.insert(*u, t.lines().collect());
        }
        let (mut budget, mut seen) = (LINE_TOKENS, rustc_hash::FxHashSet::default());
        let mut shown: Vec<(usize, u32, u32, String)> = Vec::new();
        for (u, n) in picks {
            let Some(line) = texts.get(&u).and_then(|l| l.get(n as usize)) else { continue };
            let line = line.trim();
            if line.is_empty() || !seen.insert(line) {
                continue;
            }
            let line: String = match line.char_indices().nth(SHOWN) {
                Some((i, _)) => format!("{}…", &line[..i]),
                None => line.to_string(),
            };
            budget -= est_str(&line) + 1;
            if budget < 0 {
                break;
            }
            shown.push((self.tr.units[u as usize].msg, u, n, line));
        }
        shown.sort_by_key(|s| (s.0, s.1, s.2));
        let (mut out, mut at): (Vec<String>, Option<usize>) = (Vec::new(), None);
        for (mi, _, _, line) in shown {
            if at != Some(mi) {
                out.push(format!("m{:05}:", mi + 1));
                at = Some(mi);
            }
            out.push(line);
        }
        out
    }

    /// Puts the standing facts due, and the lines of the history the person's request names, on the last message
    /// of ``view`` (when it is the person's, or carries tool results) as one text block, and counts its tokens
    /// for `tokens`. The standing facts go first, as `Standing:` lines, and do not count against the lines.
    pub(super) fn add_tail(&mut self, view: &mut [Value]) {
        self.tail_tokens = 0;
        if self.p.rules == 0 {
            return;
        }
        let Some(last) = view.last_mut().filter(|m| m.get("role").and_then(Value::as_str) == Some("user")) else {
            return;
        };
        let mut lines: Vec<String> = Vec::new();
        if !self.standing.is_empty() {
            // a fact whose message is whole in the view is there already
            let vis = self.visible();
            let hide0 = self.m0_card(self.apart());
            let whole: rustc_hash::FxHashSet<usize> = self
                .standing
                .sources()
                .into_iter()
                .filter(|&s| vis.binary_search(&s).is_ok() && if s == 0 { !hide0 } else { self.message(s).0.is_none() })
                .collect();
            let rules = &self.rules_shown;
            let due =
                self.standing.due(FACTS, &|src, line| src.is_some_and(|s| whole.contains(&s)) || rules.contains(line));
            if !due.is_empty() {
                self.bump("standing", due.len() as i64);
            }
            lines.extend(due.iter().map(|l| format!("Standing: {l}")));
        }
        // the lines the request names go with the request, not with every step it takes after: the answers the
        // agent has by then carry what it made of them
        let step = blocks(last.get("content")).iter().any(|b| get_str(b, "type") == "tool_result");
        let found = if step { Vec::new() } else { self.index_lines() };
        if !found.is_empty() {
            lines.push(format!(
                "Recall (lines of earlier messages that this request names{}):",
                if self.recall_tool { "; recall m##### brings one back whole" } else { "" }
            ));
            lines.extend(found);
        }
        if lines.is_empty() {
            return;
        }
        let block = text_block(&lines.join("\n"));
        self.tail_tokens = est(&block);
        add_blocks(last, [block]);
    }
}
