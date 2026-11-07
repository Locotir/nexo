//! The OS sandbox of the `bash` tool.
//!
//! The settings (mode, network, extra directories) are `nexo_core::sandbox`. Here is what enforces
//! them, one backend per platform:
//!
//! - Linux: Landlock (set in the child just before `exec`, with the `landlock` crate), plus a fresh
//!   network namespace for "network off" (Landlock's own TCP rules where a namespace cannot be made).
//!   Where the kernel has no Landlock, `bwrap` (bubblewrap) when it is on the PATH.
//! - macOS: `sandbox-exec` with a generated profile.
//! - Windows: nothing yet. `wrap` says so, and the tool asks before every command instead.
//!
//! Setting `NEXO_SANDBOX_BACKEND=landlock|bwrap` forces a Linux backend (tests, and bwrap by choice).
//! A sandbox that was asked for and cannot be set up never falls back to running unsandboxed.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use nexo_core::sandbox::{SandboxMode, SandboxSettings};

/// What enforces the sandbox on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Landlock,
    Bwrap(PathBuf),
    SandboxExec(PathBuf),
    /// No way to sandbox here; the text says why.
    Unavailable(String),
}

impl Backend {
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Landlock => "landlock",
            Backend::Bwrap(_) => "bwrap",
            Backend::SandboxExec(_) => "sandbox-exec",
            Backend::Unavailable(_) => "none",
        }
    }

    pub fn usable(&self) -> bool {
        !matches!(self, Backend::Unavailable(_))
    }
}

/// The backend of this machine, found once.
pub fn backend() -> &'static Backend {
    static B: OnceLock<Backend> = OnceLock::new();
    B.get_or_init(detect)
}

#[cfg(target_os = "linux")]
fn detect() -> Backend {
    let forced = std::env::var("NEXO_SANDBOX_BACKEND").unwrap_or_default();
    let bwrap = || nexo_core::shell::find_program("bwrap").map(Backend::Bwrap);
    match forced.as_str() {
        "bwrap" => {
            return bwrap().unwrap_or_else(|| Backend::Unavailable("bwrap was asked for and is not on the PATH".into()))
        }
        "landlock" => {
            return if landlock_abi() > 0 {
                Backend::Landlock
            } else {
                Backend::Unavailable("landlock was asked for and this kernel has none".into())
            }
        }
        _ => {}
    }
    if landlock_abi() > 0 {
        return Backend::Landlock;
    }
    bwrap().unwrap_or_else(|| {
        Backend::Unavailable(
            "this kernel has no Landlock and bwrap (bubblewrap) is not installed; install bubblewrap, \
             or turn the sandbox off"
                .into(),
        )
    })
}

#[cfg(target_os = "macos")]
fn detect() -> Backend {
    match Path::new("/usr/bin/sandbox-exec").is_file() {
        true => Backend::SandboxExec(PathBuf::from("/usr/bin/sandbox-exec")),
        false => Backend::Unavailable("/usr/bin/sandbox-exec is missing".into()),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn detect() -> Backend {
    Backend::Unavailable(format!(
        "not available on {} yet",
        if cfg!(windows) { "Windows" } else { std::env::consts::OS }
    ))
}

/// The Landlock ABI the kernel speaks (0: none).
#[cfg(target_os = "linux")]
#[allow(clippy::useless_conversion)]
fn landlock_abi() -> i64 {
    // landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION); the number is 444 everywhere
    let r = unsafe { libc::syscall(444, std::ptr::null::<libc::c_void>(), 0usize, 1u32) };
    i64::from(r).max(0)
}

/// Whether a fresh user and network namespace can be made here (found once, by trying in a child).
#[cfg(target_os = "linux")]
fn netns_works() -> bool {
    use std::os::unix::process::CommandExt;
    static W: OnceLock<bool> = OnceLock::new();
    *W.get_or_init(|| {
        let mut c = std::process::Command::new("true");
        // SAFETY: the closure only calls `unshare`, which is async-signal-safe
        unsafe {
            c.pre_exec(|| {
                if libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        c.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// How the network is shut when `network` is off, in words.
pub fn network_block() -> &'static str {
    match backend() {
        #[cfg(target_os = "linux")]
        Backend::Landlock if netns_works() => "network namespace",
        #[cfg(target_os = "linux")]
        Backend::Landlock if landlock_abi() >= 4 => "landlock tcp rules",
        #[cfg(target_os = "linux")]
        Backend::Landlock => "none (this kernel cannot block it)",
        Backend::Bwrap(_) => "network namespace",
        Backend::SandboxExec(_) => "profile",
        _ => "none",
    }
}

/// Whether a command in this mode runs without asking: the sandbox is on and a backend enforces it.
pub fn auto_allows(s: &SandboxSettings) -> bool {
    s.mode.is_on() && backend().usable()
}

/// One line for `/status` and the `/sandbox` answer.
pub fn describe(s: &SandboxSettings) -> String {
    if !s.mode.is_on() {
        return "off".into();
    }
    let net = if s.network { "network on".to_string() } else { format!("network off ({})", network_block()) };
    match backend() {
        Backend::Unavailable(why) if cfg!(windows) => {
            format!("{} requested, {why}: every command asks first", s.mode.as_str())
        }
        Backend::Unavailable(why) => format!("{} requested but cannot run: {why}", s.mode.as_str()),
        b => format!("{} · {} · {net}", s.mode.as_str(), b.name()),
    }
}

/// Why a command could not be sandboxed, or `None` when it falls back to asking (no sandbox on the
/// platform yet).
fn refusal(why: &str) -> Result<Option<tokio::process::Command>, String> {
    if cfg!(windows) {
        return Ok(None);
    }
    Err(format!(
        "the sandbox is on and cannot run here ({why}). Nothing was run. Turn it off with `/sandbox off` \
         (or `--sandbox off`) to run commands without it."
    ))
}

/// The command to spawn for `program args`, inside the sandbox of `s`. `Ok(None)`: this platform
/// has no sandbox and the tool asks first instead; the caller then runs it plain. `Err`: refused.
pub fn wrap(
    s: &SandboxSettings,
    cwd: &Path,
    program: &Path,
    args: &[String],
) -> Result<Option<tokio::process::Command>, String> {
    wrap_with(s, cwd, program, args, false)
}

/// Like `wrap`, for a command that must outlive this process: the same rules, but nothing ties it
/// to its parent (`bwrap` is not asked to die with it).
pub fn wrap_detached(
    s: &SandboxSettings,
    cwd: &Path,
    program: &Path,
    args: &[String],
) -> Result<Option<tokio::process::Command>, String> {
    wrap_with(s, cwd, program, args, true)
}

fn wrap_with(
    s: &SandboxSettings,
    cwd: &Path,
    program: &Path,
    args: &[String],
    detach: bool,
) -> Result<Option<tokio::process::Command>, String> {
    if !s.mode.is_on() {
        return Ok(None);
    }
    let roots = s.write_roots(cwd);
    match backend() {
        Backend::Unavailable(why) => refusal(why),
        Backend::Landlock => landlock_command(s, &roots, program, args).map(Some),
        Backend::Bwrap(bwrap) => {
            let mut c = tokio::process::Command::new(bwrap);
            c.args(bwrap_args(s, &roots, cwd, program, args, detach));
            Ok(Some(c))
        }
        Backend::SandboxExec(se) => {
            let mut c = tokio::process::Command::new(se);
            c.arg("-p").arg(macos_profile(s, &roots)).arg(program).args(args);
            Ok(Some(c))
        }
    }
}

#[cfg(target_os = "linux")]
fn landlock_command(
    s: &SandboxSettings,
    roots: &[PathBuf],
    program: &Path,
    args: &[String],
) -> Result<tokio::process::Command, String> {
    use landlock::{
        Access, AccessFs, AccessNet, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus, ABI,
    };
    use std::ffi::CString;
    let netns = !s.network && netns_works();
    let tcp_rules = !s.network && !netns;
    let fs = ABI::V5;
    let err = |e: &dyn std::fmt::Display| format!("cannot set up the landlock ruleset: {e}");
    let mut rs = Ruleset::default().handle_access(AccessFs::from_all(fs)).map_err(|e| err(&e))?;
    if tcp_rules {
        rs = rs.handle_access(AccessNet::from_all(ABI::V4)).map_err(|e| err(&e))?;
    }
    let mut rs = rs.create().map_err(|e| err(&e))?;
    rs = rs
        .add_rule(PathBeneath::new(PathFd::new("/").map_err(|e| err(&e))?, AccessFs::from_read(fs)))
        .map_err(|e| err(&e))?;
    for r in roots {
        rs = rs
            .add_rule(PathBeneath::new(PathFd::new(r).map_err(|e| err(&e))?, AccessFs::from_all(fs)))
            .map_err(|e| err(&e))?;
    }
    // the maps are made before the fork: the child must not allocate
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let uid_map = format!("{uid} {uid} 1\n").into_bytes();
    let gid_map = format!("{gid} {gid} 1\n").into_bytes();
    let paths = [
        CString::new("/proc/self/uid_map").map_err(|e| err(&e))?,
        CString::new("/proc/self/setgroups").map_err(|e| err(&e))?,
        CString::new("/proc/self/gid_map").map_err(|e| err(&e))?,
    ];
    let slot = std::sync::Mutex::new(Some(rs));
    let mut c = tokio::process::Command::new(program);
    c.args(args);
    // SAFETY: runs in the forked child before exec. It makes system calls and takes the ruleset
    // made above; a failure to restrict aborts the spawn, so a command never runs unsandboxed.
    unsafe {
        c.pre_exec(move || {
            if netns {
                if libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // the process keeps its own ids inside; a failure here only changes how ids print
                write_file(&paths[0], &uid_map);
                write_file(&paths[1], b"deny");
                write_file(&paths[2], &gid_map);
                loopback_up();
            }
            let rs = slot.lock().ok().and_then(|mut g| g.take());
            let Some(rs) = rs else {
                return Err(std::io::Error::other("sandbox ruleset missing"));
            };
            match rs.restrict_self() {
                Ok(st) if st.ruleset != RulesetStatus::NotEnforced => Ok(()),
                Ok(_) => Err(std::io::Error::other("the kernel did not enforce the sandbox")),
                Err(e) => Err(std::io::Error::other(e.to_string())),
            }
        });
    }
    Ok(c)
}

#[cfg(not(target_os = "linux"))]
fn landlock_command(
    _: &SandboxSettings,
    _: &[PathBuf],
    _: &Path,
    _: &[String],
) -> Result<tokio::process::Command, String> {
    Err("landlock exists on Linux only".into())
}

#[cfg(target_os = "linux")]
fn write_file(path: &std::ffi::CStr, data: &[u8]) {
    // SAFETY: plain open, write and close on a path and bytes made before the fork
    unsafe {
        let fd = libc::open(path.as_ptr(), libc::O_WRONLY);
        if fd >= 0 {
            libc::write(fd, data.as_ptr().cast(), data.len());
            libc::close(fd);
        }
    }
}

/// Brings `lo` up in the new network namespace, so a command can still talk to its own children
/// over 127.0.0.1 (a test that starts a server). The host's listeners stay out of reach.
#[cfg(target_os = "linux")]
fn loopback_up() {
    // SAFETY: a socket and one ioctl on a zeroed ifreq; nothing here allocates
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
        if fd < 0 {
            return;
        }
        let mut req: libc::ifreq = std::mem::zeroed();
        for (i, b) in b"lo\0".iter().enumerate() {
            req.ifr_name[i] = *b as libc::c_char;
        }
        req.ifr_ifru.ifru_flags = (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
        libc::ioctl(fd, libc::SIOCSIFFLAGS as _, &req);
        libc::close(fd);
    }
}

/// The arguments for `bwrap`: the disk read-only, the write roots on top, a network namespace when
/// the network is off. A `detach`ed command is not killed with its parent.
pub fn bwrap_args(
    s: &SandboxSettings,
    roots: &[PathBuf],
    cwd: &Path,
    program: &Path,
    args: &[String],
    detach: bool,
) -> Vec<String> {
    let mut a: Vec<String> = ["--ro-bind", "/", "/", "--dev-bind", "/dev", "/dev"].map(String::from).to_vec();
    for r in roots.iter().filter(|r| r.as_path() != Path::new("/dev")) {
        let p = r.to_string_lossy().into_owned();
        a.extend(["--bind".to_string(), p.clone(), p]);
    }
    if !detach {
        a.push("--die-with-parent".into());
    }
    if !s.network {
        a.push("--unshare-net".into());
    }
    a.extend(["--chdir".to_string(), cwd.to_string_lossy().into_owned(), "--".to_string()]);
    a.push(program.to_string_lossy().into_owned());
    a.extend(args.iter().cloned());
    a
}

/// The `sandbox-exec` profile: everything allowed, then writes and (unless allowed) the network
/// shut. Local unix sockets stay open, a build tool talks to its own daemon over one.
pub fn macos_profile(s: &SandboxSettings, roots: &[PathBuf]) -> String {
    let quote = |p: &Path| p.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    let mut out = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
    out.push_str("(allow file-write*\n");
    for r in roots {
        out.push_str(&format!("  (subpath \"{}\")\n", quote(r)));
    }
    out.push_str("  (literal \"/dev/null\") (literal \"/dev/tty\") (regex #\"^/dev/(fd|ttys?[0-9]*|pts)/\"))\n");
    if !s.network {
        out.push_str("(deny network*)\n(allow network* (remote unix-socket) (local unix-socket))\n");
    }
    out
}

/// The words in a failed command's output that a sandbox can cause.
const MARKERS: &[&str] = &[
    "permission denied",
    "operation not permitted",
    "read-only file system",
    "network is unreachable",
    "connection refused",
    "could not resolve host",
    "temporary failure in name resolution",
    "name or service not known",
    "eacces",
    "eperm",
];

/// A short note for the model when a command failed in the sandbox in a way the sandbox may have
/// caused, so it can offer to run the command outside it (which asks the person first).
pub fn note_for(s: &SandboxSettings, output: &str) -> Option<String> {
    if !s.mode.is_on() {
        return None;
    }
    let low = output.to_lowercase();
    if !MARKERS.iter().any(|m| low.contains(m)) {
        return None;
    }
    let writes = match s.mode {
        SandboxMode::WorkspaceWrite => {
            "writes only inside the project, the added directories, the temp directory and tool caches"
        }
        _ => "writes nothing but the temp directory",
    };
    let net = if s.network { "" } else { "; the network is off" };
    Some(format!(
        "[sandbox: this command ran in the {} sandbox ({writes}{net}). If it failed for that reason, tell \
         the person and, if it is needed, run it again with `unsandboxed: true`; they are asked first.]",
        s.mode.as_str()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> SandboxSettings {
        SandboxSettings::new(SandboxMode::WorkspaceWrite, false, vec![])
    }

    #[test]
    fn bwrap_args_bind_roots_and_cut_the_network() {
        let roots = vec![PathBuf::from("/dev"), PathBuf::from("/work")];
        let a = bwrap_args(&ws(), &roots, Path::new("/work"), Path::new("/bin/sh"), &["-c".into(), "ls".into()], false);
        assert!(a.contains(&"--die-with-parent".to_string()));
        let d = bwrap_args(&ws(), &roots, Path::new("/work"), Path::new("/bin/sh"), &["-c".into(), "ls".into()], true);
        assert!(!d.contains(&"--die-with-parent".to_string()));
        assert!(a.windows(3).any(|w| w == ["--bind", "/work", "/work"]));
        assert!(a.contains(&"--unshare-net".to_string()));
        assert_eq!(&a[a.len() - 3..], ["/bin/sh", "-c", "ls"]);
        let mut net = ws();
        net.network = true;
        assert!(!bwrap_args(&net, &roots, Path::new("/work"), Path::new("sh"), &[], false)
            .contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn macos_profile_names_roots_and_shuts_the_network() {
        let p = macos_profile(&ws(), &[PathBuf::from("/work"), PathBuf::from("/a \"b\"")]);
        assert!(p.contains("(subpath \"/work\")") && p.contains("(subpath \"/a \\\"b\\\"\")"));
        assert!(p.contains("(deny network*)"));
        let mut net = ws();
        net.network = true;
        assert!(!macos_profile(&net, &[]).contains("(deny network*)"));
    }

    #[test]
    fn the_note_shows_only_for_failures_the_sandbox_may_cause() {
        assert!(note_for(&ws(), "touch: cannot touch 'x': Permission denied").is_some());
        assert!(note_for(&ws(), "curl: (7) Failed to connect: Connection refused").is_some());
        assert!(note_for(&ws(), "error: test failed").is_none());
        assert!(note_for(&SandboxSettings::default(), "Permission denied").is_none());
    }
}
