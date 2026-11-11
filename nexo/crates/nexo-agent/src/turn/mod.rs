//! The loop: commands in, events out; per request, model calls and tool
//! runs until the model answers without calling a tool.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime};

use futures::StreamExt;
use nexo_core::agent::{
    AddedKind, AddedPart, AgentCommand, AgentEnds, AgentEvent, Attachment, EventTx, GoalLog, GoalState, GoalStop,
    Level, TurnEnd, GOAL_BRIEF_HEAD, GOAL_CONTINUE_MARK, GOAL_SET_MARK, GOAL_STOPS, SUPERVISOR_RESUME_MARK,
};
use nexo_core::config::{Config, OnLimit, ProviderConfig};
use nexo_core::error::{ErrorKind, ProviderError};
use nexo_core::fast::{self, Twin};
use nexo_core::jobs::Jobs;
use nexo_core::leak;
use nexo_core::mail::Mail;
use nexo_core::mailbox::Mailbox;
use nexo_core::message::{ImageSource, Message, Part, Role};
use nexo_core::permission::{PermissionDecision, PermissionGate, PermissionMode, PermissionRequest};
use nexo_core::pricing::Pricing;
use nexo_core::problem::{Problem, ProblemCtx};
use nexo_core::provider::{Effort, ModelRequest, ReasoningConfig};
use nexo_core::stream::{Collector, FinishReason, StreamEvent, Usage};
use nexo_core::tool::{Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolProgress, ToolSet};
use nexo_providers::{catalogue, hints};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::audit::{Audit, RequestRecord, ToolRecord, Why};
use crate::budget;
use crate::checkpoint;
use crate::client::{ModelClient, Target};
use crate::compact::{self, Summariser};
use crate::config_edit;
use crate::context::{self, ContextPolicy, View};
use crate::gate::ChannelGate;
use crate::limits::{self, Limits};
use crate::permission::{self, Decision, Policy};
use crate::prompt;
use crate::reader;
use crate::reads::{attached_header, Reads};
use crate::session::{Session, UsageKind, UsageMeta};
use crate::setup;
use crate::AgentSetup;

mod approval;
mod background;
mod commands;
mod compaction;
mod goal;
mod guards;
mod host;
mod lesson;
mod orient;
mod prefix;
mod round;
mod settings;
mod start;
mod tools;
mod verify;

use self::approval::*;
use self::background::*;
use self::commands::*;
use self::compaction::*;
use self::goal::*;
use self::guards::*;
use self::host::*;
use self::lesson::*;
use self::round::*;
use self::settings::*;
use self::start::*;
use self::tools::*;

/// Lines a `read` without `limit` returns (as the read tool does).
const READ_LIMIT: u64 = 2000;
/// Bytes of a file attached with `@` that are sent whole.
const ATTACHMENT_BYTES: usize = 200_000;
/// Characters of a `!` command's output kept.
const SHELL_CHARS: usize = 30_000;
const SUMMARY_TOKENS: u32 = 4_096;
/// The `compact` policy compacts only when the part it would replace holds, besides an earlier
/// summary, at least this fraction (1/n) of its threshold.
const COMPACT_MARGIN: u64 = 10;
/// Continuation rounds a goal gets after a turn ends without `GOAL DONE` (`NEXO_GOAL_ROUNDS`).
const GOAL_ROUNDS: u32 = 5;
/// What the agent submits by itself to keep working towards the goal.
pub(crate) const GOAL_CONTINUE: &str = GOAL_CONTINUE_MARK;
/// Characters at the end of a reply where `GOAL DONE` or `GOAL WAIT` counts even when it is not the last line.
const GOAL_TAIL: usize = 200;

struct State {
    config: Config,
    cwd: PathBuf,
    tools: ToolSet,
    client: Arc<dyn ModelClient>,
    target: Target,
    session: Session,
    /// What the session sends before its messages, fixed for its life (see `prefix.rs`).
    prefix: crate::session::Prefix,
    history: crate::history::History,
    policy: Box<dyn ContextPolicy>,
    /// The tokens of the history and of the last view sent, for /status.
    last_view: (u64, u64),
    /// The messages of the last request, to tell which part of the next one repeats it (the cache).
    last_sent: Vec<Message>,
    /// The last request the conversation sent, which a prompt suggestion repeats (the provider's cache) with one message more.
    last_request: Option<ModelRequest>,
    /// The mends of a broken history already said (each is said once).
    mended: HashSet<String>,
    perm: Policy,
    /// The OS sandbox of the `bash` tool (`/sandbox` changes its mode).
    sandbox: nexo_core::sandbox::SandboxSettings,
    gate: Arc<dyn PermissionGate>,
    events: EventTx,
    usage: Usage,
    cost: f64,
    system: String,
    /// What is known of the model in use (prices, window, largest reply).
    limits: Limits,
    /// `/fast` asked for the provider's fast request option (it counts only where the provider has one).
    fast: bool,
    /// The models the provider listed when `/fast` last asked, with its name.
    live: Option<(String, Vec<String>)>,
    /// Where the request in progress starts in `history`.
    turn_start: usize,
    turns: u32,
    /// The goal the agent keeps working towards, and the continuation round in progress.
    goal: Option<String>,
    goal_round: u32,
    /// Where the goal stands: only an active goal is continued when a turn ends.
    goal_state: GoalState,
    /// What the goal panel shows: both wordings of the goal, its stops, steps and spend (see `goal_log`).
    goal_log: GoalLog,
    /// The session's cost when the goal was set, to tell what the goal spent.
    goal_cost0: f64,
    /// What files looked like before the agent wrote them: `(history length at the time, file,
    /// its bytes then or None when it did not exist)`, so a rewind can put them back.
    checkpoints: Vec<(usize, PathBuf, Option<Vec<u8>>)>,
    /// The spend limit of this session (from `[agent] max_cost`, changed by `/budget`).
    budget: Option<f64>,
    /// The 80% warning was given.
    budget_warned: bool,
    /// Checkpoints taken since the person last wrote (`agent.on_limit`); a goal's rounds, a mail or a job's end do not reset it.
    ck_used: u32,
    /// Why the last request ended at a checkpoint cap or for lack of progress, for a goal's pause.
    ck_stop: Option<String>,
    /// Messages sent while a turn runs: they join it before its next model call.
    inbox: VecDeque<(String, Vec<Attachment>)>,
    /// What other sessions know, waiting to go with the next request: `(id, title, block)`.
    shared: Vec<(String, String, String)>,
    /// Files the model has seen, so an unchanged one is not sent again.
    reads: Reads,
    /// When the last requests ran (start, end): a file changed inside one was made by the model's own work.
    worked: VecDeque<(SystemTime, SystemTime)>,
    /// The notes of the reader passes so far, by material and request.
    digests: HashMap<u64, String>,
    /// Commands and sub-agents running in the background.
    jobs: Jobs,
    /// The files running sub-agents are changing (the main agent's writes respect them).
    claims: nexo_core::claims::Claims,
    /// Sends the sub-agents a round waits for to the background.
    detach: nexo_core::jobs::Detach,
    /// The files the model has read or written, as they were then.
    seen: nexo_core::seen::Seen,
    /// What the step-level check remembers: commands too slow to repeat, and the last check's report.
    post: Arc<nexo_tools::postcheck::Guard>,
    /// Failures of the session kept by what they were on, to say when one comes back.
    repeats: crate::repeat::Repeats,
    /// Bumped when the conversation is cleared, rewound or switched, so a reply asked for before is let go.
    epoch: u64,
    /// Plan mode: the mode to go back to and the switch an approved plan asks for.
    plan: crate::ask::PlanCell,
    /// The size of the last request as the provider counted it (its prompt and reply tokens) and the
    /// length of the history then: what the `compact` policy measures the next request from.
    last_prompt: Option<(u64, usize)>,
    /// The estimate (4 characters a token) of the prompt of the last request, and the provider's
    /// tokens per estimated token learned so far: code, logs and other scripts hold more tokens than
    /// 4 characters each, so the view is fitted to the window in the provider's own count.
    est_sent: u64,
    tok_ratio: Option<f64>,
    /// The last call the provider billed in this session: a call cut before its usage event is estimated from it.
    last_call: Option<Usage>,
    /// The mailbox other sessions write to, and the mails that reached the model: those waiting for the turn
    /// in progress to end, and those the turn in progress carries (told "read" when the model starts answering).
    mail: Option<Arc<Mailbox>>,
    /// The MCP servers, and the hub count the tool set was last brought up to date with.
    mcp: Option<Arc<nexo_tools::mcp_hub::McpHub>>,
    mcp_gen: u64,
    mail_wait: Vec<Mail>,
    mail_turn: Vec<Mail>,
    /// Text hooks added for the model (SessionStart, UserPromptSubmit): it goes with the next request.
    hook_context: Vec<String>,
    /// Notes the supervisor's watch wants on the NEXT model request only (never saved in the conversation), and
    /// the findings of this request already said, so each is said once.
    hidden: Vec<String>,
    /// How many of `hidden` the interface was told of (they ride on the request, which `/sent` shows).
    hidden_told: usize,
    noticed: HashSet<String>,
    /// A check-in was made for this request of the person (at most one; the person speaking opens it again).
    checked_in: bool,
    /// How many times `agent.max_tokens` a reply may take in this request: doubled (up to 4) when a reply
    /// was cut at max_tokens while still thinking, as the reasoning had no room left for an answer or a call.
    reply_boost: u32,
    /// The audit log writer, when `[audit] enabled`.
    audit: Option<Audit>,
}

type Shared = Arc<Mutex<State>>;

fn emit(events: &EventTx, ev: AgentEvent) {
    let _ = events.send(ev);
}

/// What a view that lost some sub-agent events is sent so it can catch up: the state of every run the
/// registry lists.
fn sub_resync(subs: &nexo_core::subs::Subs) -> Vec<AgentEvent> {
    subs.list().into_iter().map(|info| AgentEvent::SubState { info }).collect()
}

fn notice(events: &EventTx, level: Level, text: impl Into<String>) {
    emit(events, AgentEvent::Notice { level, text: text.into(), problem: None });
}

/// Tells the person about a failure as a card; the plain text of it rides along for the old readers.
fn notice_problem(events: &EventTx, problem: Problem) {
    emit(events, AgentEvent::Notice { level: Level::Error, text: problem.plain(), problem: Some(Box::new(problem)) });
}

/// Tells the person an operation failed: the title says what, `cause` why; each step is something to do next.
fn notice_failure(events: &EventTx, title: impl Into<String>, cause: impl std::fmt::Display, steps: &[&str]) {
    let cause = cause.to_string();
    let mut p = Problem::new(title, cause.lines().next().unwrap_or("").trim().to_string());
    for s in steps {
        p = p.step(*s);
    }
    // a cause that is long or has more lines than the row shows opens in full
    if cause.lines().count() > 1 || cause.chars().count() > 69 {
        p = p.detail(cause);
    }
    notice_problem(events, p);
}

/// Tells the person how an MCP control ended.
fn mcp_result(events: &EventTx, r: Result<String, String>) {
    match r {
        Ok(t) => notice(events, Level::Info, t),
        Err(e) => notice(events, Level::Warn, format!("mcp: {e}")),
    }
}

/// Runs the hooks of `event` with what the session knows, tells the person which failed, and returns
/// what they said. Nothing runs (and the state is not read twice) when no hook listens.
async fn fire(state: &Shared, event: &str, subject: Option<&str>, fields: Value) -> crate::hooks::Outcome {
    let (hooks, cwd, sid, events) = {
        let st = state.lock().await;
        if !crate::hooks::listens(&st.config.hooks, event) {
            return Default::default();
        }
        (st.config.hooks.clone(), st.cwd.clone(), st.session.id().to_string(), st.events.clone())
    };
    fire_with(&hooks, &events, &cwd, &sid, event, subject, &fields).await
}

async fn fire_with(
    hooks: &[nexo_core::config::HookConfig],
    events: &EventTx,
    cwd: &Path,
    sid: &str,
    event: &str,
    subject: Option<&str>,
    fields: &Value,
) -> crate::hooks::Outcome {
    let o = crate::hooks::run(hooks, event, subject, fields, cwd, sid).await;
    for f in &o.failures {
        notice(events, Level::Warn, f.clone());
    }
    o
}

/// Tells the hooks of `Notification` (a permission question, a request that ended in trouble) without
/// holding anything up.
fn notify_hooks(state: &Shared, kind: &'static str, message: String) {
    let state = state.clone();
    tokio::spawn(async move {
        fire(&state, "notification", Some(kind), serde_json::json!({"notification_type": kind, "message": message}))
            .await;
    });
}

/// A Stop hook may ask the agent to go on this many times in a row; then the request ends anyway.
const STOP_HOOK_CAP: u32 = 3;

/// Removes the scratch folders of dead processes, once per process, off the first frame.
fn sweep_scratch_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = std::thread::Builder::new().name("scratch-sweep".into()).spawn(nexo_tools::bash::sweep_tmp);
    });
}

pub async fn run(ends: AgentEnds, setup: AgentSetup) -> anyhow::Result<()> {
    let AgentEnds { mut commands, events } = ends;
    let AgentSetup { mut config, cwd, provider, model, mut tools, client, resume, gate, mail, mcp, model_pinned } =
        setup;
    // whoever built the setup (the interface, a script, ACP), the managed policy is applied once more here
    config.enforce_managed();
    sweep_scratch_once();
    let headless = gate.is_some();
    let chan_gate = ChannelGate::new(events.clone());
    let asker = crate::ask::ChannelAsker::new(events.clone());
    let gate: Arc<dyn PermissionGate> = gate.unwrap_or_else(|| chan_gate.clone());
    let pcfg =
        config.providers.get(&provider).cloned().ok_or_else(|| anyhow::anyhow!("unknown provider `{provider}`"))?;
    let audit = Audit::from_config(&config.audit);
    let limits = limits::resolve(&provider, &pcfg, &model, None);
    let target = Target::new(&provider, pcfg, &model);
    // the sub-agents, when there are tools to give them; they reach the state once it exists
    let host: crate::subagent::HostCell = Arc::new(OnceLock::new());
    if config.agent.subagents && tools.specs().iter().any(|s| s.name == "read" || s.name == "grep") {
        let profiles = crate::subagent::load(&cwd);
        let sub = crate::subagent::SubAgent::new(client.clone(), target.clone(), &tools, profiles, host.clone())
            .with_brief(crate::brief::BriefConfig::from_agent(&config.agent))
            .with_providers(config.providers.clone())
            .with_parallel(config.agent.parallel_tasks)
            .with_hooks(config.hooks.clone())
            .with_worktree_setup(config.agent.worktree_setup.clone());
        tools.add(Arc::new(sub));
    }
    // after the sub-agents: questions are the main agent's alone
    let plan: crate::ask::PlanCell = Arc::default();
    if !headless {
        tools.add(Arc::new(crate::ask::AskUser::new(asker.clone())));
    }
    tools.add(Arc::new(crate::ask::ExitPlan::new((!headless).then(|| asker.clone()), plan.clone())));
    tools.add(Arc::new(crate::goal_brief::GoalBrief));
    let (mut session, history, replay) = match &resume {
        Some(id) => {
            // taken before anything is spent: a session another nexo writes is not resumed here
            let s = Session::open(id)
                .and_then(|mut s| s.claim().map(|_| s))
                .map_err(|e| anyhow::anyhow!("cannot resume session {id}: {e}"))?;
            let h = s.messages().to_vec();
            (s, h, true)
        }
        None => match Session::create(&cwd, &provider, &model) {
            Ok(s) => (s, Vec::new(), false),
            Err(e) => {
                notice(&events, Level::Warn, format!("the session is not being saved: {e}"));
                (Session::ephemeral(&cwd, &provider, &model), Vec::new(), false)
            }
        },
    };
    let prefix = prefix::settle(&mut session, &cwd, &config.agent, &mut tools);
    let system = prompt::system_prompt_with(&cwd, &config.agent, &tools, window_of(&limits), &prefix.volatile);
    let policy = context::from_config(&config.context);
    let perm = Policy::from_config(&config.permissions);
    let config_sandbox =
        (config.permissions.sandbox, config.permissions.sandbox_network, config.permissions.sandbox_write_dirs.clone());
    plan.lock().unwrap_or_else(|e| e.into_inner()).set(perm.mode);
    if let Some(m) = &mail {
        m.set_bypass(perm.mode == PermissionMode::Auto);
        m.rebind(session.id(), &mail_name(session.meta().title.as_str(), &cwd));
        // a session that has just started is between turns: mail may be handed over right away
        m.set_idle(true);
    }
    emit(
        &events,
        AgentEvent::Ready {
            session_id: session.id().to_string(),
            provider: provider.clone(),
            model: model.clone(),
            cwd: cwd.clone(),
            permission_mode: perm.mode,
        },
    );
    for n in &config.notices {
        notice(&events, Level::Warn, n.clone());
    }
    {
        let s = nexo_core::sandbox::SandboxSettings::new(
            config.permissions.sandbox,
            config.permissions.sandbox_network,
            vec![],
        );
        if let Some((line, false)) = tools.get("bash").and_then(|t| t.sandbox_status(&s)).filter(|_| s.mode.is_on()) {
            notice(&events, Level::Warn, format!("sandbox: {line}"));
        }
    }
    if replay {
        replay_history(&events, &session, &tools);
    }
    emit(&events, limits.event(&provider, &model));
    if let Some(c) = config.providers.get(&provider) {
        emit(
            &events,
            AgentEvent::Effort {
                model: model.clone(),
                current: setup::effort_of(c, &model, &config.agent.reasoning),
                levels: c.efforts.get(&model).cloned().unwrap_or_default(),
            },
        );
        if matches!(fast::twin(&model, c.models.iter().map(String::as_str)), Twin::Base(_)) {
            emit(&events, AgentEvent::Fast { on: true });
        }
    }
    let goal = session.goal().map(String::from);
    let goal_state = session.goal_state().clone();
    if goal.is_some() {
        emit(&events, AgentEvent::GoalLog(resumed_log(goal.as_deref())));
        emit(&events, AgentEvent::Goal { text: goal.clone(), round: 0, max: goal_rounds(), state: goal_state.clone() });
    }
    let (usage, cost) = (session.usage(), session.cost());
    let max_cost = config.agent.max_cost;
    let jobs = Jobs::new();
    let detach = nexo_core::jobs::Detach::default();
    let mut job_changes = jobs.changes();
    let mut sub_changes = jobs.subs().changes();
    // the registry lives as long as `jobs`, so its channel does not close; if it ever did, the branch stops
    // (a closed channel answers at once, and the loop would spin)
    let mut subs_open = true;
    let state: Shared = Arc::new(Mutex::new(State {
        config,
        cwd,
        tools,
        client,
        target,
        session,
        prefix,
        history: crate::history::History::new(history),
        policy,
        last_view: (0, 0),
        last_sent: Vec::new(),
        last_request: None,
        mended: HashSet::new(),
        perm,
        sandbox: nexo_core::sandbox::SandboxSettings::new(config_sandbox.0, config_sandbox.1, config_sandbox.2),
        gate,
        events: events.clone(),
        usage,
        cost,
        system,
        limits,
        fast: false,
        live: None,
        turn_start: 0,
        turns: 0,
        goal_log: resumed_log(goal.as_deref()),
        goal_cost0: cost,
        goal,
        goal_round: 0,
        goal_state,
        checkpoints: Vec::new(),
        budget: max_cost,
        budget_warned: false,
        ck_used: 0,
        ck_stop: None,
        inbox: VecDeque::new(),
        shared: Vec::new(),
        reads: Reads::default(),
        worked: VecDeque::new(),
        digests: HashMap::new(),
        jobs: jobs.clone(),
        claims: Default::default(),
        detach: detach.clone(),
        seen: Default::default(),
        post: Default::default(),
        repeats: Default::default(),
        epoch: 0,
        plan,
        last_prompt: None,
        est_sent: 0,
        tok_ratio: None,
        last_call: None,
        mcp_gen: mcp.as_ref().map_or(0, |h| h.generation()),
        mcp: mcp.clone(),
        mail,
        mail_wait: Vec::new(),
        mail_turn: Vec::new(),
        hook_context: Vec::new(),
        hidden: Vec::new(),
        hidden_told: 0,
        noticed: HashSet::new(),
        checked_in: false,
        reply_boost: 1,
        audit,
    }));
    let _ = host.set(Arc::new(StateHost { state: Arc::downgrade(&state), events: events.clone() }));
    if let Some(hub) = &mcp {
        // the interface learns of every change of a server (its lists, a login, a failure)
        let (hub, events) = (hub.clone(), events.clone());
        tokio::spawn(async move {
            let mut rx = hub.watch();
            emit(&events, AgentEvent::Mcp { servers: hub.info() });
            while rx.changed().await.is_ok() {
                emit(&events, AgentEvent::Mcp { servers: hub.info() });
            }
        });
    }
    {
        let mut st = state.lock().await;
        st.system = full_system(&st);
        // with a limit set, the interface shows the spending against it from the start
        if st.budget.is_some() || st.config.agent.max_cost_day.is_some() {
            emit(&st.events, budget_event(&st, budget::spent_today()));
        }
    }
    if replay {
        let mut st = state.lock().await;
        close_open_calls(&mut st);
        if !st.history.is_empty() {
            view_now(&mut st).await;
            // a view shown, not sent: the next request finds no cache of it
            st.last_sent.clear();
        }
    }
    if replay {
        restore_model(&state, model_pinned).await;
    }
    spawn_catalogue_fill(&state);
    spawn_vendor_fill(&state);
    spawn_job_review(&state, jobs.clone());
    {
        let source = if replay { "resume" } else { "startup" };
        let o = fire(&state, "session_start", Some(source), serde_json::json!({"source": source})).await;
        state.lock().await.hook_context.extend(o.context);
    }
    let mut turn: Option<JoinHandle<TurnEnd>> = None;
    // a sign-in waiting for the browser, so Esc in the setup can drop it
    let mut signing_in: Option<tokio::task::AbortHandle> = None;
    let mut cancel = CancellationToken::new();
    // the running turn was stopped to run the queued request now, not to wait for the person
    let mut run_queue = false;
    // what a check-in decided, back to this loop (the only place a turn starts)
    let (resume_tx, mut resume_rx) = tokio::sync::mpsc::unbounded_channel::<Resume>();
    loop {
        tokio::select! {
            cmd = commands.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    AgentCommand::Submit { text, attachments } => {
                        let mut st = state.lock().await;
                        st.goal_round = 0;
                        if turn.is_some() {
                            st.inbox.push_back((text, attachments));
                            emit(&events, AgentEvent::Queued { pending: st.inbox.len() });
                            // the person is heard now: sub-agents the round waits for go to the background
                            if st.config.agent.detach_on_message {
                                detach.fire(nexo_core::jobs::DetachWhy::Message);
                            }
                        } else {
                            person_spoke(&mut st);
                            drop(st);
                            turn = start_prompt(&state, text, attachments, cancel.child_token()).await;
                        }
                    }
                    AgentCommand::Deliver { mails } => {
                        if mails.is_empty() {
                        } else if turn.is_some() {
                            // never in the middle of a turn: it goes when this one ends
                            state.lock().await.mail_wait.extend(mails);
                        } else {
                            turn = Some(deliver_mails(&state, mails, cancel.child_token()).await);
                        }
                    }
                    AgentCommand::SendMail { to, text } => {
                        let mail = state.lock().await.mail.clone();
                        match mail {
                            None => notice(&events, Level::Warn, "mail between sessions is off"),
                            Some(m) => {
                                let evs = tokio::task::spawn_blocking(move || {
                                    let _ = m.send(&to, &text, None, nexo_core::mail::FromKind::Person);
                                    m.drain_events()
                                })
                                .await
                                .unwrap_or_default();
                                for e in evs {
                                    emit(&events, e);
                                }
                            }
                        }
                    }
                    AgentCommand::ListPeers => {
                        let mail = state.lock().await.mail.clone();
                        let peers = match mail {
                            Some(m) => tokio::task::spawn_blocking(move || m.peers()).await.unwrap_or_default(),
                            None => Vec::new(),
                        };
                        emit(&events, AgentEvent::Peers { peers });
                    }
                    AgentCommand::Unqueue { index, text } => {
                        let mut st = state.lock().await;
                        let at = if st.inbox.get(index).is_some_and(|(t, _)| *t == text) {
                            Some(index)
                        } else {
                            st.inbox.iter().position(|(t, _)| *t == text)
                        };
                        if let Some(i) = at {
                            st.inbox.remove(i);
                        }
                        emit(&events, AgentEvent::Queued { pending: st.inbox.len() });
                    }
                    AgentCommand::Detach => {
                        // the key acts on what is attached now; with nothing attached it leaves no signal
                        // behind for a sub-agent that starts later
                        if turn.is_some() && detach.attached() > 0 {
                            detach.fire(nexo_core::jobs::DetachWhy::Key);
                        }
                    }
                    AgentCommand::Interrupt | AgentCommand::InterruptRun => {
                        if turn.is_some() {
                            run_queue = matches!(cmd, AgentCommand::InterruptRun);
                            cancel.cancel();
                            // the questions of the turn; those of sub-agents in the background stay open
                            chan_gate.deny_turn("interrupted", &detach);
                            asker.drop_all();
                        }
                    }
                    AgentCommand::Permission { id, decision } => {
                        if !chan_gate.resolve(&id, decision) {
                            tracing::debug!("no permission ask waits for {id}");
                        }
                    }
                    AgentCommand::SetModel { provider, model } => set_model(&state, provider, model).await,
                    AgentCommand::Answer { id, answer } => {
                        if !asker.resolve(&id, answer) {
                            tracing::debug!("no question waits for {id}");
                        }
                    }
                    AgentCommand::SetPermissionMode(mode) => set_mode(&mut *state.lock().await, mode),
                    AgentCommand::SetSandbox(mode) => set_sandbox(&mut *state.lock().await, mode),
                    AgentCommand::AddDir(dir) => add_dir(&mut *state.lock().await, &dir),
                    AgentCommand::Compact => {
                        if turn.is_some() {
                            notice(&events, Level::Warn, "compaction waits for the turn to end");
                        } else {
                            compact_now(&state, &events, None, &CancellationToken::new()).await;
                        }
                    }
                    AgentCommand::Clear => {
                        let was_running = turn.is_some();
                        if was_running {
                            cancel.cancel();
                            chan_gate.deny_all("cleared");
                            asker.drop_all();
                            if let Some(t) = turn.take() {
                                let _ = t.await;
                                emit(&events, AgentEvent::TurnEnd { reason: TurnEnd::Interrupted });
                            }
                            cancel = CancellationToken::new();
                            run_queue = false;
                            detach.clear();
                            let mut st = state.lock().await;
                            if !st.inbox.is_empty() {
                                st.inbox.clear();
                                emit(&events, AgentEvent::Queued { pending: 0 });
                            }
                        }
                        jobs.kill_all();
                        clear(&state).await;
                        // the killed jobs are not news for the new conversation
                        jobs.take_ended();
                        if was_running {
                            // the cancelled turn never reaches the end-of-turn tail: the agent is idle now
                            if let Some(m) = state.lock().await.mail.clone() {
                                m.set_idle(true);
                            }
                            emit(&events, AgentEvent::Idle { running_jobs: jobs.running() });
                        }
                    }
                    AgentCommand::Shell { command } => {
                        let cwd = state.lock().await.cwd.clone();
                        run_shell(cwd, command, events.clone());
                    }
                    AgentCommand::SetPricing { pricing } => set_pricing(&state, pricing).await,
                    AgentCommand::ShowModel => {
                        let st = state.lock().await;
                        let (p, _, m) = st.target.get();
                        emit(&events, st.limits.event(&p, &m));
                    }
                    AgentCommand::SetGoal { text } => {
                        if let Some(goal) = set_goal(&state, text).await {
                            let request = format!("{GOAL_SET_MARK} {goal}");
                            if turn.is_some() {
                                let text = format!("goal set: {goal} · checked when this turn ends; the model reads it from the next turn");
                                notice(&events, Level::Info, text);
                            } else {
                                turn = Some(start_turn(&state, request, Vec::new(), cancel.child_token()).await);
                            }
                        }
                    }
                    AgentCommand::PauseGoal => pause_goal(&state, turn.is_some()).await,
                    AgentCommand::ResumeGoal => {
                        if resume_goal(&state, turn.is_some()).await {
                            let text = GOAL_CONTINUE.to_string();
                            turn = Some(start_turn(&state, text, Vec::new(), cancel.child_token()).await);
                        }
                    }
                    AgentCommand::ShowPrompt => {
                        let st = state.lock().await;
                        let text = full_system(&st);
                        emit(&events, AgentEvent::SystemPrompt { text });
                    }
                    AgentCommand::Resume { id } => {
                        if turn.is_some() {
                            notice(&events, Level::Warn, "finish or interrupt the turn before resuming another session");
                        } else {
                            resume_session(&state, &id).await;
                        }
                    }
                    AgentCommand::Fork { index } => {
                        if turn.is_some() {
                            notice(&events, Level::Warn, "finish or interrupt the turn before forking");
                        } else {
                            fork_session(&state, index).await;
                        }
                    }
                    AgentCommand::ShowTree => {
                        let st = state.lock().await;
                        let entries =
                            crate::session::tree(&crate::session::list()).into_iter().map(|(d, m)| (d, m.id, m.title)).collect();
                        emit(&st.events, AgentEvent::SessionTree { entries });
                    }
                    AgentCommand::DeleteSession { id } => delete_session(&state, &id).await,
                    AgentCommand::ShareMemory { id, focus } => share_memory(&state, id, focus).await,
                    AgentCommand::ListTrash => {
                        let st = state.lock().await;
                        let sessions = crate::session::trashed().into_iter().map(|m| (m.id, m.title)).collect();
                        emit(&st.events, AgentEvent::Trash { sessions });
                    }
                    AgentCommand::RestoreSession { id } => {
                        let st = state.lock().await;
                        match crate::session::restore(&id) {
                            Ok(p) => {
                                let title = crate::session::Session::open_path(&p)
                                    .map(|s| s.meta().title.clone())
                                    .unwrap_or_default();
                                emit(&st.events, AgentEvent::SessionRestored { id, title });
                            }
                            Err(e) => notice(&st.events, Level::Error, format!("cannot restore: {e}")),
                        }
                    }
                    AgentCommand::Rewind { index } => {
                        if turn.is_some() {
                            notice(&events, Level::Warn, "finish or interrupt the turn before rewinding");
                        } else {
                            rewind(&state, index).await;
                        }
                    }
                    AgentCommand::Export { path, format } => export(&state, path, format).await,
                    AgentCommand::Rename { title } => {
                        let mut st = state.lock().await;
                        let title = title.trim().to_string();
                        if title.is_empty() {
                            notice(&events, Level::Warn, "a name is needed: /rename <name>".to_string());
                        } else if let Err(e) = st.session.set_title(&title) {
                            notice(&events, Level::Error, format!("cannot rename the session: {e}"));
                        } else {
                            emit(&events, AgentEvent::SessionChanged { session_id: st.session.id().to_string(), title });
                            mail_sync(&st);
                        }
                    }
                    AgentCommand::SetBudget { limit } => {
                        let mut st = state.lock().await;
                        st.budget = limit.filter(|l| *l > 0.0);
                        st.budget_warned = false;
                        let day = budget::spent_today();
                        emit(&events, budget_event(&st, day));
                        notice(&events, Level::Info, budget_text(&st, day, true));
                    }
                    AgentCommand::ShowBudget => {
                        let st = state.lock().await;
                        let day = budget::spent_today();
                        emit(&events, budget_event(&st, day));
                        notice(&events, Level::Info, budget_text(&st, day, false));
                    }
                    AgentCommand::ShowStatus => {
                        let st = state.lock().await;
                        notice(&events, Level::Info, status_text(&st));
                    }
                    AgentCommand::ShowHooks => {
                        let st = state.lock().await;
                        notice(&events, Level::Info, crate::hooks::describe(&st.config.hooks));
                    }
                    AgentCommand::ShowSent => {
                        let st = state.lock().await;
                        match &st.last_request {
                            Some(req) => emit(
                                &events,
                                AgentEvent::Sent { model: req.model.clone(), sections: crate::sent::sections(req) },
                            ),
                            None => notice(&events, Level::Info, "nothing was sent yet: /sent shows the last request"),
                        }
                    }
                    AgentCommand::ShowMcp => {
                        let hub = state.lock().await.mcp.clone();
                        match hub {
                            Some(h) => emit(&events, AgentEvent::Mcp { servers: h.info() }),
                            None => {
                                emit(&events, AgentEvent::Mcp { servers: Vec::new() });
                            }
                        }
                    }
                    AgentCommand::McpToggle { server, on, persist } => {
                        let hub = state.lock().await.mcp.clone();
                        let events = events.clone();
                        tokio::spawn(async move {
                            let r = match hub {
                                Some(h) => h.set_enabled(&server, on, persist).await,
                                None => Err("no MCP servers are configured".to_string()),
                            };
                            mcp_result(&events, r);
                        });
                    }
                    AgentCommand::McpReconnect { server } => {
                        let hub = state.lock().await.mcp.clone();
                        let events = events.clone();
                        tokio::spawn(async move {
                            let r = match hub {
                                Some(h) => h.reconnect(&server).await,
                                None => Err("no MCP servers are configured".to_string()),
                            };
                            mcp_result(&events, r);
                        });
                    }
                    AgentCommand::McpLogin { server } => {
                        let hub = state.lock().await.mcp.clone();
                        let events = events.clone();
                        tokio::spawn(async move {
                            let open = {
                                let events = events.clone();
                                move |url: &str| {
                                    if nexo_providers::login::open_url(url) {
                                        notice(&events, Level::Info, format!("Opened your browser to sign in. If nothing opened, go to:\n{url}"));
                                    } else {
                                        notice(&events, Level::Info, format!("Open this address in a browser to sign in:\n{url}"));
                                    }
                                }
                            };
                            let r = match hub {
                                Some(h) => h.login(&server, &open).await,
                                None => Err("no MCP servers are configured".to_string()),
                            };
                            mcp_result(&events, r);
                        });
                    }
                    AgentCommand::McpPrompt { server, prompt, args } => {
                        let hub = state.lock().await.mcp.clone();
                        let made = match hub {
                            Some(h) => h.prompt_request(&server, &prompt, &args).await,
                            None => Err("no MCP servers are configured".to_string()),
                        };
                        match made {
                            Err(e) => {
                                notice(&events, Level::Warn, format!("/mcp__{server}__{prompt}: {e}"));
                                if turn.is_none() {
                                    emit(&events, AgentEvent::Idle { running_jobs: state.lock().await.jobs.running() });
                                }
                            }
                            Ok(text) => {
                                let mut st = state.lock().await;
                                st.goal_round = 0;
                                if turn.is_some() {
                                    st.inbox.push_back((text, Vec::new()));
                                    emit(&events, AgentEvent::Queued { pending: st.inbox.len() });
                                } else {
                                    person_spoke(&mut st);
                                    drop(st);
                                    turn = start_prompt(&state, text, Vec::new(), cancel.child_token()).await;
                                }
                            }
                        }
                    }
                    AgentCommand::ShowTools => {
                        let st = state.lock().await;
                        notice(&events, Level::Info, tools_text(&st.tools));
                    }
                    AgentCommand::ScanLocal => {
                        let events = events.clone();
                        tokio::spawn(async move {
                            let found = nexo_providers::probe::scan_local().await;
                            emit(&events, AgentEvent::LocalServers { found });
                        });
                    }
                    AgentCommand::Probe { name, cfg, key } => {
                        let events = events.clone();
                        tokio::spawn(async move {
                            // a stored subscription login is renewed and applied like in a request
                            let (cfg, stored) = match (&key, cfg.auth.is_some()) {
                                (None, true) => nexo_providers::login::prepare(&cfg).await.unwrap_or((cfg.clone(), None)),
                                _ => (cfg, None),
                            };
                            let key = key.or_else(|| cfg.resolve_key()).or(stored);
                            let result = nexo_providers::probe::server(&cfg, key.as_deref()).await;
                            emit(&events, AgentEvent::Probed { name, result });
                        });
                    }
                    AgentCommand::Login { name, flow } => {
                        if let Some(prev) = signing_in.take() {
                            prev.abort();
                        }
                        let events = events.clone();
                        signing_in = Some(
                            tokio::spawn(async move {
                                let shown = events.clone();
                                let show = move |d: &nexo_providers::login::DeviceCode| {
                                    nexo_providers::login::open_url(&d.url);
                                    emit(&shown, AgentEvent::LoginPrompt { url: d.url.clone(), code: d.code.clone() });
                                };
                                let result = match nexo_providers::login::login(&flow, &show).await {
                                    Ok(cred) => nexo_core::auth::set(&name, cred)
                                        .map_err(|e| format!("the login could not be kept: {e}")),
                                    Err(e) => Err(e.message),
                                };
                                emit(&events, AgentEvent::LoggedIn { name, result });
                            })
                            .abort_handle(),
                        );
                    }
                    AgentCommand::CancelLogin => {
                        if let Some(prev) = signing_in.take() {
                            prev.abort();
                        }
                    }
                    AgentCommand::CheckModel { name, probe, cfg, model, key } => {
                        let events = events.clone();
                        let client = state.lock().await.client.clone();
                        tokio::spawn(async move {
                            let mut cfg = cfg;
                            cfg.protocol = probe.protocol.clone();
                            cfg.base_url = probe.base_url.clone();
                            if let Some(k) = key.as_deref().filter(|k| !k.trim().is_empty()) {
                                if let Err(e) = setup::keep_key(&name, k, &mut cfg) {
                                    notice(&events, Level::Warn, format!("the key could not be kept: {e}"));
                                }
                            }
                            let check = setup::check(client, &name, &probe, &cfg, &model).await;
                            emit(&events, AgentEvent::ModelChecked { name, model, check });
                        });
                    }
                    AgentCommand::SaveModel { name, cfg, model, key } => save_model(&state, name, cfg, model, key).await,
                    AgentCommand::SetEffort { effort } => set_effort(&state, effort).await,
                    AgentCommand::SetFast { on } => set_fast(&state, on).await,
                    AgentCommand::KillJob { id } => {
                        let n = match id {
                            Some(id) => usize::from(jobs.kill(id)),
                            None => jobs.kill_all(),
                        };
                        let what = match id {
                            Some(id) if n == 0 => format!("job {id} is not running"),
                            Some(id) => format!("job {id} stopped"),
                            None => format!("{} stopped", nexo_core::count(n, "job")),
                        };
                        notice(&events, Level::Info, what);
                        emit(&events, AgentEvent::Jobs { jobs: jobs.list() });
                    }
                    AgentCommand::JobTails => emit(&events, AgentEvent::JobTails { tails: jobs.tails() }),
                    // the event for a message that cannot be delivered is made by the registry
                    AgentCommand::SubSubmit { id, text } => {
                        let _ = jobs.subs().submit(&id, &text);
                    }
                    AgentCommand::SubTranscript { id } => {
                        let transcript = jobs.subs().transcript(&id);
                        emit(&events, AgentEvent::SubTranscript { id, transcript });
                    }
                    AgentCommand::ShowJobs => {
                        notice(&events, Level::Info, jobs_text(&jobs));
                        emit(&events, AgentEvent::Jobs { jobs: jobs.list() });
                    }
                    AgentCommand::Suggest => {
                        // only between requests: a running one makes the suggestion stale
                        if turn.is_none() {
                            spawn_suggest(&state);
                        }
                    }
                    AgentCommand::CheckIn { evidence } => {
                        if turn.is_none() {
                            check_in(&state, Some(evidence), &resume_tx).await;
                        }
                    }
                    AgentCommand::Shutdown => break,
                }
            }
            Some(r) = resume_rx.recv() => {
                // the answer comes while the session is still stopped and nothing else has started since
                let current = {
                    let st = state.lock().await;
                    (st.turns, st.epoch) == (r.turns, r.epoch) && st.inbox.is_empty()
                };
                if turn.is_none() && current {
                    state.lock().await.hidden.push(r.text);
                    turn = Some(start_turn(&state, SUPERVISOR_RESUME_MARK.to_string(), Vec::new(), cancel.child_token()).await);
                }
            }
            change = sub_changes.recv(), if subs_open => {
                match change {
                    Ok(c) => emit(&events, c.into()),
                    // a slow interface lost some: it gets every run's state again, and the registry has the rest
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        for e in sub_resync(&jobs.subs()) {
                            emit(&events, e);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => subs_open = false,
                }
            }
            change = job_changes.recv() => {
                if let Err(tokio::sync::broadcast::error::RecvError::Closed) = change {
                    continue;
                }
                emit(&events, AgentEvent::Jobs { jobs: jobs.list() });
                let ended = matches!(&change, Ok(c) if !c.0.status.running());
                // a job ended while the agent waited for the person: it goes on with the result
                if ended && turn.is_none() && !jobs.untold().is_empty() {
                    let note = jobs.note_ended(&jobs.take_ended());
                    turn = Some(start_turn(&state, note, Vec::new(), cancel.child_token()).await);
                }
            }
            res = async { turn.as_mut().expect("a turn is running").await }, if turn.is_some() => {
                turn = None;
                detach.clear();
                let reason = res.unwrap_or_else(|e| {
                    let how = if e.is_panic() { "panicked" } else { "was aborted" };
                    notice(&events, Level::Error, format!("the turn {how}"));
                    TurnEnd::Error
                });
                emit(&events, AgentEvent::TurnEnd { reason });
                let (hooks, hook_cwd, sid) = {
                    let st = state.lock().await;
                    (st.config.hooks.clone(), st.cwd.clone(), st.session.id().to_string())
                };
                if crate::hooks::listens(&hooks, "turn_end") {
                    let events = events.clone();
                    let sid = sid.clone();
                    tokio::spawn(async move {
                        let payload = serde_json::json!({"reason": format!("{reason:?}").to_ascii_lowercase(), "session": sid});
                        fire_with(&hooks, &events, &hook_cwd, &sid, "turn_end", None, &payload).await;
                    });
                }
                if matches!(reason, TurnEnd::Error | TurnEnd::MaxTurns) {
                    notify_hooks(&state, "turn_stopped", format!("the request ended early ({})", format!("{reason:?}").to_ascii_lowercase()));
                }
                cancel = CancellationToken::new();
                let run_queue = std::mem::take(&mut run_queue);
                // the goal takes the turn's end first (met, waiting, paused), then a queued message runs
                goal_settle(&state, reason).await;
                // what is left came after the turn's last model call
                let next = {
                    let mut st = state.lock().await;
                    // after Esc the person wanted to stop: what was queued waits for them (Enter sent it on)
                    let waits = reason == TurnEnd::Interrupted && !run_queue;
                    let next = if waits { None } else { st.inbox.pop_front() };
                    if waits && !st.inbox.is_empty() {
                        notice(&events, Level::Info, format!("{} queued request(s) kept; they join your next message", st.inbox.len()));
                    }
                    if next.is_some() {
                        emit(&events, AgentEvent::Queued { pending: st.inbox.len() });
                        person_spoke(&mut st);
                    }
                    next
                };
                if let Some((text, attachments)) = next {
                    notice(&events, Level::Info, "running the queued request");
                    turn = start_prompt(&state, text, attachments, cancel.child_token()).await;
                } else if !state.lock().await.mail_wait.is_empty() {
                    let mails = std::mem::take(&mut state.lock().await.mail_wait);
                    turn = Some(deliver_mails(&state, mails, cancel.child_token()).await);
                } else if reason == TurnEnd::Done {
                    if let Some(text) = goal_follow_up(&state).await {
                        turn = Some(start_turn(&state, text, Vec::new(), cancel.child_token()).await);
                    }
                }
                // a job that ended after the turn's last model call
                if turn.is_none() && reason == TurnEnd::Done && !jobs.untold().is_empty() {
                    let note = jobs.note_ended(&jobs.take_ended());
                    turn = Some(start_turn(&state, note, Vec::new(), cancel.child_token()).await);
                }
                if turn.is_none() {
                    if let Some(m) = state.lock().await.mail.clone() {
                        m.set_idle(true);
                    }
                    emit(&events, AgentEvent::Idle { running_jobs: jobs.running() });
                }
                if turn.is_none() && reason == TurnEnd::Done {
                    check_in(&state, None, &resume_tx).await;
                }
            }
        }
    }
    cancel.cancel();
    chan_gate.deny_all("shutdown");
    asker.drop_all();
    jobs.kill_all();
    if let Some(t) = turn {
        let _ = t.await;
    }
    fire(&state, "session_end", Some("exit"), serde_json::json!({"reason": "exit"})).await;
    state.lock().await.policy.shutdown().await;
    // the scratch folder (`$NEXO_TMP`) lives as long as the session
    nexo_tools::bash::wipe_tmp(state.lock().await.session.id());
    // a task still holding the state (a hook, a suggestion) must not keep the lock past the exit
    state.lock().await.session.release();
    emit(&events, AgentEvent::Shutdown);
    Ok(())
}
