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
    sweep(session);
}

/// A write that a crash or a kill cut between the file and the rename leaves its file of up to tens of MB
/// behind, and nothing else would ever remove it.
pub(crate) const YOUNG: std::time::Duration = std::time::Duration::from_secs(3600);

/// Removes the files of cut writes that belong to this session (`<id>.jsonl.curator.<pid>.<n>.tmp`): those of
/// a process that is gone, those older than the saved state that took their place, and, where a process cannot
/// be asked for (Windows), those untouched for an hour. The files of this process (a write may be running) and
/// every other name, including the files of other sessions, are left alone. Returns how many it removed.
pub fn sweep(session: &Path) -> usize {
    let (Some(dir), Some(name)) = (session.parent(), session.file_name().and_then(|n| n.to_str())) else {
        return 0;
    };
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let prefix = format!("{name}.curator.");
    let saved_at = std::fs::metadata(path_for(session)).and_then(|m| m.modified()).ok();
    let mut removed = 0;
    for e in entries.flatten() {
        let file = e.file_name();
        let Some(rest) = file.to_str().and_then(|f| f.strip_prefix(&prefix)).and_then(|r| r.strip_suffix(".tmp"))
        else {
            continue;
        };
        let mut parts = rest.split('.');
        let (Some(Ok(pid)), Some(Ok(_)), None) =
            (parts.next().map(str::parse::<u32>), parts.next().map(str::parse::<u64>), parts.next())
        else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let written = e.metadata().and_then(|m| m.modified()).ok();
        let older = matches!((written, saved_at), (Some(w), Some(s)) if w < s);
        let long_ago = written.and_then(|w| w.elapsed().ok()).is_some_and(|a| a > YOUNG);
        if (no_such_process(pid) || older || long_ago) && std::fs::remove_file(e.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Whether signal 0 finds no process under `pid` (a process of another user still exists: EPERM).
#[cfg(unix)]
pub(crate) fn no_such_process(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // SAFETY: signal 0 sends nothing; it only reports whether the process exists.
    let r = unsafe { libc::kill(pid, 0) };
    r != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(not(unix))]
pub(crate) fn no_such_process(_pid: u32) -> bool {
    false
}
