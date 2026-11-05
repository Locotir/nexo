//! The supervisor's read-only loop: poll the sources, fold what they saw into one track per session, classify
//! each track and keep the rows the panel draws. In this stage it only marks (the state of a session); it
//! sends nothing to any session.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::audit::Audit;
use super::decide::Ledger;
use super::events::Ev;
use super::policy::Policy;
use super::source::{Source, SourceKind};
use super::source_jsonl::JsonlSource;
use super::source_list::ListSource;
use super::source_nexo::NexoSource;
use super::{classify, input_hash, AgentTrack, ObsKind, Secs, State};
use nexo_core::mail::Stage;

/// What one source returned in a round: its kind, its program name and the observations.
pub(super) type Polled = (SourceKind, String, Vec<super::Observation>);

/// What the panel shows of one session.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRow {
    pub key: String,
    pub name: String,
    pub source: SourceKind,
    /// The program the session belongs to (`nexo`, or the name of the other tool), the panel's section.
    pub program: String,
    pub state: State,
    /// Since when the session has been in this state.
    pub since: Secs,
    pub last_step: Option<String>,
    /// Seconds without activity.
    pub idle_secs: Option<u64>,
    pub cost_usd: Option<f64>,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub permission_mode: Option<String>,
    /// The key of the session this one hangs from (a sub-agent).
    pub parent: Option<String>,
    /// The supervisor's last action on it and when.
    pub last_action: Option<(String, Secs)>,
    /// The supervisor leaves it alone (paused for it, for its kind, or for all).
    pub paused: bool,
    /// The latest events, newest last (at most 12).
    pub ribbon: Vec<Ev>,
    /// Tokens per minute over the last 24 minutes, the newest last (events per minute when no usage is known).
    pub spark: Vec<u32>,
    /// Todo items done and in all.
    pub todo: Option<(u16, u16)>,
    /// The last test run: passed and failed.
    pub tests: Option<(u32, u32)>,
    /// Sub-agents: key and state.
    pub subs: Vec<(String, State)>,
    /// Mails that wait for this session's next idle moment.
    pub unread: u16,
    /// Mails that wait for the person's approval.
    pub held: u16,
    /// Seconds until the provider is tried again.
    pub retry_secs: Option<u32>,
}

/// One mail between two sessions, one row per mail id.
#[derive(Clone, Debug, PartialEq)]
pub struct MailRow {
    pub id: String,
    /// The sender: a session key when known, else its name.
    pub from: String,
    /// The receiver: a session key when known, else its name.
    pub to: String,
    pub summary: String,
    pub stage: Stage,
    pub at: Secs,
    pub bytes: usize,
    /// Times this row changed (a view that animates new rows looks at it).
    pub n: u16,
}

/// Mails kept for the feed.
const MAIL_KEEP: usize = 200;

pub(super) struct Entry {
    pub(super) track: AgentTrack,
    pub(super) source: SourceKind,
    pub(super) program: String,
    pub(super) state: State,
    pub(super) since: Secs,
    pub(super) action: Option<(String, Secs)>,
}

pub struct Supervisor {
    pub policy: Policy,
    pub(super) sources: Vec<Box<dyn Source>>,
    pub(super) entries: BTreeMap<String, Entry>,
    pub(super) ledgers: BTreeMap<String, Ledger>,
    pub(super) audit: Option<Audit>,
    pub(super) paused_all: bool,
    /// Decisions waiting to see what they led to: when they were taken.
    pub(super) open: BTreeMap<String, Secs>,
    pub(super) last_act: Option<Secs>,
    /// The cheap model, when the policy names one and it could be reached.
    pub(super) advisor: Option<super::model::Advisor>,
    /// The live session of this program, to hand its nudges to.
    pub(super) nexo: Option<NexoSource>,
    /// Every mail seen, by id.
    pub(super) mails: BTreeMap<String, MailRow>,
    /// Why the model is not in use, or other news about the policy, for the policy tab.
    pub notes: Vec<String>,
    /// Counts every change of the rows, for a view that repaints on change.
    pub version: u64,
}

impl Supervisor {
    pub fn new(policy: Policy) -> Supervisor {
        Supervisor {
            policy,
            sources: Vec::new(),
            entries: BTreeMap::new(),
            ledgers: BTreeMap::new(),
            audit: None,
            paused_all: false,
            open: BTreeMap::new(),
            last_act: None,
            advisor: None,
            nexo: None,
            mails: BTreeMap::new(),
            notes: Vec::new(),
            version: 0,
        }
    }

    pub fn add(&mut self, source: Box<dyn Source>) {
        self.sources.push(source);
    }

    /// Give the supervisor a model to ask. Nothing happens unless the policy has a `[model]` section.
    pub fn set_judge(&mut self, judge: std::sync::Arc<dyn super::model::Judge>) {
        if let Some(m) = self.policy.model.clone() {
            self.advisor = Some(super::model::Advisor::new(judge, m));
        }
    }

    /// The model spend of the last hour in dollars and calls, with the caps; none without a model.
    pub fn model_usage(&mut self, now: Secs) -> Option<(f64, usize, f64, u32)> {
        let a = self.advisor.as_mut()?;
        let (usd, calls) = (a.spent(now), a.calls(now));
        Some((usd, calls, a.policy().max_usd_per_hour, a.policy().max_calls_per_hour))
    }

    /// One round: poll, fold, classify. True when anything a row shows changed.
    pub fn tick(&mut self, now: Secs) -> bool {
        let polled = self.sources.iter_mut().map(|s| (s.kind(), s.program(), s.poll(now))).collect();
        self.fold(polled, now)
    }

    /// Fold what the sources returned in and classify every session.
    pub(super) fn fold(&mut self, polled: Vec<Polled>, now: Secs) -> bool {
        let mut changed = false;
        for (kind, program, mut obs) in polled {
            obs.sort_by_key(|o| o.at);
            for o in obs {
                if o.agent_key.is_empty() {
                    continue;
                }
                if matches!(o.kind, ObsKind::MsgOut { .. } | ObsKind::MsgIn { .. } | ObsKind::MsgStage { .. }) {
                    // a mail between sessions is a row of the feed; it gives a card to no one
                    changed |= fold_mail(&mut self.mails, &o);
                    if !self.entries.contains_key(&o.agent_key) {
                        continue;
                    }
                }
                let e = self.entries.entry(o.agent_key.clone()).or_insert_with(|| Entry {
                    track: AgentTrack::new(o.agent_key.clone()),
                    source: kind,
                    program: program.clone(),
                    state: State::Working,
                    since: now,
                    action: None,
                });
                if e.program == super::source::EXTERNAL && program != e.program {
                    e.program = program.clone();
                    changed = true;
                }
                let before = fingerprint(&e.track);
                e.track.apply(&o);
                changed |= before != fingerprint(&e.track);
            }
        }
        for e in self.entries.values_mut() {
            let state = classify(&e.track, now, &self.policy);
            if std::mem::discriminant(&state) != std::mem::discriminant(&e.state) {
                e.since = now;
                e.action = Some((format!("marked {}", state.label()), now));
                changed = true;
            }
            // a quiet session counts its minutes
            if let (State::Idle { min: a }, State::Idle { min: b }) = (state, e.state) {
                changed |= a != b;
            }
            e.state = state;
        }
        changed |= self.act(now);
        if changed {
            self.version += 1;
        }
        changed
    }

    /// The mails between sessions, newest last, from `since` on (at most the newest 40).
    pub fn mail_feed(&self, since: Secs) -> Vec<MailRow> {
        let mut v: Vec<MailRow> = self.mails.values().filter(|m| m.at >= since).cloned().collect();
        v.sort_by(|a, b| (a.at, &a.id).cmp(&(b.at, &b.id)));
        let cut = v.len().saturating_sub(40);
        v.drain(..cut);
        v
    }

    /// The rows, nexo sessions first, each session's sub-agents under it.
    pub fn rows(&self, now: Secs) -> Vec<AgentRow> {
        let mut rows: Vec<AgentRow> = self
            .entries
            .values()
            .map(|e| {
                let t = &e.track;
                let parent =
                    t.key.rsplit_once('.').map(|(p, _)| p.to_string()).filter(|p| self.entries.contains_key(p));
                AgentRow {
                    key: t.key.clone(),
                    name: display_name(t),
                    source: e.source,
                    program: e.program.clone(),
                    state: e.state,
                    since: e.since,
                    last_step: t.last_step.clone(),
                    idle_secs: t.idle_secs(now),
                    cost_usd: t.cost_usd,
                    model: t.model.clone(),
                    cwd: t.cwd.clone(),
                    permission_mode: t.permission_mode.clone(),
                    parent,
                    last_action: e.action.clone(),
                    paused: self.paused(t),
                    ribbon: t.ribbon.iter().rev().take(12).rev().cloned().collect(),
                    spark: spark(t, now),
                    todo: t.todo,
                    tests: t.tests.map(|(p, f, _)| (p, f)),
                    subs: self
                        .entries
                        .values()
                        .filter(|k| k.track.key.rsplit_once('.').is_some_and(|(p, _)| p == t.key))
                        .map(|k| (k.track.key.clone(), k.state))
                        .collect(),
                    unread: t.inbox.iter().filter(|m| !m.1).count() as u16,
                    held: t.inbox.iter().filter(|m| m.1).count() as u16,
                    retry_secs: t.retry_until.map(|u| (u - now).max(0) as u32).filter(|s| *s > 0),
                }
            })
            .collect();
        order(&mut rows);
        rows
    }
}

/// What a change of a row would show, to tell whether anything moved.
type Print = (
    Option<String>,
    Option<String>,
    Option<f64>,
    Option<(Secs, super::EvKind, u32)>,
    usize,
    Option<(u16, u16)>,
    Option<(u32, u32, Secs)>,
    Option<Secs>,
    Option<(Secs, u32)>,
);

fn fingerprint(t: &AgentTrack) -> Print {
    (
        t.last_step.clone(),
        t.name.clone(),
        t.cost_usd,
        t.ribbon.back().map(|e| (e.at, e.kind, e.n)),
        t.inbox.len() * 2 + t.inbox.iter().filter(|m| m.1).count(),
        t.todo,
        t.tests,
        t.retry_until,
        t.tokens.back().copied(),
    )
}

/// Per-minute tokens of the last 24 minutes; events per minute when the session reports no usage.
fn spark(t: &AgentTrack, now: Secs) -> Vec<u32> {
    let minute = now.div_euclid(60);
    let mut v = vec![0u32; 24];
    if !t.tokens.is_empty() {
        for (m, n) in &t.tokens {
            let back = minute - m;
            if (0..24).contains(&back) {
                v[(23 - back) as usize] = *n;
            }
        }
    } else {
        for e in &t.ribbon {
            let back = minute - e.at.div_euclid(60);
            if (0..24).contains(&back) {
                v[(23 - back) as usize] += e.n;
            }
        }
    }
    if v.iter().all(|n| *n == 0) {
        v.clear();
    }
    v
}

/// Fold the mail observations of one source into the mail table; true when a row changed.
fn fold_mail(mails: &mut BTreeMap<String, MailRow>, o: &super::Observation) -> bool {
    let (id, stage, bytes) = match &o.kind {
        ObsKind::MsgOut { mail, summary, bytes, to } => {
            (mail.clone().unwrap_or_else(|| external_id(mails, summary, o.at, to)), Stage::Sent, *bytes)
        }
        ObsKind::MsgIn { mail, summary, held, from } => (
            mail.clone().unwrap_or_else(|| external_id(mails, summary, o.at, from)),
            if mail.is_none() {
                Stage::Delivered
            } else if *held {
                Stage::Held
            } else {
                Stage::Sent
            },
            0,
        ),
        ObsKind::MsgStage { mail, stage } => (mail.clone(), *stage, 0),
        _ => return false,
    };
    let is_stage = matches!(o.kind, ObsKind::MsgStage { .. });
    let row = mails.entry(id.clone()).or_insert_with(|| MailRow {
        id: id.clone(),
        from: String::new(),
        to: String::new(),
        summary: String::new(),
        stage,
        at: o.at,
        bytes: 0,
        n: 0,
    });
    let before = row.clone();
    match &o.kind {
        ObsKind::MsgOut { to, summary, .. } => {
            row.from = o.agent_key.clone();
            if row.to.is_empty() || !row.to.contains(['-', '.']) {
                row.to = to.clone();
            }
            row.summary = summary.clone();
        }
        ObsKind::MsgIn { from, summary, .. } => {
            row.to = o.agent_key.clone();
            if row.from.is_empty() {
                row.from = from.clone();
            }
            if row.summary.is_empty() {
                row.summary = summary.clone();
            }
        }
        _ => {}
    }
    if bytes > 0 {
        row.bytes = bytes;
    }
    if !is_stage || row.at == 0 {
        row.at = row.at.min(o.at);
    }
    if row.stage.advances_to(stage) {
        row.stage = stage;
    }
    let changed = *row != before;
    if changed {
        row.n = row.n.saturating_add(1);
    }
    while mails.len() > MAIL_KEEP {
        let Some(oldest) = mails.values().min_by_key(|m| m.at).map(|m| m.id.clone()) else { break };
        mails.remove(&oldest);
    }
    changed
}

/// An id for a mail another program's log tells of: the out and the in of one message, seen within 30 s with
/// the same summary, get the same id.
fn external_id(mails: &BTreeMap<String, MailRow>, summary: &str, at: Secs, _peer: &str) -> String {
    let h = input_hash(summary);
    if let Some(m) =
        mails.values().find(|m| m.id.starts_with("x-") && input_hash(&m.summary) == h && (m.at - at).abs() <= 30)
    {
        return m.id.clone();
    }
    format!("x-{h:x}-{at}")
}

/// The name a card shows: the session's name, else its folder, else the start of its key.
pub fn display_name(t: &AgentTrack) -> String {
    if let Some(n) = t.name.as_deref().filter(|n| !n.is_empty()) {
        return n.to_string();
    }
    if let Some(f) = t.cwd.as_deref().and_then(|c| Path::new(c).file_name()).and_then(|f| f.to_str()) {
        return f.to_string();
    }
    t.key.chars().take(8).collect()
}

/// Nexo sessions first, then by name; a sub-agent follows its parent.
pub fn order(rows: &mut Vec<AgentRow>) {
    rows.sort_by(|a, b| {
        (a.source, a.program.to_lowercase(), a.name.to_lowercase(), &a.key).cmp(&(
            b.source,
            b.program.to_lowercase(),
            b.name.to_lowercase(),
            &b.key,
        ))
    });
    let mut out: Vec<AgentRow> = Vec::with_capacity(rows.len());
    let mut rest: Vec<AgentRow> = std::mem::take(rows);
    let (kids, tops): (Vec<AgentRow>, Vec<AgentRow>) = rest.drain(..).partition(|r| r.parent.is_some());
    let mut kids = kids;
    for top in tops {
        let key = top.key.clone();
        out.push(top);
        let (mine, other): (Vec<AgentRow>, Vec<AgentRow>) =
            kids.drain(..).partition(|k| k.parent.as_deref() == Some(&key));
        out.extend(mine);
        kids = other;
    }
    out.extend(kids);
    *rows = out;
}

/// Where the sources look.
#[derive(Clone, Debug)]
pub struct Setup {
    pub logs_root: Option<PathBuf>,
    pub scan_hours: u64,
    /// The program that lists the other tool's running sessions, and its arguments.
    pub list_cmd: Option<(String, Vec<String>)>,
    pub list_every: Secs,
    pub poll_every_ms: u64,
}

impl Setup {
    /// The usual places on this machine. Nothing is created; a place that does not exist is skipped.
    pub fn detect(policy: &Policy) -> Setup {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
        Setup {
            logs_root: home.map(|h| h.join(".claude").join("projects")),
            scan_hours: policy.scan_hours,
            list_cmd: Some(("claude".to_string(), vec!["agents".to_string(), "--json".to_string()])),
            list_every: 10,
            poll_every_ms: 1000,
        }
    }

    /// No external sources at all (tests, or a person who turned them off).
    pub fn none() -> Setup {
        Setup { logs_root: None, scan_hours: 12, list_cmd: None, list_every: 10, poll_every_ms: 1000 }
    }

    pub fn supervisor(&self, policy: Policy, nexo: NexoSource) -> Supervisor {
        let mut s = Supervisor::new(policy);
        s.nexo = Some(nexo.clone());
        s.add(Box::new(nexo));
        if let Some(root) = &self.logs_root {
            s.add(Box::new(JsonlSource::new(root.clone(), self.scan_hours)));
        }
        if let Some((prog, args)) = &self.list_cmd {
            s.add(Box::new(ListSource::new(prog.clone(), args.clone(), self.list_every)));
        }
        s
    }
}

/// The wall clock in seconds.
pub fn now_secs() -> Secs {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as Secs).unwrap_or(0)
}

/// Where a [`Panel`] keeps its files and listens. The default has neither: nothing is written.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The hook socket. If another supervisor already answers there, the panel forwards the person's
    /// actions to it instead of listening itself.
    pub socket: Option<PathBuf>,
    /// The folder of `audit.jsonl` and `state.json`.
    pub dir: Option<PathBuf>,
    /// Fail rather than forward when the socket is taken (the daemon).
    pub must_listen: bool,
}

impl Options {
    /// The usual places under the nexo data folder.
    pub fn detect() -> Options {
        Options {
            socket: Some(super::socket::default_path()),
            dir: Some(super::socket::default_dir()),
            must_listen: false,
        }
    }
}

/// What the polling thread, the socket thread and the panel share.
struct Core {
    sup: Mutex<Supervisor>,
    /// Held while the sources are being read, so a hook can wait for the round in flight.
    polling: Mutex<()>,
    version: AtomicU64,
    state_file: Option<PathBuf>,
}

impl Core {
    /// Poll every source and fold the result in. The sources run outside the lock a drawing frame wants.
    fn round(&self, now: Secs) {
        let _turn = self.polling.lock();
        let mut taken = self.sup.lock().map(|mut g| std::mem::take(&mut g.sources)).unwrap_or_default();
        let polled: Vec<Polled> = taken.iter_mut().map(|s| (s.kind(), s.program(), s.poll(now))).collect();
        if let Ok(mut g) = self.sup.lock() {
            // a source added while the round ran stays
            taken.append(&mut g.sources);
            g.sources = taken;
            g.fold(polled, now);
            self.after(&g, now);
        }
    }

    fn after(&self, g: &Supervisor, now: Secs) {
        if self.version.swap(g.version, Ordering::Relaxed) != g.version {
            if let Some(p) = &self.state_file {
                let _ = write_state(p, &g.rows(now), now);
            }
        }
    }

    fn handle(&self, req: super::socket::Request) -> super::socket::Reply {
        let now = now_secs();
        if matches!(&req, super::socket::Request::Hook { kind: super::socket::HookKind::Stop, .. }) {
            // the turn that just ended may not have been read yet
            self.round(now);
        }
        match self.sup.lock() {
            Ok(mut g) => {
                let r = g.handle(req, now);
                self.after(&g, now);
                r
            }
            Err(_) => super::socket::Reply::ok(serde_json::json!({})),
        }
    }
}

/// A supervisor polling in the background for as long as this handle lives.
pub struct Panel {
    core: Arc<Core>,
    stop: Arc<AtomicBool>,
    remote: Option<PathBuf>,
    _server: Option<super::socket::Server>,
}

impl Panel {
    /// Reads only: no socket, no files.
    pub fn start(setup: &Setup, policy: Policy, nexo: NexoSource) -> Panel {
        Panel::start_with(setup, policy, nexo, &Options::default()).expect("no socket to take")
    }

    pub fn start_with(setup: &Setup, policy: Policy, nexo: NexoSource, opts: &Options) -> std::io::Result<Panel> {
        let mut sup = setup.supervisor(policy, nexo);
        if let Some(d) = &opts.dir {
            sup.audit = Some(Audit::new(d.join("audit.jsonl")));
        }
        let core = Arc::new(Core {
            sup: Mutex::new(sup),
            polling: Mutex::new(()),
            version: AtomicU64::new(0),
            state_file: opts.dir.as_ref().map(|d| d.join("state.json")),
        });
        let (mut server, mut remote) = (None, None);
        if let Some(path) = &opts.socket {
            let c = core.clone();
            match super::socket::serve(path, Arc::new(move |r| c.handle(r))) {
                Ok(s) => server = Some(s),
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && !opts.must_listen => remote = Some(path.clone()),
                Err(e) => return Err(e),
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (c, st, every) = (core.clone(), stop.clone(), setup.poll_every_ms.max(100));
        std::thread::spawn(move || {
            while !st.load(Ordering::Relaxed) {
                c.round(now_secs());
                let mut waited = 0;
                while waited < every && !st.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    waited += 50;
                }
            }
        });
        Ok(Panel { core, stop, remote, _server: server })
    }

    pub fn version(&self) -> u64 {
        self.core.version.load(Ordering::Relaxed)
    }

    /// The mails between sessions from `since` on, newest last (at most 40), one row per mail.
    pub fn mail_feed(&self, since: Secs) -> Vec<MailRow> {
        self.core.sup.lock().map(|g| g.mail_feed(since)).unwrap_or_default()
    }

    /// Add a source to a panel that is already polling.
    pub fn add_source(&self, source: Box<dyn Source>) {
        if let Ok(mut g) = self.core.sup.lock() {
            g.add(source);
        }
    }

    pub fn rows(&self, now: Secs) -> Vec<AgentRow> {
        self.core.sup.lock().map(|g| g.rows(now)).unwrap_or_default()
    }

    /// Let the supervisor ask a model (only used if the policy has a `[model]` section).
    pub fn set_judge(&self, judge: std::sync::Arc<dyn super::model::Judge>) {
        if let Ok(mut g) = self.core.sup.lock() {
            g.set_judge(judge);
        }
    }

    /// News for the policy tab: the errors of loading the policy, why a model is not in use.
    pub fn set_notes(&self, notes: Vec<String>) {
        if let Ok(mut g) = self.core.sup.lock() {
            g.notes = notes;
            g.version += 1;
        }
    }

    /// The effective policy as lines of text, then the notes, then the model's spend of this hour.
    pub fn policy_lines(&self) -> Vec<String> {
        let now = now_secs();
        let Ok(mut g) = self.core.sup.lock() else { return Vec::new() };
        let mut out: Vec<String> = Vec::new();
        for n in &g.notes {
            out.push(format!("! {n}"));
        }
        match g.model_usage(now) {
            Some((usd, calls, cap_usd, cap_calls)) => {
                out.push(format!("model this hour: ${usd:.3} of ${cap_usd:.2} · {calls} of {cap_calls} calls"));
            }
            None if g.policy.model.is_some() => out.push("model: named in the policy but not in use".into()),
            None => out.push("model: none (rules only, no network)".into()),
        }
        out.push(String::new());
        match toml::to_string_pretty(&g.policy) {
            Ok(t) => out.extend(t.lines().map(String::from)),
            Err(e) => out.push(format!("! cannot show the policy: {e}")),
        }
        out
    }

    /// Whether every session is paused.
    pub fn paused_all(&self) -> bool {
        self.core.sup.lock().map(|g| g.paused_all).unwrap_or(false)
    }

    /// The text a nudge to this session starts from.
    pub fn template(&self, key: &str) -> String {
        self.core.sup.lock().map(|g| g.template(key, now_secs())).unwrap_or_default()
    }

    /// Queue the person's nudge for the session's next stop.
    pub fn nudge(&self, key: &str, text: &str) {
        if let Ok(mut g) = self.core.sup.lock() {
            g.queue_manual(key, text, now_secs());
        }
        if let Some(p) = &self.remote {
            let req = super::socket::Request::Nudge { key: key.into(), text: text.into() };
            let _ = super::socket::ask(p, &req, super::socket::HOOK_TIMEOUT);
        }
    }

    /// Pause (or resume) one session, or all of them.
    pub fn pause(&self, key: Option<&str>, on: bool) {
        if let Ok(mut g) = self.core.sup.lock() {
            g.set_pause(key, on, now_secs());
        }
        if let Some(p) = &self.remote {
            let req = super::socket::Request::Pause { key: key.map(String::from), on };
            let _ = super::socket::ask(p, &req, super::socket::HOOK_TIMEOUT);
        }
    }

    /// True when another process owns the hook socket and this panel forwards to it.
    pub fn is_remote(&self) -> bool {
        self.remote.is_some()
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Write the rows to `path` as JSON, atomically (a reader sees the old file or the new one, never half).
pub fn write_state(path: &Path, rows: &[AgentRow], now: Secs) -> std::io::Result<()> {
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "key": r.key, "name": r.name, "source": r.source.label(), "program": r.program, "state": r.state.label(),
                "since": r.since, "idle_secs": r.idle_secs, "last_step": r.last_step,
                "cost_usd": r.cost_usd, "model": r.model, "cwd": r.cwd, "parent": r.parent, "paused": r.paused,
                "last_action": r.last_action.as_ref().map(|(t, at)| serde_json::json!({"text": t, "at": at})),
            })
        })
        .collect();
    let doc = serde_json::json!({ "at": now, "agents": items });
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&doc)?)?;
    std::fs::rename(&tmp, path)
}

impl std::fmt::Debug for Panel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Panel").field("version", &self.version()).finish()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervise::EvKind;
    use crate::supervise::{ObsKind, Observation};
    use nexo_core::agent::AgentEvent;

    struct Fake(SourceKind, Vec<Observation>);
    impl Source for Fake {
        fn id(&self) -> &str {
            "fake"
        }
        fn kind(&self) -> SourceKind {
            self.0
        }
        fn poll(&mut self, _now: Secs) -> Vec<Observation> {
            std::mem::take(&mut self.1)
        }
    }

    fn ob(key: &str, at: Secs, kind: ObsKind) -> Observation {
        Observation { agent_key: key.into(), at, kind }
    }

    #[test]
    fn a_session_goes_from_working_to_idle_to_stalled_and_each_change_is_marked() {
        let mut s = Supervisor::new(Policy::default());
        s.add(Box::new(Fake(
            SourceKind::External,
            vec![
                ob(
                    "a",
                    1000,
                    ObsKind::Meta { name: Some("Builder".into()), cwd: Some("/w/app".into()), permission_mode: None },
                ),
                ob("a", 1000, ObsKind::Step { text: "Bash  cargo test".into() }),
                ob("a", 1000, ObsKind::TurnEnd { last_text_tail: "done".into() }),
            ],
        )));
        assert!(s.tick(1010));
        let rows = s.rows(1010);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Builder");
        assert_eq!(rows[0].last_step.as_deref(), Some("Bash  cargo test"));
        assert_eq!(rows[0].idle_secs, Some(10));
        let v = s.version;
        assert!(!s.tick(1011), "nothing changed");
        assert_eq!(s.version, v);
        assert!(s.tick(1000 + 6 * 60), "idle after the threshold");
        let r = &s.rows(1000 + 6 * 60)[0];
        assert!(matches!(r.state, State::Idle { .. }));
        assert_eq!(r.last_action.as_ref().map(|a| a.0.as_str()), Some("marked idle"));
        assert!(s.tick(1000 + 40 * 60));
        assert_eq!(s.rows(1000 + 40 * 60)[0].state, State::Stalled);
    }

    #[test]
    fn nexo_events_become_a_session_with_sub_agents_under_it() {
        use nexo_core::jobs::{JobInfo, JobKind, JobStatus};
        let nexo = NexoSource::new();
        let mut s = Supervisor::new(Policy::default());
        s.add(Box::new(nexo.clone()));
        nexo.observe(&AgentEvent::SessionChanged { session_id: "s1".into(), title: "Main work".into() }, 100);
        nexo.observe(&AgentEvent::TurnStart { turn: 1 }, 100);
        nexo.observe(
            &AgentEvent::Jobs {
                jobs: vec![JobInfo {
                    id: 3,
                    kind: JobKind::Agent,
                    label: "scan the docs".into(),
                    status: JobStatus::Running,
                    elapsed_ms: 0,
                    lines: 0,
                }],
            },
            101,
        );
        s.tick(105);
        let rows = s.rows(105);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].key.as_str(), rows[0].source), ("s1", SourceKind::Nexo));
        assert_eq!(rows[0].program, "nexo");
        assert_eq!((rows[1].key.as_str(), rows[1].parent.as_deref()), ("s1.3", Some("s1")));
        assert_eq!(rows[1].name, "scan the docs");
    }

    #[test]
    fn the_state_file_is_written_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("supervisor").join("state.json");
        let mut s = Supervisor::new(Policy::default());
        s.add(Box::new(Fake(SourceKind::External, vec![ob("k", 5, ObsKind::Activity)])));
        s.tick(6);
        write_state(&path, &s.rows(6), 6).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["agents"][0]["state"], "working");
        assert_eq!(v["agents"][0]["source"], "external");
        let left: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "no temporary file stays");
    }
    #[test]
    fn nexo_events_fill_the_ribbon_the_counters_and_the_mail_feed() {
        use nexo_core::mail::Stage;
        let nexo = NexoSource::new();
        let mut s = Supervisor::new(Policy::default());
        s.add(Box::new(nexo.clone()));
        nexo.observe(&AgentEvent::SessionChanged { session_id: "s1".into(), title: "api".into() }, 100);
        nexo.observe(&AgentEvent::TurnStart { turn: 1 }, 100);
        nexo.observe(&AgentEvent::TodoChanged { done: 1, total: 4 }, 101);
        nexo.observe(&AgentEvent::Retrying { attempt: 1, wait_secs: 30, rate_limit: true }, 102);
        nexo.observe(&AgentEvent::Compacted { before_tokens: 9000, after_tokens: 900 }, 103);
        nexo.observe(
            &AgentEvent::MailSent {
                id: "m-1".into(),
                to: "s2".into(),
                to_name: "web".into(),
                summary: "hi".into(),
                bytes: 2,
            },
            104,
        );
        nexo.observe(
            &AgentEvent::MailArrived {
                id: "m-2".into(),
                from: "s2".into(),
                from_name: "web".into(),
                summary: "yo".into(),
                held: false,
            },
            105,
        );
        nexo.observe(
            &AgentEvent::MailArrived {
                id: "m-3".into(),
                from: "s2".into(),
                from_name: "web".into(),
                summary: "ok?".into(),
                held: true,
            },
            105,
        );
        s.tick(106);
        let r = &s.rows(106)[0];
        assert_eq!((r.todo, r.unread, r.held), (Some((1, 4)), 1, 1));
        assert_eq!(r.retry_secs, Some(26));
        let kinds: Vec<(EvKind, u32)> = r.ribbon.iter().map(|e| (e.kind, e.n)).collect();
        // two mails in one second fold into one cell
        let want = vec![
            (EvKind::TurnStart, 1),
            (EvKind::Todo, 1),
            (EvKind::RateLimit, 1),
            (EvKind::Compact, 1),
            (EvKind::MsgOut, 1),
            (EvKind::MsgIn, 2),
        ];
        assert_eq!(kinds, want);
        let feed = s.mail_feed(0);
        assert_eq!(
            feed.iter().map(|m| (m.id.as_str(), m.stage)).collect::<Vec<_>>(),
            vec![("m-1", Stage::Sent), ("m-2", Stage::Sent), ("m-3", Stage::Held)]
        );
        // the receiver's host delivers m-2 and the model reads it; the mail leaves the unread count
        nexo.observe(&AgentEvent::MailDelivered { ids: vec!["m-2".into()] }, 110);
        nexo.observe(&AgentEvent::TurnStart { turn: 2 }, 110);
        nexo.observe(&AgentEvent::MailRead { ids: vec!["m-2".into()] }, 111);
        s.tick(112);
        let r = &s.rows(112)[0];
        assert_eq!((r.unread, r.held), (0, 1));
        let m2 = s.mail_feed(0).into_iter().find(|m| m.id == "m-2").unwrap();
        assert_eq!(m2.stage, Stage::Read);
        assert!(
            !r.ribbon.iter().any(|e| e.kind == EvKind::TurnStart && e.at == 110),
            "the mail turn is not the person's"
        );
    }

    #[test]
    fn two_sources_telling_one_mail_make_one_row_and_one_ribbon_event() {
        let nexo = NexoSource::new();
        let mut s = Supervisor::new(Policy::default());
        s.add(Box::new(nexo.clone()));
        nexo.observe(&AgentEvent::SessionChanged { session_id: "s1".into(), title: "api".into() }, 100);
        let out = |at| {
            ob(
                "s1",
                at,
                ObsKind::MsgOut { to: "s2".into(), mail: Some("m-9".into()), summary: "hello".into(), bytes: 5 },
            )
        };
        nexo.observe(
            &AgentEvent::MailSent {
                id: "m-9".into(),
                to: "s2".into(),
                to_name: "web".into(),
                summary: "hello".into(),
                bytes: 5,
            },
            101,
        );
        s.add(Box::new(Fake(SourceKind::Nexo, vec![out(101)])));
        s.tick(102);
        assert_eq!(s.mail_feed(0).len(), 1);
        let r = &s.rows(102)[0];
        assert_eq!(r.ribbon.iter().filter(|e| e.kind == EvKind::MsgOut).map(|e| e.n).sum::<u32>(), 1);
    }

    #[test]
    fn a_mail_of_a_log_pairs_its_out_and_its_in_and_a_stranger_gets_no_card() {
        let mut s = Supervisor::new(Policy::default());
        s.add(Box::new(Fake(
            SourceKind::External,
            vec![
                ob("k1", 1000, ObsKind::Activity),
                ob("k2", 1000, ObsKind::Activity),
                ob(
                    "k1",
                    1010,
                    ObsKind::MsgOut { to: "second".into(), mail: None, summary: "build is red".into(), bytes: 12 },
                ),
                ob(
                    "k2",
                    1015,
                    ObsKind::MsgIn { from: "first".into(), mail: None, summary: "build is red".into(), held: false },
                ),
                ob(
                    "ghost",
                    1016,
                    ObsKind::MsgIn { from: "x".into(), mail: Some("m-5".into()), summary: "boo".into(), held: false },
                ),
            ],
        )));
        s.tick(1020);
        let feed = s.mail_feed(0);
        let x: Vec<_> = feed.iter().filter(|m| m.id.starts_with("x-")).collect();
        assert_eq!(x.len(), 1, "{feed:?}");
        assert_eq!((x[0].from.as_str(), x[0].to.as_str()), ("k1", "k2"));
        assert_eq!(x[0].stage, nexo_core::mail::Stage::Delivered);
        assert_eq!(s.rows(1020).len(), 2, "the mail of a session nobody tracks adds no card");
        assert!(feed.iter().any(|m| m.id == "m-5"));
    }
}
