//! The view as sent: each message built from the history, its outputs shrunk or left whole.

use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::{Map, Value};

use crate::goal::head_slice;
use crate::hindsight::UKind;
use crate::json::est;
use crate::mechanical::MARK;
use crate::msg::{blocks, get_str, text_block, text_of};
use crate::textops::{self};
use crate::tools::{classify, Kind};

use super::cache::fingerprint;
use super::dups::{len_of, put_lines, Dup};
use super::Engine;

/// The arguments of an edit that hold the text it replaced, in the shapes editor tools name them.
const REPLACED_ARGS: &[&str] = &["old_string", "old_str", "old_text"];

#[derive(Clone)]
pub(super) struct Built {
    key: Vec<(usize, usize)>,
    map_ver: u64,
    /// Where each of the message's runs pointed (`dups_on`).
    on: Vec<Option<(usize, usize)>>,
    msg: Option<Value>,
    size: i64,
    /// A hash of the message as built (`pockets`).
    pub(super) fp: u64,
}

/// The visible messages, by (messages, start, turns, split) they were worked out for.
pub(super) type Visible = ((usize, usize, usize, bool), Arc<Vec<usize>>);

/// ``more`` blocks at the end of message ``m``'s content. A text block left empty (a message that came with
/// no text) goes: providers refuse an empty text block, and what is added says more than it.
pub(super) fn add_blocks(m: &mut Value, more: impl IntoIterator<Item = Value>) {
    let mut bs = blocks(m.get("content")).into_owned();
    bs.retain(|b| get_str(b, "type") != "text" || !get_str(b, "text").is_empty());
    bs.extend(more);
    m["content"] = Value::Array(bs);
}

impl Engine {
    /// Takes the units that came since the last call into `unit_first`.
    pub(super) fn index_units(&mut self) {
        for i in self.unit_seen..self.tr.units.len() {
            let m = self.tr.units[i].msg;
            debug_assert!(i == 0 || self.tr.units[i - 1].msg <= m, "units come in the order of the messages");
            while self.unit_first.len() <= m {
                self.unit_first.push(i);
            }
        }
        self.unit_seen = self.tr.units.len();
    }

    /// The units of message `mi` (they come in the order of the messages; `index_units` first).
    pub(super) fn unit_range(&self, mi: usize) -> std::ops::Range<usize> {
        let end = self.unit_seen;
        let a = self.unit_first.get(mi).copied().unwrap_or(end);
        let b = self.unit_first.get(mi + 1).copied().unwrap_or(end);
        a..b
    }

    pub(super) fn visible(&mut self) -> Arc<Vec<usize>> {
        let key = (self.tr.msgs.len(), self.start, self.goal.turns.len(), self.split);
        if let Some((k, v)) = &self.vis {
            if *k == key {
                return v.clone();
            }
        }
        let g = &self.goal;
        let apart = self.apart();
        let out: Vec<usize> = if g.turns.is_empty() {
            (0..key.0).collect()
        } else if !apart {
            // the first message, and from `start` on the ones not shut
            let from = self.start.max(1);
            let mut out = if key.0 > 0 { vec![0] } else { Vec::new() };
            out.extend((from..key.0).filter(|mi| !self.shut_out.contains(mi)));
            out
        } else {
            let mut out = vec![0];
            for &j in &g.members[g.active()] {
                let (a, b) = g.span(j, key.0);
                out.extend((a.max(self.start).max(1)..b).filter(|mi| !self.shut_out.contains(mi)));
            }
            out
        };
        let out = Arc::new(out);
        self.vis = Some((key, out.clone()));
        out
    }

    fn key(&self, mi: usize) -> Vec<(usize, usize)> {
        self.fz.by_msg(mi).iter().filter_map(|i| self.dropped.get(i).map(|d| (*i, d.len()))).collect()
    }

    /// Message ``mi`` as sent now and its tokens (`None` when it is sent as it arrived).
    pub(super) fn message(&mut self, mi: usize) -> (Option<&Value>, i64) {
        let key = self.key(mi);
        let on = self.dups_on(mi);
        // (the first message that leaves as a card is told apart from the one that stays by the version)
        let hide = mi == 0 && self.m0_card(self.apart());
        let map_ver = if mi == 0 { 2 * self.map_ver + hide as u64 } else { 0 };
        let plain = key.is_empty()
            && !hide
            && (mi != 0 || self.map_text.is_empty())
            && !self.frozen.contains_key(&mi)
            && !self.shut.contains_key(&mi)
            && !self.nudged.contains(&mi)
            && on.iter().all(Option::is_none);
        if plain {
            return (None, self.sizes[mi]);
        }
        let fresh = self.built.get(&mi).is_some_and(|b| b.key == key && b.map_ver == map_ver && b.on == on);
        if !fresh {
            let (msg, size) = self.build(mi, !key.is_empty(), &on, hide);
            let fp = fingerprint(&msg);
            self.built.insert(mi, Built { key, map_ver, on, msg: Some(msg), size, fp });
        }
        let b = &self.built[&mi];
        (b.msg.as_ref(), b.size)
    }

    /// Message `mi` as sent now: its cut parts left out, and its runs that `on` marks as pointers.
    fn build(&self, mi: usize, cut: bool, on: &[Option<(usize, usize)>], hide: bool) -> (Value, i64) {
        let m = &self.tr.msgs[mi];
        let mut m2 = m.clone();
        if hide {
            // its line is in the cards, with the way to bring it back
            m2["content"] = Value::Array(Vec::new());
        }
        let runs: Vec<Dup> = match self.dups.get(&mi) {
            Some(ds) => ds
                .iter()
                .zip(on)
                .filter_map(|(d, p)| p.map(|p| Dup { stub: self.stub_at(d, p), ..d.clone() }))
                .collect(),
            None => Vec::new(),
        };
        if let (true, Some(Value::Array(content))) = (!hide && (cut || !runs.is_empty()), m.get("content")) {
            let mut new = Vec::new();
            for (bi, b) in content.iter().enumerate() {
                let Some(&i) = self.tr.at.get(&(mi, bi)) else {
                    new.push(b.clone());
                    continue;
                };
                let mine: Vec<&Dup> = runs.iter().filter(|d| d.unit == i).collect();
                match self.dropped.get(&i) {
                    Some(d) => {
                        if let Some(nb) = self.shrink(i, d, b, &mine) {
                            new.push(nb);
                        }
                    }
                    None if !mine.is_empty() => {
                        let text = self.tr.unit_text(i);
                        let lines: Vec<&str> = text.split('\n').collect();
                        let mut out = Vec::new();
                        put_lines(&lines, (0, lines.len()), &mine, &mut Vec::new(), &mut out);
                        let mut nb = b.clone();
                        nb["content"] = Value::String(out.join("\n"));
                        new.push(nb);
                    }
                    None => new.push(b.clone()),
                }
            }
            if !new.is_empty() {
                m2["content"] = Value::Array(new);
            }
        }
        let nudge = self.nudge_text();
        if self.nudged.contains(&mi) {
            if let Some(Value::Array(content)) = m2.get_mut("content") {
                let last =
                    content.iter_mut().rev().find(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"));
                if let Some(b) = last {
                    match b.get_mut("content") {
                        Some(Value::Array(parts)) => parts.push(text_block(nudge)),
                        Some(Value::String(s)) => *s = format!("{s}\n\n{nudge}"),
                        _ => b["content"] = Value::String(nudge.to_string()),
                    }
                }
            }
        }
        if mi == 0 && !self.map_text.is_empty() {
            add_blocks(&mut m2, [text_block(&self.map_text)]);
        }
        if let Some(lines) = self.shut.get(&mi) {
            add_blocks(&mut m2, lines.iter().map(|l| text_block(l)));
        }
        let mut size = est(&m2);
        if let Some(card) = self.frozen.get(&mi) {
            let tb = text_block(card);
            size += est(&tb);
            add_blocks(&mut m2, [tb]);
        }
        (m2, size)
    }

    /// Unit `i` (block `b`) without the chunks in `drop`; the runs of `runs` in what is kept go as pointers.
    fn shrink(&self, i: usize, drop: &FxHashSet<usize>, b: &Value, runs: &[&Dup]) -> Option<Value> {
        let u = &self.tr.units[i];
        let chunks = &self.fz.chunks[i];
        if u.kind == UKind::Thinking {
            return None;
        }
        let mut out = b.clone();
        if u.kind == UKind::Call {
            if let Some(Value::Object(args)) = b.get("input") {
                // only a write leaves its text in a file to read back; a command or a prompt does not
                let wrote = classify(get_str(b, "name"), b.get("input")).0 == Kind::Write;
                // the text an edit replaced is gone from the file: saying the file holds it would mislead
                // a list of changes (`edits: [{old_string, new_string}]`) folds each of its entries the same way
                fn fold(k: &str, v: &Value, wrote: bool) -> Value {
                    let what = match (wrote, REPLACED_ARGS.contains(&k)) {
                        (true, true) => "this edit replaced here are omitted; they are no longer in the file",
                        (true, false) => "written here are omitted; the file on disk is current",
                        _ => "of this argument here are omitted",
                    };
                    match v {
                        Value::String(s) if textops::len(s) > 200 => {
                            Value::String(format!("{MARK} {} lines {what}", s.matches('\n').count() + 1))
                        }
                        Value::Array(a) => Value::Array(a.iter().map(|x| fold(k, x, wrote)).collect()),
                        Value::Object(o) => {
                            Value::Object(o.iter().map(|(k2, x)| (k2.clone(), fold(k2, x, wrote))).collect())
                        }
                        _ => v.clone(),
                    }
                }
                let args: Map<String, Value> = args.iter().map(|(k, v)| (k.clone(), fold(k, v, wrote))).collect();
                out["input"] = Value::Object(args);
            }
            return Some(out);
        }
        let text = text_of(b.get("content"));
        let lines: Vec<&str> = text.split('\n').collect();
        let copy = |j: usize| self.copied.contains(&(i, j));
        if drop.len() == chunks.len() && drop.iter().all(|&j| copy(j)) {
            out["content"] = Value::String(format!(
                "{MARK} {} output left out here: a later output below sends every line of it",
                u.tool
            ));
            return Some(out);
        }
        if drop.len() == chunks.len() {
            let mut stub = if let Some((path, edit)) = self.stale_gone.get(&i) {
                // a read the file's later edit made out of date: which file, which edit, and the way back
                let back = if self.recall_tool {
                    format!("recall m{:05} brings it back as it was", u.msg + 1)
                } else {
                    "read the file again to see it as it is now".to_string()
                };
                if path == super::stale::DEBUG_STATE {
                    let back = if self.recall_tool {
                        format!("recall m{:05} brings it back as it was", u.msg + 1)
                    } else {
                        "look again to see the program as it is now".to_string()
                    };
                    format!(
                        "{MARK} {} output left out here: what the program showed before it moved on in m{:05}; {back}",
                        u.tool,
                        edit + 1
                    )
                } else {
                    format!(
                        "{MARK} {} output left out here: the read of {path} from before its edit in m{:05}; {back}",
                        u.tool,
                        edit + 1
                    )
                }
            } else if self.recall_tool {
                format!(
                    "{MARK} {} output left out here, {} line{} you read already; recall m{:05} brings it back as it was",
                    u.tool,
                    lines.len(),
                    if lines.len() == 1 { "" } else { "s" },
                    u.msg + 1
                )
            } else if self.p.brief != 0 {
                format!(
                    "{MARK} {} output left out here, {} line{} you read already; run it again to see it again",
                    u.tool,
                    lines.len(),
                    if lines.len() == 1 { "" } else { "s" }
                )
            } else {
                format!(
                    "{MARK} {} output left out here, {} line{} you read already, as not needed for this step; if the step does need it, run the tool again and it comes back in full",
                    u.tool,
                    lines.len(),
                    if lines.len() == 1 { "" } else { "s" }
                )
            };
            if let Some(lines) = self.lead_lines.get(&i) {
                stub.push_str("; its lines that name what nothing has followed yet:");
                for l in lines {
                    stub.push('\n');
                    stub.push_str(l);
                }
            }
            out["content"] = Value::String(stub);
            return Some(out);
        }
        let mut kept: Vec<String> = Vec::new();
        let mut put = Vec::new();
        let n = lines.len() as i64;
        let at = |x: i64| (if x < 0 { (n + x).max(0) } else { x.min(n) }) as usize;
        for (j, c) in chunks.iter().enumerate() {
            if drop.contains(&j) {
                let mark = if copy(j) {
                    format!("{MARK} … lines omitted: a later output below sends them …")
                } else {
                    format!("{MARK} … lines omitted …")
                };
                if kept.last() != Some(&mark) {
                    kept.push(mark);
                }
            } else {
                let (a, b) = (at(c.start), at(c.end));
                if a < b {
                    put_lines(&lines, (a, b), runs, &mut put, &mut kept);
                }
            }
        }
        out["content"] = Value::String(kept.join("\n"));
        Some(out)
    }

    pub(super) fn sizes_now(&mut self) -> Vec<i64> {
        let mut out = vec![0; self.tr.msgs.len()];
        for &mi in self.visible().iter() {
            out[mi] = self.message(mi).1;
        }
        out
    }

    /// For each message, the tokens its outputs send in place of the copies that point to them from the
    /// messages `stays` keeps: were it to leave, they would go in those copies instead (`dups_on`), so
    /// leaving does not save them.
    pub(super) fn moved(&mut self, stays: impl Fn(usize) -> bool) -> FxHashMap<usize, i64> {
        let mut out: FxHashMap<usize, i64> = FxHashMap::default();
        for (u, lines) in self.pointed(stays) {
            let held = lines.iter().filter(|x| **x).count() as i64;
            *out.entry(self.tr.units[u].msg).or_default() +=
                self.tr.units[u].tokens * held / (lines.len() as i64).max(1);
        }
        out
    }

    /// The runs of the messages `vis` sent as a pointer now: (tool result, its lines `a..b`).
    pub(super) fn pointing(&mut self, vis: &[usize]) -> Vec<(usize, (usize, usize))> {
        let mut out = Vec::new();
        for &mi in vis {
            let on = self.dups_on(mi);
            for (d, at) in self.dups.get(&mi).into_iter().flatten().zip(on) {
                if at.is_some() {
                    out.push((d.unit, d.lines));
                }
            }
        }
        out
    }

    /// For each output, its lines that a copy in a visible message `stays` keeps points to now: were the
    /// output to stop sending them, that copy would send them instead.
    pub(super) fn pointed(&mut self, stays: impl Fn(usize) -> bool) -> FxHashMap<usize, Vec<bool>> {
        let mut out: FxHashMap<usize, Vec<bool>> = FxHashMap::default();
        if self.p.dedup == 0 {
            return out;
        }
        let vis = self.visible();
        let mut holders: Vec<usize> = self.dups.keys().copied().filter(|m| stays(*m)).collect();
        holders.retain(|m| vis.binary_search(m).is_ok());
        for mi in holders {
            let on = self.dups_on(mi);
            for (d, at) in self.dups[&mi].iter().zip(on) {
                if let Some((u, x)) = at {
                    let n = self.shown.get(&u).map_or(0, Vec::len);
                    let lines = out.entry(u).or_insert_with(|| vec![false; n]);
                    lines.iter_mut().skip(x).take(len_of(d)).for_each(|l| *l = true);
                }
            }
        }
        out
    }

    pub fn tokens(&mut self) -> i64 {
        self.sizes_now().iter().sum::<i64>() + self.tail_tokens
    }

    /// The view: the visible messages, consecutive ones of the same role joined.
    pub fn render(&mut self) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        for &mi in self.visible().iter() {
            let m = match self.message(mi).0 {
                Some(v) => v.clone(),
                None => self.tr.msgs[mi].clone(),
            };
            if let Some(prev) = out.last_mut() {
                if prev.get("role") == m.get("role") {
                    let mut bs = blocks(prev.get("content")).into_owned();
                    bs.extend(blocks(m.get("content")).iter().cloned());
                    prev["content"] = Value::Array(bs);
                    continue;
                }
            }
            out.push(m);
        }
        out
    }

    /// For measurement: (first message, thread) of each turn so far.
    pub fn turn_threads(&self) -> Vec<(usize, usize)> {
        self.goal.assignment()
    }

    pub fn view(&mut self) -> Vec<Value> {
        self.decide();
        let mut view = self.render();
        self.add_tail(&mut view);
        view
    }

    /// Whether the other topics leave the view. Decided once for each request (`split_now`) and kept until the
    /// next change of topic: below that the whole conversation stays, as without topics.
    pub(super) fn apart(&self) -> bool {
        self.split
    }

    /// The tokens each topic other than the active one holds (apart from the first message), and the tokens the
    /// active one holds from the first message that leaves on: what a cut there reads again.
    pub(super) fn topic_tokens(&self) -> (Vec<(usize, i64)>, i64) {
        let g = &self.goal;
        let n = self.tr.msgs.len();
        let spans = |k: usize| g.members[k].iter().map(move |&j| g.span(j, n));
        let others: Vec<(usize, i64)> = g
            .others()
            .into_iter()
            .map(|k| (k, spans(k).map(|(a, b)| self.sizes[a.max(1).min(b)..b].iter().sum::<i64>()).sum()))
            .collect();
        let first_out = others
            .iter()
            .flat_map(|&(k, _)| spans(k))
            .filter(|&(a, b)| b > a.max(1) && self.sizes[a.max(1)..b].iter().any(|&s| s > 0))
            .map(|(a, _)| a.max(1))
            .min()
            .unwrap_or(n);
        let on = spans(g.active()).map(|(a, b)| self.sizes[a.max(first_out).min(b)..b].iter().sum::<i64>()).sum();
        (others, on)
    }

    /// Whether the other topics should leave the view from this request on: an economic decision, priced
    /// like the rent. They leave if they hold `Params.apart` tokens together (`apart <= 0`: always) and the
    /// gain is positive: the cache reads they would cost to the horizon (`ratio` of their tokens each),
    /// less the rewrite that cuts them (the anchor, the cards and the kept messages after the first change,
    /// read again without the cache, at the `margin`), less the chance each is needed again times the recall
    /// that brings it back.
    pub(super) fn split_now(&mut self) -> bool {
        if self.p.apart <= 0 || self.goal.turns.is_empty() {
            return true;
        }
        let (others, on) = self.topic_tokens();
        let x: i64 = others.iter().map(|o| o.1).sum();
        if x < self.p.apart {
            return false;
        }
        let r = self.p.ratio.clamp(0.0, 1.0);
        // the view as the whole conversation would send it
        let held = std::mem::replace(&mut self.split, false);
        let v = self.sizes_now().iter().sum::<i64>() as f64;
        self.split = held;
        let k = self.tr.steps.len();
        // how long a topic lasts: the mean steps of the ones already closed, else the share of the session so far
        let hz = match self.topic_steps() {
            Some(m) if self.p.ahead > 0.0 => m.max(4.0).min(self.room(k, v as i64)),
            _ => self.horizon(k, v as i64),
        };
        let rewrite = {
            let cards = self.cards_body();
            let anchor = crate::json::est_str(&self.map_text) + crate::json::est_str(&cards);
            // (a view that is split already is read again at a change of topic either way: the other choice is the whole one)
            if held {
                (anchor + on) as f64 - v
            } else {
                (anchor + on) as f64
            }
        };
        // the first message goes too when its topic is not the one the person is on: a card instead (F2)
        let card0 =
            if self.m0_card(true) { (self.sizes[0] - crate::json::est_str(&self.cards_line(0))).max(0) } else { 0 };
        let mut gain = r * hz * (x + card0) as f64 - self.p.margin * (1.0 - r) * rewrite;
        // the topic the person was on a moment ago may be asked for again: with its pocket gone from the cache
        // (or none kept) coming back reads it without the cache, and the whole view would have kept it
        if let Some(&(back, xb)) = others.first() {
            let (switches, returns) = self.goal.switches();
            let p_back = (returns as f64 + 1.0) / (switches as f64 + 2.0);
            // and the return has to come while the horizon lasts: one return per so many steps so far
            let within = (hz * (returns as f64 + 1.0) / k.max(1) as f64).min(1.0);
            if !self.pocket_lives(back, hz) {
                gain -= within * p_back * (1.0 - r) * xb as f64;
            }
        }
        if gain <= 0.0 {
            return false;
        }
        // what each leaving topic would cost to need: the most likely of its steps, or the classifier's own
        // doubt that the latest request is not of that topic
        self.need_context();
        let n = self.tr.msgs.len();
        // the steps of each topic, by the message they stand at (the turns of a topic hold the messages of its spans)
        let mut by_topic: Vec<Vec<usize>> = vec![Vec::new(); self.goal.members.len()];
        for (i, s) in self.steps.iter().enumerate() {
            if s.place() < n {
                by_topic[self.goal.thread_at(s.place())].push(i);
            }
        }
        for (k, xk) in others {
            let mut need = self.goal.joint.get(k).copied().unwrap_or(0.0);
            let cost = (1.0 - r) * xk as f64 + r * v;
            for &i in &by_topic[k] {
                need = need.max(self.step_need(i, hz));
                // the need only grows: once it takes the whole gain, the rest of the steps change nothing
                if cost >= 0.0 && gain - need * cost <= 0.0 {
                    return false;
                }
            }
            gain -= need * cost;
            if gain <= 0.0 {
                return false;
            }
        }
        true
    }

    /// The mean length in steps of the topics already closed (every topic but the active one, `goal.turns`).
    fn topic_steps(&self) -> Option<f64> {
        let g = &self.goal;
        let nt = g.turns.len();
        let a = g.active();
        let mut count = vec![0usize; g.members.len()];
        for &mi in &self.tr.steps {
            let j = g.turns.partition_point(|t| t.start <= mi).saturating_sub(1);
            if let Some(t) = g.turns.get(j) {
                count[t.thread] += 1;
            }
        }
        let closed: Vec<usize> = (0..count.len()).filter(|&k| k != a).collect();
        (nt >= 2 && !closed.is_empty())
            .then(|| closed.iter().map(|&k| count[k]).sum::<usize>() as f64 / closed.len() as f64)
    }

    /// Whether the first message leaves the view as a line of the cards, when the other topics are `apart`:
    /// its topic is not the active one, its card is shown, and the host offers `recall` to bring it back. With
    /// a map (a cut view) the first message is the anchor and stays.
    pub(super) fn m0_card(&self, apart: bool) -> bool {
        let g = &self.goal;
        if !apart || !self.recall_tool || !self.map_text.is_empty() || g.turns.is_empty() {
            return false;
        }
        let k0 = g.thread_at(0);
        let others = g.others();
        k0 != g.active() && others[..head_slice(others.len(), self.p.cards)].contains(&k0)
    }

    /// The line a card of the cards gives topic `k` (for the size of the first message's).
    fn cards_line(&self, k: usize) -> String {
        let n = self.tr.msgs.len();
        let g = &self.goal;
        let mut fixes = |j: usize| {
            let (a, b) = g.span(j, n);
            self.fixes(a, b, Some(n))
        };
        g.card(&self.tr, &self.fz, k, &mut fixes)
    }

    pub(super) fn cards_text(&self) -> String {
        if self.apart() {
            self.cards_body()
        } else {
            String::new()
        }
    }

    /// The cards of the other topics, as they would be sent were they to leave.
    fn cards_body(&self) -> String {
        let g = &self.goal;
        let others = g.others();
        if others.is_empty() {
            return String::new();
        }
        let n = self.tr.msgs.len();
        let mut fixes = |j: usize| {
            let (a, b) = g.span(j, n);
            self.fixes(a, b, Some(n))
        };
        let mut lines = vec![format!(
            "{MARK} This conversation has other topics. They are left out so that this request gets the context of its own goal; anything in them can be asked for or looked up again:"
        )];
        let shown = head_slice(others.len(), self.p.cards);
        for &k in &others[..shown] {
            let mut card = g.card(&self.tr, &self.fz, k, &mut fixes);
            if self.recall_tool && !card.is_empty() {
                // where the topic's messages are, so `recall` brings any of them back as they were
                let mut runs: Vec<(usize, usize)> = Vec::new();
                for &j in &g.members[k] {
                    let (a, b) = g.span(j, n);
                    match runs.last_mut() {
                        Some(r) if r.1 == a => r.1 = b,
                        _ => runs.push((a, b)),
                    }
                }
                let named: Vec<String> = runs
                    .iter()
                    .filter(|r| r.1 > r.0)
                    .map(
                        |&(a, b)| {
                            if b - a == 1 {
                                format!("m{:05}", a + 1)
                            } else {
                                format!("m{:05}-m{:05}", a + 1, b)
                            }
                        },
                    )
                    .collect();
                if !named.is_empty() {
                    card += &format!(" [recall brings any of {} back as it was]", named.join(", "));
                }
            }
            lines.push(card);
        }
        if shown < others.len() {
            lines.push(format!("• … and {} older topics.", others.len() - shown));
        }
        lines.join("\n")
    }
}
