use crate::dbg_cmd::DbgArgs;

pub(super) fn run_dbg(words: Vec<String>) -> Result<String, String> {
    crate::dbg_cmd::execute(&DbgArgs { words }).map_err(|e| format!("nexo dbg: {e}"))
}

/// Attach stays same-user: the process must belong to this user. Returns the parsed process id, which is the
/// one to pass on (only plain decimal digits are a pid: "+123" is not).
pub(super) fn same_user(pid: &str) -> Result<u32, String> {
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("attach needs a process id of plain digits, not {pid:?}"));
    }
    let n: u32 = pid.parse().map_err(|_| format!("attach needs a process id, not {pid:?}"))?;
    check_owner(n).map(|()| n)
}

/// The user id that owns process `n`, or None when there is no such process. Linux reads the owner of its /proc
/// folder; elsewhere (macOS has no /proc) `ps` prints it.
#[cfg(unix)]
fn owner_of(n: u32) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    if let Ok(m) = std::fs::metadata(format!("/proc/{n}")) {
        return Some(m.uid());
    }
    if cfg!(target_os = "linux") {
        return None;
    }
    let out = std::process::Command::new("ps").args(["-o", "uid=", "-p", &n.to_string()]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[cfg(unix)]
pub(super) fn check_owner(n: u32) -> Result<(), String> {
    let owner = owner_of(n).ok_or(());
    let me = unsafe { libc::geteuid() };
    match owner {
        Ok(u) if u != me => {
            Err(format!("refused: process {n} belongs to another user; attach is for your own processes only"))
        }
        Err(()) => Err(format!("no process {n} to attach to; check the id with `ps`")),
        Ok(_) => Ok(()),
    }
}

#[cfg(not(unix))]
pub(super) fn check_owner(n: u32) -> Result<(), String> {
    Err(format!(
        "refused: attaching to process {n} is not available on this system because the owner of a process cannot be checked here; read a core file or minidump instead (kind core)"
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn an_own_process_may_be_attached_to() {
        assert_eq!(check_owner(std::process::id()), Ok(()));
        assert_eq!(same_user(&std::process::id().to_string()), Ok(std::process::id()));
    }

    #[test]
    fn a_process_of_another_user_is_refused() {
        // init belongs to root; as root the check has nothing to refuse
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let e = check_owner(1).unwrap_err();
        assert!(e.contains("another user"), "{e}");
    }

    #[test]
    fn a_process_that_is_gone_is_said_to_be_gone() {
        let mut child = std::process::Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        let e = check_owner(pid).unwrap_err();
        assert!(e.contains("no process"), "{e}");
    }
}
