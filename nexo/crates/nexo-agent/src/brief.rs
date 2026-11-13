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

use nexo_core::config::AgentConfig;
use nexo_core::message::{Message, Part, Role};
use serde_json::{json, Value};

pub mod curated;
mod ledger;
mod pystr;
#[rustfmt::skip]
mod pytables;
#[cfg(test)]
mod tests;

use ledger::*;

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
pub async fn build(task: &str, history: &[Message], cwd: &Path, cfg: &BriefConfig) -> Option<Brief> {
    if cfg.tokens == 0 || history.is_empty() || task.trim().is_empty() {
        return None;
    }
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
                let l = short(l, 240);
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
        s.text =
            s.text.lines().filter(|l| !notes.iter().any(|n| n == &short(l.trim(), 240))).collect::<Vec<_>>().join("\n");
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
        short(&quote(focus), 200)
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

// ------------------------------------------------------------ words

const STOP: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "any", "can", "had", "her", "was", "one", "our", "out",
    "has", "him", "his", "how", "its", "may", "new", "now", "old", "see", "two", "way", "who", "did", "get", "let",
    "put", "say", "she", "too", "use", "that", "with", "have", "this", "will", "your", "from", "they", "been", "were",
    "what", "when", "where", "which", "while", "there", "their", "them", "then", "than", "into", "only", "also",
    "some", "such", "each", "more", "most", "very", "here", "these", "those", "being", "does", "done", "over", "under",
    "please", "need", "needs", "want", "like", "make", "sure", "okay", "yes", "why", "should", "would", "could",
    "about", "after", "before", "just", "find", "look", "check", "tell", "show", "file", "files",
];

/// Topic words: words of at least 3 letters (identifiers split at `_` and
/// camelCase), lower-cased and cut to 5 characters, plus [`names`].
pub fn terms(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for tok in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        for part in tok.split('_') {
            for piece in humps(part) {
                if piece.chars().count() < 3 || piece.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                let low = piece.to_lowercase();
                if STOP.contains(&low.as_str()) {
                    continue;
                }
                out.insert(low.chars().take(5).collect());
            }
        }
    }
    out.extend(names(text));
    out
}

/// `parseLine` -> `parse`, `Line`.
fn humps(s: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut start = 0;
    let b: Vec<(usize, char)> = s.char_indices().collect();
    for k in 1..b.len() {
        if b[k].1.is_uppercase() && b[k - 1].1.is_lowercase() {
            out.push(&s[start..b[k].0]);
            start = b[k].0;
        }
    }
    out.push(&s[start..]);
    out
}

/// Distinctive tokens: paths, file names and code-like identifiers (with `_`,
/// digits or camelCase), at least 4 characters.
pub fn names(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for raw in text.split(|c: char| !(c.is_alphanumeric() || "_./-".contains(c))) {
        let w = raw.trim_matches(|c: char| ".-/".contains(c));
        if w.chars().count() < 4 || w.chars().all(|c| c.is_ascii_digit() || c == '.') {
            continue;
        }
        let letters = w.chars().any(char::is_alphabetic);
        let file = w.rsplit_once('.').is_some_and(|(a, e)| {
            a.len() >= 2 && (1..=4).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric())
        });
        let code = w.contains('_')
            || (w.chars().any(|c| c.is_ascii_digit()) && letters && !w.contains('.'))
            || humps(w).len() > 1;
        if letters && (w.contains('/') || file || (code && !w.contains('.'))) {
            out.insert(w.to_string());
        } else if w.contains('.') {
            // `cfg.epoch_tokens`: the attribute carries the meaning
            out.extend(names(&w.replace('.', " ")));
        }
    }
    out
}

const NOT_REQUEST: &[&str] = &["<system-reminder>", "<local-command", "<command-", "Caveat:", "[Request interrupted"];

fn is_request(text: &str) -> bool {
    let t = text.trim_start();
    !t.is_empty() && !NOT_REQUEST.iter().any(|p| t.starts_with(p))
}

// ------------------------------------------------------------ the conversation as units

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Person,
    Agent,
    Call,
    Result,
}

struct Unit {
    msg: usize,
    kind: Kind,
    tool: String,
    path: Option<String>,
    /// The files of a `read` that names several in `paths`; `path` is then empty.
    files: Vec<String>,
    command: Option<String>,
    text: String,
    error: bool,
}

fn str_arg<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| args.get(*k).and_then(Value::as_str)).filter(|s| !s.is_empty())
}

fn result_text(content: &[Part]) -> String {
    let mut out = String::new();
    for p in content {
        if let Part::Text { text } = p {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    out
}

/// The units of `history`. A call whose result failed is marked `error` too.
fn units(history: &[Message]) -> Vec<Unit> {
    let failed: BTreeSet<&str> = history
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|p| match p {
            Part::ToolResult { call_id, is_error: true, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    let mut calls: BTreeMap<&str, (&str, &Value)> = BTreeMap::new();
    let mut out = vec![];
    for (mi, m) in history.iter().enumerate() {
        for p in &m.content {
            match p {
                Part::Text { text } if m.role == Role::User && is_request(text) => out.push(Unit {
                    msg: mi,
                    kind: Kind::Person,
                    tool: String::new(),
                    files: vec![],
                    path: None,
                    command: None,
                    text: text.clone(),
                    error: false,
                }),
                Part::Text { text } if m.role == Role::Assistant => out.push(Unit {
                    msg: mi,
                    kind: Kind::Agent,
                    tool: String::new(),
                    files: vec![],
                    path: None,
                    command: None,
                    text: text.clone(),
                    error: false,
                }),
                Part::ToolCall { id, name, args } => {
                    calls.insert(id, (name, args));
                    let text = match name.as_str() {
                        "edit" => str_arg(args, &["new_string"]).unwrap_or("").to_string(),
                        "write" => str_arg(args, &["content"]).unwrap_or("").to_string(),
                        _ => String::new(),
                    };
                    out.push(Unit {
                        msg: mi,
                        kind: Kind::Call,
                        tool: name.clone(),
                        files: if nexo_core::tool::has_paths(args) {
                            nexo_core::tool::call_paths(args)
                        } else {
                            vec![]
                        },
                        path: str_arg(args, &["path", "file_path"])
                            .filter(|_| !nexo_core::tool::has_paths(args))
                            .map(str::to_string),
                        command: str_arg(args, &["command", "cmd"]).map(str::to_string),
                        text,
                        error: failed.contains(id.as_str()),
                    });
                }
                Part::ToolResult { call_id, name, content, is_error } => {
                    // a read stub points at lines the sub-agent never saw
                    if crate::reads::is_unchanged_note(&result_text(content)) {
                        continue;
                    }
                    let (tool, args) = match calls.get(call_id.as_str()) {
                        Some((n, a)) => (n.to_string(), Some(*a)),
                        None => (name.clone().unwrap_or_default(), None),
                    };
                    out.push(Unit {
                        msg: mi,
                        kind: Kind::Result,
                        files: args
                            .filter(|a| nexo_core::tool::has_paths(a))
                            .map(nexo_core::tool::call_paths)
                            .unwrap_or_default(),
                        path: args
                            .filter(|a| !nexo_core::tool::has_paths(a))
                            .and_then(|a| str_arg(a, &["path", "file_path"]))
                            .map(str::to_string),
                        command: args.and_then(|a| str_arg(a, &["command", "cmd"])).map(str::to_string),
                        tool,
                        text: result_text(content),
                        error: *is_error,
                    });
                }
                _ => {}
            }
        }
    }
    out
}

fn writes_file(tool: &str) -> bool {
    matches!(tool, "edit" | "write" | "multi_edit" | "apply_patch")
}

/// Number prefix of a file viewer's line: `    12│ ` or `12\t`.
fn numbered(line: &str) -> Option<(usize, &str)> {
    let t = line.trim_start();
    let end = t.find(|c: char| !c.is_ascii_digit())?;
    if end == 0 {
        return None;
    }
    let n = t[..end].parse().ok()?;
    let rest = &t[end..];
    let rest = rest.strip_prefix('│').or_else(|| rest.strip_prefix('\t'))?;
    Some((n, rest.strip_prefix(' ').unwrap_or(rest)))
}

/// Line ranges [start, end) cut at blank lines (line-number prefixes ignored).
fn paragraphs(lines: &[&str]) -> Vec<(usize, usize)> {
    let mut out = vec![];
    let mut start = 0;
    for (i, line) in lines.iter().enumerate() {
        let body = numbered(line).map(|(_, r)| r).unwrap_or(line);
        let n = i + 1 - start;
        if (body.trim().is_empty() && n >= 4) || n >= MAX_CHUNK_LINES {
            out.push((start, i + 1));
            start = i + 1;
        }
    }
    if start < lines.len() {
        match out.last_mut() {
            Some(last) if lines.len() - start < 3 => last.1 = lines.len(),
            _ => out.push((start, lines.len())),
        }
    }
    out
}

fn label(u: &Unit) -> String {
    match u.kind {
        Kind::Person => "the person".into(),
        Kind::Agent => "the main agent".into(),
        _ => {
            let what = u.path.clone().or_else(|| u.command.as_ref().map(|c| c.chars().take(80).collect()));
            match what {
                Some(w) => format!("{} {w}", u.tool),
                None => u.tool.clone(),
            }
        }
    }
}

// ------------------------------------------------------------ the lexical selection

struct Cand {
    unit: usize,
    msg: usize,
    start: usize,
    hold: BTreeSet<String>,
    factor: f64,
    text: String,
    tokens: usize,
}

/// The excerpts of `history` that bear on `task`, within `budget` tokens, in
/// conversation order: the curator's selection without its model. A turn
/// (from one request of the person to the next) is as relevant as the share of
/// the task's word weight its words hold, the latest one a little more; a
/// paragraph is worth the task words it holds (IDF-weighted, names double) times
/// that relevance; reads a later write made stale are left out; long paragraphs
/// keep the lines around the task's words; the pick is a greedy coverage.
pub fn lexical(task: &str, history: &[Message], budget: usize) -> Vec<Span> {
    lexical_but(task, history, budget, None)
}

/// Like [`lexical`], with the person's words of message `skip` left out of the excerpts.
fn lexical_but(task: &str, history: &[Message], budget: usize, skip: Option<usize>) -> Vec<Span> {
    let us = units(history);
    let q = terms(task);
    let q_names = names(task);
    if q.is_empty() || us.is_empty() {
        return vec![];
    }
    // the turns
    let mut turn_of = vec![0usize; history.len()];
    let mut turn = 0usize;
    let mut started = false;
    let mut by_msg_request = vec![false; history.len()];
    for u in &us {
        if u.kind == Kind::Person {
            by_msg_request[u.msg] = true;
        }
    }
    for (mi, slot) in turn_of.iter_mut().enumerate() {
        if by_msg_request[mi] {
            if started {
                turn += 1;
            }
            started = true;
        }
        *slot = turn;
    }
    let last_turn = turn;
    // writes per path, for stale reads: those whose result did not fail
    let mut writes: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for u in &us {
        if u.kind == Kind::Call && !u.error && writes_file(&u.tool) {
            if let Some(p) = &u.path {
                writes.entry(p.as_str()).or_default().push(u.msg);
            }
        }
    }
    // candidates
    struct Raw<'a> {
        unit: usize,
        lines: Vec<&'a str>,
        start: usize,
        words: BTreeSet<String>,
    }
    let mut raws: Vec<Raw> = vec![];
    for (k, u) in us.iter().enumerate() {
        if u.text.trim().is_empty()
            || u.tool == "task"
            || (u.kind == Kind::Person && Some(u.msg) == skip)
            || (u.kind == Kind::Call && tokens(&u.text) < 40)
        {
            continue;
        }
        if u.kind == Kind::Result && u.tool == "read" {
            if let Some(p) = &u.path {
                if writes.get(p.as_str()).is_some_and(|w| w.iter().any(|&m| m > u.msg)) {
                    continue;
                }
            }
        }
        let lines: Vec<&str> = u.text.split('\n').collect();
        let parts = if u.kind == Kind::Result { paragraphs(&lines) } else { vec![(0, lines.len())] };
        for (a, b) in parts {
            let chunk = &lines[a..b];
            let words = terms(&chunk.join("\n"));
            raws.push(Raw { unit: k, lines: chunk.to_vec(), start: a, words });
        }
    }
    if raws.is_empty() {
        return vec![];
    }
    let n = raws.len() as f64;
    let mut df: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &raws {
        for w in &r.words {
            *df.entry(w.as_str()).or_default() += 1;
        }
    }
    let mut idf: BTreeMap<String, f64> = BTreeMap::new();
    for w in &q {
        let mut v = (1.0 + n / (1.0 + *df.get(w.as_str()).unwrap_or(&0) as f64)).ln();
        if q_names.contains(w) {
            v *= 2.0;
        }
        idf.insert(w.clone(), v);
    }
    let total: f64 = idf.values().sum::<f64>().max(1e-9);
    // the relevance of each turn
    let mut turn_words: Vec<BTreeSet<&str>> = vec![BTreeSet::new(); last_turn + 1];
    for r in &raws {
        let t = turn_of[us[r.unit].msg];
        turn_words[t].extend(r.words.iter().map(String::as_str));
    }
    let rel: Vec<f64> = turn_words
        .iter()
        .enumerate()
        .map(|(t, ws)| {
            let s: f64 = idf.iter().filter(|(w, _)| ws.contains(w.as_str())).map(|(_, v)| v).sum();
            (s / total + if t == last_turn { 0.25 } else { 0.0 }).min(1.0)
        })
        .collect();
    let mut cands: Vec<Cand> = vec![];
    for r in &raws {
        let u = &us[r.unit];
        let hold: BTreeSet<String> = r.words.iter().filter(|w| idf.contains_key(*w)).cloned().collect();
        if hold.is_empty() {
            continue;
        }
        let weight: f64 = hold.iter().map(|w| idf[w]).sum();
        let named = hold.iter().any(|w| q_names.contains(w));
        if weight / total < MIN_SHARE && !named {
            continue;
        }
        let lines = if r.lines.len() > LONG { trim(&r.lines, &idf) } else { r.lines.clone() };
        let text = lines.join("\n").trim_matches('\n').to_string();
        if text.trim().is_empty() {
            continue;
        }
        let factor = (0.35 + 0.65 * rel[turn_of[u.msg]]) * if u.error && u.kind != Kind::Call { 1.15 } else { 1.0 };
        cands.push(Cand { unit: r.unit, msg: u.msg, start: r.start, hold, factor, tokens: tokens(&text), text });
    }
    // greedy weighted coverage
    let mut picked: Vec<Cand> = vec![];
    let mut covered: BTreeSet<String> = BTreeSet::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut left = budget;
    while !cands.is_empty() && picked.len() < MAX_SPANS {
        let mut best: Option<(f64, usize)> = None;
        for (k, c) in cands.iter().enumerate() {
            if c.tokens > left {
                continue;
            }
            let gain: f64 = c.hold.iter().map(|w| if covered.contains(w) { 0.25 * idf[w] } else { idf[w] }).sum();
            let worth = gain * c.factor / (c.tokens as f64).sqrt();
            // ties: the later passage (it is the fresher one)
            let better = match best {
                None => true,
                Some((bw, bk)) => worth > bw || (worth == bw && (c.msg, c.start) > (cands[bk].msg, cands[bk].start)),
            };
            if better {
                best = Some((worth, k));
            }
        }
        let Some((worth, k)) = best else { break };
        if worth <= 0.0 {
            break;
        }
        let c = cands.remove(k);
        if !seen.insert(c.text.clone()) {
            continue;
        }
        covered.extend(c.hold.iter().cloned());
        left -= c.tokens;
        picked.push(c);
    }
    picked.sort_by_key(|c| (c.msg, c.unit, c.start));
    picked.into_iter().map(|c| Span { msg: c.msg, label: label(&us[c.unit]), text: c.text, tokens: c.tokens }).collect()
}

/// The lines around those holding at least half the task word weight of the
/// best line (a word every line holds singles none out); gaps marked `…`.
fn trim<'a>(lines: &[&'a str], idf: &BTreeMap<String, f64>) -> Vec<&'a str> {
    let weight: Vec<f64> = lines
        .iter()
        .map(|l| {
            let body = numbered(l).map(|(_, r)| r).unwrap_or(l);
            terms(body).iter().filter_map(|w| idf.get(w)).sum()
        })
        .collect();
    let top = weight.iter().cloned().fold(0.0, f64::max);
    if top <= 0.0 {
        return lines[..LONG.min(lines.len())].to_vec();
    }
    let mut hit: Vec<usize> = (0..lines.len()).filter(|&k| weight[k] >= 0.5 * top).collect();
    if hit.len() > LONG {
        let mut order: Vec<usize> = (0..lines.len()).collect();
        order.sort_by(|&a, &b| weight[b].total_cmp(&weight[a]).then(a.cmp(&b)));
        hit = order.into_iter().take(LONG / 2).collect();
        hit.sort_unstable();
    }
    let keep: BTreeSet<usize> =
        hit.iter().flat_map(|&k| k.saturating_sub(AROUND)..(k + AROUND + 1).min(lines.len())).collect();
    let mut out = vec![];
    let mut prev: Option<usize> = None;
    for j in keep {
        if prev.is_some_and(|p| j != p + 1) {
            out.push("…");
        }
        out.push(lines[j]);
        prev = Some(j);
    }
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

// ------------------------------------------------------------ against the disk

fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Whether every numbered line of `text` is still that line of the file
/// (None: no numbered lines, or no file).
fn current(text: &str, file: &Path) -> Option<bool> {
    let lines: Vec<(usize, &str)> = text.lines().filter_map(numbered).collect();
    if lines.is_empty() {
        return None;
    }
    let disk = std::fs::read_to_string(file).ok()?;
    let disk: Vec<&str> = disk.lines().collect();
    Some(lines.iter().all(|&(n, shown)| {
        let Some(real) = n.checked_sub(1).and_then(|i| disk.get(i)) else { return false };
        let shown = shown.trim_end();
        let real = real.trim_end();
        real == shown || shown.split_once('…').is_some_and(|(a, _)| !a.is_empty() && real.starts_with(a))
    }))
}

fn range(text: &str) -> Option<(usize, usize)> {
    let ns: Vec<usize> = text.lines().filter_map(numbered).map(|(n, _)| n).collect();
    Some((*ns.iter().min()?, *ns.iter().max()?))
}

/// Read excerpts say which lines they hold and whether those still match the disk.
fn annotate(s: &mut Span, cwd: &Path, check: bool) {
    let Some(path) = s.label.strip_prefix("read ").map(str::to_string) else { return };
    if let Some((a, b)) = range(&s.text) {
        s.label = format!("read {} lines {a}-{b}", shown(cwd, &path));
        if check {
            match current(&s.text, &resolve(cwd, &path)) {
                Some(true) => s.label.push_str(" (current)"),
                Some(false) => s.label.push_str(" (changed on disk since)"),
                None => {}
            }
        }
    }
}

// ------------------------------------------------------------ the ledger

/// A path as the briefing shows it: relative to the project when inside it (shorter).
fn shown(cwd: &Path, p: &str) -> String {
    match Path::new(p).strip_prefix(cwd) {
        Ok(r) if !r.as_os_str().is_empty() => r.display().to_string(),
        _ => p.to_string(),
    }
}

/// Up to six lines that say what went wrong.
fn error_lines(text: &str) -> Vec<String> {
    let body = text.rsplit_once("[exit code").map(|(a, _)| a).unwrap_or(text);
    let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    let keys = ["error", "fail", "panic", "assert", "expected", "cannot", "not found", "traceback"];
    let hits: Vec<&str> =
        lines.iter().copied().filter(|l| keys.iter().any(|k| l.to_lowercase().contains(k))).take(6).collect();
    let pick = if hits.is_empty() { lines[lines.len().saturating_sub(6)..].to_vec() } else { hits };
    pick.into_iter().map(|l| short(l, 200)).collect()
}

// ------------------------------------------------------------ the report back

/// The report as it enters the main conversation: lines that repeat the
/// briefing's excerpts word for word left out (the main agent has them), and
/// at most `cap` tokens.
pub fn compact_report(report: &str, brief: Option<&str>, cap: usize) -> String {
    let known: BTreeSet<&str> =
        brief.map(|b| b.lines().map(str::trim).filter(|l| l.len() >= 40).collect()).unwrap_or_default();
    let mut out = String::new();
    let mut dropped = 0usize;
    let flush = |out: &mut String, dropped: &mut usize| {
        if *dropped > 0 {
            out.push_str(&format!("(… {} line{} you already have)\n", *dropped, if *dropped == 1 { "" } else { "s" }));
            *dropped = 0;
        }
    };
    for line in report.lines() {
        let t = line.trim().trim_start_matches(['>', '-', '*', ' ']);
        let body = numbered(t).map(|(_, r)| r.trim()).unwrap_or(t);
        if t.len() >= 40 && (known.contains(t) || known.iter().any(|k| k.ends_with(body) && body.len() >= 40)) {
            dropped += 1;
            continue;
        }
        flush(&mut out, &mut dropped);
        out.push_str(line);
        out.push('\n');
    }
    flush(&mut out, &mut dropped);
    let out = out.trim_end().to_string();
    if tokens(&out) <= cap {
        return out;
    }
    let mut cut = String::new();
    for line in out.lines() {
        if tokens(&cut) + tokens(line) > cap.saturating_sub(20) {
            break;
        }
        cut.push_str(line);
        cut.push('\n');
    }
    format!("{}\n(… the report went on; cut at about {cap} tokens)", cut.trim_end())
}
