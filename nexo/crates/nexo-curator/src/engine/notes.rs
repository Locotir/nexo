//! What the agent writes down (findings, plan, rules) and the nudge that asks for it.

use std::sync::LazyLock;

use serde_json::Value;

use crate::hindsight::UKind;
use crate::json::est_str;
use crate::mechanical::MARK;
use crate::msg::{blocks, get_str};
use crate::textops::{self, Re};
use crate::tools::{partial_read, Kind};

use super::files::{basename, file_key, same_file, scratch};
use super::{Engine, NOTE_TOOL, NUDGE, NUDGE_SAID};

// A line the agent marked as a finding, as its plan or as a rule it was given, in its own text; as a
// list item or bold too.
pub(super) static NOTE_LINE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r"^[ \t]*(?:[-*•][ \t]*|\d+[.)][ \t]*)?(?:\*\*)?(Finding|Plan|Rule)(?:\*\*)?:(?:\*\*)?[ \t]*(.+?)[ \t]*$",
        textops::M,
    )
});

const NOTE_CHARS: usize = 400;

/// The text of a block that may carry note lines: a text block, or the input of a `note` tool call.
pub(super) fn note_text(b: &Value) -> Option<String> {
    match b.get("type").and_then(Value::as_str) {
        Some("text") => Some(get_str(b, "text").to_string()),
        Some("tool_use") if get_str(b, "name") == NOTE_TOOL => Some(crate::msg::flat(b.get("input"))),
        _ => None,
    }
}

impl Engine {
    pub(super) fn nudge_text(&self) -> &'static str {
        if self.note_tool {
            NUDGE
        } else {
            NUDGE_SAID
        }
    }

    /// Whether the agent is between two parts of one file: its last call read a part of a file (a read
    /// with a range), so the next step is likely the next part.
    fn mid_read(&self, t: usize) -> bool {
        t >= 2
            && blocks(self.tr.msgs[t - 2].get("content")).iter().any(|b| {
                b.get("type").and_then(Value::as_str) == Some("tool_use")
                    && partial_read(get_str(b, "name"), b.get("input"))
            })
    }

    /// Asks the agent for its findings and its plan (a `note` call, or the lines in its reply), at the end
    /// of the last tool result, once it has gone `nudge` steps without a note while the view loses what it
    /// read (it was rewritten, or the history is past the protected tail). At most once per `nudge` steps;
    /// only the latest request stays in the view, and it leaves once a note answers it.
    ///
    /// The request waits while the agent is between two parts of one file (its last call was a read with
    /// a range): in a measured run, a request between the first and the second part made the agent search
    /// the file instead of reading on, and the answers that depended on the rest were lost. The wait is
    /// bounded: with `n` the `nudge` setting, the request comes when
    /// `since_note >= n && since_nudge >= n && (!mid_read || since_note >= 2n)`.
    ///
    /// Returns the oldest earlier message it rebuilt, if any.
    pub(super) fn nudge(&mut self, t: usize) -> Option<usize> {
        let n = self.p.nudge;
        if n <= 0 || self.since_note < n as usize || self.since_nudge < n as usize {
            return None;
        }
        if self.start <= 1 && self.dropped.is_empty() && t as i64 <= self.p.protect {
            return None;
        }
        if self.since_note < 2 * n.max(1) as usize && self.mid_read(t) {
            return None;
        }
        let last = t - 1;
        let result = blocks(self.tr.msgs[last].get("content"))
            .iter()
            .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"));
        if !result || self.nudged.contains(&last) {
            return None;
        }
        // the earlier request leaves with its message rebuilt plain: one request in the view, not a pile
        let mut oldest = None;
        for old in std::mem::take(&mut self.nudged) {
            self.built.remove(&old);
            oldest = Some(oldest.map_or(old, |o: usize| o.min(old)));
        }
        self.nudged.insert(last);
        self.built.remove(&last);
        self.since_nudge = 0;
        self.bump("nudges", 1);
        oldest
    }

    /// What the anchor keeps of messages ``a`` to ``b`` (leaving the view now), in their own words: the
    /// agent's marked findings and plan (``notes``) and the files the person named, as last read
    /// (``pin``). The rules are taken as messages arrive (`take_rules`).
    pub(super) fn harvest(&mut self, a: usize, b: usize) {
        for mi in a..b {
            for i in self.fz.by_msg(mi).to_vec() {
                let kind = self.tr.units[i].kind;
                let note = kind == UKind::Text || (kind == UKind::Call && self.tr.units[i].tool == NOTE_TOOL);
                if note && self.p.notes != 0 {
                    let text = self.tr.unit_text(i);
                    for c in NOTE_LINE.captures_all(&text) {
                        let span = |g: usize| c[g].map_or("", |(x, y)| &text[x..y]);
                        let line = textops::head(&textops::squash_ws(span(2)), NOTE_CHARS).to_string();
                        if span(1) == "Rule" {
                            continue;
                        }
                        if span(1) == "Plan" {
                            self.plan = Some((mi, line));
                        } else if self.findings.iter().all(|(_, x)| *x != line) {
                            self.findings.push((mi, line));
                        }
                    }
                } else if kind == UKind::Result && self.p.pin != 0 {
                    let Some(info) = self.fz.info.get(&i) else { continue };
                    let Some(path) = info.path.as_deref() else { continue };
                    if info.kind != Kind::Read || path.is_empty() || info.error {
                        continue;
                    }
                    let base = basename(path);
                    if base.is_empty() {
                        continue;
                    }
                    let asked = |s: &str| self.int.find(s).is_some_and(|w| self.asked.contains(&w));
                    if !(asked(path) || asked(base)) {
                        continue;
                    }
                    let text = self.tr.unit_text(i);
                    let text = textops::strip(&text);
                    if !text.is_empty() {
                        // the first n characters, n = 4 * floor(pin / 2); a negative n counts from the end
                        let n = 4 * self.p.pin.div_euclid(2);
                        let n = if n >= 0 { n as usize } else { (textops::len(text) as i64 + n).max(0) as usize };
                        self.pins.insert(file_key(path), (mi, path.to_string(), textops::head(text, n).to_string()));
                    }
                }
            }
        }
        // a file written after it was read is not as it was read: the pin goes (the next read pins it again)
        let written = &self.written;
        self.pins.retain(|key, (mi, _, _)| !written.iter().any(|(k, w)| w > mi && same_file(k, key)));
        // the rules are never dropped: past their budget, the findings and then the pinned files yield
        let over = self.rules_excess();
        let (notes, pin) = (self.p.notes - over, self.p.pin - (over - self.p.notes).max(0));
        while self.findings.len() > 1 && est_str(&self.notes_text()) > notes {
            self.findings.remove(0);
        }
        // the reads and the writes share the budget, the newest first; scratch files go before the rest
        loop {
            let shown: Vec<usize> = (0..self.wrote.len()).filter(|&k| self.wrote[k].mi < b).collect();
            if self.pins.len() + shown.len() <= 1 || est_str(&self.pins_text()) + est_str(&self.wrote_text(b)) <= pin {
                break;
            }
            let oldest_write = shown.iter().copied().min_by_key(|&k| self.wrote[k].mi);
            let scratch_write = shown.iter().copied().find(|&k| scratch(self.wrote.get_index(k).map_or("", |x| x.0)));
            let oldest_read = (0..self.pins.len()).min_by_key(|&k| self.pins[k].0);
            match (scratch_write, oldest_write, oldest_read) {
                (Some(k), _, _) => {
                    self.wrote.shift_remove_index(k);
                }
                (None, Some(w), Some(r)) if self.wrote[w].mi < self.pins[r].0 => {
                    self.wrote.shift_remove_index(w);
                }
                (None, _, Some(r)) => {
                    self.pins.shift_remove_index(r);
                }
                (None, Some(w), None) => {
                    self.wrote.shift_remove_index(w);
                }
                (None, None, None) => break,
            }
        }
    }

    /// The files the agent wrote in the messages before ``upto`` (gone from the view), as last
    /// written, the newest first.
    pub(super) fn wrote_text(&self, upto: usize) -> String {
        let mut parts = vec![format!("{MARK} Files you wrote, as last written (whole):")];
        for w in self.wrote.values().rev().filter(|w| w.mi < upto) {
            let stale =
                w.edited.map_or(String::new(), |e| format!("; edited later in m{:05}: re-read before trusting", e + 1));
            parts.push(format!("--- {} (written in m{:05}{stale}) ---\n{}", w.path, w.mi + 1, w.text));
        }
        if parts.len() == 1 {
            return String::new();
        }
        parts.join("\n")
    }

    pub(super) fn notes_text(&self) -> String {
        if self.findings.is_empty() && self.plan.is_none() {
            return String::new();
        }
        let mut lines =
            vec![format!("{MARK} What you established in them, in your own words (do not redo this work):")];
        lines.extend(self.findings.iter().map(|(mi, ln)| format!("m{:05} Finding: {ln}", mi + 1)));
        if let Some((mi, ln)) = &self.plan {
            lines.push(format!("m{:05} Plan: {ln}", mi + 1));
        }
        lines.join("\n")
    }

    pub(super) fn pins_text(&self) -> String {
        if self.pins.is_empty() {
            return String::new();
        }
        let mut parts = vec![format!(
            "{MARK} Files the request names, as last read (whole; read one again only if it may have changed):"
        )];
        let mut pins: Vec<&(usize, String, String)> = self.pins.values().collect();
        pins.sort();
        for (mi, path, text) in pins {
            parts.push(format!("--- {path} (read in m{:05}) ---\n{text}", mi + 1));
        }
        parts.join("\n")
    }

    pub(super) fn unit_text_at(&self, mi: usize, kind: UKind) -> String {
        let parts: Vec<String> = self
            .fz
            .by_msg(mi)
            .iter()
            .filter(|&&i| self.tr.units[i].kind == kind)
            .map(|&i| self.tr.unit_text(i).into_owned())
            .collect();
        parts.join("\n")
    }
}
