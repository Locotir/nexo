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
        let tokens = nexo_core::tokens::text_tokens(&o.output.content);
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    const T0: i64 = 1_793_000_000; // 2026-10-26, in the afternoon UTC

    fn rfc(unix: i64) -> String {
        time::OffsetDateTime::from_unix_timestamp(unix)
            .unwrap()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    }

    fn meta(cwd: &str, model: &str, at: i64) -> Value {
        json!({"type":"meta","id":"s","cwd":cwd,"created":rfc(at),"updated":rfc(at),"provider":"p","model":model,"title":"","messages":0})
    }

    fn msg(i: usize, at: i64, role: &str, parts: Value) -> Value {
        json!({"type":"message","index":i,"ts":rfc(at),"message":{"role":role,"content":parts}})
    }

    fn call(id: &str, name: &str, cmd: &str) -> Value {
        json!([{"type":"tool_use","id":id,"name":name,"input":{"command":cmd}}])
    }

    fn output(id: &str, chars: usize) -> Value {
        json!({"type":"tool_output","call_id":id,"output":{"content":"x".repeat(chars),"is_error":false,"display":{"kind":"plain"}}})
    }

    fn usage(input: u64, cache_read: u64, cache_write: u64, out: u64, reasoning: u64, cost: f64) -> Value {
        json!({"type":"usage","usage":{"input":input,"output":out,"cache_read":cache_read,"cache_write":cache_write,"reasoning":reasoning},"cost_usd":cost})
    }

    fn text(s: &str) -> Value {
        json!([{"type":"text","text":s}])
    }

    fn digest(lines: &[Value]) -> Digest {
        digest_at(lines, 0)
    }

    fn digest_at(lines: &[Value], offset: i64) -> Digest {
        let body: String = lines.iter().map(|l| l.to_string() + "\n").collect();
        digest_reader("s", Cursor::new(body.into_bytes()), offset)
    }

    fn today() -> i64 {
        day_of(T0, 0)
    }

    fn report(ds: Vec<Digest>) -> Report {
        let n = ds.len();
        Report { sessions: ds.into_iter().map(Arc::new).collect(), eligible: n, included: n, parsed: n, today: today() }
    }

    fn total(d: &Digest) -> Tally {
        let mut t = Tally::default();
        for day in d.days.values() {
            t.add(&day.tally);
        }
        t
    }

    #[test]
    fn totals_are_split_by_the_day_of_the_message_before() {
        let d = digest(&[
            meta("/p", "m", T0 - 86_400),
            msg(0, T0 - 86_400, "user", text("a")),
            usage(100, 900, 0, 50, 10, 0.5),
            msg(1, T0, "user", text("b")),
            usage(200, 800, 0, 20, 0, 0.25),
        ]);
        assert_eq!(d.days.len(), 2);
        let yesterday = &d.days[&(today() - 1)].tally;
        assert_eq!((yesterday.input, yesterday.cache_read, yesterday.reasoning, yesterday.calls), (100, 900, 10, 1));
        assert_eq!(d.days[&today()].tally.cost, 0.25);
        assert_eq!(total(&d).total_input(), 2000);
    }

    #[test]
    fn a_day_is_cut_at_the_local_offset() {
        // 23:30 UTC is already tomorrow two hours east
        let at = today() * 86_400 + 23 * 3600 + 1800;
        let lines = [meta("/p", "m", at), msg(0, at, "user", text("a")), usage(1, 0, 0, 1, 0, 0.1)];
        assert!(digest_at(&lines, 0).days.contains_key(&today()));
        assert!(digest_at(&lines, 2 * 3600).days.contains_key(&(today() + 1)));
        assert!(digest_at(&lines, -3600).days.contains_key(&today()));
    }

    #[test]
    fn long_context_counts_the_calls_over_the_line() {
        let d = digest(&[
            meta("/p", "m", T0),
            msg(0, T0, "user", text("a")),
            usage(10_000, 0, 0, 1, 0, 0.1),
            usage(10_000, 150_000, 0, 1, 0, 0.1),
            usage(100, 149_900, 0, 1, 0, 0.1), // exactly the line: not over it
        ]);
        let t = total(&d);
        assert_eq!((t.calls, t.long_calls, t.long_input), (3, 1, 160_000));
        let r = rank(&report(vec![d]), Period::Week);
        let row = r.rows.iter().find(|r| matches!(r.kind, InsightKind::LongContext { .. })).unwrap();
        assert_eq!(row.kind, InsightKind::LongContext { sessions: 1, calls: 1 });
        assert_eq!(row.percent(), 50); // 160k of the 320k sent
    }

    #[test]
    fn a_miss_is_a_big_resend_and_a_rebuild_is_not() {
        let d = digest(&[
            meta("/p", "m", T0),
            msg(0, T0, "user", text("a")),
            usage(30_000, 0, 0, 1, 0, 0.1),   // the first call: nothing before it
            usage(500, 29_500, 0, 1, 0, 0.1), // cached
            usage(31_000, 0, 0, 1, 0, 0.1),   // miss: the prompt was sent again
            usage(500, 30_500, 0, 1, 0, 0.1), // cached again
            json!({"type":"reset"}),
            usage(31_000, 0, 0, 1, 0, 0.1), // after a reset: expected
            usage(1_000, 0, 0, 1, 0, 0.1),  // 31k before it, none cached: a miss
        ]);
        let t = total(&d);
        assert_eq!((t.misses, t.resent, t.rebuilds), (2, 32_000, 1));
    }

    #[test]
    fn a_small_prompt_is_never_a_miss() {
        let d = digest(&[
            meta("/p", "m", T0),
            msg(0, T0, "user", text("a")),
            usage(5_000, 0, 0, 1, 0, 0.1),
            usage(5_000, 0, 0, 1, 0, 0.1),
            usage(100, 4_900, 0, 1, 0, 0.1),
        ]);
        assert_eq!(total(&d).misses, 0);
    }

    #[test]
    fn a_session_with_no_cache_has_no_misses() {
        let d = digest(&[
            meta("/p", "m", T0),
            msg(0, T0, "user", text("a")),
            usage(30_000, 0, 0, 1, 0, 0.1),
            usage(31_000, 0, 0, 1, 0, 0.1),
            usage(32_000, 0, 0, 1, 0, 0.1),
        ]);
        assert_eq!(total(&d).misses, 0, "a provider that reports no cache has nothing to miss");
    }

    #[test]
    fn a_model_switch_is_a_rebuild_not_a_miss() {
        let d = digest(&[
            meta("/p", "a", T0),
            msg(0, T0, "user", text("a")),
            usage(30_000, 0, 10, 1, 0, 0.1),
            json!({"type":"model","provider":"p","model":"b"}),
            usage(30_000, 0, 0, 1, 0, 0.1),
        ]);
        let t = total(&d);
        assert_eq!((t.misses, t.rebuilds), (0, 1));
        let m = &d.days[&today()].models;
        assert_eq!((m["a"].calls, m["b"].calls), (1, 1), "the model in force takes the call");
    }

    #[test]
    fn tool_output_is_weighted_by_the_calls_that_carried_it() {
        // a run of letters costs 0.14 token a character past the first eight, so the sizes are read back
        let (big, small) =
            (nexo_core::tokens::text_tokens(&"x".repeat(4_000)), nexo_core::tokens::text_tokens(&"x".repeat(400)));
        let d = digest(&[
            meta("/p", "m", T0),
            msg(0, T0, "user", text("run")),
            msg(1, T0, "assistant", call("c1", "bash", "FOO=1 cargo test --all")),
            output("c1", 4_000),
            usage(2_000, 0, 0, 1, 0, 0.1),
            usage(2_500, 0, 0, 1, 0, 0.1),
            msg(2, T0, "assistant", call("c2", "read_file", "")),
            output("c2", 400),
            usage(3_000, 0, 0, 1, 0, 0.1),
        ]);
        let day = &d.days[&today()];
        let bash = &day.tools["bash"];
        assert_eq!((bash.runs, bash.tokens, bash.weighted), (1, big, 3 * big));
        assert_eq!(bash.top.get("cargo test"), Some(&(3 * big)), "the variable is left out, two words kept");
        let read = &day.tools["read_file"];
        assert_eq!((read.tokens, read.weighted), (small, small));
        assert!(read.top.is_empty(), "an empty command is no label");
    }

    #[test]
    fn a_reset_lets_go_of_the_tool_output_and_an_orphan_output_is_ignored() {
        let d = digest(&[
            meta("/p", "m", T0),
            msg(0, T0, "assistant", call("c1", "bash", "ls")),
            output("c1", 4_000),
            output("ghost", 4_000), // the message of this call is gone
            json!({"type":"reset"}),
            usage(2_000, 0, 0, 1, 0, 0.1),
        ]);
        let bash = &d.days[&today()].tools["bash"];
        assert_eq!((bash.runs, bash.weighted), (1, 0));
        assert!(!d.days[&today()].tools.contains_key("ghost"));
    }

    #[test]
    fn the_part_of_a_tool_is_a_share_of_the_input() {
        // the output weighs its tokens in each of the two calls, so with an input of twice that size it is half of what was sent
        let t = nexo_core::tokens::text_tokens(&"x".repeat(8_000));
        let d = digest(&[
            meta("/p", "m", T0),
            msg(0, T0, "assistant", call("c1", "bash", "cargo test")),
            output("c1", 8_000),
            usage(2 * t, 0, 0, 1, 0, 0.1),
            usage(2 * t, 0, 0, 1, 0, 0.1),
        ]);
        let r = rank(&report(vec![d]), Period::Day);
        let row = r.rows.iter().find(|r| matches!(r.kind, InsightKind::ToolOutput { .. })).unwrap();
        assert_eq!(row.kind, InsightKind::ToolOutput { tool: "bash".into(), top: Some("cargo test".into()), runs: 1 });
        assert_eq!(row.percent(), 50);
    }

    #[test]
    fn a_line_that_sums_a_rewritten_file_is_not_a_call() {
        // a rewrite stamps every message with one instant and ends with one summed usage line
        let d = digest(&[
            meta("/p", "m", T0 - 3 * 86_400),
            msg(0, T0, "user", text("a")),
            msg(1, T0, "assistant", text("b")),
            msg(2, T0, "user", text("c")),
            usage(1_000_000, 0, 0, 5, 0, 7.0),
            msg(3, T0 + 5, "assistant", text("d")),
            usage(100, 900, 0, 1, 0, 0.1),
        ]);
        let t = total(&d);
        assert_eq!((t.calls, t.long_calls, t.cost), (1, 0, 7.1));
        assert_eq!(d.days[&(today() - 3)].tally.input, 1_000_000, "on the day the session began");
        let marked = digest(&[
            meta("/p", "m", T0),
            msg(0, T0, "user", text("a")),
            json!({"type":"usage","usage":{"input":9,"output":1,"cache_read":0,"cache_write":0,"reasoning":0},"cost_usd":1.0,"ts":rfc(T0),"kind":"sum"}),
        ]);
        assert_eq!(total(&marked).calls, 0);
        assert_eq!(total(&marked).cost, 1.0);
    }

    #[test]
    fn the_second_call_of_a_live_session_is_a_call() {
        // two messages at one instant before the first usage: still a call
        let d = digest(&[
            meta("/p", "m", T0),
            msg(0, T0, "user", text("a")),
            msg(1, T0, "assistant", text("b")),
            usage(100, 0, 0, 1, 0, 0.1),
        ]);
        assert_eq!(total(&d).calls, 1);
    }

    #[test]
    fn lines_of_the_new_kind_are_read_and_old_lines_still_are() {
        let d = digest(&[
            meta("/p", "main-model", T0),
            msg(0, T0, "user", text("a")),
            json!({"type":"usage","usage":{"input":100,"output":10,"cache_read":0,"cache_write":0,"reasoning":0},"cost_usd":0.2,"ts":rfc(T0),"model":"main-model","kind":"main","held":300}),
            json!({"type":"usage","usage":{"input":50,"output":5,"cache_read":0,"cache_write":0,"reasoning":0},"cost_usd":0.1,"ts":rfc(T0),"model":"small","kind":"sub"}),
            usage(10, 0, 0, 1, 0, 0.0), // an old line: no owner
        ]);
        let t = total(&d);
        assert_eq!((t.kinded_cost, t.side_cost), (0.30000000000000004, 0.1));
        assert_eq!((t.held, t.held_base), (300, 100));
        assert_eq!(t.unpriced, 1);
        let m = &d.days[&today()].models;
        assert_eq!(m["small"].calls, 1);
        assert_eq!(m["main-model"].calls, 2, "the old line takes the model in force");
    }

    #[test]
    fn a_cut_last_line_and_unknown_lines_are_skipped() {
        let mut body = String::new();
        for l in [meta("/p", "m", T0), msg(0, T0, "user", text("a")), usage(10, 0, 0, 1, 0, 0.1)] {
            body += &(l.to_string() + "\n");
        }
        body += "{\"type\":\"future\",\"x\":1}\n{\"type\":\"goal\",\"text\":null}\nnot json\n";
        body += "{\"type\":\"usage\",\"usage\":{\"input\":99999";
        let d = digest_reader("s", Cursor::new(body.into_bytes()), 0);
        assert_eq!(total(&d).input, 10);
        assert_eq!(d.cwd, "/p");
    }

    #[test]
    fn bytes_that_are_not_utf8_spoil_only_their_line() {
        let mut body = (meta("/p", "m", T0).to_string() + "\n").into_bytes();
        body.extend_from_slice(b"{\"type\":\"title\",\"title\":\"bad \xff\xfe\"}\n");
        body.extend_from_slice((usage(10, 0, 0, 1, 0, 0.1).to_string() + "\n").as_bytes());
        let d = digest_reader("s", Cursor::new(body), 0);
        assert_eq!(total(&d).input, 10);
    }

    #[test]
    fn rows_under_a_tenth_are_dropped_and_the_rest_ranked() {
        // 5% long context, 30% misses
        let d = digest(&[
            meta("/p", "m", T0),
            msg(0, T0, "user", text("a")),
            usage(30_000, 0, 5, 100, 60, 0.1),
            usage(500, 29_500, 0, 100, 60, 0.1),
            usage(30_000, 0, 0, 100, 60, 0.1),
        ]);
        let r = rank(&report(vec![d]), Period::Day);
        let kinds: Vec<&str> = r
            .rows
            .iter()
            .map(|r| match r.kind {
                InsightKind::CacheMisses { .. } => "misses",
                InsightKind::Reasoning { .. } => "reasoning",
                InsightKind::LongContext { .. } => "long",
                _ => "other",
            })
            .collect();
        // reasoning is 60% of the output, misses a third of the input
        assert_eq!(kinds, ["reasoning", "misses"]);
        assert!(r.rows[0].share > r.rows[1].share);
        assert!(r.rows.iter().all(|r| r.share >= MIN_SHARE));
    }

    #[test]
    fn at_most_six_rows() {
        let mut lines =
            vec![meta("/a", "m1", T0), msg(0, T0, "assistant", call("c1", "bash", "cargo test")), output("c1", 40_000)];
        lines.push(msg(1, T0, "assistant", call("c2", "read_file", "")));
        lines.push(output("c2", 40_000));
        lines.push(json!({"type":"usage","usage":{"input":200000,"output":100,"cache_read":0,"cache_write":10,"reasoning":90},"cost_usd":1.0,"ts":rfc(T0),"model":"m1","kind":"sub","held":100000}));
        lines.push(usage(40_000, 0, 0, 100, 90, 1.0));
        lines.push(json!({"type":"usage","usage":{"input":200000,"output":100,"cache_read":0,"cache_write":0,"reasoning":90},"cost_usd":3.0,"ts":rfc(T0),"model":"m2","kind":"main","held":100000}));
        let mut b = digest(&lines);
        b.cwd = "/b".into();
        let a = digest(&lines[..3]);
        let r = rank(&report(vec![digest(&lines), b, a]), Period::Day);
        assert!(r.rows.len() <= MAX_ROWS);
        assert!(r.rows.windows(2).all(|w| w[0].share >= w[1].share));
    }

    #[test]
    fn model_and_project_rows_need_two_to_compare() {
        let one = |cwd: &str, model: &str, cost: f64| {
            let mut d = digest(&[meta(cwd, model, T0), msg(0, T0, "user", text("a")), usage(100, 0, 0, 1, 0, cost)]);
            d.cwd = cwd.into();
            d
        };
        let alone = rank(&report(vec![one("/a", "m1", 1.0)]), Period::Day);
        assert!(!alone.rows.iter().any(|r| matches!(r.kind, InsightKind::Model { .. } | InsightKind::Project { .. })));
        let two = rank(&report(vec![one("/a", "m1", 4.0), one("/b", "m2", 1.0)]), Period::Day);
        let model = two.rows.iter().find(|r| matches!(r.kind, InsightKind::Model { .. })).unwrap();
        assert_eq!(model.kind, InsightKind::Model { model: "m1".into(), cost: 4.0, dearer: true });
        assert_eq!(model.percent(), 80);
        let project = two.rows.iter().find(|r| matches!(r.kind, InsightKind::Project { .. })).unwrap();
        assert_eq!(project.kind, InsightKind::Project { cwd: "/a".into(), cost: 4.0 });
        // 55% is not enough to call a model out? it is (half or more); 45% is not
        let even = rank(&report(vec![one("/a", "m1", 2.0), one("/b", "m2", 2.5), one("/c", "m3", 2.4)]), Period::Day);
        assert!(!even.rows.iter().any(|r| matches!(r.kind, InsightKind::Model { .. })));
    }

    #[test]
    fn the_phase_two_rows_are_absent_without_the_new_fields() {
        let d = digest(&[meta("/p", "m", T0), msg(0, T0, "user", text("a")), usage(100, 0, 0, 1, 0, 1.0)]);
        let r = rank(&report(vec![d]), Period::Day);
        assert!(!r
            .rows
            .iter()
            .any(|r| matches!(r.kind, InsightKind::SideCalls { .. } | InsightKind::CuratorLeftOut { .. })));
    }

    #[test]
    fn side_calls_and_the_curator_have_their_rows() {
        let d = digest(&[
            meta("/p", "m", T0),
            msg(0, T0, "user", text("a")),
            json!({"type":"usage","usage":{"input":1000,"output":1,"cache_read":0,"cache_write":0,"reasoning":0},"cost_usd":0.6,"ts":rfc(T0),"kind":"main","held":3000}),
            json!({"type":"usage","usage":{"input":10,"output":1,"cache_read":0,"cache_write":0,"reasoning":0},"cost_usd":0.4,"ts":rfc(T0),"kind":"sub"}),
        ]);
        let r = rank(&report(vec![d]), Period::Day);
        let side = r.rows.iter().find(|r| matches!(r.kind, InsightKind::SideCalls { .. })).unwrap();
        assert_eq!(side.percent(), 40);
        let cur = r.rows.iter().find(|r| matches!(r.kind, InsightKind::CuratorLeftOut { .. })).unwrap();
        assert_eq!(cur.kind, InsightKind::CuratorLeftOut { tokens: 3000 });
        assert_eq!(cur.percent(), 75); // 3000 of the 4000 the history would have sent
    }

    #[test]
    fn the_period_picks_its_days() {
        let d = digest(&[
            meta("/p", "m", T0 - 10 * 86_400),
            msg(0, T0 - 10 * 86_400, "user", text("a")),
            usage(100, 0, 0, 1, 0, 1.0),
            msg(1, T0 - 3 * 86_400, "user", text("a")),
            usage(100, 0, 0, 1, 0, 2.0),
            msg(2, T0, "user", text("a")),
            usage(100, 0, 0, 1, 0, 4.0),
        ]);
        let r = report(vec![d]);
        assert_eq!(rank(&r, Period::Day).totals.cost, 4.0);
        assert_eq!(rank(&r, Period::Week).totals.cost, 6.0);
        assert_eq!(rank(&r, Period::Month).totals.cost, 7.0);
        assert_eq!(rank(&report(vec![]), Period::Week), Insights::default());
    }

    #[test]
    fn the_biggest_sessions_are_three_by_cost() {
        let mk = |id: &str, cwd: &str, cost: f64, read: u64| {
            let mut d = digest(&[meta(cwd, "m", T0), msg(0, T0, "user", text("a")), usage(100, read, 0, 1, 0, cost)]);
            d.id = id.into();
            d.cwd = cwd.into();
            d
        };
        let r = rank(
            &report(vec![
                mk("a", "/w/one", 1.0, 0),
                mk("b", "", 5.0, 900),
                mk("c", "/w/three/", 3.0, 0),
                mk("d", "/w/four", 2.0, 0),
            ]),
            Period::Day,
        );
        let got: Vec<(&str, &str, &str)> =
            r.top.iter().map(|s| (s.id.as_str(), s.project.as_str(), s.when.as_str())).collect();
        assert_eq!(
            got,
            [("b", "session", "today"), ("c", "three", "today"), ("d", "four", "today")],
            "a session is named by its folder, never by what was typed"
        );
        assert_eq!(r.top[0].cache_hit, Some(90));
        assert_eq!(r.sessions, 4);
    }

    // --- the directory -------------------------------------------------------------------

    fn write(dir: &Path, name: &str, lines: &[Value]) -> PathBuf {
        let p = dir.join(name);
        let body: String = lines.iter().map(|l| l.to_string() + "\n").collect();
        std::fs::write(&p, body).unwrap();
        p
    }

    fn simple(cost: f64) -> Vec<Value> {
        vec![meta("/p", "m", T0), msg(0, T0, "user", text("a")), usage(100, 0, 0, 1, 0, cost)]
    }

    fn now_of(dir: &Path) -> i64 {
        let m = std::fs::metadata(dir).unwrap().modified().unwrap();
        m.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
    }

    #[test]
    fn the_cache_parses_only_what_changed() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.jsonl", &simple(1.0));
        let b = write(dir.path(), "b.jsonl", &simple(2.0));
        let now = now_of(dir.path());
        let mut cache = Cache::default();
        let first = scan(dir.path(), now, 0, &mut cache).unwrap();
        assert_eq!((first.included, first.parsed), (2, 2));
        let again = scan(dir.path(), now, 0, &mut cache).unwrap();
        assert_eq!((again.included, again.parsed), (2, 0), "unchanged files are not read");
        // a file that grew is read again, whole
        let mut grown = simple(2.0);
        grown.push(usage(50, 0, 0, 1, 0, 0.5));
        write(dir.path(), "b.jsonl", &grown);
        assert!(std::fs::metadata(&b).unwrap().len() > 0);
        let third = scan(dir.path(), now, 0, &mut cache).unwrap();
        assert_eq!(third.parsed, 1);
        assert_eq!(third.sessions.iter().map(|d| total(d).calls).sum::<u64>(), 3);
        // another offset cuts the days elsewhere: nothing is reused
        assert_eq!(scan(dir.path(), now, 3600, &mut cache).unwrap().parsed, 2);
    }

    #[test]
    fn only_session_files_at_the_top_are_read() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.jsonl", &simple(1.0));
        write(dir.path(), "a.jsonl.lock", &simple(1.0));
        write(dir.path(), "a.jsonl.123.tmp", &simple(1.0));
        write(dir.path(), "notes.txt", &simple(1.0));
        std::fs::create_dir(dir.path().join("trash")).unwrap();
        write(&dir.path().join("trash"), "gone.jsonl", &simple(1.0));
        let r = scan(dir.path(), now_of(dir.path()), 0, &mut Cache::default()).unwrap();
        assert_eq!((r.eligible, r.included), (1, 1));
        assert_eq!(r.sessions[0].id, "a");
    }

    #[test]
    fn the_caps_leave_a_note() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            write(dir.path(), &format!("s{i}.jsonl"), &simple(1.0));
        }
        let big = dir.path().join("big.jsonl");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_FILE + 1).unwrap();
        let r = scan(dir.path(), now_of(dir.path()), 0, &mut Cache::default()).unwrap();
        assert_eq!((r.eligible, r.included), (6, 5), "a file over the size cap is not read");
        assert_eq!(r.partial().as_deref(), Some("partial: 5 of 6 sessions"));
        let ok = Report { eligible: 3, included: 3, ..Report::default() };
        assert_eq!(ok.partial(), None);
    }

    #[test]
    fn old_files_are_not_opened_and_a_missing_directory_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.jsonl", &simple(1.0));
        let far = now_of(dir.path()) + (KEEP_DAYS + 1) * 86_400;
        let r = scan(dir.path(), far, 0, &mut Cache::default()).unwrap();
        assert_eq!((r.eligible, r.parsed), (0, 0));
        let none = scan(&dir.path().join("nothing-here"), far, 0, &mut Cache::default()).unwrap();
        assert!(none.sessions.is_empty());
        assert_eq!(none.today, day_of(far, 0));
    }

    #[test]
    fn a_directory_that_cannot_be_read_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), "a.jsonl", &simple(1.0));
        let err = scan(&file, 0, 0, &mut Cache::default()).unwrap_err();
        assert!(err.starts_with("cannot read"), "{err}");
    }

    #[test]
    fn periods_have_keys() {
        assert_eq!(Period::from_key('w'), Some(Period::Week));
        assert_eq!(Period::from_key('x'), None);
        assert_eq!(Period::ALL.map(|p| p.days()), [1, 7, 30]);
    }

    #[test]
    fn a_command_label_is_the_program_and_a_verb_never_an_argument() {
        assert_eq!(command_head("cargo test --all").as_deref(), Some("cargo test"));
        assert_eq!(command_head("A=1 B=2 ls -la /x").as_deref(), Some("ls"));
        assert_eq!(command_head("sudo -u root cargo build").as_deref(), None);
        assert_eq!(command_head("sudo cargo build").as_deref(), Some("cargo build"));
        assert_eq!(command_head("curl --token=abc https://x").as_deref(), Some("curl"));
        assert_eq!(command_head("mysql -pSecret db").as_deref(), Some("mysql"));
        assert_eq!(command_head("echo hunter2").as_deref(), Some("echo"));
        assert_eq!(command_head("git Secret-Branch").as_deref(), Some("git"), "a verb is lower case");
        assert_eq!(command_head("git commit -m 'x'").as_deref(), Some("git commit"));
        assert_eq!(command_head("/usr/bin/ssh user@host").as_deref(), Some("ssh"));
        assert_eq!(command_head("   ").as_deref(), None);
        assert_eq!(command_head("$TOKEN run").as_deref(), None);
        assert_eq!(command_head(&"z".repeat(60)).as_deref(), None);
    }

    #[test]
    fn a_message_with_wide_characters_at_the_cut_is_read() {
        // the time is read from the head of the line: that head may end inside a character
        for pad in 0..4 {
            let words = "あ".repeat(60) + &"x".repeat(pad);
            let d = digest(&[
                meta("/p", "m", T0),
                msg(0, T0, "user", text(&words)),
                msg(1, T0, "user", json!([{"type":"text","text":"é".repeat(80)}])),
                usage(10, 0, 0, 1, 0, 0.1),
            ]);
            assert_eq!(total(&d).calls, 1);
        }
        let line = format!("{{\"type\":\"message\",\"x\":\"{}\"}}", "é".repeat(100));
        assert_eq!(field_ts(&line), None);
    }

    #[test]
    fn a_summed_line_is_not_in_the_base_of_the_shares() {
        // a million tokens and 9 dollars summed from before; then two calls on two models
        let d = digest(&[
            meta("/p", "m1", T0),
            msg(0, T0, "user", text("a")),
            msg(1, T0, "assistant", text("b")),
            msg(2, T0, "user", text("c")),
            json!({"type":"usage","usage":{"input":1_000_000,"output":5,"cache_read":0,"cache_write":0,"reasoning":0},"cost_usd":9.0,"ts":rfc(T0),"kind":"sum"}),
            json!({"type":"usage","usage":{"input":200_000,"output":5,"cache_read":0,"cache_write":0,"reasoning":0},"cost_usd":0.6,"ts":rfc(T0),"model":"m1","kind":"main"}),
            json!({"type":"usage","usage":{"input":1_000,"output":5,"cache_read":0,"cache_write":0,"reasoning":0},"cost_usd":0.2,"ts":rfc(T0),"model":"m2","kind":"main"}),
        ]);
        let r = rank(&report(vec![d]), Period::Day);
        assert_eq!((r.totals.summed_input, r.totals.summed_cost), (1_000_000, 9.0));
        let long = r.rows.iter().find(|r| matches!(r.kind, InsightKind::LongContext { .. })).unwrap();
        assert_eq!(long.percent(), 100, "200k of the 201k that were read, not of 1.2M");
        let model = r.rows.iter().find(|r| matches!(r.kind, InsightKind::Model { .. })).unwrap();
        assert_eq!(model.percent(), 75, "0.6 of the 0.8 that were told apart, not of 9.8");
    }

    #[test]
    fn tool_output_cannot_weigh_more_than_the_call_that_carried_it() {
        // the estimate says 10k tokens; the call sent 2k
        let d = digest(&[
            meta("/p", "m", T0),
            msg(0, T0, "assistant", call("c1", "bash", "cargo test")),
            output("c1", 40_000),
            usage(2_000, 0, 0, 1, 0, 0.1),
        ]);
        assert_eq!(d.days[&today()].tools["bash"].weighted, 2_000);
        let r = rank(&report(vec![d]), Period::Day);
        let row = r.rows.iter().find(|r| matches!(r.kind, InsightKind::ToolOutput { .. })).unwrap();
        assert_eq!(row.percent(), 100);
    }

    #[test]
    fn a_cheaper_model_is_advice_only_for_a_dearer_one() {
        let one = |cwd: &str, model: &str, calls: usize, cost: f64| {
            let mut lines = vec![meta(cwd, model, T0), msg(0, T0, "user", text("a"))];
            for _ in 0..calls {
                lines.push(usage(100, 0, 0, 1, 0, cost / calls as f64));
            }
            let mut d = digest(&lines);
            d.cwd = cwd.into();
            d
        };
        let dearer = |a: (usize, f64), b: (usize, f64)| {
            let r = rank(&report(vec![one("/a", "m1", a.0, a.1), one("/b", "m2", b.0, b.1)]), Period::Day);
            match r.rows.iter().find_map(|r| match &r.kind {
                InsightKind::Model { dearer, .. } => Some(*dearer),
                _ => None,
            }) {
                Some(d) => d,
                None => panic!("no model row"),
            }
        };
        assert!(dearer((2, 4.0), (10, 1.0)), "2.00 a call against 0.10");
        assert!(!dearer((20, 4.0), (1, 1.0)), "0.20 a call against 1.00: it is the cheap one doing the work");
    }

    #[test]
    fn a_session_is_named_by_its_folder_and_the_day_it_was_last_used() {
        assert_eq!(folder_name("/home/me/code/nexo"), "nexo");
        assert_eq!(folder_name("/home/me/code/nexo/"), "nexo");
        assert_eq!(folder_name("C:\\work\\api"), "api");
        assert_eq!(folder_name(""), "session");
        assert_eq!(folder_name("/"), "session");
        let t = today();
        assert_eq!(day_label(t, t), "today");
        assert_eq!(day_label(t - 1, t), "yesterday");
        // T0 is 2026-10-26
        assert_eq!(day_label(t - 3, t), "Oct 23");
        assert_eq!(day_label(t + 7, t + 20), "Nov 2");
    }
}
