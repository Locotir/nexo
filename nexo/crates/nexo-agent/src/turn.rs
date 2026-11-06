//! The loop: commands in, events out; per request, model calls and tool
//! runs until the model answers without calling a tool.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use futures::StreamExt;
use nexo_core::agent::{
    AgentCommand, AgentEnds, AgentEvent, Attachment, EventTx, GoalState, Level, TurnEnd, GOAL_BRIEF_HEAD,
    GOAL_CONTINUE_MARK, GOAL_SET_MARK,
};
use nexo_core::config::{Config, OnLimit, ProviderConfig};
use nexo_core::error::{ErrorKind, ProviderError};
use nexo_core::jobs::Jobs;
use nexo_core::leak;
use nexo_core::mail::Mail;
use nexo_core::mailbox::Mailbox;
use nexo_core::message::{ImageSource, Message, Part, Role};
use nexo_core::permission::{PermissionDecision, PermissionGate, PermissionMode, PermissionRequest};
use nexo_core::pricing::Pricing;
use nexo_core::provider::{Effort, ModelRequest, ReasoningConfig};
use nexo_core::stream::{Collector, FinishReason, StreamEvent, Usage};
use nexo_core::tool::{Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolProgress, ToolSet};
use nexo_providers::{catalogue, hints};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

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

fn goal_rounds() -> u32 {
    parse_rounds(std::env::var("NEXO_GOAL_ROUNDS").ok())
}

fn parse_rounds(v: Option<String>) -> u32 {
    v.and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(GOAL_ROUNDS)
}

/// The whole system prompt of a request: the prompt, the policy's directives
/// (stable, so cached) and the goal. A raw prompt file is sent as written.
fn full_system(st: &State) -> String {
    let base = prompt::system_prompt(&st.cwd, &st.config.agent, &st.tools);
    let specs = offered(st);
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    let mut base = prompt::with_directives(base, &st.config.agent, st.policy.directives(&names));
    if st.perm.mode == PermissionMode::Plan {
        let directive = match st.tools.get("ask_user") {
            Some(_) => crate::ask::PLAN_DIRECTIVE,
            None => crate::ask::PLAN_DIRECTIVE_HEADLESS,
        };
        base = format!("{base}\n\n{directive}");
    }
    with_goal(&base, st.goal.as_deref())
}

/// The system prompt with the goal pinned at its end. It stays the same whatever the goal's state,
/// so pausing or resuming does not cost the cached prefix. The person's own words come with the
/// request to restate them (`goal_brief`); once restated, the restatement is the goal.
fn with_goal(system: &str, goal: Option<&str>) -> String {
    let Some(g) = goal else { return system.to_string() };
    let head = if g.starts_with(GOAL_BRIEF_HEAD) {
        format!("Goal, as the person means it:\n{g}")
    } else {
        format!(
            "Goal, in the person's words: {g}\nBefore working on it, call `goal_brief` once to restate it as they \
             mean it: what they want, why, how they expect it done, and when it counts as done."
        )
    };
    format!(
        "{system}\n\n{head}\nKeep working towards this goal across turns. `{GOAL_CONTINUE_MARK}` comes from the loop, \
         not the person: the goal is not marked done yet, so go on. When it is fully met, end your reply with the \
         single line GOAL DONE; if nothing is left to do, say so and end with that line. When you cannot go on \
         without the person (a question only they can answer, a decision, an approval, access you lack), ask it and \
         end your reply with the single line GOAL WAIT: the work waits for their answer. Do not stop for what you \
         can decide or check yourself. A message from the person comes before the goal: answer it first, plainly, \
         then go on; call `goal_brief` again when they change what they want."
    )
}

/// Whether a reply declares the goal met: its last non-empty line is `GOAL DONE`, or the words
/// appear in its last `GOAL_TAIL` characters (models add a closing sentence after the line).
fn goal_done(text: &str) -> bool {
    signal(text, "GOAL DONE")
}

/// Whether a reply stops for the person: `GOAL WAIT`, read as `GOAL DONE` is, or a last line that is
/// a question (models end a reply with one when they wait for an answer, whether told to or not).
fn goal_wait(text: &str) -> bool {
    if signal(text, "GOAL WAIT") {
        return true;
    }
    let last = text.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("");
    let bare = last.trim_end_matches(['*', '_', '`', ')', '"', '\'', '»', ' ']);
    !last.starts_with("```") && (bare.ends_with('?') || bare.ends_with('？'))
}

/// `word` (upper case) as the last non-empty line of a reply, or in its last `GOAL_TAIL` characters.
fn signal(text: &str, word: &str) -> bool {
    let last_line = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.trim_end_matches(['.', '!']).trim().eq_ignore_ascii_case(word))
        .unwrap_or(false);
    let tail: String = text.chars().rev().take(GOAL_TAIL).collect::<Vec<_>>().into_iter().rev().collect();
    last_line || tail.to_ascii_uppercase().contains(word)
}

struct State {
    config: Config,
    cwd: PathBuf,
    tools: ToolSet,
    client: Arc<dyn ModelClient>,
    target: Target,
    session: Session,
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
    gate: Arc<dyn PermissionGate>,
    events: EventTx,
    usage: Usage,
    cost: f64,
    system: String,
    /// What is known of the model in use (prices, window, largest reply).
    limits: Limits,
    /// Where the request in progress starts in `history`.
    turn_start: usize,
    turns: u32,
    /// The goal the agent keeps working towards, and the continuation round in progress.
    goal: Option<String>,
    goal_round: u32,
    /// Where the goal stands: only an active goal is continued when a turn ends.
    goal_state: GoalState,
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
    /// The notes of the reader passes so far, by material and request.
    digests: HashMap<u64, String>,
    /// Commands and sub-agents running in the background.
    jobs: Jobs,
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
    /// The mailbox other sessions write to, and the mails that reached the model: those waiting for the turn
    /// in progress to end, and those the turn in progress carries (told "read" when the model starts answering).
    mail: Option<Arc<Mailbox>>,
    mail_wait: Vec<Mail>,
    mail_turn: Vec<Mail>,
}

type Shared = Arc<Mutex<State>>;

fn emit(events: &EventTx, ev: AgentEvent) {
    let _ = events.send(ev);
}

fn notice(events: &EventTx, level: Level, text: impl Into<String>) {
    emit(events, AgentEvent::Notice { level, text: text.into() });
}

pub async fn run(ends: AgentEnds, setup: AgentSetup) -> anyhow::Result<()> {
    let AgentEnds { mut commands, events } = ends;
    let AgentSetup { config, cwd, provider, model, mut tools, client, resume, gate, mail } = setup;
    let headless = gate.is_some();
    let chan_gate = ChannelGate::new(events.clone());
    let asker = crate::ask::ChannelAsker::new(events.clone());
    let gate: Arc<dyn PermissionGate> = gate.unwrap_or_else(|| chan_gate.clone());
    let pcfg =
        config.providers.get(&provider).cloned().ok_or_else(|| anyhow::anyhow!("unknown provider `{provider}`"))?;
    let limits = limits::resolve(&provider, &pcfg, &model, None);
    let target = Target::new(&provider, pcfg, &model);
    // the sub-agents, when there are tools to give them; they reach the state once it exists
    let host: crate::subagent::HostCell = Arc::new(OnceLock::new());
    if config.agent.subagents && tools.specs().iter().any(|s| s.name == "read" || s.name == "grep") {
        let profiles = crate::subagent::load(&cwd);
        let sub = crate::subagent::SubAgent::new(client.clone(), target.clone(), &tools, profiles, host.clone())
            .with_brief(crate::brief::BriefConfig::from_agent(&config.agent))
            .with_providers(config.providers.clone())
            .with_parallel(config.agent.parallel_tasks);
        tools.add(Arc::new(sub));
    }
    // after the sub-agents: questions are the main agent's alone
    let plan: crate::ask::PlanCell = Arc::default();
    if !headless {
        tools.add(Arc::new(crate::ask::AskUser::new(asker.clone())));
    }
    tools.add(Arc::new(crate::ask::ExitPlan::new((!headless).then(|| asker.clone()), plan.clone())));
    tools.add(Arc::new(crate::goal_brief::GoalBrief));
    let (session, history, replay) = match &resume {
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
    let system = prompt::system_prompt(&cwd, &config.agent, &tools);
    let policy = context::from_config(&config.context);
    let perm = Policy::from_config(&config.permissions);
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
    }
    let goal = session.goal().map(String::from);
    let goal_state = session.goal_state().clone();
    if goal.is_some() {
        emit(&events, AgentEvent::Goal { text: goal.clone(), round: 0, max: goal_rounds(), state: goal_state.clone() });
    }
    let (usage, cost) = (session.usage(), session.cost());
    let max_cost = config.agent.max_cost;
    let jobs = Jobs::new();
    let mut job_changes = jobs.changes();
    let state: Shared = Arc::new(Mutex::new(State {
        config,
        cwd,
        tools,
        client,
        target,
        session,
        history: crate::history::History::new(history),
        policy,
        last_view: (0, 0),
        last_sent: Vec::new(),
        last_request: None,
        mended: HashSet::new(),
        perm,
        gate,
        events: events.clone(),
        usage,
        cost,
        system,
        limits,
        turn_start: 0,
        turns: 0,
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
        digests: HashMap::new(),
        jobs: jobs.clone(),
        repeats: Default::default(),
        epoch: 0,
        plan,
        last_prompt: None,
        est_sent: 0,
        tok_ratio: None,
        mail,
        mail_wait: Vec::new(),
        mail_turn: Vec::new(),
    }));
    let _ = host.set(Arc::new(StateHost { state: Arc::downgrade(&state), events: events.clone() }));
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
    catalogue::refresh_if_stale(None);
    spawn_vendor_fill(&state);
    spawn_job_review(&state, jobs.clone());
    let mut turn: Option<JoinHandle<TurnEnd>> = None;
    // a sign-in waiting for the browser, so Esc in the setup can drop it
    let mut signing_in: Option<tokio::task::AbortHandle> = None;
    let mut cancel = CancellationToken::new();
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
                        } else {
                            person_spoke(&mut st);
                            drop(st);
                            turn = Some(start_turn(&state, text, attachments, cancel.child_token()).await);
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
                    AgentCommand::Interrupt => {
                        if turn.is_some() {
                            cancel.cancel();
                            chan_gate.deny_all("interrupted");
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
                                let text = format!("goal set: {goal} · applies from now; checked when this turn ends");
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
                    AgentCommand::Export { path } => export(&state, path).await,
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
                    AgentCommand::Shutdown => break,
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
                    let note = nexo_core::jobs::ended_note(&jobs.take_ended());
                    turn = Some(start_turn(&state, note, Vec::new(), cancel.child_token()).await);
                }
            }
            res = async { turn.as_mut().expect("a turn is running").await }, if turn.is_some() => {
                turn = None;
                let reason = res.unwrap_or(TurnEnd::Error);
                emit(&events, AgentEvent::TurnEnd { reason });
                let (hooks, hook_cwd, sid) = {
                    let st = state.lock().await;
                    (st.config.hooks.clone(), st.cwd.clone(), st.session.id().to_string())
                };
                if hooks.iter().any(|h| h.event == "turn_end") {
                    let events = events.clone();
                    tokio::spawn(async move {
                        let payload = serde_json::json!({"event": "turn_end", "reason": format!("{reason:?}").to_ascii_lowercase(), "session": sid});
                        for f in crate::hooks::run(&hooks, "turn_end", None, &payload, &hook_cwd).await.failures {
                            notice(&events, Level::Warn, f);
                        }
                    });
                }
                cancel = CancellationToken::new();
                // the goal takes the turn's end first (met, waiting, paused), then a queued message runs
                goal_settle(&state, reason).await;
                // what is left came after the turn's last model call
                let next = {
                    let mut st = state.lock().await;
                    let next = st.inbox.pop_front();
                    if next.is_some() {
                        emit(&events, AgentEvent::Queued { pending: st.inbox.len() });
                        person_spoke(&mut st);
                    }
                    next
                };
                if let Some((text, attachments)) = next {
                    notice(&events, Level::Info, "running the queued request");
                    turn = Some(start_turn(&state, text, attachments, cancel.child_token()).await);
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
                    let note = nexo_core::jobs::ended_note(&jobs.take_ended());
                    turn = Some(start_turn(&state, note, Vec::new(), cancel.child_token()).await);
                }
                if turn.is_none() {
                    if let Some(m) = state.lock().await.mail.clone() {
                        m.set_idle(true);
                    }
                    emit(&events, AgentEvent::Idle { running_jobs: jobs.running() });
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
    state.lock().await.policy.shutdown().await;
    emit(&events, AgentEvent::Shutdown);
    Ok(())
}

/// The recorded conversation, as the interface would have seen it live:
/// every message, the calls with their outputs, and the session's usage.
fn replay_history(events: &EventTx, session: &Session, tools: &ToolSet) {
    for (index, m) in session.messages().iter().enumerate() {
        emit(events, AgentEvent::History { index, message: m.clone() });
        if m.role == Role::Assistant {
            for (id, name, args) in m.tool_calls() {
                emit(
                    events,
                    AgentEvent::ToolCall {
                        id: id.into(),
                        name: name.into(),
                        args: args.clone(),
                        summary: summary_of(tools, name, args),
                    },
                );
                if let Some(out) = session.outputs().get(id) {
                    emit(events, AgentEvent::ToolDone { id: id.into(), output: out.clone(), elapsed_ms: 0 });
                }
            }
        }
    }
    emit(
        events,
        AgentEvent::Usage {
            turn: Usage::default(),
            call: Usage::default(),
            session: session.usage(),
            cost_usd: session.cost(),
        },
    );
}

/// Makes `session` the conversation: the interface starts over and gets the replay.
fn install_session(st: &mut State, session: Session) {
    // the jobs of the session left behind stop, and none of them is reported to this one
    st.jobs.kill_all();
    st.jobs.take_ended();
    st.session = session;
    st.checkpoints.clear();
    st.history.replace(st.session.messages().to_vec());
    st.usage = st.session.usage();
    st.cost = st.session.cost();
    st.policy.reset();
    st.last_prompt = None;
    st.last_request = None;
    st.repeats = Default::default();
    st.epoch += 1;
    st.turn_start = 0;
    st.turns = 0;
    st.goal = st.session.goal().map(String::from);
    st.goal_state = st.session.goal_state().clone();
    st.goal_round = 0;
    emit(&st.events, AgentEvent::HistoryCleared);
    emit(
        &st.events,
        AgentEvent::SessionChanged { session_id: st.session.id().to_string(), title: st.session.meta().title.clone() },
    );
    mail_sync(st);
    emit_goal(st);
    replay_history(&st.events, &st.session, &st.tools);
    close_open_calls(st);
}

/// A session that stopped between its last calls and their results (closed, killed, cut off): the
/// calls are answered as not run, and the answer is saved, so the history goes on as every protocol
/// takes it instead of failing each request after.
fn close_open_calls(st: &mut State) {
    let calls: Vec<(String, String, Value)> = match st.history.last() {
        Some(m) if m.role == Role::Assistant => {
            m.tool_calls().map(|(i, n, a)| (i.to_string(), n.to_string(), a.clone())).collect()
        }
        _ => return,
    };
    if calls.is_empty() {
        return;
    }
    let msg = Message::tool_results(cancelled_results(&calls, "the session ended before the tools ran"));
    st.history.push(msg.clone());
    if let Err(e) = st.session.append(&msg) {
        notice(&st.events, Level::Warn, format!("cannot save the session: {e}"));
    }
    let n = calls.len();
    notice(
        &st.events,
        Level::Info,
        format!("{n} tool call(s) left open when the session ended are answered as not run"),
    );
}

/// Pins (or clears) the goal; gives back the goal set, for the caller to start the work.
async fn set_goal(state: &Shared, text: Option<String>) -> Option<String> {
    let mut st = state.lock().await;
    let text = text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    st.goal = text.clone();
    st.goal_round = 0;
    st.goal_state = GoalState::Active;
    if let Err(e) = st.session.record_goal(text.as_deref()) {
        notice(&st.events, Level::Warn, format!("cannot save the goal: {e}"));
    }
    match &text {
        // a goal set shows where its request joins the conversation
        Some(_) => {}
        None => notice(&st.events, Level::Info, "goal cleared"),
    }
    emit_goal(&st);
    text
}

/// Answers `goal_brief`: the agent's restatement takes the place of the goal from the next request on.
async fn goal_brief(state: &Shared, args: &Value) -> ToolOutput {
    let brief = match crate::goal_brief::brief(args) {
        Ok(b) => b,
        Err(key) => return ToolOutput::error(format!("`{key}` is required")),
    };
    let mut st = state.lock().await;
    if st.goal.is_none() {
        return ToolOutput::error("no goal is set: carry on with the request");
    }
    st.goal = Some(brief.clone());
    if let Err(e) = st.session.record_goal(Some(&brief)) {
        notice(&st.events, Level::Warn, format!("cannot save the goal: {e}"));
    }
    st.system = full_system(&st);
    emit_goal(&st);
    ToolOutput::ok("The goal now reads as restated. Go on with the work.")
}

/// Tells the interface the goal, its round and its state.
fn emit_goal(st: &State) {
    let event = AgentEvent::Goal {
        text: st.goal.clone(),
        round: st.goal_round,
        max: goal_rounds(),
        state: st.goal_state.clone(),
    };
    emit(&st.events, event);
}

/// Moves the goal to `to`, saves it with the session (a resumed session finds it as it was) and
/// says why. The rounds start over: a goal that runs again gets all of them.
fn goal_to(st: &mut State, to: GoalState, say: String) {
    st.goal_state = to.clone();
    st.goal_round = 0;
    if let Err(e) = st.session.record_goal_state(to) {
        notice(&st.events, Level::Warn, format!("cannot save the goal: {e}"));
    }
    notice(&st.events, Level::Info, say);
    emit_goal(st);
}

/// A message of the person starts a turn: a goal waiting for their answer has it, and runs again.
fn person_spoke(st: &mut State) {
    // only a person's message gives a request its checkpoints back: a goal's rounds, a mail or a job's end do not
    st.ck_used = 0;
    if st.goal.is_some() && st.goal_state == GoalState::Waiting {
        goal_to(st, GoalState::Active, "goal: resumed with your answer".into());
    }
}

/// What a turn's end does to an active goal, before anything else runs: met, waiting for the
/// person, or paused because the turn did not end on its own (Esc, an error or spend limit, the
/// tool-round limit). A goal that is not active stays as it is.
async fn goal_settle(state: &Shared, reason: TurnEnd) {
    let mut st = state.lock().await;
    if st.goal.is_none() || !st.goal_state.is_active() {
        return;
    }
    let paused = |why: &str| GoalState::Paused(why.to_string());
    let (to, say) = match reason {
        TurnEnd::Interrupted => (paused("interrupted"), "goal paused: interrupted · /goal resume goes on".to_string()),
        TurnEnd::Error => {
            (paused("the turn failed"), "goal paused: the turn failed · /goal resume tries again".to_string())
        }
        TurnEnd::MaxTurns => match st.ck_stop.take() {
            Some(why) => (paused(&why), format!("goal paused: {why} · /goal resume goes on")),
            None => (
                paused("the tool-round limit (agent.max_turns)"),
                "goal paused: the turn reached the tool-round limit (agent.max_turns) · /goal resume goes on"
                    .to_string(),
            ),
        },
        TurnEnd::Done => {
            let from = st.turn_start.min(st.history.len());
            let last = st.history[from..]
                .iter()
                .rev()
                .find(|m| m.role == Role::Assistant)
                .map(Message::text)
                .unwrap_or_default();
            if goal_done(&last) {
                (GoalState::Done, "goal met · /goal resume reopens it, /goal off clears it".to_string())
            } else if goal_wait(&last) {
                (GoalState::Waiting, "goal waiting for your answer: your next message resumes it".to_string())
            } else {
                return;
            }
        }
    };
    goal_to(&mut st, to, say);
}

/// After a turn that ended on its own, with nothing queued: what to submit to keep working towards
/// an active goal, while rounds are left. A continuation round that called no tool did nothing
/// new, so the goal pauses there instead of asking again; the first continuation always runs.
async fn goal_follow_up(state: &Shared) -> Option<String> {
    let mut st = state.lock().await;
    st.goal.as_ref()?;
    if !st.goal_state.is_active() {
        return None;
    }
    let max = goal_rounds();
    let worked = st.history[st.turn_start.min(st.history.len())..].iter().any(Message::has_tool_calls);
    if st.goal_round > 0 && !worked {
        let say = "goal paused: the last round did nothing new · /goal resume goes on, or say what next";
        goal_to(&mut st, GoalState::Paused("the last round did nothing new".into()), say.into());
        return None;
    }
    if st.goal_round >= max {
        let why = format!("{max} rounds without GOAL DONE");
        let say = format!("goal paused: {why} · /goal resume gives it {max} more");
        goal_to(&mut st, GoalState::Paused(why), say);
        return None;
    }
    st.goal_round += 1;
    let round = st.goal_round;
    notice(&st.events, Level::Info, format!("goal: continuing (round {round}/{max})"));
    emit_goal(&st);
    Some(GOAL_CONTINUE.to_string())
}

/// `/goal pause`: nothing more starts for the goal; a turn in progress finishes as it is.
async fn pause_goal(state: &Shared, running: bool) {
    let mut st = state.lock().await;
    if st.goal.is_none() {
        notice(&st.events, Level::Info, "no goal to pause");
        return;
    }
    if matches!(st.goal_state, GoalState::Paused(_)) {
        notice(&st.events, Level::Info, "the goal is already paused · /goal resume goes on");
        return;
    }
    let say = if running {
        "goal paused: the turn in progress finishes, nothing more starts · /goal resume goes on"
    } else {
        "goal paused · /goal resume goes on"
    };
    goal_to(&mut st, GoalState::Paused("paused by you".into()), say.into());
}

/// `/goal resume`: the goal is active again with all its rounds. True when nothing runs, so the
/// caller starts a continuation now; a turn in progress is followed by one when it ends.
async fn resume_goal(state: &Shared, running: bool) -> bool {
    let mut st = state.lock().await;
    if st.goal.is_none() {
        notice(&st.events, Level::Info, "no goal set: /goal <text> sets one");
        return false;
    }
    goal_to(&mut st, GoalState::Active, "goal resumed".into());
    !running && st.inbox.is_empty()
}

async fn resume_session(state: &Shared, id: &str) {
    let mut st = state.lock().await;
    if id == st.session.id() && !st.session.is_lost() {
        // a second copy would drop the lock this one holds when it replaced it (a lost session holds none)
        notice(&st.events, Level::Info, format!("session {id} is the one in use"));
        return;
    }
    // taken before anything is spent: a session another nexo writes is not resumed here
    match Session::open(id).and_then(|mut s| s.claim().map(|_| s)) {
        Ok(s) => {
            install_session(&mut st, s);
            if !st.history.is_empty() {
                view_now(&mut st).await;
                // a view shown, not sent: the next request finds no cache of it
                st.last_sent.clear();
            }
        }
        Err(e) => notice(&st.events, Level::Error, format!("cannot resume session {id}: {e}")),
    }
}

/// `/fork`: a new session from this one up to before message `index` (all of it when None), and
/// the conversation moves to it. The session left behind stays as it was.
async fn fork_session(state: &Shared, index: Option<usize>) {
    let mut st = state.lock().await;
    let keep = index.map(|i| cut_point(&st.history, i)).unwrap_or(st.history.len());
    let from = st.session.id().to_string();
    match st.session.fork(keep) {
        Ok(s) => {
            install_session(&mut st, s);
            if !st.history.is_empty() {
                view_now(&mut st).await;
                // a view shown, not sent: the next request finds no cache of it
                st.last_sent.clear();
            }
            let id = st.session.id().to_string();
            notice(&st.events, Level::Info, format!("forked {from} into {id} (/tree to jump back)"));
        }
        Err(e) => notice(&st.events, Level::Error, format!("cannot fork the session: {e}")),
    }
}

/// Readies what session `id` knows (bearing on `focus`, or on its goal or title) to go with the next request.
async fn share_memory(state: &Shared, id: Option<String>, focus: Option<String>) {
    let mut st = state.lock().await;
    let Some(id) = id else {
        st.shared.clear();
        emit(&st.events, AgentEvent::MemoryShared { pending: Vec::new() });
        return;
    };
    if st.session.id() == id {
        notice(&st.events, Level::Warn, "that is this session: its memory is already here");
        return;
    }
    let other = match crate::session::Session::open(&id) {
        Ok(s) => s,
        Err(e) => return notice(&st.events, Level::Error, format!("cannot read session {id}: {e}")),
    };
    let title = other.meta().title.clone();
    let focus = focus.or_else(|| other.goal().map(str::to_string)).unwrap_or_default();
    let cwd = other.meta().cwd.clone();
    match crate::brief::recall(&id, &title, &focus, other.messages(), &cwd, SHARE_TOKENS) {
        Some((block, _)) => {
            st.shared.retain(|(i, _, _)| *i != id);
            st.shared.push((id, title, block));
            let pending = st.shared.iter().map(|(i, t, b)| (i.clone(), t.clone(), crate::brief::tokens(b))).collect();
            emit(&st.events, AgentEvent::MemoryShared { pending });
        }
        None => notice(&st.events, Level::Info, format!("session {id} holds nothing to share")),
    }
}

/// How much of another session one share brings, at most (tokens).
const SHARE_TOKENS: usize = 4000;

/// Moves a recorded session to the trash; the one in use stays (it is still being written).
async fn delete_session(state: &Shared, id: &str) {
    let st = state.lock().await;
    if st.session.id() == id {
        notice(&st.events, Level::Warn, "this is the session in use: /clear first, then delete it");
        return;
    }
    let title = crate::session::Session::open(id).map(|s| s.meta().title.clone()).unwrap_or_default();
    match crate::session::trash(id) {
        Ok(_) => emit(&st.events, AgentEvent::SessionDeleted { id: id.to_string(), title }),
        Err(e) => notice(&st.events, Level::Error, format!("cannot delete: {e}")),
    }
}

/// Where a rewind to `index` really cuts: never between an assistant's
/// calls and their results, so the kept history stays well-formed.
fn cut_point(history: &[Message], index: usize) -> usize {
    let mut i = index.min(history.len());
    while i > 0
        && history[i - 1].role == Role::Assistant
        && history[i - 1].has_tool_calls()
        && history.get(i).map(Message::is_tool_results).unwrap_or(true)
    {
        i -= 1;
    }
    i
}

async fn rewind(state: &Shared, index: usize) {
    let mut st = state.lock().await;
    let keep = cut_point(&st.history, index);
    if let Err(e) = st.session.rewind(keep) {
        notice(&st.events, Level::Error, format!("cannot rewind the session: {e}"));
        return;
    }
    let dropped = st.history.len().saturating_sub(keep);
    st.history.truncate(keep);
    let restored = restore_files(&mut st, keep);
    st.policy.reset();
    st.last_prompt = None;
    st.last_request = None;
    st.repeats = Default::default();
    st.epoch += 1;
    st.turn_start = 0;
    emit(&st.events, AgentEvent::HistoryCleared);
    emit(
        &st.events,
        AgentEvent::SessionChanged { session_id: st.session.id().to_string(), title: st.session.meta().title.clone() },
    );
    mail_sync(&st);
    replay_history(&st.events, &st.session, &st.tools);
    if !st.history.is_empty() {
        view_now(&mut st).await;
        // a view shown, not sent: the next request finds no cache of it
        st.last_sent.clear();
    }
    let files = match restored.len() {
        0 => String::new(),
        n => {
            let names: Vec<String> =
                restored.iter().take(4).map(|p| p.strip_prefix(&st.cwd).unwrap_or(p).display().to_string()).collect();
            let more = if n > 4 { format!(" and {} more", n - 4) } else { String::new() };
            format!("; {n} file{} put back as they were: {}{more}", if n == 1 { "" } else { "s" }, names.join(", "))
        }
    };
    notice(
        &st.events,
        Level::Info,
        format!("rewound: {} dropped, {keep} kept{files}", nexo_core::count(dropped, "message")),
    );
}

/// Puts back every file the agent wrote after `keep` messages as it was before the first
/// such write (a file that did not exist is removed); the checkpoints after `keep` go.
/// Changes made by shell commands are not known and stay.
fn restore_files(st: &mut State, keep: usize) -> Vec<PathBuf> {
    let mut seen: Vec<PathBuf> = Vec::new();
    for (at, path, before) in &st.checkpoints {
        if *at <= keep || seen.contains(path) {
            continue;
        }
        let done = match before {
            Some(bytes) => std::fs::write(path, bytes),
            None => std::fs::remove_file(path).or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(e)
                }
            }),
        };
        match done {
            Ok(()) => seen.push(path.clone()),
            Err(e) => notice(&st.events, Level::Warn, format!("could not put back {}: {e}", path.display())),
        }
    }
    st.checkpoints.retain(|(at, _, _)| *at <= keep);
    seen
}

async fn export(state: &Shared, path: Option<PathBuf>) {
    let st = state.lock().await;
    let path = match path {
        Some(p) if p.is_absolute() => p,
        Some(p) => st.cwd.join(p),
        None => st.session.export_path(),
    };
    let text = st.session.export_markdown(|name, args| summary_of(&st.tools, name, args));
    let written =
        path.parent().map(std::fs::create_dir_all).unwrap_or(Ok(())).and_then(|_| std::fs::write(&path, text));
    match written {
        Ok(()) => notice(&st.events, Level::Info, format!("exported to {}", path.display())),
        Err(e) => notice(&st.events, Level::Error, format!("cannot export to {}: {e}", path.display())),
    }
}

fn summary_of(tools: &ToolSet, name: &str, args: &Value) -> String {
    match tools.get(name) {
        Some(t) => t.summary(args),
        None => {
            let a = args.to_string();
            let a: String = a.chars().take(80).collect();
            format!("{name} {a}")
        }
    }
}

/// The name a session goes by in the mailbox: its title, else the folder it works in.
fn mail_name(title: &str, cwd: &Path) -> String {
    let t = title.trim();
    if !t.is_empty() {
        return t.chars().take(40).collect();
    }
    cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "nexo".into())
}

/// The mailbox follows the session when it changes (resumed, forked, cleared, renamed).
fn mail_sync(st: &State) {
    if let Some(m) = &st.mail {
        m.rebind(st.session.id(), &mail_name(&st.session.meta().title, &st.cwd));
    }
}

/// Items done out of the total in a `todo` call's `items`; None for a call that only shows the list.
fn todo_counts(args: &Value) -> Option<(u16, u16)> {
    let items = args.get("items")?.as_array()?;
    let done = items
        .iter()
        .filter(|i| matches!(i.get("status").and_then(Value::as_str), Some("done") | Some("completed")))
        .count();
    Some((done.min(u16::MAX as usize) as u16, items.len().min(u16::MAX as usize) as u16))
}

/// Hands mails to the model as one message that is not the person's: the host has them as delivered (receipts go
/// out, `MailDelivered`), and the first answer of the turn marks them read.
async fn deliver_mails(state: &Shared, mails: Vec<Mail>, cancel: CancellationToken) -> JoinHandle<TurnEnd> {
    let text = nexo_core::mail::render(&mails);
    {
        let mut st = state.lock().await;
        if let Some(m) = st.mail.clone() {
            emit(&st.events, m.delivered(&mails));
        }
        st.mail_turn = mails;
    }
    start_turn(state, text, Vec::new(), cancel).await
}

async fn start_turn(
    state: &Shared,
    text: String,
    attachments: Vec<Attachment>,
    cancel: CancellationToken,
) -> JoinHandle<TurnEnd> {
    {
        let mut st = state.lock().await;
        // the prompt file may have changed since the last request
        // a turn is under way: mail waits for the next idle point
        if let Some(m) = &st.mail {
            m.set_idle(false);
        }
        st.system = full_system(&st);
        st.turn_start = st.history.len();
        st.turns += 1;
        st.ck_stop = None;
        let ended = st.jobs.take_ended();
        let text = if ended.is_empty() || text.starts_with("[background jobs ended]") {
            text
        } else {
            format!("{}\n\n{text}", nexo_core::jobs::ended_note(&ended))
        };
        // what other sessions shared goes first, once: it is part of this request from now on
        let text = if st.shared.is_empty() {
            text
        } else {
            let blocks: Vec<String> = st.shared.drain(..).map(|(_, _, b)| b).collect();
            emit(&st.events, AgentEvent::MemoryShared { pending: Vec::new() });
            format!("{}\n\n{text}", blocks.join("\n\n"))
        };
        push_user(&mut st, text, &attachments);
    }
    let state = state.clone();
    tokio::spawn(async move { run_turn(state, cancel).await })
}

/// Adds a request of the person to the conversation, its attachments expanded.
fn push_user(st: &mut State, text: String, attachments: &[Attachment]) {
    let (text, images, whole) = expand_attachments(text, attachments, &st.cwd);
    for (path, bytes, header) in whole {
        st.reads.attached(path, &bytes, header);
    }
    let m = if images.is_empty() {
        Message::user(text)
    } else {
        let mut parts = vec![Part::text(text)];
        parts.extend(images);
        Message::new(Role::User, parts)
    };
    st.history.push(m.clone());
    if let Err(e) = st.session.append(&m) {
        notice(&st.events, Level::Warn, format!("cannot save the session: {e}"));
    }
    let index = st.history.len() - 1;
    emit(&st.events, AgentEvent::History { index, message: m });
}

/// Bills the usage of an attempt the turn goes on from without keeping its reply.
async fn bill_attempt(state: &Shared, events: &EventTx, model: &str, usage: Usage, call: Usage) {
    if !usage.is_empty() {
        add_usage(&mut *state.lock().await, events, model, usage, call, UsageKind::Main);
    }
}

/// Messages the person sent while the turn ran join it now, right after the tool
/// results, so the next model call already sees them. Tells whether a message of the person joined.
async fn take_inbox(state: &Shared) -> bool {
    let mut st = state.lock().await;
    // jobs that ended while the tools ran: the model hears of them now
    let ended = st.jobs.take_ended();
    if !ended.is_empty() {
        push_user(&mut st, nexo_core::jobs::ended_note(&ended), &[]);
    }
    if st.inbox.is_empty() {
        return false;
    }
    while let Some((text, attachments)) = st.inbox.pop_front() {
        push_user(&mut st, text, &attachments);
    }
    person_spoke(&mut st);
    // the message shows in the transcript where it joined: nothing else to say
    emit(&st.events, AgentEvent::Queued { pending: 0 });
    true
}

/// The media type of an image file nexo attaches as an image, by its extension.
fn image_type(p: &Path) -> Option<&'static str> {
    match p.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// The largest image attached (the vendors take about 5 MB).
const IMAGE_BYTES: usize = 5_000_000;

/// A text file attached whole: its path, its bytes and the line that heads it.
type Whole = (PathBuf, Vec<u8>, String);

/// The text of a request with its `@file` attachments as fenced blocks, the images
/// among them as image parts, and the text files attached whole.
fn expand_attachments(text: String, attachments: &[Attachment], cwd: &Path) -> (String, Vec<Part>, Vec<Whole>) {
    use base64::Engine;
    let mut out = text;
    let mut images = Vec::new();
    let mut whole = Vec::new();
    for a in attachments {
        let p = if a.path.is_absolute() { a.path.clone() } else { cwd.join(&a.path) };
        let shown = a.path.display();
        if let Some(media_type) = image_type(&p) {
            match std::fs::read(&p) {
                Ok(bytes) if bytes.len() > IMAGE_BYTES => out.push_str(&format!(
                    "\n\n@{shown}: not attached, the image is {} bytes (5 MB at most)",
                    bytes.len()
                )),
                Ok(bytes) => {
                    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    images.push(Part::Image { source: ImageSource::Base64 { media_type: media_type.into(), data } });
                    out.push_str(&format!("\n\n@{shown}: attached as an image"));
                }
                Err(e) => out.push_str(&format!("\n\n@{shown}: not attached ({e})")),
            }
            continue;
        }
        match std::fs::read(&p) {
            Ok(bytes) if bytes.len() > ATTACHMENT_BYTES => {
                out.push_str(&format!("\n\n@{shown}: not attached, it is {} bytes (read it in parts)", bytes.len()))
            }
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(s) => {
                    let fence = if s.contains("```") { "````" } else { "```" };
                    let ext = p.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
                    let header = attached_header(&shown.to_string());
                    out.push_str(&format!("\n\n{header}\n{fence}{ext}\n{}\n{fence}", s.trim_end_matches('\n')));
                    whole.push((p.clone(), s.into_bytes(), header));
                }
                Err(_) => out.push_str(&format!("\n\n@{shown}: not attached, it is not text")),
            },
            Err(e) => out.push_str(&format!("\n\n@{shown}: not attached ({e})")),
        }
    }
    (out, images, whole)
}

fn reasoning_of(s: &Option<String>) -> Option<ReasoningConfig> {
    let s = s.as_deref()?.trim().to_ascii_lowercase();
    match s.as_str() {
        "" => None,
        "off" | "none" => Some(ReasoningConfig::Off),
        // a model that only thinks or not takes `on` as its middle setting
        "on" => Some(ReasoningConfig::Effort(Effort::Medium)),
        n => Effort::parse(n).map(ReasoningConfig::Effort).or_else(|| n.parse().ok().map(ReasoningConfig::Budget)),
    }
}

/// The tools the model is offered: `exit_plan` only in plan mode, `recall` only when the policy asks for it.
fn offered(st: &State) -> Vec<nexo_core::provider::ToolSpec> {
    let plan = st.perm.mode == PermissionMode::Plan;
    // `recall` answers from the whole history what the curator left out: only the curator leaves anything
    // out, and it offers the tool unless `recall=0` (which keeps the tool list the same as without the
    // curator, so that a comparison measures the view alone)
    let curator = st.policy.recall();
    st.tools
        .specs()
        .into_iter()
        .filter(|s| {
            (plan || s.name != "exit_plan")
                && (curator || s.name != "recall")
                && (st.goal.is_some() || s.name != "goal_brief")
        })
        .collect()
}

/// The request for the next model call, through the context policy.
async fn prepare(
    state: &Shared,
    cancel: &CancellationToken,
    no_tool_call: bool,
) -> anyhow::Result<(ModelRequest, ProviderConfig, Arc<dyn ModelClient>, u32)> {
    let mut guard = state.lock().await;
    let st = &mut *guard;
    let specs = offered(st);
    if let Some(at) = st.policy.compact_at() {
        let at = compact_threshold(at, st.limits.context_window, st.limits.max_tokens(st.config.agent.max_tokens));
        if request_tokens(st, &specs) > at {
            // a compaction is worth its call only when what it replaces holds enough that the last one
            // did not: a request kept over the threshold by its tail, the system prompt or the tools is
            // not summarised again every round
            if let Some(keep) = compact::keep_recent(&st.history) {
                if compact::fresh_tokens(&st.history[..keep]) >= at / COMPACT_MARGIN {
                    compact_locked(st, keep, cancel).await;
                }
            }
        }
    }
    let view = view_now(st).await;
    let (_, cfg, model) = st.target.get();
    let effort = setup::effort_of(&cfg, &model, &st.config.agent.reasoning);
    let mut req = ModelRequest::new(model, st.system.clone(), view.messages);
    req.tools = specs;
    req.no_tool_call = no_tool_call;
    req.max_tokens = st.limits.max_tokens(st.config.agent.max_tokens);
    req.temperature = st.config.agent.temperature;
    req.reasoning = reasoning_of(&effort);
    st.last_request = Some(req.clone());
    Ok((req, cfg, st.client.clone(), st.config.agent.retries))
}

/// The window the view is kept under, in estimated tokens (4 characters a token): `room` provider
/// tokens divided by the ratio learned, with 5% to spare; before any count, 15% to spare.
fn view_window(room: u32, ratio: Option<f64>) -> u64 {
    match ratio {
        Some(r) => (f64::from(room) * 0.95 / r) as u64,
        None => u64::from(room) * 17 / 20,
    }
}

/// The ratio of provider tokens to estimated tokens after a request of `est` estimated tokens the
/// provider counted as `real`: the largest of the last ones weighs most, so a request of code after
/// one of prose does not pass the window; small requests, whose count is mostly framing, teach nothing.
fn learn_ratio(prev: Option<f64>, est: u64, real: u64) -> Option<f64> {
    if est < 500 || real == 0 {
        return prev;
    }
    let r = (real as f64 / est as f64).clamp(0.5, 3.0);
    Some(match prev {
        None => r,
        Some(p) if r > p => r,
        Some(p) => p * 0.8 + r * 0.2,
    })
}

/// Where the `compact` policy compacts: `at` tokens of request, or less when the model's window
/// (`window`, less the reply's `reply` tokens) would be passed first.
fn compact_threshold(at: u64, window: Option<u32>, reply: u32) -> u64 {
    match window {
        Some(w) if w > reply => at.min(u64::from(w - reply)),
        _ => at,
    }
}

/// The tokens of the next request: the provider's count of the last one (prompt and reply) plus an
/// estimate of what came after it, or, before any count, an estimate of the system prompt, the tools
/// and the history (4 characters a token).
fn request_tokens(st: &State, specs: &[nexo_core::provider::ToolSpec]) -> u64 {
    match st.last_prompt {
        Some((tokens, len)) if len <= st.history.len() => tokens + st.history.tokens_from(len),
        _ => {
            let tools = serde_json::to_string(specs).map(|s| s.len() as u64).unwrap_or(0);
            (st.system.len() as u64 + tools) / 4 + st.history.tokens()
        }
    }
}

/// What the next call will see, announced to the interface: on every call, and
/// when a conversation is loaded, so its numbers are there before the first call.
async fn view_now(st: &mut State) -> View {
    let specs = offered(st);
    let system_tokens =
        (st.system.len() as u64 + serde_json::to_string(&specs).map(|s| s.len() as u64).unwrap_or(0)) / 4;
    let names: Vec<String> = specs.iter().map(|s| s.name.clone()).collect();
    st.policy.tools(&names);
    // the view never passes the model's window: what it takes less the reply, in estimated tokens
    // through the ratio learned from the provider's counts
    let reply = st.limits.max_tokens(st.config.agent.max_tokens);
    if let Some(w) = st.limits.context_window.filter(|w| *w > reply) {
        st.policy.window(view_window(w - reply, st.tok_ratio));
    }
    let (_, cfg, model) = st.target.get();
    if let Some(p) = st.limits.pricing.or_else(|| cfg.pricing_for(&model)) {
        st.policy.prices(p.input, p.cache_read);
    }
    st.policy.history_version(st.history.version());
    let mut view = match st.policy.view(system_tokens, &st.history).await {
        Ok(v) => v,
        Err(e) => {
            notice(&st.events, Level::Warn, format!("context policy failed ({e}); sending everything"));
            View { messages: st.history.to_vec(), tokens: st.history.tokens(), note: None, memory: None }
        }
    };
    if let Some(n) = &view.note {
        notice(&st.events, Level::Warn, n.clone());
    }
    // a request with a call and no result, or a result and no call, fails on every provider: the view
    // is mended (what it holds stays, as text when it must) and each mend is said once
    let (messages, mends) = nexo_core::message::well_formed(std::mem::take(&mut view.messages));
    view.messages = messages;
    for m in mends {
        if st.mended.insert(m.clone()) {
            notice(&st.events, Level::Warn, format!("history mended before sending: {m}"));
        }
    }
    st.reads.note_view(&view.messages);
    let bytes = |ms: &[Message]| -> u64 {
        ms.iter().map(|m| serde_json::to_string(m).map(|s| s.len() as u64).unwrap_or(0)).sum()
    };
    let history_tokens = st.history.tokens();
    st.last_view = (history_tokens, view.tokens);
    st.est_sent = system_tokens + view.tokens;
    let (label, detail) = crate::context::describe(st.policy.as_ref(), history_tokens, view.tokens);
    emit(
        &st.events,
        AgentEvent::Context {
            view_tokens: view.tokens,
            history_tokens,
            history_bytes: st.history.bytes(),
            view_bytes: bytes(&view.messages),
            messages_in_view: view.messages.len(),
            messages_total: st.history.len(),
            policy: st.policy.name().to_string(),
            note: view.note.clone(),
            label,
            detail: Some(detail),
        },
    );
    let (cells, zones) = match view.memory.as_ref().and_then(|m| crate::context_map::from_memory(&st.history, m)) {
        Some(map) => map,
        None => (crate::context_map::map(&st.history, &view.messages), Vec::new()),
    };
    let same = st.last_sent.iter().zip(&view.messages).take_while(|(a, b)| a == b).count();
    let calls = crate::context_map::calls(&st.history, &cells, &view.messages, same);
    st.last_sent = view.messages.clone();
    let call_tokens = crate::context_map::call_tokens(&st.history, &cells);
    emit(&st.events, AgentEvent::ContextMap { cells, zones, calls, call_tokens });
    view
}

async fn run_turn(state: Shared, cancel: CancellationToken) -> TurnEnd {
    let (events, max_turns, guard, stale_guard, guard_answer, turn_no, mut known, ck_on, mut ck, mut known_ck) = {
        let st = state.lock().await;
        let a = &st.config.agent;
        let ck_on = a.on_limit == OnLimit::Checkpoint && a.max_checkpoints != Some(0);
        // the output lines the session has already brought (`agent.stale_guard`, and the checkpoints' progress)
        let known = if a.stale_guard > 0 { known_lines(&st.history) } else { HashSet::new() };
        let known_ck = if ck_on { known_numberless(&st.history) } else { HashSet::new() };
        let mark = checkpoint::Mark { writes: st.checkpoints.len(), wrote: 0, fresh: 0, todo: 0 };
        let todo = checkpoint::todo_of(&st.history[st.turn_start.min(st.history.len())..]);
        let mark = checkpoint::Mark { todo: todo.as_ref().map_or(0, |t| t.hash), ..mark };
        let ck = checkpoint::Tracker::new(st.cost, st.usage, mark, todo);
        (
            st.events.clone(),
            a.max_turns,
            a.loop_guard,
            a.stale_guard,
            a.guard_answer,
            st.turns,
            known,
            ck_on,
            ck,
            known_ck,
        )
    };
    emit(&events, AgentEvent::TurnStart { turn: turn_no });
    let mut compacted = false;
    // the view was asked again after the window was learned smaller (once, before any compaction)
    let mut refitted = false;
    let mut capped = false;
    let mut rounds = 0u32;
    // each tool round of this request as one hash of its calls and outputs (`agent.loop_guard`)
    let mut seen: Vec<u64> = Vec::new();
    // tool rounds in a row that wrote nothing and brought no new output line (`agent.stale_guard`)
    let mut stale = 0u32;
    // a guard stopped the tool rounds and the next call may only answer (`agent.guard_answer`)
    let mut answer = Answer::No;
    // replies in a row cut at max_tokens while writing their calls
    let mut cut_calls = 0u32;
    // replies in a row that held only reasoning: no answer and no call
    let mut mute = 0u32;
    loop {
        // with checkpoints the limit is met after the tool round that reaches it; here only a request that went
        // on without tools (a guard's answer) stops
        if rounds >= max_turns && (!ck_on || answer != Answer::No) {
            notice(
                &events,
                Level::Warn,
                format!("stopped after {max_turns} tool rounds (agent.max_turns); ask to continue"),
            );
            return TurnEnd::MaxTurns;
        }
        rounds += 1;
        // the spend limits are checked before the call, so none is passed by more than one call
        {
            let mut st = state.lock().await;
            match budget::verdict(st.cost, budget::spent_today(), st.budget, st.config.agent.max_cost_day) {
                budget::Verdict::Stop(why) => {
                    notice(&events, Level::Error, why);
                    return TurnEnd::Error;
                }
                budget::Verdict::Warn(why) if !st.budget_warned => {
                    st.budget_warned = true;
                    notice(&events, Level::Warn, why);
                }
                _ => {}
            }
        }
        let (mut req, cfg, client, retries) = match prepare(&state, &cancel, answer == Answer::Forced).await {
            Ok(x) => x,
            Err(e) => {
                notice(&events, Level::Error, e.to_string());
                return TurnEnd::Error;
            }
        };
        if cut_calls > 0 {
            req.temperature = retry_temperature(req.temperature);
        }
        if cancel.is_cancelled() {
            return TurnEnd::Interrupted;
        }
        emit(&events, AgentEvent::AssistantStart);
        {
            let mut st = state.lock().await;
            if !st.mail_turn.is_empty() {
                let mails = std::mem::take(&mut st.mail_turn);
                if let Some(m) = st.mail.clone() {
                    emit(&events, m.read(&mails));
                }
            }
        }
        let started = tokio::select! {
            r = client.stream(&cfg, &req, retries) => r,
            _ = cancel.cancelled() => return TurnEnd::Interrupted,
        };
        let mut stream = match started {
            Ok(s) => s,
            Err(e) => match learn(&state, &e, &req.model).await {
                Lesson::Refit if !refitted => {
                    refitted = true;
                    continue;
                }
                Lesson::Context | Lesson::Refit if !compacted => {
                    compacted = true;
                    if compact_now(&state, &events, Some(()), &cancel).await {
                        continue;
                    }
                    notice(
                        &events,
                        Level::Error,
                        format!(
                            "the request does not fit the model's window and could not be compacted: {}",
                            e.human()
                        ),
                    );
                    return TurnEnd::Error;
                }
                Lesson::Output if !capped => {
                    capped = true;
                    continue;
                }
                _ => {
                    notice(&events, Level::Error, explain(&e));
                    return TurnEnd::Error;
                }
            },
        };
        let mut c = Collector::for_model(req.model.clone());
        let mut interrupted = false;
        let mut marker = ck.pending.is_some().then(checkpoint::MarkerFilter::start);
        loop {
            tokio::select! {
                ev = stream.next() => match ev {
                    None => break,
                    Some(ev) => {
                        match &ev {
                            StreamEvent::TextDelta(t) => {
                                let text = match &mut marker {
                                    Some(f) => f.push(t),
                                    None => t.clone(),
                                };
                                if !text.is_empty() {
                                    emit(&events, AgentEvent::TextDelta { text });
                                }
                            }
                            StreamEvent::ReasoningDelta(t) => emit(&events, AgentEvent::ReasoningDelta { text: t.clone() }),
                            _ => {}
                        }
                        c.push(ev);
                    }
                },
                _ = cancel.cancelled() => { interrupted = true; break }
            }
        }
        if let Some(f) = &mut marker {
            let text = f.finish();
            if !text.is_empty() {
                emit(&events, AgentEvent::TextDelta { text });
            }
        }
        drop(stream);
        let mut msg = c.message();
        if c.error.is_none() && !interrupted {
            // a reply that wrote its calls as text, in the model's own markup, makes them as calls
            let specs = state.lock().await.tools.specs();
            // ids unique in the session: turns count from 0 again after a resume or a fork
            let prefix = format!("text_call_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
            if let Some(m) = leak::recover(&msg, &specs, &prefix) {
                let n = m.tool_calls().count();
                notice(&events, Level::Info, format!("the model wrote {n} tool call(s) as text; they run as calls"));
                msg = m;
            }
        }
        let (usage, call) = (c.usage, c.call);
        let cut = c.finish == Some(FinishReason::Length);
        if cut && c.error.is_none() {
            leave_out_cut_args(&mut msg);
        }
        if let Some(e) = &c.error {
            if msg.content.is_empty() {
                match learn(&state, e, &req.model).await {
                    Lesson::Refit if !refitted => {
                        bill_attempt(&state, &events, &req.model, usage, call).await;
                        refitted = true;
                        continue;
                    }
                    Lesson::Context | Lesson::Refit if !compacted => {
                        compacted = true;
                        if compact_now(&state, &events, Some(()), &cancel).await {
                            bill_attempt(&state, &events, &req.model, usage, call).await;
                            continue;
                        }
                    }
                    Lesson::Output if !capped => {
                        capped = true;
                        bill_attempt(&state, &events, &req.model, usage, call).await;
                        continue;
                    }
                    _ => {}
                }
            }
        }
        {
            let mut st = state.lock().await;
            if !msg.content.is_empty() {
                st.history.push(msg.clone());
                if let Err(e) = st.session.append(&msg) {
                    notice(&events, Level::Warn, format!("cannot save the session: {e}"));
                }
            }
            if !usage.is_empty() {
                let model = req.model.clone();
                add_usage(&mut st, &events, &model, usage, call, UsageKind::Main);
                st.policy.usage(call.cache_read, call.total_input());
                st.tok_ratio = learn_ratio(st.tok_ratio, st.est_sent, call.total_input());
            }
            if !msg.content.is_empty() && call.total_input() > 0 {
                let len = st.history.len();
                st.last_prompt = Some((call.total_input() + call.output, len));
            }
            if !msg.content.is_empty() {
                emit(&events, AgentEvent::AssistantDone { message: msg.clone() });
            }
        }
        let calls: Vec<(String, String, Value)> =
            msg.tool_calls().map(|(i, n, a)| (i.to_string(), n.to_string(), a.clone())).collect();
        if let Some(e) = c.error {
            notice(&events, Level::Error, explain(&e));
            if !calls.is_empty() {
                push_results(&state, cancelled_results(&calls, "the model call failed before the tools ran")).await;
            }
            return TurnEnd::Error;
        }
        if interrupted {
            if !calls.is_empty() {
                push_results(&state, cancelled_results(&calls, "interrupted by the person")).await;
            }
            return TurnEnd::Interrupted;
        }
        if cut && !calls.is_empty() {
            // the last call stopped half written: its arguments are not what the model meant, and the
            // calls before it belong to a plan cut short. None runs; the model is told why and asks again
            push_results(&state, cancelled_results(&calls, CUT_NOTE)).await;
            cut_calls += 1;
            if cut_calls >= 2 {
                notice(&events, Level::Error, "two replies in a row were cut at max_tokens while writing their calls; stopped (raise agent.max_tokens or ask for smaller steps)");
                return TurnEnd::Error;
            }
            notice(&events, Level::Warn, "the reply was cut at max_tokens while writing its calls; they did not run");
            continue;
        }
        cut_calls = 0;
        if calls.is_empty() && msg.text().trim().is_empty() && !msg.reasoning().trim().is_empty() && mute < 2 {
            // the model thought and stopped before writing anything: ending here would leave the request
            // undone with nothing said, so it is asked to go on from its reasoning
            mute += 1;
            notice(&events, Level::Warn, "the reply held only reasoning, with no answer and no call; asked to go on");
            push_results(&state, vec![Part::text(MUTE_NOTE)]).await;
            continue;
        }
        mute = 0;
        if let Some((n, line)) = ck.pending.take() {
            let text = msg.text();
            match checkpoint::parse_decision(&text) {
                Some(d) if !d.go_on => {
                    notice(&events, Level::Info, format!("⟳ checkpoint {n} · {line} · stop: {}", d.reason));
                    ck.stopping = Some(d.reason.clone());
                    if d.rest.is_empty() {
                        // the reason alone is no summary: one more call asks for it, the tools staying declared
                        let mut out = cancelled_results(&calls, "stopped at a checkpoint");
                        out.push(Part::text(SUMMARY_NOW));
                        push_results(&state, out).await;
                        answer = Answer::Asked;
                        continue;
                    }
                    if !calls.is_empty() {
                        push_results(&state, cancelled_results(&calls, "stopped at a checkpoint")).await;
                    }
                    checkpoint_stopped(&state, &d.reason, &text).await;
                    return TurnEnd::Done;
                }
                Some(d) => {
                    notice(&events, Level::Info, format!("⟳ checkpoint {n} · {line} · continue: {}", d.reason));
                    if calls.is_empty() && !ck.nudged {
                        ck.nudged = true;
                        push_results(&state, vec![Part::text(CHECKPOINT_GO_ON)]).await;
                        continue;
                    }
                }
                None if !calls.is_empty() => {
                    notice(&events, Level::Info, format!("⟳ checkpoint {n} · {line} · continue (no decision line)"));
                }
                None if calls.is_empty() => {
                    notice(&events, Level::Info, format!("⟳ checkpoint {n} · {line} · ended without a decision line"));
                }
                None => {}
            }
        }
        if calls.is_empty() {
            if cut {
                notice(&events, Level::Warn, "the reply was cut at max_tokens (agent.max_tokens)");
            }
            if let Some(why) = ck.stopping.take() {
                checkpoint_stopped(&state, &why, &msg.text()).await;
            }
            return TurnEnd::Done;
        }
        if answer != Answer::No {
            // the model called a tool all the same: the call does not run; the next call is told by the protocol
            // that none may be called, and if it calls one even so, the request stops
            push_results(&state, cancelled_results(&calls, "a guard stopped the tool rounds")).await;
            if answer == Answer::Forced {
                if let Some(why) = ck.stopping.take() {
                    // it chose to stop and would not write the summary: the stop stands, not the round limit
                    checkpoint_stopped(&state, &why, &msg.text()).await;
                    return TurnEnd::Done;
                }
                return TurnEnd::MaxTurns;
            }
            answer = Answer::Forced;
            continue;
        }
        let asked = calls.iter().map(|(_, n, a)| (n.clone(), a.to_string())).collect::<Vec<_>>();
        let kept = calls.clone();
        let (results, cancelled) = run_tools(&state, &events, calls, &cancel).await;
        seen.push(round_hash(&asked, &results));
        let (lines, new) = if stale_guard > 0 { fresh_count(&mut known, &results, false) } else { (0, 0) };
        if ck_on {
            ck.fresh += fresh_count(&mut known_ck, &results, true).1;
            ck.round(&asked, &results);
            note_writes(&state, &mut ck, &asked, &results).await;
        }
        // the guards above weigh the outputs as they came; the line of a failure that came back is added after
        let results = {
            let mut st = state.lock().await;
            st.repeats.round(&kept, results)
        };
        if stale_guard > 0 {
            let wrote = {
                let st = state.lock().await;
                asked.iter().any(|(n, _)| st.tools.get(n).is_some_and(|t| t.kind() == ToolKind::Write))
            };
            // a round with no output line (a file written by a shell command, a silent step) is neither
            stale = match (lines, new) {
                _ if wrote => 0,
                (0, _) => stale,
                (_, 0) => stale + 1,
                _ => 0,
            };
        }
        // a guard that stops the tool rounds here, and what the person is told
        let stop = if cancelled {
            None
        } else if let Some(p) = looping(&seen, guard) {
            let what = if p == 1 { "the same tool round".to_string() } else { format!("the same {p} tool rounds") };
            Some(format!(
                "stopped: {what} ran {guard} times in a row with the same outputs (agent.loop_guard){}",
                then(guard_answer)
            ))
        } else if stale_guard > 0 && stale >= stale_guard {
            Some(format!("stopped: {stale} tool rounds in a row wrote nothing and brought no output line this session had not seen (agent.stale_guard){}", then(guard_answer)))
        } else {
            None
        };
        let mut results = results;
        if stop.is_some() && guard_answer {
            // the model is told, next to the outputs, that the tools are off: told nothing, it writes the next
            // call as text even when the protocol forbids calls (seen with DeepSeek, with or without tools)
            answer = Answer::Asked;
            results.push(Part::text(ANSWER_NOW));
        }
        if ck_on && stop.is_none() && !cancelled && answer == Answer::No && rounds >= max_turns {
            let ctx = Standing { rounds: &seen, guard, stale, stale_guard };
            match checkpoint_now(&state, &events, &mut ck, &ctx).await {
                Ok(text) => {
                    results.push(Part::text(text));
                    rounds = 0;
                }
                Err(end) => {
                    push_results(&state, results).await;
                    return end;
                }
            }
        }
        push_results(&state, results).await;
        if cancelled {
            return TurnEnd::Interrupted;
        }
        if let Some(why) = stop {
            notice(&events, Level::Warn, why);
            if !guard_answer {
                return TurnEnd::MaxTurns;
            }
        }
        if take_inbox(&state).await {
            // the person spoke: the new message is answered with tools on, and the guards start over
            answer = Answer::No;
            seen.clear();
            stale = 0;
        }
    }
}

/// What follows a guard's stop: an answer from what was gathered (`agent.guard_answer`), or the person's word.
/// What the model reads, after the outputs of the round a guard stopped, before the call that may only answer
/// (`agent.guard_answer`).
/// Where a request stands after a guard's stop (`agent.guard_answer`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// No guard stopped the tool rounds.
    No,
    /// The model is asked in words (`ANSWER_NOW`) to answer. The call keeps the tools and the tool choice of
    /// every other call: a request that changes them misses the provider's prompt cache (DeepSeek leaves the
    /// tools out of the prompt, Anthropic drops the cached messages), and that one call then costs as much as
    /// the whole view sent anew.
    Asked,
    /// The model called a tool all the same: the protocol is told that none may be called.
    Forced,
}

/// Why the calls of a reply cut at max_tokens did not run, and how to fit the next one in a reply: a model at a
/// low temperature otherwise sends the same call again and is cut at the same place
const CUT_NOTE: &str = "the reply was cut at max_tokens before its calls were complete. A call has to fit in one \
    reply: write a long file in parts, keep test scripts short, and do not spell out long lists of data";

/// The arguments of the calls of a reply cut at max_tokens, left out of the history: they never run, and a
/// model that looped into the cap (a test spelling out 1..100) copies its own loop when it sees it again
fn leave_out_cut_args(msg: &mut Message) {
    for p in &mut msg.content {
        if let Part::ToolCall { args, .. } = p {
            let chars = args.to_string().chars().count();
            *args = serde_json::json!({ "left_out": format!("cut at max_tokens after {chars} characters") });
        }
    }
}

/// The lowest temperature of the request that follows a reply cut at max_tokens while writing its calls
const CUT_TEMPERATURE: f32 = 0.6;

/// The temperature of the request after a cut: a low one is raised to `CUT_TEMPERATURE`, because a model at
/// temperature 0 that looped into the cap loops the same way again from the same history; none (the provider's
/// own) and a higher one stay
fn retry_temperature(t: Option<f32>) -> Option<f32> {
    t.map(|t| t.max(CUT_TEMPERATURE))
}

/// Sent when a reply held only reasoning (often cut at max_tokens while thinking): not a request of the person.
const MUTE_NOTE: &str =
    "<system-reminder>Your last reply held only reasoning: no answer and no tool call reached the person. \
     Go on from it now with a tool call or the answer, keeping the reasoning short.</system-reminder>";

const ANSWER_NOW: &str =
    "Tool calls are off for the rest of this request: they will not run. Give your answer now, from what you have already seen. \
     The next message of the person turns them on again.";

/// Sent after `CHECKPOINT STOP` with no summary: the request ends on the model's account of its work.
const SUMMARY_NOW: &str = "<system-reminder>You chose to stop at the checkpoint. Tool calls will not run now. \
     Write your final summary for the person: what was done, what is left and how to go on.</system-reminder>";

/// Sent once when the model chose to continue at a checkpoint but called no tool.
const CHECKPOINT_GO_ON: &str =
    "<system-reminder>You chose to continue at the checkpoint. Go on with the next tool call.</system-reminder>";

/// What `checkpoint_now` reads of the loop's locals.
struct Standing<'a> {
    rounds: &'a [u64],
    guard: u32,
    stale: u32,
    stale_guard: u32,
}

/// The most checkpoints this request may still take: none for a spend limit that is in force (the limit
/// stops the request), `agent.max_checkpoints` or 5 otherwise. A limit counts only once this request has spent
/// something: on a model with no prices, or when the day's spend cannot be kept, it would never fire.
fn checkpoint_cap(st: &State, cost0: f64) -> Option<u32> {
    if let Some(n) = st.config.agent.max_checkpoints {
        return Some(n);
    }
    let limited = st.budget.is_some_and(|b| b > 0.0) || st.config.agent.max_cost_day.is_some_and(|d| d > 0.0);
    if limited && st.cost > cost0 {
        None
    } else {
        Some(checkpoint::DEFAULT_MAX)
    }
}

/// A request reached `agent.max_turns` with `on_limit = "checkpoint"`: the message to add to the tool results,
/// or the end of the request when a cap or the lack of progress says so (the model is not asked then).
async fn checkpoint_now(
    state: &Shared,
    events: &EventTx,
    ck: &mut checkpoint::Tracker,
    ctx: &Standing<'_>,
) -> Result<String, TurnEnd> {
    let mut st = state.lock().await;
    let (used, cap) = (st.ck_used, checkpoint_cap(&st, ck.cost0));
    if cap.is_some_and(|c| used >= c) {
        let limited = if st.config.agent.max_checkpoints.is_some() { "" } else { "; no spend limit is set" };
        notice(
            events,
            Level::Warn,
            format!("stopped after {used} checkpoints (agent.max_checkpoints{limited}); ask to continue"),
        );
        st.ck_stop = Some(format!("stopped after {used} checkpoints"));
        return Err(TurnEnd::MaxTurns);
    }
    let todo = checkpoint::todo_of(&st.history[st.turn_start.min(st.history.len())..]);
    let now = checkpoint::Mark {
        writes: st.checkpoints.len(),
        wrote: ck.wrote(),
        fresh: ck.fresh,
        todo: todo.as_ref().map_or(0, |t| t.hash),
    };
    // a message of the person that joins now is steering: it counts as progress
    if checkpoint::progress(&now, &ck.prev) || !st.inbox.is_empty() {
        ck.idle = 0;
    } else {
        ck.idle += 1;
    }
    if ck.idle >= 2 {
        notice(
            events,
            Level::Warn,
            "stopped: two checkpoints in a row with no file written, no new output line and no change in the todo list; ask to continue",
        );
        st.ck_stop = Some("two checkpoints in a row without progress".to_string());
        return Err(TurnEnd::MaxTurns);
    }
    let from = st.turn_start.min(st.history.len());
    let distinct = |from: usize| {
        let mut out: Vec<String> = Vec::new();
        for (_, p, _) in st.checkpoints.iter().skip(from) {
            let p = p.display().to_string();
            if !out.contains(&p) {
                out.push(p);
            }
        }
        out
    };
    let first = st.history.get(from).map(Message::text).unwrap_or_default();
    let request = if first.starts_with(GOAL_CONTINUE_MARK) { "(goal continuation)".to_string() } else { first };
    let (u, u0) = (st.usage, ck.usage0);
    let spend = {
        let mut s = format!("${:.2} this request, ${:.2} this session", st.cost - ck.cost0, st.cost);
        if let Some(b) = st.budget.filter(|b| *b > 0.0) {
            s.push_str(&format!(" of ${b:.2} (max_cost)"));
        }
        if let Some(d) = st.config.agent.max_cost_day.filter(|d| *d > 0.0) {
            s.push_str(&format!("; ${:.2} of ${d:.2} today (max_cost_day)", budget::spent_today()));
        }
        match checkpoint_cap(&st, ck.cost0) {
            Some(c) if st.config.agent.max_checkpoints.is_none() => {
                s.push_str(&format!(" | no spend limit is set: at most {c} checkpoints"))
            }
            Some(c) => s.push_str(&format!(" | at most {c} checkpoints")),
            None => s.push_str(" | a spend limit is set: it ends the request"),
        }
        s
    };
    let loop_hits = (2..ctx.guard.max(2)).rev().find(|t| looping(ctx.rounds, *t).is_some()).unwrap_or(0);
    let report = checkpoint::Report {
        number: used + 1,
        cap,
        cost: st.cost - ck.cost0,
        calls: checkpoint::calls_in(&st.history[from..]),
        rounds: ck.rounds,
        input: u.total_input().saturating_sub(u0.total_input()),
        output: u.output.saturating_sub(u0.output),
        cached: u.cache_read.saturating_sub(u0.cache_read),
        elapsed: ck.started.elapsed(),
        request,
        goal: st.goal.clone(),
        todo: todo.clone(),
        prev_todo: ck.prev_todo.clone(),
        files: distinct(ck.writes0),
        new_files: distinct(ck.prev.writes),
        fresh_total: ck.fresh,
        fresh_new: ck.fresh - ck.prev.fresh,
        build: ck.build(),
        errors: ck.repeated(),
        loop_hits,
        loop_limit: ctx.guard,
        stale: ctx.stale,
        stale_limit: ctx.stale_guard,
        spend,
        idle: ck.idle,
    };
    let line = checkpoint::stats(&report);
    notice(events, Level::Info, format!("⟳ checkpoint {} · {line} · asking", report.number));
    st.ck_used += 1;
    ck.pending = Some((report.number, line));
    ck.nudged = false;
    ck.prev = now;
    ck.prev_todo = todo;
    Ok(checkpoint::render(&report))
}

/// Notes the files the write tools of a round left: only a write that succeeded, with content this request had
/// not left in that file before, is progress (a rewrite of the same bytes, a denied or failed write is not).
async fn note_writes(state: &Shared, ck: &mut checkpoint::Tracker, asked: &[(String, String)], results: &[Part]) {
    use std::hash::{Hash, Hasher};
    let outs: Vec<bool> = results
        .iter()
        .filter_map(|p| if let Part::ToolResult { is_error, .. } = p { Some(*is_error) } else { None })
        .collect();
    if outs.len() != asked.len() {
        return;
    }
    let st = state.lock().await;
    for ((name, args), failed) in asked.iter().zip(outs) {
        if failed || !st.tools.get(name).is_some_and(|t| t.kind() == ToolKind::Write) {
            continue;
        }
        let args: Value = serde_json::from_str(args).unwrap_or(Value::Null);
        let Some(p) = args.get("path").and_then(Value::as_str) else { continue };
        let path = if Path::new(p).is_absolute() { PathBuf::from(p) } else { st.cwd.join(p) };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match std::fs::read(&path) {
            Ok(bytes) => bytes.hash(&mut h),
            Err(_) => args.to_string().hash(&mut h),
        }
        ck.note_write(&path.display().to_string(), h.finish());
    }
}

/// The model chose to stop at a checkpoint: an active goal pauses there, unless the reply meets or awaits it.
async fn checkpoint_stopped(state: &Shared, why: &str, reply: &str) {
    let mut st = state.lock().await;
    if st.goal.is_some() && st.goal_state.is_active() && !goal_done(reply) && !goal_wait(reply) {
        let say = format!("goal paused: stopped at a checkpoint ({why}) · /goal resume goes on");
        goal_to(&mut st, GoalState::Paused(format!("stopped at a checkpoint: {why}")), say);
    }
}

fn then(answer: bool) -> &'static str {
    if answer {
        "; one last call without tools asks for the answer"
    } else {
        "; ask to continue"
    }
}

/// One tool round as a hash of what it asked (tool and arguments) and what came back, call ids aside: two
/// rounds with the same hash brought the agent nothing the other did not.
fn round_hash(asked: &[(String, String)], results: &[Part]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    asked.hash(&mut h);
    for r in results {
        if let Part::ToolResult { name, content, is_error, .. } = r {
            (name, serde_json::to_string(content).unwrap_or_default(), is_error).hash(&mut h);
        }
    }
    h.finish()
}

/// Puts the output lines of `parts` among the `known` ones and says whether any was not there yet, or None
/// when there was no line (`agent.stale_guard`). A line counts trimmed and from four characters on: blank
/// lines, braces and short markers come back in any output and tell nothing of what it brought.
fn fresh_lines(known: &mut HashSet<u64>, parts: &[Part]) -> Option<bool> {
    match fresh_count(known, parts, false) {
        (0, _) => None,
        (_, new) => Some(new > 0),
    }
}

/// [`fresh_lines`] as numbers: the lines counted and how many of them were new. With `norm` a line counts
/// with its digit runs read as one mark, so a clock, a counter or a pid does not make an old line new.
fn fresh_count(known: &mut HashSet<u64>, parts: &[Part], norm: bool) -> (usize, usize) {
    use std::hash::{Hash, Hasher};
    let (mut lines, mut fresh) = (0, 0);
    for p in parts {
        if let Part::ToolResult { content, .. } = p {
            for line in content.iter().filter_map(Part::as_text).flat_map(str::lines) {
                let line = line.trim();
                if line.chars().count() >= 4 {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    if norm {
                        numberless(line).hash(&mut h);
                    } else {
                        line.hash(&mut h);
                    }
                    lines += 1;
                    fresh += usize::from(known.insert(h.finish()));
                }
            }
        }
    }
    (lines, fresh)
}

/// `line` with each run of digits turned into one `#`.
fn numberless(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        if c.is_ascii_digit() {
            if !out.ends_with('#') {
                out.push('#');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The lines a conversation has brought, digits aside: what the checkpoints call new.
fn known_numberless(history: &[Message]) -> HashSet<u64> {
    let mut known = HashSet::new();
    for m in history {
        let _ = fresh_count(&mut known, &m.content, true);
    }
    known
}

/// The output lines a conversation has already brought, as `fresh_lines` counts them.
fn known_lines(history: &[Message]) -> HashSet<u64> {
    let mut known = HashSet::new();
    for m in history {
        let _ = fresh_lines(&mut known, &m.content);
    }
    known
}

/// `agent.loop_guard`: the length `p` of the block of rounds that closes `rounds` and ran `times` times in a
/// row, the same calls with the same outputs each time (the shortest such block). A person watching would
/// see the agent going down the same path again with nothing new on it: a command run over and over, or a
/// cycle of reads that never changes. None below two times, or when the latest rounds are not such a loop.
fn looping(rounds: &[u64], times: u32) -> Option<usize> {
    let (n, t) = (rounds.len(), times as usize);
    if t < 2 {
        return None;
    }
    (1..=n / t).find(|&p| (1..t).all(|k| (0..p).all(|i| rounds[n - 1 - i] == rounds[n - 1 - i - k * p])))
}

fn cancelled_results(calls: &[(String, String, Value)], reason: &str) -> Vec<Part> {
    calls
        .iter()
        .map(|(id, name, _)| Part::ToolResult {
            call_id: id.clone(),
            name: Some(name.clone()),
            content: vec![Part::text(format!("The call did not run: {reason}."))],
            is_error: true,
        })
        .collect()
}

async fn push_results(state: &Shared, results: Vec<Part>) {
    if results.is_empty() {
        return;
    }
    let mut st = state.lock().await;
    // the outputs, and after them the text the loop adds (`ANSWER_NOW`)
    let m = Message { role: Role::User, content: results };
    st.history.push(m.clone());
    if let Err(e) = st.session.append(&m) {
        notice(&st.events, Level::Warn, format!("cannot save the session: {e}"));
    }
}

/// What the permission dialog shows for a call.
fn detail_of(kind: ToolKind, args: &Value, cwd: &Path) -> ToolDisplay {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    match kind {
        ToolKind::Exec => ToolDisplay::Shell {
            command: s("command").or_else(|| s("cmd")).unwrap_or_default(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
        },
        ToolKind::Write => {
            let path = s("path").or_else(|| s("file_path")).unwrap_or_default();
            let file = if Path::new(&path).is_absolute() { PathBuf::from(&path) } else { cwd.join(&path) };
            if let (Some(old), Some(new)) = (s("old_string"), s("new_string")) {
                // only the replaced text is shown: where it sits in the file numbers the diff
                let start_line = line_in_file(&file, &old);
                ToolDisplay::Diff { path, old, new, start_line }
            } else if let Some(content) = s("content") {
                let old = std::fs::read_to_string(&file).unwrap_or_default();
                ToolDisplay::Diff { path, old, new: content, start_line: 1 }
            } else {
                ToolDisplay::Plain
            }
        }
        // the whole address, query included: what it would carry out is the person's call
        ToolKind::Network => match s("url").or_else(|| s("uri")) {
            Some(u) => ToolDisplay::List { items: vec![u] },
            None => ToolDisplay::Plain,
        },
        _ => ToolDisplay::Plain,
    }
}

/// The most of a file read to find where a text sits in it (the edit tool takes as much).
const LINE_LOOKUP_MAX: u64 = 32 * 1024 * 1024;

/// The line (counted from 1) of the file at `path` where `snippet` first appears; 1 when the
/// file cannot be read, is not a plain file within `LINE_LOOKUP_MAX` (a preview must not wait
/// on a pipe or read a huge file) or does not hold the text.
fn line_in_file(path: &Path, snippet: &str) -> usize {
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= LINE_LOOKUP_MAX) {
        return 1;
    }
    let Ok(bytes) = std::fs::read(path) else { return 1 };
    line_of(&String::from_utf8_lossy(&bytes), snippet)
}

/// The line (counted from 1) where `snippet` first appears in `text`; 1 if it does not.
fn line_of(text: &str, snippet: &str) -> usize {
    let (text, snippet) = (text.replace("\r\n", "\n"), snippet.replace("\r\n", "\n"));
    text.find(&snippet).map_or(1, |at| text[..at].matches('\n').count() + 1)
}

enum Verdict {
    Run,
    Denied(String),
}

/// Whether a call may run: the policy decides, or the person is asked.
#[allow(clippy::too_many_arguments)]
async fn permit(
    state: &Shared,
    events: &EventTx,
    gate: &Arc<dyn PermissionGate>,
    cwd: &Path,
    id: &str,
    name: &str,
    tool: &dyn Tool,
    args: &Value,
    cancel: &CancellationToken,
) -> Verdict {
    let kind = tool.kind();
    let decision = state.lock().await.perm.decide(name, kind, args, cwd);
    match decision {
        Decision::Allow => Verdict::Run,
        Decision::Deny(r) => Verdict::Denied(r),
        Decision::Ask => {
            let req = PermissionRequest {
                id: id.to_string(),
                tool: name.to_string(),
                kind,
                summary: tool.summary(args),
                detail: detail_of(kind, args, cwd),
                suggested_rules: permission::suggested_rules(name, kind, args, cwd),
            };
            let d = tokio::select! {
                d = gate.ask(req) => d,
                _ = cancel.cancelled() => PermissionDecision::Deny { reason: Some("interrupted by the person".into()) },
            };
            emit(events, AgentEvent::PermissionResolved { id: id.to_string(), decision: d.clone() });
            match d {
                PermissionDecision::Allow => Verdict::Run,
                PermissionDecision::AllowAlways { rule, persist } => {
                    state.lock().await.perm.add_allow(&rule);
                    if persist {
                        match nexo_core::paths::project_dir(cwd) {
                            Some(dir) => {
                                if let Err(e) = permission::persist_rule(&dir, &rule) {
                                    notice(events, Level::Warn, format!("cannot save the rule: {e}"));
                                } else if let Some(root) = dir.parent().filter(|r| {
                                    !nexo_core::config::is_trusted(r, &nexo_core::paths::trusted_projects_file())
                                }) {
                                    notice(
                                        events,
                                        Level::Info,
                                        format!(
                                            "saved; the allow rules of a project apply in later sessions once \
                                             you trust it: nexo config trust \"{}\"",
                                            root.display()
                                        ),
                                    );
                                }
                            }
                            None => notice(
                                events,
                                Level::Warn,
                                "no project directory to save the rule in (only for this session)",
                            ),
                        }
                    }
                    Verdict::Run
                }
                PermissionDecision::Deny { reason } => {
                    Verdict::Denied(reason.unwrap_or_else(|| "denied by the person".into()))
                }
            }
        }
    }
}

/// Adds what a model call spent to the session and the day, and says so; `call` is the usage of the
/// attempt that answered, without the attempts the client retried, and empty for a side call (a
/// sub-agent, the reader, a compaction) whose request is not the conversation's.
fn add_usage(st: &mut State, events: &EventTx, model: &str, usage: Usage, call: Usage, kind: UsageKind) {
    add_usage_priced(st, events, model, usage, call, None, kind)
}

/// [`add_usage`] with the prices of another provider (a sub-agent sent there); none: the main agent's.
fn add_usage_priced(
    st: &mut State,
    events: &EventTx,
    model: &str,
    usage: Usage,
    call: Usage,
    other: Option<Pricing>,
    kind: UsageKind,
) {
    st.usage += usage;
    let (_, cfg, main) = st.target.get();
    let pricing = other.or_else(|| {
        if model == main {
            st.limits.pricing.or_else(|| cfg.pricing_for(model))
        } else {
            cfg.pricing_for(model)
        }
    });
    let cost = pricing.map(|p| p.cost(&usage)).unwrap_or(0.0);
    st.cost += cost;
    // what the view left out of the history at this call (the conversation's own calls only)
    let held = (kind == UsageKind::Main).then(|| st.last_view.0.saturating_sub(st.last_view.1));
    let meta = UsageMeta { model: Some(model.to_string()), kind: Some(kind), held };
    let _ = st.session.record_usage_meta(usage, cost, meta);
    emit(events, AgentEvent::Usage { turn: usage, call, session: st.usage, cost_usd: st.cost });
    let day = budget::add(cost);
    emit(events, budget_event(st, day));
}

/// The sub-agents' way to the agent: its permissions and its bill.
struct StateHost {
    state: Weak<Mutex<State>>,
    events: EventTx,
}

#[async_trait::async_trait]
impl crate::subagent::Host for StateHost {
    async fn permit(
        &self,
        call_id: &str,
        tool: &dyn Tool,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let Some(state) = self.state.upgrade() else { return Err("the session has ended".into()) };
        let (gate, cwd) = {
            let st = state.lock().await;
            (st.gate.clone(), st.cwd.clone())
        };
        let name = tool.name().to_string();
        match permit(&state, &self.events, &gate, &cwd, call_id, &name, tool, args, cancel).await {
            Verdict::Run => Ok(()),
            Verdict::Denied(r) => Err(r),
        }
    }

    async fn spent(&self, model: &str, usage: Usage, pricing: Option<Pricing>) {
        if let Some(state) = self.state.upgrade() {
            let mut st = state.lock().await;
            add_usage_priced(&mut st, &self.events, model, usage, Usage::default(), pricing, UsageKind::Sub);
        }
    }

    async fn history(&self) -> Vec<Message> {
        match self.state.upgrade() {
            Some(state) => state.lock().await.history.to_vec(),
            None => Vec::new(),
        }
    }

    /// A sub-agent's write is put back by a rewind like the main agent's.
    async fn before_write(&self, path: PathBuf) {
        if let Some(state) = self.state.upgrade() {
            let before = std::fs::read(&path).ok();
            let mut st = state.lock().await;
            let at = st.history.len();
            st.checkpoints.push((at, path, before));
        }
    }
}

/// Changes the permission mode; entering or leaving plan mode changes the system prompt.
fn set_mode(st: &mut State, mode: PermissionMode) {
    let was = st.perm.mode;
    st.perm.mode = mode;
    st.plan.lock().unwrap_or_else(|e| e.into_inner()).set(mode);
    if (was == PermissionMode::Plan) != (mode == PermissionMode::Plan) {
        st.system = full_system(st);
    }
    if let Some(m) = &st.mail {
        m.set_bypass(mode == PermissionMode::Auto);
    }
    emit(&st.events, AgentEvent::PermissionModeChanged { mode });
}

/// Runs the calls of one assistant message; (results in order, interrupted).
async fn run_tools(
    state: &Shared,
    events: &EventTx,
    calls: Vec<(String, String, Value)>,
    cancel: &CancellationToken,
) -> (Vec<Part>, bool) {
    let (tools, cwd, gate, session_id) = {
        let st = state.lock().await;
        (st.tools.clone(), st.cwd.clone(), st.gate.clone(), st.session.id().to_string())
    };
    for (id, name, args) in &calls {
        emit(
            events,
            AgentEvent::ToolCall {
                id: id.clone(),
                name: name.clone(),
                args: args.clone(),
                summary: summary_of(&tools, name, args),
            },
        );
    }
    // permissions first, in order, so the asks come one at a time
    let mut verdicts: Vec<Verdict> = Vec::with_capacity(calls.len());
    for (id, name, args) in &calls {
        if cancel.is_cancelled() {
            verdicts.push(Verdict::Denied("interrupted by the person".into()));
            continue;
        }
        let Some(tool) = tools.get(name) else {
            verdicts.push(Verdict::Denied(format!("unknown tool `{name}`")));
            continue;
        };
        let v = permit(state, events, &gate, &cwd, id, name, tool.as_ref(), args, cancel).await;
        verdicts.push(v);
    }
    // every verdict is in by now: reads, searches and page fetches can overlap
    let overlaps: Vec<bool> = calls
        .iter()
        .map(|(_, n, a)| {
            tools
                .get(n)
                .map(|t| matches!(t.call_kind(a), ToolKind::Read | ToolKind::Search | ToolKind::Network))
                .unwrap_or(false)
        })
        .collect();
    let mut results: Vec<Option<Part>> = (0..calls.len()).map(|_| None).collect();
    let mut cancelled = false;
    // in the order asked: each run of calls that can overlap goes at once, any other call alone, so one
    // command among three sub-agents does not make the sub-agents wait for each other
    let mut i = 0;
    while i < calls.len() {
        let end = if overlaps[i] { i + overlaps[i..].iter().take_while(|o| **o).count() } else { i + 1 };
        if cancelled || cancel.is_cancelled() {
            cancelled = true;
            for (j, (id, name, args)) in calls.iter().enumerate().take(end).skip(i) {
                results[j] = Some(
                    cancelled_results(&[(id.clone(), name.clone(), args.clone())], "interrupted by the person")
                        .remove(0),
                );
                emit(
                    events,
                    AgentEvent::ToolDone { id: id.clone(), output: ToolOutput::error("interrupted"), elapsed_ms: 0 },
                );
            }
        } else {
            let futs = calls[i..end].iter().zip(verdicts[i..end].iter()).map(|((id, name, args), v)| {
                run_one(state, events, &tools, &cwd, &session_id, id, name, args, v, cancel)
            });
            for (k, (part, c)) in futures::future::join_all(futs).await.into_iter().enumerate() {
                results[i + k] = Some(part);
                cancelled |= c;
            }
        }
        i = end;
    }
    (results.into_iter().flatten().collect(), cancelled)
}

#[allow(clippy::too_many_arguments)]
async fn run_one(
    state: &Shared,
    events: &EventTx,
    tools: &ToolSet,
    cwd: &Path,
    session_id: &str,
    id: &str,
    name: &str,
    args: &Value,
    verdict: &Verdict,
    cancel: &CancellationToken,
) -> (Part, bool) {
    let result = |out: &ToolOutput| Part::ToolResult {
        call_id: id.to_string(),
        name: Some(name.to_string()),
        content: vec![Part::text(if out.content.is_empty() { "(no output)".to_string() } else { out.content.clone() })],
        is_error: out.is_error,
    };
    let output = match verdict {
        Verdict::Denied(reason) => ToolOutput::error(format!("Permission denied: {reason}")),
        Verdict::Run => {
            let tool = tools.get(name).cloned();
            let Some(tool) = tool else {
                let out = ToolOutput::error(format!("unknown tool `{name}`"));
                emit(events, AgentEvent::ToolDone { id: id.into(), output: out.clone(), elapsed_ms: 0 });
                return (result(&out), false);
            };
            // what a file looked like before the agent writes it, for a rewind
            if tool.kind() == ToolKind::Write {
                if let Some(p) = args.get("path").and_then(Value::as_str) {
                    let path = if Path::new(p).is_absolute() { PathBuf::from(p) } else { cwd.join(p) };
                    let before = std::fs::read(&path).ok();
                    let mut st = state.lock().await;
                    let at = st.history.len();
                    st.checkpoints.push((at, path, before));
                }
            }
            // a pre_tool hook may stop the call, with a reason the model reads
            let (hooks, hook_cwd) = {
                let st = state.lock().await;
                (st.config.hooks.clone(), st.cwd.clone())
            };
            if !hooks.is_empty() {
                let payload = serde_json::json!({"event": "pre_tool", "tool": name, "args": args});
                let o = crate::hooks::run(&hooks, "pre_tool", Some(name), &payload, &hook_cwd).await;
                for f in &o.failures {
                    notice(events, Level::Warn, f.clone());
                }
                if let Some(why) = o.blocked {
                    let out = ToolOutput::error(format!("Blocked by a hook: {why}"));
                    emit(events, AgentEvent::ToolDone { id: id.into(), output: out.clone(), elapsed_ms: 0 });
                    return (result(&out), false);
                }
            }
            emit(events, AgentEvent::ToolStart { id: id.into() });
            let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel::<ToolProgress>();
            let fwd_events = events.clone();
            let forwarder = tokio::spawn(async move {
                while let Some(p) = prx.recv().await {
                    emit(&fwd_events, AgentEvent::ToolProgress { id: p.call_id, line: p.line });
                }
            });
            // the rules as they stand now, for a tool that is led somewhere it was not asked about
            let (jobs, perm, rules_cwd) = {
                let st = state.lock().await;
                (st.jobs.clone(), st.perm.clone(), cwd.to_path_buf())
            };
            let ctx = ToolContext {
                cwd: cwd.to_path_buf(),
                session_id: session_id.to_string(),
                call_id: id.to_string(),
                cancel: cancel.child_token(),
                progress: Some(ptx),
                env: Default::default(),
                jobs,
                check: Some(Arc::new(move |tool: &str, kind, args: &Value| {
                    match perm.decide(tool, kind, args, &rules_cwd) {
                        Decision::Allow => nexo_core::tool::Check::Allow,
                        Decision::Ask => nexo_core::tool::Check::Ask,
                        Decision::Deny(_) => nexo_core::tool::Check::Deny,
                    }
                })),
            };
            let t0 = Instant::now();
            let out = if name == "recall" {
                crate::recall::answer(&state.lock().await.history, args)
            } else if name == "goal_brief" {
                goal_brief(state, args).await
            } else {
                tokio::select! {
                    o = tool.run(args.clone(), &ctx) => o,
                    _ = cancel.cancelled() => ToolOutput::error("interrupted by the person"),
                }
            };
            drop(ctx);
            let _ = forwarder.await;
            if name == "exit_plan" {
                let mut st = state.lock().await;
                let switch = st.plan.lock().unwrap_or_else(|e| e.into_inner()).switch.take();
                if let Some(mode) = switch {
                    set_mode(&mut st, mode);
                }
            }
            let elapsed_ms = t0.elapsed().as_millis() as u64;
            // the file the call printed (part of), if that is all it did
            let file = match (name, args.get("path").and_then(Value::as_str)) {
                ("read", Some(p)) => {
                    Some((if Path::new(p).is_absolute() { PathBuf::from(p) } else { cwd.join(p) }, p.to_string()))
                }
                ("bash", _) => {
                    args.get("command").and_then(Value::as_str).and_then(crate::reads::shell_print).map(|(rel, _)| {
                        (
                            if rel.is_absolute() { rel.clone() } else { cwd.join(&rel) },
                            rel.to_string_lossy().into_owned(),
                        )
                    })
                }
                _ => None,
            };
            let seen = {
                let mut st = state.lock().await;
                let _ = st.session.record_output(id, &out);
                match (name, &file) {
                    ("read", Some((path, p))) if !out.is_error => {
                        let offset = args.get("offset").and_then(Value::as_u64);
                        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(READ_LIMIT);
                        st.reads.after_read(path, p, offset, limit, id, crate::reads::printed(&out))
                    }
                    ("bash", Some((path, p))) if !out.is_error => {
                        let span = args.get("command").and_then(Value::as_str).and_then(crate::reads::shell_print);
                        let printed = crate::reads::printed(&out);
                        span.and_then(|(_, span)| st.reads.after_lines(path, p, span, id, printed))
                    }
                    _ => None,
                }
            };
            emit(events, AgentEvent::ToolDone { id: id.into(), output: out.clone(), elapsed_ms });
            if name == "todo" && !out.is_error {
                if let Some((done, total)) = todo_counts(args) {
                    emit(events, AgentEvent::TodoChanged { done, total });
                }
            }
            if !hooks.is_empty() {
                let payload = serde_json::json!({
                    "event": "post_tool", "tool": name, "args": args,
                    "output": out.content, "is_error": out.is_error, "elapsed_ms": elapsed_ms,
                });
                for f in crate::hooks::run(&hooks, "post_tool", Some(name), &payload, &hook_cwd).await.failures {
                    notice(events, Level::Warn, f);
                }
            }
            // the person sees the lines; the model, which has them, a note
            let sent = match seen {
                Some(note) => ToolOutput::ok(note),
                None if !out.is_error => match digest(state, events, name, args, file, &out, cancel).await {
                    Some(notes) => ToolOutput::ok(notes),
                    None => out,
                },
                None => out,
            };
            return (result(&sent), cancel.is_cancelled());
        }
    };
    emit(events, AgentEvent::ToolDone { id: id.into(), output: output.clone(), elapsed_ms: 0 });
    (result(&output), false)
}

/// With `[context] reader_at`, the notes a separate reader request takes from a long output (from the
/// whole file, when the call printed part of one), in the form the session gets them; None when the
/// output is short, too long for one request, or the reader failed (then the output goes as it is).
async fn digest(
    state: &Shared,
    events: &EventTx,
    name: &str,
    args: &Value,
    file: Option<(PathBuf, String)>,
    out: &ToolOutput,
    cancel: &CancellationToken,
) -> Option<String> {
    let (ask, client, target, retries, temperature) = {
        let st = state.lock().await;
        let at = st.config.context.reader_at;
        if at == 0 || out.content.len() as u64 / 4 <= at {
            return None;
        }
        let whole = file.and_then(|(path, shown)| {
            let text = std::fs::read_to_string(&path).ok()?;
            let lines = text.lines().count();
            (text.len() <= reader::MATERIAL_CHARS)
                .then(|| (format!("{shown}, whole file, {lines} lines"), reader::numbered(&text, 1)))
        });
        let (label, material) = match whole {
            Some(w) => w,
            None if out.content.len() <= reader::MATERIAL_CHARS => {
                (format!("output of {}", summary_of(&st.tools, name, args)), reader::numbered(&out.content, 1))
            }
            None => return None,
        };
        let ask = reader::Ask::from_history(&st.history, label, material);
        if let Some(notes) = st.digests.get(&ask.key()) {
            return Some(reader::shown(&ask.label, notes));
        }
        let (retries, temperature) = (st.config.agent.retries, st.config.agent.temperature);
        (ask, st.client.clone(), st.target.clone(), retries, temperature)
    };
    let done = tokio::select! {
        r = reader::read(&*client, &target, retries, temperature, &ask) => r,
        _ = cancel.cancelled() => return None,
    };
    let mut st = state.lock().await;
    match done {
        Ok((notes, usage, model)) => {
            add_usage(&mut st, events, &model, usage, Usage::default(), UsageKind::Reader);
            st.digests.insert(ask.key(), notes.clone());
            Some(reader::shown(&ask.label, &notes))
        }
        Err(e) => {
            notice(events, Level::Warn, format!("{e}; the output goes as it is"));
            None
        }
    }
}

/// Compacts the history now. `in_turn` keeps the request in progress whole.
/// Returns whether it happened.
async fn compact_now(state: &Shared, events: &EventTx, in_turn: Option<()>, cancel: &CancellationToken) -> bool {
    let mut guard = state.lock().await;
    let st = &mut *guard;
    if st.history.is_empty() {
        notice(events, Level::Info, "nothing to compact");
        return false;
    }
    let keep = match in_turn {
        Some(()) => match compact::keep_recent(&st.history) {
            Some(k) => k,
            None => {
                notice(events, Level::Info, "nothing to compact");
                return false;
            }
        },
        None => st.history.len(),
    };
    compact_locked(st, keep, cancel).await
}

async fn compact_locked(st: &mut State, keep_from: usize, cancel: &CancellationToken) -> bool {
    let s = Summariser {
        client: st.client.clone(),
        target: st.target.clone(),
        retries: st.config.agent.retries,
        max_tokens: SUMMARY_TOKENS,
        window: st.limits.context_window,
    };
    let before = st.history.len();
    let (tokens_before, events) = (st.history.tokens(), st.events.clone());
    notice(&events, Level::Info, "compacting the history…");
    let mut work = st.history.to_vec();
    let done = tokio::select! {
        r = compact::compact(&s, &mut work, keep_from) => r,
        _ = cancel.cancelled() => Err(anyhow::anyhow!("interrupted")),
    };
    match done {
        Ok(done) => {
            // the checkpoints count positions of the old history: those in the kept tail move with
            // it, and those of the summarised part go to 0, where no rewind puts them back (the
            // summary reports their writes as done)
            let from = keep_from.min(before);
            let shift = work.len().saturating_sub(before - from);
            for c in &mut st.checkpoints {
                c.0 = if c.0 >= from { c.0 - from + shift } else { 0 };
            }
            // the turn in progress starts where its first kept message now sits
            st.turn_start = if st.turn_start >= from { st.turn_start - from + shift } else { 0 };
            st.history.replace(work);
            // the failures it held are out of the history: a line pointing back at them would point at nothing
            st.repeats = Default::default();
            add_usage(st, &events, &done.model, done.usage, Usage::default(), UsageKind::Compact);
            st.last_prompt = None;
            if let Err(e) = st.session.reset_with(&st.history.to_vec()) {
                notice(&events, Level::Warn, format!("cannot save the session: {e}"));
            }
            st.policy.compacted();
            emit(
                &events,
                AgentEvent::Compacted {
                    before_tokens: tokens_before.min(u32::MAX as u64) as u32,
                    after_tokens: st.history.tokens().min(u32::MAX as u64) as u32,
                },
            );
            notice(
                &events,
                Level::Info,
                format!(
                    "history compacted: {} messages, ~{} tokens → {} messages, ~{} tokens",
                    before,
                    tokens_before,
                    st.history.len(),
                    st.history.tokens()
                ),
            );
            true
        }
        Err(e) => {
            notice(&events, Level::Error, format!("compaction failed: {e}"));
            false
        }
    }
}

async fn set_model(state: &Shared, provider: Option<String>, model: String) {
    let mut st = state.lock().await;
    let provider = provider.unwrap_or_else(|| st.target.provider());
    match st.config.providers.get(&provider).cloned() {
        None => notice(&st.events, Level::Error, format!("unknown provider `{provider}` (see `nexo providers`)")),
        Some(cfg) => {
            st.limits = limits::resolve(&provider, &cfg, &model, None);
            // another model may count with another tokenizer
            st.tok_ratio = None;
            st.target.set(&provider, cfg, &model);
            let _ = st.session.record_model(&provider, &model);
            emit(&st.events, AgentEvent::ModelChanged { provider: provider.clone(), model: model.clone() });
            emit(&st.events, st.limits.event(&provider, &model));
            emit(&st.events, effort_event(&st));
            drop(st);
            spawn_vendor_fill(state);
        }
    }
}

fn budget_event(st: &State, day: f64) -> AgentEvent {
    AgentEvent::Budget {
        session_cost: st.cost,
        day_cost: day,
        session_limit: st.budget,
        day_limit: st.config.agent.max_cost_day,
    }
}

/// `/budget`: what was spent and what may be.
fn budget_text(st: &State, day: f64, changed: bool) -> String {
    let limit = |l: Option<f64>| l.map(|l| format!(" of ${l:.2}")).unwrap_or_else(|| " (no limit)".into());
    let head = if changed { "spend limit set" } else { "spending" };
    format!(
        "{head}\nthis session ${:.4}{}\ntoday        ${day:.4}{}\n`/budget <dollars>` sets this session's limit, `/budget off` lifts it; [agent] max_cost and max_cost_day keep them",
        st.cost,
        limit(st.budget),
        limit(st.config.agent.max_cost_day)
    )
}

/// `/status`: what nexo is running with, one fact a line.
fn status_text(st: &State) -> String {
    let (provider, cfg, model) = st.target.get();
    let key = match (&cfg.api_key_env, &cfg.api_key_cmd) {
        _ if cfg.api_key_file.is_some() => {
            format!("from the file {}", cfg.api_key_file.as_ref().map(|f| f.display().to_string()).unwrap_or_default())
        }
        (_, Some(c)) => format!("from the command `{c}`"),
        (Some(v), None) if std::env::var(v).is_ok_and(|k| !k.trim().is_empty()) => format!("${v} ✓"),
        (Some(v), None) => format!("${v} is not set"),
        (None, None) => "none needed".into(),
    };
    let user = nexo_core::paths::user_config_file();
    let project = nexo_core::paths::project_config_file(&st.cwd);
    let short = nexo_core::paths::shown;
    let file = |p: &Path| format!("{}{}", short(p), if p.is_file() { "" } else { " (none)" });
    let effort = setup::effort_of(&cfg, &model, &st.config.agent.reasoning).unwrap_or_else(|| "default".into());
    let mut lines = vec![
        format!(
            "nexo {} · {}{}",
            nexo_core::version::get(),
            std::env::consts::OS,
            if nexo_core::paths::is_root() { " · running as root" } else { "" }
        ),
        format!("shell        {} ({})", nexo_core::shell::shell().kind, nexo_core::shell::shell().program.display()),
        format!("model        {provider}/{model} · effort {effort}"),
        format!("endpoint     {} ({})", cfg.base_url, cfg.protocol),
        format!("key          {key}"),
        {
            let (label, why) = crate::context::describe(st.policy.as_ref(), st.last_view.0, st.last_view.1);
            format!("context      {label} · {} messages in memory\n             {why}", st.history.len())
        },
        format!("permissions  {}", st.perm.mode.as_str()),
        format!("session      {} ({})", st.session.id(), short(st.session.path())),
        format!("directory    {}", short(&st.cwd)),
        format!("config       {}", file(&user)),
    ];
    if let Some(p) = project {
        lines.push(format!("             {}", file(&p)));
    }
    lines.push(format!("tools        {} (/tools lists them)", st.tools.specs().len()));
    lines.join("\n")
}

/// `/tools`: the tools by kind.
fn jobs_text(jobs: &Jobs) -> String {
    let all = jobs.list();
    if all.is_empty() {
        return "no background jobs".into();
    }
    let mut lines = vec![format!("{} ({} running)", nexo_core::count(all.len(), "background job"), jobs.running())];
    for j in all {
        let first: String = j.label.lines().next().unwrap_or("").chars().take(70).collect();
        lines.push(format!(
            "  {:>3}  {:<14} {:>10}  {}",
            j.id,
            j.status.plain(),
            nexo_core::jobs::duration(j.elapsed_ms),
            first
        ));
    }
    lines.push("stop one with /jobs kill <id>, all with /jobs kill".into());
    lines.join("\n")
}

fn tools_text(tools: &ToolSet) -> String {
    let mut by_kind: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
    for spec in tools.specs() {
        let kind = tools.get(&spec.name).map(|t| t.kind().label()).unwrap_or("Other");
        by_kind.entry(kind).or_default().push(spec.name.clone());
    }
    let mut lines = vec![format!("{} tools", tools.specs().len())];
    for (k, names) in by_kind {
        lines.push(format!("{k:<13} {}", names.join(", ")));
    }
    lines.join("\n")
}

/// The reasoning level of the model in use and the levels it takes.
fn effort_event(st: &State) -> AgentEvent {
    let (_, cfg, model) = st.target.get();
    AgentEvent::Effort {
        current: setup::effort_of(&cfg, &model, &st.config.agent.reasoning),
        levels: cfg.efforts.get(&model).cloned().unwrap_or_default(),
        model,
    }
}

/// Sets the reasoning level of the model in use and remembers it in the user's configuration.
async fn set_effort(state: &Shared, effort: Option<String>) {
    let mut st = state.lock().await;
    let (provider, mut cfg, model) = st.target.get();
    let effort = effort.map(|e| e.trim().to_ascii_lowercase()).filter(|e| !e.is_empty());
    let levels = cfg.efforts.get(&model).cloned().unwrap_or_default();
    if let Some(e) = &effort {
        let known = levels.iter().any(|l| l == e) || (levels.is_empty() && reasoning_of(&Some(e.clone())).is_some());
        if !known {
            let take = if levels.is_empty() {
                "off, minimal, low, medium, high or a token budget".to_string()
            } else {
                levels.join(", ")
            };
            notice(&st.events, Level::Warn, format!("{model} does not take `{e}`: {take}"));
            return;
        }
    }
    let value = effort.clone().unwrap_or_default();
    cfg.effort.insert(model.clone(), value.clone());
    if let Some(c) = st.config.providers.get_mut(&provider) {
        c.effort.insert(model.clone(), value.clone());
    }
    st.target.set(&provider, cfg, &model);
    let file = nexo_core::paths::user_config_file();
    if let Err(e) =
        config_edit::set(&file, &["providers", &provider, "effort"], &model, toml_edit::Value::from(value.as_str()))
    {
        notice(&st.events, Level::Warn, format!("effort set for this session only ({e})"));
    }
    emit(&st.events, effort_event(&st));
}

/// Saves what the setup found for a model and switches to it.
async fn save_model(state: &Shared, name: String, cfg: ProviderConfig, model: String, key: Option<String>) {
    let mut cfg = cfg;
    let mut kept = None;
    {
        let st = state.lock().await;
        if let Some(k) = key.as_deref().filter(|k| !k.trim().is_empty()) {
            match setup::keep_key(&name, k, &mut cfg) {
                Ok(f) => kept = Some(f),
                Err(e) => notice(&st.events, Level::Warn, format!("the key could not be kept: {e}")),
            }
        }
    }
    let mut st = state.lock().await;
    // what was there stays (other models, headers, prices); what the setup found wins
    let mut merged = st.config.providers.get(&name).cloned().unwrap_or_default();
    merged.protocol = cfg.protocol.clone();
    merged.base_url = cfg.base_url.clone();
    if cfg.api_key_env.is_some() || cfg.api_key_cmd.is_some() || cfg.api_key_file.is_some() {
        merged.api_key_env = cfg.api_key_env.clone();
        merged.api_key_cmd = cfg.api_key_cmd.clone();
        merged.api_key_file = cfg.api_key_file.clone();
    }
    for (k, v) in &cfg.headers {
        merged.headers.insert(k.clone(), v.clone());
    }
    if !merged.models.contains(&model) {
        merged.models.push(model.clone());
    }
    merged.default_model = Some(model.clone());
    for (map, from) in [(&mut merged.context_window, &cfg.context_window), (&mut merged.max_output, &cfg.max_output)] {
        if let Some(n) = from.get(&model) {
            map.insert(model.clone(), *n);
        }
    }
    if let Some(p) = cfg.pricing.get(&model) {
        merged.pricing.insert(model.clone(), *p);
    }
    if let Some(l) = cfg.efforts.get(&model) {
        merged.efforts.insert(model.clone(), l.clone());
    }
    if let Some(e) = cfg.effort.get(&model) {
        merged.effort.insert(model.clone(), e.clone());
    }
    let file = nexo_core::paths::user_config_file();
    match setup::save(&file, &name, &merged, &model) {
        Ok(()) => {
            let key_note =
                kept.map(|f| format!("; the key is in {} (only you can read it)", f.display())).unwrap_or_default();
            notice(&st.events, Level::Info, format!("{name}/{model} saved to {}{key_note}", file.display()));
        }
        Err(e) => notice(&st.events, Level::Warn, format!("{name}/{model} is set for this session only ({e})")),
    }
    st.config.providers.insert(name.clone(), merged);
    st.config.provider = Some(name.clone());
    st.config.model = Some(model.clone());
    drop(st);
    set_model(state, Some(name), model).await;
}

/// Asks the vendor for its model list in the background and fills what the
/// limits still lack (context window, largest reply, prices).
/// Weighs every background shell command that runs past `job_review` seconds with one tool-less request apart
/// from the conversation, which never waits for it: STOP ends the job (the model learns why from its last
/// line), anything else lets it run and looks again at twice the time. Ends with the session.
fn spawn_job_review(state: &Shared, jobs: Jobs) {
    let weak = Arc::downgrade(state);
    tokio::spawn(async move {
        // job -> (seconds of the next look, lines it had at the last one)
        let mut due: HashMap<u32, (u64, usize)> = HashMap::new();
        // the first look comes soon, to read the setting
        let mut tick = 1;
        loop {
            tokio::time::sleep(Duration::from_secs(tick)).await;
            let Some(state) = weak.upgrade() else { return };
            let (first, client, target, retries, request, events) = {
                let st = state.lock().await;
                let a = &st.config.agent;
                (
                    a.job_review as u64,
                    st.client.clone(),
                    st.target.clone(),
                    a.retries,
                    crate::review::request_of(&st.history),
                    st.events.clone(),
                )
            };
            drop(state);
            tick = crate::review::TICK_S.min(first.max(1));
            let running: Vec<_> = jobs
                .list()
                .into_iter()
                .filter(|j| j.kind == nexo_core::jobs::JobKind::Shell && j.status.running())
                .collect();
            due.retain(|id, _| running.iter().any(|j| j.id == *id));
            if first == 0 {
                continue;
            }
            let tails = jobs.tails();
            for j in running {
                let secs = j.elapsed_ms / 1000;
                let (next, seen) = *due.entry(j.id).or_insert((first, 0));
                if secs < next {
                    continue;
                }
                let tail = tails.iter().find(|t| t.id == j.id).map(|t| t.text.as_str()).unwrap_or("");
                let ask = crate::review::prompt(&j.label, secs, j.lines, j.lines.saturating_sub(seen), tail, &request);
                match crate::review::ask(&*client, &target, retries, ask).await {
                    Ok((stop, usage, model)) => {
                        let Some(state) = weak.upgrade() else { return };
                        add_usage(
                            &mut *state.lock().await,
                            &events,
                            &model,
                            usage,
                            Usage::default(),
                            UsageKind::Review,
                        );
                        if stop && jobs.stop(j.id, "a review judged it no longer worth waiting for") {
                            let took = nexo_core::jobs::duration(j.elapsed_ms);
                            notice(&events, Level::Info, format!("job {} stopped by the review after {took}", j.id));
                            continue;
                        }
                    }
                    Err(e) => notice(&events, Level::Warn, format!("the review of job {} failed: {e}", j.id)),
                }
                due.insert(j.id, (secs.max(1) * 2, j.lines));
            }
        }
    });
}

/// Sent after the last request of the conversation and its reply, to ask for a prompt suggestion.
const SUGGEST_ASK: &str = "Suggest the next message the user is most likely to send, in their language and voice, \
     at most 12 words. Reply with only that message, or with nothing if unclear.";

/// The longest reply of a prompt suggestion's request.
const SUGGEST_TOKENS: u32 = 200;

/// The request of a prompt suggestion: the last request of the conversation as it was sent (the same system
/// prompt, tools, tool choice and messages, so the provider's cache of it is hit), the reply it got, and one
/// more message from the person that asks for the suggestion.
fn suggest_request(last: &ModelRequest, reply: Message, levels: &[String]) -> ModelRequest {
    let mut req = last.clone();
    req.messages.push(reply);
    req.messages.push(Message::user(SUGGEST_ASK));
    // a guess of a dozen words: no thinking first (or the least the model takes, where off is not a level it
    // has), and a cap that keeps it cheap on any model
    let has = |l: &str| levels.iter().any(|x| x == l);
    req.reasoning = Some(if has("minimal") && !has("off") && !has("none") {
        ReasoningConfig::Effort(Effort::Minimal)
    } else if has("low") && !has("off") && !has("none") {
        ReasoningConfig::Effort(Effort::Low)
    } else {
        ReasoningConfig::Off
    });
    req.max_tokens = SUGGEST_TOKENS;
    req
}

#[cfg(test)]
mod suggest_tests {
    use super::*;

    #[test]
    fn the_suggestion_asks_for_the_least_thinking_the_model_takes() {
        let last = ModelRequest::new("m", "", vec![]);
        let lv = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let r = |l: &[&str]| suggest_request(&last, Message::assistant("ok"), &lv(l)).reasoning;
        assert_eq!(r(&[]), Some(ReasoningConfig::Off));
        assert_eq!(r(&["off", "low", "high"]), Some(ReasoningConfig::Off));
        assert_eq!(r(&["minimal", "low", "medium", "high"]), Some(ReasoningConfig::Effort(Effort::Minimal)));
        assert_eq!(r(&["low", "high"]), Some(ReasoningConfig::Effort(Effort::Low)));
        assert_eq!(suggest_request(&last, Message::assistant("ok"), &[]).max_tokens, SUGGEST_TOKENS);
    }
}

/// A suggestion as one line without the quotes a model puts round it; None when it holds nothing.
fn tidy_suggestion(text: &str) -> Option<String> {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = line.trim_matches(|c| matches!(c, '"' | '\'' | '`' | '“' | '”')).trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// `AgentCommand::Suggest`: a guess at the person's next message, in a task of its own so the chat never waits
/// for it. It is let go when the reply calls a tool or holds nothing, or when a new request started (or the
/// conversation changed) while the call ran.
fn spawn_suggest(state: &Shared) {
    let state = state.clone();
    tokio::spawn(async move {
        let (req, cfg, client, retries, turns, events) = {
            let st = state.lock().await;
            let Some(last) = st.last_request.as_ref() else { return };
            // the conversation ends on a reply that is words only, from the model that got that request
            let Some(reply) =
                st.history.last().filter(|m| m.role == Role::Assistant && m.tool_calls().next().is_none())
            else {
                return;
            };
            let (_, cfg, model) = st.target.get();
            // a model that always reasons first would spend the whole cap thinking and say nothing
            let thinks = model.split(['/', '-', ':']).any(|w| matches!(w, "reasoner" | "r1" | "o1" | "o3" | "o4"))
                || ["grok-4", "gpt-5", "codex"].iter().any(|m| model.contains(m));
            if last.model != model || !st.inbox.is_empty() || thinks {
                return;
            }
            let req = suggest_request(last, reply.clone(), cfg.efforts.get(&model).map(Vec::as_slice).unwrap_or(&[]));
            (req, cfg, st.client.clone(), st.config.agent.retries, (st.turns, st.epoch), st.events.clone())
        };
        let Ok(mut stream) = client.stream(&cfg, &req, retries).await else { return };
        let mut c = Collector::for_model(req.model.clone());
        while let Some(ev) = stream.next().await {
            let failed = matches!(ev, StreamEvent::Error(_));
            c.push(ev);
            if failed {
                break;
            }
        }
        let msg = c.message();
        let mut st = state.lock().await;
        if !c.usage.is_empty() {
            add_usage(&mut st, &events, &req.model, c.usage, Usage::default(), UsageKind::Suggest);
        }
        if c.error.is_some() || msg.tool_calls().next().is_some() || (st.turns, st.epoch) != turns {
            return;
        }
        if let Some(text) = tidy_suggestion(&msg.text()) {
            emit(&events, AgentEvent::Suggestion { text });
        }
    });
}

fn spawn_vendor_fill(state: &Shared) {
    let state = state.clone();
    tokio::spawn(async move {
        let (provider, cfg, model, client, incomplete) = {
            let st = state.lock().await;
            let (p, c, m) = st.target.get();
            (p, c, m, st.client.clone(), st.limits.incomplete())
        };
        if !incomplete {
            return;
        }
        let list = match tokio::time::timeout(Duration::from_secs(15), client.list_models(&cfg)).await {
            Ok(Ok(l)) => l,
            _ => return,
        };
        let mut st = state.lock().await;
        let (p, c, m) = st.target.get();
        if p != provider || m != model {
            return;
        }
        let l = limits::resolve(&p, &c, &m, Some(&list));
        if l != st.limits {
            st.limits = l;
            emit(&st.events, st.limits.event(&p, &m));
        }
    });
}

/// Remembers the prices of the model in use in the user's configuration.
async fn set_pricing(state: &Shared, pricing: Pricing) {
    let mut st = state.lock().await;
    let (provider, mut cfg, model) = st.target.get();
    let file = nexo_core::paths::user_config_file();
    let saved = config_edit::set_pricing(&file, &provider, &model, pricing);
    cfg.pricing.insert(model.clone(), pricing);
    if let Some(c) = st.config.providers.get_mut(&provider) {
        c.pricing.insert(model.clone(), pricing);
    }
    st.target.set(&provider, cfg, &model);
    st.limits.pricing = Some(pricing);
    st.limits.source = "config".into();
    emit(&st.events, st.limits.event(&provider, &model));
    match saved {
        Ok(()) => notice(
            &st.events,
            Level::Info,
            format!(
                "prices of {model} saved to {}: in {} / out {} / cache {} per million",
                file.display(),
                pricing.input,
                pricing.output,
                pricing.cache_read
            ),
        ),
        Err(e) => notice(&st.events, Level::Warn, format!("prices of {model} set for this session only ({e})")),
    }
}

/// What a provider's error taught about the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lesson {
    None,
    /// The request is longer than the window (compaction is the answer).
    Context,
    /// The request is longer than the window, which was just learned smaller than thought, and the
    /// policy keeps its view under the window: a new view may fit with the history as it is.
    Refit,
    /// `max_tokens` was above the largest reply; it is capped now (retry).
    Output,
    UnknownModel,
}

/// Reads the error for what it says about the model and remembers it:
/// the context window, the largest reply, or that the model does not exist.
async fn learn(state: &Shared, e: &ProviderError, model: &str) -> Lesson {
    let mut guard = state.lock().await;
    let st = &mut *guard;
    let (provider, mut cfg, _) = st.target.get();
    if let Some(n) = hints::context_window(&e.message) {
        let before = st.limits.context_window;
        remember(st, &provider, &mut cfg, model, "context_window", n);
        if before.is_none_or(|b| n < b) && st.policy.fits_window() {
            return Lesson::Refit;
        }
        return Lesson::Context;
    }
    if e.kind == ErrorKind::ContextLength {
        return Lesson::Context;
    }
    if let Some(n) = hints::max_output(&e.message) {
        remember(st, &provider, &mut cfg, model, "max_output", n);
        return Lesson::Output;
    }
    if hints::unknown_model(e) {
        let mut names: Vec<String> = cfg.models.clone();
        if let Some(c) = catalogue::cached() {
            names.extend(c.model_names(&provider));
        }
        let close = hints::closest(model, names.iter().map(String::as_str), 5);
        let hint =
            if close.is_empty() { String::new() } else { format!("; closest: {} (`/model <name>`)", close.join(", ")) };
        notice(&st.events, Level::Warn, format!("{provider} does not know the model `{model}`{hint}"));
        return Lesson::UnknownModel;
    }
    Lesson::None
}

/// Stores a limit learned from an error in the running configuration and
/// the user's file, and tells the interface.
fn remember(st: &mut State, provider: &str, cfg: &mut ProviderConfig, model: &str, which: &str, n: u32) {
    let file = nexo_core::paths::user_config_file();
    let saved = config_edit::set_limit(&file, provider, model, which, n);
    let window = which == "context_window";
    let map = if window { &mut cfg.context_window } else { &mut cfg.max_output };
    map.insert(model.to_string(), n);
    if let Some(c) = st.config.providers.get_mut(provider) {
        let map = if window { &mut c.context_window } else { &mut c.max_output };
        map.insert(model.to_string(), n);
    }
    st.target.set(provider, cfg.clone(), model);
    if window {
        st.limits.context_window = Some(n);
    } else {
        st.limits.max_output = Some(n);
    }
    st.limits.source = "error".into();
    emit(&st.events, st.limits.event(provider, model));
    let what = if window { "context window" } else { "largest reply" };
    match saved {
        Ok(()) => notice(
            &st.events,
            Level::Info,
            format!(
                "{what} of {model} set to {n} tokens (learned from the provider's error; saved to {})",
                file.display()
            ),
        ),
        Err(e) => notice(
            &st.events,
            Level::Warn,
            format!("{what} of {model} set to {n} tokens for this session (could not save it: {e})"),
        ),
    }
}

/// The error as shown, with the wait a rate limit asked for.
fn explain(e: &ProviderError) -> String {
    match hints::wait_hint(e) {
        Some(h) => format!("{} ({h})", e.human()),
        None => e.human(),
    }
}

async fn clear(state: &Shared) {
    let mut st = state.lock().await;
    let (provider, _, model) = st.target.get();
    st.session = match Session::create(&st.cwd, &provider, &model) {
        Ok(s) => s,
        Err(e) => {
            notice(&st.events, Level::Warn, format!("the session is not being saved: {e}"));
            Session::ephemeral(&st.cwd, &provider, &model)
        }
    };
    st.history.clear();
    st.checkpoints.clear();
    st.policy.reset();
    st.last_prompt = None;
    st.last_request = None;
    st.repeats = Default::default();
    st.epoch += 1;
    st.reads.clear();
    st.digests.clear();
    st.inbox.clear();
    st.usage = Usage::default();
    st.cost = 0.0;
    st.turn_start = 0;
    st.turns = 0;
    let had_goal = st.goal.take().is_some();
    st.goal_round = 0;
    st.goal_state = GoalState::Active;
    if had_goal {
        emit_goal(&st);
    }
    emit(
        &st.events,
        AgentEvent::Ready {
            session_id: st.session.id().to_string(),
            provider,
            model,
            cwd: st.cwd.clone(),
            permission_mode: st.perm.mode,
        },
    );
    emit(&st.events, AgentEvent::HistoryCleared);
    emit(&st.events, AgentEvent::SessionChanged { session_id: st.session.id().to_string(), title: String::new() });
    mail_sync(&st);
}

/// A `!` command: run outside the model's view, its output shown.
fn run_shell(cwd: PathBuf, command: String, events: EventTx) {
    tokio::spawn(async move {
        let shell = nexo_core::shell::shell();
        let run =
            tokio::process::Command::new(&shell.program).args(shell.args_for(&command)).current_dir(&cwd).output();
        let (output, exit_code) = match tokio::time::timeout(Duration::from_secs(120), run).await {
            Ok(Ok(o)) => {
                let mut s = String::from_utf8_lossy(&o.stdout).to_string();
                let err = String::from_utf8_lossy(&o.stderr);
                if !err.trim().is_empty() {
                    if !s.is_empty() && !s.ends_with('\n') {
                        s.push('\n');
                    }
                    s.push_str(&err);
                }
                (s, o.status.code())
            }
            Ok(Err(e)) => (format!("cannot run: {e}"), None),
            Err(_) => ("timed out after 120 s".to_string(), None),
        };
        let output = if output.chars().count() > SHELL_CHARS {
            let head: String = output.chars().take(SHELL_CHARS / 2).collect();
            let tail: String =
                output.chars().rev().take(SHELL_CHARS / 2).collect::<Vec<_>>().into_iter().rev().collect();
            format!("{head}\n[... output shortened ...]\n{tail}")
        } else {
            output
        };
        emit(&events, AgentEvent::ShellOutput { command, output, exit_code });
    });
}

#[cfg(test)]
mod goal_tests {
    use super::*;

    #[test]
    fn the_view_window_follows_the_providers_count() {
        // before any count: 15% to spare
        assert_eq!(view_window(10_000, None), 8_500);
        // code counted 1.4 tokens an estimated token: the view gets 0.95/1.4 of the room
        let r = learn_ratio(None, 12_827, 18_158);
        assert!((r.unwrap() - 1.4156).abs() < 0.001);
        assert_eq!(view_window(10_000, r), 6_710);
        // a larger ratio is taken at once, a smaller one slowly
        let up = learn_ratio(Some(1.2), 10_000, 15_000).unwrap();
        assert!((up - 1.5).abs() < 1e-9);
        let down = learn_ratio(Some(1.5), 10_000, 10_000).unwrap();
        assert!((down - 1.4).abs() < 1e-9);
        // tiny requests and missing counts teach nothing; wild counts are bounded
        assert_eq!(learn_ratio(Some(1.3), 200, 900), Some(1.3));
        assert_eq!(learn_ratio(None, 10_000, 0), None);
        assert_eq!(learn_ratio(None, 1_000, 90_000), Some(3.0));
    }

    #[test]
    fn goal_done_reads_the_last_line() {
        assert!(goal_done("all fixed\n\nGOAL DONE\n"));
        assert!(goal_done("done.\ngoal done."));
        assert!(goal_done("all set\nGOAL DONE. Thanks for the task!"), "the words in the tail count");
        assert!(!goal_done(&format!("GOAL DONE is what I will say later\n{}", "still working. ".repeat(20))));
        assert!(!goal_done(""));
    }

    #[test]
    fn goal_wait_reads_a_question_or_the_wait_line() {
        assert!(goal_wait("Which branch should I use?"));
        assert!(goal_wait("Two ways.\n\n**Should I drop the old table?**\n"));
        assert!(goal_wait("¿Lo borro (`rm -rf build`)?"));
        assert!(goal_wait("I need the staging key.\nGOAL WAIT"));
        assert!(goal_wait("I need the staging key.\n\nGOAL WAIT. I will go on once I have it."));
        assert!(!goal_wait("Fixed it. Want more? Not needed: all tests pass."));
        assert!(!goal_wait("```\nwhat?\n```"), "a fence closes the reply, not a question");
        assert!(!goal_wait("all set\nGOAL DONE"));
        assert!(!goal_wait(""));
    }

    #[test]
    fn fresh_lines_tells_an_output_that_brings_a_line_not_seen_before() {
        let out = |text: &str| vec![Part::tool_result("c", text, false)];
        let mut known = known_lines(&[Message::tool_results(out("fn main() {\n    run();\n}"))]);
        // a slice of what was already read, other spacing aside, brings nothing
        assert_eq!(fresh_lines(&mut known, &out("run();\n}")), Some(false));
        // one new line is enough, and it is known from then on
        assert_eq!(fresh_lines(&mut known, &out("fn main() {\nfn exit() {")), Some(true));
        assert_eq!(fresh_lines(&mut known, &out("fn exit() {")), Some(false));
        // blank lines and short markers tell nothing: the round brought no line at all
        assert_eq!(fresh_lines(&mut known, &out("\n}\nok\n...")), None);
    }

    #[test]
    fn looping_sees_a_block_of_rounds_that_brings_nothing_new() {
        // the same round three times: a command run over and over with the same output
        assert_eq!(looping(&[9, 1, 1, 1], 3), Some(1));
        assert_eq!(looping(&[9, 1, 1], 3), None, "twice is not yet a loop");
        // a cycle of three reads that comes back three times
        assert_eq!(looping(&[5, 1, 2, 3, 1, 2, 3, 1, 2, 3], 3), Some(3));
        assert_eq!(looping(&[1, 2, 3, 1, 2, 3, 1, 2], 3), None, "the third time is not over");
        assert_eq!(looping(&[1, 2, 3, 1, 2, 4, 1, 2, 3], 3), None, "an output that changed is progress");
        assert_eq!(looping(&[1, 1, 1, 1], 0), None, "0 turns the guard off");
        // rounds differ by what came back, not by the call ids
        let ask = vec![("bash".to_string(), "{\"command\":\"make test\"}".to_string())];
        let out = |id: &str, text: &str| vec![Part::tool_result(id, text, false)];
        assert_eq!(round_hash(&ask, &out("a", "1 failed")), round_hash(&ask, &out("b", "1 failed")));
        assert_ne!(round_hash(&ask, &out("a", "1 failed")), round_hash(&ask, &out("a", "all passed")));
    }

    #[test]
    fn rounds_default_and_parse() {
        assert_eq!(parse_rounds(None), GOAL_ROUNDS);
        assert_eq!(parse_rounds(Some(" 2 ".into())), 2);
        assert_eq!(parse_rounds(Some("many".into())), GOAL_ROUNDS);
    }

    #[test]
    fn the_goal_is_pinned_at_the_end() {
        assert_eq!(with_goal("base", None), "base");
        let s = with_goal("base", Some("ship it"));
        assert!(s.starts_with("base\n\nGoal, in the person's words: ship it\n") && s.contains("single line GOAL DONE"));
        assert!(s.contains("single line GOAL WAIT") && s.contains("call `goal_brief` once"));
        // once restated, the restatement is the goal and nothing asks for it again
        let s = with_goal("base", Some("Want: a\nWhy: b\nHow: c\nDone when: d"));
        assert!(
            s.starts_with("base\n\nGoal, as the person means it:\nWant: a\n") && !s.contains("call `goal_brief` once")
        );
        assert!(s.contains("`[goal: continue]` comes from the loop"));
    }
}

#[cfg(test)]
mod detail_tests {
    use super::*;
    use serde_json::json;

    fn start_of(d: &ToolDisplay) -> usize {
        match d {
            ToolDisplay::Diff { start_line, .. } => *start_line,
            other => panic!("not a diff: {other:?}"),
        }
    }

    #[test]
    fn an_edit_preview_starts_at_the_line_of_the_text_it_replaces() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "one\ntwo\r\nthree\nfour\n").unwrap();
        let call = |old: &str| json!({"path": "a.rs", "old_string": old, "new_string": "x"});
        assert_eq!(start_of(&detail_of(ToolKind::Write, &call("three\nfour"), d.path())), 3);
        assert_eq!(start_of(&detail_of(ToolKind::Write, &call("two\r\nthree"), d.path())), 2);
        // a text the file does not hold, or a file that is not there: from the top
        assert_eq!(start_of(&detail_of(ToolKind::Write, &call("zzz"), d.path())), 1);
        let gone = json!({"path": "nope.rs", "old_string": "a", "new_string": "b"});
        assert_eq!(start_of(&detail_of(ToolKind::Write, &gone, d.path())), 1);
        // what is not a plain file, or is past the size the edit tool takes, is not read
        let dir = json!({"path": ".", "old_string": "a", "new_string": "b"});
        assert_eq!(start_of(&detail_of(ToolKind::Write, &dir, d.path())), 1);
        let mut big = std::fs::File::create(d.path().join("big.log")).unwrap();
        std::io::Write::write_all(&mut big, b"a\nb\nTARGET\n").unwrap();
        big.set_len(LINE_LOOKUP_MAX + 1).unwrap(); // sparse
        let target = json!({"path": "big.log", "old_string": "TARGET", "new_string": "b"});
        assert_eq!(start_of(&detail_of(ToolKind::Write, &target, d.path())), 1);
        // a whole file written over another is shown whole
        let whole = json!({"path": "a.rs", "content": "new"});
        assert_eq!(start_of(&detail_of(ToolKind::Write, &whole, d.path())), 1);
    }
}
