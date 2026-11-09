//! Making the history shorter: a summary of the older part replaces it.
//!
//! What replaces it, in one message from the person: a header, every message the person wrote in the
//! summarised part word for word (`[request]` parts, carried over from an earlier summary too), the
//! files the tools changed and read (`[files]`), and the model's summary of the work. The task and
//! the files are kept by the harness, not left to the summary, so a compaction never loses them.

use std::sync::Arc;

use futures::StreamExt;
use nexo_core::message::{estimate_tokens, Message, Part, Role};
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
/// Characters of the whole transcript given to the summariser, at most (fewer when the model's window is
/// smaller, see [`transcript_chars`]).
const TRANSCRIPT_CHARS: usize = 160_000;
/// Characters a token is taken to be when the transcript is fitted to the window: fewer than the
/// usual 4, since code, paths and numbers take more tokens.
const WINDOW_CHARS_PER_TOKEN: u64 = 3;
/// Characters of one message of the person kept word for word (longer ones keep their head and tail);
/// the first one, the task statement, keeps up to [`FIRST_REQUEST_CHARS`].
const REQUEST_CHARS: usize = 8_000;
/// Characters of the person's first message (the task statement) kept word for word.
const FIRST_REQUEST_CHARS: usize = 48_000;
/// Tokens all the kept messages of the person take together, at most (fewer with a small window): past
/// it the oldest are left out, never the first.
const REQUESTS_TOKENS: u64 = 24_000;

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
    /// The model's context window in tokens, when known: the summary request and what the summary
    /// keeps word for word are sized to it.
    pub window: Option<u32>,
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
        let reply = self.window.map_or(self.max_tokens, |w| self.max_tokens.min(w / 2));
        let transcript = transcript(msgs, transcript_chars(self.window, reply));
        let ask = format!("Transcript of the session so far:\n\n{transcript}\n\nWrite the summary.");
        let mut req = ModelRequest::new(model.clone(), SUMMARY_SYSTEM, vec![Message::user(ask)]);
        req.max_tokens = reply;
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

    /// Tokens the kept messages of the person may take: a quarter of a known window at most.
    fn requests_tokens(&self) -> u64 {
        self.window.map_or(REQUESTS_TOKENS, |w| REQUESTS_TOKENS.min(u64::from(w) / 4))
    }
}

/// Characters of transcript the summary request may carry: what the window leaves after the
/// summary's own reply (`reply` tokens), with room for the estimate running short, and never more
/// than [`TRANSCRIPT_CHARS`].
pub fn transcript_chars(window: Option<u32>, reply: u32) -> usize {
    match window {
        Some(w) => {
            let room = u64::from(w.saturating_sub(reply).max(w / 2));
            let chars = room * 3 / 5 * WINDOW_CHARS_PER_TOKEN;
            (chars as usize).min(TRANSCRIPT_CHARS)
        }
        None => TRANSCRIPT_CHARS,
    }
}

/// The conversation as plain text, tool results shortened, in `max_chars` characters at most (about):
/// past it, the first message (the task, or the summary that stands for the older work) and the
/// newest messages that fit, with a mark where the middle is left out.
pub fn transcript(msgs: &[Message], max_chars: usize) -> String {
    let lines: Vec<String> = msgs.iter().map(rendered).collect();
    let total: usize = lines.iter().map(|l| l.chars().count()).sum();
    if total <= max_chars || lines.is_empty() {
        return lines.concat();
    }
    const GAP: &str = "[... the middle of the transcript is left out ...]\n";
    let mut out = shorten(&lines[0], max_chars / 4);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    let mut left = max_chars.saturating_sub(out.chars().count() + GAP.len());
    let mut tail = Vec::new();
    for l in lines[1..].iter().rev() {
        let n = l.chars().count();
        if n > left {
            if tail.is_empty() {
                // the newest message alone is past the room left: its end
                tail.push(l.chars().skip(n - left).collect::<String>());
            }
            break;
        }
        left -= n;
        tail.push(l.clone());
    }
    tail.reverse();
    out.push_str(GAP);
    out.push_str(&tail.concat());
    out
}

/// One message of the transcript.
fn rendered(m: &Message) -> String {
    let mut out = String::new();
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
const JOBS_NOTE: &str = "[background jobs ended]";
const JOBS_NOTE_END: &str = "Read the output with the `jobs` tool";
/// Memory shared from another session, put before a request.
const MEMORY_START: &str = "<memory from=\"";
const MEMORY_END: &str = "</memory>";

/// What the person wrote in `text`, without what the harness adds to a request: notes of ended
/// background jobs and memory shared by other sessions (before it), the files attached to it (after
/// it; the paths of those attached whole go to `attached`), and the loop's own continuation of a
/// goal. None when nothing of the person is left.
fn person_text(text: &str, attached: &mut Vec<String>) -> Option<String> {
    if text == crate::turn::GOAL_CONTINUE {
        return None;
    }
    let mut t = text.trim_start();
    loop {
        if t.starts_with(JOBS_NOTE) {
            let i = t.find(JOBS_NOTE_END)?;
            t = t[i..].split_once('\n').map_or("", |(_, after)| after).trim_start();
        } else if t.starts_with(MEMORY_START) {
            let i = t.find(MEMORY_END)?;
            t = t[i + MEMORY_END.len()..].trim_start();
        } else {
            break;
        }
    }
    // the attachments come after the text, each from a line that starts with `@` and the file's path
    let mut end = t.len();
    for (i, _) in t.match_indices("\n\n@") {
        let line = t[i + 2..].lines().next().unwrap_or("");
        match line.strip_suffix(crate::reads::ATTACHED) {
            Some(path) => {
                end = end.min(i);
                push_new(attached, path.trim_start_matches('@').trim_end());
            }
            None if line.contains(": attached as an image") || line.contains(": not attached") => end = end.min(i),
            None => {}
        }
    }
    let t = t[..end].trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Whether `text` is one of the parts a summary is made of.
fn summary_part(text: &str) -> bool {
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

/// Replaces `history[..keep_from]` with its summary (see the module). Returns the summary and
/// what its request spent.
pub async fn compact(s: &Summariser, history: &mut Vec<Message>, keep_from: usize) -> anyhow::Result<Compaction> {
    let keep_from = keep_from.min(history.len());
    let done = s.summarise(&history[..keep_from]).await?;
    let mut k = kept(&history[..keep_from]);
    k.cap(s.requests_tokens());
    let parts = summary_parts(&k, &done.summary);
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
        again.push(result("4", "ok"));
        let k2 = kept(&again);
        assert_eq!(k2.requests, ["Fix the parser in src/p.rs; never touch tests/.", "Now add a test."]);
        assert_eq!(k2.changed, ["src/p.rs", "tests/t.rs"]);
        assert_eq!(k2.read, ["README.md"]);
    }

    #[test]
    fn every_file_of_a_read_of_several_is_kept_as_read() {
        let h = vec![
            Message::user("Compare a and b."),
            Message::new(
                Role::Assistant,
                vec![Part::tool_call("1", "read", json!({"paths": ["a.rs", "b.rs", "a.rs"]}))],
            ),
            result("1", "x"),
        ];
        assert_eq!(kept(&h).read, ["a.rs", "b.rs"]);
    }

    #[test]
    fn transcript_shortens_results() {
        let m = vec![
            Message::user("hi"),
            Message::new(Role::Assistant, vec![Part::tool_call("c", "read", serde_json::json!({"path": "a"}))]),
            Message::tool_results(vec![Part::tool_result("c", "x".repeat(10_000), false)]),
        ];
        let t = transcript(&m, TRANSCRIPT_CHARS);
        assert!(t.starts_with("[person] hi\n[call read]"));
        assert!(t.len() < 3_000);
        assert!(t.contains("[...]"));
    }

    #[test]
    fn the_transcript_fits_a_small_window_with_the_task_and_the_newest_work() {
        // a 32k window: the summary request must fit in it, its own reply included
        let max = transcript_chars(Some(32_000), 4_096);
        assert!(max / 3 + 4_096 < 32_000 && max > 20_000, "{max}");
        assert_eq!(transcript_chars(None, 4_096), TRANSCRIPT_CHARS);
        assert_eq!(transcript_chars(Some(1_000_000), 4_096), TRANSCRIPT_CHARS);
        let mut h = vec![Message::user("Port the CSV loader to streaming; keep the column order.")];
        for i in 0..200 {
            h.push(call(&format!("c{i}"), "read", &format!("data/part{i}.csv")));
            h.push(result(&format!("c{i}"), &"row,".repeat(400)));
        }
        h.push(Message::assistant("LATEST: the loader streams now."));
        let t = transcript(&h, max);
        assert!(t.chars().count() <= max + 100, "{} > {max}", t.chars().count());
        assert!(t.starts_with("[person] Port the CSV loader to streaming; keep the column order."));
        assert!(t.contains("left out") && t.ends_with("[assistant] LATEST: the loader streams now.\n"));
    }

    #[test]
    fn the_requests_are_capped_in_tokens_keeping_the_whole_task_statement() {
        // the task's rules sit in its middle, past the 8000 characters any other message keeps
        let task =
            format!("Clean the sales table. {} RULE: never drop a row. {}", "a".repeat(9_000), "b".repeat(9_000));
        let mut h = vec![Message::user(task.clone())];
        for i in 0..30 {
            h.push(Message::assistant("ok"));
            h.push(Message::user(format!("step {i}: {}", "c".repeat(3_000))));
        }
        let mut k = kept(&h);
        assert_eq!(k.requests.len(), 31);
        k.cap(24_000);
        assert_eq!(k.requests[0], task, "the task statement is kept whole");
        let chars: usize = k.requests.iter().map(|r| r.chars().count()).sum();
        assert!(chars <= 24_000 * 4, "{chars}");
        assert!(k.requests.len() < 31 && k.requests.last().unwrap().starts_with("step 29:"), "the oldest go first");
        // a small window: a smaller budget, the task still first and its head and tail kept
        let mut k = kept(&h);
        k.cap(2_000);
        assert!(k.requests[0].starts_with("Clean the sales table.") && k.requests[0].chars().count() <= 4_100);
        assert!(k.requests.iter().map(|r| r.chars().count()).sum::<usize>() <= 8_000 || k.requests.len() == 1);
    }

    #[test]
    fn what_the_harness_adds_to_a_request_is_not_kept_as_one() {
        let shared = "<memory from=\"s-1\" title=\"t\">\nShared from another session.\nExcerpts: x\n</memory>";
        let jobs = "[background jobs ended]\n- job 1 `make` ended after 3s, 2 lines of output\n\
                    Read the output with the `jobs` tool (action \"output\", the job's id).";
        let attached = format!(
            "\n\n{}\n```csv\nid,total\n1,2\n```\n\n@shot.png: attached as an image",
            crate::reads::attached_header("data/sales.csv")
        );
        let h = vec![
            Message::user(format!("{shared}\n\n{jobs}\n\nSum the totals per month.{attached}")),
            Message::assistant("ok"),
            Message::user(jobs),
            Message::user(crate::turn::GOAL_CONTINUE),
            Message::new(
                Role::User,
                vec![
                    Part::tool_result("1", "x", false),
                    Part::text("Tool calls are off for the rest of this request."),
                ],
            ),
        ];
        let k = kept(&h);
        assert_eq!(k.requests, ["Sum the totals per month."]);
        assert_eq!(k.read, ["data/sales.csv"], "a file attached whole was seen");
    }

    #[test]
    fn a_failed_write_is_not_listed_as_changed() {
        let h = vec![
            Message::user("rename the field"),
            call("1", "edit", "src/a.ts"),
            Message::tool_results(vec![Part::tool_result("1", "old text not found", true)]),
            call("2", "read", "missing.ts"),
            Message::tool_results(vec![Part::tool_result("2", "no such file", true)]),
            call("3", "edit", "src/b.ts"),
            result("3", "ok"),
            call("4", "write", "src/c.ts"),
        ];
        let k = kept(&h);
        assert_eq!(k.changed, ["src/b.ts"], "failed and unanswered writes changed nothing");
        assert!(k.read.is_empty(), "{:?}", k.read);
    }

    #[test]
    fn fresh_tokens_leave_out_an_earlier_summary() {
        let k = Kept { requests: vec!["x".repeat(4_000)], ..Default::default() };
        let summary = Message::new(Role::User, summary_parts(&k, &"y".repeat(4_000)));
        let small = vec![summary.clone(), call("1", "read", "a"), result("1", "z")];
        assert!(fresh_tokens(&small) < 100, "{}", fresh_tokens(&small));
        let big = vec![summary, call("1", "read", "a"), result("1", &"z".repeat(8_000))];
        assert!(fresh_tokens(&big) >= 2_000, "{}", fresh_tokens(&big));
    }
}
