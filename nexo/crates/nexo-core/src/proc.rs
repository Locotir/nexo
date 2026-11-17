//! Whether a process runs, and when it started, where the system has no `/proc` or `kill(pid, 0)`.
//!
//! Only Windows has a body here: Linux, macOS and the other Unixes answer these questions in the callers
//! (a `kill` probe, `/proc`, `ps`). Everywhere else both functions say `None`, so a caller keeps whatever
//! it did before it asked (judge by age, or take the pid as alive).

/// Whether process `pid` runs. `None` when the system cannot tell, so that the caller falls back to
/// whatever else it knows (usually how long ago the process last touched its file).
pub fn pid_alive(pid: u32) -> Option<bool> {
    #[cfg(windows)]
    {
        windows_probe::alive(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        None
    }
}

/// When process `pid` started, as a number that only means something compared with the same process asked
/// again later (100 ns ticks since 1601 on Windows). Pids are reused fast there, so a start stamp is what
/// tells a process from another one that got its pid. `None` when the system does not say.
pub fn process_start(pid: u32) -> Option<u64> {
    #[cfg(windows)]
    {
        windows_probe::started(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        None
    }
}

#[cfg(windows)]
mod windows_probe {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, FILETIME, HANDLE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// `STILL_ACTIVE`, the exit code of a running process.
    const RUNNING: u32 = 259;

    /// Runs `f` on a handle of `pid` that can be asked about its state, closing it after. `Err` is the Win32
    /// error of the open that failed.
    fn with_process<T>(pid: u32, f: impl FnOnce(HANDLE) -> T) -> Result<T, u32> {
        // SAFETY: the handle is closed before returning, whatever `f` returns.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return Err(GetLastError());
            }
            let out = f(handle);
            CloseHandle(handle);
            Ok(out)
        }
    }

    pub fn alive(pid: u32) -> Option<bool> {
        if pid == 0 {
            return Some(false);
        }
        let running = |handle: HANDLE| {
            let mut code = 0u32;
            // SAFETY: `handle` is open and `code` is valid for the call.
            let ok = unsafe { GetExitCodeProcess(handle, &mut code) } != 0;
            // A process that exited with code 259 looks running; the caller's age check then decides, which is
            // harmless.
            ok.then_some(code == RUNNING)
        };
        match with_process(pid, running) {
            Ok(state) => state,
            // no process has this pid
            Err(ERROR_INVALID_PARAMETER) => Some(false),
            // a process of another user, or an elevated one, that this one may not open: it exists
            Err(ERROR_ACCESS_DENIED) => Some(true),
            Err(_) => None,
        }
    }

    pub fn started(pid: u32) -> Option<u64> {
        let creation = |handle: HANDLE| {
            let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
            let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
            // SAFETY: `handle` is open and the four out pointers are valid for the call.
            let ok = unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } != 0;
            ok.then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
        };
        with_process(pid, creation).ok().flatten()
    }
}

/// The pid of a child that has already exited, with its handle released.
#[cfg(all(test, windows))]
pub(crate) fn exited_pid() -> u32 {
    let mut child = std::process::Command::new("cmd").args(["/C", "exit"]).spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    drop(child);
    pid
}

/// Whether `check` holds for the pid of an exited child, tried on up to five fresh ones. Windows hands freed pids
/// to the next spawn, and tests run as parallel threads, so another test can own the pid when `check` looks; a
/// new pid per try makes that rare miss pass, while a real fault still fails all five.
#[cfg(all(test, windows))]
pub(crate) fn holds_for_an_exited_pid(check: impl Fn(u32) -> bool) -> bool {
    (0..5).any(|_| check(exited_pid()))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_process_that_has_exited_is_not_alive() {
        assert!(holds_for_an_exited_pid(|pid| pid_alive(pid) == Some(false)));
    }

    #[test]
    fn this_process_is_alive_and_has_a_stable_start_time() {
        let me = std::process::id();
        assert_eq!(pid_alive(me), Some(true));
        let first = process_start(me);
        assert!(first.is_some());
        assert_eq!(first, process_start(me));
    }

    #[test]
    fn pid_zero_is_not_alive() {
        assert_eq!(pid_alive(0), Some(false));
    }
}
