//! The briefing of a sub-agent: what the conversation that sends it already
//! holds that bears on its task, picked by nexo (not written by the model).
//!
//! Without it a sub-agent starts from the task text alone and reads again what
//! the main agent has just read, runs again the test that has just failed, and
//! asks again what the person said not to do: model calls and tokens spent twice.
//! The briefing is two things within a budget of tokens (`[agent]
//! subagent_brief`, 0 turns it off):
//!
//! - a ledger of facts (at most 40% of the budget): the person's request and
//!   the constraints they gave, the files read and changed (with the lines seen
//!   and whether they still match the disk), the commands that fail and how,
//!   and the plan;
//! - excerpts: the passages of tool outputs and messages that bear on the task,
//!   chosen by the curator's selection, run in
//!   process ([`curated`]: the link model of turns, IDF-weighted paragraphs,
//!   line-level trimming, a greedy coverage), or by [`lexical`], a simpler
//!   selection of the same shape without the model.
//!
//! Both are deterministic: the same conversation and task give the same bytes.
//! The report coming back is compacted by [`compact_report`] (lines echoed from
//! the briefing dropped, a cap on its length).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nexo_core::config::AgentConfig;
use nexo_core::message::{Message, Part, Role};
use serde_json::{json, Value};

mod annotate;
pub mod curated;
mod ledger;
mod lexical;
mod pystr;
mod report;
#[rustfmt::skip]
mod pytables;
#[cfg(test)]
mod tests;
mod units;
mod words;

use crate::repeat::one_line;
use annotate::*;
use ledger::*;
pub use lexical::lexical;
use lexical::*;
pub use report::compact_report;
use units::*;
use words::*;
pub use words::{names, terms};

/// A paragraph cut at blank lines holds at most this many lines.
const MAX_CHUNK_LINES: usize = 30;
/// A longer excerpt keeps only the lines around the task's words.
const LONG: usize = 14;
/// Lines kept on each side of a matching line.
const AROUND: usize = 2;
/// Share of the task's word weight an excerpt must hold (or one of its names).
const MIN_SHARE: f64 = 0.12;
const MAX_SPANS: usize = 24;
/// The report coming back is cut at this many tokens.
pub const REPORT_TOKENS: usize = 2000;

/// Where the excerpts come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// The curator's selection ([`curated::select`]).
    Auto,
    /// The same as `Auto`.
    Curator,
    Lexical,
}

impl Engine {
    pub fn parse(s: &str) -> Engine {
        match s.trim().to_ascii_lowercase().as_str() {
            "curator" => Engine::Curator,
            "lexical" | "rust" => Engine::Lexical,
            _ => Engine::Auto,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BriefConfig {
    /// The budget in tokens; 0: no briefing.
    pub tokens: usize,
    pub engine: Engine,
    /// Compare the lines read with the files on disk.
    pub check_disk: bool,
}

impl Default for BriefConfig {
    fn default() -> BriefConfig {
        BriefConfig { tokens: 3000, engine: Engine::Auto, check_disk: true }
    }
}

impl BriefConfig {
    pub fn from_agent(cfg: &AgentConfig) -> BriefConfig {
        BriefConfig {
            tokens: cfg.subagent_brief as usize,
            engine: Engine::parse(&cfg.subagent_brief_engine),
            ..BriefConfig::default()
        }
    }
}

/// One excerpt of the conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    /// The message it comes from.
    pub msg: usize,
    /// What it is: `read src/x.rs`, `bash cargo test`, `the person`...
    pub label: String,
    pub text: String,
    pub tokens: usize,
}

/// What a sub-agent is handed with its task.
#[derive(Debug, Clone, PartialEq)]
pub struct Brief {
    pub text: String,
    pub tokens: usize,
    pub spans: usize,
    /// Lines of the ledger (files, failures, constraints, plan items).
    pub facts: usize,
    /// `curator` or `lexical`.
    pub engine: &'static str,
}

impl Brief {
    /// The line shown where the sub-agent works: `briefed: 2.1k tok, 6 spans, 4 facts · curator`.
    pub fn line(&self) -> String {
        format!("briefed: {} tok, {} spans, {} facts · {}", ktok(self.tokens), self.spans, self.facts, self.engine)
    }
}

fn ktok(n: usize) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        format!("{:.1}k", n as f64 / 1000.0)
    }
}

/// Tokens of a text, roughly (four bytes each).
pub fn tokens(text: &str) -> usize {
    text.len() / 4 + 1
}

/// The briefing for `task`, from `history` (the conversation that sends the
/// sub-agent); None when off or when the conversation holds nothing for it.
pub async fn build(task: &str, history: &Arc<Vec<Message>>, cwd: &Path, cfg: &BriefConfig) -> Option<Brief> {
    if cfg.tokens == 0 || history.is_empty() || task.trim().is_empty() {
        return None;
    }
    // Selection, the copy of the history and the disk checks are synchronous and can take seconds on
    // long sessions. Keep all of that off Tokio's workers, including single-worker runtimes.
    let (task, history, cwd, cfg) = (task.to_owned(), Arc::clone(history), cwd.to_owned(), cfg.clone());
    tokio::task::spawn_blocking(move || build_sync(&task, &history, &cwd, &cfg)).await.expect("briefing worker failed")
}

fn build_sync(task: &str, history: &[Message], cwd: &Path, cfg: &BriefConfig) -> Option<Brief> {
    let ledger = Ledger::of(history, cwd, cfg.check_disk, task);
    let (head, facts) = ledger.render(cfg.tokens * 2 / 5);
    // the latest request the ledger states whole is not an excerpt too
    let skip = ledger.request_at.filter(|_| ledger.request.as_deref().is_some_and(|r| head.contains(r)));
    let mut left = cfg.tokens.saturating_sub(tokens(&render(&head, &[])) + 10);
    let mut tries = 0;
    let (spans, engine, text) = loop {
        let (mut spans, engine) = match cfg.engine {
            Engine::Lexical => (lexical_but(task, history, left, skip), "lexical"),
            Engine::Auto | Engine::Curator => (curator_but(task, history, left, skip), "curator"),
        };
        for s in &mut spans {
            annotate(s, cwd, cfg.check_disk);
        }
        let mut text = render(&head, &spans);
        let over = tokens(&text).saturating_sub(cfg.tokens);
        tries += 1;
        if over == 0 || spans.is_empty() || tries == 3 {
            // still over after a few tries: the last excerpts go
            while tokens(&text) > cfg.tokens && spans.pop().is_some() {
                text = render(&head, &spans);
            }
            break (spans, engine, text);
        }
        // the excerpts' labels cost tokens the selection does not count: it gets fewer
        left = left.saturating_sub(over);
    };
    if spans.is_empty() && facts == 0 {
        return None;
    }
    Some(Brief { tokens: tokens(&text), text, spans: spans.len(), facts, engine })
}

/// Words that open a note an agent keeps for itself: its findings, plans, decisions and constraints.
const NOTE_HEADS: &[&str] = &["Finding:", "Plan:", "Decision:", "Constraint:", "Fact:"];

/// What another session (`from`, named `title`) knows that bears on `focus`, for use in this one: the
/// notes its agent wrote, its ledger (latest request, constraints, files, failing commands, plan) and
/// the excerpts with the most of `focus`'s words, within about `budget` tokens. None when it holds
/// nothing. Its tokens come with it.
pub fn recall(
    from: &str,
    title: &str,
    focus: &str,
    history: &[Message],
    cwd: &Path,
    budget: usize,
) -> Option<(String, usize)> {
    if history.is_empty() || budget < 200 {
        return None;
    }
    let focus = if focus.trim().is_empty() { title } else { focus };
    // the agent's own notes, the latest copy of each, newest last
    let mut notes: Vec<String> = Vec::new();
    for m in history.iter().filter(|m| m.role == Role::Assistant) {
        for l in m.text().lines().map(str::trim) {
            if NOTE_HEADS.iter().any(|h| l.starts_with(h)) {
                let l = one_line(l, 240);
                notes.retain(|n| *n != l);
                notes.push(l);
            }
        }
    }
    let mut note_text = String::new();
    for n in notes.iter().rev() {
        if tokens(&note_text) + tokens(n) > budget / 4 {
            break;
        }
        note_text.insert_str(0, &format!("- {n}\n"));
    }
    let ledger = Ledger::of(history, cwd, false, focus);
    let (head, facts) = ledger.render(budget / 3);
    let left = budget.saturating_sub(tokens(&note_text) + tokens(&head) + 80);
    // a note already listed is not repeated in the excerpts
    let mut spans = lexical(focus, history, left);
    for s in &mut spans {
        s.text = s
            .text
            .lines()
            .filter(|l| !notes.iter().any(|n| n == &one_line(l.trim(), 240)))
            .collect::<Vec<_>>()
            .join("\n");
    }
    spans.retain(|s| !s.text.trim().is_empty());
    if spans.is_empty() && facts == 0 && notes.is_empty() {
        return None;
    }
    let quote = |s: &str| s.replace(['"', '\n', '\r'], " ");
    let mut out = format!(
        "<memory from=\"{}\" title=\"{}\">\nShared from another session of the person, picked by its bearing on: {}. \
         It was true then; files may have changed since.\n",
        quote(from),
        quote(title),
        one_line(&quote(focus), 200)
    );
    if !note_text.is_empty() {
        out.push_str("Notes its agent wrote:\n");
        out.push_str(&note_text);
    }
    out.push_str(&head);
    if !spans.is_empty() {
        out.push_str("Excerpts:\n");
        for s in &spans {
            out.push_str(&format!("--- {} ---\n{}\n", s.label, s.text.trim_end()));
        }
    }
    out.push_str("</memory>");
    let n = tokens(&out);
    Some((out, n))
}

fn render(head: &str, spans: &[Span]) -> String {
    let mut out = String::from(
        "<briefing>\nnexo picked these from the conversation that sent you, by their bearing on your task \
         (the main agent did not write them). Excerpts are tool outputs as they were; those marked \"current\" \
         still match the file on disk, so do not read those lines again.\n",
    );
    if !head.is_empty() {
        out.push('\n');
        out.push_str(head);
    }
    if !spans.is_empty() {
        out.push_str("\nExcerpts:\n");
        for s in spans {
            out.push_str(&format!("--- {} ---\n{}\n", s.label, s.text.trim_end()));
        }
    }
    out.push_str("</briefing>");
    out
}

// ------------------------------------------------------------ the curator's selection

/// The messages as the curator reads them (images left out: they only weigh; a read stub is
/// emptied: it points at lines the sub-agent never saw).
fn wire(history: &[Message]) -> Vec<Value> {
    history
        .iter()
        .map(|m| {
            let parts: Vec<Part> = m
                .content
                .iter()
                .filter(|p| !matches!(p, Part::Image { .. }))
                .map(|p| match p {
                    Part::ToolResult { call_id, name, content, is_error }
                        if crate::reads::is_unchanged_note(&result_text(content)) =>
                    {
                        Part::ToolResult {
                            call_id: call_id.clone(),
                            name: name.clone(),
                            content: vec![],
                            is_error: *is_error,
                        }
                    }
                    p => p.clone(),
                })
                .collect();
            json!({"role": m.role, "content": parts})
        })
        .collect()
}

/// The curator's selection, as excerpts.
/// The selection counts a token per four characters, the briefing per four bytes: text of several bytes
/// a letter (Cyrillic, CJK) is fitted to `budget` again, the least scored excerpts going first.
#[cfg(test)]
fn curator(task: &str, history: &[Message], budget: usize) -> Vec<Span> {
    curator_but(task, history, budget, None)
}

fn curator_but(task: &str, history: &[Message], budget: usize, skip: Option<usize>) -> Vec<Span> {
    let mut spans = curated::select_but(task, &wire(history), i64::try_from(budget).unwrap_or(i64::MAX), skip).spans;
    let mut total: usize = spans.iter().map(|s| tokens(&s.text)).sum();
    while total > budget {
        let Some(k) = (0..spans.len()).min_by(|&a, &b| spans[a].score.total_cmp(&spans[b].score)) else { break };
        total -= tokens(&spans.remove(k).text);
    }
    spans.into_iter().map(|s| Span { msg: s.msg, label: s.label, tokens: tokens(&s.text), text: s.text }).collect()
}
