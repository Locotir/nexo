//! Checkpoints: what a request does at `agent.max_turns` tool rounds instead of stopping.
//!
//! Code gathers the facts (cost, calls, files written, the todo list, the latest build or test line, errors
//! that repeat) and renders them as one short message, appended to the end of the conversation so the prompt
//! cache stays. The model answers on its first line with `CHECKPOINT CONTINUE: <reason>` or
//! `CHECKPOINT STOP: <reason>`. Whether a request has made progress is decided here, from the facts, never
//! by the model. The functions are pure so they are tested without the loop.

use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use nexo_core::message::{Message, Part};
use nexo_core::stream::Usage;
use serde_json::Value;

/// Checkpoints one request may take when no spend limit is in force and `max_checkpoints` is not set.
pub const DEFAULT_MAX: u32 = 5;
/// Characters of a message the model gets, at most.
const MAX_CHARS: usize = 1500;
/// Characters of one detail (the request, an error line, a build line).
const DETAIL: usize = 160;

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
const GO_ON: [&str; 6] = ["continue", "continuar", "continua", "seguir", "go on", "proceed"];
const STOP: [&str; 6] = ["stop", "parar", "detener", "alto", "halt", "para"];

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

/// The facts of one checkpoint.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub number: u32,
    /// The most checkpoints this request may take, if it has a cap.
    pub cap: Option<u32>,
    pub cost: f64,
    pub calls: usize,
    pub rounds: u32,
    pub input: u64,
    pub output: u64,
    pub cached: u64,
    pub elapsed: Duration,
    pub request: String,
    pub goal: Option<String>,
    pub todo: Option<Todo>,
    pub prev_todo: Option<Todo>,
    /// Files written since the start of the request, and since the last checkpoint.
    pub files: Vec<String>,
    pub new_files: Vec<String>,
    pub fresh_total: usize,
    pub fresh_new: usize,
    pub build: Option<(u32, String)>,
    /// Errors seen at least three times, most frequent first.
    pub errors: Vec<(String, u32)>,
    pub loop_hits: u32,
    pub loop_limit: u32,
    pub stale: u32,
    pub stale_limit: u32,
    /// The spend so far against the limits in force, as one line.
    pub spend: String,
    pub idle: u32,
}

fn cut(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= n {
        s
    } else {
        format!("{}...", s.chars().take(n.saturating_sub(3)).collect::<String>())
    }
}

pub fn tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{}k", (n as f64 / 1000.0).round() as u64),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

pub fn elapsed(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        _ => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
    }
}

/// `cost · calls`, the short form the person sees.
pub fn stats(r: &Report) -> String {
    format!("${:.2} · {} calls", r.cost, r.calls)
}

fn names(files: &[String], n: usize) -> String {
    let short: Vec<&str> = files.iter().take(n).map(|f| f.rsplit(['/', '\\']).next().unwrap_or(f.as_str())).collect();
    if files.len() > n {
        format!("{}, +{} more", short.join(", "), files.len() - n)
    } else {
        short.join(", ")
    }
}

/// The message the model gets: a header with the numbers, the details, and how to answer. It stays within
/// `MAX_CHARS`: when the details are too long the least telling ones go whole (repeated errors, then the build
/// line, then the file names), never the lines about progress and spend that the decision rests on.
pub fn render(r: &Report) -> String {
    let cached = (r.cached * 100).checked_div(r.input).unwrap_or(0);
    let count = match r.cap {
        Some(c) => format!("{}/{}", r.number, c),
        None => r.number.to_string(),
    };
    let head = format!(
        "[checkpoint {count}] Session at this point: ${:.2} · {} tool calls · {} steps · {} in / {} out, {cached}% cached · {} | \
         Consider, based on all the progress and details below, whether it is better to stop now or to continue until the job is done.",
        r.cost,
        r.calls,
        r.rounds,
        tokens(r.input),
        tokens(r.output),
        elapsed(r.elapsed)
    );
    let foot = "Reply with the first line exactly `CHECKPOINT CONTINUE: <reason>. NEXT: <steps>` and carry on with tool calls, \
                or `CHECKPOINT STOP: <reason>` followed by your final summary for the person. Keep the words CHECKPOINT, \
                CONTINUE and STOP in English whatever language you write in.";
    let room = MAX_CHARS.saturating_sub(head.chars().count() + foot.chars().count() + 2);
    // level 0 shows everything; each level after drops more and shortens the request and the goal
    let mut body = String::new();
    for level in 0..4 {
        body = details(r, level).join("\n");
        if body.chars().count() <= room {
            break;
        }
    }
    if body.chars().count() > room {
        body = body.chars().take(room.saturating_sub(3)).collect::<String>() + "...";
    }
    format!("{head}\n{body}\n{foot}")
}

fn details(r: &Report, level: u32) -> Vec<String> {
    let (ask, goal) = if level >= 3 { (80, 60) } else { (200, DETAIL) };
    let mut d: Vec<String> = Vec::new();
    d.push(format!(
        "Request: {}   Goal: {}",
        cut(&r.request, ask),
        r.goal.as_deref().map(|g| cut(g, goal)).unwrap_or_else(|| "none".into())
    ));
    d.push(match &r.todo {
        Some(t) => {
            let doing = t.doing_text.as_deref().map(|x| format!(" (doing: \"{}\")", cut(x, 80))).unwrap_or_default();
            format!("Todo: {} done, {} doing, {} pending{doing}", t.done, t.doing, t.pending)
        }
        None => "Todo: no list".into(),
    });
    let new_files = if r.new_files.is_empty() || level >= 3 {
        String::new()
    } else {
        format!(" ({})", names(&r.new_files, if level >= 2 { 2 } else { 4 }))
    };
    d.push(format!(
        "Files written (write tools): {} since the start, {} since the last checkpoint{new_files}",
        r.files.len(),
        r.new_files.len()
    ));
    if level < 2 {
        d.push(match &r.build {
            Some((round, line)) => format!("Last build/test: {} (step {round})", cut(line, DETAIL)),
            None => "Last build/test: not seen".into(),
        });
    }
    if !r.errors.is_empty() && level < 1 {
        let e: Vec<String> = r.errors.iter().take(2).map(|(l, n)| format!("\"{}\" x{n}", cut(l, 80))).collect();
        d.push(format!("Repeated errors: {}", e.join("; ")));
    }
    let limit = |n: u32| if n == 0 { "off".to_string() } else { n.to_string() };
    d.push(format!(
        "Guards: loop_guard {}/{}, stale_guard {}/{}",
        r.loop_hits,
        limit(r.loop_limit),
        r.stale,
        limit(r.stale_limit)
    ));
    let todo_change = match (&r.todo, &r.prev_todo) {
        (Some(t), Some(p)) if t.hash != p.hash => format!(", todo {:+} done", t.done as i64 - p.done as i64),
        (Some(t), None) => format!(", todo {} done", t.done),
        _ => String::new(),
    };
    d.push(if r.idle > 0 {
        "Since the last checkpoint: no progress (no file written, no new output line, todo unchanged); one more like this and the request stops".to_string()
    } else {
        format!("Since the last checkpoint: +{} files, +{} new output lines{todo_change}", r.new_files.len(), r.fresh_new)
    });
    d.push(format!("Spend: {}", cut(&r.spend, 220)));
    d
}

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
            if **is_error {
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

const BUILDERS: [&str; 10] = [
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

/// The line of a build or test command's output that tells how it went, if `cmd` is one.
pub fn build_line(cmd: &str, output: &str) -> Option<String> {
    if !BUILDERS.iter().any(|b| cmd.contains(b)) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decisions_are_read_from_the_first_lines() {
        let d = parse_decision("CHECKPOINT CONTINUE: tests still failing in parser.rs. NEXT: fix it").unwrap();
        assert!(d.go_on);
        assert_eq!(d.reason, "tests still failing in parser.rs");
        let d = parse_decision("**checkpoint stop:** all done\nThe parser works.").unwrap();
        assert!(!d.go_on);
        assert_eq!((d.reason.as_str(), d.rest.as_str()), ("all done", "The parser works."));
        let d = parse_decision("Let me think.\n\nCheckpoint Continue - more to do").unwrap();
        assert!(d.go_on && d.reason == "more to do");
        assert!(
            parse_decision("a\nb\nc\nd\ne\nf\ng\nh\nCHECKPOINT STOP: late").is_none(),
            "only the first eight lines"
        );
        assert!(parse_decision("I will stop at the checkpoint").is_none());
        assert!(parse_decision("").is_none());
        assert!(parse_decision("CHECKPOINT").is_none());
        let d = parse_decision("Voy a decidir.\n1. **CHECKPOINT PARAR**: ya esta").unwrap();
        assert!(!d.go_on && d.reason == "ya esta");
        let d = parse_decision("[CHECKPOINT CONTINUAR]: sigo. NEXT: a").unwrap();
        assert!(d.go_on && d.reason == "sigo");
        assert!(parse_decision("checkpoint stopwatch").is_none(), "a word that only begins with the marker word");
        assert!(parse_decision("a\nb\nc\nd\ne\nf\ng\nCHECKPOINT STOP: in the eighth").is_some());
    }

    #[test]
    fn progress_needs_one_signal() {
        let a = Mark { writes: 2, wrote: 2, fresh: 10, todo: 7 };
        assert!(!progress(&a, &a));
        assert!(!progress(&Mark { writes: 3, ..a.clone() }, &a), "a write that left nothing new is not progress");
        assert!(progress(&Mark { wrote: 3, ..a.clone() }, &a));
        assert!(progress(&Mark { fresh: 11, ..a.clone() }, &a));
        assert!(progress(&Mark { todo: 8, ..a.clone() }, &a));
    }

    #[test]
    fn the_todo_list_is_read_from_the_last_call() {
        let call = |items: Value| {
            Message::new(
                nexo_core::message::Role::Assistant,
                vec![Part::tool_call("t", "todo", json!({"items": items}))],
            )
        };
        let h = vec![
            call(json!([{"text": "a", "status": "done"}])),
            call(
                json!([{"text": "a", "status": "done"}, {"text": "fix parser", "status": "in_progress"}, {"text": "c"}]),
            ),
        ];
        let t = todo_of(&h).unwrap();
        assert_eq!((t.done, t.doing, t.pending), (1, 1, 1));
        assert_eq!(t.doing_text.as_deref(), Some("fix parser"));
        assert_ne!(t.hash, todo_of(&h[..1]).unwrap().hash);
        assert!(todo_of(&[]).is_none());
        assert_eq!(calls_in(&h), 2);
    }

    fn big() -> Report {
        Report {
            number: 2,
            cap: Some(5),
            cost: 0.31,
            calls: 400,
            rounds: 400,
            input: 1_200_000,
            output: 38_000,
            cached: 1_116_000,
            elapsed: Duration::from_secs(41 * 60),
            request: "x".repeat(900),
            goal: Some("g".repeat(500)),
            todo: Some(Todo { done: 3, doing: 1, pending: 2, doing_text: Some("fix parser tests".into()), hash: 1 }),
            prev_todo: Some(Todo { done: 2, hash: 2, ..Default::default() }),
            files: (0..30).map(|i| format!("src/file{i}.rs")).collect(),
            new_files: (0..30).map(|i| format!("src/file{i}.rs")).collect(),
            fresh_total: 300,
            fresh_new: 85,
            build: Some((391, "cargo test -> test result: FAILED. 2 failed".into())),
            errors: (0..20).map(|i| (format!("error number {i} {}", "e".repeat(100)), 4)).collect(),
            loop_hits: 0,
            loop_limit: 3,
            stale: 2,
            stale_limit: 6,
            spend: "$0.31 this request, $2.10 of $5.00 this session (max_cost)".into(),
            idle: 0,
        }
    }

    #[test]
    fn the_message_is_compact_and_has_the_header() {
        let m = render(&big());
        assert!(m.chars().count() <= MAX_CHARS, "{}", m.chars().count());
        for keep in ["Since the last checkpoint", "Spend:", "Guards:", "Todo:"] {
            assert!(m.contains(keep), "{keep} survives a long message: {m}");
        }
        assert!(m.starts_with("[checkpoint 2/5] Session at this point: $0.31 · 400 tool calls · 400 steps · 1.2M in / 38k out, 93% cached · 41m | Consider, based on all the progress"), "{m}");
        assert!(m.contains("CHECKPOINT CONTINUE") && m.contains("CHECKPOINT STOP"));
        let mut small = big();
        small.cap = None;
        small.errors.clear();
        small.files = vec!["a.rs".into()];
        small.new_files = small.files.clone();
        small.request = "fix it".into();
        small.goal = None;
        let m = render(&small);
        assert!(m.starts_with("[checkpoint 2] Session"), "{m}");
        assert!(m.contains("Goal: none") && m.contains("(a.rs)") && !m.contains("Repeated errors"), "{m}");
    }

    #[test]
    fn an_idle_checkpoint_says_so() {
        let mut r = big();
        r.idle = 1;
        assert!(render(&r).contains("no progress"));
    }

    #[test]
    fn build_lines_and_repeated_errors_are_noted() {
        assert_eq!(build_line("ls", "error: x"), None);
        assert_eq!(
            build_line("cargo test -j 2", "running 3 tests\ntest result: FAILED. 1 passed; 2 failed\n\n").as_deref(),
            Some("test result: FAILED. 1 passed; 2 failed")
        );
        let mut t = Tracker::new(0.0, Usage::default(), Mark::default(), None);
        let err = vec![Part::ToolResult {
            call_id: "1".into(),
            name: Some("bash".into()),
            content: vec![Part::text("error: cannot borrow x\nmore")],
            is_error: true,
        }];
        for _ in 0..3 {
            t.round(&[("bash".into(), json!({"command": "cargo build"}).to_string())], &err);
        }
        assert_eq!(t.repeated(), vec![("error: cannot borrow x".to_string(), 3)]);
        assert_eq!(t.build().unwrap().0, 3);
        assert_eq!(tokens(1_200_000), "1.2M");
        assert_eq!(elapsed(Duration::from_secs(3 * 3600 + 5 * 60)), "3h05m");
    }
}
