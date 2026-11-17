//! The per-request state of the checkpoints, and the build and test line of a command.

use super::marker::*;
use super::report::*;
use nexo_core::message::Part;
use nexo_core::stream::Usage;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// Per-request state of the checkpoints, kept by the loop.
pub struct Tracker {
    pub started: Instant,
    pub cost0: f64,
    pub usage0: Usage,
    pub prev: Mark,
    /// File writes recorded when the request began.
    pub writes0: usize,
    pub prev_todo: Option<Todo>,
    /// Each (path, content) a write tool left, so that writing the same bytes again is not progress.
    written: HashSet<(String, u64)>,
    /// Output lines new to the session, counted over the request.
    pub fresh: usize,
    pub rounds: u32,
    errors: HashMap<String, u32>,
    build: Option<(u32, String)>,
    /// Checkpoints in a row with no progress.
    pub idle: u32,
    /// A checkpoint was sent and its answer is awaited: its number and the stats line.
    pub pending: Option<(u32, String)>,
    /// The model said to continue without a call and was told once to go on.
    pub nudged: bool,
    /// The model chose to stop: why, until the request ends.
    pub stopping: Option<String>,
}

impl Tracker {
    pub fn new(cost0: f64, usage0: Usage, mark: Mark, todo: Option<Todo>) -> Tracker {
        Tracker {
            started: Instant::now(),
            cost0,
            usage0,
            writes0: mark.writes,
            prev: mark,
            prev_todo: todo,
            written: HashSet::new(),
            fresh: 0,
            rounds: 0,
            errors: HashMap::new(),
            build: None,
            idle: 0,
            pending: None,
            nudged: false,
            stopping: None,
        }
    }

    /// Notes a tool round: its errors and, from the shell commands, the latest build or test line.
    pub fn round(&mut self, asked: &[(String, String)], results: &[Part]) {
        self.rounds += 1;
        let outs: Vec<(&bool, String)> = results
            .iter()
            .filter_map(|p| match p {
                Part::ToolResult { content, is_error, .. } => {
                    Some((is_error, content.iter().filter_map(Part::as_text).collect::<Vec<_>>().join("\n")))
                }
                _ => None,
            })
            .collect();
        for (is_error, text) in &outs {
            // a shell command that ended with a non-zero code is not shown as an error, but it still counts here
            if **is_error || nexo_core::shell::exit_code_in(text).is_some_and(|c| c != 0) {
                if let Some(l) = text.lines().map(str::trim).find(|l| !l.is_empty()) {
                    *self.errors.entry(l.chars().take(80).collect()).or_insert(0) += 1;
                }
            }
        }
        if asked.len() == outs.len() {
            for ((name, args), (_, text)) in asked.iter().zip(&outs) {
                if name != "bash" {
                    continue;
                }
                let cmd = serde_json::from_str::<Value>(args)
                    .ok()
                    .and_then(|v| v.get("command").or(v.get("cmd")).and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_default();
                if let Some(line) = build_line(&cmd, text) {
                    self.build = Some((self.rounds, format!("{} -> {line}", cut(&cmd, 40))));
                }
            }
        }
    }

    /// A write tool succeeded on `path` and left `content` (a hash).
    pub fn note_write(&mut self, path: &str, content: u64) {
        self.written.insert((path.to_string(), content));
    }

    /// Distinct (path, content) pairs written so far in the request.
    pub fn wrote(&self) -> usize {
        self.written.len()
    }

    pub fn build(&self) -> Option<(u32, String)> {
        self.build.clone()
    }

    /// Errors that came back at least three times, most frequent first.
    pub fn repeated(&self) -> Vec<(String, u32)> {
        let mut v: Vec<(String, u32)> =
            self.errors.iter().filter(|(_, n)| **n >= 3).map(|(l, n)| (l.clone(), *n)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }
}

pub(super) const BUILDERS: [&str; 10] = [
    "cargo build",
    "cargo test",
    "cargo clippy",
    "cargo check",
    "npm test",
    "pytest",
    "go test",
    "make",
    "tsc",
    "gradle",
];

/// Programs that run the command after them: their own options and numbers (a niceness, a timeout) are skipped.
const WRAPPERS: [&str; 6] = ["nice", "time", "env", "nohup", "timeout", "exec"];

/// The words of each command `cmd` runs: split at `;`, `&`, `|`, a newline and parentheses outside quotes, and
/// at spaces outside quotes. A quoted part stays one word, so `pgrep -af "cargo test"` never reads as a build.
fn segments(cmd: &str) -> Vec<Vec<String>> {
    let (mut all, mut words, mut word) = (Vec::new(), Vec::new(), String::new());
    let mut quote: Option<char> = None;
    let mut chars = cmd.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') | (None, '\\') => word.extend(chars.next()),
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, ';' | '&' | '|' | '\n' | '(' | ')') => {
                words.extend((!word.is_empty()).then(|| std::mem::take(&mut word)));
                all.extend((!words.is_empty()).then(|| std::mem::take(&mut words)));
            }
            (None, c) if c.is_whitespace() => words.extend((!word.is_empty()).then(|| std::mem::take(&mut word))),
            (None, c) => word.push(c),
        }
    }
    words.extend((!word.is_empty()).then_some(word));
    all.extend((!words.is_empty()).then_some(words));
    all
}

/// Whether the command of `words` is a build or test of [`BUILDERS`]: its program (past variable assignments
/// and wrappers) is the builder's, or a wrapper named after it (`nxcargo`), and its first word that is not an
/// option is the builder's subcommand.
fn runs_builder(words: &[String]) -> bool {
    let assignment = |w: &str| w.split_once('=').is_some_and(|(k, _)| !k.is_empty() && !k.contains('/'));
    let mut rest = words.iter().map(String::as_str).skip_while(|w| assignment(w)).peekable();
    while rest.peek().is_some_and(|w| WRAPPERS.contains(w)) {
        rest.next();
        while rest
            .peek()
            .is_some_and(|w| w.starts_with('-') || w.starts_with(|c: char| c.is_ascii_digit()) || assignment(w))
        {
            rest.next();
        }
    }
    let Some(program) = rest.next() else { return false };
    let program = program.rsplit('/').next().unwrap_or(program);
    let sub = rest.find(|w| !w.starts_with('-') && !w.starts_with('+'));
    BUILDERS.iter().any(|b| {
        let mut parts = b.split(' ');
        let name = parts.next().unwrap_or(b);
        program.ends_with(name) && parts.next().is_none_or(|s| sub == Some(s))
    })
}

/// The line of a build or test command's output that tells how it went, if `cmd` runs one (as a command, never
/// as an argument or inside quotes: `cat RULES.md; pgrep -af "cargo test"` is not a test).
pub fn build_line(cmd: &str, output: &str) -> Option<String> {
    if !segments(cmd).iter().any(|words| runs_builder(words)) {
        return None;
    }
    const KEYS: [&str; 5] = ["test result", "error", "failed", "passed", "warning"];
    output
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| {
            let low = l.to_ascii_lowercase();
            !l.is_empty() && KEYS.iter().any(|k| low.contains(k))
        })
        .map(|l| cut(l, 100))
}
