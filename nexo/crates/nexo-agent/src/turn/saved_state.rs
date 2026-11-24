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
    if let Some(path) = session_file(st) {
        let history = st.history.to_vec();
        if st.policy.name() != "curator" {
            if let Some(bytes) = saved::read(&path) {
                st.policy.restore(bytes);
            }
            return;
        }
        let mut candidates = saved::read(&path).into_iter().collect::<Vec<_>>();
        candidates.extend(saved::compaction_candidates(&path).into_iter().rev());
        // A late background writer may have replaced the ordinary snapshot with an older one.
        // Prefer the compatible state that covers most of the current history; on a tie the
        // prepared compaction state wins because it contains the rules from the removed prefix.
        let best = candidates
            .into_iter()
            .filter(|b| st.policy.accepts_snapshot(b, &history))
            .max_by_key(|b| nexo_curator::snapshot::Header::read(b).map(|h| h.covered).unwrap_or(0));
        if let Some(bytes) = best {
            st.policy.restore(bytes);
        }
    }
}

/// When a turn has ended: the policy's state is copied under the lock and written on a thread of its own, at
/// most one at a time, and only when the history grew since the last one.
pub(super) fn save_in_background(st: &mut State, writing: &mut Option<std::thread::JoinHandle<std::io::Result<()>>>) {
    if writing.as_ref().is_some_and(|h| !h.is_finished()) {
        return;
    }
    if let Some(h) = writing.take() {
        if !matches!(h.join(), Ok(Ok(()))) {
            st.policy.save_failed();
        }
    }
    let Some(path) = session_file(st) else { return };
    if let Some(job) = st.policy.save() {
        *writing = saved::write_in_background(path, job);
        if writing.is_none() {
            st.policy.save_failed();
        }
    }
}

/// At the exit: the write a turn left running ends first, then the last state is written.
pub(super) fn save_now(st: &mut State, writing: &mut Option<std::thread::JoinHandle<std::io::Result<()>>>) {
    if let Some(h) = writing.take() {
        if !matches!(h.join(), Ok(Ok(()))) {
            st.policy.save_failed();
        }
    }
    let Some(path) = session_file(st) else { return };
    if let Some(job) = st.policy.save() {
        if saved::write(&path, job).is_err() {
            st.policy.save_failed();
        }
    }
}
