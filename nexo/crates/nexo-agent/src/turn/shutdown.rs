//! Part of the agent loop; see the parent module.
//!
//! The session's end, once the loop stops: what runs is stopped and waited for, the state is written, the session is
//! let go, and the `session_end` hook fires last, because it is the one step whose length the session does not set.

use super::runtime::Runtime;
use super::*;

/// How long a turn that was told to stop gets before it is cut off. A tool that ends itself on the cancel (a sub-agent,
/// a workflow) takes up to its own cleanup time, 5 s, to remove its worktree and run its hooks, so this is a little
/// longer than that. A tool that ignores the cancel (a blocking call) must not hold the rest back past it. The
/// interface waits for the whole close a few seconds longer than this (`CLOSE_WAIT` in the CLI).
const TURN_STOP: Duration = Duration::from_secs(6);

pub(super) async fn close(rt: Runtime) {
    let Runtime { state, events, chan_gate, asker, jobs, mut saving, turn, cancel, .. } = rt;
    cancel.cancel();
    chan_gate.deny_all("shutdown");
    asker.drop_all();
    jobs.kill_all();
    if let Some(mut t) = turn {
        // An aborted task drops its future where it waits; a thread inside `spawn_blocking` still runs on, but it no
        // longer stands between the session and its save.
        if tokio::time::timeout(TURN_STOP, &mut t).await.is_err() {
            t.abort();
        }
    }
    // The state is written and the session let go before the hook: a hook may run for as long as its own limit (60 s
    // unless it sets one), and a process that stops waiting for it must not lose them. The hook does not change the
    // state; it only learns that the session ended.
    saved_state::save_now(&mut *state.lock().await, &mut saving);
    state.lock().await.policy.shutdown().await;
    // a task still holding the state (a hook, a suggestion) must not keep the lock past the exit
    state.lock().await.session.release();
    fire(&state, "session_end", Some("exit"), serde_json::json!({"reason": "exit"})).await;
    // the scratch folder (`$NEXO_TMP`) lives as long as the session, hooks included; one left behind by a stop that
    // came first is swept later with the folders of sessions that are gone
    nexo_tools::bash::wipe_tmp(state.lock().await.session.id());
    emit(&events, AgentEvent::Shutdown);
}
