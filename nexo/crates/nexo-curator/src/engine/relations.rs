//! Resolve graph hits against the host's original messages, never against shortened tracer output.
//! Positions are (unit, line). Identical text at different positions retains different provenance.
#[path = "segments.rs"]
mod segments;

use std::ops::Range;

use serde_json::{json, Value};

use super::{files::same_file, Engine};
use crate::facts::keys;
use crate::hindsight::{UKind, Unit};
use crate::index::{Index, Post, REL_OUT};
use crate::json::{est_str, is_person};
use crate::msg::{get_str, request_words};
use crate::tools::{classify, Kind};

fn source<'a>(m: &'a Value, u: &Unit) -> Option<&'a str> {
    let content = m.get("content")?;
    match u.kind {
        UKind::User | UKind::Text => {
            if u.kind == UKind::User && !is_person(m) {
                return None;
            }
            let text = content.as_str().or_else(|| {
                let b = content.as_array()?.get(u.block)?;
                (get_str(b, "type") == "text").then(|| b.get("text")?.as_str())?
            })?;
            // An attached file or previous memory is not a statement by the person.
            if u.kind == UKind::User && request_words(text).as_ref() != text {
                return None;
            }
            Some(text)
        }
        UKind::Result => {
            let b = content.as_array()?.get(u.block)?;
            if m["role"] != "user" || b["type"] != "tool_result" || b["is_error"] == true {
                return None;
            }
            let c = b.get("content")?;
            c.as_str().or_else(|| match c.as_array()?.as_slice() {
                [one] if one["type"] == "text" => one["text"].as_str(),
                _ => None,
            })
        }
        _ => None,
    }
}

fn original_line<'a>(index: &Index, history: &'a [Value], units: &[Unit], p: Post) -> Option<&'a str> {
    let u = units.get(p.0 as usize)?;
    index.relation_line(p, source(history.get(u.msg)?, u)?)
}

// Only the immediately preceding assistant batch can supply call provenance. Require a bijection
// for this ID in the two batches; missing, repeated or unmatched IDs are not evidence of a pair.
fn paired<'a>(history: &'a [Value], u: &Unit) -> Option<(usize, usize, &'a str, &'a Value)> {
    let mi = u.msg.checked_sub(1)?;
    let prev = history.get(mi)?;
    if prev["role"] != "assistant" {
        return None;
    }
    let batch = history.get(u.msg)?.get("content")?.as_array()?;
    let id = batch.get(u.block)?.get("tool_use_id")?.as_str().filter(|s| !s.is_empty())?;
    if batch.iter().filter(|b| b["type"] == "tool_result" && b["tool_use_id"].as_str() == Some(id)).take(2).count() != 1
    {
        return None;
    }
    let mut matches = prev
        .get("content")?
        .as_array()?
        .iter()
        .enumerate()
        .filter(|(_, b)| b["type"] == "tool_use" && b["id"].as_str() == Some(id));
    let (bi, call) = matches.next()?;
    matches.next().is_none().then_some((mi, bi, id, call))
}

impl Engine {
    pub(super) fn take_relation_index(&mut self, new: Range<usize>) {
        for i in new {
            let u = &self.tr.units[i];
            if let Some(text) = source(&self.last_raw, u) {
                self.index.mt().add_relation(i, text);
            }
            if u.kind == UKind::User && is_person(&self.last_raw) {
                let text = self.tr.unit_text(i);
                let query = request_words(&text);
                if !query.trim().is_empty() {
                    self.index.mt().want_relation(i, &query);
                }
            }
        }
    }

    pub(super) fn relation_lines(&mut self, history: &[Value], budget: i64) -> Vec<String> {
        if budget <= 0 || self.index.is_empty() || history.len() != self.tr.msgs.len() {
            return vec![];
        }
        let vis = self.visible();
        let hide0 = self.m0_card(self.apart());
        // A new task can share words with a large output of an unrelated turn. As in
        // keyword retrieval, do not let those accidental hits bring the old task back.
        let changed = self.goal.changed();
        let last = history.len().saturating_sub(1);
        let mut allowed = crate::sorted::FxHashMap::default();
        let index = self.index.clone();
        let tr = self.tr.clone();
        let eligible = |p: Post| {
            let mi = tr.units[p.0 as usize].msg;
            *allowed.entry(mi).or_insert_with(|| {
                if mi >= last || (changed && !self.goal.goes_on(mi)) {
                    return false;
                }
                let whole = vis.binary_search(&mi).is_ok()
                    && !self.own.born.contains(&mi)
                    && if mi == 0 { !hide0 } else { self.message(mi).0.is_none() };
                !whole
            })
        };
        let picks = index.relation_pick(
            |p| {
                let mut words = Vec::new();
                if let Some(line) = original_line(&index, history, &tr.units, p) {
                    keys(line, &mut words);
                }
                words
            },
            eligible,
        );
        self.admit_relation_lines(history, &picks, budget)
    }

    /// Budget the unmodified selector first. Context uses only residual tokens/slots and can only
    /// come from an original quote actually admitted, never from a rejected expensive seed.
    fn admit_relation_lines(&self, history: &[Value], picks: &[Post], mut budget: i64) -> Vec<String> {
        let mut rows = Vec::new();
        let mut admitted = Vec::new();
        // `pending` is later replaced by an owned iterator over the expanded picks.
        #[allow(clippy::unnecessary_to_owned)]
        let mut pending = picks.to_vec().into_iter();
        let mut expanded = !self.p.segments;
        loop {
            let p = match pending.next() {
                Some(p) => p,
                None if !expanded => {
                    expanded = true;
                    pending = segments::expand(&self.index, history, &self.tr.units, &admitted, picks).into_iter();
                    continue;
                }
                None => break,
            };
            if rows.len() >= REL_OUT || budget <= 0 {
                break;
            }
            let u = &self.tr.units[p.0 as usize];
            let mi = u.msg;
            let Some(line) = original_line(&self.index, history, &self.tr.units, p) else { continue };
            let kind = match u.kind {
                UKind::Result => "tool",
                UKind::User => "person",
                UKind::Text => "assistant",
                _ => continue,
            };
            let mut row = json!({"kind":kind,"message":format!("m{:05}", mi + 1),"block":u.block + 1,"line":p.1 + 1,"quote":line});
            if u.kind == UKind::Result {
                if let Some((cm, cb, id, call)) = paired(history, u) {
                    row["call_message"] = json!(format!("m{:05}", cm + 1));
                    row["call_block"] = json!(cb + 1);
                    row["call_id"] = json!(id);
                    let (kind, path, _) = classify(get_str(call, "name"), call.get("input"));
                    if let Some(path) = path.filter(|_| kind == Kind::Read) {
                        if self.file_edits.iter().any(|(file, when)| *when > mi && same_file(file, &path)) {
                            row["status"] = json!("possibly superseded by a later edit");
                        }
                        row["path"] = json!(path);
                    }
                }
            }
            let line = row.to_string();
            let cost = est_str(&line) + 2;
            if cost <= budget {
                budget -= cost;
                rows.push(line);
                if !expanded {
                    admitted.push(p);
                }
            }
        }
        rows
    }
}

#[cfg(test)]
mod segment_budget_tests {
    use super::*;
    use crate::params::params;

    fn fixture(body: &str, question: &str) -> (Engine, Vec<Value>, Vec<Post>) {
        let history = vec![
            json!({"role":"user","content":"Audit the museum inventory ledger."}),
            json!({"role":"assistant","content":[{"type":"tool_use","id":"read-a","name":"read_file","input":{"path":"west/ledger.csv"}}]}),
            json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"read-a","content":body}]}),
            json!({"role":"assistant","content":"Inventory pass completed."}),
            json!({"role":"user","content":question}),
        ];
        let mut e = Engine::new(params("segments=1").unwrap());
        e.sync(&history);
        let picks = e.index.relation_pick(
            |p| {
                let mut words = Vec::new();
                if let Some(line) = original_line(&e.index, &history, &e.tr.units, p) {
                    keys(line, &mut words);
                }
                words
            },
            |p| e.tr.units[p.0 as usize].kind == UKind::Result,
        );
        (e, history, picks)
    }

    fn cost(rows: &[String]) -> i64 {
        rows.iter().map(|r| est_str(r) + 2).sum()
    }

    fn assert_provenance(rows: &[String], body: &str) {
        for row in rows {
            let r: Value = serde_json::from_str(row).unwrap();
            assert_eq!(r["message"], "m00003");
            assert_eq!(r["block"], 1);
            assert_eq!(r["call_message"], "m00002");
            assert_eq!(r["call_block"], 1);
            assert_eq!(r["call_id"], "read-a");
            assert_eq!(r["path"], "west/ledger.csv");
            assert_eq!(body.lines().nth(r["line"].as_u64().unwrap() as usize - 1), r["quote"].as_str());
        }
    }

    #[test]
    fn segments_preserve_three_requested_hits_under_every_token_budget() {
        let body = "spindle916 receipt amber\nneighbor cobalt\n\nbeacon254 receipt indigo\nneighbor crimson\n\nfinial409 receipt topaz\nneighbor bronze";
        let (mut e, h, picks) = fixture(body, "Locate spindle916 beacon254 finial409 receipts.");
        assert_eq!(picks.len(), 3, "fixture must exercise three direct selector hits");
        e.p.segments = false;
        let all = e.admit_relation_lines(&h, &picks, 100_000);
        let exact = cost(&all);
        assert_eq!(all.len(), 3);
        for budget in -1..=exact + 250 {
            e.p.segments = false;
            let plain = e.admit_relation_lines(&h, &picks, budget);
            e.p.segments = true;
            let segment = e.admit_relation_lines(&h, &picks, budget);
            assert!(segment.starts_with(&plain), "original evidence displaced at budget {budget}");
            assert!(cost(&segment) <= budget.max(0));
            assert_provenance(&segment, body);
        }
        e.p.segments = true;
        assert_eq!(e.admit_relation_lines(&h, &picks, exact), all);
        assert!(e.admit_relation_lines(&h, &picks, exact + 250).len() > all.len());
    }

    #[test]
    fn segments_preserve_selector_at_output_limit() {
        let body: String = (0..REL_OUT).map(|i| format!("spindle916 receipt {i}\nneighbor cobalt {i}\n\n")).collect();
        let (mut e, h, picks) = fixture(&body, "Locate spindle916 receipts.");
        assert!(picks.len() >= REL_OUT);
        e.p.segments = false;
        let plain = e.admit_relation_lines(&h, &picks, 100_000);
        assert_eq!(plain.len(), REL_OUT);
        e.p.segments = true;
        assert_eq!(e.admit_relation_lines(&h, &picks, 100_000), plain);
        assert_provenance(&plain, &body);
    }

    #[test]
    fn segments_never_quote_neighbors_of_an_unaffordable_anchor() {
        let body = format!(
            "spindle916 receipt {}\nneighbor cobalt\n\nbeacon254 receipt indigo\nneighbor crimson",
            "界".repeat(350)
        );
        let (mut e, h, picks) = fixture(&body, "Locate spindle916 beacon254 receipts.");
        assert_eq!(picks.len(), 2);
        let anchor = picks.iter().copied().find(|p| p.1 == 0).unwrap();
        let cheap = picks.iter().copied().find(|p| p.1 == 3).unwrap();
        e.p.segments = false;
        let expensive_cost = cost(&e.admit_relation_lines(&h, &[anchor], 100_000));
        let cheap_cost = cost(&e.admit_relation_lines(&h, &[cheap], 100_000));
        let neighbor_cost = cost(&e.admit_relation_lines(&h, &[(cheap.0, cheap.1 + 1)], 100_000));
        e.p.segments = true;
        for budget in 1..expensive_cost {
            // No accepted seed: even a neighbor that would fit alone must stay out.
            assert!(e.admit_relation_lines(&h, &[anchor], budget).is_empty());
        }
        let budget = cheap_cost + neighbor_cost;
        assert!(budget < expensive_cost);
        let rows = e.admit_relation_lines(&h, &picks, budget);
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|r| !r.contains("neighbor cobalt")));
        assert!(rows.iter().any(|r| r.contains("neighbor crimson")), "admitted cheap seed may expand");
        assert_provenance(&rows, &body);
    }
}
