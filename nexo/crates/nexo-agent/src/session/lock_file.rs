use super::*;

/// Where the system cannot tell whether a lock's process runs, a lock untouched for this long is
/// taken over whoever left it. Its owner touches it as it writes (see [`LOCK_TOUCH`]).
const STALE_LOCK: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
/// How often a writer touches its lock, at most, so that only an idle lock grows old.
pub(super) const LOCK_TOUCH: std::time::Duration = std::time::Duration::from_secs(3600);

/// What this process writes in the locks it takes: its pid and, where the system tells, when it
/// started, the boot it runs in, the host and (Linux) its pid namespace, `-` for what it does not
/// tell. The same pid used later by another process (after a reboot) is not taken for this one, and
/// a lock written on another host or in another container, whose pid says nothing here, is judged
/// by its age.
pub(super) fn stamp() -> &'static str {
    static STAMP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    STAMP.get_or_init(|| {
        let pid = std::process::id();
        let mut words = vec![
            pid.to_string(),
            proc_start(pid).map(|s| s.to_string()).unwrap_or_else(|| "-".into()),
            boot_id().unwrap_or_else(|| "-".into()),
            host_name().unwrap_or("-").to_string(),
            pid_ns().unwrap_or("-").to_string(),
        ];
        while words.last().is_some_and(|w| w == "-") {
            words.pop();
        }
        words.join(" ")
    })
}

/// The name of this host, where the system says it.
fn host_name() -> Option<&'static str> {
    static HOST: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HOST.get_or_init(|| {
        let name = if cfg!(target_os = "linux") {
            std::fs::read_to_string("/proc/sys/kernel/hostname").ok()
        } else if cfg!(windows) {
            std::env::var("COMPUTERNAME").ok()
        } else {
            let out = std::process::Command::new("hostname").stdin(std::process::Stdio::null()).output().ok()?;
            out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
        }?;
        let name: String = name.trim().chars().map(|c| if c.is_whitespace() { '_' } else { c }).collect();
        (!name.is_empty() && name != "-").then_some(name)
    })
    .as_deref()
}

/// The pid namespace this process runs in (Linux), as `/proc/self/ns/pid` names it.
fn pid_ns() -> Option<&'static str> {
    static NS: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NS.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return None;
        }
        let ns = std::fs::read_link("/proc/self/ns/pid").ok()?.to_string_lossy().into_owned();
        (!ns.is_empty() && !ns.contains(char::is_whitespace)).then_some(ns)
    })
    .as_deref()
}

/// The lock text was written on another host or in another pid namespace (where both say so): its
/// pid names no process here. A lock that does not say (an older one) is taken as written here.
fn foreign(text: &str) -> bool {
    let mut words = text.split_whitespace().skip(3).map(|w| Some(w).filter(|w| *w != "-"));
    let differs = |was: Option<&str>, now: Option<&str>| matches!((was, now), (Some(a), Some(b)) if a != b);
    differs(words.next().flatten(), host_name()) || differs(words.next().flatten(), pid_ns())
}

/// When process `pid` started (clock ticks after boot), where the system says so (Linux).
pub(super) fn proc_start(pid: u32) -> Option<u64> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // the fields after the name, which can hold spaces and parentheses; the start is field 22
    stat.get(stat.rfind(')')? + 1..)?.split_whitespace().nth(19)?.parse().ok()
}

/// The id of the running boot, where the system says so (Linux).
pub(super) fn boot_id() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    Some(id.trim().to_string()).filter(|s| !s.is_empty())
}

/// The pid written in a lock, if it can be read.
#[cfg(test)]
pub(super) fn lock_pid(lock: &Path) -> Option<u32> {
    std::fs::read_to_string(lock).ok().and_then(|t| t.split_whitespace().next()?.parse().ok())
}

/// The text of the lock at `lock` and who holds it (`Some(pid)`, the pid unknown when unreadable),
/// or None as holder when it is stale: written in another boot, by a process that is gone or by
/// one that started at another time than the process now under its pid (where the system says
/// so), or, where it cannot say (the lock holds no start time, or comes from another host or pid
/// namespace), older than [`STALE_LOCK`]. None when there is no lock.
fn judge(lock: &Path) -> Option<(String, Option<Option<u32>>)> {
    let md = std::fs::metadata(lock).ok()?;
    let text = std::fs::read_to_string(lock).unwrap_or_default();
    let mut words = text.split_whitespace();
    let pid: Option<u32> = words.next().and_then(|p| p.parse().ok());
    let start: Option<u64> = words.next().and_then(|s| s.parse().ok());
    let other_boot = match (words.next().filter(|w| *w != "-"), boot_id()) {
        (Some(was), Some(now)) => was != now,
        _ => false,
    };
    let old = md.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > STALE_LOCK);
    let live = pid.and_then(|p| {
        let reused = matches!((start, proc_start(p)), (Some(was), Some(now)) if was != now);
        // without the start time a running pid may be another process by now: live while young
        alive(p).map(|live| live && !reused && (start.is_some() || !old))
    });
    let stale = if foreign(&text) {
        old // another host or container: its pid and boot say nothing here
    } else {
        other_boot
            || match live {
                Some(live) => !live,
                None => old,
            }
    };
    let holder = (!stale).then_some(pid);
    Some((text, holder))
}

/// Who holds the lock at `lock`, or None when it is free or stale (see [`judge`]).
pub(super) fn holder(lock: &Path) -> Option<Option<u32>> {
    judge(lock)?.1
}

/// Whether process `pid` is stopped (Ctrl+Z or a stop signal), where the system says so (Linux).
pub(super) fn stopped(pid: u32) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    // the state is the first field after the name, which can hold spaces and parentheses
    stat.rfind(')').and_then(|i| stat.get(i + 1..)).and_then(|s| s.split_whitespace().next()).is_some_and(|s| s == "T")
}

pub(super) fn in_use(lock: &Path, pid: Option<u32>) -> anyhow::Error {
    let id = lock.file_stem().and_then(|s| Path::new(s).file_stem()).unwrap_or_default().to_string_lossy();
    match pid {
        Some(p) if stopped(p) => anyhow::anyhow!(
            "session {id} is held by a paused nexo (pid {p}, stopped by Ctrl+Z or a signal): type `fg` in its \
             terminal to go back to it, or end it with `kill {p}` and resume here"
        ),
        _ => anyhow::anyhow!(
            "session {id} is in use by another nexo (pid {}); if none runs, delete {}",
            pid.map(|p| p.to_string()).unwrap_or_else(|| "unknown".into()),
            lock.display()
        ),
    }
}

/// Takes the advisory lock next to a session file (`<id>.jsonl.lock`, created exclusively, holding
/// the writer's [`stamp`]). A stale lock (see [`judge`]) is taken over.
pub(super) fn take_lock(lock: &Path) -> anyhow::Result<()> {
    for _ in 0..3 {
        match private_file().write(true).create_new(true).open(lock) {
            Ok(mut f) => {
                f.write_all(stamp().as_bytes())?;
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let Some((text, held)) = judge(lock) else { continue }; // gone meanwhile
                if text == stamp() {
                    return Ok(()); // this process holds it already
                }
                // under this pid: a process gone before this one, never another live writer
                let ours =
                    !foreign(&text) && text.split_whitespace().next() == Some(std::process::id().to_string().as_str());
                match held {
                    Some(pid) if !ours => return Err(in_use(lock, pid)),
                    _ => {
                        take_over(lock, &text)?;
                    }
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::bail!("cannot take the lock {}", lock.display())
}

/// Removes the lock at `lock` when it still holds `judged`, the text found stale: moved aside
/// first, so that one taken meanwhile by another nexo is put back, never deleted. Whether it went;
/// an error (the session in use) when one taken meanwhile could not be put back.
pub(super) fn take_over(lock: &Path, judged: &str) -> anyhow::Result<bool> {
    take_over_with(lock, judged, |from, to| std::fs::hard_link(from, to))
}

/// [`take_over`], putting a lock back with `link` (a hard link where the file system has them).
pub(super) fn take_over_with(
    lock: &Path,
    judged: &str,
    link: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> anyhow::Result<bool> {
    let aside = lock.with_extension(format!("lock.{}.stale", std::process::id()));
    if std::fs::rename(lock, &aside).is_err() {
        return Ok(false); // another took it over first
    }
    let same = std::fs::read_to_string(&aside).is_ok_and(|t| t == judged);
    if !same {
        // a lock taken since: back in place, never over one yet another took meanwhile. Without
        // hard links, moved back while the place is empty; else kept aside, and the session in use.
        let back = link(&aside, lock).is_ok() || (!lock.exists() && std::fs::rename(&aside, lock).is_ok());
        if !back {
            let text = std::fs::read_to_string(&aside).unwrap_or_default();
            return Err(in_use(lock, text.split_whitespace().next().and_then(|p| p.parse().ok())));
        }
    }
    let _ = std::fs::remove_file(&aside);
    Ok(same)
}

/// Whether process `pid` runs, where the system says so: Linux from `/proc`, other unix systems
/// from `ps`; None elsewhere (the age of the lock decides).
fn alive(pid: u32) -> Option<bool> {
    if cfg!(target_os = "linux") {
        let proc = Path::new("/proc");
        return proc.join("self").exists().then(|| proc.join(pid.to_string()).exists());
    }
    if cfg!(unix) {
        // `ps -p` lists the process when it runs, and nothing (with no complaint) when it does not
        let out = std::process::Command::new("ps")
            .args(["-o", "pid=", "-p", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .output()
            .ok()?;
        if !String::from_utf8_lossy(&out.stdout).trim().is_empty() {
            return Some(true);
        }
        return (!out.status.success() && out.stderr.is_empty()).then_some(false);
    }
    None
}
