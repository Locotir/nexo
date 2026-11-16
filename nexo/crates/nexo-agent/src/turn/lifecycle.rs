//! Part of the agent loop; see the parent module.
//!
//! What the loop does when something other than a command arrives: a check-in's answer, a change of a sub-agent
//! or of a job, and the end of a turn, which decides what runs next by itself.

use tokio::sync::broadcast::error::RecvError;

use super::runtime::Runtime;
use super::*;

/// The end of the running turn; the loop awaits it only while there is one.
pub(super) async fn turn_ended(turn: &mut Option<JoinHandle<TurnEnd>>) -> Result<TurnEnd, tokio::task::JoinError> {
    turn.as_mut().expect("a turn is running").await
}

/// A check-in decided to resume the session.
pub(super) async fn on_resume(rt: &mut Runtime, r: Resume) {
    // the answer comes while the session is still stopped and nothing else has started since
    let current = {
        let st = rt.state.lock().await;
        (st.turns, st.epoch) == (r.turns, r.epoch) && st.inbox.is_empty()
    };
    if rt.turn.is_none() && current {
        rt.state.lock().await.hidden.push(r.text);
        rt.turn =
            Some(start_turn(&rt.state, SUPERVISOR_RESUME_MARK.to_string(), Vec::new(), rt.cancel.child_token()).await);
    }
}

/// A sub-agent run changed: the interface is told.
pub(super) fn on_sub_change(rt: &mut Runtime, change: Result<nexo_core::subs::SubChange, RecvError>) {
    match change {
        Ok(c) => emit(&rt.events, c.into()),
        // a slow interface lost some: it gets every run's state again, and the registry has the rest
        Err(RecvError::Lagged(_)) => {
            for e in sub_resync(&rt.jobs.subs()) {
                emit(&rt.events, e);
            }
        }
        Err(RecvError::Closed) => rt.subs_open = false,
    }
}

/// A job changed: the interface is told, and a job that ended while the agent waited for the person starts a
/// turn with its result.
pub(super) async fn on_job_change(rt: &mut Runtime, change: Result<nexo_core::jobs::JobChange, RecvError>) {
    if let Err(RecvError::Closed) = change {
        return;
    }
    emit(&rt.events, AgentEvent::Jobs { jobs: rt.jobs.list() });
    let ended = matches!(&change, Ok(c) if !c.0.status.running());
    // a job ended while the agent waited for the person: it goes on with the result
    if ended && rt.turn.is_none() && !rt.jobs.untold().is_empty() {
        let note = room::ended_note_now(&rt.state, &rt.jobs).await;
        rt.turn = Some(start_turn(&rt.state, note, Vec::new(), rt.cancel.child_token()).await);
    }
}

/// The running turn ended: the interface and the hooks are told, the goal takes the end, then what runs next
/// starts by itself; with nothing to run the agent is idle and may check in.
pub(super) async fn on_turn_end(rt: &mut Runtime, res: Result<TurnEnd, tokio::task::JoinError>) {
    rt.turn = None;
    rt.detach.clear();
    let reason = end_reason(&rt.events, res);
    emit(&rt.events, AgentEvent::TurnEnd { reason });
    fire_end_hooks(&rt.state, &rt.events, reason).await;
    rt.cancel = CancellationToken::new();
    let run_queue = std::mem::take(&mut rt.run_queue);
    // the goal takes the turn's end first (met, waiting, paused), then a queued message runs
    goal_settle(&rt.state, reason).await;
    start_next(rt, reason, run_queue).await;
    if rt.turn.is_none() {
        if let Some(m) = rt.state.lock().await.mail.clone() {
            m.set_idle(true);
        }
        emit(&rt.events, AgentEvent::Idle { running_jobs: rt.jobs.running() });
        saved_state::save_in_background(&mut *rt.state.lock().await, &mut rt.saving);
    }
    if rt.turn.is_none() && reason == TurnEnd::Done {
        check_in(&rt.state, None, &rt.resume_tx).await;
    }
}

/// How the turn ended; the person is told when its task panicked or was aborted, which counts as an error.
fn end_reason(events: &EventTx, res: Result<TurnEnd, tokio::task::JoinError>) -> TurnEnd {
    res.unwrap_or_else(|e| {
        let how = if e.is_panic() { "panicked" } else { "was aborted" };
        notice(events, Level::Error, format!("the turn {how}"));
        TurnEnd::Error
    })
}

/// The hooks of a turn's end: `turn_end` for every end, in a task of its own, and `turn_stopped` when the
/// request ended early.
async fn fire_end_hooks(state: &Shared, events: &EventTx, reason: TurnEnd) {
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
        notify_hooks(
            state,
            "turn_stopped",
            format!("the request ended early ({})", format!("{reason:?}").to_ascii_lowercase()),
        );
    }
}

/// What starts by itself after a turn, the first that applies: the queued requests (unless an Esc left them to
/// wait for the person), the mail that came during the turn, the goal's next round. When none started and the
/// turn ended done, the note of jobs that ended after its last model call starts one.
async fn start_next(rt: &mut Runtime, reason: TurnEnd, run_queue: bool) {
    // what is left came after the turn's last model call
    let (next, joined) = {
        let mut st = rt.state.lock().await;
        // after Esc the person wanted to stop: what was queued waits for them (Enter sent it on)
        let waits = reason == TurnEnd::Interrupted && !run_queue;
        let (next, joined) = if waits { (None, 0) } else { queue::take_queued(&mut st) };
        if waits && !st.inbox.is_empty() {
            notice(
                &rt.events,
                Level::Info,
                format!("{} queued request(s) kept; they join your next message", st.inbox.len()),
            );
        }
        if next.is_some() {
            emit(&rt.events, AgentEvent::Queued { pending: st.inbox.len() });
            person_spoke(&mut st);
        }
        (next, joined)
    };
    if let Some(mut item) = next {
        let text = std::mem::take(&mut item.text);
        let attachments = std::mem::take(&mut item.attachments);
        let what = if joined > 1 {
            format!("running the {joined} queued requests as one")
        } else {
            "running the queued request".into()
        };
        notice(&rt.events, Level::Info, what);
        rt.turn = start_prompt_item(&rt.state, text, attachments, false, rt.cancel.child_token(), Some(item)).await;
    } else if !rt.state.lock().await.mail_wait.is_empty() {
        let mails = std::mem::take(&mut rt.state.lock().await.mail_wait);
        rt.turn = Some(deliver_mails(&rt.state, mails, rt.cancel.child_token()).await);
    } else if reason == TurnEnd::Done {
        if let Some(text) = goal_follow_up(&rt.state).await {
            rt.turn = Some(start_turn(&rt.state, text, Vec::new(), rt.cancel.child_token()).await);
        }
    }
    // a job that ended after the turn's last model call
    if rt.turn.is_none() && reason == TurnEnd::Done && !rt.jobs.untold().is_empty() {
        let note = room::ended_note_now(&rt.state, &rt.jobs).await;
        rt.turn = Some(start_turn(&rt.state, note, Vec::new(), rt.cancel.child_token()).await);
    }
}
