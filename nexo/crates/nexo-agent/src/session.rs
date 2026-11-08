//! Sessions on disk: one JSON-lines file each, appended as things happen,
//! so nothing is lost and a session can be continued.
//!
//! Lines: `{"type":"meta",...}` first; then `message` (with its index),
//! `tool_output`, `usage`, `title`, and `reset` (the history was rewritten:
//! what follows replaces what came before).

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use nexo_core::message::{Message, Part, Role};
use nexo_core::paths;
use nexo_core::stream::Usage;
use nexo_core::tool::ToolOutput;
use nexo_core::GoalState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub cwd: PathBuf,
    pub created: String,
    #[serde(default)]
    pub updated: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub messages: usize,
    /// The session this one was forked from (old sessions have none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// How many of the parent's messages the fork kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_at: Option<usize>,
    /// Started by `nexo -p` (a script or another program), not by the person: left out of `/resume`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub headless: bool,
}

/// Whose model call a `usage` line pays for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    /// The conversation's own call.
    Main,
    /// A sub-agent.
    Sub,
    /// The reader that digests a large tool output.
    Reader,
    /// A compaction.
    Compact,
    /// The guess at the person's next message (`[ui] suggest`).
    Suggest,
    /// The look at a long background command (`job_review`).
    Review,
    /// The supervisor's check-in on a session that stopped (`agent.check_in`).
    CheckIn,
    /// Everything spent so far, written in one line when the file is rewritten.
    Sum,
}

/// What a `usage` line says besides the counts; every field is optional on disk.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageMeta {
    pub model: Option<String>,
    pub kind: Option<UsageKind>,
    pub held: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Line {
    Meta(SessionMeta),
    Message {
        index: usize,
        ts: String,
        message: Message,
    },
    ToolOutput {
        call_id: String,
        output: ToolOutput,
    },
    Usage {
        usage: Usage,
        cost_usd: f64,
        /// When the call was paid (RFC 3339); old files have none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ts: Option<String>,
        /// The model that answered; old files have none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Whose call it was; old files have none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<UsageKind>,
        /// Tokens of the history the view left out at this call; old files have none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        held: Option<u64>,
    },
    Title {
        title: String,
    },
    Model {
        provider: String,
        model: String,
    },
    /// The goal in force from here on (None: cleared), and where it stands (active when absent).
    Goal {
        text: Option<String>,
        #[serde(default, skip_serializing_if = "GoalState::is_active")]
        state: GoalState,
    },
    Reset,
}

pub struct Session {
    meta: SessionMeta,
    path: PathBuf,
    messages: Vec<Message>,
    outputs: BTreeMap<String, ToolOutput>,
    usage: Usage,
    cost: f64,
    file: Option<File>,
    goal: Option<String>,
    goal_state: GoalState,
    /// Whether this session holds the lock next to its file (taken at the first write).
    locked: bool,
    /// When the lock was last touched by this session.
    touched: std::time::Instant,
    /// The file ends inside a line (a process died mid-write): closed once the lock is held.
    open_tail: bool,
    /// The pid of the nexo that took the lock over, once one has: nothing more is saved by this
    /// session, even after that nexo lets the lock go (its history and this one have parted).
    lost: Option<String>,
    /// Let go by [`Session::release`]: the agent has stopped, and nothing more is saved.
    closed: bool,
    /// The length of the file after this session's own last write: a longer file, or another one,
    /// holds lines of somebody else.
    size: u64,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.release();
    }
}

impl Session {
    /// Lets the session go when the agent stops: its lock is removed, and a session opened and left
    /// without a word is removed from the list. A task still holding the session when the process
    /// exits (exiting runs no destructors) would otherwise leave the lock behind. Nothing is saved
    /// after this; a second call does nothing.
    pub fn release(&mut self) {
        if std::mem::replace(&mut self.closed, true) {
            return;
        }
        // opened and left without a word: nothing worth keeping in the list
        let empty = self.file.is_some()
            && self.locked
            && self.meta.messages == 0
            && self.meta.title.is_empty()
            && self.meta.parent.is_none()
            && self.goal.is_none()
            && self.cost == 0.0;
        // only a lock still this process's: another nexo may have taken it over
        let lock = lock_path(&self.path);
        if self.locked && std::fs::read_to_string(&lock).is_ok_and(|t| t == stamp()) {
            let _ = std::fs::remove_file(&lock);
            self.locked = false;
        }
        if empty && std::fs::read_to_string(&lock).is_err() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn lock_path(session: &Path) -> PathBuf {
    session.with_extension("jsonl.lock")
}

fn now() -> String {
    time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

fn new_id() -> String {
    let t = time::OffsetDateTime::now_utc();
    let f = time::macros::format_description!("[year][month][day]-[hour][minute][second]");
    let bytes = uuid::Uuid::new_v4();
    format!("{}-{}", t.format(&f).unwrap_or_default(), &bytes.simple().to_string()[..4])
}

/// Set once by `nexo -p`: the sessions this process starts are marked as not the person's own.
static HEADLESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Marks the sessions this process starts from now on as run by a program (`nexo -p`).
pub fn mark_headless() {
    HEADLESS.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Where sessions live.
pub fn dir() -> PathBuf {
    paths::sessions_dir()
}

/// All sessions, newest first.
pub fn list() -> Vec<SessionMeta> {
    list_in(&dir())
}

pub fn list_in(dir: &Path) -> Vec<SessionMeta> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().map(|x| x == "jsonl").unwrap_or(false) {
            if let Some(m) = read_meta(&p) {
                out.push(m);
            }
        }
    }
    out.sort_by(|a, b| b.updated.cmp(&a.updated));
    out
}

/// Where deleted sessions wait: a folder inside the sessions folder, so a delete can be undone.
pub fn trash_dir_in(dir: &Path) -> PathBuf {
    dir.join("trash")
}

/// An id names one file of the folder, never a path out of it.
fn checked_id(id: &str) -> anyhow::Result<&str> {
    let ok = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(id)
    } else {
        anyhow::bail!("{id:?} is not a session id")
    }
}

/// Sessions hold tool outputs, which can hold secrets: the folder is the user's alone (on unix).
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

/// Options for a session file only the user can read (on unix).
fn private_file() -> OpenOptions {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut o = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}

/// A rewrite replaced the file but could not open it again.
#[derive(Debug)]
struct NotReopened(std::io::Error);

impl std::fmt::Display for NotReopened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the session file was rewritten but cannot be opened again: {}", self.0)
    }
}

impl std::error::Error for NotReopened {}

#[cfg(test)]
thread_local! {
    /// Tests make the next reopenings of a session file fail.
    static FAIL_REOPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The session file, opened again to append to it.
fn reopen(path: &Path) -> std::io::Result<File> {
    #[cfg(test)]
    if FAIL_REOPEN.with(|f| f.get()) {
        return Err(std::io::Error::other("cannot open (test)"));
    }
    OpenOptions::new().append(true).open(path)
}

/// One line in one write, so a line from another writer never lands inside it.
fn write_line(f: &mut File, line: &Line) -> anyhow::Result<()> {
    let mut s = serde_json::to_string(line)?;
    s.push('\n');
    f.write_all(s.as_bytes())?;
    Ok(())
}

/// Where the system cannot tell whether a lock's process runs, a lock untouched for this long is
/// taken over whoever left it. Its owner touches it as it writes (see [`LOCK_TOUCH`]).
const STALE_LOCK: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
/// How often a writer touches its lock, at most, so that only an idle lock grows old.
const LOCK_TOUCH: std::time::Duration = std::time::Duration::from_secs(3600);

/// What this process writes in the locks it takes: its pid and, where the system tells, when it
/// started, the boot it runs in, the host and (Linux) its pid namespace, `-` for what it does not
/// tell. The same pid used later by another process (after a reboot) is not taken for this one, and
/// a lock written on another host or in another container, whose pid says nothing here, is judged
/// by its age.
fn stamp() -> &'static str {
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
fn proc_start(pid: u32) -> Option<u64> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // the fields after the name, which can hold spaces and parentheses; the start is field 22
    stat.get(stat.rfind(')')? + 1..)?.split_whitespace().nth(19)?.parse().ok()
}

/// The id of the running boot, where the system says so (Linux).
fn boot_id() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    Some(id.trim().to_string()).filter(|s| !s.is_empty())
}

/// The pid written in a lock, if it can be read.
#[cfg(test)]
fn lock_pid(lock: &Path) -> Option<u32> {
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
fn holder(lock: &Path) -> Option<Option<u32>> {
    judge(lock)?.1
}

/// Whether process `pid` is stopped (Ctrl+Z or a stop signal), where the system says so (Linux).
fn stopped(pid: u32) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    // the state is the first field after the name, which can hold spaces and parentheses
    stat.rfind(')').and_then(|i| stat.get(i + 1..)).and_then(|s| s.split_whitespace().next()).is_some_and(|s| s == "T")
}

fn in_use(lock: &Path, pid: Option<u32>) -> anyhow::Error {
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
fn take_lock(lock: &Path) -> anyhow::Result<()> {
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
fn take_over(lock: &Path, judged: &str) -> anyhow::Result<bool> {
    take_over_with(lock, judged, |from, to| std::fs::hard_link(from, to))
}

/// [`take_over`], putting a lock back with `link` (a hard link where the file system has them).
fn take_over_with(
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

/// Moves session `id` to the trash and says where it went. Nothing is erased: `restore` brings it back.
pub fn trash(id: &str) -> anyhow::Result<PathBuf> {
    trash_in(&dir(), id)
}

pub fn trash_in(dir: &Path, id: &str) -> anyhow::Result<PathBuf> {
    let id = checked_id(id)?;
    let from = dir.join(format!("{id}.jsonl"));
    if !from.is_file() {
        anyhow::bail!("no session {id}");
    }
    // another nexo still writing to it would bring it back at its next rewrite
    let lock = lock_path(&from);
    if let Some(pid) = holder(&lock).filter(|p| *p != Some(std::process::id())) {
        return Err(in_use(&lock, pid));
    }
    let bin = trash_dir_in(dir);
    std::fs::create_dir_all(&bin)?;
    let to = bin.join(format!("{id}.jsonl"));
    std::fs::rename(&from, &to)?;
    Ok(to)
}

/// Brings session `id` back from the trash.
pub fn restore(id: &str) -> anyhow::Result<PathBuf> {
    restore_in(&dir(), id)
}

pub fn restore_in(dir: &Path, id: &str) -> anyhow::Result<PathBuf> {
    let id = checked_id(id)?;
    let from = trash_dir_in(dir).join(format!("{id}.jsonl"));
    let to = dir.join(format!("{id}.jsonl"));
    if !from.is_file() {
        anyhow::bail!("no session {id} in the trash");
    }
    if to.exists() {
        anyhow::bail!("a session {id} is already there");
    }
    std::fs::rename(&from, &to)?;
    Ok(to)
}

/// The sessions in the trash, newest first.
pub fn trashed() -> Vec<SessionMeta> {
    list_in(&trash_dir_in(&dir()))
}

/// The session most recently touched in `cwd` that something was said in.
pub fn latest_for(cwd: &Path) -> Option<SessionMeta> {
    latest_for_in(&dir(), cwd)
}

pub fn latest_for_in(dir: &Path, cwd: &Path) -> Option<SessionMeta> {
    let want = same_form(cwd);
    list_in(dir).into_iter().find(|m| m.messages > 0 && same_form(&m.cwd) == want)
}

/// A folder in one form, however it was written: canonical where it exists, and on Windows
/// without the `\\?\` prefix canonicalizing adds.
fn same_form(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if cfg!(windows) {
        let s = c.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\").filter(|r| !r.starts_with(r"UNC\")) {
            return PathBuf::from(rest);
        }
    }
    c
}

fn read_meta(path: &Path) -> Option<SessionMeta> {
    let f = File::open(path).ok()?;
    let mut r = BufReader::new(f);
    let mut first = String::new();
    r.read_line(&mut first).ok()?;
    let Line::Meta(mut m) = serde_json::from_str(&first).ok()? else { return None };
    let mut n = 0;
    let mut title = None;
    // each line on its own, as `open_path` reads them: a UTF-8 sequence cut by a crash spoils one line
    for l in r.split(b'\n').map_while(Result::ok) {
        let l = String::from_utf8_lossy(&l);
        match serde_json::from_str::<Line>(&l) {
            Ok(Line::Message { .. }) => n += 1,
            Ok(Line::Reset) => n = 0,
            Ok(Line::Title { title: t }) => title = Some(t),
            _ => {}
        }
    }
    m.messages = n;
    if let Some(t) = title {
        m.title = t;
    }
    if let Ok(md) = std::fs::metadata(path) {
        if let Ok(t) = md.modified() {
            let t: time::OffsetDateTime = t.into();
            m.updated = t.format(&time::format_description::well_known::Rfc3339).unwrap_or_default();
        }
    }
    Some(m)
}

impl Session {
    pub fn create(cwd: &Path, provider: &str, model: &str) -> anyhow::Result<Session> {
        Session::create_in(&dir(), cwd, provider, model)
    }

    pub fn create_in(dir: &Path, cwd: &Path, provider: &str, model: &str) -> anyhow::Result<Session> {
        Session::create_linked(dir, cwd, provider, model, None, None)
    }

    fn create_linked(
        dir: &Path,
        cwd: &Path,
        provider: &str,
        model: &str,
        parent: Option<String>,
        fork_at: Option<usize>,
    ) -> anyhow::Result<Session> {
        create_private_dir(dir)?;
        // an id is the second it was made in and 4 random hex digits: two sessions born in the same
        // second can draw the same one, and the later takes another instead of going unsaved
        let mut tries = 0;
        let (id, path, mut file) = loop {
            let id = new_id();
            let path = dir.join(format!("{id}.jsonl"));
            match private_file().create_new(true).append(true).open(&path) {
                Ok(f) => break (id, path, f),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 32 => tries += 1,
                Err(e) => return Err(e.into()),
            }
        };
        let meta = SessionMeta {
            id: id.clone(),
            cwd: cwd.to_path_buf(),
            created: now(),
            updated: now(),
            provider: provider.into(),
            model: model.into(),
            title: String::new(),
            messages: 0,
            parent,
            fork_at,
            headless: HEADLESS.load(std::sync::atomic::Ordering::Relaxed),
        };
        take_lock(&lock_path(&path))?;
        write_line(&mut file, &Line::Meta(meta.clone()))?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Session {
            meta,
            path,
            messages: Vec::new(),
            outputs: BTreeMap::new(),
            usage: Usage::default(),
            cost: 0.0,
            file: Some(file),
            goal: None,
            goal_state: GoalState::Active,
            locked: true,
            touched: std::time::Instant::now(),
            open_tail: false,
            lost: None,
            closed: false,
            size,
        })
    }

    /// A session that is not written anywhere (tests, one-off runs).
    pub fn ephemeral(cwd: &Path, provider: &str, model: &str) -> Session {
        let meta = SessionMeta {
            id: new_id(),
            cwd: cwd.to_path_buf(),
            created: now(),
            updated: now(),
            provider: provider.into(),
            model: model.into(),
            title: String::new(),
            messages: 0,
            parent: None,
            fork_at: None,
            headless: false,
        };
        Session {
            meta,
            path: PathBuf::new(),
            messages: Vec::new(),
            outputs: BTreeMap::new(),
            usage: Usage::default(),
            cost: 0.0,
            file: None,
            goal: None,
            goal_state: GoalState::Active,
            locked: false,
            touched: std::time::Instant::now(),
            open_tail: false,
            lost: None,
            closed: false,
            size: 0,
        }
    }

    pub fn open(id: &str) -> anyhow::Result<Session> {
        Session::open_path(&dir().join(format!("{}.jsonl", checked_id(id)?)))
    }

    /// Reads a session file; a truncated last line is ignored. Each line is read on its own, so a
    /// UTF-8 sequence cut by a crash spoils only the line it is in.
    pub fn open_path(path: &Path) -> anyhow::Result<Session> {
        let bytes = std::fs::read(path)?;
        let mut lines = bytes
            .split(|b| *b == b'\n')
            .map(|l| String::from_utf8_lossy(l.strip_suffix(b"\r").unwrap_or(l)))
            .filter(|l| !l.is_empty());
        let first = lines.next().ok_or_else(|| anyhow::anyhow!("empty session file {}", path.display()))?;
        let Line::Meta(mut meta) = serde_json::from_str(&first)? else {
            anyhow::bail!("{} does not start with a meta line", path.display());
        };
        let mut messages = Vec::new();
        let mut outputs = BTreeMap::new();
        let mut usage = Usage::default();
        let mut cost = 0.0;
        let mut goal = None;
        let mut goal_state = GoalState::Active;
        for l in lines {
            match serde_json::from_str::<Line>(&l) {
                Ok(Line::Message { message, .. }) => messages.push(message),
                Ok(Line::ToolOutput { call_id, output }) => {
                    outputs.insert(call_id, output);
                }
                Ok(Line::Usage { usage: u, cost_usd, .. }) => {
                    usage += u;
                    cost += cost_usd;
                }
                Ok(Line::Title { title }) => meta.title = title,
                Ok(Line::Model { provider, model }) => {
                    meta.provider = provider;
                    meta.model = model;
                }
                Ok(Line::Goal { text, state }) => {
                    goal = text;
                    goal_state = state;
                }
                Ok(Line::Reset) => messages.clear(),
                Ok(Line::Meta(_)) => {}
                Err(_) => {} // a cut line: whatever was being written when the process died
            }
        }
        meta.messages = messages.len();
        let file = OpenOptions::new().append(true).open(path)?;
        Ok(Session {
            meta,
            path: path.to_path_buf(),
            messages,
            outputs,
            usage,
            cost,
            file: Some(file),
            goal,
            goal_state,
            locked: false,
            touched: std::time::Instant::now(),
            // a process killed mid-write leaves a last line without its end
            open_tail: bytes.last().is_some_and(|b| *b != b'\n'),
            lost: None,
            closed: false,
            size: bytes.len() as u64,
        })
    }

    /// Takes the session for writing now, as a resume does before anything is spent: fails when
    /// another nexo holds it. Reading a session never takes it; its first write does.
    pub fn claim(&mut self) -> anyhow::Result<()> {
        if self.file.is_none() {
            return Ok(());
        }
        if self.closed {
            anyhow::bail!("session {} was closed when the agent stopped: nothing more is saved", self.meta.id);
        }
        if let Some(pid) = &self.lost {
            anyhow::bail!(
                "session {} was taken over by another nexo (pid {pid}): nothing more is saved here; open it again",
                self.meta.id
            );
        }
        let lock = lock_path(&self.path);
        if !self.locked {
            take_lock(&lock)?;
            (self.locked, self.touched) = (true, std::time::Instant::now());
        } else {
            // the lock is still this process's: one that judged it stale may have taken it over
            match std::fs::read_to_string(&lock) {
                Ok(t) if t == stamp() => {
                    if self.touched.elapsed() > LOCK_TOUCH {
                        // a lock in use stays young: only an idle one can be taken over by its age
                        self.touched = std::time::Instant::now();
                        if let Ok(l) = File::options().write(true).open(&lock) {
                            let _ = l.set_modified(std::time::SystemTime::now());
                        }
                    }
                }
                // gone: deleted by hand (taken again), or let go by a nexo that took it over, wrote and
                // ended (lost for good: its lines are in the file, and this memory lacks them)
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if self.file_changed() {
                        self.lost = Some("unknown".into());
                        self.locked = false;
                        return self.claim();
                    }
                    take_lock(&lock)?
                }
                Ok(t) => {
                    // lost for good: the lock is not this one's to take again, even once let go
                    self.lost = Some(t.split_whitespace().next().unwrap_or("unknown").to_string());
                    self.locked = false;
                    return self.claim();
                }
                Err(e) => return Err(e.into()),
            }
        }
        if self.open_tail {
            if let Some(f) = &mut self.file {
                // so that the next line starts on a line of its own
                f.write_all(b"\n")?;
            }
            self.open_tail = false;
            self.mark();
        }
        Ok(())
    }

    /// The file is no longer the one this session wrote, as far as it left it: longer, or (on Unix,
    /// where files are told apart by device and inode) replaced.
    fn file_changed(&self) -> bool {
        let Ok(now) = std::fs::metadata(&self.path) else { return false };
        if now.len() != self.size {
            return true;
        }
        #[cfg(unix)]
        if let Some(mine) = self.file.as_ref().and_then(|f| f.metadata().ok()) {
            use std::os::unix::fs::MetadataExt;
            return (now.dev(), now.ino()) != (mine.dev(), mine.ino());
        }
        false
    }

    /// Remembers the length of the file as this session left it.
    fn mark(&mut self) {
        if let Some(m) = self.file.as_ref().and_then(|f| f.metadata().ok()) {
            self.size = m.len();
        }
    }

    /// Whether another nexo took this session over: it writes nothing more here, and /resume opens it again.
    pub fn is_lost(&self) -> bool {
        self.lost.is_some()
    }

    pub fn id(&self) -> &str {
        &self.meta.id
    }

    pub fn meta(&self) -> &SessionMeta {
        &self.meta
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn outputs(&self) -> &BTreeMap<String, ToolOutput> {
        &self.outputs
    }

    pub fn usage(&self) -> Usage {
        self.usage
    }

    pub fn cost(&self) -> f64 {
        self.cost
    }

    /// The file of a session saved on disk, opened again when a rewrite could not reopen it: a
    /// write that cannot reach it fails instead of going unsaved without a word.
    fn ensure_open(&mut self) -> anyhow::Result<()> {
        if self.file.is_none() && !self.path.as_os_str().is_empty() {
            self.file = Some(reopen(&self.path)?);
        }
        Ok(())
    }

    fn write(&mut self, line: &Line) -> anyhow::Result<()> {
        self.ensure_open()?;
        self.claim()?;
        if let Some(f) = &mut self.file {
            write_line(f, line)?;
        }
        self.mark();
        self.meta.updated = now();
        Ok(())
    }

    pub fn append(&mut self, m: &Message) -> anyhow::Result<()> {
        let index = self.messages.len();
        self.messages.push(m.clone());
        self.meta.messages = self.messages.len();
        self.write(&Line::Message { index, ts: now(), message: m.clone() })?;
        if self.meta.title.is_empty() && m.role == Role::User && !m.is_tool_results() {
            let t = title_of(&m.text());
            if !t.is_empty() {
                self.set_title(&t)?;
            }
        }
        Ok(())
    }

    pub fn record_output(&mut self, call_id: &str, output: &ToolOutput) -> anyhow::Result<()> {
        // the pictures are already in the tool-result message; the display copy keeps none
        let output = ToolOutput { images: Vec::new(), ..output.clone() };
        self.outputs.insert(call_id.to_string(), output.clone());
        self.write(&Line::ToolOutput { call_id: call_id.into(), output })
    }

    pub fn record_usage(&mut self, usage: Usage, cost_usd: f64) -> anyhow::Result<()> {
        self.record_usage_meta(usage, cost_usd, UsageMeta::default())
    }

    /// [`Session::record_usage`] with the time, and what else is known of the call (the insights of
    /// the usage window read them; old readers ignore them).
    pub fn record_usage_meta(&mut self, usage: Usage, cost_usd: f64, meta: UsageMeta) -> anyhow::Result<()> {
        self.usage += usage;
        self.cost += cost_usd;
        self.write(&Line::Usage {
            usage,
            cost_usd,
            ts: Some(now()),
            model: meta.model,
            kind: meta.kind,
            held: meta.held,
        })
    }

    pub fn record_model(&mut self, provider: &str, model: &str) -> anyhow::Result<()> {
        self.meta.provider = provider.into();
        self.meta.model = model.into();
        self.write(&Line::Model { provider: provider.into(), model: model.into() })
    }

    /// The goal in force, as last recorded.
    pub fn goal(&self) -> Option<&str> {
        self.goal.as_deref()
    }

    /// Where the goal stands, as last recorded.
    pub fn goal_state(&self) -> &GoalState {
        &self.goal_state
    }

    /// A new goal (or none): it starts active.
    pub fn record_goal(&mut self, text: Option<&str>) -> anyhow::Result<()> {
        self.goal = text.map(String::from);
        self.goal_state = GoalState::Active;
        self.write(&Line::Goal { text: self.goal.clone(), state: GoalState::Active })
    }

    /// The same goal, standing elsewhere (paused, waiting, met, active again).
    pub fn record_goal_state(&mut self, state: GoalState) -> anyhow::Result<()> {
        self.goal_state = state.clone();
        self.write(&Line::Goal { text: self.goal.clone(), state })
    }

    pub fn set_title(&mut self, title: &str) -> anyhow::Result<()> {
        self.meta.title = title.into();
        self.write(&Line::Title { title: title.into() })
    }

    /// The history was rewritten (compaction): what follows replaces it.
    pub fn reset_with(&mut self, messages: &[Message]) -> anyhow::Result<()> {
        // one atomic rewrite: a crash leaves the old file or the new one, never a half history
        let (old, outputs) = (std::mem::replace(&mut self.messages, messages.to_vec()), self.outputs.clone());
        let done = self.rewrite();
        // as in `rewind`: when the file was not replaced, memory keeps matching it
        if done.as_ref().is_err_and(|e| !e.is::<NotReopened>()) {
            (self.messages, self.outputs) = (old, outputs);
            self.meta.messages = self.messages.len();
        }
        done
    }

    /// The text of a tool result, for the parts of a message.
    pub fn result_text(parts: &[Part]) -> String {
        nexo_core::message::result_text(parts)
    }

    /// Keeps the first `keep` messages (and the outputs of the calls they
    /// make) and rewrites the file to hold only them: written to a temporary
    /// file next to it and renamed over it, so a crash leaves either the old
    /// file or the new one, never half of each. Usage and cost stay: they
    /// were spent. When the file cannot be rewritten, memory is left as it was, matching the file.
    pub fn rewind(&mut self, keep: usize) -> anyhow::Result<()> {
        let keep = keep.min(self.messages.len());
        let (messages, outputs) = (self.messages.clone(), self.outputs.clone());
        self.messages.truncate(keep);
        let ids: std::collections::BTreeSet<String> =
            self.messages.iter().flat_map(|m| m.tool_calls().map(|(id, _, _)| id.to_string())).collect();
        self.outputs.retain(|id, _| ids.contains(id));
        let done = self.rewrite();
        // a file replaced but not opened again holds the kept messages: memory stays as they are
        if done.as_ref().is_err_and(|e| !e.is::<NotReopened>()) {
            (self.messages, self.outputs) = (messages, outputs);
            self.meta.messages = self.messages.len();
        }
        done
    }

    /// Writes the whole file again from memory: to a temporary file next to it, renamed over it.
    fn rewrite(&mut self) -> anyhow::Result<()> {
        self.meta.messages = self.messages.len();
        self.meta.updated = now();
        if self.path.as_os_str().is_empty() {
            return Ok(()); // saved nowhere
        }
        self.ensure_open()?;
        self.claim()?;
        // one temporary name per process: two writers never share it
        let tmp = self.path.with_extension(format!("jsonl.{}.tmp", std::process::id()));
        // a write that fails leaves no partial copy (it holds tool outputs) behind
        let made = (|| -> anyhow::Result<()> {
            let mut f = private_file().write(true).create(true).truncate(true).open(&tmp)?;
            writeln!(f, "{}", serde_json::to_string(&Line::Meta(self.meta.clone()))?)?;
            if !self.meta.title.is_empty() {
                writeln!(f, "{}", serde_json::to_string(&Line::Title { title: self.meta.title.clone() })?)?;
            }
            let mut written = std::collections::BTreeSet::new();
            for (index, m) in self.messages.iter().enumerate() {
                writeln!(f, "{}", serde_json::to_string(&Line::Message { index, ts: now(), message: m.clone() })?)?;
                for (id, _, _) in m.tool_calls() {
                    if let Some(out) = self.outputs.get(id) {
                        written.insert(id.to_string());
                        writeln!(
                            f,
                            "{}",
                            serde_json::to_string(&Line::ToolOutput { call_id: id.into(), output: out.clone() })?
                        )?;
                    }
                }
            }
            // outputs of calls whose messages were replaced (compaction) stay, as they did in the log
            for (id, out) in self.outputs.iter().filter(|(id, _)| !written.contains(*id)) {
                writeln!(
                    f,
                    "{}",
                    serde_json::to_string(&Line::ToolOutput { call_id: id.clone(), output: out.clone() })?
                )?;
            }
            if !self.usage.is_empty() || self.cost > 0.0 {
                writeln!(
                    f,
                    "{}",
                    serde_json::to_string(&Line::Usage {
                        usage: self.usage,
                        cost_usd: self.cost,
                        ts: Some(now()),
                        model: None,
                        kind: Some(UsageKind::Sum),
                        held: None,
                    })?
                )?;
            }
            if self.goal.is_some() {
                writeln!(
                    f,
                    "{}",
                    serde_json::to_string(&Line::Goal { text: self.goal.clone(), state: self.goal_state.clone() })?
                )?;
            }
            f.sync_all()?;
            Ok(())
        })();
        if let Err(e) = made {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        // Windows refuses to replace a file that is open: the handle closes first
        self.file = None;
        let renamed = std::fs::rename(&tmp, &self.path);
        // the file is opened again even when the rename failed (Windows, a virus scanner), so the
        // session keeps being saved; the failure is still reported. One that cannot be opened is
        // tried again at the next write.
        let reopened = reopen(&self.path);
        match renamed {
            Ok(()) => {
                self.file = Some(reopened.map_err(NotReopened)?);
                self.mark();
                Ok(())
            }
            Err(e) => {
                self.file = reopened.ok();
                let _ = std::fs::remove_file(&tmp);
                anyhow::bail!("cannot replace {}: {e}", self.path.display());
            }
        }
    }

    /// A new session that starts as a copy of the first `keep` messages of this one (and the
    /// outputs of the calls they make), remembering its parent and the cut point. This session is
    /// left as it is. Usage and cost are not copied: the fork has spent nothing yet.
    pub fn fork(&self, keep: usize) -> anyhow::Result<Session> {
        let dir = self.path.parent().map(Path::to_path_buf).unwrap_or_else(dir);
        let keep = keep.min(self.messages.len());
        let (parent, at) = (Some(self.meta.id.clone()), Some(keep));
        let mut f = if self.file.is_some() {
            Session::create_linked(&dir, &self.meta.cwd, &self.meta.provider, &self.meta.model, parent, at)?
        } else {
            let mut e = Session::ephemeral(&self.meta.cwd, &self.meta.provider, &self.meta.model);
            (e.meta.parent, e.meta.fork_at) = (parent, at);
            e
        };
        for m in &self.messages[..keep] {
            f.append(m)?;
            for (id, _, _) in m.tool_calls() {
                if let Some(out) = self.outputs.get(id) {
                    f.record_output(id, out)?;
                }
            }
        }
        if f.meta.title.is_empty() && !self.meta.title.is_empty() {
            f.set_title(&format!("{} (fork)", self.meta.title))?;
        }
        if self.goal.is_some() {
            f.record_goal(self.goal.as_deref())?; // on disk too, so a resumed fork keeps it
            if !self.goal_state.is_active() {
                f.record_goal_state(self.goal_state.clone())?;
            }
        }
        Ok(f)
    }

    /// Where `export` writes when no path is given: next to the session file.
    pub fn export_path(&self) -> PathBuf {
        let d = if self.path.as_os_str().is_empty() {
            dir()
        } else {
            self.path.parent().map(Path::to_path_buf).unwrap_or_else(dir)
        };
        d.join(format!("{}.md", self.meta.id))
    }

    /// The conversation as markdown. `summary` names what a call does
    /// (`read src/a.rs`) from the tool's name and arguments.
    pub fn export_markdown(&self, summary: impl Fn(&str, &serde_json::Value) -> String) -> String {
        self.markdown(summary, false)
    }

    /// [`Session::export_markdown`] for sharing: the output of a call that names a `.env*` file is replaced
    /// whole by `[redacted: .env file]`.
    pub fn export_markdown_redacted(&self, summary: impl Fn(&str, &serde_json::Value) -> String) -> String {
        self.markdown(summary, true)
    }

    fn markdown(&self, summary: impl Fn(&str, &serde_json::Value) -> String, hide_env: bool) -> String {
        const FOLD_LINES: usize = 60;
        let mut out = String::new();
        let title = if self.meta.title.is_empty() { "nexo session".to_string() } else { self.meta.title.clone() };
        out.push_str(&format!("# {title}\n\n"));
        out.push_str(&format!(
            "- session: `{}`\n- started: {}\n- model: {}/{}\n- directory: `{}`\n\n",
            self.meta.id,
            self.meta.created,
            self.meta.provider,
            self.meta.model,
            self.meta.cwd.display()
        ));
        for m in &self.messages {
            if m.is_tool_results() {
                continue; // shown under the calls that made them
            }
            out.push_str(if m.role == Role::User { "## You\n\n" } else { "## nexo\n\n" });
            for p in &m.content {
                match p {
                    Part::Text { text } => {
                        out.push_str(text.trim_end());
                        out.push_str("\n\n");
                    }
                    Part::Reasoning { text, .. } if !text.trim().is_empty() => {
                        out.push_str("<details><summary>reasoning</summary>\n\n");
                        out.push_str(text.trim_end());
                        out.push_str("\n\n</details>\n\n");
                    }
                    Part::ToolCall { id, name, args } => {
                        out.push_str(&format!("**{}** {}\n\n", name, summary(name, args)));
                        let body = if hide_env && crate::scrub::touches_env_file(args) {
                            crate::scrub::ENV_BODY.to_string()
                        } else {
                            match self.outputs.get(id) {
                                Some(o) => o.content.clone(),
                                None => self
                                    .messages
                                    .iter()
                                    .flat_map(|m| m.content.iter())
                                    .find_map(|p| match p {
                                        Part::ToolResult { call_id, content, .. } if call_id == id => {
                                            Some(Session::result_text(content))
                                        }
                                        _ => None,
                                    })
                                    .unwrap_or_default(),
                            }
                        };
                        let lines: Vec<&str> = body.lines().collect();
                        let fence = if body.contains("```") { "````" } else { "```" };
                        out.push_str(fence);
                        out.push('\n');
                        for l in lines.iter().take(FOLD_LINES) {
                            out.push_str(l);
                            out.push('\n');
                        }
                        if lines.len() > FOLD_LINES {
                            out.push_str(&format!("… {} more lines\n", lines.len() - FOLD_LINES));
                        }
                        out.push_str(fence);
                        out.push_str("\n\n");
                    }
                    _ => {}
                }
            }
        }
        let u = self.usage;
        out.push_str(&format!(
            "---\n\ntokens: {} in, {} out, {} cached read, {} cached write · cost: ${:.4}\n",
            u.input, u.output, u.cache_read, u.cache_write, self.cost
        ));
        out
    }
}

/// The first line of a request, at most 60 characters.
/// The sessions as a forest, parents before children (newest sibling first), each with its depth.
/// A session whose parent is gone is a root.
pub fn tree(metas: &[SessionMeta]) -> Vec<(usize, SessionMeta)> {
    let ids: std::collections::BTreeSet<&str> = metas.iter().map(|m| m.id.as_str()).collect();
    let mut sorted: Vec<&SessionMeta> = metas.iter().collect();
    sorted.sort_by(|a, b| b.updated.cmp(&a.updated));
    fn walk(out: &mut Vec<(usize, SessionMeta)>, all: &[&SessionMeta], m: &SessionMeta, depth: usize) {
        if out.iter().any(|(_, o)| o.id == m.id) {
            return;
        }
        out.push((depth, m.clone()));
        for c in all.iter().filter(|c| c.parent.as_deref() == Some(m.id.as_str())) {
            walk(out, all, c, depth + 1);
        }
    }
    let mut out = Vec::new();
    for m in &sorted {
        if !m.parent.as_deref().is_some_and(|p| ids.contains(p)) {
            walk(&mut out, &sorted, m, 0);
        }
    }
    out
}

pub fn title_of(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = short_paths(line);
    let mut t: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        t.push('…');
    }
    t
}

/// A long path written in full (`/home/me/shots/a.png`, quoted, `@`-prefixed or a `file://` URL), as a
/// dropped or attached file comes, shortened to its file name: the title keeps the words around it.
fn short_paths(line: &str) -> String {
    const LONG: usize = 30;
    let words: Vec<&str> = line.split_whitespace().collect();
    if !words.iter().any(|w| w.len() > LONG || w.starts_with(['\'', '"']) || w.starts_with("@'")) {
        return line.to_string();
    }
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let (at, rest) = w.strip_prefix('@').map_or(("", w), |r| ("@", r));
        let mut whole = rest.to_string();
        let mut j = i;
        if let Some(q) = rest.chars().next().filter(|c| matches!(c, '\'' | '"')) {
            while !(whole.len() > 1 && whole.ends_with(q)) && j + 1 < words.len() {
                j += 1;
                whole.push(' ');
                whole.push_str(words[j]);
            }
            if !(whole.len() > 1 && whole.ends_with(q)) {
                j = i;
                whole = rest.to_string();
            }
        }
        let inner = whole.trim_matches(['\'', '"']);
        let inner = inner.strip_prefix("file://").unwrap_or(inner);
        let name = inner.rsplit(['/', '\\']).find(|s| !s.is_empty());
        match name {
            Some(name) if inner.len() > LONG && absolute(inner) => {
                out.push(format!("{at}{name}"));
                i = j + 1;
            }
            _ => {
                out.push(w.to_string());
                i += 1;
            }
        }
    }
    out.join(" ")
}

/// `/a/b`, `~/a` or `C:\a`.
fn absolute(p: &str) -> bool {
    let b = p.as_bytes();
    p.starts_with('/')
        || p.starts_with("~/")
        || (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::tool::ToolDisplay;

    #[test]
    fn a_long_path_in_the_first_request_is_titled_by_its_file_name() {
        let shot = "/tmp/claude-1000/project/scratchpad/My Shots/a b.png";
        assert_eq!(title_of(&format!("what is this? '{shot}'")), "what is this? a b.png");
        assert_eq!(title_of("look at @/home/someone/work/project/src/main.rs please"), "look at @main.rs please");
        assert_eq!(title_of("open file:///home/someone/Pictures/screenshots/x.png"), "open x.png");
        assert_eq!(title_of(r"why C:\Users\someone\Pictures\Screenshots\err.png"), "why err.png");
        // short paths and plain quotes stay as written
        assert_eq!(title_of("why is /usr/bin/env missing"), "why is /usr/bin/env missing");
        assert_eq!(title_of("rename 'old name' to new"), "rename 'old name' to new");
        assert_eq!(title_of("an 'unclosed /very/long/path/that/goes/on/and/on.rs"), "an 'unclosed on.rs");
    }
    #[test]
    fn sessions_born_in_the_same_second_are_all_saved() {
        // 3000 ids drawn in a few seconds from 65536 per second: some draw the same one
        let d = tempfile::tempdir().unwrap();
        let mut ids = std::collections::HashSet::new();
        let mut kept = Vec::new();
        for _ in 0..3000 {
            let s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
            assert!(ids.insert(s.id().to_string()));
            // an empty session is dropped with its file, so these stay open
            kept.push(s);
        }
        let files = std::fs::read_dir(d.path())
            .unwrap()
            .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "jsonl"))
            .count();
        assert_eq!(files, 3000);
    }
    #[test]
    fn a_cut_last_line_does_not_swallow_the_next_message() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
        s.append(&Message::user("one")).unwrap();
        let p = s.path().to_path_buf();
        drop(s);
        // the process died in the middle of the next line
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"{\"type\":\"message\",\"index\":1,\"ts\":\"x\",\"mess").unwrap();
        drop(f);
        let mut s = Session::open_path(&p).unwrap();
        assert_eq!(s.messages().len(), 1);
        s.append(&Message::user("two")).unwrap();
        drop(s);
        let s = Session::open_path(&p).unwrap();
        assert_eq!(s.messages().len(), 2, "the message after the cut line was lost");
    }

    #[test]
    fn compaction_rewrites_the_file_whole_and_keeps_the_outputs() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
        s.append(&Message::user("a")).unwrap();
        s.append(&Message::assistant("b")).unwrap();
        s.reset_with(&[Message::user("[summary]")]).unwrap();
        let p = s.path().to_path_buf();
        drop(s);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("\"type\":\"reset\""), "a reset line means a non-atomic rewrite: {text}");
        assert_eq!(text.matches("\"type\":\"message\"").count(), 1);
        assert!(!p.with_extension("jsonl.tmp").exists());
        let s = Session::open_path(&p).unwrap();
        assert_eq!(s.messages().len(), 1);
    }

    #[test]
    fn a_fork_keeps_the_first_messages_and_knows_its_parent() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
        for t in ["one", "two", "three"] {
            s.append(&Message::user(t)).unwrap();
        }
        let f = s.fork(2).unwrap();
        assert_ne!(f.id(), s.id());
        assert_eq!(f.messages().len(), 2);
        assert_eq!(f.meta().parent.as_deref(), Some(s.id()));
        assert_eq!(f.meta().fork_at, Some(2));
        assert_eq!(s.messages().len(), 3);
        // it reloads from disk with the same links
        let g = Session::open_path(f.path()).unwrap();
        assert_eq!(g.messages().len(), 2);
        assert_eq!(g.meta().parent.as_deref(), Some(s.id()));
        // an old meta line (no parent field) still loads
        let m: SessionMeta =
            serde_json::from_str(r#"{"id":"a","cwd":"/p","created":"","provider":"x","model":"y"}"#).unwrap();
        assert!(m.parent.is_none() && m.fork_at.is_none());
    }

    #[test]
    fn a_session_run_by_a_program_says_so_and_old_ones_read_as_the_persons() {
        let d = tempfile::tempdir().unwrap();
        let s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
        let first = std::fs::read_to_string(d.path().join(format!("{}.jsonl", s.meta().id))).unwrap();
        assert!(!first.contains("headless"), "the person's sessions keep the old meta line: {first}");
        let mut m = s.meta().clone();
        m.headless = true;
        let back: SessionMeta = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert!(back.headless);
    }

    #[test]
    fn the_tree_puts_children_under_their_parent() {
        let mk = |id: &str, parent: Option<&str>, updated: &str| SessionMeta {
            id: id.into(),
            cwd: "/p".into(),
            created: String::new(),
            updated: updated.into(),
            provider: "x".into(),
            model: "y".into(),
            title: String::new(),
            messages: 0,
            parent: parent.map(String::from),
            fork_at: None,
            headless: false,
        };
        let metas =
            vec![mk("a", None, "1"), mk("c", Some("a"), "3"), mk("b", Some("a"), "2"), mk("z", Some("gone"), "9")];
        let t: Vec<(usize, String)> = tree(&metas).into_iter().map(|(d, m)| (d, m.id)).collect();
        assert_eq!(t, vec![(0, "z".into()), (0, "a".into()), (1, "c".into()), (1, "b".into())]);
    }

    #[test]
    fn usage_lines_say_when_whose_and_how_much_was_held_and_old_readers_still_read_them() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), &PathBuf::from("/p"), "mock", "m1").unwrap();
        s.append(&Message::user("hello there")).unwrap();
        let meta = UsageMeta { model: Some("m1".into()), kind: Some(UsageKind::Main), held: Some(42) };
        s.record_usage_meta(Usage { input: 5, ..Default::default() }, 0.5, meta).unwrap();
        s.record_usage(Usage { input: 1, ..Default::default() }, 0.25).unwrap();
        let text = std::fs::read_to_string(s.path()).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .filter(|v: &serde_json::Value| v["type"] == "usage")
            .collect();
        assert_eq!(
            (lines[0]["kind"].as_str(), lines[0]["model"].as_str(), lines[0]["held"].as_u64()),
            (Some("main"), Some("m1"), Some(42))
        );
        assert!(lines[0]["ts"].as_str().is_some_and(|t| t.contains('T')));
        assert!(lines[1].get("kind").is_none() && lines[1].get("held").is_none(), "unknown stays unwritten");
        // a file with none of the new fields (an older nexo) reads as before
        let old = text.replace(",\"kind\":\"main\"", "").replace(",\"held\":42", "");
        std::fs::write(s.path(), old).unwrap();
        let mut back = Session::open_path(s.path()).unwrap();
        assert_eq!(back.usage().input, 6);
        // a rewrite keeps the totals in one line marked as a sum
        back.rewind(1).unwrap();
        let text = std::fs::read_to_string(back.path()).unwrap();
        let sums = text.lines().filter(|l| l.contains("\"type\":\"usage\"") && l.contains("\"kind\":\"sum\"")).count();
        assert_eq!(sums, 1, "{text}");
    }

    #[test]
    fn writes_reads_lists_and_survives_a_cut_line() {
        let d = tempfile::tempdir().unwrap();
        let cwd = PathBuf::from("/p");
        let mut s = Session::create_in(d.path(), &cwd, "mock", "m1").unwrap();
        s.append(&Message::user("Fix the bug in src/lib.rs please, it is urgent and the tests fail on main")).unwrap();
        s.append(&Message::new(Role::Assistant, vec![Part::tool_call("c1", "read", serde_json::json!({"path": "a"}))]))
            .unwrap();
        s.record_output(
            "c1",
            &ToolOutput::ok("text").with_display(ToolDisplay::File {
                path: "a".into(),
                text: "text".into(),
                start_line: 1,
            }),
        )
        .unwrap();
        s.record_usage(Usage { input: 5, output: 2, ..Default::default() }, 0.001).unwrap();
        let id = s.id().to_string();
        let path = s.path().to_path_buf();
        drop(s);
        // a line cut in the middle
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        write!(f, "{{\"type\":\"message\",\"index\":2,\"ts\":\"\",\"mess").unwrap();
        drop(f);
        let s = Session::open_path(&path).unwrap();
        assert_eq!(s.id(), id);
        assert_eq!(s.messages().len(), 2);
        assert_eq!(s.outputs()["c1"].content, "text");
        assert_eq!(s.usage().input, 5);
        assert!((s.cost() - 0.001).abs() < 1e-12);
        let want: String = "Fix the bug in src/lib.rs please, it is urgent and the tests fail on main"
            .chars()
            .take(60)
            .collect::<String>()
            + "…";
        assert_eq!(s.meta().title, want);
        let l = list_in(d.path());
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].messages, 2);
        assert_eq!(l[0].cwd, cwd);
    }

    #[test]
    fn a_deleted_session_waits_in_the_trash_and_comes_back() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("keep me")).unwrap();
        let id = s.id().to_string();
        drop(s);
        let to = trash_in(d.path(), &id).unwrap();
        assert!(to.is_file());
        assert!(list_in(d.path()).is_empty(), "the list no longer shows it");
        assert_eq!(list_in(&trash_dir_in(d.path())).len(), 1);
        assert!(trash_in(d.path(), &id).is_err(), "a second delete finds nothing");
        for bad in ["../x", "a/b", "", "..", "x.jsonl"] {
            assert!(trash_in(d.path(), bad).is_err(), "{bad:?} is refused");
        }
        restore_in(d.path(), &id).unwrap();
        assert_eq!(list_in(d.path())[0].messages, 1);
        assert!(restore_in(d.path(), &id).is_err());
    }

    #[test]
    fn a_rewind_rewrites_the_file_with_only_what_is_kept() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("first request")).unwrap();
        s.append(&Message::new(Role::Assistant, vec![Part::tool_call("c1", "read", serde_json::json!({"path": "a"}))]))
            .unwrap();
        s.record_output("c1", &ToolOutput::ok("A")).unwrap();
        s.append(&Message::tool_results(vec![Part::tool_result("c1", "A", false)])).unwrap();
        s.append(&Message::assistant("done")).unwrap();
        s.record_usage(Usage { input: 7, ..Default::default() }, 0.5).unwrap();
        s.rewind(1).unwrap();
        assert_eq!(s.messages().len(), 1);
        assert!(s.outputs().is_empty(), "outputs of dropped calls go with them");
        s.append(&Message::assistant("after the rewind")).unwrap();
        let p = s.path().to_path_buf();
        drop(s);
        assert!(!p.with_extension("jsonl.tmp").exists());
        let s = Session::open_path(&p).unwrap();
        assert_eq!(s.messages().len(), 2);
        assert_eq!(s.messages()[1].text(), "after the rewind");
        assert_eq!(s.meta().title, "first request");
        assert_eq!(s.usage().input, 7, "what was spent stays");
        assert!(s.outputs().is_empty());
        let md = s.export_markdown(|n, a| format!("{n} {}", a["path"].as_str().unwrap_or("")));
        assert!(md.starts_with("# first request"));
        assert!(md.contains("## You\n\nfirst request") && md.contains("## nexo\n\nafter the rewind"));
    }

    #[test]
    fn export_shows_calls_with_their_output_folded() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("look")).unwrap();
        s.append(&Message::new(
            Role::Assistant,
            vec![
                Part::Reasoning { text: "hm".into(), signature: None, encrypted: None, model: None },
                Part::tool_call("c1", "bash", serde_json::json!({"command": "seq 100"})),
            ],
        ))
        .unwrap();
        let long: String = (1..=100).map(|i| format!("{i}\n")).collect();
        s.record_output("c1", &ToolOutput::ok(long.clone())).unwrap();
        s.append(&Message::tool_results(vec![Part::tool_result("c1", long, false)])).unwrap();
        s.append(&Message::assistant("a hundred lines")).unwrap();
        let md = s.export_markdown(|n, a| format!("{n}: {}", a["command"].as_str().unwrap_or("")));
        assert!(md.contains("<details><summary>reasoning</summary>\n\nhm"));
        assert!(md.contains("**bash** bash: seq 100\n\n```\n1\n2\n"));
        assert!(md.contains("… 40 more lines\n```"));
        assert!(md.contains("## nexo\n\na hundred lines"));
        assert!(md.trim_end().ends_with("cost: $0.0000"));
        assert_eq!(s.export_path(), d.path().join(format!("{}.md", s.id())));
    }

    #[test]
    fn a_reset_replaces_the_history() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("a")).unwrap();
        s.append(&Message::assistant("b")).unwrap();
        s.reset_with(&[Message::user("[summary] a then b")]).unwrap();
        let p = s.path().to_path_buf();
        drop(s);
        let s = Session::open_path(&p).unwrap();
        assert_eq!(s.messages().len(), 1);
        assert!(s.messages()[0].text().starts_with("[summary]"));
    }
    #[cfg(unix)]
    #[test]
    fn sessions_are_readable_only_by_their_owner() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("sessions");
        let mut s = Session::create_in(&dir, Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("a secret")).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(s.path()), 0o600);
        s.rewind(0).unwrap();
        assert_eq!(mode(s.path()), 0o600, "the rewritten file too");
    }

    #[test]
    fn a_released_session_leaves_no_lock_even_while_still_held() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("hi")).unwrap();
        let lock = lock_path(s.path());
        assert!(lock.exists());
        // the agent stopped while a task still holds the session: the process may exit now
        s.release();
        assert!(!lock.exists());
        assert!(s.path().exists(), "a session with a message stays");
        assert!(s.append(&Message::user("late")).is_err(), "nothing is saved after the release");
        assert!(!lock.exists(), "a late write does not take the lock again");
        s.release();
        drop(s);
        let names: Vec<_> = std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
        // one opened and left without a word goes from the list at once
        let mut e = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        e.claim().unwrap();
        let p = e.path().to_path_buf();
        e.release();
        assert!(!p.exists() && !lock_path(&p).exists());
    }

    #[test]
    fn a_utf8_sequence_cut_by_a_crash_keeps_the_session() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("héllo")).unwrap();
        let p = s.path().to_path_buf();
        drop(s);
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"{\"type\":\"title\",\"title\":\"h\xc3").unwrap();
        drop(f);
        let s = Session::open_path(&p).unwrap();
        assert_eq!(s.messages().len(), 1);
        assert_eq!(s.messages()[0].text(), "héllo");
    }

    #[test]
    fn a_second_writer_is_refused_and_a_stale_lock_taken_over() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("first")).unwrap();
        let p = s.path().to_path_buf();
        let lock = lock_path(&p);
        assert!(lock.is_file());
        drop(s);
        assert!(!lock.exists(), "the lock goes with its session");
        // another live writer (pid 1 always runs) holds it
        std::fs::write(&lock, "1").unwrap();
        let mut s = Session::open_path(&p).unwrap();
        let e = s.append(&Message::user("x")).unwrap_err().to_string();
        assert!(e.contains("in use"), "{e}");
        // a lock whose pid cannot be read and that was left a day and more ago is taken over
        std::fs::write(&lock, "").unwrap();
        let f = File::options().write(true).open(&lock).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 3600)).unwrap();
        drop(f);
        s.append(&Message::user("y")).unwrap();
        assert_eq!(std::fs::read_to_string(&lock).unwrap(), stamp());
        assert_eq!(lock_pid(&lock), Some(std::process::id()));
        drop(s);
        if cfg!(target_os = "linux") {
            // a lock whose process is gone is taken over at once
            std::fs::write(&lock, "4000000000").unwrap();
            let mut s = Session::open_path(&p).unwrap();
            s.append(&Message::user("z")).unwrap();
            drop(s);
        }
        // the temporary file of a rewrite is named for its process and does not stay
        let mut s = Session::open_path(&p).unwrap();
        s.rewind(1).unwrap();
        drop(s);
        let left: Vec<_> = std::fs::read_dir(d.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1, "{left:?}");
    }

    #[test]
    fn continue_skips_empty_sessions_and_matches_the_folder_however_written() {
        let d = tempfile::tempdir().unwrap();
        let work = d.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let dir = d.path().join("sessions");
        let mut s = Session::create_in(&dir, &work, "mock", "m").unwrap();
        s.append(&Message::user("said something")).unwrap();
        let id = s.id().to_string();
        drop(s);
        let empty = Session::create_in(&dir, &work, "mock", "m").unwrap();
        drop(empty);
        let other_form = d.path().join("work").join("..").join("work");
        assert_eq!(latest_for_in(&dir, &other_form).map(|m| m.id), Some(id));
        assert!(Session::open("../x").is_err_and(|e| e.to_string().contains("not a session id")));
    }

    #[test]
    fn a_live_writer_keeps_its_lock_however_old() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("first")).unwrap();
        let p = s.path().to_path_buf();
        let lock = lock_path(&p);
        drop(s);
        let two_days_old = |text: &str| {
            std::fs::write(&lock, text).unwrap();
            let f = File::options().write(true).open(&lock).unwrap();
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 3600)).unwrap();
        };
        // pid 1 always runs; its lock, with when it started, is two days old
        two_days_old(&pid1_stamp());
        let mut s = Session::open_path(&p).unwrap();
        let r = s.append(&Message::user("x"));
        if cfg!(target_os = "linux") {
            assert!(r.unwrap_err().to_string().contains("in use"));
        } else {
            r.unwrap(); // no start time here: a pid in use may be another process, the age decides
        }
        drop(s);
        // without its start time, a live pid may have been taken by another process since: the age decides
        two_days_old("1");
        let mut s = Session::open_path(&p).unwrap();
        s.append(&Message::user("y")).unwrap();
        assert_eq!(lock_pid(&lock), Some(std::process::id()));
    }

    /// The lock pid 1 would write: its pid and, where the system tells, when it started and the boot.
    fn pid1_stamp() -> String {
        match (proc_start(1), boot_id()) {
            (Some(start), Some(boot)) => format!("1 {start} {boot}"),
            _ => "1".into(),
        }
    }

    #[test]
    fn a_failed_rewind_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        for t in ["one", "two", "three"] {
            s.append(&Message::user(t)).unwrap();
        }
        // the temporary file cannot be made: a folder holds its name
        let tmp = s.path().with_extension(format!("jsonl.{}.tmp", std::process::id()));
        std::fs::create_dir(&tmp).unwrap();
        assert!(s.rewind(1).is_err());
        assert_eq!(s.messages().len(), 3, "memory still matches the file");
        std::fs::remove_dir(&tmp).unwrap();
        s.rewind(2).unwrap();
        let g = Session::open_path(s.path()).unwrap();
        assert_eq!(g.messages().iter().map(|m| m.text()).collect::<Vec<_>>(), ["one", "two"]);
    }

    #[test]
    fn a_failed_reset_keeps_memory_as_the_file() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        for t in ["one", "two", "three"] {
            s.append(&Message::user(t)).unwrap();
        }
        let tmp = s.path().with_extension(format!("jsonl.{}.tmp", std::process::id()));
        std::fs::create_dir(&tmp).unwrap();
        assert!(s.reset_with(&[Message::user("summary")]).is_err());
        assert_eq!(s.messages().iter().map(|m| m.text()).collect::<Vec<_>>(), ["one", "two", "three"]);
        std::fs::remove_dir(&tmp).unwrap();
        s.reset_with(&[Message::user("summary")]).unwrap();
        assert_eq!(s.messages().len(), 1);
    }
    #[test]
    fn a_file_replaced_but_not_reopened_keeps_memory_as_the_file_and_says_so() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        for t in ["one", "two", "three"] {
            s.append(&Message::user(t)).unwrap();
        }
        FAIL_REOPEN.with(|f| f.set(true));
        assert!(s.rewind(1).is_err());
        assert_eq!(s.messages().len(), 1, "the file holds only the kept message now");
        // while it cannot be opened, every write says it was not saved
        assert!(s.append(&Message::user("lost")).is_err());
        FAIL_REOPEN.with(|f| f.set(false));
        s.append(&Message::user("four")).unwrap();
        let on_disk: Vec<String> = Session::open_path(s.path()).unwrap().messages().iter().map(|m| m.text()).collect();
        assert_eq!(on_disk, ["one", "four"]);
    }

    #[test]
    fn a_fork_keeps_the_goal_on_disk() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("one")).unwrap();
        s.record_goal(Some("ship it")).unwrap();
        let f = s.fork(1).unwrap();
        assert_eq!(f.goal(), Some("ship it"));
        let g = Session::open_path(f.path()).unwrap();
        assert_eq!(g.goal(), Some("ship it"));
    }

    #[test]
    fn the_list_counts_past_a_line_cut_inside_a_utf8_sequence() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("one")).unwrap();
        let p = s.path().to_path_buf();
        drop(s);
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"{\"type\":\"title\",\"title\":\"h\xc3\n").unwrap();
        drop(f);
        let mut s = Session::open_path(&p).unwrap();
        s.append(&Message::user("two")).unwrap();
        s.set_title("later").unwrap();
        drop(s);
        let m = &list_in(d.path())[0];
        assert_eq!((m.messages, m.title.as_str()), (2, "later"));
    }

    #[test]
    fn a_held_session_is_read_but_not_claimed_nor_touched() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("one")).unwrap();
        let p = s.path().to_path_buf();
        drop(s);
        // the other writer is in the middle of a line
        OpenOptions::new().append(true).open(&p).unwrap().write_all(b"{\"type\":\"ti").unwrap();
        let before = std::fs::read(&p).unwrap();
        std::fs::write(lock_path(&p), "1").unwrap(); // pid 1 always runs
        let mut s = Session::open_path(&p).unwrap();
        assert_eq!(s.messages().len(), 1);
        assert!(s.claim().unwrap_err().to_string().contains("in use"));
        drop(s);
        assert_eq!(std::fs::read(&p).unwrap(), before);
        std::fs::remove_file(lock_path(&p)).unwrap();
        let mut s = Session::open_path(&p).unwrap();
        s.claim().unwrap();
        s.append(&Message::user("two")).unwrap();
        assert_eq!(Session::open_path(&p).unwrap().messages().len(), 2);
    }

    #[test]
    fn a_lock_taken_over_stops_the_saving_and_is_left_to_its_new_holder() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("one")).unwrap();
        let lock = lock_path(s.path());
        // another nexo judged the lock stale and took it
        std::fs::write(&lock, "1").unwrap();
        let e = s.append(&Message::user("two")).unwrap_err().to_string();
        assert!(e.contains("taken over") && e.contains("pid 1"), "{e}");
        let p = s.path().to_path_buf();
        drop(s);
        assert_eq!(std::fs::read_to_string(&lock).unwrap(), "1", "its lock stays");
        assert_eq!(Session::open_path(&p).unwrap().messages().len(), 1);
        // a live lock deleted by hand is taken again by its writer
        std::fs::remove_file(&lock).unwrap();
        let mut s = Session::open_path(&p).unwrap();
        s.append(&Message::user("two")).unwrap();
        std::fs::remove_file(&lock).unwrap();
        s.append(&Message::user("three")).unwrap();
        assert_eq!(lock_pid(&lock), Some(std::process::id()));
        std::fs::write(&lock, "1").unwrap();
        assert!(s.append(&Message::user("four")).is_err());
        drop(s);
        std::fs::remove_file(&lock).unwrap();
        assert_eq!(Session::open_path(&p).unwrap().messages().len(), 3);
    }

    #[test]
    fn a_stale_lock_is_taken_over_only_while_it_is_the_one_judged() {
        let d = tempfile::tempdir().unwrap();
        let lock = d.path().join("x.jsonl.lock");
        // judged stale, but another nexo took it over first and holds a fresh one
        std::fs::write(&lock, "1").unwrap();
        assert!(!take_over(&lock, "4000000000").unwrap());
        assert_eq!(std::fs::read_to_string(&lock).unwrap(), "1", "the fresh lock stays");
        // still the one judged: it goes
        std::fs::write(&lock, "4000000000").unwrap();
        assert!(take_over(&lock, "4000000000").unwrap());
        assert!(!lock.exists());
        // already gone: nothing to take over
        assert!(!take_over(&lock, "4000000000").unwrap());
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0, "nothing left aside");
    }

    #[test]
    fn without_hard_links_a_fresh_lock_is_put_back_never_deleted() {
        let d = tempfile::tempdir().unwrap();
        let lock = d.path().join("x.jsonl.lock");
        let no_links = |_: &Path, _: &Path| Err(std::io::Error::other("no hard links here"));
        // another nexo took it over first: moved back, the place being empty
        std::fs::write(&lock, "1").unwrap();
        assert!(!take_over_with(&lock, "4000000000", no_links).unwrap());
        assert_eq!(std::fs::read_to_string(&lock).unwrap(), "1", "the fresh lock stays");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1, "nothing left aside");
        // yet another took the place meanwhile: the one moved aside is kept, the session is in use
        let third = |_: &Path, to: &Path| {
            std::fs::write(to, "2").unwrap();
            Err(std::io::Error::other("no hard links here"))
        };
        let e = take_over_with(&lock, "4000000000", third).unwrap_err().to_string();
        assert!(e.contains("in use") && e.contains("pid 1"), "{e}");
        assert_eq!(std::fs::read_to_string(&lock).unwrap(), "2");
        let aside = lock.with_extension(format!("lock.{}.stale", std::process::id()));
        assert_eq!(std::fs::read_to_string(&aside).unwrap(), "1", "kept, not deleted");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_lock_whose_pid_now_names_another_process_is_stale() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("first")).unwrap();
        let p = s.path().to_path_buf();
        let lock = lock_path(&p);
        drop(s);
        let stat = std::fs::read_to_string("/proc/1/stat").unwrap();
        let start: u64 = stat[stat.rfind(')').unwrap() + 1..].split_whitespace().nth(19).unwrap().parse().unwrap();
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap().trim().to_string();
        // pid 1 runs, but it is not the process that wrote these: it started at another time
        // (a reused pid, or a pid of a container), or the machine has booted since
        for other in [format!("1 {} {boot}", start + 12_345), format!("1 {start} not-this-boot")] {
            std::fs::write(&lock, &other).unwrap();
            let mut s = Session::open_path(&p).unwrap();
            s.append(&Message::user("x")).unwrap();
            assert_eq!(lock_pid(&lock), Some(std::process::id()));
            drop(s);
        }
        // the same process, still running: in use
        std::fs::write(&lock, format!("1 {start} {boot}")).unwrap();
        let mut s = Session::open_path(&p).unwrap();
        assert!(s.append(&Message::user("y")).unwrap_err().to_string().contains("in use"));
    }

    #[test]
    fn a_session_in_use_elsewhere_is_not_trashed() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("one")).unwrap();
        let id = s.id().to_string();
        let lock = lock_path(s.path());
        drop(s);
        std::fs::write(&lock, "1").unwrap(); // pid 1 always runs
        let e = trash_in(d.path(), &id).unwrap_err().to_string();
        assert!(e.contains("in use"), "{e}");
        std::fs::remove_file(&lock).unwrap();
        trash_in(d.path(), &id).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_lock_from_another_host_or_pid_namespace_is_judged_by_its_age() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("first")).unwrap();
        let p = s.path().to_path_buf();
        let lock = lock_path(&p);
        drop(s);
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap().trim().to_string();
        let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap().trim().to_string();
        let ns = std::fs::read_link("/proc/self/ns/pid").unwrap().to_string_lossy().into_owned();
        // the pids written there say nothing here: no process of theirs runs under them in this one
        for other in
            [format!("4000000000 77 {boot} {host} pid:[1]"), format!("4000000000 77 another-boot another-host {ns}")]
        {
            std::fs::write(&lock, &other).unwrap();
            let mut s = Session::open_path(&p).unwrap();
            assert!(s.append(&Message::user("x")).unwrap_err().to_string().contains("in use"), "{other}");
            drop(s);
            // left a day and more ago: taken over
            let f = File::options().write(true).open(&lock).unwrap();
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 3600)).unwrap();
            drop(f);
            let mut s = Session::open_path(&p).unwrap();
            s.append(&Message::user("y")).unwrap();
            assert_eq!(std::fs::read_to_string(&lock).unwrap(), stamp());
            drop(s);
        }
        // this process's own stamp names its host and namespace
        assert!(stamp().ends_with(&format!(" {host} {ns}")), "{}", stamp());
    }

    #[test]
    fn a_taker_that_ended_before_the_next_write_is_noticed_in_the_file() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("one")).unwrap();
        let lock = lock_path(s.path());
        let p = s.path().to_path_buf();
        // another nexo took the idle lock over, wrote, and ended, removing its lock
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        write_line(&mut f, &Line::Message { index: 1, ts: now(), message: Message::user("theirs") }).unwrap();
        std::fs::remove_file(&lock).unwrap();
        let e = s.append(&Message::user("mine")).unwrap_err().to_string();
        assert!(e.contains("taken over"), "{e}");
        assert!(!lock.exists(), "no lock taken again");
        drop(s);
        let on_disk: Vec<String> = Session::open_path(&p).unwrap().messages().iter().map(|m| m.text()).collect();
        assert_eq!(on_disk, ["one", "theirs"]);
        // and one that replaced the file with one of the same length: only Unix tells files apart
        // (device and inode); elsewhere a taker that wrote is caught by the length
        #[cfg(unix)]
        {
            let mut s = Session::open_path(&p).unwrap();
            s.claim().unwrap();
            let other = d.path().join("other");
            std::fs::copy(&p, &other).unwrap();
            std::fs::rename(&other, &p).unwrap();
            std::fs::remove_file(&lock).unwrap();
            assert!(s.append(&Message::user("mine")).unwrap_err().to_string().contains("taken over"));
        }
    }

    #[test]
    fn a_session_taken_over_stays_lost_after_its_taker_is_gone() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        s.append(&Message::user("one")).unwrap();
        let lock = lock_path(s.path());
        let p = s.path().to_path_buf();
        // another nexo took it over, wrote, and ended, removing its lock
        std::fs::write(&lock, "1").unwrap();
        assert!(s.append(&Message::user("two")).is_err());
        std::fs::remove_file(&lock).unwrap();
        let e = s.append(&Message::user("three")).unwrap_err().to_string();
        assert!(e.contains("taken over"), "{e}");
        assert!(s.rewind(0).is_err());
        assert!(!lock.exists(), "no lock taken again");
        drop(s);
        assert_eq!(Session::open_path(&p).unwrap().messages().len(), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_paused_holder_is_named_with_the_way_back() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let lock = Path::new("/x/20260101-000000-abcd.jsonl.lock");
        assert!(!stopped(pid));
        assert!(in_use(lock, Some(pid)).to_string().contains("in use by another nexo"));
        std::process::Command::new("kill").args(["-STOP", &pid.to_string()]).status().unwrap();
        let mut paused = false;
        for _ in 0..200 {
            paused = stopped(pid);
            if paused {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let msg = in_use(lock, Some(pid)).to_string();
        let _ = child.kill();
        let _ = child.wait();
        assert!(paused, "a stopped process reads as paused");
        assert!(msg.contains("paused nexo") && msg.contains("`fg`") && msg.contains(&format!("kill {pid}")), "{msg}");
        assert!(msg.contains("20260101-000000-abcd"), "{msg}");
    }
}
