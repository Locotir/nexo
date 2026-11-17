//! Part of the agent loop; see the parent module.
//!
//! One command of the interface, answered. `on_command` holds one arm per command; an arm longer than a few
//! lines is a function of its own below, named after the command. The commands of the model setup and the MCP
//! servers, which run in tasks of their own, are in `connect`.

use std::ops::ControlFlow;

use nexo_core::agent::{DirectedReply, DirectedResult, ObservationTarget, Stopper};
use nexo_core::jobs::DetachWhy;

use super::connect;
use super::runtime::Runtime;
use super::*;

/// Answers one command; `Break` when the session ends.
pub(super) async fn on_command(rt: &mut Runtime, cmd: AgentCommand) -> ControlFlow<()> {
    match cmd {
        AgentCommand::Submit { text, attachments } => on_submit(rt, text, attachments).await,
        AgentCommand::SubmitDirected { target, text, reply } => on_submit_directed(rt, target, text, reply).await,
        AgentCommand::Ultracode { text, attachments } => on_ultracode(rt, text, attachments).await,
        AgentCommand::Deliver { mails } => on_deliver(rt, mails).await,
        AgentCommand::SendMail { to, text } => on_send_mail(rt, to, text).await,
        AgentCommand::ListPeers => on_list_peers(rt).await,
        AgentCommand::Unqueue { index, text } => on_unqueue(rt, index, text).await,
        AgentCommand::Detach => {
            // the key acts on what is attached now; with nothing attached it leaves no signal
            // behind for a sub-agent that starts later
            if rt.turn.is_some() && rt.detach.attached() > 0 {
                rt.detach.fire(DetachWhy::Key);
            }
        }
        AgentCommand::Interrupt | AgentCommand::InterruptRun | AgentCommand::StopBy(_) => {
            let by = if let AgentCommand::StopBy(by) = cmd { by } else { Stopper::Esc };
            on_interrupt(rt, matches!(cmd, AgentCommand::InterruptRun), by)
        }
        AgentCommand::Permission { id, decision } => {
            if !rt.chan_gate.resolve(&id, decision) {
                tracing::debug!("no permission ask waits for {id}");
            }
        }
        AgentCommand::SetModel { provider, model } => set_model(&rt.state, provider, model).await,
        AgentCommand::Answer { id, answer } => {
            if !rt.asker.resolve(&id, answer) {
                tracing::debug!("no question waits for {id}");
            }
        }
        AgentCommand::SetPermissionMode(mode) => set_mode(&mut *rt.state.lock().await, mode),
        AgentCommand::SetSandbox(mode) => set_sandbox(&mut *rt.state.lock().await, mode),
        AgentCommand::AddDir(dir) => add_dir(&mut *rt.state.lock().await, &dir),
        AgentCommand::Compact => {
            if rt.turn.is_some() {
                notice(&rt.events, Level::Warn, "compaction waits for the turn to end");
            } else {
                compact_now(&rt.state, &rt.events, None, &CancellationToken::new()).await;
            }
        }
        AgentCommand::Clear => on_clear(rt).await,
        AgentCommand::Shell { command } => {
            let cwd = rt.state.lock().await.cwd.clone();
            run_shell(cwd, command, rt.events.clone());
        }
        AgentCommand::SetPricing { pricing } => set_pricing(&rt.state, pricing).await,
        AgentCommand::ShowModel => {
            let st = rt.state.lock().await;
            let (p, _, m) = st.target.get();
            emit(&rt.events, st.limits.event(&p, &m));
        }
        AgentCommand::SetGoal { text } => on_set_goal(rt, text).await,
        AgentCommand::PauseGoal => pause_goal(&rt.state, rt.turn.is_some()).await,
        AgentCommand::ResumeGoal => {
            if resume_goal(&rt.state, rt.turn.is_some()).await {
                let text = GOAL_CONTINUE.to_string();
                rt.turn = Some(start_turn(&rt.state, text, Vec::new(), rt.cancel.child_token()).await);
            }
        }
        AgentCommand::ShowPrompt => {
            let st = rt.state.lock().await;
            let text = full_system(&st);
            emit(&rt.events, AgentEvent::SystemPrompt { text });
        }
        AgentCommand::Resume { id } => on_resume(rt, id).await,
        AgentCommand::Fork { index } => on_fork(rt, index).await,
        AgentCommand::ShowTree => {
            // the scan reads every session file: it runs on a blocking thread and takes no lock, so a turn's
            // rounds do not wait for it. It is awaited here so its event comes before those of later commands
            // and a panic in it is reported
            let scan = tokio::task::spawn_blocking(|| {
                crate::session::tree(&crate::session::list()).into_iter().map(|(d, m)| (d, m.id, m.title)).collect()
            })
            .await;
            match scan {
                Ok(entries) => emit(&rt.events, AgentEvent::SessionTree { entries }),
                Err(e) => notice(&rt.events, Level::Error, format!("cannot read the session tree: {e}")),
            }
        }
        AgentCommand::DeleteSession { id } => delete_session(&rt.state, &id).await,
        AgentCommand::ShareMemory { id, focus } => share_memory(&rt.state, id, focus).await,
        AgentCommand::ShareMemories { ids } => share_memories(&rt.state, ids).await,
        AgentCommand::ReadSharedMemory { since } => read_shared_memory(&rt.state, since).await,
        AgentCommand::WriteSharedMemory { text } => append_shared_memory(&rt.state, &text).await,
        AgentCommand::ListTrash => {
            // read on a blocking thread without the lock and awaited, as the tree is
            let read = tokio::task::spawn_blocking(|| {
                crate::session::trashed().into_iter().map(|m| (m.id, m.title)).collect()
            })
            .await;
            match read {
                Ok(sessions) => emit(&rt.events, AgentEvent::Trash { sessions }),
                Err(e) => notice(&rt.events, Level::Error, format!("cannot read the trash: {e}")),
            }
        }
        AgentCommand::RestoreSession { id } => on_restore_session(rt, id).await,
        AgentCommand::Rewind { index } => {
            if rt.turn.is_some() {
                notice(&rt.events, Level::Warn, "finish or interrupt the turn before rewinding");
            } else {
                rewind(&rt.state, index).await;
            }
        }
        AgentCommand::Export { path, format } => export(&rt.state, path, format).await,
        AgentCommand::Rename { title } => on_rename(rt, title).await,
        AgentCommand::SetBudget { limit } => on_set_budget(rt, limit).await,
        AgentCommand::ShowBudget => {
            let st = rt.state.lock().await;
            let day = budget::spent_today();
            emit(&rt.events, budget_event(&st, day));
            notice(&rt.events, Level::Info, budget_text(&st, day, false));
        }
        AgentCommand::ShowStatus => {
            let st = rt.state.lock().await;
            notice(&rt.events, Level::Info, status_text(&st));
        }
        AgentCommand::ShowHooks => {
            let st = rt.state.lock().await;
            notice(&rt.events, Level::Info, crate::hooks::describe(&st.config.hooks));
        }
        AgentCommand::ShowSent => on_show_sent(rt).await,
        AgentCommand::ShowMcp => {
            let hub = rt.state.lock().await.mcp.clone();
            match hub {
                Some(h) => emit(&rt.events, AgentEvent::Mcp { servers: h.info() }),
                None => {
                    emit(&rt.events, AgentEvent::Mcp { servers: Vec::new() });
                }
            }
        }
        AgentCommand::ChromeCheck => chrome::check(&rt.state, &rt.events).await,
        AgentCommand::ChromeSetup => chrome::set_up(&rt.state, &rt.events).await,
        AgentCommand::ChromeUse { id } => chrome::use_browser(&rt.state, &rt.events, id).await,
        AgentCommand::McpToggle { server, on, persist } => connect::on_mcp_toggle(rt, server, on, persist).await,
        AgentCommand::McpReconnect { server } => connect::on_mcp_reconnect(rt, server).await,
        AgentCommand::McpLogin { server } => connect::on_mcp_login(rt, server).await,
        AgentCommand::McpPrompt { server, prompt, args } => on_mcp_prompt(rt, server, prompt, args).await,
        AgentCommand::ShowTools => {
            let st = rt.state.lock().await;
            notice(&rt.events, Level::Info, tools_text(&st.tools));
        }
        AgentCommand::ScanLocal { own } => connect::on_scan_local(rt, own),
        AgentCommand::Probe { name, cfg, key, seq } => connect::on_probe(rt, name, cfg, key, seq),
        AgentCommand::Login { name, flow } => connect::on_login(rt, name, flow),
        AgentCommand::CancelLogin => {
            if let Some(prev) = rt.signing_in.take() {
                prev.abort();
            }
        }
        AgentCommand::CancelCheck => {
            if let Some(prev) = rt.checking.take() {
                prev.abort();
            }
        }
        AgentCommand::CheckModel { name, probe, cfg, model, key, seq } => {
            connect::on_check_model(rt, name, probe, cfg, model, key, seq).await
        }
        AgentCommand::SaveModel { name, cfg, model, key } => save_model(&rt.state, name, cfg, model, key).await,
        AgentCommand::SetEffort { effort } => set_effort(&rt.state, effort).await,
        AgentCommand::SetFast { on } => set_fast(&rt.state, on).await,
        AgentCommand::KillJob { id } => on_kill_job(rt, id),
        AgentCommand::JobTails => emit(&rt.events, AgentEvent::JobTails { tails: rt.jobs.tails() }),
        // the event for a message that cannot be delivered is made by the registry
        AgentCommand::SubSubmit { id, text } => {
            let _ = rt.jobs.subs().submit(&id, &text);
        }
        AgentCommand::SubTranscript { id, request } => {
            let transcript = rt.jobs.subs().transcript(&id);
            emit(&rt.events, AgentEvent::SubTranscript { id, request, transcript });
        }
        AgentCommand::FlowStart { name, args } => flow::start(&rt.state, rt.flow_sub.clone(), name, args).await,
        AgentCommand::FlowStopStep { run, step } => {
            flow::stop(&rt.state, rt.jobs.subs().stop_flow_step(&run, &step)).await
        }
        AgentCommand::FlowStop { run } => flow::stop(&rt.state, rt.jobs.subs().stop_flow(&run)).await,
        AgentCommand::ShowJobs => {
            notice(&rt.events, Level::Info, jobs_text(&rt.jobs));
            emit(&rt.events, AgentEvent::Jobs { jobs: rt.jobs.list() });
        }
        AgentCommand::Suggest => {
            // only between requests: a running one makes the suggestion stale
            if rt.turn.is_none() {
                spawn_suggest(&rt.state);
            }
        }
        AgentCommand::CheckIn { evidence } => {
            if rt.turn.is_none() {
                check_in(&rt.state, Some(evidence), &rt.resume_tx).await;
            }
        }
        AgentCommand::Shutdown => return ControlFlow::Break(()),
    }
    ControlFlow::Continue(())
}

/// A request of the person: queued while a turn runs, else the start of one.
async fn on_submit(rt: &mut Runtime, text: String, attachments: Vec<Attachment>) {
    let mut st = rt.state.lock().await;
    st.goal_round = 0;
    if rt.turn.is_some() {
        st.inbox.push_back(InboxItem::sent(text, attachments, true));
        emit(&rt.events, AgentEvent::Queued { pending: st.inbox.len() });
        // the person is heard now: sub-agents the round waits for go to the background
        if st.config.agent.detach_on_message {
            rt.detach.fire(DetachWhy::Message);
        }
    } else {
        person_spoke(&mut st);
        // requests kept after an Esc were written before this one: they go with it, first
        let kept = st.inbox.len();
        st.inbox.push_back(InboxItem::sent(text, attachments, false));
        let (next, _) = queue::take_queued(&mut st);
        if kept > 0 {
            emit(&rt.events, AgentEvent::Queued { pending: st.inbox.len() });
        }
        drop(st);
        if let Some(mut item) = next {
            let text = std::mem::take(&mut item.text);
            let attachments = std::mem::take(&mut item.attachments);
            rt.turn = start_prompt_item(&rt.state, text, attachments, false, rt.cancel.child_token(), Some(item)).await;
        }
    }
}

/// A request sent to this session's observation: queued behind the running turn while the observation still
/// matches, else rejected.
async fn on_submit_directed(rt: &mut Runtime, target: ObservationTarget, text: String, reply: DirectedReply) {
    let mut st = rt.state.lock().await;
    if !st.target_matches(&target) {
        reply.resolve(DirectedResult::Rejected("observation changed"));
    } else if rt.turn.is_none() {
        reply.resolve(DirectedResult::Rejected("no active turn"));
    } else {
        let mut item = InboxItem::sent(text, Vec::new(), true);
        item.directed = Some((target, reply));
        st.inbox.push_back(item);
        emit(&rt.events, AgentEvent::Queued { pending: st.inbox.len() });
        if st.config.agent.detach_on_message {
            rt.detach.fire(DetachWhy::Message);
        }
    }
}

/// `/ultracode`: a turn that may only plan sub-agents and run them through `ultracode_execute`, which exists for
/// that turn alone and holds every other sub-agent launch back while it lasts.
async fn on_ultracode(rt: &mut Runtime, text: String, attachments: Vec<Attachment>) {
    if rt.turn.is_some() {
        notice(&rt.events, Level::Warn, "ultracode requires an idle turn");
    } else if let Some(sub) = rt.flow_sub.clone() {
        if let Some(guard) = sub.exclusive_launches() {
            let guard = Arc::new(guard);
            {
                let mut st = rt.state.lock().await;
                person_spoke(&mut st);
                let tool = ultracode::ExecutePlan {
                    sub,
                    guard: guard.clone(),
                    config: st.config.clone(),
                    asker: (!rt.headless).then(|| rt.asker.clone()),
                    permissions: st.plan.clone(),
                    state: Arc::downgrade(&rt.state),
                    used: Default::default(),
                };
                st.tools.add(Arc::new(tool));
            }
            let request = format!("{text}\n\nCreate a sequential agent plan and submit it once using ultracode_execute. Do not start agents any other way. Approval is required before execution.");
            if let Some(handle) = start_prompt(&rt.state, request, attachments, true, rt.cancel.child_token()).await {
                let state = rt.state.clone();
                rt.turn = Some(tokio::spawn(async move {
                    let end = handle.await.unwrap_or(TurnEnd::Interrupted);
                    state.lock().await.tools.remove("ultracode_execute");
                    drop(guard);
                    end
                }));
            } else {
                rt.state.lock().await.tools.remove("ultracode_execute");
                drop(guard);
            }
        } else {
            notice(&rt.events, Level::Warn, "ultracode cannot start while a sub-agent is active or launching");
        }
    } else {
        notice(&rt.events, Level::Warn, "ultracode requires enabled sub-agents");
    }
}

/// Mail from other sessions: read now when the agent is idle, else when the running turn ends.
async fn on_deliver(rt: &mut Runtime, mails: Vec<Mail>) {
    if mails.is_empty() {
        return;
    }
    if rt.turn.is_some() {
        // never in the middle of a turn: it goes when this one ends
        rt.state.lock().await.mail_wait.extend(mails);
    } else {
        rt.turn = Some(deliver_mails(&rt.state, mails, rt.cancel.child_token()).await);
    }
}

/// `/msg`: the person writes to another session.
async fn on_send_mail(rt: &mut Runtime, to: String, text: String) {
    let mail = rt.state.lock().await.mail.clone();
    match mail {
        None => notice(&rt.events, Level::Warn, "mail between sessions is off"),
        Some(m) => {
            let evs = tokio::task::spawn_blocking(move || {
                let _ = m.send(&to, &text, None, nexo_core::mail::FromKind::Person);
                m.drain_events()
            })
            .await
            .unwrap_or_default();
            for e in evs {
                emit(&rt.events, e);
            }
        }
    }
}

/// The sessions mail can reach.
async fn on_list_peers(rt: &mut Runtime) {
    let mail = rt.state.lock().await.mail.clone();
    let peers = match mail {
        Some(m) => tokio::task::spawn_blocking(move || m.peers()).await.unwrap_or_default(),
        None => Vec::new(),
    };
    emit(&rt.events, AgentEvent::Peers { peers });
}

/// The person takes back a queued request: the one at `index` when it still holds `text`, else the first that
/// does.
async fn on_unqueue(rt: &mut Runtime, index: usize, text: String) {
    let mut st = rt.state.lock().await;
    let at = if st.inbox.get(index).is_some_and(|item| item.directed.is_none() && item.text == text) {
        Some(index)
    } else {
        st.inbox.iter().position(|item| item.directed.is_none() && item.text == text)
    };
    if let Some(i) = at {
        st.inbox.remove(i);
    }
    emit(&rt.events, AgentEvent::Queued { pending: st.inbox.len() });
}

/// Esc (or Enter on a queued request, `run_queue`): the running turn is cancelled with its open questions.
fn on_interrupt(rt: &mut Runtime, run_queue: bool, by: Stopper) {
    if rt.turn.is_some() {
        rt.run_queue = run_queue;
        if rt.stopped.is_none() {
            rt.stopped = Some(SystemTime::now());
            rt.stopper = Some(by);
        }
        rt.cancel.cancel();
        // the questions of the turn; those of sub-agents in the background stay open
        rt.chan_gate.deny_turn("interrupted", &rt.detach);
        rt.asker.drop_all();
    }
}

/// `/clear`: a running turn is stopped and waited for, the queue and the jobs go, and the conversation starts
/// over.
async fn on_clear(rt: &mut Runtime) {
    let was_running = rt.turn.is_some();
    if was_running {
        rt.cancel.cancel();
        rt.chan_gate.deny_all("cleared");
        rt.asker.drop_all();
        if let Some(t) = rt.turn.take() {
            let _ = t.await;
            emit(&rt.events, AgentEvent::TurnEnd { reason: TurnEnd::Interrupted });
        }
        rt.cancel = CancellationToken::new();
        rt.run_queue = false;
        rt.detach.clear();
        let mut st = rt.state.lock().await;
        if !st.inbox.is_empty() {
            st.inbox.clear();
            emit(&rt.events, AgentEvent::Queued { pending: 0 });
        }
    }
    rt.jobs.kill_all();
    clear(&rt.state).await;
    flow::forget_old(&rt.jobs, &mut rt.sub_changes, &rt.chan_gate);
    // the killed jobs are not news for the new conversation
    rt.jobs.take_ended();
    if was_running {
        // the cancelled turn never reaches the end-of-turn tail: the agent is idle now
        if let Some(m) = rt.state.lock().await.mail.clone() {
            m.set_idle(true);
        }
        emit(&rt.events, AgentEvent::Idle { running_jobs: rt.jobs.running() });
    }
}

/// `/goal`: a goal set while idle starts a turn towards it; one set during a turn is read from the next.
async fn on_set_goal(rt: &mut Runtime, text: Option<String>) {
    if let Some(goal) = set_goal(&rt.state, text).await {
        let request = format!("{GOAL_SET_MARK} {goal}");
        if rt.turn.is_some() {
            let text = format!(
                "goal set: {} · checked when this turn ends; the model reads it from the next turn",
                nexo_core::agent::goal_shown(&goal)
            );
            notice(&rt.events, Level::Info, text);
        } else {
            rt.turn = Some(start_turn(&rt.state, request, Vec::new(), rt.cancel.child_token()).await);
        }
    }
}

/// `/resume <id>` between turns; the sub-agents of the conversation it replaced are forgotten.
async fn on_resume(rt: &mut Runtime, id: String) {
    if rt.turn.is_some() {
        notice(&rt.events, Level::Warn, "finish or interrupt the turn before resuming another session");
    } else {
        let before = rt.state.lock().await.epoch;
        resume_session(&rt.state, &id).await;
        if rt.state.lock().await.epoch != before {
            flow::forget_old(&rt.jobs, &mut rt.sub_changes, &rt.chan_gate);
        }
    }
}

/// `/fork` between turns; the sub-agents of the conversation it replaced are forgotten.
async fn on_fork(rt: &mut Runtime, index: Option<usize>) {
    if rt.turn.is_some() {
        notice(&rt.events, Level::Warn, "finish or interrupt the turn before forking");
    } else {
        let before = rt.state.lock().await.epoch;
        fork_session(&rt.state, index).await;
        if rt.state.lock().await.epoch != before {
            flow::forget_old(&rt.jobs, &mut rt.sub_changes, &rt.chan_gate);
        }
    }
}

/// A session taken out of the trash.
async fn on_restore_session(rt: &mut Runtime, id: String) {
    // nothing here reads the state, so the lock stays free while the file moves and its title is read (the title
    // alone, as the list reads it, not the whole conversation)
    let restoring = id.clone();
    let done = tokio::task::spawn_blocking(move || {
        crate::session::restore(&restoring)
            .map(|_| crate::session::meta_in(&crate::session::dir(), &restoring).map(|m| m.title).unwrap_or_default())
    })
    .await;
    match done {
        Ok(Ok(title)) => emit(&rt.events, AgentEvent::SessionRestored { id, title }),
        Ok(Err(e)) => notice(&rt.events, Level::Error, format!("cannot restore: {e}")),
        Err(e) => notice(&rt.events, Level::Error, format!("cannot restore: {e}")),
    }
}

/// `/rename <name>`: the session's title, also for mail.
async fn on_rename(rt: &mut Runtime, title: String) {
    let mut st = rt.state.lock().await;
    let title = title.trim().to_string();
    if title.is_empty() {
        notice(&rt.events, Level::Warn, "a name is needed: /rename <name>".to_string());
    } else if let Err(e) = st.session.set_title(&title) {
        notice(&rt.events, Level::Error, format!("cannot rename the session: {e}"));
    } else {
        emit(&rt.events, AgentEvent::SessionChanged { session_id: st.session.id().to_string(), title });
        mail_sync(&st);
    }
}

/// `/budget <limit>`: the session's spending limit, shown with what was spent.
async fn on_set_budget(rt: &mut Runtime, limit: Option<f64>) {
    let mut st = rt.state.lock().await;
    st.budget = limit.filter(|l| *l > 0.0);
    st.budget_warned = false;
    let day = budget::spent_today();
    emit(&rt.events, budget_event(&st, day));
    notice(&rt.events, Level::Info, budget_text(&st, day, true));
}

/// `/sent`: the last request as it was sent, by section.
async fn on_show_sent(rt: &mut Runtime) {
    let st = rt.state.lock().await;
    match &st.last_request {
        Some(req) => {
            emit(&rt.events, AgentEvent::Sent { model: req.model.clone(), sections: crate::sent::sections(req) })
        }
        None => notice(&rt.events, Level::Info, "nothing was sent yet: /sent shows the last request"),
    }
}

/// An MCP server's prompt the person picked: its text is a request like any other, queued while a turn runs.
async fn on_mcp_prompt(rt: &mut Runtime, server: String, prompt: String, args: String) {
    let hub = rt.state.lock().await.mcp.clone();
    let made = match hub {
        Some(h) => h.prompt_request(&server, &prompt, &args).await,
        None => Err("no MCP servers are configured".to_string()),
    };
    match made {
        Err(e) => {
            notice(&rt.events, Level::Warn, format!("/mcp__{server}__{prompt}: {e}"));
            if rt.turn.is_none() {
                emit(&rt.events, AgentEvent::Idle { running_jobs: rt.state.lock().await.jobs.running() });
            }
        }
        Ok(text) => {
            let mut st = rt.state.lock().await;
            st.goal_round = 0;
            if rt.turn.is_some() {
                st.inbox.push_back(InboxItem::sent(text, Vec::new(), true));
                emit(&rt.events, AgentEvent::Queued { pending: st.inbox.len() });
            } else {
                person_spoke(&mut st);
                drop(st);
                rt.turn = start_prompt(&rt.state, text, Vec::new(), false, rt.cancel.child_token()).await;
            }
        }
    }
}

/// `/kill [id]`: one job, or all of them.
fn on_kill_job(rt: &mut Runtime, id: Option<u32>) {
    let n = match id {
        Some(id) => usize::from(rt.jobs.kill(id)),
        None => rt.jobs.kill_all(),
    };
    let what = match id {
        Some(id) if n == 0 => format!("job {id} is not running"),
        Some(id) => format!("job {id} stopped"),
        None => format!("{} stopped", nexo_core::count(n, "job")),
    };
    notice(&rt.events, Level::Info, what);
    emit(&rt.events, AgentEvent::Jobs { jobs: rt.jobs.list() });
}
