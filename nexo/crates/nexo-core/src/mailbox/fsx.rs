//! Clock, pid, directory and atomic-write helpers the mailbox shares.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::paths;

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Where the mailboxes live: `<data_dir>/mail`.
pub fn mail_root() -> PathBuf {
    paths::data_dir().join("mail")
}

pub(super) fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if pid == 0 {
            return false;
        }
        // SAFETY: signal 0 only checks that the process exists.
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        // Windows asks the process; where the system cannot tell, any pid but 0 counts
        crate::proc::pid_alive(pid).unwrap_or(pid != 0)
    }
}

pub(super) fn make_dir(p: &Path) -> std::io::Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(p)
}

/// Writes `bytes` to `path` whole: a temporary file in the same folder, then a rename.
pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".tmp-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let r = o.open(&tmp).and_then(|mut f| f.write_all(bytes)).and_then(|_| fs::rename(&tmp, path));
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r
}

pub(super) fn age_ms(p: &Path, now: i64) -> i64 {
    let m = fs::metadata(p).and_then(|m| m.modified()).ok();
    let t = m.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64);
    t.map_or(0, |t| now - t)
}

pub(super) fn text_hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[cfg(all(test, windows))]
#[test]
fn a_peer_whose_process_has_exited_is_not_alive_on_windows() {
    assert!(crate::proc::holds_for_an_exited_pid(|pid| !pid_alive(pid)));
    assert!(pid_alive(std::process::id()));
}
