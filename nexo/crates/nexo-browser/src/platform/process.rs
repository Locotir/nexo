//! Tying the launched browser to nexo's lifetime.
//!
//! chromiumoxide kills the main browser process when its handle is dropped, but not when nexo itself dies
//! without running destructors (a crash, a `taskkill`, closing the console), and it never touches the helper
//! processes. On Windows the browser is therefore put in a job object that kills everything in it when its
//! handle closes, which the OS does when nexo exits however it exits. On Linux and macOS the helpers exit with
//! the main process, so nothing more is done there.
//!
//! The browser binaries are GUI-subsystem programs (`chrome.exe`, `msedge.exe`, `brave.exe`), so launching
//! them never opens a console window; nothing in this crate starts a console program.

/// Keeps a launched browser and its helper processes alive no longer than the owner of this value.
#[derive(Debug)]
pub struct ProcessGuard {
    #[cfg(windows)]
    /// Held for its Drop: closing the handle ends the browser.
    _job: Option<windows_job::Job>,
}

impl ProcessGuard {
    /// Puts the browser process `pid` and the processes it has already started in a kill-on-close job. Failing
    /// to do so (a locked-down machine) is not an error: the browser then only dies with its handle.
    pub fn adopt(pid: Option<u32>) -> ProcessGuard {
        #[cfg(windows)]
        {
            ProcessGuard { _job: pid.and_then(windows_job::Job::containing) }
        }
        #[cfg(not(windows))]
        {
            let _ = pid;
            ProcessGuard {}
        }
    }

    /// True when the browser is in a job that outlives a crash of nexo.
    #[cfg(test)]
    pub fn contained(&self) -> bool {
        #[cfg(windows)]
        {
            self._job.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

/// One entry of a process listing. `created` is the creation time (any unit, only compared) when it could be read.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Proc {
    pid: u32,
    ppid: u32,
    created: Option<u64>,
}

/// `root` and every process below it, parents before children. Windows keeps a dead parent's pid in its
/// children and reuses pids quickly, so a process only counts as a child if it is not older than its parent;
/// without a creation time on both sides it is not counted at all.
#[cfg(any(windows, test))]
fn family(listing: &[Proc], root: u32) -> Vec<Proc> {
    let Some(first) = listing.iter().find(|p| p.pid == root).copied() else {
        return vec![Proc { pid: root, ppid: 0, created: None }];
    };
    let mut out = vec![first];
    let mut next = 0;
    while next < out.len() {
        let parent = out[next];
        next += 1;
        let Some(born) = parent.created else { continue };
        for p in listing {
            let younger = p.created.is_some_and(|c| c >= born);
            if p.ppid == parent.pid && p.pid != parent.pid && younger && !out.iter().any(|o| o.pid == p.pid) {
                out.push(*p);
            }
        }
    }
    out
}

#[cfg(windows)]
mod windows_job {
    use super::{family, Proc};
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    /// A job object that terminates its processes when the last handle to it closes.
    #[derive(Debug)]
    pub struct Job(HANDLE);

    // SAFETY: a job handle is just a kernel object reference; the Win32 calls on it are thread-safe.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    /// The creation time of an open process, in 100 ns ticks since 1601.
    fn creation_time(process: HANDLE) -> Option<u64> {
        let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: `process` is an open handle and the four out pointers are valid for the call.
        let ok = unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) } != 0;
        ok.then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }

    impl Job {
        /// A new job holding process `pid` and the processes below it, or `None` if the job could not be made
        /// or the browser could not be put in it.
        pub fn containing(pid: u32) -> Option<Job> {
            // SAFETY: plain Win32 calls with valid arguments; every handle opened here is closed here or by
            // `Drop for Job`.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return None;
                }
                let job = Job(handle);
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let set = SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&info).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if set == 0 || !job.assign(pid, None) {
                    return None;
                }
                // Helpers the browser started before it was in the job do not join it on their own.
                for child in family(&process_listing(), pid).into_iter().skip(1) {
                    job.assign(child.pid, child.created);
                }
                Some(job)
            }
        }

        /// Puts process `pid` in the job. With `created` the process must still be the one that listing saw: a
        /// pid that was reused in the meantime is left alone.
        fn assign(&self, pid: u32, created: Option<u64>) -> bool {
            // SAFETY: `process` is closed before returning; `self.0` is a live job handle.
            unsafe {
                let process =
                    OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                if process.is_null() {
                    return false;
                }
                let same = created.is_none() || creation_time(process) == created;
                let done = same && AssignProcessToJobObject(self.0, process) != 0;
                CloseHandle(process);
                done
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle is ours and closed once. Closing the last handle ends the processes in the job.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// Every process, with its creation time when it can be read.
    pub fn process_listing() -> Vec<Proc> {
        let mut out = Vec::new();
        // SAFETY: the snapshot handle is closed before returning; `entry.dwSize` is set as the API requires.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return out;
            }
            let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            let mut more = Process32FirstW(snap, &mut entry) != 0;
            while more {
                let pid = entry.th32ProcessID;
                let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                let created = if process.is_null() {
                    None
                } else {
                    let t = creation_time(process);
                    CloseHandle(process);
                    t
                };
                out.push(Proc { pid, ppid: entry.th32ParentProcessID, created });
                more = Process32NextW(snap, &mut entry) != 0;
            }
            CloseHandle(snap);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, ppid: u32, created: u64) -> Proc {
        Proc { pid, ppid, created: Some(created) }
    }

    fn pids(list: Vec<Proc>) -> Vec<u32> {
        list.into_iter().map(|p| p.pid).collect()
    }

    #[test]
    fn family_walks_down_the_process_tree() {
        // 10 -> 11 -> 13, 10 -> 12, and an unrelated 20 -> 21.
        let listing = [
            proc(11, 10, 101),
            proc(12, 10, 102),
            proc(13, 11, 103),
            proc(20, 1, 50),
            proc(21, 20, 60),
            proc(10, 1, 100),
        ];
        assert_eq!(pids(family(&listing, 10)), vec![10, 11, 12, 13]);
        assert_eq!(pids(family(&listing, 13)), vec![13]);
        assert_eq!(pids(family(&listing, 99)), vec![99]);
    }

    #[test]
    fn family_survives_a_process_that_is_its_own_parent() {
        assert_eq!(pids(family(&[proc(5, 5, 1), proc(6, 5, 2)], 5)), vec![5, 6]);
    }

    #[test]
    fn family_ignores_orphans_whose_recorded_parent_pid_was_reused() {
        // Chrome got pid 10 now. 30 was started by an earlier process that had pid 10 and died; Windows still
        // reports 10 as its parent, but it is older than this Chrome. 11 is a real helper.
        let listing = [proc(10, 1, 1_000), proc(30, 10, 400), proc(11, 10, 1_001), proc(31, 30, 2_000)];
        assert_eq!(pids(family(&listing, 10)), vec![10, 11]);
        // The same goes further down: an old orphan under a real helper.
        let listing = [proc(10, 1, 1_000), proc(11, 10, 1_001), proc(32, 11, 900)];
        assert_eq!(pids(family(&listing, 10)), vec![10, 11]);
    }

    #[test]
    fn family_needs_creation_times_on_both_sides() {
        let unknown = |pid, ppid| Proc { pid, ppid, created: None };
        // A child whose time cannot be read is left out; so are all the children of a parent whose time cannot.
        assert_eq!(pids(family(&[proc(10, 1, 5), unknown(11, 10), proc(12, 10, 6)], 10)), vec![10, 12]);
        assert_eq!(pids(family(&[unknown(10, 1), proc(11, 10, 6)], 10)), vec![10]);
    }

    #[test]
    fn adopting_nothing_is_harmless() {
        assert!(!ProcessGuard::adopt(None).contained());
    }

    #[cfg(windows)]
    mod job {
        use super::super::windows_job::process_listing;
        use super::super::*;
        use std::time::{Duration, Instant};
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        /// `STILL_ACTIVE`, the exit code of a running process.
        const RUNNING: u32 = 259;

        fn alive(pid: u32) -> bool {
            // SAFETY: the handle is closed before returning.
            unsafe {
                let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                if h.is_null() {
                    return false;
                }
                let mut code = 0u32;
                let ok = GetExitCodeProcess(h, &mut code) != 0;
                CloseHandle(h);
                ok && code == RUNNING
            }
        }

        fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
            let start = Instant::now();
            while !done() {
                assert!(start.elapsed() < Duration::from_secs(10), "{what}");
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        /// `cmd.exe` running `ping` for a minute: `ping.exe` is a child of `cmd.exe`.
        fn spawn_cmd_with_child() -> (std::process::Child, u32) {
            let child = std::process::Command::new("cmd.exe")
                .args(["/C", "ping -n 60 127.0.0.1 >NUL"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let me = child.id();
            let mut grandchild = None;
            wait_until("cmd.exe never started ping.exe", || {
                grandchild = process_listing().into_iter().find(|p| p.ppid == me).map(|p| p.pid);
                grandchild.is_some()
            });
            (child, grandchild.unwrap())
        }

        #[test]
        fn a_job_ends_its_process_and_its_helpers_when_the_guard_drops() {
            // The helper was already running when the process was adopted, like Chrome's own helpers.
            let (mut child, helper) = spawn_cmd_with_child();
            let guard = ProcessGuard::adopt(Some(child.id()));
            assert!(guard.contained(), "the child could not be put in a job");
            assert!(child.try_wait().unwrap().is_none(), "exited before the guard dropped");
            assert!(alive(helper), "the helper exited before the guard dropped");
            drop(guard);
            wait_until("the child outlived its job", || child.try_wait().unwrap().is_some());
            wait_until("the helper outlived its job", || !alive(helper));
        }

        #[test]
        fn a_helper_started_after_adoption_is_in_the_job_too() {
            // cmd.exe runs two pings one after the other: the second is started after the job exists and must
            // inherit it.
            let mut child = std::process::Command::new("cmd.exe")
                .args(["/C", "ping -n 3 127.0.0.1 >NUL & ping -n 60 127.0.0.1 >NUL"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let me = child.id();
            let guard = ProcessGuard::adopt(Some(me));
            assert!(guard.contained());
            // The first ping ends after about two seconds and the second one starts then.
            let mut seen: Vec<u32> = Vec::new();
            wait_until("cmd.exe never started the second ping.exe", || {
                for p in process_listing().into_iter().filter(|p| p.ppid == me) {
                    if !seen.contains(&p.pid) {
                        seen.push(p.pid);
                    }
                }
                seen.len() >= 2 && alive(seen[seen.len() - 1])
            });
            let late = seen[seen.len() - 1];
            drop(guard);
            wait_until("the child outlived its job", || child.try_wait().unwrap().is_some());
            wait_until("the late helper outlived its job", || !alive(late));
        }
    }
}
