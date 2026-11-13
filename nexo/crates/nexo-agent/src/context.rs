//! What the next model call sees: everything (`Plain`), everything with a
//! summary when it grows past a size (`Compact`), or what the context curator
//! built into nexo keeps (`BuiltIn`).

use async_trait::async_trait;
use nexo_core::config::ContextConfig;
use nexo_core::message::{estimate_tokens, Message, Role};

#[cfg(test)]
mod tests;

/// What to send.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub messages: Vec<Message>,
    pub tokens: u64,
    /// Something the person should hear (a fallback, a restart).
    pub note: Option<String>,
    /// What the curator holds and what of it the call takes (only the curator).
    pub memory: Option<Memory>,
}

/// The curator's memory: a cell per message of the history, in zones (the
/// anchor and the conversation's topics), as `nexo_curator::Reply::memory` gives it.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Memory {
    pub zones: Vec<nexo_core::ContextZone>,
    pub cells: Vec<MemoryCell>,
    /// What the call carries beyond the conversation and what it holds back (set by the policy, not read
    /// from the curator's memory).
    #[serde(skip)]
    pub curated: Vec<nexo_core::CuratedItem>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct MemoryCell {
    pub zone: u32,
    pub tokens: u64,
    /// `whole`, `cut`, `recalled` or `out`.
    pub state: String,
}

#[async_trait]
pub trait ContextPolicy: Send {
    fn name(&self) -> &str;
    async fn view(&mut self, system_tokens: u64, history: &[Message]) -> anyhow::Result<View>;
    /// The version of the history the next `view` gets (see `History::version`): the same version
    /// as the last view's, with no fewer messages, says only appends happened. Optional; without it
    /// the policy compares the whole history.
    fn history_version(&mut self, _version: u64) {}
    /// The loop replaced the history with a summary of it (compaction).
    fn compacted(&mut self) {
        self.reset()
    }
    /// The history was replaced (compaction, a new session).
    fn reset(&mut self);
    /// Tokens of history above which the loop should compact (the `compact` policy).
    fn compact_at(&self) -> Option<u64> {
        None
    }
    /// Fixed instructions this policy adds at the end of the system prompt
    /// (the same on every request, so they stay in the cache). `tools`: the
    /// names of the tools the model is offered, which decide how it is asked to
    /// keep its notes.
    fn directives(&self, _tools: &[&str]) -> Option<&'static str> {
        None
    }
    /// Whether the model is offered the `recall` tool (a message the view left out, back as it was).
    fn recall(&self) -> bool {
        false
    }
    /// The tools the model is offered (their names), before each view.
    fn tools(&mut self, _names: &[String]) {}
    /// The tokens the model takes in one call, less its reply, before each view (the curator keeps
    /// the view under it).
    fn window(&mut self, _tokens: u64) {}
    /// The model's prices per million input tokens, uncached and read from the cache, before each view
    /// (the curator weighs what a cut saves against what it costs the cache at their ratio).
    fn prices(&mut self, _input: f64, _cache_read: f64) {}
    /// What the provider said of the call made with the last view: the input tokens it read from its
    /// cache, and all of them, from which the curator learns how long the cache keeps a view.
    fn usage(&mut self, _read: u64, _input: u64) {}
    /// Whether the view keeps under the tokens `window` gives it, so a window learned smaller needs a
    /// new view rather than a compaction.
    fn fits_window(&self) -> bool {
        false
    }
    /// What the last view changed: messages the curator brought back and ones it took out or cut.
    fn activity(&self) -> Option<(usize, usize)> {
        None
    }
    /// Why this policy sends what it sends, when that is not what was asked for
    /// (`auto` or `curator` with settings the curator cannot run, an unknown policy).
    fn detail(&self) -> Option<String> {
        None
    }
    /// What of the settings asked for this policy does not run (it runs with the rest).
    fn caveat(&self) -> Option<String> {
        None
    }
    async fn shutdown(&mut self) {}
}

/// What the interface shows of the context policy: a short label for the title
/// bar (`curator`, `curator idle`, `plain`, and `curator off` when `auto`
/// or `curator` fell back to everything) and the line `/context` and `/status` print.
pub fn describe(policy: &dyn ContextPolicy, history_tokens: u64, view_tokens: u64) -> (String, String) {
    describe_since(policy, history_tokens, view_tokens, None)
}

/// [`describe`], with the last call's figures (`history_tokens`, `view_tokens`): when the curator moved
/// messages, the label then says by how many tokens that changed the window in use (`curator -12k tok`).
pub fn describe_since(
    policy: &dyn ContextPolicy,
    history_tokens: u64,
    view_tokens: u64,
    last: Option<(u64, u64)>,
) -> (String, String) {
    let name = policy.name();
    if let Some(why) = policy.detail() {
        return ("curator off".to_string(), why);
    }
    let (label, detail) = describe_base(policy, name, history_tokens, view_tokens);
    let detail = match policy.caveat() {
        Some(c) => format!("{detail}. {c}"),
        None => detail,
    };
    match policy.activity() {
        Some((0, 0)) | None => (label, detail),
        Some((a, r)) => {
            // the curator's own doing: this view against the last one plus what the conversation appended
            // since. A history that shrank was rewound or cleared: nothing to compare with.
            let moved = last
                .filter(|&(h, v)| v > 0 && history_tokens >= h)
                .map(|(h, v)| view_tokens as i64 - (v + (history_tokens - h)) as i64)
                .filter(|d| d.unsigned_abs() >= MOVED_LEAST);
            // ASCII only: a sign and a word read the same in every terminal
            let tag = match moved {
                Some(d) => format!(" {}{} tok", if d < 0 { '-' } else { '+' }, fmt_k(d.unsigned_abs())),
                None => String::new(),
            };
            let what = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
            let window = match moved {
                Some(d) => format!(
                    ": the window in use is {} tokens {} for it",
                    fmt_k(d.unsigned_abs()),
                    if d < 0 { "smaller" } else { "larger" }
                ),
                None => String::new(),
            };
            let line = format!(
                "{detail}. This call: {} brought back, {} taken out or cut{window}",
                what(a, "message", "messages"),
                what(r, "message", "messages")
            );
            (format!("{label}{tag}"), line)
        }
    }
}

fn describe_base(policy: &dyn ContextPolicy, name: &str, history_tokens: u64, view_tokens: u64) -> (String, String) {
    let k = |t: u64| format!("{} tokens", fmt_k(t));
    match name {
        "curator" => {
            let saved = saving(history_tokens, view_tokens);
            if saved > 0 {
                (
                    // the share saved is in the detail; on the bar it reads as noise once the history is huge
                    "curator".to_string(),
                    format!(
                        "curator: this call sends {} of the {} kept ({saved}% less)",
                        k(view_tokens),
                        k(history_tokens)
                    ),
                )
            } else {
                (
                    "curator idle".to_string(),
                    format!(
                        "curator idle: the whole history ({}) still costs less to send than to rewrite, since a \
                         rewrite pays the provider's cache again; it steps in as the history grows (in recorded \
                         sessions, from 30k to 70k tokens)",
                        k(history_tokens)
                    ),
                )
            }
        }
        "compact" => (
            name.to_string(),
            match policy.compact_at() {
                Some(at) => format!("compact: the whole history, summarised past {}", k(at)),
                None => "compact: the whole history".to_string(),
            },
        ),
        _ => (name.to_string(), format!("{name}: every call sends the whole history")),
    }
}

/// The least change of the window, in tokens, the label names: under it the figure is noise.
const MOVED_LEAST: u64 = 100;

/// 950, 12k, 1.2M.
fn fmt_k(t: u64) -> String {
    match t {
        0..=999 => t.to_string(),
        1_000..=9_999 => format!("{:.1}k", t as f64 / 1e3),
        10_000..=999_999 => format!("{}k", (t as f64 / 1e3).round()),
        _ => format!("{:.1}M", t as f64 / 1e6),
    }
}

/// Percent of the history a view leaves out (0 below 3 %: the curator's
/// squeezing of each message as it arrives is not worth a label).
pub fn saving(history_tokens: u64, view_tokens: u64) -> u64 {
    if history_tokens == 0 || view_tokens >= history_tokens {
        return 0;
    }
    let p = ((history_tokens - view_tokens) as f64 / history_tokens as f64 * 100.0).round() as u64;
    if p < 3 {
        0
    } else {
        p
    }
}

/// The curator's efficiency directives: with the curator's view the first four
/// took 41 % off the time to a solution and 23 % off the calls (the second
/// and fourth are bounded: a range read never stands in for the material a
/// task has to reason about, and brevity holds for changes, not for answers;
/// a partial read of a large file and a bare-array answer each lost an
/// objective in a measured run); the sixth asks
/// for the `Finding:` and `Plan:` notes (the `note` tool) the curator keeps with
/// the first message once their messages leave the view; the seventh for the
/// `Rule:` notes it keeps for the whole session, beside the user's own standing
/// instructions. Without a `note` tool the same directives ask for the lines
/// in the reply, where the curator reads them as well: in a measured run a
/// request for a tool that was not offered went unanswered for the whole task.
macro_rules! directives {
    ($keep:literal, $rule:literal) => {
        concat!("[curator] Working efficiently: (1) when several tool calls do not depend on \
each other (reading or listing files, searching, running independent checks), make them all in the same response; \
(2) do not read again what is already in this conversation; search and line ranges are for reaching the part of a \
large file you need, never for skipping material the task has to reason about: read everything the answer depends \
on before concluding, since a conclusion drawn from a partial read is worth less than the tokens it saved; (3) make \
your edits, then run the checks once, not after every small step; (4) when a change is done, reply in at most three \
short lines: what changed, in which files, and why; do not repeat code or file contents; when the request is a \
question or an analysis, answer it in full, the result stated plainly and in the form asked for (JSON when JSON was \
asked); (5) do not cut the output of a test run or a build with head or tail: a failure hidden by the cut cannot \
be recalled later. (6) When a result settles something (a value, a formula, an address, a format, the cause of an \
error), keep it ", $keep, ": one line beginning with \"Finding:\"; keep your current plan as one \
line beginning with \"Plan:\" and update it when it changes. Older messages leave this context, but these lines \
are kept with the first message, in your words: that block (marked [curator]) is where to look before reading or \
running something again. (7) When the user states a preference, a constraint or a standing instruction, keep it \
", $rule, ": one line beginning with \"Rule:\" so it holds for the rest of the session; follow every rule \
listed in this context (marked [curator]), the later one when two conflict.")
    };
}

/// The directives when the model has a `note` tool.
pub const CURATOR_DIRECTIVES: &str = directives!("with the `note` tool", "with `note`");
/// The directives when it has not: the notes are lines of its reply.
pub const CURATOR_DIRECTIVES_SAID: &str =
    directives!("as the first line of your next reply", "as a line of your reply");

/// Everything. `why`: the reason `auto` came to it (said once, then shown by `/context`).
#[derive(Default)]
pub struct Plain {
    pub why: Option<String>,
    told: bool,
}

impl Plain {
    pub fn because(why: String) -> Plain {
        tracing::warn!("context policy auto: {why}");
        Plain { why: Some(why), told: false }
    }
}

#[async_trait]
impl ContextPolicy for Plain {
    fn name(&self) -> &str {
        "plain"
    }

    async fn view(&mut self, _system_tokens: u64, history: &[Message]) -> anyhow::Result<View> {
        let note = match &self.why {
            Some(w) if !self.told => {
                self.told = true;
                Some(w.clone())
            }
            _ => None,
        };
        Ok(View { messages: history.to_vec(), tokens: estimate_tokens(history), note, memory: None })
    }

    fn reset(&mut self) {}

    fn detail(&self) -> Option<String> {
        self.why.clone()
    }
}

/// Everything, and the loop compacts the history when it passes `compact_at`.
pub struct Compact {
    pub compact_at: u64,
}

#[async_trait]
impl ContextPolicy for Compact {
    fn name(&self) -> &str {
        "compact"
    }

    async fn view(&mut self, _system_tokens: u64, history: &[Message]) -> anyhow::Result<View> {
        Ok(View { messages: history.to_vec(), tokens: estimate_tokens(history), note: None, memory: None })
    }

    fn reset(&mut self) {}

    fn compact_at(&self) -> Option<u64> {
        Some(self.compact_at)
    }
}

/// The most model replies a resumed session takes one by one to rebuild the curator's decisions.
const REPLAY: usize = 2000;

/// The context curator built into nexo, run in process. It keeps the history
/// it was last given, so each call converts only the messages that changed.
pub struct BuiltIn {
    cur: nexo_curator::Curator,
    seen: Vec<Message>,
    values: Vec<serde_json::Value>,
    /// Messages the last view put in and took out.
    moved: (usize, usize),
    /// The version the caller said the next history has, and the one `seen` was taken at.
    hint: Option<u64>,
    seen_version: Option<u64>,
    /// The settings asked for that it runs without, and whether the transcript was told.
    caveat: Option<String>,
    told: bool,
}

impl BuiltIn {
    /// The curator with the settings of `spec` (`key=value;...`), or why they cannot run here.
    pub fn new(spec: &str) -> Result<BuiltIn, String> {
        Ok(BuiltIn {
            cur: nexo_curator::Curator::new(spec.trim())?,
            seen: Vec::new(),
            values: Vec::new(),
            moved: (0, 0),
            hint: None,
            seen_version: None,
            caveat: None,
            told: false,
        })
    }

    /// The curator with what of `spec` it can run: a `key=value` it cannot run alone is left out, the
    /// rest still applies, and the caveat says which and why. Only when the rest together cannot run
    /// either does it run on its defaults (and says so).
    pub fn lenient(spec: &str) -> BuiltIn {
        if let Ok(b) = BuiltIn::new(spec) {
            return b;
        }
        let mut kept = Vec::new();
        let mut dropped = Vec::new();
        for item in spec.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            match nexo_curator::Curator::new(item) {
                Ok(_) => kept.push(item),
                Err(e) => dropped.push(format!("{item} ({e})")),
            }
        }
        let (mut b, dropped) = match BuiltIn::new(&kept.join(";")) {
            Ok(b) if !dropped.is_empty() => (b, dropped.join(", ")),
            // each setting runs alone but not together: the defaults, and the whole spec named
            other => {
                let why = other.err().unwrap_or_else(|| "its settings cannot run together".into());
                (BuiltIn::new("").expect("the defaults run"), format!("{} ({why})", spec.trim()))
            }
        };
        b.caveat = Some(format!("context: the curator runs without [context] curator_spec {dropped}"));
        b
    }
}

#[async_trait]
impl ContextPolicy for BuiltIn {
    fn name(&self) -> &str {
        "curator"
    }

    fn directives(&self, tools: &[&str]) -> Option<&'static str> {
        if !self.cur.directives() {
            return None;
        }
        Some(if tools.contains(&"note") { CURATOR_DIRECTIVES } else { CURATOR_DIRECTIVES_SAID })
    }

    fn history_version(&mut self, version: u64) {
        self.hint = Some(version);
    }

    fn recall(&self) -> bool {
        self.cur.recall()
    }

    fn tools(&mut self, names: &[String]) {
        self.cur.tools(names);
    }

    fn window(&mut self, tokens: u64) {
        self.cur.window(tokens);
    }

    fn prices(&mut self, input: f64, cache_read: f64) {
        self.cur.prices(input, cache_read);
    }

    fn usage(&mut self, read: u64, input: u64) {
        self.cur.cached(read, input);
    }

    fn fits_window(&self) -> bool {
        true
    }

    fn activity(&self) -> Option<(usize, usize)> {
        Some(self.moved)
    }

    fn caveat(&self) -> Option<String> {
        self.caveat.clone()
    }

    async fn view(&mut self, system_tokens: u64, history: &[Message]) -> anyhow::Result<View> {
        let hint = self.hint.take();
        // the time the view is sent, which tells what the provider's cache still keeps
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        // the same version and no fewer messages: only appends happened, what was seen still holds
        let same = match (hint, self.seen_version) {
            (Some(v), Some(s)) if v == s && history.len() >= self.seen.len() => self.seen.len(),
            _ => self.seen.iter().zip(history).take_while(|(a, b)| a == b).count(),
        };
        // a curator new to this history (a resumed session, the policy turned on mid-way) or sent
        // back to an earlier point of it (a rewind)
        let replay = self.seen.is_empty() || (same == history.len() && same < self.seen.len());
        self.seen_version = None;
        self.seen.truncate(same);
        self.values.truncate(same);
        for m in &history[same..] {
            self.values.push(serde_json::to_value(m)?);
            self.seen.push(m.clone());
        }
        self.seen_version = hint;
        if replay {
            // takes the decisions it would have taken while the session ran: one view per model
            // reply, at the history that reply answered. The view it sends next is then the one the
            // open session sent, cuts and marks alike, and the provider's cache still holds. Past
            // REPLAY replies, the older ones are taken in one step.
            let points: Vec<usize> = (1..history.len()).filter(|&p| history[p].role == Role::Assistant).collect();
            for &p in &points[points.len().saturating_sub(REPLAY)..] {
                if let Err(e) = self.cur.view(system_tokens, &self.values[..p], now) {
                    anyhow::bail!("the built-in curator failed: {e}");
                }
            }
        }
        let r = match self.cur.view(system_tokens, &self.values, now) {
            Ok(r) => r,
            Err(e) => anyhow::bail!("the built-in curator failed: {e}"),
        };
        let messages = r.view.into_iter().map(serde_json::from_value).collect::<Result<Vec<Message>, _>>()?;
        // what the curator moved: the messages the conversation appended are always "in", every call,
        // and would hide the rest
        self.moved = (r.back, r.removed);
        let zone = |z: &nexo_curator::Zone| nexo_core::ContextZone {
            label: z.label.to_string(),
            active: z.active,
            card: z.card,
        };
        let cell = |c: &nexo_curator::Cell| MemoryCell {
            zone: u32::try_from(c.zone).unwrap_or(u32::MAX),
            tokens: u64::try_from(c.tokens).unwrap_or(0),
            state: c.state.as_str().to_string(),
        };
        let memory = Some(Memory {
            zones: r.memory.zones.iter().map(zone).collect(),
            cells: r.memory.cells.iter().map(cell).collect(),
            curated: curated_items(&r.curated),
        });
        let note = match &self.caveat {
            Some(c) if !self.told => {
                self.told = true;
                Some(c.clone())
            }
            _ => None,
        };
        Ok(View { messages, tokens: r.tokens.max(0) as u64, note, memory })
    }

    fn reset(&mut self) {
        self.cur.reset();
        self.seen.clear();
        self.seen_version = None;
        self.hint = None;
        self.moved = (0, 0);
        self.values.clear();
    }

    /// The engine notices the rewritten history by itself and starts over on it,
    /// keeping the user's standing instructions; a fresh engine would lose them.
    fn compacted(&mut self) {}
}

/// The curator's items for the interface.
fn curated_items(items: &[nexo_curator::Added]) -> Vec<nexo_core::CuratedItem> {
    use nexo_core::{CuratedKind, CuratedState};
    items
        .iter()
        .map(|a| nexo_core::CuratedItem {
            kind: match a.kind {
                nexo_curator::AddedKind::Recalled => CuratedKind::Recalled,
                nexo_curator::AddedKind::Start => CuratedKind::Start,
                nexo_curator::AddedKind::Rule => CuratedKind::Rule,
                nexo_curator::AddedKind::Thread => CuratedKind::Thread,
                nexo_curator::AddedKind::Goal => CuratedKind::Goal,
                nexo_curator::AddedKind::Memory => CuratedKind::Memory,
            },
            label: a.label.clone(),
            tokens: a.tokens.max(0) as u64,
            state: match a.state {
                nexo_curator::AddedState::In => CuratedState::In,
                nexo_curator::AddedState::Trimmed => CuratedState::Trimmed,
                nexo_curator::AddedState::Out => CuratedState::Out,
            },
        })
        .collect()
}

/// `spec` with the working window of `[context] working_set` (`target/cap/hard`, in tokens) over its
/// `target`, `cap` and `hard`; `spec` as it is when `working_set` is empty.
pub fn working_set_spec(spec: &str, working_set: &str) -> Result<String, String> {
    let ws = working_set.trim();
    if ws.is_empty() {
        return Ok(spec.to_string());
    }
    let nums: Vec<u64> = ws.split('/').filter_map(|n| n.trim().replace('_', "").parse().ok()).collect();
    if nums.len() != 3 || ws.split('/').count() != 3 {
        return Err(format!(
            "context: [context] working_set {ws:?} is not target/cap/hard in tokens, e.g. 20000/40000/80000"
        ));
    }
    let given = format!("target={};cap={};hard={}", nums[0], nums[1], nums[2]);
    Ok(if spec.trim().is_empty() { given } else { format!("{};{given}", spec.trim()) })
}

/// The policy `cfg.policy` names (`auto`, `plain`, `compact`, `curator`;
/// anything else: plain). `auto` and `curator` are the curator.
pub fn from_config(cfg: &ContextConfig) -> Box<dyn ContextPolicy> {
    match cfg.policy.trim() {
        "compact" => Box::new(Compact { compact_at: cfg.compact_at }),
        "plain" => Box::new(Plain::default()),
        // a setting the curator cannot run leaves out that setting, never the curator
        "curator" | "auto" | "" => match working_set_spec(&cfg.curator_spec, &cfg.working_set) {
            Ok(spec) => Box::new(BuiltIn::lenient(&spec)),
            Err(why) => {
                let mut b = BuiltIn::lenient(&cfg.curator_spec);
                b.caveat = Some(match b.caveat.take() {
                    Some(c) => format!("{c}; {why}"),
                    None => why,
                });
                Box::new(b)
            }
        },
        other => Box::new(Plain::because(format!(
            "context: plain, since [context] policy = {other:?} is not one of auto, plain, curator, compact"
        ))),
    }
}
