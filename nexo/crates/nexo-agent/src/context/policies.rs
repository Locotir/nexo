//! The context policies: everything, compaction, and the curator.

use super::*;
use async_trait::async_trait;
use nexo_core::config::ContextConfig;
use nexo_core::message::{estimate_tokens, Message, Role};

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
pub(super) const REPLAY: usize = 2000;

/// The context curator built into nexo, run in process. It keeps the history
/// it was last given, so each call converts only the messages that changed.
pub struct BuiltIn {
    cur: nexo_curator::Curator,
    seen: Vec<Message>,
    values: Vec<serde_json::Value>,
    /// Messages the last view put in and took out.
    pub(super) moved: (usize, usize),
    /// The version the caller said the next history has, and the one `seen` was taken at.
    hint: Option<u64>,
    seen_version: Option<u64>,
    /// The settings asked for that it runs without, and whether the transcript was told.
    caveat: Option<String>,
    told: bool,
    /// A hash of each message of `values` (`nexo_curator::snapshot::message_hash`), so that saving the state
    /// never goes over the history again.
    hashes: Vec<u64>,
    /// The views asked of the curator, and the messages its state covered when it was last saved.
    views: u64,
    saved_at: usize,
    /// A state saved with the session, for the first view of a resumed one (`ContextPolicy::restore`).
    pending: Option<Vec<u8>>,
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
            hashes: Vec::new(),
            views: 0,
            saved_at: 0,
            pending: None,
        })
    }

    /// The curator saved with the session, in place of the fresh one, when it still fits `self.values`; the
    /// messages it covers (0 when there is none or it does not fit, and the whole history is replayed).
    fn take_saved(&mut self) -> usize {
        let Some(bytes) = self.pending.take() else { return 0 };
        let t = std::time::Instant::now();
        match nexo_curator::Curator::restore(self.cur.spec(), &bytes, &self.values, &self.hashes) {
            Ok((cur, head)) => {
                // what the host said of this run went to the new curator before this view (tools, window, prices,
                // calls left, or nothing of them); the saved one has the last run's, so it takes this one's
                let fresh = std::mem::replace(&mut self.cur, cur);
                self.cur.take_host_settings(&fresh);
                self.views = head.views;
                self.saved_at = self.cur.covered();
                tracing::debug!(
                    "context: the saved curator covers {} of {} messages, read in {} ms",
                    head.covered,
                    self.values.len(),
                    t.elapsed().as_millis()
                );
                self.cur.covered()
            }
            Err(e) => {
                tracing::info!("context: the saved curator is not used ({e}); the history is replayed");
                0
            }
        }
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

    fn calls_left(&mut self, calls: Option<u64>) {
        self.cur.calls_left(calls);
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
        let fresh = self.seen.is_empty();
        self.seen_version = None;
        self.seen.truncate(same);
        self.values.truncate(same);
        self.hashes.truncate(same);
        for m in &history[same..] {
            let v = serde_json::to_value(m)?;
            self.hashes.push(nexo_curator::snapshot::message_hash(&v));
            self.values.push(v);
            self.seen.push(m.clone());
        }
        self.seen_version = hint;
        // the replies taken again before this view (`ContextPolicy::save` writes the state only after some)
        let mut replayed = 0;
        if replay {
            // a resumed session starts from the curator saved with it, when it still fits the history, and
            // takes only the replies after the messages it covers
            let from = if fresh { self.take_saved() } else { 0 };
            self.pending = None;
            // takes the decisions it would have taken while the session ran: one view per model
            // reply, at the history that reply answered. The view it sends next is then the one the
            // open session sent, cuts and marks alike, and the provider's cache still holds. Past
            // REPLAY replies, the older ones are taken in one step.
            let points: Vec<usize> =
                (from.max(1)..history.len()).filter(|&p| p > from && history[p].role == Role::Assistant).collect();
            replayed = points.len().min(REPLAY);
            for &p in &points[points.len().saturating_sub(REPLAY)..] {
                self.views += 1;
                if let Err(e) = self.cur.view(system_tokens, &self.values[..p], now) {
                    anyhow::bail!("the built-in curator failed: {e}");
                }
            }
        }
        self.views += 1;
        let r = match self.cur.view(system_tokens, &self.values, now) {
            Ok(r) => r,
            Err(e) => anyhow::bail!("the built-in curator failed: {e}"),
        };
        if fresh && self.saved_at > 0 && replayed == 0 {
            // the saved state took no reply again: the one saved is as good to start from, so it is not written over
            self.saved_at = self.cur.covered();
        }
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
        self.hashes.clear();
        self.saved_at = 0;
        self.pending = None;
    }

    fn save(&mut self) -> Option<super::SaveJob> {
        let covered = self.cur.covered();
        if covered == 0 || covered == self.saved_at || covered > self.hashes.len() {
            return None;
        }
        self.saved_at = covered;
        let saved = self.cur.save(self.views, nexo_curator::snapshot::history_hash(&self.hashes[..covered]));
        Some(Box::new(move || saved.encode()))
    }

    fn restore(&mut self, saved: Vec<u8>) {
        self.pending = Some(saved);
    }

    /// The engine notices the rewritten history by itself and starts over on it,
    /// keeping the user's standing instructions; a fresh engine would lose them.
    fn compacted(&mut self) {}
}

/// The curator's items for the interface.
pub(super) fn curated_items(items: &[nexo_curator::Added]) -> Vec<nexo_core::CuratedItem> {
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

/// The names a context policy is asked for by, in the order they are listed to the person.
pub const POLICY_NAMES: [&str; 4] = ["auto", "plain", "curator", "compact"];

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
            "context: plain, since [context] policy = {other:?} is not one of {}",
            POLICY_NAMES.join(", ")
        ))),
    }
}
