//! The context policy's state saved next to the session (`context::saved`): read before the first view of a
//! resumed session, and written when a turn ends and at the exit, so that entering an old session does not
//! rebuild that state from the whole history.

use super::*;

use crate::context::saved;

/// The session's file, when this nexo writes it: not for a session kept nowhere or one another nexo took over.
fn session_file(st: &State) -> Option<PathBuf> {
    let p = st.session.path();
    (!p.as_os_str().is_empty() && !st.session.is_lost()).then(|| p.to_path_buf())
}

/// Before the first view of a resumed session: the policy starts from the state saved with it, when there is one.
pub(super) fn load(st: &mut State) {
    if let Some(p) = session_file(st) {
        saved::sweep(&p);
    }
    if let Some(bytes) = session_file(st).and_then(|p| saved::read(&p)) {
        st.policy.restore(bytes);
    }
}

/// When a turn has ended: the policy's state is copied under the lock and written on a thread of its own, at
/// most one at a time, and only when the history grew since the last one.
pub(super) fn save_in_background(st: &mut State, writing: &mut Option<std::thread::JoinHandle<()>>) {
    if writing.as_ref().is_some_and(|h| !h.is_finished()) {
        return;
    }
    let Some(path) = session_file(st) else { return };
    if let Some(job) = st.policy.save() {
        *writing = saved::write_in_background(path, job);
    }
}

/// At the exit: the write a turn left running ends first, then the last state is written.
pub(super) fn save_now(st: &mut State, writing: &mut Option<std::thread::JoinHandle<()>>) {
    if let Some(h) = writing.take() {
        let _ = h.join();
    }
    let Some(path) = session_file(st) else { return };
    if let Some(job) = st.policy.save() {
        saved::write(&path, job);
    }
}
