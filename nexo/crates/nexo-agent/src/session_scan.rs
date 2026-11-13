//! What the usage is made of: the saved sessions read once, boiled down per day, and ranked.
//!
//! The reader lives next to the writer (`session.rs`) so the line format stays in one place. Each
//! file is read whole, in one pass, into a [`Digest`] (per local day: the counts, the tool output
//! that stayed in the context, the cost per model); a digest is kept while the file's modification
//! time and size stay the same, so a second scan reads only what changed. [`rank`] then answers for
//! a [`Period`] without touching the disk. Nothing here keeps text of a session: only counts, tool
//! names, the program (and verb) of a command, and folders. Reading takes no lock.
//!
//! Approximate by construction. A `usage` line of an older file has no time, model or owner: it is
//! put in the day of the message before it and under the model in force. A file rewritten by a
//! compaction or a rewind holds one `usage` line with everything spent so far; it counts in the
//! totals (on the day the session began) but is not one call.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::Value;

use crate::session::UsageKind;

#[cfg(test)]
mod tests;

/// A call that sends more than this many tokens is long context.
pub const LONG_CTX: u64 = 150_000;
/// A cache miss needs the call before it to have sent at least this much.
pub const MISS_FROM: u64 = 20_000;
/// A row is shown from this share of the usage.
pub const MIN_SHARE: f64 = 0.10;
/// Rows shown at most.
pub const MAX_ROWS: usize = 6;
/// Sessions read at most, newest first.
pub const MAX_FILES: usize = 400;
/// Bytes of sessions read at most.
pub const MAX_BYTES: u64 = 64 << 20;
/// A session file larger than this is left out.
pub const MAX_FILE: u64 = 32 << 20;
/// Files not touched for this many days are not opened.
pub const KEEP_DAYS: i64 = 30;
/// A model must carry this share of the spend to be called out.
pub const MODEL_SHARE: f64 = 0.5;
/// Commands remembered per tool and day.
const TOP_KEEP: usize = 64;

/// The window of days the figures cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Day,
    Week,
    Month,
}

impl Period {
    pub const ALL: [Period; 3] = [Period::Day, Period::Week, Period::Month];

    pub fn days(self) -> i64 {
        match self {
            Period::Day => 1,
            Period::Week => 7,
            Period::Month => 30,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Period::Day => "today",
            Period::Week => "7 days",
            Period::Month => "30 days",
        }
    }

    /// The key that picks it.
    pub fn key(self) -> char {
        match self {
            Period::Day => 'd',
            Period::Week => 'w',
            Period::Month => 'm',
        }
    }

    pub fn from_key(c: char) -> Option<Period> {
        Period::ALL.into_iter().find(|p| p.key() == c)
    }
}

/// Counts of model calls. All additive.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tally {
    /// Model calls (the one line that sums a rewritten file is not one).
    pub calls: u64,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cost: f64,
    /// Calls that spent tokens and no money: the model has no price.
    pub unpriced: u64,
    pub long_calls: u64,
    pub long_input: u64,
    /// Calls that re-sent a prompt the cache no longer held, and the tokens they sent.
    pub misses: u64,
    pub resent: u64,
    /// Calls after a compaction, a rewind or a model switch: expected, not misses.
    pub rebuilds: u64,
    /// Money of the lines that say whose call they are, and the part of it that is not the conversation's.
    pub kinded_cost: f64,
    pub side_cost: f64,
    /// Tokens of history the view left out, and what the calls that said so sent.
    pub held: u64,
    pub held_base: u64,
    /// Input tokens and money of the lines that sum a rewritten file: in the totals, but none of
    /// the calls the shares are worked out over.
    pub summed_input: u64,
    pub summed_cost: f64,
}

impl Tally {
    pub fn total_input(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }

    pub fn add(&mut self, o: &Tally) {
        self.calls += o.calls;
        self.input += o.input;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.output += o.output;
        self.reasoning += o.reasoning;
        self.cost += o.cost;
        self.unpriced += o.unpriced;
        self.long_calls += o.long_calls;
        self.long_input += o.long_input;
        self.misses += o.misses;
        self.resent += o.resent;
        self.rebuilds += o.rebuilds;
        self.kinded_cost += o.kinded_cost;
        self.side_cost += o.side_cost;
        self.held += o.held;
        self.held_base += o.held_base;
        self.summed_input += o.summed_input;
        self.summed_cost += o.summed_cost;
    }

    /// Share of the input that came from the cache, 0..=100.
    pub fn cache_hit(&self) -> Option<u64> {
        (self.cache_read * 100).checked_div(self.total_input()).map(|h| h.min(100))
    }
}

/// What one tool's results weighed in a day.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolTally {
    pub runs: u64,
    /// Tokens of the results (about four characters each).
    pub tokens: u64,
    /// Tokens of the results times the calls that carried them: what they cost the context.
    pub weighted: u64,
    /// The program of the command (and its verb, for `cargo test`) where the tool has one, and its weighted tokens.
    pub top: BTreeMap<String, u64>,
}

impl ToolTally {
    fn add(&mut self, o: &ToolTally) {
        self.runs += o.runs;
        self.tokens += o.tokens;
        self.weighted += o.weighted;
        for (k, v) in &o.top {
            *self.top.entry(k.clone()).or_default() += v;
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelTally {
    pub calls: u64,
    pub cost: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DayDigest {
    pub tally: Tally,
    pub tools: BTreeMap<String, ToolTally>,
    pub models: BTreeMap<String, ModelTally>,
}

/// One session file, boiled down.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Digest {
    pub id: String,
    pub cwd: String,
    /// Per local day (days since 1970-01-01).
    pub days: BTreeMap<i64, DayDigest>,
}

// ---------------------------------------------------------------------------------------------
// reading one file

#[derive(Deserialize, Default)]
struct MetaLite {
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    created: String,
    #[serde(default)]
    model: String,
}

#[derive(Deserialize)]
struct ModelLite {
    #[serde(default)]
    model: String,
}

#[derive(Deserialize, Default)]
struct UsageLite {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    cache_read: u64,
    #[serde(default)]
    cache_write: u64,
    #[serde(default)]
    reasoning: u64,
}

#[derive(Deserialize)]
struct UsageLine {
    #[serde(default)]
    usage: UsageLite,
    #[serde(default)]
    cost_usd: f64,
    #[serde(default)]
    ts: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    kind: Option<UsageKind>,
    #[serde(default)]
    held: Option<u64>,
}

#[derive(Deserialize)]
struct MessageLine {
    #[serde(default)]
    message: MessageLite,
}

#[derive(Deserialize, Default)]
struct MessageLite {
    #[serde(default)]
    content: Value,
}

#[derive(Deserialize)]
struct OutputLine {
    #[serde(default)]
    call_id: String,
    #[serde(default)]
    output: OutputLite,
}

#[derive(Deserialize, Default)]
struct OutputLite {
    #[serde(default)]
    content: String,
}

/// A tool result still in the context.
struct Live {
    tool: String,
    cmd: Option<String>,
    tokens: u64,
}

struct Walk {
    offset: i64,
    digest: Digest,
    /// The day of the last message: where a line with no time of its own falls.
    day: i64,
    created_day: i64,
    model: String,
    /// The model of the last conversation call, and the total input of the call before.
    last_model: Option<String>,
    prev: Option<u64>,
    /// The next call re-sends the prompt on purpose (a reset, a compaction, a new model).
    rebuild: bool,
    calls: HashMap<String, (String, Option<String>)>,
    live: Vec<Live>,
    cache_seen: bool,
    maybe_misses: Vec<(i64, u64)>,
    usage_seen: bool,
    messages: u64,
    first_ts: Option<String>,
    same_ts: bool,
}

impl Walk {
    fn new(id: &str, offset: i64) -> Walk {
        Walk {
            offset,
            digest: Digest { id: id.to_string(), ..Digest::default() },
            day: 0,
            created_day: 0,
            model: String::new(),
            last_model: None,
            prev: None,
            rebuild: false,
            calls: HashMap::new(),
            live: Vec::new(),
            cache_seen: false,
            maybe_misses: Vec::new(),
            usage_seen: false,
            messages: 0,
            first_ts: None,
            same_ts: true,
        }
    }

    fn day_mut(&mut self, day: i64) -> &mut DayDigest {
        self.digest.days.entry(day).or_default()
    }

    fn day_of_ts(&self, ts: &str) -> Option<i64> {
        unix_of(ts).map(|u| day_of(u, self.offset))
    }

    fn line(&mut self, line: &str) {
        let Some(rest) = line.strip_prefix("{\"type\":\"") else { return };
        let kind = rest.split('"').next().unwrap_or("");
        match kind {
            "meta" => {
                if let Ok(m) = serde_json::from_str::<MetaLite>(line) {
                    self.digest.cwd = m.cwd;
                    self.model = m.model;
                    if let Some(d) = self.day_of_ts(&m.created) {
                        self.day = d;
                        self.created_day = d;
                    }
                }
            }
            "model" => {
                if let Ok(m) = serde_json::from_str::<ModelLite>(line) {
                    if m.model != self.model {
                        self.rebuild = true;
                    }
                    self.model = m.model;
                }
            }
            "reset" => {
                self.live.clear();
                self.rebuild = true;
            }
            "message" => self.message(line),
            "tool_output" => self.output(line),
            "usage" => self.usage(line),
            _ => {}
        }
    }

    fn message(&mut self, line: &str) {
        let ts = field_ts(line);
        if let Some(day) = ts.and_then(|t| self.day_of_ts(t)) {
            self.day = day;
        }
        if !self.usage_seen {
            self.messages += 1;
            match (&self.first_ts, ts) {
                (None, Some(t)) => self.first_ts = Some(t.to_string()),
                (Some(a), Some(t)) if a != t => self.same_ts = false,
                _ => {}
            }
        }
        if !line.contains("\"type\":\"tool_use\"") {
            return;
        }
        let Ok(m) = serde_json::from_str::<MessageLine>(line) else { return };
        let Some(parts) = m.message.content.as_array() else { return };
        for p in parts {
            if p.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let (Some(id), Some(name)) = (p.get("id").and_then(Value::as_str), p.get("name").and_then(Value::as_str))
            else {
                continue;
            };
            let cmd = p.get("input").and_then(|i| i.get("command")).and_then(Value::as_str).and_then(command_head);
            self.calls.insert(id.to_string(), (name.to_string(), cmd));
        }
    }

    fn output(&mut self, line: &str) {
        let Ok(o) = serde_json::from_str::<OutputLine>(line) else { return };
        // the result of a call whose message is gone (a compaction) is not in the context
        let Some((tool, cmd)) = self.calls.get(&o.call_id).cloned() else { return };
        let chars = o.output.content.chars().count() as u64;
        let tokens = chars.div_ceil(4);
        let day = self.day;
        let t = self.day_mut(day).tools.entry(tool.clone()).or_default();
        t.runs += 1;
        t.tokens += tokens;
        self.live.push(Live { tool, cmd, tokens });
    }

    fn usage(&mut self, line: &str) {
        let Ok(u) = serde_json::from_str::<UsageLine>(line) else { return };
        let first = !self.usage_seen;
        self.usage_seen = true;
        // one line that holds everything spent before a rewrite: the messages above it are all
        // stamped with the instant of the rewrite
        let summed = u.kind == Some(UsageKind::Sum)
            || (first && u.kind.is_none() && u.ts.is_none() && self.messages >= 3 && self.same_ts);
        let total = u.usage.input + u.usage.cache_read + u.usage.cache_write;
        if u.usage.cache_read > 0 || u.usage.cache_write > 0 {
            self.cache_seen = true;
        }
        let day =
            if summed { self.created_day } else { u.ts.as_deref().and_then(|t| self.day_of_ts(t)).unwrap_or(self.day) };
        let model = u.model.clone().unwrap_or_else(|| self.model.clone());
        let main = matches!(u.kind, None | Some(UsageKind::Main));
        // the chain of conversation calls the cache is judged on
        let mut miss = None;
        if summed {
            self.prev = None;
            self.rebuild = true;
        } else if main {
            let switched = self.last_model.as_ref().is_some_and(|m| *m != model);
            if self.rebuild || switched {
                if self.prev.is_some() {
                    miss = Some(false);
                }
            } else if let Some(p) = self.prev {
                if p >= MISS_FROM && u.usage.cache_read * 10 < p {
                    miss = Some(true);
                }
            }
            self.prev = Some(total);
            self.rebuild = false;
            self.last_model = Some(model.clone());
        } else if u.kind == Some(UsageKind::Compact) {
            self.rebuild = true;
        }
        if miss == Some(true) {
            self.maybe_misses.push((day, total));
        }
        // the tool results this call carried, weighted; they cannot weigh more than the call sent
        let carried: Vec<(String, Option<String>, u64)> = if main && !summed {
            let held: u64 = self.live.iter().map(|l| l.tokens).sum();
            let scale = |n: u64| if held > total { (n as u128 * total as u128 / held as u128) as u64 } else { n };
            self.live.iter().map(|l| (l.tool.clone(), l.cmd.clone(), scale(l.tokens))).collect()
        } else {
            Vec::new()
        };
        let d = self.day_mut(day);
        for (tool, cmd, tokens) in carried {
            let t = d.tools.entry(tool).or_default();
            t.weighted += tokens;
            if let Some(c) = cmd {
                if t.top.len() < TOP_KEEP || t.top.contains_key(&c) {
                    *t.top.entry(c).or_default() += tokens;
                }
            }
        }
        let t = &mut d.tally;
        t.input += u.usage.input;
        t.cache_read += u.usage.cache_read;
        t.cache_write += u.usage.cache_write;
        t.output += u.usage.output;
        t.reasoning += u.usage.reasoning;
        t.cost += u.cost_usd;
        if summed {
            t.summed_input += total;
            t.summed_cost += u.cost_usd;
        } else {
            t.calls += 1;
            if u.cost_usd <= 0.0 && (total > 0 || u.usage.output > 0) {
                t.unpriced += 1;
            }
            if total > LONG_CTX {
                t.long_calls += 1;
                t.long_input += total;
            }
            if miss == Some(false) {
                t.rebuilds += 1;
            }
        }
        if let Some(k) = u.kind {
            if k != UsageKind::Sum {
                t.kinded_cost += u.cost_usd;
                if k != UsageKind::Main {
                    t.side_cost += u.cost_usd;
                }
            }
        }
        if let (Some(h), true) = (u.held, main && !summed) {
            t.held += h;
            t.held_base += total;
        }
        if !summed {
            let m = d.models.entry(model).or_default();
            m.calls += 1;
            m.cost += u.cost_usd;
        }
    }

    fn finish(mut self) -> Digest {
        // a session that never reported a cache has no misses: it has no cache
        if self.cache_seen {
            for (day, total) in std::mem::take(&mut self.maybe_misses) {
                let t = &mut self.day_mut(day).tally;
                t.misses += 1;
                t.resent += total;
            }
        }
        self.digest
    }
}

/// The `ts` of a `message` line, read without parsing the line (it follows `index`).
fn field_ts(line: &str) -> Option<&str> {
    let mut end = line.len().min(160);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let head = &line[..end];
    let at = head.find("\"ts\":\"")? + 6;
    let end = head[at..].find('"')?;
    Some(&head[at..at + end])
}

/// Programs whose second word is a verb (`cargo test`) and not an argument.
const VERBS: [&str; 30] = [
    "git",
    "cargo",
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "bun",
    "docker",
    "podman",
    "kubectl",
    "helm",
    "go",
    "make",
    "cmake",
    "pip",
    "pip3",
    "uv",
    "gh",
    "systemctl",
    "journalctl",
    "apt",
    "pacman",
    "brew",
    "dotnet",
    "gradle",
    "mvn",
    "rustup",
    "terraform",
    "just",
    "adb",
];

/// Words that only run the next one.
const WRAPPERS: [&str; 7] = ["sudo", "env", "nice", "nohup", "time", "exec", "command"];

/// A label for a shell command: the program, and its verb when it is a program that has them.
/// Arguments are never kept: they are where paths, hosts and secrets are.
fn command_head(cmd: &str) -> Option<String> {
    let plain = |w: &str, max: usize| {
        !w.is_empty() && w.chars().count() <= max && w.chars().all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
    };
    let assignment = |w: &str| {
        w.split_once('=').is_some_and(|(k, _)| {
            !k.is_empty()
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !k.starts_with(|c: char| c.is_ascii_digit())
        })
    };
    let mut words = cmd.split_whitespace().skip_while(|w| assignment(w) || WRAPPERS.contains(w));
    let first = words.next()?;
    let program = first.rsplit(['/', '\\']).next().unwrap_or(first);
    if !program.starts_with(|c: char| c.is_ascii_alphanumeric()) || !plain(program, 24) {
        return None;
    }
    let verb = words
        .next()
        .filter(|v| VERBS.contains(&program) && v.starts_with(|c: char| c.is_ascii_lowercase()) && plain(v, 16));
    Some(match verb {
        Some(v) => format!("{program} {v}"),
        None => program.to_string(),
    })
}

/// Seconds since 1970 of an RFC 3339 time.
fn unix_of(ts: &str) -> Option<i64> {
    time::OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339).ok().map(|t| t.unix_timestamp())
}

/// The local day (days since 1970-01-01) of a unix time at `offset` seconds east of UTC.
pub fn day_of(unix: i64, offset: i64) -> i64 {
    (unix + offset).div_euclid(86_400)
}

/// Reads one session from `r` into its digest; lines it does not know, or cannot read, are skipped.
pub fn digest_reader<R: BufRead>(id: &str, mut r: R, offset: i64) -> Digest {
    let mut w = Walk::new(id, offset);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\n', '\r']);
        if !line.is_empty() {
            w.line(line);
        }
    }
    w.finish()
}

// ---------------------------------------------------------------------------------------------
// reading the directory

/// The digests kept between scans, per file and while the file stays as it was.
#[derive(Default)]
pub struct Cache {
    offset: i64,
    map: HashMap<PathBuf, Entry>,
}

struct Entry {
    mtime: SystemTime,
    size: u64,
    digest: Arc<Digest>,
}

/// What a scan found.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub sessions: Vec<Arc<Digest>>,
    /// Sessions of the last [`KEEP_DAYS`] days, and how many of them were read (the rest are over the caps).
    pub eligible: usize,
    pub included: usize,
    /// Files parsed in this scan (the others came from the cache).
    pub parsed: usize,
    /// Today, as a day number.
    pub today: i64,
}

impl Report {
    /// The note for a scan that did not read everything.
    pub fn partial(&self) -> Option<String> {
        (self.included < self.eligible).then(|| format!("partial: {} of {} sessions", self.included, self.eligible))
    }
}

/// Scans `dir` (top level `*.jsonl` only) as of `now` (unix seconds) with days cut at `offset`
/// seconds east of UTC. A missing directory is an empty report.
pub fn scan(dir: &Path, now: i64, offset: i64, cache: &mut Cache) -> Result<Report, String> {
    if cache.offset != offset {
        cache.map.clear();
        cache.offset = offset;
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Report { today: day_of(now, offset), ..Report::default() })
        }
        Err(e) => return Err(format!("cannot read {}: {e}", dir.display())),
    };
    let oldest = now - KEEP_DAYS * 86_400;
    let mut files: Vec<(i64, SystemTime, u64, PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        if !md.is_file() {
            continue;
        }
        let Ok(mtime) = md.modified() else { continue };
        let secs = mtime.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        if secs < oldest {
            continue;
        }
        files.push((secs, mtime, md.len(), path));
    }
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.3.cmp(&b.3)));
    let eligible = files.len();
    let mut report = Report { eligible, today: day_of(now, offset), ..Report::default() };
    let mut bytes = 0u64;
    let mut seen = Vec::new();
    for (_, mtime, size, path) in files {
        if report.included >= MAX_FILES {
            break;
        }
        if size > MAX_FILE {
            // left out for good: it counts among the sessions not read
            continue;
        }
        if bytes + size > MAX_BYTES {
            break;
        }
        bytes += size;
        let hit = cache.map.get(&path).filter(|e| e.mtime == mtime && e.size == size).map(|e| e.digest.clone());
        let digest = match hit {
            Some(d) => d,
            None => {
                let Ok(f) = std::fs::File::open(&path) else { continue };
                let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                let d = Arc::new(digest_reader(&id, BufReader::with_capacity(64 * 1024, f), offset));
                report.parsed += 1;
                cache.map.insert(path.clone(), Entry { mtime, size, digest: d.clone() });
                d
            }
        };
        seen.push(path);
        report.included += 1;
        report.sessions.push(digest);
    }
    // files that are gone, or fell out of the window, are forgotten
    if cache.map.len() > seen.len() {
        let keep: std::collections::HashSet<&PathBuf> = seen.iter().collect();
        cache.map.retain(|p, _| keep.contains(p));
    }
    Ok(report)
}

/// [`scan`] of the sessions directory with the cache of this process.
pub fn scan_sessions(now: i64, offset: i64) -> Result<Report, String> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(Cache::default()));
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    scan(&nexo_core::paths::sessions_dir(), now, offset, &mut cache)
}

/// Now, in unix seconds, and the local offset east of UTC in seconds (0 where it cannot be told).
pub fn clock() -> (i64, i64) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs() as i64;
    (now, local_offset())
}

fn local_offset() -> i64 {
    if let Ok(o) = time::UtcOffset::current_local_offset() {
        return o.whole_seconds() as i64;
    }
    // a process with several threads cannot ask the system on some platforms: the shell can
    #[cfg(unix)]
    {
        if let Ok(out) = std::process::Command::new("date").arg("+%z").output() {
            let s = String::from_utf8_lossy(&out.stdout);
            let s = s.trim();
            if s.len() == 5 {
                let sign = if s.starts_with('-') { -1 } else { 1 };
                if let (Ok(h), Ok(m)) = (s[1..3].parse::<i64>(), s[3..5].parse::<i64>()) {
                    return sign * (h * 3600 + m * 60);
                }
            }
        }
    }
    0
}

// ---------------------------------------------------------------------------------------------
// ranking

/// What a row says.
#[derive(Debug, Clone, PartialEq)]
pub enum InsightKind {
    /// Calls over [`LONG_CTX`], and the sessions they were in.
    LongContext { sessions: usize, calls: u64 },
    /// Prompts re-sent whole, and the expected rebuilds that are not counted.
    CacheMisses { misses: u64, tokens: u64, rebuilds: u64 },
    /// One tool's results, and the command that weighed most.
    ToolOutput { tool: String, top: Option<String>, runs: u64 },
    /// One model's part of the spend.
    /// `dearer`: its calls cost more, one with another, than the rest's (else a cheaper model is no advice).
    Model { model: String, cost: f64, dearer: bool },
    /// One folder's part of the spend.
    Project { cwd: String, cost: f64 },
    /// Reasoning in what the model wrote.
    Reasoning { tokens: u64 },
    /// Sub-agents and other calls on the side (needs sessions written with the owner of each call).
    SideCalls { cost: f64 },
    /// History the curator kept out of the calls (needs the same).
    CuratorLeftOut { tokens: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Insight {
    /// 0..=1 of the usage the row applies to; rows overlap.
    pub share: f64,
    pub kind: InsightKind,
}

impl Insight {
    pub fn percent(&self) -> u64 {
        (self.share * 100.0).round() as u64
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: String,
    /// The folder the session ran in (its last name).
    pub project: String,
    /// When it was last used in the period: `today`, `yesterday` or `Nov 2`.
    pub when: String,
    pub cost: f64,
    pub cache_hit: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Insights {
    pub rows: Vec<Insight>,
    pub top: Vec<SessionRow>,
    pub totals: Tally,
    /// Sessions with a call in the period.
    pub sessions: usize,
}

/// The last name of a folder (the session's own name is not shown: it comes from what was typed).
pub fn folder_name(cwd: &str) -> String {
    let name = cwd.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("");
    if name.is_empty() {
        "session".to_string()
    } else {
        name.to_string()
    }
}

/// `today`, `yesterday`, or the date (`Nov 2`) of a day number.
pub fn day_label(day: i64, today: i64) -> String {
    match today - day {
        0 => "today".to_string(),
        1 => "yesterday".to_string(),
        _ => match time::Date::from_julian_day((day + 2_440_588) as i32) {
            Ok(d) => format!("{} {}", &d.month().to_string()[..3], d.day()),
            Err(_) => String::new(),
        },
    }
}

/// The figures of `period` ending today.
pub fn rank(report: &Report, period: Period) -> Insights {
    let from = report.today - period.days() + 1;
    let mut totals = Tally::default();
    let mut tools: BTreeMap<String, ToolTally> = BTreeMap::new();
    let mut models: BTreeMap<String, (u64, f64)> = BTreeMap::new();
    let mut projects: BTreeMap<String, f64> = BTreeMap::new();
    let mut per_session: Vec<(Arc<Digest>, Tally, i64)> = Vec::new();
    let mut long_sessions = 0;
    for d in &report.sessions {
        let mut t = Tally::default();
        let mut last = from;
        for (n, day) in d.days.range(from..=report.today) {
            last = last.max(*n);
            t.add(&day.tally);
            for (name, tt) in &day.tools {
                tools.entry(name.clone()).or_default().add(tt);
            }
            for (name, m) in &day.models {
                let e = models.entry(name.clone()).or_default();
                e.0 += m.calls;
                e.1 += m.cost;
            }
        }
        if t.calls == 0 && t.total_input() == 0 && t.output == 0 {
            continue;
        }
        if t.long_calls > 0 {
            long_sessions += 1;
        }
        *projects.entry(d.cwd.clone()).or_default() += t.cost;
        totals.add(&t);
        per_session.push((d.clone(), t, last));
    }
    let sessions = per_session.len();
    // the shares are over the calls that were read: a rewritten file's one summed line is in no row
    let input = totals.total_input().saturating_sub(totals.summed_input) as f64;
    let mut rows = Vec::new();
    let mut push = |share: f64, kind: InsightKind| {
        if share.is_finite() && share >= MIN_SHARE {
            rows.push(Insight { share: share.min(1.0), kind });
        }
    };
    if input > 0.0 {
        push(
            totals.long_input as f64 / input,
            InsightKind::LongContext { sessions: long_sessions, calls: totals.long_calls },
        );
        push(
            totals.resent as f64 / input,
            InsightKind::CacheMisses { misses: totals.misses, tokens: totals.resent, rebuilds: totals.rebuilds },
        );
        let mut by_weight: Vec<(&String, &ToolTally)> = tools.iter().filter(|(_, t)| t.weighted > 0).collect();
        by_weight.sort_by(|a, b| b.1.weighted.cmp(&a.1.weighted).then_with(|| a.0.cmp(b.0)));
        for (name, t) in by_weight.into_iter().take(2) {
            let top = t.top.iter().max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0))).map(|(k, _)| k.clone());
            push(t.weighted as f64 / input, InsightKind::ToolOutput { tool: name.clone(), top, runs: t.runs });
        }
    }
    if totals.cost > 0.0 {
        let priced = (totals.cost - totals.summed_cost).max(0.0);
        if priced > 0.0 && models.values().filter(|m| m.1 > 0.0).count() >= 2 {
            if let Some((m, (calls, c))) =
                models.iter().max_by(|a, b| a.1 .1.total_cmp(&b.1 .1).then_with(|| b.0.cmp(a.0)))
            {
                if c / priced >= MODEL_SHARE {
                    let (rest_calls, rest_cost) =
                        models.iter().filter(|(k, _)| *k != m).fold((0u64, 0.0f64), |a, (_, v)| (a.0 + v.0, a.1 + v.1));
                    let dearer = *calls > 0 && rest_calls > 0 && c / *calls as f64 > rest_cost / rest_calls as f64;
                    push(c / priced, InsightKind::Model { model: m.clone(), cost: *c, dearer });
                }
            }
        }
        if projects.values().filter(|c| **c > 0.0).count() >= 2 {
            if let Some((p, c)) = projects.iter().max_by(|a, b| a.1.total_cmp(b.1).then_with(|| b.0.cmp(a.0))) {
                push(c / totals.cost, InsightKind::Project { cwd: p.clone(), cost: *c });
            }
        }
    }
    if totals.output > 0 {
        push(totals.reasoning as f64 / totals.output as f64, InsightKind::Reasoning { tokens: totals.reasoning });
    }
    if totals.kinded_cost > 0.0 {
        push(totals.side_cost / totals.kinded_cost, InsightKind::SideCalls { cost: totals.side_cost });
    }
    if totals.held > 0 {
        let base = (totals.held + totals.held_base) as f64;
        push(totals.held as f64 / base, InsightKind::CuratorLeftOut { tokens: totals.held });
    }
    // biggest first; equal shares keep the order above
    rows.sort_by(|a, b| b.share.total_cmp(&a.share));
    rows.truncate(MAX_ROWS);
    per_session.sort_by(|a, b| {
        b.1.cost
            .total_cmp(&a.1.cost)
            .then_with(|| b.1.total_input().cmp(&a.1.total_input()))
            .then_with(|| a.0.id.cmp(&b.0.id))
    });
    let top = per_session
        .iter()
        .take(3)
        .map(|(d, t, last)| SessionRow {
            id: d.id.clone(),
            project: folder_name(&d.cwd),
            when: day_label(*last, report.today),
            cost: t.cost,
            cache_hit: t.cache_hit(),
        })
        .collect();
    Insights { rows, top, totals, sessions }
}
