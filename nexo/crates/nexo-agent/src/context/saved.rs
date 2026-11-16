//! The file next to a session where its context policy's state is saved (`ContextPolicy::save`), so that a
//! resume starts from that state instead of rebuilding it from the whole history. The session file itself is
//! never changed for it: a build that knows nothing of this file reads the session as before.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use super::SaveJob;

/// `<id>.jsonl.curator` next to `<id>.jsonl`: the session lists read only `.jsonl` files, so it is never taken
/// for a session.
pub fn path_for(session: &Path) -> PathBuf {
    session.with_extension("jsonl.curator")
}

/// The saved state of the session at `session`, when there is one.
pub fn read(session: &Path) -> Option<Vec<u8>> {
    std::fs::read(path_for(session)).ok()
}

/// Encodes the state and writes it next to the session, whole or not at all: it goes to a file of this process
/// first and then takes the place of the old one, so a reader never finds half a state, and two writers (a
/// turn's and the exit's) leave one of the two.
pub fn write(session: &Path, job: SaveJob) {
    let t = Instant::now();
    let bytes = job();
    let encoded = t.elapsed();
    let path = path_for(session);
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("curator.{}.{n}.tmp", std::process::id()));
    let done = std::fs::write(&tmp, &bytes).and_then(|_| std::fs::rename(&tmp, &path));
    match done {
        Ok(()) => tracing::debug!(
            "context state saved: {} bytes, encoded in {} ms, written in {} ms",
            bytes.len(),
            encoded.as_millis(),
            t.elapsed().as_millis()
        ),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            tracing::warn!("context state not saved next to the session: {e}");
        }
    }
}

/// `write` on a thread of its own, off the agent's lock and the turn that follows.
pub fn write_in_background(session: PathBuf, job: SaveJob) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new().name("context-save".into()).spawn(move || write(&session, job)).ok()
}

/// Removes the saved state of a session, when it cannot go where the session went.
pub fn forget(session: &Path) {
    let _ = std::fs::remove_file(path_for(session));
}
