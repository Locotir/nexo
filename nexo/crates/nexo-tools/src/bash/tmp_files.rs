/// The scratch folder of a session, in the OS temp folder, outside the project: `$NEXO_TMP` in its commands.
pub fn tmp_dir(session: &str) -> std::path::PathBuf {
    let id: String = session.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(40).collect();
    let id = if id.is_empty() { "main".to_string() } else { id };
    std::env::temp_dir().join(format!("nexo-tmp-{}-{id}", std::process::id()))
}

/// Removes a session's scratch folder; the agent calls it when the session ends or is replaced.
pub fn wipe_tmp(session: &str) {
    let _ = std::fs::remove_dir_all(tmp_dir(session));
}

/// Most directory entries a sweep looks at.
pub(super) const SWEEP_LOOK: usize = 5_000;
/// Most folders a sweep removes.
pub(super) const SWEEP_REMOVE: usize = 200;

/// A folder touched more recently than this stays even when its pid is gone: a process in another
/// container that shares the temp folder has a pid that cannot be probed from here.
pub(super) const SWEEP_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Removes the scratch folders that processes now gone left in the OS temp folder (a killed or crashed
/// nexo never wiped its own). Bounded, silent, errors ignored; the agent runs it once at start.
pub fn sweep_tmp() {
    sweep_tmp_in(&std::env::temp_dir(), &pid_alive, SWEEP_MIN_AGE);
}

/// Whether process `pid` exists (a process of another user counts as alive).
#[cfg(unix)]
pub(super) fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(not(unix))]
pub(super) fn pid_alive(_pid: i32) -> bool {
    true
}

/// The pid in a `nexo-tmp-<digits>-<rest>` folder name.
pub(super) fn tmp_name_pid(name: &str) -> Option<i32> {
    let (digits, rest) = name.strip_prefix("nexo-tmp-")?.split_once('-')?;
    if rest.is_empty() || digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<i32>().ok().filter(|p| *p > 0)
}

/// `sweep_tmp` on a given temp folder, with the test for "this pid is alive" and the least age given.
/// Windows and other systems do not probe pids: a folder goes only when untouched for 7 days.
pub fn sweep_tmp_in(tmp: &std::path::Path, alive: &dyn Fn(i32) -> bool, min_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(tmp) else { return };
    let me = std::process::id();
    let mut removed = 0;
    for entry in entries.take(SWEEP_LOOK) {
        if removed >= SWEEP_REMOVE {
            break;
        }
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(tmp_name_pid) else { continue };
        if pid as u32 == me {
            continue;
        }
        let path = entry.path();
        // never follow a link: a symlink is not a directory here
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if !meta.is_dir() {
            continue;
        }
        let age = meta.modified().ok().and_then(|m| m.elapsed().ok());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // SAFETY: geteuid has no preconditions.
            if meta.uid() != unsafe { libc::geteuid() } || alive(pid) || !age.is_some_and(|a| a >= min_age) {
                continue;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = alive;
            let least = min_age.max(std::time::Duration::from_secs(7 * 24 * 3600));
            if !age.is_some_and(|a| a > least) {
                continue;
            }
        }
        removed += 1;
        let _ = std::fs::remove_dir_all(&path);
    }
}
