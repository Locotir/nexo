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

#[cfg(unix)]
pub(super) fn check_owner(n: u32) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let owner = std::fs::metadata(format!("/proc/{n}")).map(|m| m.uid());
    let me = unsafe { libc::geteuid() };
    match owner {
        Ok(u) if u != me => {
            Err(format!("refused: process {n} belongs to another user; attach is for your own processes only"))
        }
        Err(_) => Err(format!("no process {n} to attach to; check the id with `ps`")),
        Ok(_) => Ok(()),
    }
}

#[cfg(not(unix))]
pub(super) fn check_owner(n: u32) -> Result<(), String> {
    Err(format!(
        "refused: attaching to process {n} is not available on this system because the owner of a process cannot be checked here; read a core file or minidump instead (kind core)"
    ))
}
