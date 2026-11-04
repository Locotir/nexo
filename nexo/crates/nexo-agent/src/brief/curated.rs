//! The curator's selection of a briefing.
//!
//! From the conversation that sends a sub-agent, the passages that bear on its
//! task, within a budget of tokens:
//!
//! - turns and threads: the task is taken in as a new request, and the link
//!   model (the curator's learnt model) gives, for every earlier turn, the
//!   probability that this request draws on it;
//! - paragraphs (tool outputs cut at blank lines, the person's and the agent's
//!   words) scored by the IDF-weighted words of the task they hold, times the
//!   relevance of their turn;
//! - file reads that a later write made stale are skipped;
//! - a long paragraph keeps only the lines around the task's words;
//! - a weighted coverage, picked greedily: a paragraph is worth the task words
//!   it adds that those taken before do not already cover, per square root of
//!   its tokens.
//!
//! `tests/brief_golden.rs` replays recorded selections and asserts the same
//! spans. Words, lengths and case follow the Unicode tables of
//! [`super::pystr`]; sums run in a fixed order, so scores agree with the
//! recorded ones to rounding.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde_json::{Map, Value};

use super::pystr as py;

/// A longer paragraph keeps only the lines around the task's words.
const LONG: usize = 14;
/// Lines kept on each side of a matching line.
const AROUND: usize = 2;
/// Share of the task's word weight a paragraph must hold (or one of its names).
const MIN_SHARE: f64 = 0.12;
const MAX_SPANS: usize = 24;
/// A paragraph of a tool output holds at most this many lines.
const MAX_CHUNK_LINES: usize = 30;
/// A request joins the thread of the turn it most probably draws on from this probability.
const LINK: f64 = 0.15;
const CARD_ASK: usize = 160;
/// Nominal tokens of an image or document block.
const MEDIA_TOKENS: usize = 1500;

/// One excerpt: `text` is lines `start..end` of block `block` of message `msg`
/// (trimmed around the task's words when long).
#[derive(Debug, Clone, PartialEq)]
pub struct CuratedSpan {
    pub msg: usize,
    pub block: usize,
    pub start: usize,
    pub end: usize,
    /// `read src/x.rs`, `bash cargo test`, `the person`, `the main agent`...
    pub label: String,
    pub text: String,
    pub score: f64,
    /// Characters / 4 + 1.
    pub tokens: usize,
}

/// An earlier request of the person that the task draws on.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub turn: usize,
    pub ask: String,
    pub relevance: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reply {
    /// In conversation order.
    pub spans: Vec<CuratedSpan>,
    pub asks: Vec<Ask>,
}

// ------------------------------------------------------------ words

const FILE_EXT: &[&str] = &[
    "py", "js", "mjs", "ts", "tsx", "jsx", "md", "json", "jsonl", "toml", "yaml", "yml", "txt", "cfg", "ini", "sh",
    "rs", "go", "java", "c", "h", "cc", "cpp", "hpp", "cs", "css", "scss", "html", "sql", "lock", "log", "csv", "xml",
    "env", "db", "rb", "php", "swift", "kt", "conf", "docx", "pdf", "lua", "vue", "svelte", "ipynb", "proto", "tf",
    "gradle", "mod", "sum", "zig", "ex", "exs", "dart",
];

const TLD: &[&str] = &[
    "com",
    "net",
    "org",
    "io",
    "dev",
    "ai",
    "co",
    "app",
    "cloud",
    "internal",
    "local",
    "localhost",
    "lan",
    "edu",
    "gov",
    "info",
    "xyz",
    "tech",
    "sh",
    "es",
    "uk",
    "de",
    "fr",
    "eu",
    "us",
    "corp",
    "test",
    "example",
];

const STOP: &str = "
the and for with that this from have has are was were not but you your can will would should could there their
they them then than what when where which while into onto about after before also just only some such each other
more most very here these those been being does did done its our out over under please now let use using need
needs want like make sure okay yes all any how why who
que los las del por con para una uno unos unas como pero más mas esto esta este eso esa ese hay muy son ser está
están estan fue han has hace hacer puede puedes tiene tienen todo toda todos todas cuando donde porque sobre entre
sin también tambien solo sólo nos les ella ellos algo bien así asi luego qué cómo mis tus sus
";

/// Names any model knows without reading them here (the curator's list).
const GENERAL: &str = nexo_curator::GENERAL_WORDS;
/// The curator's model; only its link head is used here.
const MODEL: &str = nexo_curator::MODEL_JSON;

fn stop() -> &'static HashSet<&'static str> {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| py::split_ws(STOP).collect())
}

fn general() -> &'static HashSet<&'static str> {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| py::split_ws(GENERAL).collect())
}

/// A set of interned words, sorted.
type Set = Vec<u32>;

fn has(s: &Set, w: u32) -> bool {
    s.binary_search(&w).is_ok()
}

fn union(sets: impl IntoIterator<Item = Set>) -> Set {
    let mut out: Set = sets.into_iter().flatten().collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Words, interned.
#[derive(Default)]
struct Lex {
    ids: HashMap<String, u32>,
    strs: Vec<String>,
}

impl Lex {
    fn id(&mut self, w: &str) -> u32 {
        if let Some(&k) = self.ids.get(w) {
            return k;
        }
        let k = self.strs.len() as u32;
        self.ids.insert(w.to_string(), k);
        self.strs.push(w.to_string());
        k
    }

    fn set<'a>(&mut self, ws: impl IntoIterator<Item = &'a str>) -> Set {
        let mut out: Set = ws.into_iter().map(|w| self.id(w)).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// `hindsight.words`.
    fn words(&mut self, text: &str) -> Set {
        let mut ws = vec![];
        words(text, &mut ws);
        self.set(ws)
    }

    /// (`learn.terms`, `hindsight.words`) of a text: the topic words (which hold the names) and the names.
    fn analyze(&mut self, text: &str) -> (Set, Set) {
        let mut ws = vec![];
        words(text, &mut ws);
        let names = self.set(ws.iter().copied());
        let mut out = names.clone();
        for tok in text.split(|c: char| !py::is_alnum(c)).filter(|t| !t.is_empty()) {
            for part in humps(tok) {
                if py::len(part) < 3 || part.chars().all(py::is_digit) {
                    continue;
                }
                let low = py::lower(part);
                if !stop().contains(low.as_str()) {
                    out.push(self.id(py::prefix(&low, 5)));
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        (out, names)
    }

    fn terms(&mut self, text: &str) -> Set {
        self.analyze(text).0
    }
}

/// A token cut where an ASCII lower-case letter meets an upper-case one (camelCase).
fn humps(tok: &str) -> Vec<&str> {
    let b = tok.as_bytes();
    let mut out = vec![];
    let mut start = 0;
    for k in 1..b.len() {
        if b[k - 1].is_ascii_lowercase() && b[k].is_ascii_uppercase() {
            out.push(&tok[start..k]);
            start = k;
        }
    }
    out.push(&tok[start..]);
    out
}

fn distinctive(w: &str) -> bool {
    let n = py::len(w);
    if n < 4 {
        return false;
    }
    if w.chars().all(py::is_digit) {
        // years are everywhere
        return !(n == 4 && py::int_value(w).is_some_and(|v| (1900..=2099).contains(&v)));
    }
    let tail = w.chars().skip(1);
    w.contains(['_', '$'])
        || w.chars().any(py::is_digit)
        || (tail.clone().any(py::changes_lower) && tail.clone().any(py::changes_upper))
}

/// What `[A-Za-z_$][\w$]*(?:[./-][\w$]+)*|\d+(?:\.\d+)+|\d{4,}` finds.
fn word_matches(text: &str) -> Vec<&str> {
    let cs: Vec<(usize, char)> = text.char_indices().collect();
    let n = cs.len();
    let at = |k: usize| if k < n { cs[k].0 } else { text.len() };
    let mut out = vec![];
    let mut i = 0;
    while i < n {
        let c = cs[i].1;
        let mut end = None;
        if c.is_ascii_alphabetic() || c == '_' || c == '$' {
            let mut k = i + 1;
            while k < n && py::is_word(cs[k].1) {
                k += 1;
            }
            while k + 1 < n && matches!(cs[k].1, '.' | '/' | '-') && py::is_word(cs[k + 1].1) {
                k += 2;
                while k < n && py::is_word(cs[k].1) {
                    k += 1;
                }
            }
            end = Some(k);
        } else if py::is_decimal(c) {
            let mut k = i + 1;
            while k < n && py::is_decimal(cs[k].1) {
                k += 1;
            }
            let run = k;
            while k + 1 < n && cs[k].1 == '.' && py::is_decimal(cs[k + 1].1) {
                k += 2;
                while k < n && py::is_decimal(cs[k].1) {
                    k += 1;
                }
            }
            if k > run {
                end = Some(k);
            } else if run - i >= 4 {
                end = Some(run);
            }
        }
        match end {
            Some(k) => {
                out.push(&text[cs[i].0..at(k)]);
                i = k;
            }
            None => i += 1,
        }
    }
    out
}

/// Distinctive tokens: paths, file names, versions, long numbers and code-like
/// identifiers (`_`, `$`, digits, camelCase), at least 4 characters; dotted
/// names split, names any model knows left out.
fn words<'a>(text: &'a str, out: &mut Vec<&'a str>) {
    let common = general();
    for w in word_matches(text) {
        if w.contains('/') || w.chars().next().is_some_and(py::is_digit) {
            let ok = py::len(w) >= 4 && (!w.chars().all(py::is_digit) || distinctive(w));
            if ok && !common.contains(w) {
                out.push(w);
            }
            continue;
        }
        let parts: Vec<&str> = w.split('.').collect();
        if parts.len() > 1 {
            let last = py::lower(parts[parts.len() - 1]);
            if FILE_EXT.contains(&last.as_str()) || (parts.len() > 2 && TLD.contains(&last.as_str())) {
                if py::len(w) >= 4 && !common.contains(w) {
                    out.push(w); // a file name or a host name
                }
                continue;
            }
        }
        for part in parts {
            if distinctive(part) && !common.contains(part) {
                out.push(part);
            }
        }
    }
}

fn is_request(text: &str) -> bool {
    const NOT: &[&str] = &["<system-reminder>", "<local-command", "<command-", "Caveat:", "[Request interrupted"];
    !py::strip(text).is_empty() && {
        let t = text.trim_start_matches(py::is_space);
        !NOT.iter().any(|p| t.starts_with(p))
    }
}

/// `"  12\t"`, `"12→"`, `"12: "`, `"12 | "`: the prefix file viewers put on a line.
fn strip_mechanical_lineno(line: &str) -> &str {
    let cs: Vec<(usize, char)> = line.char_indices().collect();
    let mut k = 0;
    while k < cs.len() && py::is_space(cs[k].1) {
        k += 1;
    }
    let d = k;
    while k < cs.len() && py::is_decimal(cs[k].1) {
        k += 1;
    }
    if k == d || k >= cs.len() {
        return line;
    }
    let c = |j: usize| cs.get(j).map(|x| x.1);
    let end = match cs[k].1 {
        '\t' | '→' => k + 1,
        ':' if c(k + 1).is_some_and(py::is_space) => k + 2,
        s if py::is_space(s) && c(k + 1) == Some('|') && c(k + 2).is_some_and(py::is_space) => k + 3,
        _ => return line,
    };
    match cs.get(end) {
        Some(&(b, _)) => &line[b..],
        None => "",
    }
}

/// `"     8│ "`, `"  8\t"`, `"8 | "`: a line-number prefix, as the briefing strips it to weigh a line.
fn strip_brief_lineno(line: &str) -> &str {
    let cs: Vec<(usize, char)> = line.char_indices().collect();
    let n = cs.len();
    let mut k = 0;
    while k < n && py::is_space(cs[k].1) {
        k += 1;
    }
    let d0 = k;
    while k < n && py::is_decimal(cs[k].1) {
        k += 1;
    }
    if k == d0 {
        return line;
    }
    let d = k;
    while k < n && py::is_space(cs[k].1) {
        k += 1;
    }
    let mut end = if k < n && matches!(cs[k].1, '│' | '|') {
        k + 1
    } else {
        match (d..k).rev().find(|&j| cs[j].1 == '\t') {
            Some(t) => t + 1,
            None => return line,
        }
    };
    if end < n && py::is_space(cs[end].1) {
        end += 1;
    }
    if end < n {
        &line[cs[end].0..]
    } else {
        ""
    }
}

/// Line ranges `[start, end)` cut at blank lines (line-number prefixes ignored).
fn split_paragraphs(text: &str) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = vec![];
    let mut start = 0;
    let mut count = 0;
    for (i, line) in text.split('\n').enumerate() {
        count = i + 1;
        let blank = py::strip(strip_mechanical_lineno(line)).is_empty();
        let n = i + 1 - start;
        if (blank && n >= 4) || n >= MAX_CHUNK_LINES {
            out.push((start, i + 1));
            start = i + 1;
        }
    }
    if start < count {
        match out.last_mut() {
            Some(last) if count - start < 3 => last.1 = count,
            _ => out.push((start, count)),
        }
    }
    out
}

fn is_error(text: &str) -> bool {
    py::contains_any_folded(
        text,
        &["error", "exception", "traceback", "failed", "failure", "fatal", "panic", "denied", "not found"],
    )
}

// ------------------------------------------------------------ tools

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolKind {
    Read,
    Write,
    Exec,
    Search,
    Other,
}

#[derive(Debug, Clone)]
struct Info {
    kind: ToolKind,
    path: Option<String>,
    command: Option<String>,
}

/// `tools.classify`: the kind of a tool by its name, its path and command by the usual argument keys.
fn classify(name: &str, args: Option<&Value>) -> Info {
    const KINDS: &[(ToolKind, &[&str])] = &[
        (
            ToolKind::Write,
            &["edit", "write", "create", "patch", "replace", "insert", "apply", "multiedit", "str_replace"],
        ),
        (ToolKind::Read, &["read", "view", "open", "cat", "get_file", "fetch_file"]),
        (ToolKind::Search, &["grep", "glob", "search", "find", "list", "ls"]),
        (ToolKind::Exec, &["bash", "shell", "exec", "run", "terminal", "command"]),
    ];
    let args = args.and_then(Value::as_object);
    let first = |keys: &[&str]| {
        let args = args?;
        keys.iter().find_map(|k| args.get(*k).and_then(Value::as_str)).filter(|s| !s.is_empty()).map(str::to_string)
    };
    let path = first(&["file_path", "path", "filepath", "filename", "target_file", "file", "notebook_path"]);
    let command = first(&["command", "cmd", "script"]);
    let mut kind = KINDS.iter().find(|(_, pats)| py::contains_any_folded(name, pats)).map_or(ToolKind::Other, |k| k.0);
    if kind == ToolKind::Other && command.is_some() {
        kind = ToolKind::Exec;
    }
    Info { kind, path, command }
}

// ------------------------------------------------------------ messages

fn has_cache_control(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.contains_key("cache_control") || m.values().any(has_cache_control),
        Value::Array(a) => a.iter().any(has_cache_control),
        _ => false,
    }
}

fn strip_cache_control(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter().filter(|(k, _)| *k != "cache_control").map(|(k, v)| (k.clone(), strip_cache_control(v))).collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(strip_cache_control).collect()),
        v => v.clone(),
    }
}

/// Content as a list of blocks (a plain string becomes one text block).
fn blocks(content: Option<&Value>) -> Vec<Cow<'_, Value>> {
    match content {
        Some(Value::String(s)) => vec![Cow::Owned(serde_json::json!({"type": "text", "text": s}))],
        Some(Value::Array(a)) => a.iter().map(Cow::Borrowed).collect(),
        _ => vec![],
    }
}

fn str_of<'a>(o: &'a Map<String, Value>, k: &str) -> &'a str {
    o.get(k).and_then(Value::as_str).unwrap_or("")
}

fn type_of(v: &Value) -> Option<&str> {
    v.get("type").and_then(Value::as_str)
}

/// The text of a content value (string, block list, nested tool results).
fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => {
            let mut out: Vec<Cow<str>> = vec![];
            for b in a.iter().filter_map(Value::as_object) {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => out.push(Cow::Borrowed(str_of(b, "text"))),
                    Some("tool_result") => out.push(Cow::Owned(text_of(b.get("content")))),
                    Some(t @ ("image" | "document")) => out.push(Cow::Owned(format!("[{t}]"))),
                    _ => {}
                }
            }
            out.join("\n")
        }
        _ => String::new(),
    }
}

const SKIP_KEYS: &[&str] =
    &["type", "id", "tool_use_id", "signature", "data", "cache_control", "source", "is_error", "citations"];

fn strings<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Object(m) => {
            for (k, v) in m {
                if !SKIP_KEYS.contains(&k.as_str()) {
                    strings(v, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
        _ => {}
    }
}

/// All the text of a value, without ids, signatures and other metadata.
fn flat(v: Option<&Value>) -> String {
    let mut out = vec![];
    if let Some(v) = v {
        strings(v, &mut out);
    }
    out.join("\n")
}

// ------------------------------------------------------------ token estimate

/// Length of a string in canonical JSON (non-ASCII kept as is).
fn json_str_len(s: &str) -> usize {
    2 + s
        .chars()
        .map(|c| match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            c if (c as u32) < 0x20 => 6,
            _ => 1,
        })
        .sum::<usize>()
}

fn json_obj_len(entries: impl Iterator<Item = (usize, usize)>) -> usize {
    let (mut n, mut len) = (0usize, 2);
    for (k, v) in entries {
        len += k + 1 + v;
        n += 1;
    }
    len + n.saturating_sub(1)
}

/// Length of a value in canonical JSON (sorted keys, no spaces), `cache_control` left out.
fn json_len(v: &Value) -> usize {
    match v {
        Value::Null => 4,
        Value::Bool(b) => {
            if *b {
                4
            } else {
                5
            }
        }
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string().len(),
            (_, Some(u)) => u.to_string().len(),
            _ => py::float_repr(n.as_f64().unwrap_or(0.0)).len(),
        },
        Value::String(s) => json_str_len(s),
        Value::Array(a) => 2 + a.iter().map(json_len).sum::<usize>() + a.len().saturating_sub(1),
        Value::Object(m) => {
            json_obj_len(m.iter().filter(|(k, _)| *k != "cache_control").map(|(k, v)| (json_str_len(k), json_len(v))))
        }
    }
}

/// `json_len` of the value without base64 payloads, signatures and reasoning text; and its media blocks.
fn stripped_len(v: &Value, media: &mut usize) -> usize {
    match v {
        Value::Object(m) => {
            let ty = m.get("type").and_then(Value::as_str);
            if let (Some("image" | "document"), Some(Value::Object(src))) = (ty, m.get("source")) {
                *media += 1;
                let source = json_obj_len(
                    src.iter()
                        .filter(|(k, _)| *k != "data" && *k != "cache_control")
                        .map(|(k, v)| (json_str_len(k), json_len(v))),
                );
                return json_obj_len(
                    m.iter()
                        .filter(|(k, _)| *k != "cache_control")
                        .map(|(k, v)| (json_str_len(k), if k == "source" { source } else { json_len(v) })),
                );
            }
            if let Some(t @ ("thinking" | "redacted_thinking")) = ty {
                return json_obj_len(std::iter::once((json_str_len("type"), json_str_len(t))));
            }
            json_obj_len(
                m.iter()
                    .filter(|(k, _)| *k != "signature" && *k != "cache_control")
                    .map(|(k, v)| (json_str_len(k), stripped_len(v, media)))
                    .collect::<Vec<_>>()
                    .into_iter(),
            )
        }
        Value::Array(a) => 2 + a.iter().map(|v| stripped_len(v, media)).sum::<usize>() + a.len().saturating_sub(1),
        v => json_len(v),
    }
}

/// Estimated tokens of a block (~4 characters each, images and documents at a nominal size).
fn est(b: &Value) -> usize {
    let mut media = 0;
    let len = stripped_len(b, &mut media);
    len / 4 + MEDIA_TOKENS * media
}

// ------------------------------------------------------------ the trace

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    User,
    Text,
    Thinking,
    Call,
    Result,
}

struct Chunk {
    /// Line range in the unit's text; the whole unit when None.
    lines: Option<(usize, usize)>,
    words: Set,
    terms: Set,
    error: bool,
}

struct Unit {
    msg: usize,
    block: usize,
    kind: Kind,
    tool: String,
    tokens: usize,
    words: Set,
    text: String,
    info: Option<Info>,
    chunks: Vec<Chunk>,
    terms: Set,
}

/// The key a tool id has in the map of tool calls (its JSON text).
fn key(v: Option<&Value>, missing: &Value) -> String {
    v.unwrap_or(missing).to_string()
}

struct Trace<'a> {
    msgs: Vec<Cow<'a, Value>>,
    units: Vec<Unit>,
    /// Message -> its units `[lo, hi)`.
    by_msg: Vec<(usize, usize)>,
    /// Path -> messages that wrote it.
    writes: HashMap<String, Vec<usize>>,
}

impl<'a> Trace<'a> {
    fn new(msgs: Vec<Cow<'a, Value>>, lex: &mut Lex) -> Trace<'a> {
        let mut units = vec![];
        let mut by_msg = vec![];
        let mut tool_names: HashMap<String, String> = HashMap::new();
        let mut calls: HashMap<String, Info> = HashMap::new();
        let mut writes: HashMap<String, Vec<usize>> = HashMap::new();
        let mut write_calls: Vec<(String, String, usize)> = vec![];
        let mut failed: std::collections::HashSet<String> = std::collections::HashSet::new();
        let (null, empty) = (Value::Null, Value::String(String::new()));
        for (mi, m) in msgs.iter().enumerate() {
            let lo = units.len();
            let user = m.get("role").and_then(Value::as_str) == Some("user");
            for (bi, b) in blocks(m.get("content")).iter().enumerate() {
                let Some(o) = b.as_object() else { continue };
                let ty = o.get("type").and_then(Value::as_str);
                let (kind, tool, text, said) = match (user, ty) {
                    (true, Some("text")) => (Kind::User, String::new(), None, str_of(o, "text")),
                    (true, Some("tool_result")) => {
                        let tool = tool_names.get(&key(o.get("tool_use_id"), &null)).cloned();
                        (Kind::Result, tool.unwrap_or_else(|| "?".into()), Some(text_of(o.get("content"))), "")
                    }
                    (false, Some("text")) => (Kind::Text, String::new(), None, str_of(o, "text")),
                    (false, Some("thinking")) => (Kind::Thinking, String::new(), None, str_of(o, "thinking")),
                    (false, Some("tool_use")) => {
                        let name = str_of(o, "name").to_string();
                        tool_names.insert(key(o.get("id"), &null), name.clone());
                        (Kind::Call, name, Some(flat(o.get("input"))), "")
                    }
                    _ => continue,
                };
                // the text the unit stands for; the person's and the agent's words fall back on the reasoning
                let text = text.unwrap_or_else(|| {
                    [str_of(o, "text"), str_of(o, "thinking")].into_iter().find(|s| !s.is_empty()).unwrap_or("").into()
                });
                let info = match kind {
                    Kind::Call => {
                        let info = classify(str_of(o, "name"), o.get("input"));
                        let id = key(o.get("id"), &empty);
                        if let (Some(p), ToolKind::Write) = (&info.path, info.kind) {
                            write_calls.push((id.clone(), p.clone(), mi));
                        }
                        calls.insert(id, info.clone());
                        Some(info)
                    }
                    Kind::Result => {
                        let id = key(o.get("tool_use_id"), &empty);
                        if o.get("is_error").and_then(Value::as_bool) == Some(true) {
                            failed.insert(id.clone());
                        }
                        calls.get(&id).cloned()
                    }
                    _ => None,
                };
                let (chunks, words, terms) = if kind == Kind::Result {
                    let lines: Vec<&str> = text.split('\n').collect();
                    let chunks: Vec<Chunk> = split_paragraphs(&text)
                        .into_iter()
                        .map(|(a, b)| {
                            let part = lines[a..b].join("\n");
                            let (terms, words) = lex.analyze(&part);
                            Chunk { lines: Some((a, b)), words, terms, error: is_error(&part) }
                        })
                        .collect();
                    let terms = union(chunks.iter().map(|c| c.terms.clone()));
                    (chunks, lex.words(&text), terms)
                } else {
                    let (terms, names) = lex.analyze(&text);
                    let words = if kind == Kind::Call || said == text { names } else { lex.words(said) };
                    let chunk = Chunk { lines: None, words: words.clone(), terms: terms.clone(), error: false };
                    (vec![chunk], words, terms)
                };
                units.push(Unit { msg: mi, block: bi, kind, tool, tokens: est(b), words, text, info, chunks, terms });
            }
            by_msg.push((lo, units.len()));
        }
        // a write counts at its call unless its result says it failed (one still running counts)
        for (id, p, mi) in write_calls {
            if !failed.contains(&id) {
                writes.entry(p).or_default().push(mi);
            }
        }
        Trace { msgs, units, by_msg, writes }
    }

    fn role(&self, mi: usize) -> Option<&str> {
        self.msgs[mi].get("role").and_then(Value::as_str)
    }

    fn requests(&self, mi: usize) -> impl Iterator<Item = usize> + '_ {
        let (lo, hi) = self.by_msg[mi];
        (lo..hi).filter(|&i| self.units[i].kind == Kind::User && is_request(&self.units[i].text))
    }

    /// A user message that carries a request of the person and no tool results.
    fn starts_turn(&self, mi: usize) -> bool {
        if mi == 0 {
            return true;
        }
        if self.role(mi) != Some("user") {
            return false;
        }
        let (lo, hi) = self.by_msg[mi];
        !(lo..hi).any(|i| self.units[i].kind == Kind::Result) && self.requests(mi).next().is_some()
    }

    /// What a span is: `the person`, `the main agent`, `read src/x.rs`, `bash cargo test`...
    fn label(&self, i: usize) -> String {
        let u = &self.units[i];
        match u.kind {
            Kind::User => return "the person".into(),
            Kind::Text => return "the main agent".into(),
            _ => {}
        }
        let path = u.info.as_ref().and_then(|x| x.path.as_deref());
        if u.kind == Kind::Call {
            return py::strip(&format!("{} {}", u.tool, path.unwrap_or(""))).to_string();
        }
        let mut cmd = String::new();
        if u.msg > 0 {
            let blocks_here = blocks(self.msgs[u.msg].get("content"));
            let id = blocks_here[u.block].get("tool_use_id").unwrap_or(&Value::Null);
            for c in blocks(self.msgs[u.msg - 1].get("content")) {
                if type_of(&c) == Some("tool_use") && c.get("id").unwrap_or(&Value::Null) == id {
                    let input = c.get("input").filter(|v| !is_falsy(v));
                    let name = c.get("name").and_then(Value::as_str).unwrap_or("");
                    cmd = classify(name, input).command.unwrap_or_default();
                }
            }
        }
        let what = match path {
            Some(p) => p,
            None => py::prefix(&cmd, 80),
        };
        py::strip(&format!("{} {what}", u.tool)).to_string()
    }
}

fn is_falsy(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Object(m) => m.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::String(s) => s.is_empty(),
        Value::Number(n) => n.as_f64() == Some(0.0),
    }
}

// ------------------------------------------------------------ turns and threads

/// An additive logistic model: each feature is binned and every bin has a weight.
struct Head {
    edges: Vec<Vec<f64>>,
    w: Vec<Vec<f64>>,
    b: f64,
}

/// The link head (trained balanced, so corrected by the share of links it was trained on).
struct LinkModel {
    head: Head,
    share: f64,
}

const LINK_FEATURES: [&str; 13] = [
    "share",
    "overlap",
    "rarest",
    "asked",
    "names",
    "prev",
    "active",
    "since",
    "novelty",
    "words",
    "short_prev",
    "size",
    "anchor",
];

fn link_model() -> Option<&'static LinkModel> {
    static M: OnceLock<Option<LinkModel>> = OnceLock::new();
    M.get_or_init(|| {
        let d: Value = serde_json::from_str(MODEL).ok()?;
        let names: Vec<&str> = d.get("link_features")?.as_array()?.iter().filter_map(Value::as_str).collect();
        if names != LINK_FEATURES {
            return None;
        }
        let link = d.get("link")?;
        let floats = |v: &Value| -> Option<Vec<Vec<f64>>> {
            v.as_array()?.iter().map(|r| r.as_array()?.iter().map(Value::as_f64).collect()).collect()
        };
        let head = Head { edges: floats(link.get("edges")?)?, w: floats(link.get("w")?)?, b: link.get("b")?.as_f64()? };
        if head.edges.len() != LINK_FEATURES.len() || head.w.len() != LINK_FEATURES.len() {
            return None;
        }
        let share = d.get("trained_on").and_then(|t| t.get("link_share")).and_then(Value::as_f64).unwrap_or(0.0);
        Some(LinkModel { head, share: if share != 0.0 { share } else { 0.5 } })
    })
    .as_ref()
}

fn sigmoid(z: f64) -> f64 {
    1.0 / (1.0 + (-z.clamp(-30.0, 30.0)).exp())
}

fn link_prob(x: &[f64; 13]) -> f64 {
    match link_model() {
        Some(m) => {
            let mut s = 0.0;
            for ((edges, w), &v) in m.head.edges.iter().zip(&m.head.w).zip(x) {
                s += w.get(edges.partition_point(|&e| e <= v)).copied().unwrap_or(0.0);
            }
            let (p, pi) = (sigmoid(m.head.b + s), m.share);
            p * pi / (p * pi + (1.0 - p) * (1.0 - pi))
        }
        None => {
            // the curator's stand-in for a trained head
            const W: [f64; 13] = [7.0, 0.0, 0.0, 1.5, 4.0, 1.5, 0.6, -0.4, -6.0, 0.0, 6.0, 0.0, -1.0];
            let mut s = 0.0;
            for (w, v) in W.iter().zip(x) {
                s += w * v;
            }
            sigmoid(-2.0 + s)
        }
    }
}

struct Turn {
    start: usize,
    request: Set,
    names: Set,
    terms: HashSet<u32>,
    seen: HashSet<u32>,
    tokens: usize,
    thread: usize,
}

/// Turns and threads, built message by message.
#[derive(Default)]
struct Threads {
    turns: Vec<Turn>,
    /// Thread -> its latest turn.
    last: Vec<usize>,
    df: HashMap<u32, usize>,
    n: usize,
    names: HashSet<u32>,
    /// IDF of the words of the latest request, as it opened its turn.
    weights: Vec<(u32, f64)>,
}

impl Threads {
    fn build(tr: &Trace) -> Threads {
        let mut th = Threads::default();
        for mi in 0..tr.msgs.len() {
            if tr.starts_turn(mi) {
                th.open(tr, mi);
            }
            let (lo, hi) = tr.by_msg[mi];
            let Some(t) = th.turns.last_mut() else { continue };
            for u in &tr.units[lo..hi] {
                t.terms.extend(&u.terms);
                t.seen.extend(&u.words);
                t.tokens += u.tokens;
                for &w in &u.terms {
                    *th.df.entry(w).or_default() += 1;
                }
                th.names.extend(&u.words);
            }
            th.n += hi - lo;
        }
        th
    }

    fn open(&mut self, tr: &Trace, mi: usize) {
        let asked: Vec<usize> = tr.requests(mi).collect();
        let req = union(asked.iter().map(|&i| tr.units[i].terms.clone()));
        let names = union(asked.iter().map(|&i| tr.units[i].words.clone()));
        let n = self.turns.len();
        let mut turn =
            Turn { start: mi, request: req, names, terms: HashSet::new(), seen: HashSet::new(), tokens: 0, thread: 0 };
        if n == 0 {
            self.turns.push(turn);
            self.last.push(0);
            return;
        }
        self.weights = turn.request.iter().map(|&w| (w, self.idf(w))).collect();
        let mut joint = vec![0.0f64; self.last.len()];
        for j in 0..n {
            let p = link_prob(&self.features(&turn.request, &turn.names, &self.weights, j, n));
            let k = self.turns[j].thread;
            joint[k] = joint[k].max(p);
        }
        let mut order: Vec<usize> = (0..joint.len()).collect();
        order.sort_by(|&a, &b| joint[b].total_cmp(&joint[a]).then(self.last[b].cmp(&self.last[a])));
        let best = order[0];
        let th = if joint[best] >= LINK { best } else { self.last.len() };
        if th == self.last.len() {
            self.last.push(n);
        }
        self.last[th] = n;
        turn.thread = th;
        self.turns.push(turn);
    }

    fn idf(&self, w: u32) -> f64 {
        (1.0 + self.n as f64 / (1 + self.df.get(&w).copied().unwrap_or(0)) as f64).ln()
    }

    /// What is known, when a request arrives, about whether its turn (number `n`) draws on turn `j`.
    fn features(&self, req: &Set, names: &Set, weights: &[(u32, f64)], j: usize, n: usize) -> [f64; 13] {
        let t = &self.turns[j];
        let total = weights.iter().map(|x| x.1).sum::<f64>();
        let total = if total == 0.0 { 1.0 } else { total };
        let (mut s, mut asked, mut top) = (0.0, 0.0, 0.0f64);
        for &(w, x) in weights {
            if t.terms.contains(&w) {
                s += x;
                top = top.max(x);
            }
            if has(&t.request, w) {
                asked += x;
            }
        }
        let prev = if j + 1 == n { 1.0 } else { 0.0 };
        let seen = names.iter().filter(|w| t.seen.contains(w)).count();
        let new = names.iter().filter(|w| !self.names.contains(w)).count();
        [
            s / total,
            s.ln_1p(),
            top,
            asked / total,
            if names.is_empty() { 0.0 } else { seen as f64 / names.len() as f64 },
            prev,
            if t.thread == self.turns[n - 1].thread { 1.0 } else { 0.0 },
            ((n - 1 - j) as f64).ln_1p(),
            new as f64 / (names.len() + 2) as f64,
            (req.len() as f64).ln_1p(),
            prev / (1 + req.len()) as f64,
            (t.tokens as f64).ln_1p(),
            if j == 0 { 1.0 } else { 0.0 },
        ]
    }

    /// Probability that the turn in progress (the last one) draws on turn `j`.
    fn relevance(&self, j: usize) -> f64 {
        let n = self.turns.len() - 1;
        let cur = &self.turns[n];
        link_prob(&self.features(&cur.request, &cur.names, &self.weights, j, n))
    }

    /// Messages of turn `j`: `[start, end)`.
    fn span(&self, j: usize, msgs: usize) -> (usize, usize) {
        let end = self.turns.get(j + 1).map_or(msgs, |t| t.start);
        (self.turns[j].start, end)
    }

    /// What the person asked in turn `j`, on one line.
    fn ask(&self, tr: &Trace, j: usize) -> String {
        let text: Vec<&str> = tr.requests(self.turns[j].start).map(|i| tr.units[i].text.as_str()).collect();
        one_line(&text.join(" "), CARD_ASK)
    }
}

fn one_line(text: &str, n: usize) -> String {
    let x = py::split_ws(text).collect::<Vec<_>>().join(" ");
    if py::len(&x) <= n {
        x
    } else {
        format!("{}…", py::prefix(&x, n - 1))
    }
}

// ------------------------------------------------------------ the selection

/// The lines around those holding most of the task's word weight (at least
/// half of the best line's); gaps marked with an ellipsis.
fn trim<'t>(lines: &[&'t str], idf: &[(u32, f64)], lex: &mut Lex) -> Vec<&'t str> {
    let weight: Vec<f64> = lines
        .iter()
        .map(|line| {
            let ts = lex.terms(strip_brief_lineno(line));
            idf.iter().filter(|(w, _)| has(&ts, *w)).map(|x| x.1).sum()
        })
        .collect();
    let top = weight.iter().copied().fold(0.0, f64::max);
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
    let mut keep: Vec<usize> =
        hit.iter().flat_map(|&k| k.saturating_sub(AROUND)..(k + AROUND + 1).min(lines.len())).collect();
    keep.sort_unstable();
    keep.dedup();
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

struct Scored {
    unit: usize,
    msg: usize,
    block: usize,
    start: usize,
    end: usize,
    hold: Set,
    factor: f64,
    text: String,
    tokens: usize,
}

/// The spans of `msgs` (Messages API shape: `{role, content}`) that bear on
/// `task`, within `budget` tokens, and the earlier requests it draws on.
pub fn select(task: &str, msgs: &[Value], budget: i64) -> Reply {
    let mut lex = Lex::default();
    let mut all: Vec<Cow<Value>> = msgs
        .iter()
        .map(|m| if has_cache_control(m) { Cow::Owned(strip_cache_control(m)) } else { Cow::Borrowed(m) })
        .collect();
    let upto = all.len();
    all.push(Cow::Owned(serde_json::json!({"role": "user", "content": [{"type": "text", "text": task}]})));
    let tr = Trace::new(all, &mut lex);
    let th = Threads::build(&tr);
    let n = th.turns.len().saturating_sub(1);
    let rel: Vec<f64> = (0..n).map(|j| th.relevance(j)).collect();
    let mut turn_of = vec![0usize; upto];
    for j in 0..n {
        let (a, b) = th.span(j, tr.msgs.len());
        for t in turn_of.iter_mut().take(b.min(upto)).skip(a) {
            *t = j;
        }
    }

    let (q_terms, q_names) = lex.analyze(task);
    // candidates: tool outputs (by paragraph), the person's and the agent's words, long inputs
    let mut cands: Vec<(usize, usize)> = vec![]; // (unit, chunk)
    let mut df: HashMap<u32, usize> = HashMap::new();
    for (i, u) in tr.units.iter().enumerate() {
        if u.msg >= upto || u.kind == Kind::Thinking || (u.kind == Kind::Call && (u.tool == "task" || u.tokens < 40)) {
            continue;
        }
        if let (Kind::Result, Some(Info { kind: ToolKind::Read, path: Some(p), .. })) = (u.kind, &u.info) {
            if tr.writes.get(p).is_some_and(|w| w.iter().any(|&m| m > u.msg)) {
                continue; // a later write made it stale
            }
        }
        for (k, c) in u.chunks.iter().enumerate() {
            for &w in &q_terms {
                if has(&c.terms, w) {
                    *df.entry(w).or_default() += 1;
                }
            }
            cands.push((i, k));
        }
    }
    if cands.is_empty() || q_terms.is_empty() {
        return Reply::default();
    }
    let big_n = cands.len() as f64;
    // a name of the task (path, identifier) weighs double
    let mut idf: Vec<(u32, f64)> = q_terms
        .iter()
        .map(|&w| {
            let x = (1.0 + big_n / (1 + df.get(&w).copied().unwrap_or(0)) as f64).ln();
            (w, if has(&q_names, w) { x * 2.0 } else { x })
        })
        .collect();
    // sums run over the words in order (as the reference's do), so a sum does not depend on how they were met
    idf.sort_by(|a, b| lex.strs[a.0 as usize].cmp(&lex.strs[b.0 as usize]));
    let idf_of: HashMap<u32, f64> = idf.iter().copied().collect();
    let total = idf.iter().map(|x| x.1).sum::<f64>();
    let total = if total == 0.0 { 1.0 } else { total };

    let mut scored = vec![];
    for &(i, k) in &cands {
        let u = &tr.units[i];
        let c = &u.chunks[k];
        let hold: Set = idf.iter().map(|x| x.0).filter(|&w| has(&c.terms, w) || has(&c.words, w)).collect();
        if hold.is_empty() {
            continue;
        }
        let weight: f64 = hold.iter().map(|w| idf_of[w]).sum();
        let named = q_names.iter().any(|&w| has(&c.words, w));
        if weight / total < MIN_SHARE && !named {
            continue;
        }
        let all_lines: Vec<&str> = u.text.split('\n').collect();
        let (a, b) = c.lines.unwrap_or((0, all_lines.len()));
        let mut lines = all_lines[a.min(all_lines.len())..b.min(all_lines.len())].to_vec();
        if lines.len() > LONG {
            lines = trim(&lines, &idf, &mut lex);
        }
        let text = lines.join("\n").trim_matches('\n').to_string();
        if py::strip(&text).is_empty() {
            continue;
        }
        let r = if rel.is_empty() { 1.0 } else { rel[turn_of[u.msg]] };
        let factor = (0.35 + 0.65 * r) * if c.error { 1.15 } else { 1.0 };
        let tokens = py::len(&text) / 4 + 1;
        scored.push(Scored { unit: i, msg: u.msg, block: u.block, start: a, end: b, hold, factor, text, tokens });
    }

    // weighted coverage, greedy by worth per square root of tokens
    let mut picked: Vec<(Scored, f64)> = vec![];
    let mut covered: HashSet<u32> = HashSet::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut left = budget;
    while !scored.is_empty() && picked.len() < MAX_SPANS {
        let mut best: Option<(f64, usize, usize, usize, usize)> = None; // (worth, msg, block, start, k)
        for (k, s) in scored.iter().enumerate() {
            if s.tokens as i64 > left {
                continue;
            }
            let (mut new, mut again) = (0.0, 0.0);
            for w in &s.hold {
                if covered.contains(w) {
                    again += idf_of[w];
                } else {
                    new += idf_of[w];
                }
            }
            let worth = (new + 0.25 * again) * s.factor / (s.tokens as f64).sqrt();
            // ties: the later message, then the earlier block and line
            let better = match best {
                None => true,
                Some((bw, bm, bb, bs, _)) => {
                    worth > bw
                        || (worth == bw
                            && (s.msg > bm || (s.msg == bm && (s.block < bb || (s.block == bb && s.start < bs)))))
                }
            };
            if better {
                best = Some((worth, s.msg, s.block, s.start, k));
            }
        }
        let Some((worth, _, _, _, k)) = best else { break };
        if worth <= 0.0 {
            break;
        }
        let s = scored.remove(k);
        if !seen.insert(s.text.clone()) {
            continue;
        }
        covered.extend(&s.hold);
        left -= s.tokens as i64;
        picked.push((s, py::round(worth, 4)));
    }
    picked.sort_by_key(|(s, _)| (s.msg, s.block, s.start));
    let spans = picked
        .into_iter()
        .map(|(s, score)| CuratedSpan {
            msg: s.msg,
            block: s.block,
            start: s.start,
            end: s.end,
            label: tr.label(s.unit),
            text: s.text,
            score,
            tokens: s.tokens,
        })
        .collect();

    let current = n.checked_sub(1);
    let mut asks: Vec<(f64, usize)> =
        (1..n).filter(|&j| Some(j) != current && rel[j] >= 0.3).map(|j| (rel[j], j)).collect();
    asks.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
    asks.truncate(3);
    asks.sort_by_key(|x| x.1);
    let asks =
        asks.into_iter().map(|(p, j)| Ask { turn: j, ask: th.ask(&tr, j), relevance: py::round(p, 3) }).collect();
    Reply { spans, asks }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn words_and_terms_follow_the_curator() {
        let mut ws = vec![];
        words(
            "cfg.epoch_tokens json.dumps v2 src/parser.rs 2024 1999x 3.11.2 parseLine main.rs api.example.com",
            &mut ws,
        );
        for w in ["epoch_tokens", "src/parser.rs", "3.11.2", "parseLine", "main.rs", "api.example.com"] {
            assert!(ws.contains(&w), "{w}: {ws:?}");
        }
        assert!(!ws.contains(&"2024") && !ws.contains(&"json.dumps"), "{ws:?}");
        let mut lex = Lex::default();
        let (t, _) = lex.analyze("Fix parseLine in the handler_config");
        let mut got: Vec<&str> = t.iter().map(|&k| lex.strs[k as usize].as_str()).collect();
        got.sort_unstable();
        assert_eq!(got, vec!["confi", "fix", "handl", "handler_config", "line", "parse", "parseLine"]);
    }

    #[test]
    fn line_numbers_are_recognized() {
        assert_eq!(strip_brief_lineno("     8│ fn x()"), "fn x()");
        assert_eq!(strip_brief_lineno("  12\t\tfoo"), "foo");
        assert_eq!(strip_brief_lineno("  12\t \tfoo"), "foo");
        assert_eq!(strip_brief_lineno("12 | bar"), "bar");
        assert_eq!(strip_brief_lineno("12 bar"), "12 bar");
        assert_eq!(strip_mechanical_lineno("  12→x"), "x");
        assert_eq!(strip_mechanical_lineno("12: x"), "x");
        assert_eq!(strip_mechanical_lineno("12 | x"), "x");
        assert_eq!(strip_mechanical_lineno("12x"), "12x");
    }

    #[test]
    fn paragraphs_cut_at_blank_lines() {
        assert_eq!(split_paragraphs("a\nb\nc\n\nd\ne\nf\ng"), vec![(0, 4), (4, 8)]);
        assert_eq!(split_paragraphs("a\nb\nc\n\nd"), vec![(0, 5)]);
        assert_eq!(split_paragraphs(""), vec![(0, 1)]);
    }

    #[test]
    fn tokens_are_estimated_on_canonical_json() {
        let b = json!({"type": "text", "text": "héllo \"x\"\n", "cache_control": {"type": "ephemeral"}});
        assert_eq!(stripped_len(&b, &mut 0), r#"{"text":"héllo \"x\"\n","type":"text"}"#.chars().count());
        let img = json!({"type": "image", "source": {"type": "base64", "data": "AAAA"}});
        let mut media = 0;
        assert_eq!(stripped_len(&img, &mut media), r#"{"source":{"type":"base64"},"type":"image"}"#.len());
        assert_eq!(media, 1);
        assert_eq!(json_len(&json!({"a": [1, 2.5, 1e-5, null, true]})), r#"{"a":[1,2.5,1e-05,null,true]}"#.len());
    }

    #[test]
    fn a_selection_bears_on_the_task_and_keeps_its_budget() {
        let lines: Vec<String> =
            (0..60).map(|i| format!("{:>6}│ fn parse_line_{i}(s: &str) -> Token {{ lex(s, {i}) }}", i + 1)).collect();
        let msgs = vec![
            json!({"role": "user", "content": "Fix parse_line_7 in src/parser.rs"}),
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": "r1", "name": "read", "input": {"path": "src/parser.rs"}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "r1", "content": lines.join("\n")}]}),
        ];
        let r = select("Fix parse_line_7 in src/parser.rs", &msgs, 600);
        let read = r.spans.iter().find(|s| s.label == "read src/parser.rs").expect("the read");
        assert!(read.text.contains("fn parse_line_7("), "{}", read.text);
        assert!(read.text.lines().count() <= LONG, "{}", read.text);
        assert!(r.spans.iter().map(|s| s.tokens).sum::<usize>() <= 600);
        assert_eq!(r, select("Fix parse_line_7 in src/parser.rs", &msgs, 600));
        assert!(select("", &msgs, 600).spans.is_empty());
    }
}
