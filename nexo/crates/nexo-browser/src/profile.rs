//! The folder a browser keeps its cookies and logins in: opening it, and telling a profile another Chrome is
//! using from one that only has a lock left over from a Chrome that crashed.
//!
//! Chrome allows one browser per `--user-data-dir`. On Linux and macOS it marks the folder with a
//! `SingletonLock` symlink to `<hostname>-<pid>`; on Windows it holds `lockfile` open. A second launch on
//! the same folder does not fail: it hands the request to the first Chrome and exits, so the caller would
//! wait on a browser that never comes up. [`open`] looks first and says what holds the folder.

use crate::{Browser, Error, Profile, Result};
use std::path::{Path, PathBuf};

/// Names of the processes that may hold a profile (the process name, lowercase, contains one of these).
#[cfg(unix)]
const BROWSER_NAMES: [&str; 6] = ["chrom", "edge", "brave", "opera", "vivaldi", "headless_shell"];

/// What the folder's lock says.
#[derive(Debug, PartialEq, Eq)]
enum Lock {
    /// Nothing marks the folder as in use.
    Free,
    /// A lock is there but nothing holds it (the browser crashed or was killed).
    /// (On Windows the lock file is removed while it is judged, so only Unix finds this.)
    #[cfg_attr(windows, allow(dead_code))]
    Stale,
    /// A browser has it, or may have it: the lock names another machine, or the process cannot be told.
    Held(Option<u32>),
}

/// What a process id is.
#[derive(Debug, PartialEq, Eq)]
#[cfg(unix)]
enum Process {
    Gone,
    /// Alive, and a web browser by its name.
    Browser,
    /// Alive but not a browser: the id was reused after the browser died.
    Other,
}

/// The profile folder to launch with, and the temporary folder that owns it when the profile is ephemeral
/// (dropped, it deletes the folder). A kept profile is created when missing and checked for a live lock.
pub(crate) fn open(profile: Profile) -> Result<(Option<crate::platform::TempProfile>, PathBuf)> {
    match profile {
        Profile::Ephemeral => {
            let tmp = crate::platform::TempProfile::create()?;
            let dir = tmp.path().to_path_buf();
            Ok((Some(tmp), dir))
        }
        Profile::Persistent(dir) => {
            create_private(&dir)?;
            claim(&dir)?;
            Ok((None, dir))
        }
    }
}

/// The kept profile holds the person's logins, so only they may read it: created `0700` on Unix, and tightened
/// when it already exists with looser rights (Chrome does not change an existing folder).
fn create_private(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)
}

/// The browser program to launch: the explicit `path`, or the first installed one.
pub(crate) fn resolve_executable(explicit: Option<PathBuf>) -> Result<PathBuf> {
    match explicit {
        Some(p) if p.is_file() => Ok(p),
        Some(p) => Err(Error::BadExecutable(p)),
        None => crate::find_browser().ok_or_else(|| Error::NoBrowser {
            tried: crate::platform::tried_names(),
            note: crate::platform::override_problem().map(|p| format!("; {p}")).unwrap_or_default(),
        }),
    }
}

/// Every reason [`Browser::launch`] would refuse `opts` that can be known without starting Chrome: no such
/// executable, no screen for a window, and (when `profile_free` is true) a kept profile another browser holds.
/// Pass `profile_free = false` when the browser about to be closed is the one holding the profile.
pub fn preflight(opts: &crate::LaunchOptions, profile_free: bool) -> Result<()> {
    resolve_executable(opts.executable.clone())?;
    if !opts.headless {
        require_display()?;
    }
    if let (Profile::Persistent(dir), true) = (&opts.profile, profile_free) {
        claim(dir)?;
    }
    Ok(())
}

/// Fails with [`Error::ProfileInUse`] when a browser holds `dir`; removes the lock a dead browser left.
pub(crate) fn claim(dir: &Path) -> Result<()> {
    match lock_state(dir) {
        Lock::Free => Ok(()),
        Lock::Stale => {
            clear(dir);
            Ok(())
        }
        Lock::Held(pid) => Err(Error::ProfileInUse { dir: dir.to_path_buf(), pid }),
    }
}

/// Whether a screen exists to show a window on. Always true off Linux, where a window needs no variable.
pub fn has_display() -> bool {
    #[cfg(target_os = "macos")]
    if !mac_has_screen() {
        return false;
    }
    display_from(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
}

fn display_from(set: impl Fn(&str) -> bool) -> bool {
    !cfg!(target_os = "linux") || set("DISPLAY") || set("WAYLAND_DISPLAY")
}

/// macOS has no variable that says a screen exists. The one case known to have none is a login over ssh while
/// somebody else (or nobody: `root` at the login window) owns the console, so only that case is refused.
fn mac_session_has_screen(over_ssh: bool, console_user: Option<&str>, me: Option<&str>) -> bool {
    match (over_ssh, console_user, me) {
        (true, Some(console), Some(me)) => console == me,
        _ => true,
    }
}

#[cfg(target_os = "macos")]
fn mac_has_screen() -> bool {
    if std::env::var_os("SSH_CONNECTION").is_none_or(|v| v.is_empty()) {
        return true;
    }
    let console = std::process::Command::new("stat")
        .args(["-f", "%Su", "/dev/console"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());
    let me = std::env::var("USER").ok();
    mac_session_has_screen(true, console.as_deref().filter(|s| !s.is_empty()), me.as_deref())
}

/// Refuses a visible window where there is no screen: Chrome would die with an obscure message.
pub(crate) fn require_display() -> Result<()> {
    if has_display() {
        Ok(())
    } else {
        Err(Error::NoDisplay)
    }
}

#[cfg(unix)]
fn lock_state(dir: &Path) -> Lock {
    unix_lock_state(dir, &hostname(), probe)
}

#[cfg(unix)]
fn unix_lock_state(dir: &Path, host: &str, probe: impl Fn(u32) -> Process) -> Lock {
    // a symlink, so `read_link` (the target is not a file)
    let Ok(target) = std::fs::read_link(dir.join("SingletonLock")) else {
        return if dir.join("SingletonLock").symlink_metadata().is_ok() { Lock::Stale } else { Lock::Free };
    };
    let target = target.to_string_lossy().into_owned();
    let Some((lock_host, pid)) = target.rsplit_once('-').and_then(|(h, p)| Some((h, p.parse::<u32>().ok()?))) else {
        // not what Chrome writes: no live Chrome made it
        return Lock::Stale;
    };
    if !same_host(lock_host, host, cfg!(target_os = "macos")) {
        // the folder may be shared with another machine, whose process cannot be looked at from here
        return Lock::Held(None);
    }
    match probe(pid) {
        Process::Browser => Lock::Held(Some(pid)),
        Process::Gone | Process::Other => Lock::Stale,
    }
}

/// Whether the host in a lock is this machine. On macOS the name the system call gives changes with the network
/// (`Mac.local`, `mac`, a name from DHCP), so only the first label counts and case does not.
#[cfg(unix)]
fn same_host(lock_host: &str, host: &str, loose: bool) -> bool {
    if lock_host == host {
        return true;
    }
    let first = |h: &str| h.split('.').next().unwrap_or(h).to_ascii_lowercase();
    loose && !first(host).is_empty() && first(lock_host) == first(host)
}

#[cfg(unix)]
fn clear(dir: &Path) {
    for name in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

#[cfg(unix)]
fn hostname() -> String {
    // the same call Chrome makes when it writes `<host>-<pid>` into SingletonLock; the `hostname` command can
    // differ from it on macOS when the name was set with scutil
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is writable for its whole length, and gethostname writes at most that many bytes.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc == 0 {
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        let name = String::from_utf8_lossy(&buf[..end]).trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    if let Ok(h) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        return h.trim().to_string();
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn probe(pid: u32) -> Process {
    // `pid (name) S ...`: the name may hold spaces and parentheses, so cut at the last `)`
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return Process::Gone };
    let Some((head, tail)) = stat.rsplit_once(')') else { return Process::Gone };
    let name = head.split_once('(').map_or("", |(_, n)| n).to_ascii_lowercase();
    if tail.trim_start().starts_with('Z') {
        return Process::Gone;
    }
    classify(&name)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn probe(pid: u32) -> Process {
    let out = std::process::Command::new("ps").args(["-p", &pid.to_string(), "-o", "comm="]).output();
    match out {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => {
            classify(&String::from_utf8_lossy(&o.stdout).to_ascii_lowercase())
        }
        _ => Process::Gone,
    }
}

#[cfg(unix)]
fn classify(name: &str) -> Process {
    // macOS `ps` prints the whole path: Arc is not in the list, because "arc" would match unrelated names
    let program = name.trim().rsplit_once('/').map(|(_, last)| last);
    if BROWSER_NAMES.iter().any(|b| name.contains(b)) || program == Some("arc") {
        Process::Browser
    } else {
        Process::Other
    }
}

#[cfg(windows)]
fn lock_state(dir: &Path) -> Lock {
    // Chrome holds `lockfile` open without sharing it and deletes it on exit: a file that cannot be removed
    // is held, one that can be removed was left by a crash.
    let file = dir.join("lockfile");
    if !file.exists() {
        return Lock::Free;
    }
    match std::fs::remove_file(&file) {
        Ok(()) => Lock::Free,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Lock::Free,
        Err(_) => Lock::Held(None),
    }
}

#[cfg(windows)]
fn clear(dir: &Path) {
    let _ = std::fs::remove_file(dir.join("lockfile"));
}

impl Browser {
    /// The folder the browser keeps its profile in. For an ephemeral profile it is deleted on [`Browser::close`].
    pub fn profile_dir(&self) -> &Path {
        &self.dir
    }

    /// Whether the profile is kept after [`Browser::close`].
    pub fn is_persistent(&self) -> bool {
        self.profile.is_none()
    }

    /// Whether the browser runs without a window.
    pub fn is_headless(&self) -> bool {
        self.headless
    }

    /// Whether the browser still answers: false once its window was closed by the person or it crashed.
    pub async fn is_alive(&self) -> bool {
        matches!(tokio::time::timeout(std::time::Duration::from_secs(3), self.chrome.version()).await, Ok(Ok(_)))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn link(dir: &Path, target: &str) {
        std::os::unix::fs::symlink(target, dir.join("SingletonLock")).unwrap();
    }

    #[test]
    fn the_host_name_is_the_one_the_system_call_gives_and_not_empty() {
        let host = hostname();
        assert!(!host.is_empty());
        let out = std::process::Command::new("hostname").output();
        if let Ok(o) = out {
            assert_eq!(host, String::from_utf8_lossy(&o.stdout).trim());
        }
    }

    #[test]
    fn a_macos_browser_path_is_a_browser() {
        for p in [
            "/applications/google chrome.app/contents/macos/google chrome",
            "/applications/microsoft edge.app/contents/macos/microsoft edge",
            "/applications/brave browser.app/contents/macos/brave browser",
        ] {
            assert_eq!(classify(p), Process::Browser, "{p}");
        }
        assert_eq!(classify("/usr/bin/vim"), Process::Other);
    }

    #[test]
    fn a_kept_profile_is_private_to_the_person() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let d = tempfile::tempdir().unwrap();
        let profile = d.path().join("browser").join("profile");
        let (tmp, dir) = open(Profile::Persistent(profile.clone())).unwrap();
        assert!(tmp.is_none() && dir == profile);
        assert_eq!((mode(&profile), mode(&d.path().join("browser"))), (0o700, 0o700));
        // an older folder with looser rights is tightened
        std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o755)).unwrap();
        open(Profile::Persistent(profile.clone())).unwrap();
        assert_eq!(mode(&profile), 0o700);
    }

    #[test]
    fn preflight_finds_what_would_stop_a_launch_without_starting_chrome() {
        let d = tempfile::tempdir().unwrap();
        let bad = crate::LaunchOptions { executable: Some(d.path().join("nope")), ..Default::default() };
        assert!(matches!(preflight(&bad, true), Err(Error::BadExecutable(_))));
        // a profile another browser holds, unless the holder is the one about to be closed
        let host = hostname();
        let profile = d.path().join("p");
        std::fs::create_dir_all(&profile).unwrap();
        link(&profile, &format!("{host}-{}", std::process::id()));
        let exe = d.path().join("chrome");
        std::fs::write(&exe, "").unwrap();
        let opts = crate::LaunchOptions {
            executable: Some(exe),
            profile: Profile::Persistent(profile.clone()),
            ..Default::default()
        };
        // this process is no browser by name, so the lock reads as stale and is cleared
        assert!(preflight(&opts, true).is_ok());
        let other = d.path().join("q");
        std::fs::create_dir_all(&other).unwrap();
        link(&other, "elsewhere-1");
        let held = crate::LaunchOptions { profile: Profile::Persistent(other.clone()), ..opts.clone() };
        assert!(matches!(preflight(&held, true), Err(Error::ProfileInUse { .. })));
        assert!(preflight(&held, false).is_ok(), "the lock is not looked at when the holder is being closed");
    }

    #[test]
    fn no_lock_means_free() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(unix_lock_state(d.path(), "box", |_| Process::Gone), Lock::Free);
    }

    #[test]
    fn a_lock_of_a_live_browser_holds_the_profile() {
        let d = tempfile::tempdir().unwrap();
        link(d.path(), "box-4242");
        assert_eq!(
            unix_lock_state(d.path(), "box", |p| if p == 4242 { Process::Browser } else { Process::Gone }),
            Lock::Held(Some(4242))
        );
    }

    #[test]
    fn a_lock_of_a_dead_or_reused_pid_is_stale() {
        let d = tempfile::tempdir().unwrap();
        link(d.path(), "box-4242");
        assert_eq!(unix_lock_state(d.path(), "box", |_| Process::Gone), Lock::Stale);
        assert_eq!(unix_lock_state(d.path(), "box", |_| Process::Other), Lock::Stale);
    }

    #[test]
    fn a_lock_from_another_machine_is_never_judged() {
        let d = tempfile::tempdir().unwrap();
        link(d.path(), "elsewhere-4242");
        assert_eq!(unix_lock_state(d.path(), "box", |_| Process::Gone), Lock::Held(None));
        // a hostname with dashes still splits at the last one
        let d = tempfile::tempdir().unwrap();
        link(d.path(), "my-laptop-17");
        assert_eq!(unix_lock_state(d.path(), "my-laptop", |_| Process::Browser), Lock::Held(Some(17)));
    }

    #[test]
    fn a_lock_that_is_not_what_chrome_writes_is_stale() {
        for target in ["garbage", "box-notanumber"] {
            let d = tempfile::tempdir().unwrap();
            link(d.path(), target);
            assert_eq!(unix_lock_state(d.path(), "box", |_| Process::Browser), Lock::Stale, "{target:?}");
        }
        // a regular file in its place
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("SingletonLock"), "x").unwrap();
        assert_eq!(unix_lock_state(d.path(), "box", |_| Process::Browser), Lock::Stale);
    }

    #[test]
    fn claiming_a_stale_profile_removes_what_the_dead_browser_left() {
        let d = tempfile::tempdir().unwrap();
        let (host, dead) = (hostname(), u32::MAX - 1);
        link(d.path(), &format!("{host}-{dead}"));
        std::os::unix::fs::symlink("/nonexistent/sock", d.path().join("SingletonSocket")).unwrap();
        std::os::unix::fs::symlink("123", d.path().join("SingletonCookie")).unwrap();
        std::fs::write(d.path().join("Cookies"), "keep").unwrap();
        claim(d.path()).unwrap();
        for name in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
            assert!(d.path().join(name).symlink_metadata().is_err(), "{name} left behind");
        }
        assert!(d.path().join("Cookies").is_file(), "the person's data stays");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn probing_this_very_process_finds_a_live_process_that_is_not_a_browser() {
        // the test binary is no browser, so a lock naming it reads as a reused id
        assert_eq!(probe(std::process::id()), Process::Other);
        assert_eq!(probe(u32::MAX - 1), Process::Gone);
    }

    #[test]
    fn arc_is_a_browser_by_its_program_and_unrelated_names_are_not() {
        assert_eq!(classify("/applications/arc.app/contents/macos/arc\n"), Process::Browser);
        for other in ["/usr/bin/search", "arc", "/usr/bin/arch", "/users/a/archive"] {
            assert_eq!(classify(other), Process::Other, "{other}");
        }
    }

    #[test]
    fn a_macos_host_name_that_changed_spelling_is_still_this_machine() {
        let d = tempfile::tempdir().unwrap();
        link(d.path(), "Mac-123");
        // as macOS compares: first label, any case
        assert!(same_host("Mac", "Mac.local", true) && same_host("mac", "MAC.lan", true));
        assert!(!same_host("Mac", "Other.local", true) && !same_host("Mac", "Mac.local", false));
        let stale = |host: &str| unix_lock_state(d.path(), host, |_| Process::Gone);
        assert_eq!(stale("Mac"), Lock::Stale);
        assert_eq!(stale("Mac.local"), if cfg!(target_os = "macos") { Lock::Stale } else { Lock::Held(None) });
    }

    #[test]
    fn a_visible_launch_is_refused_over_ssh_only_when_the_console_belongs_to_someone_else() {
        assert!(mac_session_has_screen(false, Some("root"), Some("ana")));
        assert!(mac_session_has_screen(true, Some("ana"), Some("ana")));
        assert!(!mac_session_has_screen(true, Some("root"), Some("ana")));
        // not knowing is not a reason to refuse
        assert!(mac_session_has_screen(true, None, Some("ana")));
        assert!(mac_session_has_screen(true, Some("root"), None));
    }

    #[test]
    fn browsers_are_told_by_name() {
        for name in ["chrome", "chromium", "msedge", "brave", "chrome-headless", "headless_shell", "vivaldi-bin"] {
            assert_eq!(classify(name), Process::Browser, "{name}");
        }
        for name in ["nexo", "bash", "firefox"] {
            assert_eq!(classify(name), Process::Other, "{name}");
        }
    }

    #[test]
    fn a_screen_is_needed_for_a_window_on_linux_only() {
        assert_eq!(display_from(|_| false), !cfg!(target_os = "linux"));
        assert!(display_from(|k| k == "DISPLAY"));
        assert!(display_from(|k| k == "WAYLAND_DISPLAY"));
    }
}
