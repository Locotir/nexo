//! The tail of the view: what the session settled that the action about to be made supposes without naming it.
//!
//! It goes as text at the end of the last message, which is new on every call, so it costs the cache nothing,
//! and it is not part of the history: the next call builds it again from what is due then.

use serde_json::Value;

use crate::json::est;
use crate::msg::{blocks, get_str, text_block, text_of};
use crate::standing::CALL_KEYS;

use super::notes::note_text;
use super::view::add_blocks;
use super::Engine;

/// Standing facts in one tail.
const FACTS: usize = 12;

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

    /// Puts the standing facts due on the last message of ``view`` (when it is the person's, or carries tool
    /// results) as one text block of `Standing:` lines, and counts its tokens for `tokens`.
    pub(super) fn add_tail(&mut self, view: &mut [Value]) {
        self.tail_tokens = 0;
        if self.p.rules == 0 || self.standing.is_empty() {
            return;
        }
        let Some(last) = view.last_mut().filter(|m| m.get("role").and_then(Value::as_str) == Some("user")) else {
            return;
        };
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
        if due.is_empty() {
            return;
        }
        let text = due.iter().map(|l| format!("Standing: {l}")).collect::<Vec<_>>().join("\n");
        let block = text_block(&text);
        self.tail_tokens = est(&block);
        add_blocks(last, [block]);
        self.bump("standing", due.len() as i64);
    }
}
