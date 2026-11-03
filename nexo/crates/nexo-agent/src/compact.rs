//! Making the history shorter: a summary of the older part replaces it.
//!
//! What replaces it, in one message from the person: a header, every message the person wrote in the
//! summarised part word for word (`[request]` parts, carried over from an earlier summary too), the
//! files the tools changed and read (`[files]`), and the model's summary of the work. The task and
//! the files are kept by the harness, not left to the summary, so a compaction never loses them.

use std::sync::Arc;

use futures::StreamExt;
use nexo_core::message::{Message, Part, Role};
use nexo_core::provider::ModelRequest;
use nexo_core::stream::{Collector, StreamEvent, Usage};

use crate::client::{ModelClient, Target};

/// The system prompt of the summary request (a recorder can tell that request apart by it).
pub const SUMMARY_SYSTEM: &str =
    "You write the summary of a coding session so that the work can continue without the transcript. \
Keep: the goal; what was done (files changed and how, commands run and their results); what was learned (errors seen, \
decisions and why); what remains. Keep exact names: paths, functions, identifiers, values, commands. Short sections, \
plain text, no preamble.";

/// Characters of a tool result kept in the transcript given to the summariser.
const RESULT_CHARS: usize = 2_000;
/// Characters of the whole transcript given to the summariser.
const TRANSCRIPT_CHARS: usize = 160_000;
/// Characters of one message of the person kept word for word (longer ones keep their head and tail).
const REQUEST_CHARS: usize = 8_000;

const HEADER: &str = "[summary] The conversation before this point was replaced by a summary. Below: the person's \
messages in it word for word ([request]), the files touched ([files]), then what was done.";
const REQUEST: &str = "[request] ";
const FILES: &str = "[files]";
const WORK: &str = "[summary] What was done:";

pub struct Summariser {
    pub client: Arc<dyn ModelClient>,
    pub target: Target,
    pub retries: u32,
    pub max_tokens: u32,
}

/// What a compaction did: the model's summary and what its request spent.
#[derive(Debug, Clone, Default)]
pub struct Compaction {
    pub summary: String,
    pub usage: Usage,
    pub model: String,
}

impl Summariser {
    /// One model call: the summary of `msgs`, and what the call spent.
    pub async fn summarise(&self, msgs: &[Message]) -> anyhow::Result<Compaction> {
        let (_, cfg, model) = self.target.get();
        let transcript = transcript(msgs);
        let ask = format!("Transcript of the session so far:\n\n{transcript}\n\nWrite the summary.");
        let mut req = ModelRequest::new(model.clone(), SUMMARY_SYSTEM, vec![Message::user(ask)]);
        req.max_tokens = self.max_tokens;
        req.cache = false;
        let mut stream = self.client.stream(&cfg, &req, self.retries).await?;
        let mut c = Collector::new();
        while let Some(ev) = stream.next().await {
            if let StreamEvent::Error(e) = &ev {
                anyhow::bail!("summary failed: {e}");
            }
            c.push(ev);
        }
        let text = c.message().text();
        if text.trim().is_empty() {
            anyhow::bail!("the summary came back empty");
        }
        Ok(Compaction { summary: text, usage: c.usage, model })
    }
}

/// The conversation as plain text, tool results shortened.
pub fn transcript(msgs: &[Message]) -> String {
    let mut out = String::new();
    for m in msgs {
        for p in &m.content {
            match p {
                Part::Text { text } => {
                    let who = if m.role == Role::User { "person" } else { "assistant" };
                    out.push_str(&format!("[{who}] {text}\n"));
                }
                Part::ToolCall { name, args, .. } => {
                    out.push_str(&format!("[call {name}] {}\n", shorten(&args.to_string(), 400)));
                }
                Part::ToolResult { content, is_error, .. } => {
                    let t = nexo_core::message::result_text(content);
                    let tag = if *is_error { "result, error" } else { "result" };
                    out.push_str(&format!("[{tag}] {}\n", shorten(&t, RESULT_CHARS)));
                }
                _ => {}
            }
        }
    }
    if out.len() > TRANSCRIPT_CHARS {
        let head: String = out.chars().take(TRANSCRIPT_CHARS / 3).collect();
        let tail: String =
            out.chars().rev().take(TRANSCRIPT_CHARS * 2 / 3).collect::<Vec<_>>().into_iter().rev().collect();
        out = format!("{head}\n[... the middle of the transcript is left out ...]\n{tail}");
    }
    out
}

fn shorten(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let head: String = s.chars().take(n / 2).collect();
    let tail: String = s.chars().rev().take(n / 2).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head} [...] {tail}")
}

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

fn push_new(v: &mut Vec<String>, s: &str) {
    if !s.is_empty() && !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

/// What a compaction keeps word for word from `msgs`.
pub fn kept(msgs: &[Message]) -> Kept {
    let mut k = Kept::default();
    for m in msgs {
        for p in &m.content {
            match p {
                Part::Text { text } if m.role == Role::User && !m.is_tool_results() => {
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
                    } else if !text.starts_with("[summary]") && !text.trim().is_empty() {
                        k.requests.push(shorten(text, REQUEST_CHARS));
                    }
                }
                Part::ToolCall { name, args, .. } => {
                    let path = ["path", "file_path", "file"].iter().find_map(|key| args.get(*key)?.as_str());
                    let Some(path) = path else { continue };
                    let n = name.to_ascii_lowercase();
                    if n.contains("write") || n.contains("edit") || n.contains("patch") {
                        push_new(&mut k.changed, path);
                    } else if n.contains("read") {
                        push_new(&mut k.read, path);
                    }
                }
                _ => {}
            }
        }
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

/// Replaces `history[..keep_from]` with its summary (see the module). Returns the summary and
/// what its request spent.
pub async fn compact(s: &Summariser, history: &mut Vec<Message>, keep_from: usize) -> anyhow::Result<Compaction> {
    let keep_from = keep_from.min(history.len());
    let done = s.summarise(&history[..keep_from]).await?;
    let parts = summary_parts(&kept(&history[..keep_from]), &done.summary);
    let mut tail: Vec<Message> = history[keep_from..].to_vec();
    match tail.first_mut() {
        Some(m) if m.role == Role::User && !m.is_tool_results() => {
            m.content.splice(0..0, parts);
        }
        _ => tail.insert(0, Message::new(Role::User, parts)),
    }
    *history = tail;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str, name: &str, path: &str) -> Message {
        Message::new(Role::Assistant, vec![Part::tool_call(id, name, json!({"path": path}))])
    }

    fn result(id: &str, text: &str) -> Message {
        Message::tool_results(vec![Part::tool_result(id, text, false)])
    }

    #[test]
    fn the_tail_keeps_the_last_reply_with_its_results_or_the_new_request() {
        let h = vec![Message::user("task"), call("1", "read", "a"), result("1", "x"), call("2", "edit", "a")];
        assert_eq!(keep_recent(&h), Some(3));
        let mut h = vec![Message::user("task"), call("1", "read", "a"), result("1", "x")];
        assert_eq!(keep_recent(&h), Some(1), "the call stays with its result");
        h.push(Message::assistant("done"));
        h.push(Message::user("next"));
        assert_eq!(keep_recent(&h), Some(4), "the new request is kept whole");
        assert_eq!(keep_recent(&[Message::user("x")]), None, "nothing to summarise");
        assert_eq!(keep_recent(&[]), None);
    }

    #[test]
    fn the_requests_and_files_are_kept_word_for_word_across_compactions() {
        let h = vec![
            Message::user("Fix the parser in src/p.rs; never touch tests/."),
            call("1", "read", "src/p.rs"),
            result("1", "fn p() {}"),
            call("2", "edit", "src/p.rs"),
            result("2", "ok"),
            call("3", "read", "README.md"),
            result("3", "readme"),
        ];
        let k = kept(&h);
        assert_eq!(k.requests, ["Fix the parser in src/p.rs; never touch tests/."]);
        assert_eq!(
            (k.changed.as_slice(), k.read.as_slice()),
            (&["src/p.rs".to_string()][..], &["README.md".to_string()][..])
        );
        // a second compaction carries the first one's parts over
        let mut again = vec![Message::new(Role::User, summary_parts(&k, "parser fixed"))];
        again.push(Message::user("Now add a test."));
        again.push(call("4", "write", "tests/t.rs"));
        let k2 = kept(&again);
        assert_eq!(k2.requests, ["Fix the parser in src/p.rs; never touch tests/.", "Now add a test."]);
        assert_eq!(k2.changed, ["src/p.rs", "tests/t.rs"]);
        assert_eq!(k2.read, ["README.md"]);
    }

    #[test]
    fn transcript_shortens_results() {
        let m = vec![
            Message::user("hi"),
            Message::new(Role::Assistant, vec![Part::tool_call("c", "read", serde_json::json!({"path": "a"}))]),
            Message::tool_results(vec![Part::tool_result("c", "x".repeat(10_000), false)]),
        ];
        let t = transcript(&m);
        assert!(t.starts_with("[person] hi\n[call read]"));
        assert!(t.len() < 3_000);
        assert!(t.contains("[...]"));
    }
}
