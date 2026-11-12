//! The anchor the model reads first: rules, notes, pinned files, ledger and map.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::hindsight::{Sym, UKind};
use crate::json::est_str;
use crate::ledger::{prior, select, PER_MESSAGE};
use crate::mechanical::MARK;
use crate::textops::{self, bisect_left};

use super::Engine;

/// Characters of the request in progress the anchor keeps word for word once it left the view (a longer one keeps
/// its head and its tail).
const IN_PROGRESS_CHARS: usize = 6_000;
/// What the loop sends in the person's place to go on with a goal or after a check-in: not a request of the person.
const LOOP_MARKS: [&str; 2] = ["[goal: continue]", "[supervisor: continue]"];

fn thousands(n: i64) -> String {
    let s = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (k, c) in s.chars().enumerate() {
        if k > 0 && (s.len() - k).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

impl Engine {
    /// The rules block: every rule whose message the view does not take (left, cut out by a topic
    /// switch, or in a history since rewritten).
    pub(super) fn rules_block(&mut self) -> String {
        if self.p.rules == 0 || self.rules.list.is_empty() {
            return String::new();
        }
        let vis = self.visible();
        let seen: FxHashSet<usize> = vis.iter().copied().collect();
        self.rules.text(|r| !r.msg.is_some_and(|m| seen.contains(&m)))
    }

    /// The anchor: what `leave` worked out, and the rules block after it.
    pub(super) fn compose(&mut self) {
        self.map_text = match (self.base_map.is_empty(), self.rules_shown.is_empty()) {
            (_, true) => self.base_map.clone(),
            (true, false) => self.rules_shown.clone(),
            (false, false) => format!("{}\n\n{}", self.base_map, self.rules_shown),
        };
        self.map_ver += 1;
    }

    /// Puts the rules block in the anchor when the rules it lists changed, and says whether it did. The
    /// anchor is the first message: a change to it makes the provider read the whole view again. So unless
    /// the view changes near its start anyway (`rewriting`), the block changes only when it lacks a rule
    /// the view no longer holds; a rule it lists while the view holds it again waits.
    pub(super) fn refresh_rules(&mut self, rewriting: bool) -> bool {
        let block = self.rules_block();
        if block == self.rules_shown {
            return false;
        }
        let shown: FxHashSet<&str> = self.rules_shown.lines().collect();
        if !rewriting && block.lines().skip(1).all(|l| shown.contains(l)) {
            return false;
        }
        self.rules_shown = block;
        self.compose();
        true
    }

    /// Tokens the rules take past their soft budget: the findings, then the pinned files, give them up.
    pub(super) fn rules_excess(&self) -> i64 {
        if self.p.rules == 0 || self.rules.list.is_empty() {
            return 0;
        }
        (est_str(&self.rules.text(|_| true)) - self.p.rules).max(0)
    }

    pub(super) fn ledger_text(&mut self, budget: i64) -> String {
        let t = self.tr.msgs.len();
        let mut names = self.names.take().unwrap_or_default();
        names.extend(&self.tr, t);
        let mut covered: FxHashSet<Sym> = FxHashSet::default();
        for &mi in self.visible().iter() {
            for &i in self.fz.by_msg(mi) {
                if self.tr.units[i].kind != UKind::Thinking {
                    covered.extend(self.tr.units[i].words.iter().copied());
                }
            }
        }
        let mut p: FxHashMap<Sym, f64> = FxHashMap::default();
        for (_, _, ws, _) in &self.pool {
            for &w in ws {
                if !p.contains_key(&w) && !covered.contains(&w) && names.stats.contains_key(&w) {
                    p.insert(w, prior(&names.features(w, t, &self.int)));
                }
            }
        }
        self.names = Some(names);
        let head = if self.recall_tool {
            format!("{MARK} Facts from the messages that moved out, the ones most likely to be needed again (recall m##### brings a whole message back as it was):")
        } else {
            format!("{MARK} Facts from the messages that moved out, the ones most likely to be needed again:")
        };
        // the header is part of the text: the lines take what it leaves of the budget
        let budget = budget - est_str(&head) - 1;
        let taken =
            if budget > 0 { select(&self.pool, &p, &covered, budget, self.p.least, PER_MESSAGE) } else { Vec::new() };
        self.bump("ledger_lines", taken.len() as i64);
        if taken.is_empty() {
            return String::new();
        }
        let mut lines = vec![head];
        lines.extend(taken.iter().map(|(mi, ln)| format!("m{:05} {}", mi + 1, ln)));
        lines.join("\n")
    }

    /// The person's latest request (its message and its words) once it left the view: in a long run of steps it
    /// goes out like any other message, and the model then saw the first request whole and the latest as one
    /// line of the list, and took the first for the request in progress. The loop's own continuations are not one.
    fn in_progress(&self) -> Option<(usize, String)> {
        let r = self.fz.requests.iter().rev().copied().find(|&r| {
            let t = self.unit_text_at(r, UKind::User);
            let w = crate::msg::person_words(&t).trim_start();
            !LOOP_MARKS.iter().any(|m| w.starts_with(m))
        })?;
        if r == 0 || r >= self.start {
            return None;
        }
        let t = self.unit_text_at(r, UKind::User);
        let w = textops::strip(crate::msg::person_words(&t));
        if w.is_empty() {
            return None;
        }
        let w = if textops::len(w) > IN_PROGRESS_CHARS {
            format!(
                "{} [… middle left out …] {}",
                textops::head(w, IN_PROGRESS_CHARS / 2),
                textops::tail(w, IN_PROGRESS_CHARS / 2)
            )
        } else {
            w.to_string()
        };
        Some((r, w))
    }

    /// The map text, with the findings and the pinned files after it, and the tokens of those.
    pub(super) fn map(&self) -> (String, i64) {
        let mut lines = vec![format!(
            "{MARK} Messages 2 to {} of this conversation (about {} tokens) were moved out of the context to keep it small. They do not come back by themselves: what is kept of them is below; to see more of a file or an output again, read the file or run the command again.",
            self.start,
            thousands(self.gone_tokens)
        )];
        let reqs = &self.fz.requests;
        let n = self.tr.msgs.len();
        let end_of = |x: usize| reqs.get(x + 1).copied().unwrap_or(n);
        let upto = bisect_left(reqs, &self.start);
        let current = self.in_progress();
        let requests: Vec<usize> = (0..upto)
            .filter(|&x| {
                reqs[x] > 0 || !self.outcome(0, end_of(x)).is_empty() || !self.fixes(0, end_of(x), None).is_empty()
            })
            .collect();
        if !requests.is_empty() {
            lines.push("Requests the user made in them, each with the agent's closing reply (→):".into());
            let shown: Vec<Option<usize>> = if requests.len() <= 12 {
                requests.iter().map(|&x| Some(x)).collect()
            } else {
                let mut v: Vec<Option<usize>> = requests[..2].iter().map(|&x| Some(x)).collect();
                v.push(None);
                v.extend(requests[requests.len() - 10..].iter().map(|&x| Some(x)));
                v
            };
            for x in shown {
                let Some(x) = x else {
                    lines.push(format!("- … {} more …", requests.len() - 12));
                    continue;
                };
                let r = reqs[x];
                let ask = if r == 0 {
                    "(the first request, above)".to_string()
                } else if current.as_ref().is_some_and(|c| c.0 == r) {
                    "(the latest request, in progress: word for word below)".to_string()
                } else {
                    let s = textops::squash_ws(&self.unit_text_at(r, UKind::User));
                    if textops::len(&s) > 300 {
                        format!("{}…", textops::head(&s, 300))
                    } else {
                        s
                    }
                };
                let out = self.outcome(r, end_of(x));
                let mut l = format!("- {ask}");
                if !out.is_empty() {
                    l += &format!("\n  → {out}");
                }
                l += &self.fixes(r, end_of(x), None);
                lines.push(l);
            }
        }
        if !self.gone.is_empty() {
            let mut common: Vec<(&String, &i64)> = self.gone.iter().collect();
            common.sort_by(|a, b| b.1.cmp(a.1));
            let files: Vec<String> = common
                .iter()
                .take(20)
                .map(|(p, n)| if **n > 1 { format!("{p} ×{n}") } else { p.to_string() })
                .collect();
            lines.push(format!("Files changed in them: {}.", files.join(", ")));
        }
        if self.gone_commands != 0 {
            lines.push(format!("Commands run in them: {}.", self.gone_commands));
        }
        let mut kept = 0;
        if let Some((_, words)) = &current {
            let block = format!(
                "The user's latest request, the one the work is on, moved out with them; here it is word for word:\n{words}"
            );
            kept += est_str(&block);
            lines.push(block);
        }
        let mut text = lines.join("\n");
        for block in [self.notes_text(), self.pins_text(), self.wrote_text(self.start)] {
            if !block.is_empty() {
                text += "\n\n";
                text += &block;
                kept += est_str(&block);
            }
        }
        (text, kept)
    }
}
