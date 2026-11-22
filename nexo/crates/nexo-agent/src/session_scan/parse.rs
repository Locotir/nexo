//! Reading one saved session file into its [`Digest`].

use super::model::*;
use crate::session::UsageKind;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::BufRead;

#[derive(Deserialize, Default)]
pub(super) struct MetaLite {
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    created: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    provider: String,
}

#[derive(Deserialize)]
pub(super) struct ModelLite {
    #[serde(default)]
    model: String,
    #[serde(default)]
    provider: String,
}

#[derive(Deserialize, Default)]
pub(super) struct UsageLite {
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
    #[serde(default)]
    transformations: u64,
}

#[derive(Deserialize)]
pub(super) struct UsageLine {
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
pub(super) struct MessageLine {
    #[serde(default)]
    message: MessageLite,
}

#[derive(Deserialize, Default)]
pub(super) struct MessageLite {
    #[serde(default)]
    content: Value,
}

#[derive(Deserialize)]
pub(super) struct OutputLine {
    #[serde(default)]
    call_id: String,
    #[serde(default)]
    output: OutputLite,
    /// The length of the text a slim line left to the tool result (its `content` is empty then).
    #[serde(default)]
    result_chars: Option<usize>,
}

#[derive(Deserialize, Default)]
pub(super) struct OutputLite {
    #[serde(default)]
    content: String,
}

/// A tool result still in the context.
pub(super) struct Live {
    tool: String,
    cmd: Option<String>,
    tokens: u64,
}

pub(super) struct Walk {
    offset: i64,
    digest: Digest,
    /// The day of the last message: where a line with no time of its own falls.
    day: i64,
    created_day: i64,
    model: String,
    provider: String,
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
            provider: String::new(),
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
                    self.provider = m.provider;
                    if let Some(d) = self.day_of_ts(&m.created) {
                        self.day = d;
                        self.created_day = d;
                    }
                }
            }
            "model" => {
                if let Ok(m) = serde_json::from_str::<ModelLite>(line) {
                    if m.model != self.model || m.provider != self.provider {
                        self.finish_cache_segment();
                        self.rebuild = true;
                    }
                    self.model = m.model;
                    self.provider = m.provider;
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
        let chars = o.result_chars.unwrap_or_else(|| o.output.content.chars().count()) as u64;
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
        let day =
            if summed { self.created_day } else { u.ts.as_deref().and_then(|t| self.day_of_ts(t)).unwrap_or(self.day) };
        let model = u.model.clone().unwrap_or_else(|| self.model.clone());
        let main = matches!(u.kind, None | Some(UsageKind::Main));
        // the chain of conversation calls the cache is judged on
        let mut miss = None;
        if summed {
            self.finish_cache_segment();
            self.prev = None;
            self.rebuild = true;
        } else if main {
            let switched = self.last_model.as_ref().is_some_and(|m| *m != model);
            if switched {
                self.finish_cache_segment();
            }
            if u.usage.cache_read > 0 || u.usage.cache_write > 0 {
                self.cache_seen = true;
            }
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
        t.transformations += u.usage.transformations;
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

    // Cache evidence belongs to this main-call model/provider segment only. Side calls and
    // another provider's cache cannot establish that this chain supports cache reporting.
    fn finish_cache_segment(&mut self) {
        let misses = std::mem::take(&mut self.maybe_misses);
        if self.cache_seen {
            for (day, total) in misses {
                let t = &mut self.day_mut(day).tally;
                t.misses += 1;
                t.resent += total;
            }
        }
        self.cache_seen = false;
    }

    fn finish(mut self) -> Digest {
        self.finish_cache_segment();
        self.digest
    }
}

/// The `ts` of a `message` line, read without parsing the line (it follows `index`).
pub(super) fn field_ts(line: &str) -> Option<&str> {
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
pub(super) const VERBS: [&str; 30] = [
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
pub(super) const WRAPPERS: [&str; 7] = ["sudo", "env", "nice", "nohup", "time", "exec", "command"];

/// A label for a shell command: the program, and its verb when it is a program that has them.
/// Arguments are never kept: they are where paths, hosts and secrets are.
pub(super) fn command_head(cmd: &str) -> Option<String> {
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
pub(super) fn unix_of(ts: &str) -> Option<i64> {
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
