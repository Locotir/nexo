//! Resolve graph hits against the host's original messages, never against shortened tracer output.
//! Positions are (unit, line). Identical text at different positions retains different provenance.
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

    pub(super) fn relation_lines(&mut self, history: &[Value], mut budget: i64) -> Vec<String> {
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
        let mut rows = Vec::new();
        for p in picks {
            if rows.len() >= REL_OUT || budget <= 0 {
                break;
            }
            let u = &tr.units[p.0 as usize];
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
            }
        }
        rows
    }
}
