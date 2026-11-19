//! Part of the agent loop; see the parent module.
//!
//! What the loop holds while the session lives, and how it is built from the setup before the first command: the
//! tools the agent adds, the session opened or resumed, the events that tell the interface where it starts, the
//! shared state, and the tasks that start with the session.

use super::*;

/// Everything the loop's branches act on besides the command channel.
pub(super) struct Runtime {
    pub(super) state: Shared,
    pub(super) events: EventTx,
    /// Answers the permission asks the interface resolves; a headless run gives its own gate to the state.
    pub(super) chan_gate: Arc<ChannelGate>,
    pub(super) asker: Arc<crate::ask::ChannelAsker>,
    /// No interface answers: the agent asks no questions and permission goes through the gate it was given.
    pub(super) headless: bool,
    /// What a workflow the person starts by name runs its sub-agents with.
    pub(super) flow_sub: Option<Arc<crate::subagent::SubAgent>>,
    pub(super) jobs: Jobs,
    pub(super) detach: nexo_core::jobs::Detach,
    pub(super) job_changes: tokio::sync::broadcast::Receiver<nexo_core::jobs::JobChange>,
    pub(super) sub_changes: tokio::sync::broadcast::Receiver<nexo_core::subs::SubChange>,
    /// The registry lives as long as `jobs`, so its channel does not close; if it ever did, the branch stops
    /// (a closed channel answers at once, and the loop would spin).
    pub(super) subs_open: bool,
    /// The write of the context policy's state a resume's replay or a turn's end started (`saved_state`).
    pub(super) saving: Option<std::thread::JoinHandle<()>>,
    pub(super) turn: Option<JoinHandle<TurnEnd>>,
    /// A sign-in waiting for the browser, so Esc in the setup can drop it.
    pub(super) signing_in: Option<tokio::task::AbortHandle>,
    pub(super) checking: Option<tokio::task::AbortHandle>,
    pub(super) cancel: CancellationToken,
    /// The running turn was stopped to run the queued request now, not to wait for the person.
    pub(super) run_queue: bool,
    /// When the person stopped the running turn (Esc or Enter on a queued request), until its end records it.
    pub(super) stopped: Option<SystemTime>,
    /// Who or what stopped it (`stopped` is set along with it).
    pub(super) stopper: Option<nexo_core::agent::Stopper>,
    /// What a check-in decided, back to this loop (the only place a turn starts).
    pub(super) resume_tx: tokio::sync::mpsc::UnboundedSender<Resume>,
    pub(super) resume_rx: tokio::sync::mpsc::UnboundedReceiver<Resume>,
}

impl Runtime {
    /// Builds what the loop holds, in the order the interface sees it: the tools, the session, the events of the
    /// start, the shared state, then the tasks that start with the session and the `session_start` hook.
    pub(super) async fn build(events: EventTx, setup: AgentSetup) -> anyhow::Result<Runtime> {
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
        let tools_enabled = add_shared_memory(&mut tools);
        // the sub-agents, when there are tools to give them; they reach the state once it exists
        let host: crate::subagent::HostCell = Arc::new(OnceLock::new());
        let flow_sub = add_subagents(&config, &cwd, &client, &target, &mut tools, &host);
        let plan = add_ask_tools(&mut tools, tools_enabled, headless, &asker);
        let (mut session, history, replay) = open_session(resume.as_deref(), &cwd, &provider, &model, &events)?;
        apply_title(&mut session, config.agent.session_title.as_deref(), &events);
        let bin_told = crate::bin_hint::told(&history);
        let dbg_told = crate::dbg_hint::told(&history);
        let ask_told = crate::ask_hint::told(&history);
        let prefix = prefix::settle(&mut session, &cwd, &config.agent, &mut tools);
        let system = prompt::system_prompt_with(&cwd, &config.agent, &tools, window_of(&limits), &prefix.volatile);
        let policy = context::from_config(&config.context);
        context::record(&mut session, policy.as_ref(), &config.context.policy);
        let perm = Policy::from_config(&config.permissions);
        let config_sandbox = (
            config.permissions.sandbox,
            config.permissions.sandbox_network,
            config.permissions.sandbox_write_dirs.clone(),
        );
        plan.lock().unwrap_or_else(|e| e.into_inner()).set(perm.mode);
        if let Some(m) = &mail {
            m.set_bypass(perm.mode == PermissionMode::Auto);
            m.rebind(session.id(), &mail_name(session.meta().title.as_str(), &cwd));
            // a session that has just started is between turns: mail may be handed over right away
            m.set_idle(true);
        }
        announce_session(&events, &session, &provider, &model, &cwd, perm.mode);
        warn_setup(&events, &config, &tools);
        if replay {
            replay_history(&events, &session, &tools);
        }
        announce_model(&events, &config, &provider, &model);
        let resumed_at = if replay { std::fs::metadata(session.path()).and_then(|m| m.modified()).ok() } else { None };
        let goal = session.goal().map(String::from);
        let goal_state = session.goal_state().clone();
        if goal.is_some() {
            emit(&events, AgentEvent::GoalLog(resumed_log(goal.as_deref())));
            emit(
                &events,
                AgentEvent::Goal { text: goal.clone(), round: 0, max: goal_rounds(), state: goal_state.clone() },
            );
        }
        // Marks the end of startup, including replay, for consumers such as ACP.
        emit(&events, limits.event(&provider, &model));
        let (usage, cost) = (session.usage(), session.cost());
        let max_cost = config.agent.max_cost;
        // the machine map is indexed only where the request would carry from it
        let machine = crate::machine_map::MachineMap::start(
            config.agent.attach_named_files && config.agent.attach_budget > 0 && !config.agent.hermetic,
        );
        let jobs = Jobs::new();
        let detach = nexo_core::jobs::Detach::default();
        let job_changes = jobs.changes();
        let sub_changes = jobs.subs().changes();
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
            fit_view: None,
            last_view: (0, 0),
            view_failed: false,
            last_sent: crate::history::Sent::default(),
            last_request: None,
            mended: HashSet::new(),
            perm,
            sandbox: nexo_core::sandbox::SandboxSettings::new(config_sandbox.0, config_sandbox.1, config_sandbox.2),
            gate,
            alone: headless,
            events: events.clone(),
            usage,
            cost,
            system,
            limits,
            fast: false,
            live: None,
            last_person_at: resumed_at,
            turn_start: 0,
            request_in: false,
            turns: 0,
            goal_log: resumed_log(goal.as_deref()),
            goal_cost0: cost,
            goal_idle: GoalIdle::default(),
            goal_counts: GoalCounts::new(cost),
            goal,
            goal_round: 0,
            goal_state,
            checkpoints: Vec::new(),
            budget: max_cost,
            budget_warned: false,
            unpriced_warned: false,
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
            post: Arc::new(nexo_tools::postcheck::Guard::with_store(nexo_core::paths::cache_dir().join("slow-checks"))),
            repeats: Default::default(),
            bin_told,
            dbg_told,
            ask_told,
            ask_again: None,
            machine,
            epoch: 0,
            flow_slot: Default::default(),
            plan,
            last_prompt: None,
            pauses: Default::default(),
            est_sent: 0,
            compact_failed_at: None,
            tok_ratio: None,
            last_call: None,
            // the tools given may predate servers that connected since: the first turn brings them in
            mcp_gen: 0,
            mcp: mcp.clone(),
            mail,
            mail_wait: Vec::new(),
            mail_turn: Vec::new(),
            hook_context: Vec::new(),
            hidden: Vec::new(),
            hidden_told: 0,
            noticed: HashSet::new(),
            mcp_short: Vec::new(),
            mcp_told: String::new(),
            checked_in: false,
            held: false,
            reply_boost: 1,
            audit,
        }));
        let _ = host.set(Arc::new(StateHost { state: Arc::downgrade(&state), events: events.clone() }));
        if let Some(hub) = &mcp {
            watch_mcp(hub.clone(), events.clone());
        }
        {
            let mut st = state.lock().await;
            session_opened(&st);
            st.system = full_system(&st);
            // with a limit set, the interface shows the spending against it from the start
            if st.budget.is_some() || st.config.agent.max_cost_day.is_some() {
                emit(&st.events, budget_event(&st, budget::spent_today()));
            }
        }
        let mut saving: Option<std::thread::JoinHandle<()>> = None;
        if replay {
            settle_replay(&mut *state.lock().await, &mut saving).await;
            restore_model(&state, model_pinned).await;
        }
        spawn_catalogue_fill(&state);
        spawn_vendor_fill(&state);
        if !headless {
            spawn_model_lists(&state);
        }
        spawn_job_review(&state, jobs.clone());
        {
            let source = if replay { "resume" } else { "startup" };
            let o = fire(&state, "session_start", Some(source), serde_json::json!({"source": source})).await;
            state.lock().await.hook_context.extend(o.context);
        }
        // a pasted key that a killed run was trying must not stay on disk until the setup opens again
        tokio::task::spawn_blocking(setup::sweep_trial_keys);
        let (resume_tx, resume_rx) = tokio::sync::mpsc::unbounded_channel::<Resume>();
        Ok(Runtime {
            state,
            events,
            chan_gate,
            asker,
            headless,
            flow_sub,
            jobs,
            detach,
            job_changes,
            sub_changes,
            subs_open: true,
            saving,
            turn: None,
            signing_in: None,
            checking: None,
            cancel: CancellationToken::new(),
            run_queue: false,
            stopped: None,
            stopper: None,
            resume_tx,
            resume_rx,
        })
    }
}

/// Removes the scratch folders of dead processes, once per process, off the first frame.
fn sweep_scratch_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = std::thread::Builder::new().name("scratch-sweep".into()).spawn(nexo_tools::bash::sweep_tmp);
    });
}

/// Adds the shared-memory tools to a session that has tools, and tells whether it has any.
fn add_shared_memory(tools: &mut ToolSet) -> bool {
    // An explicitly empty tool set is a tool-free session, including agent-provided tools.
    let tools_enabled = tools.names().next().is_some();
    // Register before taking the sub-agent tool snapshot; the ordinary permission policy applies.
    if tools_enabled {
        tools.add(Arc::new(crate::share_tool::ReadSharedMemory));
        tools.add(Arc::new(crate::share_tool::WriteSharedMemory));
    }
    tools_enabled
}

/// Adds the sub-agent tool (and the workflow tool where a workflow is saved) when sub-agents are on and there
/// are tools to give them, and returns what a workflow the person starts by name runs its sub-agents with.
fn add_subagents(
    config: &Config,
    cwd: &Path,
    client: &Arc<dyn ModelClient>,
    target: &Target,
    tools: &mut ToolSet,
    host: &crate::subagent::HostCell,
) -> Option<Arc<crate::subagent::SubAgent>> {
    if !(config.agent.subagents && tools.specs().iter().any(|s| s.name == "read" || s.name == "grep")) {
        return None;
    }
    let profiles = crate::subagent::load(cwd);
    let sub = crate::subagent::SubAgent::new(client.clone(), target.clone(), tools, profiles, host.clone())
        .with_brief(crate::brief::BriefConfig::from_agent(&config.agent))
        .with_providers(config.providers.clone())
        .with_parallel(config.agent.parallel_tasks)
        .with_hooks(config.hooks.clone())
        .with_worktree_setup(config.agent.worktree_setup.clone())
        .with_context(config.context.clone());
    let sub = Arc::new(sub);
    tools.add(sub.clone());
    let flow_sub = Some(sub.clone());
    // offered only where a workflow is saved: it costs a description in every request
    if crate::workflow::Workflow::any_saved(cwd) {
        tools.add(Arc::new(crate::workflow::Workflow::new(sub, cwd.to_path_buf())));
    }
    flow_sub
}

/// Adds the tools that ask the person and leave plan mode, and the goal brief, after the sub-agents: questions
/// are the main agent's alone. Returns the cell plan mode is kept in.
fn add_ask_tools(
    tools: &mut ToolSet,
    tools_enabled: bool,
    headless: bool,
    asker: &Arc<crate::ask::ChannelAsker>,
) -> crate::ask::PlanCell {
    let plan: crate::ask::PlanCell = Arc::default();
    if tools_enabled {
        if !headless {
            tools.add(Arc::new(crate::ask::AskUser::new(asker.clone())));
        }
        tools.add(Arc::new(crate::ask::ExitPlan::new((!headless).then(|| asker.clone()), plan.clone())));
        tools.add(Arc::new(crate::goal_brief::GoalBrief));
    }
    plan
}

/// The session this run writes: the one `resume` names, else a new one, else one that is not saved. With it come
/// the history it continues and whether that history is replayed to the interface.
fn open_session(
    resume: Option<&str>,
    cwd: &Path,
    provider: &str,
    model: &str,
    events: &EventTx,
) -> anyhow::Result<(Session, Vec<Message>, bool)> {
    Ok(match resume {
        Some(id) => {
            // taken before anything is spent: a session another nexo writes is not resumed here
            // an open error already names the session file, so the id is said once; only a claim needs it
            let mut s = Session::open(id).map_err(|e| anyhow::anyhow!("cannot resume: {e}"))?;
            s.claim().map_err(|e| anyhow::anyhow!("cannot resume session {id}: {e}"))?;
            skipped_note(events, id, &s);
            let h = s.messages().to_vec();
            (s, h, true)
        }
        None => match Session::create(cwd, provider, model) {
            Ok(s) => (s, Vec::new(), false),
            Err(e) => {
                notice(events, Level::Warn, format!("the session is not being saved: {e}"));
                (Session::ephemeral(cwd, provider, model), Vec::new(), false)
            }
        },
    })
}

/// `--name`: the title of the session this run writes, set as `nexo sessions rename` does. It is set before the
/// first announcement, so every view shows it at once.
pub(crate) fn apply_title(session: &mut Session, title: Option<&str>, events: &EventTx) {
    if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
        if let Err(e) = session.set_title(t) {
            notice(events, Level::Warn, format!("the session name was not saved: {e}"));
        }
    }
}

/// The first events of a session: which session, model and folder it is, and its name.
fn announce_session(
    events: &EventTx,
    session: &Session,
    provider: &str,
    model: &str,
    cwd: &Path,
    permission_mode: PermissionMode,
) {
    emit(
        events,
        AgentEvent::Ready {
            session_id: session.id().to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            cwd: cwd.to_path_buf(),
            permission_mode,
        },
    );
    emit(
        events,
        AgentEvent::SessionChanged { session_id: session.id().to_string(), title: session.meta().title.clone() },
    );
}

/// The warnings of the configuration, and the sandbox's when it is on but cannot work here.
fn warn_setup(events: &EventTx, config: &Config, tools: &ToolSet) {
    for n in &config.notices {
        notice(events, Level::Warn, n.clone());
    }
    let s = nexo_core::sandbox::SandboxSettings::new(
        config.permissions.sandbox,
        config.permissions.sandbox_network,
        vec![],
    );
    if let Some((line, false)) = tools.get("bash").and_then(|t| t.sandbox_status(&s)).filter(|_| s.mode.is_on()) {
        notice(events, Level::Warn, format!("sandbox: {line}"));
    }
}

/// The model's effort levels and, when the model has a fast twin, that fast mode is on.
fn announce_model(events: &EventTx, config: &Config, provider: &str, model: &str) {
    if let Some(c) = config.providers.get(provider) {
        if let Some(n) = setup::effort_clamp_note(c, model, &config.agent.reasoning) {
            notice(events, Level::Info, n);
        }
        emit(
            events,
            AgentEvent::Effort {
                model: model.to_string(),
                current: setup::effective_effort(c, model, &config.agent.reasoning),
                levels: c.efforts.get(model).cloned().unwrap_or_default(),
            },
        );
        if matches!(fast::twin(model, c.models.iter().map(String::as_str)), Twin::Base(_)) {
            emit(events, AgentEvent::Fast { on: true });
        }
    }
}

/// The interface learns of every change of an MCP server (its lists, a login, a failure).
fn watch_mcp(hub: Arc<nexo_tools::mcp_hub::McpHub>, events: EventTx) {
    tokio::spawn(async move {
        let mut rx = hub.watch();
        emit(&events, AgentEvent::Mcp { servers: hub.info() });
        while rx.changed().await.is_ok() {
            emit(&events, AgentEvent::Mcp { servers: hub.info() });
        }
    });
}

/// After a resume's replay: calls left open are closed, and the context policy's saved state and view are
/// rebuilt, then written again in the background.
async fn settle_replay(st: &mut State, saving: &mut Option<std::thread::JoinHandle<()>>) {
    close_open_calls(st);
    if !st.history.is_empty() {
        saved_state::load(st);
        view_now(st, 0).await;
        // a view shown, not sent: the next request finds no cache of it
        st.last_sent.clear();
        // what the replay rebuilt is kept even when this nexo is closed before its first turn ends
        saved_state::save_in_background(st, saving);
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn the_name_is_set_on_a_new_session_and_a_resumed_one_and_a_blank_one_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let (_h, ends) = nexo_core::agent::channels();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
        s.append(&nexo_core::message::Message::user("one")).unwrap();
        apply_title(&mut s, Some("  api cleanup "), &ends.events);
        assert_eq!(s.meta().title, "api cleanup");
        assert_eq!(Session::open_path(s.path()).unwrap().meta().title, "api cleanup", "it is on the file");
        apply_title(&mut s, None, &ends.events);
        apply_title(&mut s, Some("   "), &ends.events);
        assert_eq!(s.meta().title, "api cleanup");
        let mut again = Session::open_path(s.path()).unwrap();
        apply_title(&mut again, Some("second"), &ends.events);
        assert_eq!(Session::open_path(s.path()).unwrap().meta().title, "second", "a resumed session is renamed");
    }
}
