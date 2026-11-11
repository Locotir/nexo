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

pub mod curated;
mod pystr;
#[rustfmt::skip]
mod pytables;
use nexo_core::message::{Message, Part, Role};
use serde_json::{json, Value};

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
    let mut left = cfg.tokens.saturating_sub(tokens(&render(&head, &[])) + 10);
    let mut tries = 0;
    let (spans, engine, text) = loop {
        let (mut spans, engine) = match cfg.engine {
            Engine::Lexical => (lexical(task, history, left), "lexical"),
            Engine::Auto | Engine::Curator => (curator(task, history, left), "curator"),
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
        if u.text.trim().is_empty() || u.tool == "task" || (u.kind == Kind::Call && tokens(&u.text) < 40) {
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
fn curator(task: &str, history: &[Message], budget: usize) -> Vec<Span> {
    let mut spans = curated::select(task, &wire(history), i64::try_from(budget).unwrap_or(i64::MAX)).spans;
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

/// How much a fact bears on the task: the task's words it holds, its names thrice.
fn bearing(text: &str, q: &BTreeSet<String>, qn: &BTreeSet<String>) -> usize {
    let t = terms(text);
    t.intersection(q).count() + 2 * t.intersection(qn).count()
}

/// A request longer than this is a pasted document or a summary, not a rule to keep.
const RULE_REQUEST: usize = 1500;

#[derive(Debug, Default)]
struct FileNote {
    /// The lines of the latest read, and whether they still match the disk.
    read: Option<(usize, usize)>,
    current: Option<bool>,
    read_at: usize,
    edits: usize,
    written: bool,
    changed_at: usize,
    /// Read in a call of several files, whose lines are not told apart.
    read_in_many: bool,
    last: usize,
}

/// Facts of the conversation, as lines.
#[derive(Debug, Default)]
struct Ledger {
    request: Option<String>,
    constraints: Vec<String>,
    files: Vec<String>,
    failing: Vec<String>,
    plan: Vec<String>,
}

const MARKERS: &[&str] = &[
    "don't",
    "do not",
    "never",
    "always",
    "must",
    "only",
    "avoid",
    "without",
    "keep ",
    "prefer",
    "make sure",
    "no ",
    "nunca",
    "siempre",
    "sin ",
    "solo ",
];

fn short(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= n {
        s
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

impl Ledger {
    /// The facts of `history`, those that bear on `task` first.
    fn of(history: &[Message], cwd: &Path, check: bool, task: &str) -> Ledger {
        let us = units(history);
        let (q, qn) = (terms(task), names(task));
        let mut l = Ledger::default();
        let requests: Vec<&Unit> = us.iter().filter(|u| u.kind == Kind::Person).collect();
        if let Some(last) = requests.last() {
            if last.text.trim() != task.trim() {
                l.request = Some(short(&last.text, 500));
            }
        }
        let mut rules: Vec<(usize, String)> = vec![];
        for r in requests.iter().rev().skip(1).filter(|r| r.text.len() <= RULE_REQUEST) {
            for sentence in r.text.split(['\n', ';']).flat_map(|s| s.split(". ")) {
                let low = sentence.to_lowercase();
                let s = short(sentence, 200);
                if s.len() >= 8 && MARKERS.iter().any(|m| low.contains(m)) && !rules.iter().any(|(_, x)| *x == s) {
                    rules.push((bearing(&s, &q, &qn), s));
                }
            }
            if rules.len() >= 16 {
                break;
            }
        }
        // those that bear on the task first, the latest first among equals
        rules.sort_by_key(|a| std::cmp::Reverse(a.0));
        l.constraints = rules.into_iter().take(6).map(|(_, s)| s).collect();
        // files
        let mut files: BTreeMap<String, FileNote> = BTreeMap::new();
        for u in &us {
            if u.kind == Kind::Result && u.tool == "read" && !u.error {
                for p in &u.files {
                    let f = files.entry(p.clone()).or_default();
                    f.last = f.last.max(u.msg);
                    f.read_in_many = true;
                    f.read_at = u.msg;
                }
            }
            let Some(p) = &u.path else { continue };
            let f = files.entry(p.clone()).or_default();
            f.last = f.last.max(u.msg);
            match (u.kind, u.tool.as_str()) {
                (Kind::Result, "read") if !u.error => {
                    if let Some(r) = range(&u.text) {
                        f.read = Some(r);
                        f.read_at = u.msg;
                        f.current = if check { current(&u.text, &resolve(cwd, p)) } else { None };
                    }
                }
                // a write counts unless its result failed (one still running counts)
                (Kind::Call, t) if writes_file(t) && !u.error => {
                    if t == "write" {
                        f.written = true;
                    } else {
                        f.edits += 1;
                    }
                    f.changed_at = u.msg;
                }
                _ => {}
            }
        }
        // files the task names first, then the latest
        let mut order: Vec<(&String, &FileNote)> = files.iter().collect();
        order.sort_by(|a, b| {
            let (x, y) = (bearing(a.0, &q, &qn) > 0, bearing(b.0, &q, &qn) > 0);
            y.cmp(&x).then(b.1.last.cmp(&a.1.last)).then(a.0.cmp(b.0))
        });
        for (p, f) in order.into_iter().take(12) {
            let mut what = vec![];
            if let Some((a, b)) = f.read {
                let state = if f.changed_at > f.read_at {
                    " (changed after)".to_string()
                } else {
                    match f.current {
                        Some(true) => " (current)".into(),
                        Some(false) => " (changed on disk since)".into(),
                        None => String::new(),
                    }
                };
                what.push(format!("read lines {a}-{b}{state}"));
            }
            if f.read.is_none() && f.read_in_many {
                what.push(if f.changed_at > f.read_at { "read (changed after)" } else { "read" }.into());
            }
            if f.written {
                what.push("written".into());
            }
            if f.edits > 0 {
                what.push(format!("edited {}x", f.edits));
            }
            if !what.is_empty() {
                l.files.push(format!("- {}: {}", shown(cwd, p), what.join("; ")));
            }
        }
        // failing commands: an error with no later success of the same command
        let mut last: BTreeMap<&str, &Unit> = BTreeMap::new();
        for u in us.iter().filter(|u| u.kind == Kind::Result && u.tool == "bash") {
            if let Some(c) = &u.command {
                last.insert(c.as_str(), u);
            }
        }
        // still failing: those that bear on the task, and the latest one since the person's last request
        let since = requests.last().map(|r| r.msg).unwrap_or(0);
        let mut failing: Vec<(usize, &Unit)> = last
            .into_values()
            .filter(|u| u.error)
            .map(|u| {
                (
                    bearing(
                        &format!("{}\n{}", u.command.as_deref().unwrap_or(""), error_lines(&u.text).join("\n")),
                        &q,
                        &qn,
                    ),
                    u,
                )
            })
            .collect();
        failing.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.msg.cmp(&a.1.msg)));
        let latest = failing.iter().map(|(_, u)| u.msg).max().filter(|m| *m > since);
        for (_, u) in failing.into_iter().filter(|(b, u)| *b > 0 || Some(u.msg) == latest).take(3) {
            let code = u.text.rsplit_once("[exit code ").and_then(|(_, r)| r.split(']').next());
            let code = code.map(|c| format!("exit {c}")).unwrap_or_else(|| "failed".into());
            let mut out = format!("- `{}` ({code})", short(u.command.as_deref().unwrap_or(""), 120));
            for line in error_lines(&u.text) {
                out.push_str(&format!("\n    {line}"));
            }
            l.failing.push(out);
        }
        // the plan
        if let Some(items) = history
            .iter()
            .flat_map(|m| m.tool_calls())
            .filter(|(_, n, _)| *n == "todo")
            .filter_map(|(_, _, a)| a.get("items").and_then(Value::as_array))
            .last()
        {
            for it in items.iter().take(12) {
                let text = it.get("text").and_then(Value::as_str).unwrap_or("");
                let status = it.get("status").and_then(Value::as_str).unwrap_or("pending");
                let mark = match status {
                    "done" | "completed" => "x",
                    "in_progress" => "~",
                    _ => " ",
                };
                if !text.is_empty() {
                    l.plan.push(format!("- [{mark}] {}", short(text, 120)));
                }
            }
        }
        l
    }

    /// The ledger within `budget` tokens, and how many facts it holds.
    fn render(&self, budget: usize) -> (String, usize) {
        let mut out = String::new();
        let mut facts = 0;
        let fits = |out: &str, add: &str| tokens(out) + tokens(add) <= budget;
        if let Some(r) = &self.request {
            let line = format!("The person's latest request: {r}\n");
            if fits(&out, &line) {
                out.push_str(&line);
            }
        }
        let sections: [(&str, &Vec<String>); 4] = [
            ("Constraints the person gave earlier:", &self.constraints),
            ("Files (latest first):", &self.files),
            ("Failing commands:", &self.failing),
            ("Plan:", &self.plan),
        ];
        for (title, items) in sections {
            let mut body = String::new();
            for it in items {
                let it = if title.starts_with("Constraints") { format!("- {it}") } else { it.clone() };
                let add = format!("{it}\n");
                if !fits(&format!("{out}{title}\n{body}"), &add) {
                    break;
                }
                body.push_str(&add);
                facts += 1;
            }
            if !body.is_empty() {
                out.push_str(&format!("{title}\n{body}"));
            }
        }
        (out, facts)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn read(id: &str, path: &str, from: usize, lines: &[&str]) -> [Message; 2] {
        let text: String =
            lines.iter().enumerate().map(|(i, l)| format!("{:>6}│ {l}\n", from + i)).collect::<Vec<_>>().concat();
        [
            Message::new(Role::Assistant, vec![Part::tool_call(id, "read", json!({"path": path}))]),
            Message::tool_results(vec![Part::tool_result(id, text, false)]),
        ]
    }

    fn bash(id: &str, cmd: &str, out: &str, err: bool) -> [Message; 2] {
        [
            Message::new(Role::Assistant, vec![Part::tool_call(id, "bash", json!({"command": cmd}))]),
            Message::tool_results(vec![Part::tool_result(id, out, err)]),
        ]
    }

    /// A recorded session: the person asks, the agent reads two files, runs the
    /// tests (they fail), edits, plans, then the person moves on.
    fn session(dir: &Path) -> (Vec<String>, Vec<String>, Vec<Message>) {
        let parser: Vec<String> =
            (0..60).map(|i| format!("fn parse_line_{i}(s: &str) -> Token {{ lex(s, {i}) }}")).collect();
        let config: Vec<String> = (0..40).map(|i| format!("pub retry_limit_{i}: u32,")).collect();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/parser.rs"), parser.join("\n")).unwrap();
        std::fs::write(dir.join("src/config.rs"), config.join("\n")).unwrap();
        let p: Vec<&str> = parser.iter().map(String::as_str).collect();
        let c: Vec<&str> = config.iter().map(String::as_str).collect();
        let mut h = vec![Message::user(
            "Look at the parser in src/parser.rs and the config in src/config.rs. Never touch Cargo.lock.",
        )];
        h.extend(read("r1", "src/parser.rs", 1, &p));
        h.extend(read("r2", "src/config.rs", 1, &c));
        h.extend(bash(
            "b1",
            "cargo test parser",
            "running 3 tests\ntest parser::tests::parse_line_7 ... FAILED\nassertion failed: lex(s, 7) == Token::Num\n\n[exit code 101]",
            true,
        ));
        h.extend(bash("b2", "cargo build", "error[E0425]: cannot find value `x`\n\n[exit code 1]", true));
        h.extend(bash("b3", "cargo build", "Finished", false));
        h.push(Message::new(
            Role::Assistant,
            vec![
                Part::text("parse_line_7 fails: lex returns the wrong token."),
                Part::tool_call("t1", "todo", json!({"items": [{"text": "fix parse_line_7", "status": "in_progress"}, {"text": "run the tests", "status": "pending"}]})),
            ],
        ));
        h.push(Message::tool_results(vec![Part::tool_result("t1", "ok", false)]));
        h.push(Message::user("Now something else: summarize the config."));
        h.push(Message::assistant("The config holds forty retry limits."));
        (parser, config, h)
    }

    #[test]
    fn lexical_picks_the_passages_of_the_task_within_its_budget() {
        let d = tempfile::tempdir().unwrap();
        let (_, _, h) = session(d.path());
        let spans = lexical("Fix parse_line_7 in src/parser.rs so the parser test passes", &h, 600);
        let all: String = spans.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n");
        assert!(all.contains("fn parse_line_7("), "{all}");
        assert!(all.contains("FAILED"), "{all}");
        assert!(!all.contains("retry_limit_"), "{all}");
        assert!(spans.iter().map(|s| s.tokens).sum::<usize>() <= 600);
        let order: Vec<usize> = spans.iter().map(|s| s.msg).collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted);
        // a long read keeps the lines around the task's words
        let read = spans.iter().find(|s| s.label == "read src/parser.rs").unwrap();
        assert!(read.text.lines().count() < 15, "{}", read.text);
        // deterministic
        assert_eq!(spans, lexical("Fix parse_line_7 in src/parser.rs so the parser test passes", &h, 600));
        assert!(lexical("zzz qqq", &h, 600).is_empty());
    }

    #[test]
    fn recall_brings_the_notes_and_what_bears_on_the_focus_within_its_budget() {
        let d = tempfile::tempdir().unwrap();
        let (_, _, mut h) = session(d.path());
        h.push(Message::assistant("Finding: parse_line_7 skips the last field.\nnot a note"));
        h.push(Message::assistant("Finding: parse_line_7 skips the last field."));
        let (text, n) =
            recall("s-1", "the \"parser\" fix", "parse_line_7 in src/parser.rs", &h, d.path(), 1200).unwrap();
        assert!(text.starts_with("<memory from=\"s-1\" title=\"the  parser  fix\">"), "{text}");
        assert_eq!(text.matches("Finding: parse_line_7").count(), 1, "one copy of a repeated note: {text}");
        let notes = text.split("Notes its agent wrote:").nth(1).unwrap_or("").split("\n\n").next().unwrap_or("");
        assert!(notes.lines().take_while(|l| l.starts_with("- ")).all(|l| !l.contains("not a note")), "{text}");
        assert!(text.contains("fn parse_line_7("), "{text}");
        assert!(!text.contains("retry_limit_"), "what does not bear on the focus stays out: {text}");
        assert!(n <= 1300 && text.ends_with("</memory>"));
        // no focus: the title leads
        assert!(recall("s-1", "parser", "", &h, d.path(), 1200).is_some());
        assert!(recall("s-1", "t", "x", &[], d.path(), 1200).is_none());
    }

    #[test]
    fn lexical_leaves_out_reads_a_later_write_made_stale() {
        let d = tempfile::tempdir().unwrap();
        let (_, _, mut h) = session(d.path());
        h.truncate(3);
        h.push(Message::new(
            Role::Assistant,
            vec![Part::tool_call("w1", "write", json!({"path": "src/parser.rs", "content": "fn new_parser() {}"}))],
        ));
        h.push(Message::tool_results(vec![Part::tool_result("w1", "wrote src/parser.rs", false)]));
        let spans = lexical("Fix parse_line_7 in src/parser.rs", &h, 2000);
        assert!(!spans.iter().any(|s| s.text.contains("fn parse_line_7(")), "{spans:?}");
    }

    #[test]
    fn a_failed_edit_does_not_make_a_read_stale() {
        let d = tempfile::tempdir().unwrap();
        let (_, _, mut h) = session(d.path());
        h.truncate(3);
        let edit = |ok: bool| {
            vec![
                Message::new(
                    Role::Assistant,
                    vec![Part::tool_call(
                        "e1",
                        "edit",
                        json!({"path": "src/parser.rs", "old_string": "fn parse_line_7", "new_string": "fn parse_line_seven_new"}),
                    )],
                ),
                Message::tool_results(vec![Part::tool_result(
                    "e1",
                    if ok { "edited src/parser.rs" } else { "edit: `old_string` not found in src/parser.rs" },
                    !ok,
                )]),
            ]
        };
        let task = "Fix parse_line_7 in src/parser.rs";
        let mut failed = h.clone();
        failed.extend(edit(false));
        let mut done = h;
        done.extend(edit(true));
        let has_read = |spans: &[Span]| spans.iter().any(|s| s.text.contains("fn parse_line_7("));
        assert!(has_read(&lexical(task, &failed, 2000)));
        assert!(!has_read(&lexical(task, &done, 2000)));
        assert!(has_read(&curator(task, &failed, 2000)));
        assert!(!has_read(&curator(task, &done, 2000)));
        let l = Ledger::of(&failed, d.path(), true, task);
        assert!(l.files.iter().any(|f| f == "- src/parser.rs: read lines 1-60 (current)"), "{:?}", l.files);
        let l = Ledger::of(&done, d.path(), true, task);
        assert!(l.files.iter().any(|f| f.contains("(changed after)") && f.contains("edited 1x")), "{:?}", l.files);
    }

    #[test]
    fn a_read_stub_is_never_an_excerpt() {
        let d = tempfile::tempdir().unwrap();
        let (parser, _, mut h) = session(d.path());
        h.truncate(3);
        let stub = "src/parser.rs: unchanged since shown above (60 lines); not repeated: lines 1-60 are inside your \
                    earlier output of lines 1-60 above. If that is no longer in the conversation, ask again and you \
                    get it in full. parse_line_7";
        assert!(crate::reads::is_unchanged_note(stub));
        h.extend([
            Message::new(Role::Assistant, vec![Part::tool_call("r9", "read", json!({"path": "src/parser.rs"}))]),
            Message::tool_results(vec![Part::tool_result("r9", stub, false)]),
        ]);
        let task = "Fix parse_line_7 in src/parser.rs";
        let line = parser[7].as_str();
        let join = |spans: &[Span]| spans.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n");
        for (name, spans) in [("lexical", lexical(task, &h, 2000)), ("curator", curator(task, &h, 2000))] {
            let all = join(&spans);
            assert!(!all.contains("unchanged since shown above"), "{name}: {all}");
            assert!(all.contains(line), "{name}: {all}");
        }
        let l = Ledger::of(&h, d.path(), true, task);
        assert!(!format!("{l:?}").contains("unchanged since"), "{l:?}");
        assert!(l.files.iter().any(|f| f.starts_with("- src/parser.rs: read lines 1-60")), "{:?}", l.files);
    }

    #[test]
    fn a_read_of_several_files_is_listed_in_the_ledger_and_not_made_stale_by_an_edit_of_one() {
        let d = tempfile::tempdir().unwrap();
        let h = vec![
            Message::user("Compare parse_line_7 in src/a.rs with src/b.rs"),
            Message::new(
                Role::Assistant,
                vec![Part::tool_call("m", "read", json!({"paths": ["src/a.rs", "src/b.rs"]}))],
            ),
            Message::tool_results(vec![Part::tool_result(
                "m",
                "=== src/a.rs ===\n     1│ fn parse_line_7() {}\n=== src/b.rs ===\n     1│ fn other_b() {}\n",
                false,
            )]),
            Message::new(
                Role::Assistant,
                vec![Part::tool_call("e", "edit", json!({"path": "src/a.rs", "old_string": "x", "new_string": "y"}))],
            ),
            Message::tool_results(vec![Part::tool_result("e", "edited src/a.rs", false)]),
        ];
        let l = Ledger::of(&h, d.path(), false, "Compare parse_line_7 in src/a.rs with src/b.rs");
        assert!(l.files.iter().any(|f| f.starts_with("- src/b.rs: read")), "{:?}", l.files);
        assert!(
            l.files.iter().any(|f| f.starts_with("- src/a.rs:") && f.contains("read") && f.contains("edited")),
            "{:?}",
            l.files
        );
        // the result is no one file's, so the edit of a.rs does not drop it as stale
        let spans = lexical("Compare parse_line_7 in src/a.rs with src/b.rs", &h, 2000);
        assert!(spans.iter().any(|s| s.text.contains("other_b")), "{spans:?}");
    }

    #[test]
    fn the_ledger_holds_files_failures_constraints_and_the_plan() {
        let d = tempfile::tempdir().unwrap();
        let (_, config, h) = session(d.path());
        // the config changes on disk after it was read
        let mut changed = config.clone();
        changed[3] = "pub retry_limit_3: u64,".into();
        std::fs::write(d.path().join("src/config.rs"), changed.join("\n")).unwrap();
        let l = Ledger::of(&h, d.path(), true, "Fix parse_line_7 in src/parser.rs");
        assert_eq!(l.request.as_deref(), Some("Now something else: summarize the config."));
        assert!(l.constraints.iter().any(|c| c.contains("Never touch Cargo.lock")), "{:?}", l.constraints);
        assert!(l.files.iter().any(|f| f == "- src/parser.rs: read lines 1-60 (current)"), "{:?}", l.files);
        assert!(
            l.files.iter().any(|f| f == "- src/config.rs: read lines 1-40 (changed on disk since)"),
            "{:?}",
            l.files
        );
        // `cargo build` failed, then passed: only the test is still failing
        assert_eq!(l.failing.len(), 1, "{:?}", l.failing);
        assert!(
            l.failing[0].starts_with("- `cargo test parser` (exit 101)")
                && l.failing[0].contains("parse_line_7 ... FAILED")
        );
        assert_eq!(l.plan, vec!["- [~] fix parse_line_7".to_string(), "- [ ] run the tests".into()]);
        let (text, facts) = l.render(10_000);
        assert_eq!(facts, 1 + 2 + 1 + 2);
        assert!(text.contains("Failing commands:"));
        // a small budget keeps what fits
        let (small, few) = l.render(40);
        assert!(tokens(&small) <= 40 && few < facts, "{small}");
    }

    #[tokio::test]
    async fn a_lexical_brief_keeps_its_budget() {
        let d = tempfile::tempdir().unwrap();
        let (_, _, h) = session(d.path());
        let cfg = BriefConfig { tokens: 800, engine: Engine::Lexical, ..BriefConfig::default() };
        let b = build("Fix parse_line_7 in src/parser.rs", &h, d.path(), &cfg).await.unwrap();
        assert_eq!(b.engine, "lexical");
        assert!(b.tokens <= 800, "{}", b.tokens);
        assert!(b.text.starts_with("<briefing>") && b.text.ends_with("</briefing>"));
        assert!(b.text.contains("--- read src/parser.rs lines "), "{}", b.text);
        assert!(b.text.contains("(current) ---"), "{}", b.text);
        assert!(b.line().starts_with("briefed: ") && b.line().ends_with("· lexical"), "{}", b.line());
        let off = BriefConfig { tokens: 0, ..cfg.clone() };
        assert!(build("Fix parse_line_7", &h, d.path(), &off).await.is_none());
        assert!(build("Fix parse_line_7", &[], d.path(), &cfg).await.is_none());
    }

    #[tokio::test]
    async fn a_brief_keeps_its_budget_with_many_labels_and_text_of_several_bytes_a_letter() {
        let d = tempfile::tempdir().unwrap();
        let mut h = vec![Message::user("Исправь разбор_строки во всех модулях: проверка_значения падает.")];
        for i in 0..24 {
            let lines: Vec<String> = (0..30)
                .map(|k| format!("функция разбор_строки_{k} проверка_значения модуль {i} строка {k} без изменений"))
                .collect();
            let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
            let path = format!("src/очень_длинный_модуль_номер_{i}/подмодуль_разбора/файл_разбора_строки_{i}.rs");
            h.extend(read(&format!("r{i}"), &path, 1, &lines));
        }
        let task = "Исправь разбор_строки_7 и проверка_значения";
        for engine in [Engine::Lexical, Engine::Curator] {
            for budget in [600, 1500, 3000] {
                let cfg = BriefConfig { tokens: budget, engine, check_disk: false };
                let b = build(task, &h, d.path(), &cfg).await.unwrap();
                assert!(b.spans > 0, "{}", b.text);
                assert!(b.tokens <= budget, "{engine:?} {budget}: {} tokens", b.tokens);
            }
        }
    }

    #[tokio::test]
    async fn a_brief_comes_from_the_curator_by_default() {
        let d = tempfile::tempdir().unwrap();
        let (_, _, h) = session(d.path());
        for engine in [Engine::Auto, Engine::Curator] {
            let cfg = BriefConfig { tokens: 1500, engine, ..BriefConfig::default() };
            let b = build("Fix parse_line_7 in src/parser.rs", &h, d.path(), &cfg).await.unwrap();
            assert_eq!(b.engine, "curator", "{}", b.text);
            assert!(b.tokens <= 1500, "{}", b.tokens);
            assert!(b.text.contains("parse_line_7"), "{}", b.text);
            assert!(!b.text.contains("retry_limit_"), "{}", b.text);
        }
    }

    #[test]
    fn the_report_drops_what_the_briefing_holds_and_is_capped() {
        let brief = "<briefing>\n--- read src/parser.rs ---\n     8│ fn parse_line_7(s: &str) -> Token { lex(s, 7) }\n</briefing>";
        let report = "The bug is at src/parser.rs:8:\n     8│ fn parse_line_7(s: &str) -> Token { lex(s, 7) }\nIt should call lex_num.";
        let out = compact_report(report, Some(brief), REPORT_TOKENS);
        assert_eq!(out, "The bug is at src/parser.rs:8:\n(… 1 line you already have)\nIt should call lex_num.");
        let long = "a line of the report that goes on\n".repeat(400);
        let cut = compact_report(&long, None, 200);
        assert!(tokens(&cut) <= 220 && cut.ends_with("cut at about 200 tokens)"), "{}", tokens(&cut));
    }

    #[test]
    fn words_split_identifiers_and_keep_names() {
        let t = terms("Fix parseLine in src/parser.rs and retry_limit");
        for w in ["parse", "line", "src/parser.rs", "retry", "limit", "retry_limit", "parseLine"] {
            assert!(t.contains(w), "{w}: {t:?}");
        }
        assert!(!t.contains("and"));
        assert!(names("cfg.epoch_tokens json.dumps v2").contains("epoch_tokens"));
    }
}
