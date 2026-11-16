//! Part of the agent loop; see the parent module.
//!
//! The session's end, once the loop stops: what runs is stopped and waited for, the `session_end` hook fires, the
//! state is written, and the session is let go.

use super::runtime::Runtime;
use super::*;

pub(super) async fn close(rt: Runtime) {
    let Runtime { state, events, chan_gate, asker, jobs, mut saving, turn, cancel, .. } = rt;
    cancel.cancel();
    chan_gate.deny_all("shutdown");
    asker.drop_all();
    jobs.kill_all();
    if let Some(t) = turn {
        let _ = t.await;
    }
    fire(&state, "session_end", Some("exit"), serde_json::json!({"reason": "exit"})).await;
    saved_state::save_now(&mut *state.lock().await, &mut saving);
    state.lock().await.policy.shutdown().await;
    // the scratch folder (`$NEXO_TMP`) lives as long as the session
    nexo_tools::bash::wipe_tmp(state.lock().await.session.id());
    // a task still holding the state (a hook, a suggestion) must not keep the lock past the exit
    state.lock().await.session.release();
    emit(&events, AgentEvent::Shutdown);
}
