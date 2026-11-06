//! What the next model call sees: everything (`Plain`), everything with a
//! summary when it grows past a size (`Compact`), or what the context curator
//! built into nexo keeps (`BuiltIn`).

use async_trait::async_trait;
use nexo_core::config::ContextConfig;
use nexo_core::message::{estimate_tokens, Message, Role};

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
    /// Its description is an instruction the model gets only with this policy, so it goes with the
    /// directives: off by default, so that the view alone is what the policy changes.
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
    /// What the last view changed: messages put in (new or brought back) and taken out or cut.
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
/// bar (`curator −38%`, `curator idle`, `plain`, and `curator off` when `auto`
/// or `curator` fell back to everything) and the line `/context` and `/status` print.
pub fn describe(policy: &dyn ContextPolicy, history_tokens: u64, view_tokens: u64) -> (String, String) {
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
            // ASCII only: a sign and a word read the same in every terminal
            let mut tag = String::new();
            if a > 0 {
                tag.push_str(&format!("  +{a} in"));
            }
            if r > 0 {
                tag.push_str(&format!("  -{r} out"));
            }
            let what = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
            let line = format!(
                "{detail}. This call: {} put in (new or brought back), {} taken out or cut",
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
                    format!("curator −{saved}%"),
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
        self.cur.directives()
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
        self.moved = (r.added, r.removed);
        let memory = serde_json::from_value(r.memory).ok();
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

/// The policy `cfg.policy` names (`auto`, `plain`, `compact`, `curator`;
/// anything else: plain). `auto` and `curator` are the curator.
pub fn from_config(cfg: &ContextConfig) -> Box<dyn ContextPolicy> {
    match cfg.policy.trim() {
        "compact" => Box::new(Compact { compact_at: cfg.compact_at }),
        "plain" => Box::new(Plain::default()),
        // a setting the curator cannot run leaves out that setting, never the curator
        "curator" | "auto" | "" => Box::new(BuiltIn::lenient(&cfg.curator_spec)),
        other => Box::new(Plain::because(format!(
            "context: plain, since [context] policy = {other:?} is not one of auto, plain, curator, compact"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history() -> Vec<Message> {
        vec![
            Message::user("a"),
            Message::assistant("b"),
            Message::user("c"),
            Message::assistant("d"),
            Message::user("e"),
        ]
    }

    #[test]
    fn only_the_curator_adds_its_directives() {
        assert_eq!(Plain::default().directives(&["note"]), None);
        // off by default (a pure view); `directives=1` turns them on
        assert_eq!(BuiltIn::new("").unwrap().directives(&["note"]), None);
        let c = BuiltIn::new("directives=1").unwrap();
        let d = c.directives(&["read", "note"]).unwrap();
        assert!(d.starts_with("[curator] Working efficiently") && d.contains("at most three short lines"));
        assert!(d.contains("beginning with \"Finding:\"") && d.contains("beginning with \"Plan:\""));
        assert!(d.contains("(7)") && d.contains("beginning with \"Rule:\""), "{d}");
        assert!(!d.contains("  "), "the line breaks of the source leave no double spaces");
        // without a `note` tool, the same directives ask for the lines in the reply
        let s = c.directives(&["read", "bash"]).unwrap();
        assert!(s.contains("as the first line of your next reply: one line beginning with \"Finding:\""), "{s}");
        assert!(s.contains("as a line of your reply: one line beginning with \"Rule:\""), "{s}");
        assert!(!s.contains("`note`") && d.contains("`note`"));
        assert!(!s.contains("  "));
        // `directives=0`: the system prompt stays as the other policies send it
        assert_eq!(BuiltIn::new("directives=0").unwrap().directives(&["note"]), None);
    }

    #[test]
    fn recall_goes_with_the_directives() {
        // the tool list is part of what the model gets: with the directives off (the default) the
        // curator offers the same tools as the other policies, so the view is all that differs
        assert!(!Plain::default().recall());
        assert!(!BuiltIn::new("").unwrap().recall());
        assert!(!BuiltIn::new("directives=0").unwrap().recall());
        assert!(BuiltIn::new("directives=1").unwrap().recall());
    }

    #[test]
    fn config_picks_the_policy() {
        let mut cfg = ContextConfig::default();
        assert_eq!(cfg.policy, "auto");
        cfg.policy = "plain".into();
        let p = from_config(&cfg);
        assert_eq!(p.name(), "plain");
        assert!(p.detail().is_none(), "asked for: nothing to explain");
        // auto and curator: the built-in curator
        for name in ["auto", "curator", ""] {
            cfg.policy = name.into();
            let p = from_config(&cfg);
            assert_eq!((p.name(), p.directives(&["note"])), ("curator", None));
            assert!(p.detail().is_none());
        }
        cfg.curator_spec = "directives=1".into();
        let p = from_config(&cfg);
        assert_eq!(p.directives(&["note"]), Some(CURATOR_DIRECTIVES));
        assert_eq!(p.directives(&[]), Some(CURATOR_DIRECTIVES_SAID));
        // a setting the curator cannot run: the curator runs without it, and says which
        cfg.curator_spec = "rule=lru".into();
        let p = from_config(&cfg);
        assert_eq!(p.name(), "curator");
        assert!(p.detail().is_none() && p.caveat().unwrap().contains("rule=lru"));
        cfg.curator_spec.clear();
        cfg.policy = "curatr".into();
        assert!(from_config(&cfg).detail().unwrap().contains("\"curatr\""));
        cfg.policy = "compact".into();
        assert_eq!(from_config(&cfg).compact_at(), Some(100_000));
    }

    #[tokio::test]
    async fn auto_tells_once_why_it_is_plain() {
        let mut p = Plain::because("context: plain, since the curator cannot run this spec".into());
        let v = p.view(0, &history()).await.unwrap();
        assert!(v.note.unwrap().contains("cannot run this spec"));
        assert!(p.view(0, &history()).await.unwrap().note.is_none(), "said once");
        assert!(p.detail().is_some(), "and /context keeps saying it");
    }

    #[tokio::test]
    async fn the_label_says_what_came_in_and_went_out() {
        let mut c = BuiltIn::new("").unwrap();
        c.view(0, &history()).await.unwrap();
        let (label, detail) = describe(&c, 100_000, 62_000);
        assert!(label.starts_with("curator −38%  +") && label.ends_with(" in"), "{label}");
        assert!(label[label.find('+').unwrap()..].is_ascii(), "{label}");
        assert!(detail.contains("This call:") && detail.contains("put in"), "{detail}");
        c.moved = (2, 5);
        assert_eq!(describe(&c, 100_000, 62_000).0, "curator −38%  +2 in  -5 out");
        c.moved = (0, 0);
        assert_eq!(describe(&c, 100_000, 62_000).0, "curator −38%");
    }

    #[test]
    fn the_label_says_what_the_policy_does() {
        let c = BuiltIn::new("").unwrap();
        let (label, detail) = describe(&c, 100_000, 62_000);
        assert_eq!(label, "curator −38%");
        assert!(detail.contains("62k tokens of the 100k tokens"), "{detail}");
        let (label, detail) = describe(&c, 20_000, 19_900);
        assert_eq!(label, "curator idle");
        assert!(detail.contains("20k tokens"), "{detail}");
        let (label, _) = describe(&Plain::default(), 20_000, 20_000);
        assert_eq!(label, "plain");
        let (label, detail) = describe(&Compact { compact_at: 120_000 }, 1, 1);
        assert_eq!(label, "compact");
        assert!(detail.contains("120k"));
        assert_eq!(saving(0, 0), 0);
        assert_eq!(saving(1000, 980), 0, "squeezing alone is not worth a label");
        assert_eq!(saving(1000, 500), 50);
        assert_eq!(fmt_k(950), "950");
        assert_eq!(fmt_k(1_234), "1.2k");
        assert_eq!(fmt_k(1_500_000), "1.5M");
    }

    #[test]
    fn a_setting_the_curator_cannot_run_leaves_only_that_setting_out() {
        // a typo in one setting keeps the curator and the rest of the spec, and is said once
        let cfg = ContextConfig { curator_spec: "dedup=0;rule=lru;apart=5000".into(), ..Default::default() };
        let p = from_config(&cfg);
        let (label, detail) = describe(p.as_ref(), 1000, 1000);
        assert!(label.starts_with("curator") && label != "curator off", "{label}");
        assert!(detail.contains("without [context] curator_spec rule=lru ("), "{detail}");
        assert!(!detail.contains("dedup=0") && !detail.contains("apart"), "{detail}");
    }

    #[tokio::test]
    async fn the_setting_left_out_is_said_once_in_the_transcript() {
        let mut p = BuiltIn::lenient("rule=lru");
        assert!(p.view(0, &history()).await.unwrap().note.unwrap().contains("rule=lru"));
        assert!(p.view(0, &history()).await.unwrap().note.is_none(), "said once");
        // a spec that runs has nothing to say
        let mut ok = BuiltIn::lenient("dedup=3");
        assert!(ok.caveat().is_none() && ok.view(0, &history()).await.unwrap().note.is_none());
    }

    /// A user's standing instruction outlives a compaction of the history.
    #[tokio::test]
    async fn a_compacted_history_keeps_the_rules() {
        let mut c = BuiltIn::new("").unwrap();
        let mut h = vec![Message::user("Never use unwrap in library code."), Message::assistant("Understood.")];
        c.view(1000, &h).await.unwrap();
        h = vec![Message::user("Summary of the work so far: the parser is done."), Message::assistant("Ok.")];
        c.compacted();
        let v = c.view(1000, &h).await.unwrap();
        let text: String = v.messages.iter().map(|m| m.text()).collect();
        assert!(text.contains("Never use unwrap in library code."), "{text}");
    }

    /// A session closed and resumed: the notes come back from the saved history, nothing else holds them.
    #[tokio::test]
    async fn notes_come_back_when_a_session_is_resumed() {
        use nexo_core::message::{Part, Role};
        let mut h = vec![
            Message::user("Find where the key is kept."),
            Message::new(
                Role::Assistant,
                vec![Part::tool_call("n0", "note", serde_json::json!({"text": "Finding: the key lives at 0x4f2a"}))],
            ),
            Message::new(Role::User, vec![Part::tool_result("n0", "noted 1 line", false)]),
        ];
        for i in 0..40 {
            h.push(Message::new(
                Role::Assistant,
                vec![Part::tool_call(format!("c{i}"), "read", serde_json::json!({"path": format!("m{i}.rs")}))],
            ));
            let body: String = (0..300).map(|l| format!("{l:>4}  fn f_{i}_{l}() {{}} // module {i}\n")).collect();
            h.push(Message::new(Role::User, vec![Part::tool_result(format!("c{i}"), body, false)]));
        }
        // the open session saw the history grow; the resumed one gets it whole, with a fresh curator
        let mut open = BuiltIn::new("").unwrap();
        for n in (4..=h.len()).step_by(2) {
            open.view(3000, &h[..n]).await.unwrap();
        }
        let open = open.view(3000, &h).await.unwrap();
        let resumed = BuiltIn::new("").unwrap().view(3000, &h).await.unwrap();
        for v in [&open, &resumed] {
            let sent = serde_json::to_string(&v.messages).unwrap();
            assert!(sent.contains("0x4f2a"), "the note is in the view");
            assert!(saving(estimate_tokens(&h), estimate_tokens(&v.messages)) > 0, "and the view was cut");
        }
    }

    /// A long session, as it was sent while it ran (one view per model reply) and as a resumed
    /// session sees it (the whole history at once): the view is the same, cuts and marks alike, so
    /// a restart neither changes what the model sees nor misses the provider's cache.
    #[tokio::test]
    async fn a_resumed_session_sends_the_view_the_open_one_would() {
        use nexo_core::message::{Part, Role};
        let mut h = vec![Message::user("Go through every module, then fix the one that panics.")];
        for i in 0..60 {
            let path = format!("m{}.rs", i % 23);
            h.push(Message::new(
                Role::Assistant,
                vec![
                    Part::text(format!("Step {i}.")),
                    Part::tool_call(format!("c{i}"), "read", serde_json::json!({"path": path})),
                ],
            ));
            let body: String = (0..120 + 7 * (i % 5))
                .map(|l| format!("{l:>4}  fn f_{i}_{l}(x: u64) -> u64 {{ x + {l} }}\n"))
                .collect();
            h.push(Message::new(Role::User, vec![Part::tool_result(format!("c{i}"), body, i % 11 == 7)]));
            if i % 17 == 16 {
                h.push(Message::assistant(format!("Module batch {i} done.")));
                h.push(Message::user(format!("Now batch {}.", i + 1)));
            }
        }
        let mut open = BuiltIn::new("").unwrap();
        for p in 0..h.len() {
            if h[p].role == Role::Assistant {
                open.view(3000, &h[..p]).await.unwrap();
            }
        }
        let open = open.view(3000, &h).await.unwrap();
        let resumed = BuiltIn::new("").unwrap().view(3000, &h).await.unwrap();
        assert!(saving(estimate_tokens(&h), estimate_tokens(&open.messages)) > 0, "the open session was cut");
        assert_eq!(serde_json::to_string(&resumed.messages).unwrap(), serde_json::to_string(&open.messages).unwrap());
        assert_eq!(resumed.tokens, open.tokens);
    }

    /// Many generated sessions (reads, rereads, edits, commands, notes, failures, new requests):
    /// the view a resumed session sends is the one the open session would have sent.
    #[tokio::test]
    async fn resumed_sessions_of_every_shape_send_the_open_view() {
        use nexo_core::message::{Part, Role};
        let mut differ = Vec::new();
        for seed in 0..40u64 {
            let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let mut next = |m: u64| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (x >> 33) % m
            };
            let mut h = vec![Message::user(format!("Task {seed}: look through the project and fix what is broken."))];
            let steps = 30 + next(120);
            for i in 0..steps {
                let f = format!("src/m{}.rs", next(15));
                let (name, args, body): (&str, serde_json::Value, String) = match next(6) {
                    0 | 1 => (
                        "read",
                        serde_json::json!({"path": f}),
                        (0..40 + next(300)).map(|l| format!("{l:>4}  let v{l} = f{i}({l});\n")).collect(),
                    ),
                    2 => (
                        "edit",
                        serde_json::json!({"path": f, "old_string": "a", "new_string": "b"}),
                        format!("edited {f}: +1 -1 lines"),
                    ),
                    3 => (
                        "bash",
                        serde_json::json!({"command": format!("cargo test m{i}")}),
                        (0..20 + next(200))
                            .map(|l| format!("test t{l} ... {}\n", if l % 9 == 0 { "FAILED" } else { "ok" }))
                            .collect(),
                    ),
                    4 => (
                        "note",
                        serde_json::json!({"text": format!("Finding {i}: value {} matters", next(9999))}),
                        "noted 1 line".into(),
                    ),
                    _ => (
                        "grep",
                        serde_json::json!({"pattern": format!("f{}", next(50))}),
                        (0..next(60)).map(|l| format!("{f}:{l}: f{l}()\n")).collect(),
                    ),
                };
                h.push(Message::new(
                    Role::Assistant,
                    vec![Part::text(format!("Step {i}.")), Part::tool_call(format!("c{i}"), name, args)],
                ));
                h.push(Message::new(Role::User, vec![Part::tool_result(format!("c{i}"), body, next(13) == 0)]));
                if next(25) == 0 {
                    h.push(Message::assistant(format!("Done with part {i}.")));
                    h.push(Message::user(format!("Next: part {}.", i + 1)));
                }
            }
            let mut open = BuiltIn::new("").unwrap();
            for p in 0..h.len() {
                if h[p].role == Role::Assistant {
                    open.view(3000, &h[..p]).await.unwrap();
                }
            }
            let open = open.view(3000, &h).await.unwrap();
            let resumed = BuiltIn::new("").unwrap().view(3000, &h).await.unwrap();
            let (a, b) =
                (serde_json::to_string(&open.messages).unwrap(), serde_json::to_string(&resumed.messages).unwrap());
            if a != b {
                differ.push((seed, h.len(), open.tokens, resumed.tokens));
            }
        }
        assert!(differ.is_empty(), "seed, messages, open tokens, resumed tokens: {differ:?}");
    }
    /// The curator on a long session of large tool results.
    #[tokio::test]
    async fn the_curator_cuts_a_long_session() {
        let mut c = BuiltIn::new("").unwrap();
        let mut h = vec![Message::user("Go through every module and summarise what each does.")];
        let mut first_cut = None;
        let mut last = (0, 0);
        for i in 0..40 {
            h.push(Message::new(
                nexo_core::message::Role::Assistant,
                vec![
                    nexo_core::message::Part::text(format!("Step {i}: reading module {i}.")),
                    nexo_core::message::Part::tool_call(
                        format!("c{i}"),
                        "read",
                        serde_json::json!({"path": format!("m{i}.rs")}),
                    ),
                ],
            ));
            let body: String = (0..300)
                .map(|l| format!("{l:>4}  fn f_{i}_{l}(x: u64) -> u64 {{ x * {l} + {i} }} // module {i}\n"))
                .collect();
            h.push(Message::new(
                nexo_core::message::Role::User,
                vec![nexo_core::message::Part::tool_result(format!("c{i}"), body, false)],
            ));
            let v = c.view(3000, &h).await.unwrap();
            assert!(v.memory.is_some());
            let (full, sent) = (estimate_tokens(&h), estimate_tokens(&v.messages));
            if first_cut.is_none() && saving(full, sent) > 0 {
                first_cut = Some(i + 1);
            }
            last = (full, sent);
        }
        assert!(first_cut.is_some(), "the curator never cut anything in 40 calls");
        assert!(saving(last.0, last.1) >= 30, "{last:?}");
        // a rewritten history is taken as it is
        h.truncate(3);
        let v = c.view(3000, &h).await.unwrap();
        assert_eq!(v.messages.len(), 3);
    }
}
