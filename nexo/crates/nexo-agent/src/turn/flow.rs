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

/// Runs the saved workflow `name` for the person. It goes through the permission question and the content
/// pin of the `workflow` tool, so typing a name is not leave for whatever the file holds today. It runs as a
/// job in the background: the person goes on talking to the main agent, and the result reaches it as a note
/// when the run ends.
pub(super) async fn start(state: &Shared, sub: Option<Arc<crate::subagent::SubAgent>>, name: String, given: String) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let (events, jobs, cwd, gate, epoch) = {
        let st = state.lock().await;
        (st.events.clone(), st.jobs.clone(), st.cwd.clone(), st.gate.clone(), st.epoch)
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
    crate::workflow::tool::pin(&mut call, &cwd);
    // no pin: the name is not a saved workflow that loads, so there is nothing to ask leave for
    if call.get(crate::workflow::tool::PIN).is_none() {
        notice(&events, Level::Warn, crate::workflow::tool::question(&call, &cwd).0);
        return;
    }
    let id = format!("workflow-start-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let state = state.clone();
    // the question waits for an answer that comes through the same channel as this command
    tokio::spawn(async move {
        let never = CancellationToken::new();
        let (verdict, _) =
            permit_why(&state, &events, &gate, &cwd, &id, "workflow", &tool, &call, &never, None, None).await;
        match verdict {
            Verdict::Run => {}
            Verdict::Denied(why) | Verdict::Blocked(why) => {
                notice(&events, Level::Info, format!("Workflow `{name}` was not started: {why}"));
                return;
            }
        }
        // the conversation was cleared while the question was open: the answer belongs to a session that is gone
        if state.lock().await.epoch != epoch {
            return;
        }
        let job = jobs.start(JobKind::Agent, format!("workflow `{name}`"));
        let mut ctx = context(&state, &cwd, &format!("wf{}", job.id())).await;
        // stopping the job (the person, or `/clear`) is the interrupt of this run; the turn's own is not
        ctx.cancel = job.cancel();
        let out = nexo_core::tool::Tool::run(&tool, call, &ctx).await;
        let report = format!(
            "nexo ran the saved workflow `{name}` because the person typed /workflow {name}; this note is from nexo, \
             not from the person.\n\n{}",
            out.content
        );
        job.line("--- report ---");
        job.text(&report);
        job.set_report(&report);
        if out.is_error {
            let first = out.content.lines().next().unwrap_or("");
            notice(&events, Level::Warn, format!("Workflow `{name}` did not finish: {first}"));
        }
        job.finish(!out.is_error, "reported");
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
