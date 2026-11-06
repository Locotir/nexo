//! G0: who removed what. Every removal the engine makes (a part of an output, a step, a run of
//! messages, the shortening at birth) is tagged with the rule that made it and the request it was
//! made at, so a replay can name the rule behind each word the view lost, and what each rule saved.
//! Measurement only: nothing here changes what the view holds.

use rustc_hash::FxHashMap;

use super::Engine;
use crate::hindsight::UKind;

#[derive(Default)]
pub struct Own {
    /// Requests decided so far.
    pub clock: u64,
    /// (unit, chunk) -> (request, rule) of the first rule that took it out.
    chunks: FxHashMap<(usize, usize), (u64, &'static str)>,
    /// message -> (request, rule) for a whole message taken out (closed steps, loops, the window).
    msgs: FxHashMap<usize, (u64, &'static str)>,
    /// (rule, request, tokens): +tokens when something left, -tokens when it came back.
    events: Vec<(&'static str, u64, i64)>,
    /// Messages the shortening at birth changed.
    pub born: rustc_hash::FxHashSet<usize>,
}

impl Engine {
    /// Takes the chunks `js` of unit `i` out of the view, for `rule`.
    pub(crate) fn drop_as(&mut self, i: usize, js: impl IntoIterator<Item = usize>, rule: &'static str) {
        for j in js {
            self.drop_one(i, j, rule);
        }
    }

    pub(crate) fn drop_one(&mut self, i: usize, j: usize, rule: &'static str) {
        let fresh = !self.dropped.get(&i).is_some_and(|d| d.contains(&j));
        if fresh && !self.own.chunks.contains_key(&(i, j)) {
            let tokens = self.fz.chunks[i].get(j).map_or(0, |c| c.tokens);
            self.own.chunks.insert((i, j), (self.own.clock, rule));
            self.own.events.push((rule, self.own.clock, tokens));
        }
        self.dropped.entry(i).or_default().insert(j);
        // a read out of date as it leaves says so in its note, whichever rule took it (`Params.stale`)
        if fresh && self.p.stale != 0 && !self.stale_gone.contains_key(&i) {
            if let Some(x) = self.superseded_by(i) {
                self.stale_gone.insert(i, x);
            }
        }
    }

    /// Messages that go out (`on`) or come back (`!on`) as a whole, for `rule`.
    pub(crate) fn tag_msgs(&mut self, msgs: impl IntoIterator<Item = usize>, on: bool, rule: &'static str) {
        for mi in msgs {
            let tokens = self.sizes.get(mi).copied().unwrap_or(0);
            if on {
                if self.own.msgs.insert(mi, (self.own.clock, rule)).is_none() {
                    self.own.events.push((rule, self.own.clock, tokens));
                }
            } else if let Some((_, r)) = self.own.msgs.remove(&mi) {
                self.own.events.push((r, self.own.clock, -tokens));
            }
        }
    }

    pub(crate) fn tag_event(&mut self, rule: &'static str, tokens: i64) {
        self.own.events.push((rule, self.own.clock, tokens));
    }

    /// Token-requests each rule saved: tokens taken out times the requests left after it.
    pub fn savings(&self) -> Vec<(&'static str, i64)> {
        let end = self.own.clock;
        let mut out: FxHashMap<&'static str, i64> = FxHashMap::default();
        for &(r, c, t) in &self.own.events {
            *out.entry(r).or_default() += t * end.saturating_sub(c) as i64;
        }
        let mut v: Vec<_> = out.into_iter().collect();
        v.sort();
        v
    }

    /// Why message `mi`'s copy of `word` is not in the view: the rule and the request that took out the
    /// last place it was held, `None` if some copy of it is still sent (or the message holds none).
    /// `Some((0, "birth"))`: the text the engine kept of the message does not hold it.
    pub fn fate(&self, mi: usize, word: &str) -> Option<(u64, &'static str)> {
        if mi >= self.tr.msgs.len() {
            return None;
        }
        let mut held = false;
        let mut kept = false;
        let mut at: Option<(u64, &'static str)> = None;
        let bump = |f: (u64, &'static str), at: &mut Option<(u64, &'static str)>| {
            *at = Some(at.map_or(f, |a| a.max(f)));
        };
        // the whole message out of the view
        let msg_out = if mi < self.start && mi != 0 {
            Some(self.own.msgs.get(&mi).copied().unwrap_or((self.own.clock, "leave")))
        } else if self.shut_out.contains(&mi) {
            Some(self.own.msgs.get(&mi).copied().unwrap_or((self.own.clock, "apart")))
        } else {
            None
        };
        'units: for &i in self.fz.by_msg(mi) {
            let u = &self.tr.units[i];
            let text = self.tr.unit_text(i);
            if !text.contains(word) {
                continue;
            }
            held = true;
            if let Some(m) = msg_out {
                bump(m, &mut at);
                continue;
            }
            let Some(d) = self.dropped.get(&i) else {
                kept = true;
                continue;
            };
            let chunks = &self.fz.chunks[i];
            if u.kind == UKind::Call {
                // only the long strings of a call's input go
                let big = self.tr.block(i).get("input").is_some_and(|inp| {
                    inp.as_object().is_some_and(|o| {
                        o.values().any(|v| v.as_str().is_some_and(|s| s.len() > 200 && s.contains(word)))
                    })
                });
                if !big {
                    kept = true;
                    continue;
                }
                let f = d.iter().filter_map(|&j| self.own.chunks.get(&(i, j)).copied()).max();
                bump(f.unwrap_or((0, "call")), &mut at);
                continue;
            }
            if u.kind == UKind::Thinking {
                bump((0, "thinking"), &mut at);
                continue;
            }
            // the chunks whose lines hold the word
            let lines: Vec<&str> = text.split('\n').collect();
            let n = lines.len() as i64;
            let pos = |x: i64| (if x < 0 { (n + x).max(0) } else { x.min(n) }) as usize;
            let mut worst: Option<(u64, &'static str)> = None;
            for (j, c) in chunks.iter().enumerate() {
                let (a, b) = (pos(c.start), pos(c.end));
                if !lines[a.min(b)..b].iter().any(|l| l.contains(word)) {
                    continue;
                }
                if !d.contains(&j) {
                    kept = true;
                    continue 'units;
                }
                let f = self.own.chunks.get(&(i, j)).copied().unwrap_or((0, "untagged"));
                worst = Some(worst.map_or(f, |w| w.max(f)));
            }
            bump(worst.unwrap_or((0, "untagged")), &mut at);
        }
        if !held {
            return Some((0, if self.own.born.contains(&mi) { "birth" } else { "nohold" }));
        }
        at.or(kept.then_some((0, "kept")))
    }
}
