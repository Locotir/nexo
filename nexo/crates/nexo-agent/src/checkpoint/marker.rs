//! What counts as progress, the todo list, and the model's decision line.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::DETAIL;
use nexo_core::message::{Message, Part};
use serde_json::Value;

/// What counts as progress between two checkpoints.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mark {
    /// File writes recorded so far (`State.checkpoints`): where the lists of files start.
    pub writes: usize,
    /// Writes that succeeded and left content the request had not written there before.
    pub wrote: usize,
    /// Output lines the session had not seen before, counted over the request.
    pub fresh: usize,
    /// A hash of the todo list (0: none).
    pub todo: u64,
}

/// True when something was written, something new came in or the todo list changed since `prev`.
pub fn progress(now: &Mark, prev: &Mark) -> bool {
    now.wrote != prev.wrote || now.fresh > prev.fresh || now.todo != prev.todo
}

/// The todo list of a request as its last `todo` call left it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Todo {
    pub done: usize,
    pub doing: usize,
    pub pending: usize,
    pub doing_text: Option<String>,
    pub hash: u64,
}

/// The last `todo` call in `history`, if it listed items.
pub fn todo_of(history: &[Message]) -> Option<Todo> {
    let items = history.iter().rev().flat_map(|m| m.content.iter().rev()).find_map(|p| match p {
        Part::ToolCall { name, args, .. } if name == "todo" => args.get("items").and_then(Value::as_array),
        _ => None,
    })?;
    let mut t = Todo::default();
    let mut h = DefaultHasher::new();
    for i in items {
        let status = i.get("status").and_then(Value::as_str).unwrap_or("pending").trim().to_ascii_lowercase();
        let text = i.get("text").and_then(Value::as_str).unwrap_or("").trim().to_string();
        (&status, &text).hash(&mut h);
        match status.as_str() {
            "done" | "completed" | "complete" => t.done += 1,
            "in_progress" | "in-progress" | "doing" | "active" => {
                t.doing += 1;
                t.doing_text.get_or_insert(text);
            }
            _ => t.pending += 1,
        }
    }
    t.hash = h.finish();
    Some(t)
}

/// Tool calls made in `history` (a slice from the start of the request).
pub fn calls_in(history: &[Message]) -> usize {
    history.iter().map(|m| m.content.iter().filter(|p| p.is_tool_call()).count()).sum()
}

/// What the model decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub go_on: bool,
    pub reason: String,
    /// What follows the decision line: for a stop, the summary for the person.
    pub rest: String,
}

/// Words after `CHECKPOINT` that say to go on, and to stop (the marker is asked for in English, but a reply in
/// the person's language is read too: a stop that is not understood would run on all night).
pub(super) const GO_ON: [&str; 6] = ["continue", "continuar", "continua", "seguir", "go on", "proceed"];
pub(super) const STOP: [&str; 6] = ["stop", "parar", "detener", "alto", "halt", "para"];

/// The first line within the first eight that begins with `CHECKPOINT` and a word for continuing or stopping
/// (case, numbering and markdown marks ignored).
pub fn parse_decision(text: &str) -> Option<Decision> {
    let lines: Vec<&str> = text.lines().collect();
    for (n, line) in lines.iter().take(8).enumerate() {
        let mut l = line.trim().trim_start_matches(['*', '_', '`', '#', '>', '-', '[', '(', ' ']);
        let digits = l.trim_start_matches(|c: char| c.is_ascii_digit());
        if digits.len() < l.len() && digits.starts_with(['.', ')']) {
            l = digits[1..].trim_start_matches(['*', '_', '`', '[', '(', ' ']);
        }
        let lower = l.to_ascii_lowercase();
        let Some(rest) = lower.strip_prefix("checkpoint") else { continue };
        let word = rest.trim_start_matches([':', '-', '*', '_', '`', ' ', ']', ')']);
        fn word_of<'a>(word: &str, set: &[&'a str]) -> Option<&'a str> {
            set.iter().copied().find(|w| word.starts_with(*w) && !word[w.len()..].starts_with(char::is_alphabetic))
        }
        let (go_on, w) = if let Some(w) = word_of(word, &GO_ON) {
            (true, w)
        } else if let Some(w) = word_of(word, &STOP) {
            (false, w)
        } else {
            continue;
        };
        // the lower-case copy has the length of the line: the reason keeps its case
        let at = lower.len() - word.len() + w.len();
        let after = l[at..].trim_start_matches([']', ')', ':', '-', '*', '_', '`', ' ', '.']).trim();
        let low = after.to_ascii_lowercase();
        let reason = match low.find("next:").or_else(|| low.find("siguiente:")) {
            Some(i) => after[..i].trim().trim_end_matches(['.', ';']).to_string(),
            None => after.to_string(),
        };
        let reason: String = reason.chars().take(DETAIL).collect();
        return Some(Decision { go_on, reason, rest: lines[n + 1..].join("\n").trim().to_string() });
    }
    None
}

/// Holds back the start of a checkpoint reply until its first line is known, so the decision line is not shown
/// to the person as reply text. The model's message itself is kept as sent.
#[derive(Debug)]
pub struct MarkerFilter {
    held: String,
    open: bool,
}

impl MarkerFilter {
    pub fn start() -> Self {
        MarkerFilter { held: String::new(), open: true }
    }

    /// Whether `s`, the start of a line, may still turn out to begin with the marker.
    fn may_start(s: &str) -> bool {
        let t = s.trim().trim_start_matches(['*', '_', '`', '#', '>', '-', '[', '(', ' ']);
        let t = t.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches(['.', ')']);
        let t = t.trim_start_matches(['*', '_', '`', '[', '(', ' ']).to_ascii_lowercase();
        t.is_empty() || "checkpoint".starts_with(&t) || t.starts_with("checkpoint")
    }

    /// The text of the next delta that may be shown.
    pub fn push(&mut self, t: &str) -> String {
        if !self.open {
            return t.to_string();
        }
        self.held.push_str(t);
        if let Some(i) = self.held.find('\n') {
            self.open = false;
            let held = std::mem::take(&mut self.held);
            return if parse_decision(&held[..i]).is_some() { held[i + 1..].to_string() } else { held };
        }
        if Self::may_start(&self.held) {
            return String::new();
        }
        self.open = false;
        std::mem::take(&mut self.held)
    }

    /// What is still held when the stream ends.
    pub fn finish(&mut self) -> String {
        self.open = false;
        let held = std::mem::take(&mut self.held);
        if parse_decision(&held).is_some() {
            String::new()
        } else {
            held
        }
    }
}
