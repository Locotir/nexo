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

mod lock_file;
mod markdown_export;
mod open_and_claim;
mod recording;
mod rewind_and_fork;
#[cfg(test)]
mod tests;
mod trash;
mod tree_and_titles;

use lock_file::*;
pub use trash::*;
pub use tree_and_titles::*;

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

/// What a session sent before its messages, fixed for as long as it lives (the provider caches a prefix only
/// when it is byte-identical to an earlier one): the names of its tools, the parts of the system text that can
/// change (see [`crate::prompt::Volatile`]) and the project map the model was last shown. Old session files have none.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Prefix {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub volatile: crate::prompt::Volatile,
    /// The project map as the model last saw it (in the system text or in a note of a request).
    #[serde(default)]
    pub shown: String,
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
    /// The prefix in force from here on; the last such line wins.
    Prefix(Prefix),
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
    prefix: Option<Prefix>,
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
