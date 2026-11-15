//! The facts of one checkpoint and the message they are rendered as.

use std::time::Duration;

use super::marker::Todo;
use super::{DETAIL, MAX_CHARS};

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

pub(super) fn cut(s: &str, n: usize) -> String {
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

pub(super) fn names(files: &[String], n: usize) -> String {
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

pub(super) fn details(r: &Report, level: u32) -> Vec<String> {
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
