//! Part of the agent loop; see the parent module.
//!
//! The session's end, once the loop stops: what runs is stopped and waited for, the `session_end` hook fires, the
//! state is written, and the session is let go.

use super::runtime::Runtime;
use super::*;

/// How long a turn that was told to stop gets before it is cut off. The hook, the saved state and the session release
/// come after it, so a tool that ignores the cancel (a blocking call, a stuck hook) must not hold them back. The
/// interface waits for the whole close a little longer than this (`CLOSE_WAIT` in the CLI).
const TURN_STOP: Duration = Duration::from_secs(2);

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
    fire(&state, "session_end", Some("exit"), serde_json::json!({"reason": "exit"})).await;
    saved_state::save_now(&mut *state.lock().await, &mut saving);
    state.lock().await.policy.shutdown().await;
    // the scratch folder (`$NEXO_TMP`) lives as long as the session
    nexo_tools::bash::wipe_tmp(state.lock().await.session.id());
    // a task still holding the state (a hook, a suggestion) must not keep the lock past the exit
    state.lock().await.session.release();
    emit(&events, AgentEvent::Shutdown);
}
