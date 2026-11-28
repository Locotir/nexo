//! The file next to a session where its context policy's state is saved (`ContextPolicy::save`), so that a
//! resume starts from that state instead of rebuilding it from the whole history. The session file itself is
//! never changed for it: a build that knows nothing of this file reads the session as before.

use std::io::Write;
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
pub fn write(session: &Path, job: SaveJob) -> std::io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    write_with_counter(session, job, &NEXT)
}

fn write_with_counter(session: &Path, job: SaveJob, next: &AtomicU64) -> std::io::Result<()> {
    let t = Instant::now();
    let bytes = job();
    let encoded = t.elapsed();
    let path = path_for(session);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // A crashed process with a reused PID can leave one of our names behind. Never
    // remove a file we did not create, and bound retries if the directory is full of them.
    let mut opened = None;
    let mut error = None;
    for _ in 0..128 {
        let n = next.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_extension(format!("curator.{}.{n}.tmp", std::process::id()));
        match options.open(&tmp) {
            Ok(file) => {
                opened = Some((tmp, file));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => error = Some(e),
            Err(e) => {
                error = Some(e);
                break;
            }
        }
    }
    let done = opened.ok_or_else(|| error.expect("at least one open attempt")).and_then(|(tmp, mut file)| {
        let result = file.write_all(&bytes).and_then(|_| std::fs::rename(&tmp, &path));
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    });
    match &done {
        Ok(()) => tracing::debug!(
            "context state saved: {} bytes, encoded in {} ms, written in {} ms",
            bytes.len(),
            encoded.as_millis(),
            t.elapsed().as_millis()
        ),
        Err(e) => {
            tracing::warn!("context state not saved next to the session: {e}");
        }
    }
    done
}

/// Prepare a separate recovery slot before replacing the session. Ordinary background saves never
/// write this name, so a delayed save cannot overwrite the transaction's recovery state.
pub fn write_compaction(
    session: &Path,
    bytes: Vec<u8>,
    history: &[nexo_core::message::Message],
) -> std::io::Result<()> {
    let values: Vec<_> =
        history.iter().map(serde_json::to_value).collect::<Result<_, _>>().map_err(std::io::Error::other)?;
    let hashes: Vec<_> = values.iter().map(nexo_race::snapshot::message_hash).collect();
    let hash = nexo_race::snapshot::history_hash(&hashes);
    let path = session.with_extension(format!("jsonl.curator.compact.{}.{hash:016x}", history.len()));
    static NEXT_COMPACT: AtomicU64 = AtomicU64::new(0);
    let tmp = session.with_extension(format!(
        "jsonl.curator.compact.{}.{}.tmp",
        std::process::id(),
        NEXT_COMPACT.fetch_add(1, Ordering::Relaxed)
    ));
    let done = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &path));
    if done.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    done
}

/// Candidate recovery slots, newest first. The caller validates each against policy and history.
pub fn compaction_candidates(session: &Path) -> Vec<Vec<u8>> {
    let (Some(dir), Some(name)) = (session.parent(), session.file_name().and_then(|n| n.to_str())) else {
        return vec![];
    };
    let prefix = format!("{name}.curator.compact.");
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut paths: Vec<_> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let rest = name.to_str()?.strip_prefix(&prefix)?;
            let (count, hash) = rest.split_once('.')?;
            if count.parse::<usize>().is_err() || hash.len() != 16 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            Some((e.metadata().and_then(|m| m.modified()).ok(), e.path()))
        })
        .collect();
    paths.sort_by_key(|p| std::cmp::Reverse(p.0));
    paths.into_iter().filter_map(|(_, p)| std::fs::read(p).ok()).collect()
}

/// `write` on a thread of its own, off the agent's lock and the turn that follows.
pub fn write_in_background(session: PathBuf, job: SaveJob) -> Option<std::thread::JoinHandle<std::io::Result<()>>> {
    std::thread::Builder::new().name("context-save".into()).spawn(move || write(&session, job)).ok()
}

/// Removes the saved state of a session, when it cannot go where the session went.
pub fn forget(session: &Path) {
    let _ = std::fs::remove_file(path_for(session));
    // Clean up the experimental immediate-compaction slot, if it exists.
    let _ = std::fs::remove_file(session.with_extension("jsonl.curator.compact"));
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
        let rest = rest.strip_prefix("compact.").unwrap_or(rest);
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn collision_retries_without_touching_existing_temp() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("session.jsonl");
        let state = path_for(&session);
        let stale = state.with_extension(format!("curator.{}.0.tmp", std::process::id()));
        std::fs::write(&stale, b"other writer").unwrap();
        let counter = AtomicU64::new(0);
        write_with_counter(&session, Box::new(|| b"new state".to_vec()), &counter).unwrap();
        assert_eq!(std::fs::read(&state).unwrap(), b"new state");
        assert_eq!(std::fs::read(&stale).unwrap(), b"other writer");
        assert_eq!(counter.load(Ordering::Relaxed), 2);
    }

    #[cfg(unix)]
    #[test]
    fn saved_context_is_private_even_when_replacing_an_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("session.jsonl");
        let state = path_for(&session);
        std::fs::write(&state, b"old").unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o644)).unwrap();
        write(&session, Box::new(|| b"private context".to_vec())).unwrap();
        assert_eq!(std::fs::read(&state).unwrap(), b"private context");
        assert_eq!(std::fs::metadata(state).unwrap().permissions().mode() & 0o077, 0);
    }
}
