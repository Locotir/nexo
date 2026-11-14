//! Part of the agent loop; see the parent module.
//!
//! A saved workflow the person starts by name (`/workflow <name> [args]`), and the stops they ask for.

use std::sync::atomic::{AtomicU64, Ordering};

use nexo_core::jobs::JobKind;

use super::approval::{permit_why, Verdict};
use super::*;

/// What the person is told when nothing can start.
const NO_SUB_AGENTS: &str = "Workflows need sub-agents, and they are off in this session.";

/// Stops what a stop request named: the sub-agents it returned are cancelled the way any sub-agent is, through
/// its job. A request the registry refused is told to the person.
pub(super) async fn stop(state: &Shared, asked: Result<Vec<u32>, String>) {
    let (jobs, events) = {
        let st = state.lock().await;
        (st.jobs.clone(), st.events.clone())
    };
    match asked {
        Ok(running) => {
            for job in running {
                jobs.kill(job);
            }
        }
        Err(why) => notice(&events, Level::Warn, why),
    }
}

/// The conversation was replaced (`/clear`, `/resume`, `/fork`): what the old one still had in flight reaches
/// nobody. The registry starts over (what a run still sends is dropped at the source), what it sent already is
/// dropped here, and the questions still open are denied the way an interrupt denies them, so an answer that
/// comes late adds no rule to the new conversation. Called after the new conversation is in place, so a
/// question denied here finds the epoch changed and says nothing.
pub(super) fn forget_old(
    jobs: &nexo_core::jobs::Jobs,
    sub_changes: &mut tokio::sync::broadcast::Receiver<nexo_core::subs::SubChange>,
    gate: &crate::gate::ChannelGate,
) {
    jobs.subs().clear();
    while !matches!(
        sub_changes.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty | tokio::sync::broadcast::error::TryRecvError::Closed)
    ) {}
    gate.deny_all("cleared");
}

/// The conversation was replaced: a start of the old one holds nothing of the new one's slot.
pub(super) fn release_slot(st: &State) {
    st.flow_slot.store(0, Ordering::SeqCst);
}

/// The one-run slot of a start by name, held from before the question until the run is over. It is let go when
/// this is dropped, on every way out (a denial, a refusal, an error, the conversation changing, the end), and
/// only if it is still this start's: a start of a conversation that was replaced cannot free a newer one's.
struct Slot {
    cell: Arc<AtomicU64>,
    mine: u64,
}

impl Slot {
    fn take(cell: &Arc<AtomicU64>, mine: u64) -> Option<Slot> {
        cell.compare_exchange(0, mine, Ordering::SeqCst, Ordering::SeqCst).ok()?;
        Some(Slot { cell: cell.clone(), mine })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let _ = self.cell.compare_exchange(self.mine, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

/// Runs the saved workflow `name` for the person. It goes through the hooks, the permission question and the
/// content pin of the `workflow` tool, so typing a name is not leave for whatever the file holds today. It runs
/// as a job in the background: the person goes on talking to the main agent, and the result reaches it as a note
/// when the run ends. One start at a time: the slot is taken before anything is asked.
pub(super) async fn start(state: &Shared, sub: Option<Arc<crate::subagent::SubAgent>>, name: String, given: String) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let (events, jobs, cwd, gate, epoch, cell) = {
        let st = state.lock().await;
        (st.events.clone(), st.jobs.clone(), st.cwd.clone(), st.gate.clone(), st.epoch, st.flow_slot.clone())
    };
    let Some(sub) = sub else {
        notice(&events, Level::Warn, NO_SUB_AGENTS);
        return;
    };
    // one run at a time: said before the person is asked anything
    if let Some(busy) = jobs.subs().flow_running() {
        notice(
            &events,
            Level::Warn,
            format!("A workflow is already running ({busy}); wait for it to end or stop it first."),
        );
        return;
    }
    let tool = crate::workflow::Workflow::new(sub, cwd.clone());
    let mut call = serde_json::json!({"name": name, "args": given});
    // no pin: the name is not a saved workflow that loads, so there is nothing to ask leave for
    let mut probe = call.clone();
    crate::workflow::tool::pin(&mut probe, &cwd);
    if probe.get(crate::workflow::tool::PIN).is_none() {
        notice(&events, Level::Warn, crate::workflow::tool::question(&probe, &cwd).0);
        return;
    }
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let Some(slot) = Slot::take(&cell, n) else {
        notice(
            &events,
            Level::Warn,
            "A workflow is already starting; answer its question or wait for it to end first.",
        );
        return;
    };
    let id = format!("workflow-start-{n}");
    let state = state.clone();
    // the question waits for an answer that comes through the same channel as this command
    tokio::spawn(async move {
        let _slot = slot;
        // the hooks of the tool, as the model's call has them: one may block it, decide its permission or replace
        // its input (then the pin is taken again from what will really run)
        let hooked = fire(&state, "pre_tool", Some("workflow"), crate::hooks::tool_fields("workflow", &call)).await;
        if let Some(u) = hooked.updated_input {
            call = u;
        }
        crate::workflow::tool::pin(&mut call, &cwd);
        // the conversation was replaced while a hook ran: nothing is asked in the new one
        if state.lock().await.epoch != epoch {
            return;
        }
        if let Some(why) = hooked.blocked {
            notice(&events, Level::Info, format!("Workflow `{name}` was not started: {why}"));
            return;
        }
        if call.get(crate::workflow::tool::PIN).is_none() {
            notice(&events, Level::Warn, crate::workflow::tool::question(&call, &cwd).0);
            return;
        }
        let never = CancellationToken::new();
        let (verdict, _) =
            permit_why(&state, &events, &gate, &cwd, &id, "workflow", &tool, &call, &never, hooked.decision, None)
                .await;
        // the conversation was cleared while the question was open (an interrupt denies it): the answer belongs to a
        // session that is gone, and it hears nothing of it
        if state.lock().await.epoch != epoch {
            return;
        }
        match verdict {
            Verdict::Run => {}
            Verdict::Denied(why) | Verdict::Blocked(why) => {
                notice(&events, Level::Info, format!("Workflow `{name}` was not started: {why}"));
                return;
            }
        }
        let job = jobs.start(JobKind::Agent, format!("workflow `{name}`"));
        let mut ctx = context(&state, &cwd, &format!("wf{}", job.id())).await;
        // stopping the job (the person, or `/clear`) is the interrupt of this run; the turn's own is not
        ctx.cancel = job.cancel();
        let fields = crate::hooks::tool_fields("workflow", &call);
        let t0 = Instant::now();
        let out = nexo_core::tool::Tool::run(&tool, call, &ctx).await;
        // the conversation was cleared during the run: the job is finished, but nothing of it is said to the new one
        if state.lock().await.epoch != epoch {
            job.finish(false, "stopped");
            return;
        }
        // PostToolUse hooks: their text goes to the main agent in the note, as it goes after a tool's output
        let mut fields = fields;
        fields["output"] = serde_json::json!(out.content);
        fields["tool_response"] = serde_json::json!(out.content);
        fields["is_error"] = serde_json::json!(out.is_error);
        fields["elapsed_ms"] = serde_json::json!(t0.elapsed().as_millis() as u64);
        let after = fire(&state, "post_tool", Some("workflow"), fields).await;
        let hook_notes: Vec<String> = after.context.into_iter().chain(after.blocked).collect();
        let mut report = format!(
            "nexo ran the saved workflow `{name}` because the person typed /workflow {name}; this note is from nexo, \
             not from the person.\n\n{}",
            out.content
        );
        if !hook_notes.is_empty() {
            report = format!("{report}\n\n[hook] {}", hook_notes.join("\n[hook] "));
        }
        job.line("--- report ---");
        job.text(&report);
        job.set_report(&report);
        if out.is_error {
            let first = out.content.lines().next().unwrap_or("");
            notice(&events, Level::Warn, format!("Workflow `{name}` did not finish: {first}"));
        }
        job.finish(!out.is_error, if out.is_error { "reported an error" } else { "reported" });
    });
}

/// The context a workflow the person started runs its sub-agents in: the one a tool call gets, but not tied to
/// a turn.
async fn context(state: &Shared, cwd: &Path, call_id: &str) -> ToolContext {
    let st = state.lock().await;
    let (provider, _, model) = st.target.get();
    let vision = catalogue::cached().and_then(|c| c.lookup(&provider, &model).and_then(|e| e.vision)).unwrap_or(true);
    let post_check =
        st.config.tools.post_write_check && nexo_tools::project::check_command_within(cwd, Some(cwd)).is_some();
    let (perm, rules_cwd) = (st.perm.clone(), cwd.to_path_buf());
    ToolContext {
        cwd: cwd.to_path_buf(),
        session_id: st.session.id().to_string(),
        call_id: call_id.to_string(),
        cancel: CancellationToken::new(),
        progress: None,
        env: Default::default(),
        jobs: st.jobs.clone(),
        claims: st.claims.clone(),
        detach: None,
        seen: st.seen.clone(),
        sandbox: st.sandbox.clone(),
        vision,
        post_check,
        check_secs: st.config.tools.post_write_check_secs,
        check: Some(Arc::new(move |tool: &str, kind, args: &Value| match perm.decide(tool, kind, args, &rules_cwd) {
            Decision::Allow => nexo_core::tool::Check::Allow,
            Decision::Ask => nexo_core::tool::Check::Ask,
            Decision::Deny(_) => nexo_core::tool::Check::Deny,
        })),
    }
}
