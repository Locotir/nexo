//! What survives a compaction word for word: the person's messages and the files the tools touched.

use super::transcript::shorten;
use super::*;
use nexo_core::message::{estimate_tokens, Message, Part, Role};

/// Where the kept tail starts when the history is compacted in the middle of the work: at the
/// last message that can open a tail (the person's last message when nothing followed it, else the
/// model's last reply, so its tool calls stay with their results). None when that leaves nothing to
/// summarise.
pub fn keep_recent(history: &[Message]) -> Option<usize> {
    let opens = |m: &Message| m.role == Role::Assistant || (m.role == Role::User && !m.is_tool_results());
    let i = history.iter().rposition(opens)?;
    (i > 0).then_some(i)
}

/// The person's messages and the files the tools touched in `msgs`, with what an earlier summary
/// in them carried over.
#[derive(Debug, Default, PartialEq)]
pub struct Kept {
    pub requests: Vec<String>,
    pub changed: Vec<String>,
    pub read: Vec<String>,
}

pub(super) fn push_new(v: &mut Vec<String>, s: &str) {
    if !s.is_empty() && !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

impl Kept {
    /// Keeps the person's messages within `tokens` (4 characters a token): the first (the task
    /// statement) up to [`FIRST_REQUEST_CHARS`] or half of them, each other up to [`REQUEST_CHARS`],
    /// and past that the oldest after the first are left out.
    pub fn cap(&mut self, tokens: u64) {
        let budget = usize::try_from(tokens.saturating_mul(4)).unwrap_or(usize::MAX);
        let Some((first, rest)) = self.requests.split_first_mut() else { return };
        *first = shorten(first, FIRST_REQUEST_CHARS.min(budget / 2));
        for r in rest.iter_mut() {
            *r = shorten(r, REQUEST_CHARS);
        }
        let size = |v: &[String]| v.iter().map(|r| r.chars().count()).sum::<usize>();
        while self.requests.len() > 1 && size(&self.requests) > budget {
            self.requests.remove(1);
        }
    }
}

/// The note of ended background jobs the loop puts before a request (`nexo_core::jobs::ended_note`).
pub(super) const JOBS_NOTE: &str = "[background jobs ended]";
pub(super) const JOBS_NOTE_END: &str = "Read the output with the `jobs` tool";
/// Memory shared from another session, put before a request.
pub(super) const MEMORY_START: &str = "<memory from=\"";
pub(super) const MEMORY_END: &str = "</memory>";

/// What the person wrote in `text`, without what the harness adds to a request: notes of ended
/// background jobs, memory shared by other sessions and the line that says a goal is not running (before it), the
/// files attached to it (after it; the paths of those attached whole go to `attached`), and the loop's own
/// continuation of a goal. None when nothing of the person is left.
pub(super) fn person_text(text: &str, attached: &mut Vec<String>) -> Option<String> {
    if text == crate::turn::GOAL_CONTINUE {
        return None;
    }
    // what nexo added went before the words, under the mark: the words are what follows it
    let words = nexo_core::agent::person_words(text);
    if words.len() < text.len() {
        for line in text[..text.len() - words.len()].lines() {
            if let Some(path) = crate::reads::headed_file(line) {
                push_new(attached, path);
            }
        }
        let t = words.trim();
        return (!t.is_empty()).then(|| t.to_string());
    }
    let mut t = text.trim_start();
    loop {
        if let Some(line) = nexo_core::agent::goal_state_line_of(t) {
            // nexo's line in front of a request it started: not the person's
            t = t[line.len()..].trim_start();
        } else if t.starts_with(JOBS_NOTE) {
            let i = t.find(JOBS_NOTE_END)?;
            t = t[i..].split_once('\n').map_or("", |(_, after)| after).trim_start();
        } else if t.starts_with(MEMORY_START) {
            let i = t.find(MEMORY_END)?;
            t = t[i + MEMORY_END.len()..].trim_start();
        } else {
            break;
        }
    }
    // the files come after the text, each from the line that heads it: `@path (attached ...):` for one the person
    // attached, `path (project file, ...):` for one nexo included
    let mut end = t.len();
    for (i, _) in t.match_indices("\n\n") {
        let line = t[i + 2..].lines().next().unwrap_or("");
        match crate::reads::headed_file(line) {
            Some(path) => {
                end = end.min(i);
                push_new(attached, path);
            }
            None if line.starts_with('@')
                && (line.contains(": attached as an image") || line.contains(": not attached")) =>
            {
                end = end.min(i)
            }
            None => {}
        }
    }
    let t = t[..end].trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Whether `text` is one of the parts a summary is made of.
pub(super) fn summary_part(text: &str) -> bool {
    text.starts_with("[summary]") || text.starts_with(REQUEST) || text.starts_with(FILES)
}

/// The tokens of `msgs` a compaction would replace, less the parts of an earlier summary in them
/// (carried over as they are): what a new compaction gains over the last one.
pub fn fresh_tokens(msgs: &[Message]) -> u64 {
    let carried: u64 = msgs
        .iter()
        .filter(|m| m.role == Role::User)
        .flat_map(|m| &m.content)
        .map(|p| match p {
            Part::Text { text } if summary_part(text) => text.len() as u64 / 4,
            _ => 0,
        })
        .sum();
    estimate_tokens(msgs).saturating_sub(carried)
}

/// What a compaction keeps word for word from `msgs`. A file counts as changed or read only when the
/// call's result came back without an error.
pub fn kept(msgs: &[Message]) -> Kept {
    let mut k = Kept::default();
    let mut calls: Vec<(&str, &str, String)> = Vec::new();
    let mut failed: std::collections::HashMap<&str, bool> = std::collections::HashMap::new();
    let mut attached = Vec::new();
    for m in msgs {
        // a message with tool results holds no words of the person: its text is the loop's
        let person = m.role == Role::User && !m.content.iter().any(Part::is_tool_result);
        for p in &m.content {
            match p {
                Part::Text { text } if person => {
                    if let Some(r) = text.strip_prefix(REQUEST) {
                        k.requests.push(r.to_string());
                    } else if let Some(files) = text.strip_prefix(FILES) {
                        let mut into_changed = true;
                        for line in files.lines() {
                            match line.trim() {
                                "changed:" => into_changed = true,
                                "read:" => into_changed = false,
                                "" => {}
                                f if into_changed => push_new(&mut k.changed, f),
                                f => push_new(&mut k.read, f),
                            }
                        }
                    } else if !text.starts_with("[summary]") {
                        if let Some(r) = person_text(text, &mut attached) {
                            k.requests.push(r);
                        }
                    }
                }
                Part::ToolCall { id, name, args } => {
                    let single = ["path", "file_path", "file"].iter().find_map(|key| args.get(*key)?.as_str());
                    // a read of several files read each of them
                    let named = nexo_core::tool::call_paths(args);
                    for path in single.map(str::to_string).into_iter().chain(named) {
                        if !calls.iter().any(|(i, _, p)| *i == id && *p == path) {
                            calls.push((id, name, path));
                        }
                    }
                }
                Part::ToolResult { call_id, is_error, .. } => {
                    failed.insert(call_id, *is_error);
                }
                _ => {}
            }
        }
    }
    for (id, name, path) in calls {
        // a call that failed, or whose result is not here (it did not run), touched nothing
        if failed.get(id) != Some(&false) {
            continue;
        }
        let n = name.to_ascii_lowercase();
        if n.contains("write") || n.contains("edit") || n.contains("patch") {
            push_new(&mut k.changed, &path);
        } else if n.contains("read") {
            push_new(&mut k.read, &path);
        }
    }
    for f in attached {
        push_new(&mut k.read, &f);
    }
    k.read.retain(|f| !k.changed.contains(f));
    k
}

/// The parts that stand for the summarised messages.
pub fn summary_parts(k: &Kept, summary: &str) -> Vec<Part> {
    let mut parts = vec![Part::text(HEADER)];
    parts.extend(k.requests.iter().map(|r| Part::text(format!("{REQUEST}{r}"))));
    if !k.changed.is_empty() || !k.read.is_empty() {
        let mut f = String::from(FILES);
        for (head, list) in [("changed:", &k.changed), ("read:", &k.read)] {
            if !list.is_empty() {
                f.push_str(&format!("\n{head}"));
                for p in list {
                    f.push_str(&format!("\n  {p}"));
                }
            }
        }
        parts.push(Part::text(f));
    }
    parts.push(Part::text(format!("{WORK}\n\n{}", summary.trim())));
    parts
}
