//! The supervisor's read-only loop: poll the sources, fold what they saw into one track per session, classify
//! each track and keep the rows the panel draws. In this stage it only marks (the state of a session); it
//! sends nothing to any session.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::audit::Audit;
use super::decide::Ledger;
use super::policy::Policy;
use super::source::{Source, SourceKind};
use super::source_jsonl::JsonlSource;
use super::source_list::ListSource;
use super::source_nexo::NexoSource;
use super::{classify, AgentTrack, Secs, State};

/// What the panel shows of one session.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRow {
    pub key: String,
    pub name: String,
    pub source: SourceKind,
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
}

pub(super) struct Entry {
    pub(super) track: AgentTrack,
    pub(super) source: SourceKind,
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
        let polled = self.sources.iter_mut().map(|s| (s.kind(), s.poll(now))).collect();
        self.fold(polled, now)
    }

    /// Fold what the sources returned in and classify every session.
    pub(super) fn fold(&mut self, polled: Vec<(SourceKind, Vec<super::Observation>)>, now: Secs) -> bool {
        let mut changed = false;
        for (kind, mut obs) in polled {
            obs.sort_by_key(|o| o.at);
            for o in obs {
                if o.agent_key.is_empty() {
                    continue;
                }
                let e = self.entries.entry(o.agent_key.clone()).or_insert_with(|| Entry {
                    track: AgentTrack::new(o.agent_key.clone()),
                    source: kind,
                    state: State::Working,
                    since: now,
                    action: None,
                });
                let before = (e.track.last_step.clone(), e.track.name.clone(), e.track.cost_usd);
                e.track.apply(&o);
                changed |= before != (e.track.last_step.clone(), e.track.name.clone(), e.track.cost_usd);
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
                }
            })
            .collect();
        order(&mut rows);
        rows
    }
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
    rows.sort_by(|a, b| (a.source, a.name.to_lowercase(), &a.key).cmp(&(b.source, b.name.to_lowercase(), &b.key)));
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
        let polled: Vec<(SourceKind, Vec<super::Observation>)> =
            taken.iter_mut().map(|s| (s.kind(), s.poll(now))).collect();
        if let Ok(mut g) = self.sup.lock() {
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
                "key": r.key, "name": r.name, "source": r.source.label(), "state": r.state.label(),
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
}
