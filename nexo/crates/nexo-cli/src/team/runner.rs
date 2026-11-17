//! `nexo team run <name>`: one lane's runner. It sets up one long-lived agent the way a headless run does (the
//! same setup, the same gate that refuses what nobody can be asked), opens the session's mailbox as the
//! interface does, so the lane is live (other sessions and /panel see it and its mail is delivered between
//! turns), and submits the lane's routine now and then every `every`, start to start. It writes the lane's state
//! after each round and on exit, and honours the stop and pause files between turns.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nexo_core::agent::{channels, AgentCommand, AgentEvent, Level, TurnEnd};
use nexo_core::config::Lane;
use nexo_core::mailbox::{now_ms, Mailbox};
use nexo_core::permission::PermissionMode;

use crate::setup;
use nexo_core::team::state::{self, LaneState, Round};

/// Failed rounds in a row that end the runner.
const FAILURES_MAX: u32 = 3;

/// How often the runner looks at its mailbox and at the stop and pause files.
const TICK: Duration = Duration::from_millis(500);

/// The configuration, folder and model of the lane, as `nexo -p` in that folder would have them.
async fn prepare(lane: &Lane) -> anyhow::Result<(nexo_core::Config, PathBuf, setup::Picked)> {
    let dir = nexo_core::team::lane_dir(lane);
    let cwd = nexo_core::paths::canonical(&dir).map_err(|e| anyhow::anyhow!("dir {}: {e}", dir.display()))?;
    let provider = lane.model.as_deref().and_then(|m| m.split_once('/')).map(|(p, _)| p);
    let mut config = crate::cli::load_config_for(provider, &cwd)?;
    if let Some(m) = &lane.mode {
        let mode = PermissionMode::parse(m).ok_or_else(|| anyhow::anyhow!("unknown permission mode {m:?}"))?;
        config.check_mode(mode).map_err(|e| anyhow::anyhow!(e))?;
        config.permissions.mode = mode;
    }
    lane.apply_sandbox(&mut config).map_err(|e| anyhow::anyhow!("lane {}: {e}", lane.name))?;
    if lane.model.is_none() && setup::nothing_chosen(&config) {
        anyhow::bail!(setup::nothing_chosen_message());
    }
    let picked = setup::pick(&config, None, lane.model.as_deref())?;
    Ok((config, cwd, picked))
}

/// The routine of a round: the lane's text, or the section of its file as it is now.
fn routine(lane: &Lane, cwd: &std::path::Path) -> Result<String, String> {
    if let Some(t) = &lane.routine {
        return Ok(t.clone());
    }
    let (Some(f), Some(s)) = (&lane.routine_file, &lane.routine_section) else {
        return Err("no routine".into());
    };
    let text = std::fs::read_to_string(cwd.join(f)).map_err(|e| format!("routine_file {f}: {e}"))?;
    nexo_core::config::routine_section(&text, s)
        .ok_or_else(|| format!("routine_file {f}: no code block under the line \"{s}:\""))
}

/// The local time of day, for the log.
fn stamp() -> String {
    super::clock(now_ms())
}

/// How long the first round waits: none when asked for at once (`now`), when the lane has no clock, or when it has
/// never had a round; otherwise what is left of `every` since the last round (`last_at`, in ms).
fn first_wait(last_at: Option<i64>, every: Option<Duration>, now_ms: i64, now: bool) -> Duration {
    let (Some(last), Some(every), false) = (last_at, every, now) else { return Duration::ZERO };
    let next = last.saturating_add(every.as_millis() as i64);
    Duration::from_millis((next - now_ms).max(0) as u64)
}

/// A round (or a mail's turn) in flight.
#[derive(Default)]
struct Turn {
    /// A round of the routine, not a mail's turn.
    round: bool,
    started: Option<Instant>,
    end: Option<TurnEnd>,
    denied: bool,
    error: Option<String>,
    capped: bool,
    text: String,
    spend: f64,
}

impl Turn {
    /// The result the state file records: `ok`, `failed`, `denied` or `capped`.
    fn result(&self) -> &'static str {
        match self.end {
            _ if self.capped => "capped",
            Some(TurnEnd::Done) | None if self.denied => "denied",
            Some(TurnEnd::Done) | None if self.error.is_none() => "ok",
            _ => "failed",
        }
    }
}

struct Runner {
    name: String,
    lane: Lane,
    cwd: PathBuf,
    every: Option<Duration>,
    st: LaneState,
    mail: Option<Arc<Mailbox>>,
    /// The session's cost as the agent last said it: what it adds is the lane's spending.
    cost_seen: Option<f64>,
    turn: Option<Turn>,
    /// When the next round is due; None: none is planned.
    due: Option<Instant>,
    paused: bool,
}

impl Runner {
    fn save(&mut self) {
        self.st.next_round =
            self.due.map(|d| now_ms() + d.saturating_duration_since(Instant::now()).as_millis() as i64);
        if let Err(e) = self.st.save(&self.name) {
            eprintln!("[{}] cannot write the lane's state: {e}", stamp());
        }
    }

    /// The session's cost is now `cost`: what it grew by is spent by the lane (and by the turn in flight).
    fn costs(&mut self, cost: f64) {
        let Some(seen) = self.cost_seen.replace(cost) else { return };
        let grew = (cost - seen).max(0.0);
        if grew > 0.0 {
            self.st.spend(&nexo_agent::budget::today(), grew);
            if let Some(t) = &mut self.turn {
                t.spend += grew;
            }
        }
    }

    fn on_event(&mut self, ev: &AgentEvent) {
        if let Some(m) = &self.mail {
            m.observe(ev);
        }
        match ev {
            // a turn nobody here started (queued mail): it is followed like a mail's turn, so no round starts over it
            AgentEvent::TurnStart { .. } if self.turn.is_none() => {
                self.turn = Some(Turn { started: Some(Instant::now()), ..Default::default() });
            }
            AgentEvent::Ready { session_id, .. } | AgentEvent::SessionChanged { session_id, .. } => {
                if self.st.session_id.as_deref() != Some(session_id) {
                    self.st.session_id = Some(session_id.clone());
                    self.save();
                }
            }
            AgentEvent::Budget { session_cost, .. } => self.costs(*session_cost),
            AgentEvent::Usage { cost_usd, .. } => self.costs(*cost_usd),
            AgentEvent::AssistantStart => {
                if let Some(t) = &mut self.turn {
                    t.text.clear();
                }
            }
            AgentEvent::TextDelta { text } => {
                if let Some(t) = &mut self.turn {
                    t.text.push_str(text);
                }
            }
            AgentEvent::ToolDone { output, .. }
                if output.is_error && output.content.starts_with("Permission denied") =>
            {
                if let Some(t) = &mut self.turn {
                    t.denied = true;
                }
            }
            AgentEvent::Notice { level: Level::Error, text, problem } => {
                let said = problem.as_ref().map_or_else(|| nexo_core::problem::tidy(text), |p| p.one_line());
                eprintln!("[{}] error: {said}", stamp());
                if let Some(t) = &mut self.turn {
                    t.capped |= said.starts_with("spend limit reached");
                    t.error = Some(said);
                }
            }
            AgentEvent::TurnEnd { reason } => {
                if let Some(t) = &mut self.turn {
                    if t.end.is_none() || *reason != TurnEnd::Done {
                        t.end = Some(*reason);
                    }
                }
            }
            _ => {}
        }
    }

    /// The agent is idle again: the turn in flight is over. A round's result goes to the state file; the third
    /// failed round in a row is the lane's end (true).
    fn turn_over(&mut self) -> bool {
        let Some(t) = self.turn.take() else { return false };
        let result = t.result();
        let reply = t.text.trim();
        let what = if t.round { format!("round {}", self.st.rounds + 1) } else { "mail".to_string() };
        println!("[{}] {what}: {result} (${:.4}){}", stamp(), t.spend, if reply.is_empty() { "" } else { ":" });
        if !reply.is_empty() {
            println!("{reply}");
        }
        if result == "denied" {
            println!(
                "[{}] a tool call was refused: nobody can be asked in a lane; set its `mode` or an allow rule",
                stamp()
            );
        }
        if !t.round {
            self.save();
            return false;
        }
        self.st.rounds += 1;
        self.st.last_round = Some(Round { at: now_ms(), result: result.to_string(), spend: t.spend });
        if result == "failed" {
            self.st.failures += 1;
            self.st.why = t.error.clone().or_else(|| Some(format!("the round ended: {:?}", t.end)));
        } else {
            self.st.failures = 0;
        }
        // start to start; a round that ran past the next start is followed at once, never doubled
        self.due = self.every.map(|e| t.started.map_or_else(Instant::now, |s| s + e));
        self.st.state = "idle".into();
        if self.st.failures >= FAILURES_MAX {
            self.st.state = "failed".into();
            self.due = None;
        }
        self.save();
        self.st.failures >= FAILURES_MAX
    }

    /// Starts the round that is due, or records it as capped when the lane spent its day.
    fn start_round(&mut self, commands: &tokio::sync::mpsc::UnboundedSender<AgentCommand>) {
        let today = nexo_agent::budget::today();
        if let Some(cap) = self.lane.max_spend_day {
            let left = cap - self.st.spent_on(&today);
            if left <= 0.0 {
                println!("[{}] round skipped: ${cap:.2} spent today (max_spend_day)", stamp());
                self.st.last_round = Some(Round { at: now_ms(), result: "capped".into(), spend: 0.0 });
                self.due = self.every.map(|e| Instant::now() + e);
                self.save();
                return;
            }
            // the session's own limit stops the round when the lane's day runs out, as `/budget` would
            let limit = self.cost_seen.unwrap_or(0.0) + left;
            let _ = commands.send(AgentCommand::SetBudget { limit: Some(limit) });
        }
        let text = match routine(&self.lane, &self.cwd) {
            Ok(t) => t,
            Err(e) => {
                println!("[{}] round not started: {e}", stamp());
                self.turn = Some(Turn { round: true, error: Some(e), end: Some(TurnEnd::Error), ..Default::default() });
                self.turn_over();
                return;
            }
        };
        println!("[{}] round {} starts", stamp(), self.st.rounds + 1);
        if let Some(m) = &self.mail {
            // no mail is handed over between this request and the turn it starts
            m.set_idle(false);
        }
        self.turn = Some(Turn { round: true, started: Some(Instant::now()), ..Default::default() });
        self.due = None;
        self.st.state = "running".into();
        self.save();
        let _ = commands.send(AgentCommand::Submit { text, attachments: vec![] });
    }
}

/// Runs lane `lane` until its stop file, its third failed round in a row or the agent's end. The exit code: 0
/// stopped, 1 failed.
pub async fn run(lane: &Lane, now: bool) -> i32 {
    let name = lane.name.clone();
    let me = std::process::id();
    let mut st = LaneState::load(&name);
    if let Some(p) = st.pid.filter(|p| *p != me && state::is_runner(*p, &name)) {
        println!("already running: {name} (process {p})");
        return 0;
    }
    // `team start --now` asks through a file, so this runner's command line is the plain one
    let now = now || std::fs::remove_file(state::now_file(&name)).is_ok();
    let fail = |mut st: LaneState, why: String| {
        eprintln!("[{}] {name}: {why}", stamp());
        st.state = "failed".into();
        st.why = Some(why);
        st.pid = None;
        st.next_round = None;
        let _ = st.save(&name);
        let _ = std::fs::remove_file(state::stop_file(&name));
        1
    };
    let (config, cwd, picked) = match prepare(lane).await {
        Ok(x) => x,
        Err(e) => return fail(st, format!("{e:#}")),
    };
    let every = lane.every.as_deref().and_then(|e| nexo_core::config::parse_every(e).ok());
    st.pid = Some(me);
    st.started = Some(now_ms());
    st.state = "idle".into();
    st.why = None;
    st.failures = 0;
    let _ = st.save(&name);
    println!(
        "[{}] {name}: runner {me} in {} with {}/{}{}",
        stamp(),
        cwd.display(),
        picked.provider,
        picked.model,
        every.map(|e| format!(", a round every {}s", e.as_secs())).unwrap_or_default()
    );

    // the lane is live as any session is: its mailbox beats and its mail comes in between turns
    let mail = config.mail.enabled.then(|| {
        Mailbox::unbound(
            &nexo_core::mailbox::mail_root(),
            &cwd.display().to_string(),
            config.permissions.mode == PermissionMode::Auto,
            nexo_core::mail::Inbound::parse(&config.mail.inbound),
        )
    });
    // A lane's session is a program's, not the person's: it stays out of `/resume` and `nexo -c`, as a `nexo -p` run's
    // does. Sessions this runner starts are marked at birth; the one the lane already has is marked now, before the
    // agent opens it. `nexo team open` and `/team` still open it by its id.
    nexo_agent::session::mark_headless();
    if let Some(id) = &st.session_id {
        if let Err(e) = nexo_agent::session::mark_headless_in(&nexo_agent::session::dir(), id) {
            eprintln!("[{}] cannot mark the lane's session as a program's: {e}", stamp());
        }
    }
    let (mut handle, ends) = channels();
    let gate: Arc<dyn nexo_core::PermissionGate> = Arc::new(setup::HeadlessGate);
    let agent_setup =
        match setup::agent_setup(&config, &cwd, &picked, st.session_id.clone(), Some(gate), mail.clone()).await {
            Ok(s) => s,
            Err(e) => return fail(st, format!("{e:#}")),
        };
    let agent = tokio::spawn(nexo_agent::run(ends, agent_setup));

    // a restart does not run a round the lane has no need of: the clock goes on from the last one
    let wait = first_wait(st.last_round.as_ref().map(|r| r.at), every, now_ms(), now);
    if let (Some(last), false) = (st.last_round.as_ref(), wait.is_zero()) {
        println!(
            "[{}] next round at {} (last one {} min ago)",
            stamp(),
            super::clock(now_ms() + wait.as_millis() as i64),
            (now_ms() - last.at).max(0) / 60_000
        );
    }
    let mut r = Runner {
        name: name.clone(),
        lane: lane.clone(),
        cwd,
        every,
        st,
        mail,
        cost_seen: None,
        turn: None,
        due: Some(Instant::now() + wait),
        paused: false,
    };
    if let Some(m) = &r.mail {
        m.set_lane(&name);
    }
    // the session's name is the lane's, which is how the other sessions and /panel know it
    let _ = handle.commands.send(AgentCommand::Rename { title: name.clone() });
    // what the session has spent before: what it adds from now on is the lane's
    let _ = handle.commands.send(AgentCommand::ShowBudget);
    let mut tick = tokio::time::interval(TICK);
    let mut failed = false;
    let mut gone = None;
    loop {
        tokio::select! {
            ev = handle.events.recv() => {
                let Some(ev) = ev else { gone = Some("the agent stopped".to_string()); break };
                r.on_event(&ev);
                match ev {
                    AgentEvent::Idle { .. } => {
                        if r.turn_over() {
                            failed = true;
                            break;
                        }
                    }
                    AgentEvent::Shutdown => {
                        gone = Some("the agent stopped".to_string());
                        break;
                    }
                    _ => {}
                }
            }
            _ = tick.tick() => {
                if r.turn.is_some() {
                    if let Some(m) = &r.mail {
                        // a session at work keeps showing as live; mail taken as the turn ends is queued by the
                        // agent for the turn after
                        let t = m.tick();
                        if !t.deliver.is_empty() {
                            let _ = handle.commands.send(AgentCommand::Deliver { mails: t.deliver });
                        }
                    }
                    continue;
                }
                if state::stop_file(&name).exists() {
                    println!("[{}] {name}: stop asked", stamp());
                    break;
                }
                let paused = state::paused();
                if paused != r.paused {
                    r.paused = paused;
                    if let Some(m) = &r.mail {
                        // paused, mail waits in the inbox like the rounds do
                        m.set_idle(!paused);
                    }
                    r.st.state = if paused { "paused" } else { "idle" }.into();
                    println!("[{}] {name}: {}", stamp(), if paused { "paused" } else { "resumed" });
                    r.save();
                }
                if let Some(m) = &r.mail {
                    let t = m.tick();
                    if !t.deliver.is_empty() {
                        println!("[{}] mail: {} message(s)", stamp(), t.deliver.len());
                        m.set_idle(false);
                        r.turn = Some(Turn { started: Some(Instant::now()), ..Default::default() });
                        r.st.state = "running".into();
                        r.save();
                        let _ = handle.commands.send(AgentCommand::Deliver { mails: t.deliver });
                        continue;
                    }
                }
                let costs_known = r.cost_seen.is_some();
                if !paused && costs_known && r.due.is_some_and(|d| Instant::now() >= d) {
                    r.start_round(&handle.commands);
                }
            }
        }
    }
    let _ = handle.commands.send(AgentCommand::Shutdown);
    let ended = match tokio::time::timeout(Duration::from_secs(10), agent).await {
        Ok(Ok(Err(e))) => Some(format!("{e:#}")),
        Ok(Err(e)) => Some(format!("the agent stopped: {e}")),
        _ => None,
    };
    if let Some(m) = &r.mail {
        m.close();
    }
    let _ = std::fs::remove_file(state::stop_file(&name));
    r.due = None;
    r.st.pid = None;
    if let (Some(why), false) = (ended.or(gone), failed) {
        r.st.state = "failed".into();
        r.st.why = Some(why);
        failed = true;
    } else if !failed {
        r.st.state = "stopped".into();
    }
    r.save();
    println!("[{}] {name}: {}", stamp(), if failed { "failed" } else { "stopped" });
    i32::from(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_s_result_says_whether_it_worked_was_refused_or_hit_the_limit() {
        let t = |end, denied, error: Option<&str>, capped| Turn {
            end,
            denied,
            error: error.map(String::from),
            capped,
            ..Default::default()
        };
        assert_eq!(t(Some(TurnEnd::Done), false, None, false).result(), "ok");
        assert_eq!(t(Some(TurnEnd::Done), true, None, false).result(), "denied");
        assert_eq!(t(Some(TurnEnd::Error), false, Some("HTTP 500"), false).result(), "failed");
        assert_eq!(t(Some(TurnEnd::MaxTurns), false, None, false).result(), "failed");
        assert_eq!(t(Some(TurnEnd::Error), false, Some("spend limit reached"), true).result(), "capped");
        assert_eq!(t(None, false, Some("a hook stopped it"), false).result(), "failed");
    }

    #[test]
    fn a_restarted_lane_waits_for_what_is_left_of_its_clock_unless_it_is_asked_to_run_now() {
        let hour = Some(Duration::from_secs(3600));
        let last = Some(1_000_000);
        let at = |after_ms: i64, every, now| first_wait(last, every, 1_000_000 + after_ms, now);
        assert_eq!(at(10_000, hour, false), Duration::from_millis(3_590_000), "10 s into the hour");
        assert_eq!(at(10_000, hour, true), Duration::ZERO, "--now");
        assert_eq!(at(3_600_000, hour, false), Duration::ZERO, "the hour is over");
        assert_eq!(at(9_000_000, hour, false), Duration::ZERO, "long over");
        assert_eq!(first_wait(None, hour, 5, false), Duration::ZERO, "no round yet");
        assert_eq!(at(10_000, None, false), Duration::ZERO, "no clock: the once-at-start round");
    }

    #[test]
    fn a_routine_comes_from_the_lane_or_from_its_file_as_it_is_now() {
        let d = tempfile::tempdir().unwrap();
        let lane = Lane { routine: Some("Do it.".into()), ..Default::default() };
        assert_eq!(routine(&lane, d.path()).unwrap(), "Do it.");
        let lane = Lane {
            routine_file: Some("LANES.md".into()),
            routine_section: Some("WINDOWS".into()),
            ..Default::default()
        };
        assert!(routine(&lane, d.path()).unwrap_err().contains("LANES.md"));
        std::fs::write(d.path().join("LANES.md"), "WINDOWS:\n```\nCheck the orders.\n```\n").unwrap();
        assert_eq!(routine(&lane, d.path()).unwrap(), "Check the orders.");
        std::fs::write(d.path().join("LANES.md"), "IOS:\n```\nx\n```\n").unwrap();
        assert!(routine(&lane, d.path()).unwrap_err().contains("no code block under the line \"WINDOWS:\""));
    }
}
