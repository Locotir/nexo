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
    },
    Title {
        title: String,
    },
    Model {
        provider: String,
        model: String,
    },
    /// The goal in force from here on (None: cleared).
    Goal {
        text: Option<String>,
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
    /// Whether this session holds the lock next to its file (taken at the first write).
    locked: bool,
    /// When the lock was last touched by this session.
    touched: std::time::Instant,
    /// The file ends inside a line (a process died mid-write): closed once the lock is held.
    open_tail: bool,
}

impl Drop for Session {
    fn drop(&mut self) {
        // only a lock still this process's: another nexo may have taken it over
        let lock = lock_path(&self.path);
        if self.locked && std::fs::read_to_string(&lock).is_ok_and(|t| t == stamp()) {
            let _ = std::fs::remove_file(lock);
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

/// What this process writes in the locks it takes: its pid and, where the system tells (Linux),
/// when it started and the boot it runs in, so that the same pid used later by another process
/// (after a reboot, or in another container) is not taken for this one.
fn stamp() -> &'static str {
    static STAMP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    STAMP.get_or_init(|| {
        let pid = std::process::id();
        match (proc_start(pid), boot_id()) {
            (Some(start), Some(boot)) => format!("{pid} {start} {boot}"),
            _ => pid.to_string(),
        }
    })
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
/// so), or, where it cannot say, older than [`STALE_LOCK`]. None when there is no lock.
fn judge(lock: &Path) -> Option<(String, Option<Option<u32>>)> {
    let md = std::fs::metadata(lock).ok()?;
    let text = std::fs::read_to_string(lock).unwrap_or_default();
    let mut words = text.split_whitespace();
    let pid: Option<u32> = words.next().and_then(|p| p.parse().ok());
    let start: Option<u64> = words.next().and_then(|s| s.parse().ok());
    let other_boot = match (words.next(), boot_id()) {
        (Some(was), Some(now)) => was != now,
        _ => false,
    };
    let old = md.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > STALE_LOCK);
    let live = pid.and_then(|p| {
        let reused = matches!((start, proc_start(p)), (Some(was), Some(now)) if was != now);
        alive(p).map(|live| live && !reused)
    });
    let stale = other_boot
        || match live {
            Some(live) => !live,
            None => old,
        };
    let holder = (!stale).then_some(pid);
    Some((text, holder))
}

/// Who holds the lock at `lock`, or None when it is free or stale (see [`judge`]).
fn holder(lock: &Path) -> Option<Option<u32>> {
    judge(lock)?.1
}

fn in_use(lock: &Path, pid: Option<u32>) -> anyhow::Error {
    anyhow::anyhow!(
        "session {} is in use by another nexo (pid {}); if none runs, delete {}",
        lock.file_stem().and_then(|s| Path::new(s).file_stem()).unwrap_or_default().to_string_lossy(),
        pid.map(|p| p.to_string()).unwrap_or_else(|| "unknown".into()),
        lock.display()
    )
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
                let ours = text.split_whitespace().next() == Some(std::process::id().to_string().as_str());
                match held {
                    Some(pid) if !ours => return Err(in_use(lock, pid)),
                    _ => {
                        take_over(lock, &text);
                    }
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::bail!("cannot take the lock {}", lock.display())
}

/// Removes the lock at `lock` when it still holds `judged`, the text found stale: moved aside
/// first, so that one taken meanwhile by another nexo is put back, never deleted. Whether it went.
fn take_over(lock: &Path, judged: &str) -> bool {
    let aside = lock.with_extension(format!("lock.{}.stale", std::process::id()));
    if std::fs::rename(lock, &aside).is_err() {
        return false; // another took it over first
    }
    let same = std::fs::read_to_string(&aside).is_ok_and(|t| t == judged);
    if !same {
        // a lock taken since: back in place, unless yet another is there now
        let _ = std::fs::hard_link(&aside, lock);
    }
    let _ = std::fs::remove_file(&aside);
    same
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
        };
        take_lock(&lock_path(&path))?;
        write_line(&mut file, &Line::Meta(meta.clone()))?;
        Ok(Session {
            meta,
            path,
            messages: Vec::new(),
            outputs: BTreeMap::new(),
            usage: Usage::default(),
            cost: 0.0,
            file: Some(file),
            goal: None,
            locked: true,
            touched: std::time::Instant::now(),
            open_tail: false,
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
            locked: false,
            touched: std::time::Instant::now(),
            open_tail: false,
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
        for l in lines {
            match serde_json::from_str::<Line>(&l) {
                Ok(Line::Message { message, .. }) => messages.push(message),
                Ok(Line::ToolOutput { call_id, output }) => {
                    outputs.insert(call_id, output);
                }
                Ok(Line::Usage { usage: u, cost_usd }) => {
                    usage += u;
                    cost += cost_usd;
                }
                Ok(Line::Title { title }) => meta.title = title,
                Ok(Line::Model { provider, model }) => {
                    meta.provider = provider;
                    meta.model = model;
                }
                Ok(Line::Goal { text }) => goal = text,
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
            locked: false,
            touched: std::time::Instant::now(),
            // a process killed mid-write leaves a last line without its end
            open_tail: bytes.last().is_some_and(|b| *b != b'\n'),
        })
    }

    /// Takes the session for writing now, as a resume does before anything is spent: fails when
    /// another nexo holds it. Reading a session never takes it; its first write does.
    pub fn claim(&mut self) -> anyhow::Result<()> {
        let Some(f) = &mut self.file else { return Ok(()) };
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
                // deleted by hand while in use: taken again
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => take_lock(&lock)?,
                Ok(t) => anyhow::bail!(
                    "session {} was taken over by another nexo (pid {}): nothing more is saved here",
                    self.meta.id,
                    t.split_whitespace().next().unwrap_or("unknown")
                ),
                Err(e) => return Err(e.into()),
            }
        }
        if self.open_tail {
            // so that the next line starts on a line of its own
            f.write_all(b"\n")?;
            self.open_tail = false;
        }
        Ok(())
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
        self.outputs.insert(call_id.to_string(), output.clone());
        self.write(&Line::ToolOutput { call_id: call_id.into(), output: output.clone() })
    }

    pub fn record_usage(&mut self, usage: Usage, cost_usd: f64) -> anyhow::Result<()> {
        self.usage += usage;
        self.cost += cost_usd;
        self.write(&Line::Usage { usage, cost_usd })
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

    pub fn record_goal(&mut self, text: Option<&str>) -> anyhow::Result<()> {
        self.goal = text.map(String::from);
        self.write(&Line::Goal { text: text.map(String::from) })
    }

    pub fn set_title(&mut self, title: &str) -> anyhow::Result<()> {
        self.meta.title = title.into();
        self.write(&Line::Title { title: title.into() })
    }

    /// The history was rewritten (compaction): what follows replaces it.
    pub fn reset_with(&mut self, messages: &[Message]) -> anyhow::Result<()> {
        // one atomic rewrite: a crash leaves the old file or the new one, never a half history
        self.messages = messages.to_vec();
        self.rewrite()
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
                writeln!(f, "{}", serde_json::to_string(&Line::Usage { usage: self.usage, cost_usd: self.cost })?)?;
            }
            if self.goal.is_some() {
                writeln!(f, "{}", serde_json::to_string(&Line::Goal { text: self.goal.clone() })?)?;
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
                        let body = match self.outputs.get(id) {
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
    let mut t: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        t.push('…');
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::tool::ToolDisplay;

    #[test]
    fn sessions_born_in_the_same_second_are_all_saved() {
        // 3000 ids drawn in a few seconds from 65536 per second: some draw the same one
        let d = tempfile::tempdir().unwrap();
        let mut ids = std::collections::HashSet::new();
        for _ in 0..3000 {
            let s = Session::create_in(d.path(), Path::new("/p"), "mock", "m1").unwrap();
            assert!(ids.insert(s.id().to_string()));
        }
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 3000);
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
        };
        let metas =
            vec![mk("a", None, "1"), mk("c", Some("a"), "3"), mk("b", Some("a"), "2"), mk("z", Some("gone"), "9")];
        let t: Vec<(usize, String)> = tree(&metas).into_iter().map(|(d, m)| (d, m.id)).collect();
        assert_eq!(t, vec![(0, "z".into()), (0, "a".into()), (1, "c".into()), (1, "b".into())]);
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
        let s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
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
        let s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
        let p = s.path().to_path_buf();
        let lock = lock_path(&p);
        drop(s);
        // pid 1 always runs; its lock is two days old
        std::fs::write(&lock, "1").unwrap();
        let f = File::options().write(true).open(&lock).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 3600)).unwrap();
        drop(f);
        let mut s = Session::open_path(&p).unwrap();
        let r = s.append(&Message::user("x"));
        if cfg!(unix) {
            assert!(r.unwrap_err().to_string().contains("in use"));
        } else {
            r.unwrap(); // no liveness check here: the age decides
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
        assert!(!take_over(&lock, "4000000000"));
        assert_eq!(std::fs::read_to_string(&lock).unwrap(), "1", "the fresh lock stays");
        // still the one judged: it goes
        std::fs::write(&lock, "4000000000").unwrap();
        assert!(take_over(&lock, "4000000000"));
        assert!(!lock.exists());
        // already gone: nothing to take over
        assert!(!take_over(&lock, "4000000000"));
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0, "nothing left aside");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_lock_whose_pid_now_names_another_process_is_stale() {
        let d = tempfile::tempdir().unwrap();
        let s = Session::create_in(d.path(), Path::new("/p"), "mock", "m").unwrap();
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
}
