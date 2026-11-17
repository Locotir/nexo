//! Starting a lane's runner so that it outlives the terminal that started it: on Unix in a session of its own
//! (no controlling terminal, so closing the terminal sends it no hangup), on Windows detached from the console
//! and in a process group of its own. Its input is empty and what it prints goes to `<name>.log`.

use std::path::Path;
use std::process::{Command, Stdio};

use super::state;

/// A log past this size is moved to `<name>.log.1` (the one old copy kept) when its runner starts.
const LOG_MAX: u64 = 5 * 1024 * 1024;

/// Moves `log` to `<log>.1` when it is past `max` bytes, over the older copy.
pub fn rotate(log: &Path, max: u64) {
    if std::fs::metadata(log).is_ok_and(|m| m.len() > max) {
        let mut old = log.as_os_str().to_owned();
        old.push(".1");
        let _ = std::fs::rename(log, old);
    }
}

/// Starts `nexo team run <name>` detached, with this program as the runner; its pid.
pub fn runner(name: &str) -> std::io::Result<u32> {
    runner_of(&std::env::current_exe()?, name)
}

/// Like [`runner`], and the runner starts its first round at once, whatever the last one was (`team start --now`).
pub fn runner_now(name: &str) -> std::io::Result<u32> {
    launch(&std::env::current_exe()?, name, true)
}

/// Starts `<exe> team run <name>` detached; its pid. The program is a parameter so a test can start a stand-in.
pub fn runner_of(exe: &Path, name: &str) -> std::io::Result<u32> {
    launch(exe, name, false)
}

/// Opens a lane's log for appending, after it is rotated. What a lane prints is its transcript: commands and tool
/// output, which can hold secrets read from a file. So the team folder is `0700` (this also closes the older logs and
/// state files inside it) and the log `0600`, an existing one included.
fn open_log(log: &Path) -> std::io::Result<std::fs::File> {
    use crate::fsmode::{create_private_dir, private_file, tighten_dir, tighten_file};
    let dir = log.parent().unwrap_or(Path::new("."));
    create_private_dir(dir)?;
    tighten_dir(dir)?;
    rotate(log, LOG_MAX);
    let out = private_file().create(true).append(true).open(log)?;
    tighten_file(log)?;
    Ok(out)
}

// the runner is meant to outlive this command, so it is never waited on
#[allow(clippy::zombie_processes)]
fn launch(exe: &Path, name: &str, now: bool) -> std::io::Result<u32> {
    let log = state::log_file(name);
    let out = open_log(&log)?;
    let err = out.try_clone()?;
    if now {
        std::fs::write(state::now_file(name), b"")?;
    }
    let mut c = Command::new(exe);
    c.args(["team", "run", name]).stdin(Stdio::null()).stdout(out).stderr(err).current_dir(state::dir());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and touches nothing of the parent's; it runs between fork and exec.
        unsafe {
            c.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    // the child is never waited on: it runs on after this command ends, and the system reaps it
    let child = c.spawn().inspect_err(|_| {
        if now {
            let _ = std::fs::remove_file(state::now_file(name));
        }
    })?;
    Ok(child.id())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn a_started_lane_gets_a_private_log_and_folder() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let d = tempfile::tempdir().unwrap();
        // a team folder and log from before nexo set a mode, as an installed nexo has them
        let dir = d.path().join("team");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let old = dir.join("OLD.log");
        std::fs::write(&old, b"earlier").unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o644)).unwrap();
        drop(super::open_log(&old).unwrap());
        assert_eq!((mode(&dir), mode(&old)), (0o700, 0o600));
        assert_eq!(std::fs::read(&old).unwrap(), b"earlier");
        // a folder and log made new
        let fresh = d.path().join("fresh/team/NEW.log");
        drop(super::open_log(&fresh).unwrap());
        assert_eq!((mode(fresh.parent().unwrap()), mode(&fresh)), (0o700, 0o600));
    }

    #[test]
    fn a_log_past_its_size_keeps_one_old_copy() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("A.log");
        std::fs::write(&log, b"0123456789").unwrap();
        super::rotate(&log, 100);
        assert!(log.exists(), "a small log stays");
        super::rotate(&log, 5);
        assert!(!log.exists());
        assert_eq!(std::fs::read(d.path().join("A.log.1")).unwrap(), b"0123456789");
        std::fs::write(&log, b"newer and longer").unwrap();
        super::rotate(&log, 5);
        assert_eq!(std::fs::read(d.path().join("A.log.1")).unwrap(), b"newer and longer", "one old copy");
    }
}
