//! Sessions on disk: one JSON-lines file each, appended as things happen,
//! so nothing is lost and a session can be continued.
//!
//! Lines: `{"type":"meta",...}` first; then `message` (with its index),
//! `tool_output` (its text left out when the tool-result message holds it), `usage`, `title`, `prefix_shown` (the project
//! map alone, for a changed tree), `stop` (the person stopped the latest
//! request), `context` (the context policy that runs), `headless` (a program took the session over), and `reset`
//! (the history was rewritten: what follows replaces what came before).

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

// Sessions hold tool outputs, which can hold secrets: their folder and files are the user's alone (on unix).
use nexo_core::fsmode::{create_private_dir, private_file};
use nexo_core::message::{Message, Part, Role};
use nexo_core::paths;
use nexo_core::stream::Usage;
use nexo_core::tool::ToolOutput;
use nexo_core::GoalState;
use serde::{Deserialize, Serialize};

mod history;
pub use history::{history, HistorySnapshot};
mod lock_file;
mod markdown_export;
mod open_and_claim;
mod recording;
mod rewind_and_fork;
mod sweep;
#[cfg(test)]
mod tests;
mod trash;
mod tree_and_titles;

use lock_file::*;
use recording::{held_by_result, output_line, result_texts};
pub use sweep::{sweep_orphans, sweep_orphans_in_background};
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
    /// Started by `nexo -p` or a team lane (a script or another program), not by the person: left out of `/resume`
    /// and of `nexo -c`.
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
    /// The view failed at this call and the whole history was sent.
    pub fallback: Option<bool>,
}

/// The context policy a session ran with, as a `context` line writes it: what was asked, what runs, and what of the
/// ask was left out or replaced.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ContextRecord {
    /// `ContextPolicy::name()` of the policy that runs.
    pub policy: String,
    /// `[context] policy` as configured, trimmed.
    pub asked: String,
    /// The curator spec that runs; empty for plain and compact.
    #[serde(default)]
    pub spec: String,
    /// What was asked and left out or replaced, as the person sees it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caveat: Option<String>,
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
        /// Set when this output's text is the one the call's tool-result message carries: `output.content` is then
        /// left empty here (the message holds the text, the file has it once) and this is its length in chars.
        /// Reading the session fills the text back in from the message; old files have none and hold it whole.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_chars: Option<usize>,
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
        /// The call's view failed and the whole history went; absent when it did not, and in old files.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback: Option<bool>,
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
    /// The context setup in force from here on: the last such line wins. Old readers skip it.
    Context(ContextRecord),
    /// The prefix in force from here on; the last such line wins.
    Prefix(Prefix),
    /// The project map the model was last shown, which is all a changed tree alters of the prefix: it replaces `shown` in
    /// the prefix read before it, so a changed tree does not write the whole prefix again. A reader that does not know
    /// the line skips it (and counts it among the lines it could not read).
    PrefixShown {
        shown: String,
    },
    Reset,
    /// A program (a lane's runner) took the session over: it is not the person's own from here on. The line is how
    /// a session started before that is marked, since the first line of a file is not rewritten. Old readers skip it.
    Headless,
    /// The person stopped the latest request (Esc): the line the next request carries, until a message that starts
    /// with it is appended (`Session::stop`). Old readers skip it.
    Stop {
        ts: String,
        line: String,
    },
}

pub struct Session {
    meta: SessionMeta,
    path: PathBuf,
    messages: Vec<Message>,
    /// When each message was written, parallel to `messages`: a rewrite of the file keeps the time a message was
    /// said instead of stamping it with the time of the rewrite.
    stamps: Vec<String>,
    outputs: BTreeMap<String, ToolOutput>,
    usage: Usage,
    cost: f64,
    file: Option<File>,
    goal: Option<String>,
    goal_state: GoalState,
    prefix: Option<Prefix>,
    /// The context setup last written, so the same one is not written again.
    context: Option<ContextRecord>,
    /// The person stopped the latest request and no request has carried the line that says so yet: that line.
    stop: Option<String>,
    /// Whether this session holds the lock next to its file (taken at the first write).
    locked: bool,
    /// When the lock was last touched by this session.
    touched: std::time::Instant,
    /// The file ends inside a line (a process died mid-write): closed once the lock is held.
    open_tail: bool,
    /// Where the cut last line starts, when the file ended in one at open (not just a line missing its end): the
    /// claim removes it if nobody has written since.
    cut_tail: Option<u64>,
    /// The cut last line was removed by the claim, so the file holds no more of it.
    tail_removed: bool,
    /// The pid of the nexo that took the lock over, once one has: nothing more is saved by this
    /// session, even after that nexo lets the lock go (its history and this one have parted).
    lost: Option<String>,
    /// Let go by [`Session::release`]: the agent has stopped, and nothing more is saved.
    closed: bool,
    /// The length of the file after this session's own last write: a longer file, or another one,
    /// holds lines of somebody else.
    size: u64,
    /// Lines of the file that could not be read when it was opened (a cut or garbled line): their
    /// content is not in this session.
    skipped: usize,
    /// Saving is failing right now: the person has been told once, and is told again when it works.
    unsaved: bool,
    /// What the person has not yet been told about saving.
    news: Option<SaveNews>,
    /// Calls whose output is kept in memory and not written yet: it goes into the file with the tool-result message
    /// that answers the call (see [`Session::hold_output`]).
    held: std::collections::BTreeSet<String>,
}

/// A change in whether the session is being saved, for the person to be told once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveNews {
    /// Saving just failed (the reason); the conversation goes on in memory.
    Stopped(String),
    /// Saving works again.
    Resumed,
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
        if self.closed {
            return;
        }
        // an output kept for the result of a round that never ended (the turn was dropped) is not lost with the
        // session: written before the session is marked closed, which refuses every write
        self.write_held_whole();
        self.closed = true;
        // opened and left without a word: nothing worth keeping in the list
        let empty = self.file.is_some()
            && self.locked
            && self.meta.messages == 0
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

/// The metadata of one session in `dir`, or none when there is no such session.
pub fn meta_in(dir: &Path, id: &str) -> Option<SessionMeta> {
    read_meta(&dir.join(format!("{}.jsonl", checked_id(id).ok()?)))
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

/// The session `nexo -c` opens in `cwd`: [`latest_for`] without the sessions programs run (`nexo -p`, team lanes),
/// which the person did not start and a lane's runner may hold. `nexo -c -p` keeps [`latest_for`]: a script
/// continues the session of its own earlier run. Interactive `nexo -c` falls back to [`latest_for`] when this finds
/// nothing (cli/dispatch.rs).
pub fn latest_own_for(cwd: &Path) -> Option<SessionMeta> {
    latest_own_for_in(&dir(), cwd)
}

pub fn latest_own_for_in(dir: &Path, cwd: &Path) -> Option<SessionMeta> {
    let want = same_form(cwd);
    list_in(dir).into_iter().find(|m| m.messages > 0 && !m.headless && same_form(&m.cwd) == want)
}

/// Marks the session `id` of `dir` as run by a program, when it was not marked at birth (a team lane keeps one
/// session for good, and the lanes that exist were started before lanes were marked). It appends a line instead
/// of rewriting the first one, and leaves a session another nexo holds alone: that nexo would take the longer
/// file for somebody else's writing. Returns whether the line was written.
pub fn mark_headless_in(dir: &Path, id: &str) -> anyhow::Result<bool> {
    let Some(meta) = meta_in(dir, id) else { return Ok(false) };
    let path = dir.join(format!("{id}.jsonl"));
    if meta.headless || lock_file::holder(&lock_path(&path)).is_some_and(|p| p != Some(std::process::id())) {
        return Ok(false);
    }
    let mut f = OpenOptions::new().read(true).append(true).open(&path)?;
    // a file cut in the middle of a line gets the line's end first, so that the mark is a line of its own
    let mut last = *b"\n";
    if std::io::Seek::seek(&mut f, std::io::SeekFrom::End(-1)).is_ok() {
        std::io::Read::read_exact(&mut f, &mut last)?;
    }
    if last[0] != b'\n' {
        f.write_all(b"\n")?;
    }
    write_line(&mut f, &Line::Headless)?;
    Ok(true)
}

/// When the session was last written, in milliseconds since the epoch; 0 for a file with no readable time.
pub fn updated_ms(m: &SessionMeta) -> i64 {
    time::OffsetDateTime::parse(&m.updated, &time::format_description::well_known::Rfc3339)
        .map(|t| (t.unix_timestamp_nanos() / 1_000_000) as i64)
        .unwrap_or(0)
}

/// Whether two paths are the same folder however they are written (a link, a trailing slash).
pub fn same_folder(a: &Path, b: &Path) -> bool {
    a == b || same_form(a) == same_form(b)
}

/// What `/resume` offers, newest first within two groups: the sessions that ran in `cwd`, then the others. Sessions
/// nothing was said in and the ones programs run are left out. Every folder's sessions are offered because
/// `/resume <id>` works on any of them; the row says where it ran.
pub fn rows_for(metas: Vec<SessionMeta>, cwd: &Path) -> Vec<nexo_core::agent::SessionRow> {
    // One canonical form per distinct folder: the plain comparison settles most rows, and a folder written another
    // way (a link, a trailing slash) costs one look at the disk, not one per session.
    let mut seen: std::collections::HashMap<PathBuf, bool> = std::collections::HashMap::new();
    let mut want = None;
    let mut rows: Vec<nexo_core::agent::SessionRow> = metas
        .into_iter()
        .filter(|m| m.messages > 0 && !m.headless)
        .map(|m| {
            let here = m.cwd == cwd
                || *seen.entry(m.cwd.clone()).or_insert_with(|| {
                    let want = want.get_or_insert_with(|| same_form(cwd));
                    same_form(&m.cwd) == *want
                });
            let updated_ms = updated_ms(&m);
            nexo_core::agent::SessionRow {
                id: m.id,
                title: m.title,
                cwd: m.cwd,
                here,
                messages: m.messages,
                updated_ms,
            }
        })
        .collect();
    // a stable sort: the order inside each group stays newest first
    rows.sort_by_key(|r| !r.here);
    rows
}

/// [`rows_for`] the sessions on disk.
pub fn picker_rows(cwd: &Path) -> Vec<nexo_core::agent::SessionRow> {
    rows_for(list(), cwd)
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

/// What the list shows of one file: the head for the id and folder, the last title, and the messages since
/// the last reset. A line is told by how it starts (the writer puts `type` first), so a message is counted
/// and a tool output, which can be megabytes, is read past without parsing either; only the short title and
/// reset lines, and any line that does not start like the writer's, are parsed.
fn read_meta(path: &Path) -> Option<SessionMeta> {
    const MESSAGE: &[u8] = br#"{"type":"message""#;
    const SHORT: [&[u8]; 3] = [br#"{"type":"title""#, br#"{"type":"reset""#, br#"{"type":"headless""#];
    const OTHER: &[u8] = br#"{"type":""#;
    let f = File::open(path).ok()?;
    let mut r = BufReader::with_capacity(256 * 1024, f);
    let mut first = String::new();
    r.read_line(&mut first).ok()?;
    let Line::Meta(mut m) = serde_json::from_str(&first).ok()? else { return None };
    let mut n = 0;
    let mut title = None;
    let mut headless = false;
    let mut buf = Vec::new();
    // each line on its own, as `open_path` reads them: a UTF-8 sequence cut by a crash spoils one line
    loop {
        buf.clear();
        if !matches!(r.read_until(b'\n', &mut buf), Ok(len) if len > 0) {
            break;
        }
        let line = buf.trim_ascii();
        if line.starts_with(MESSAGE) {
            // a line cut by a crash lacks its end: it is not a message, as when every line was parsed
            if line.ends_with(b"}}") {
                n += 1;
            }
        } else if SHORT.iter().any(|p| line.starts_with(p)) || !line.starts_with(OTHER) {
            match serde_json::from_str::<Line>(&String::from_utf8_lossy(line)) {
                Ok(Line::Message { .. }) => n += 1,
                Ok(Line::Reset) => n = 0,
                Ok(Line::Title { title: t }) => title = Some(t),
                Ok(Line::Headless) => headless = true,
                _ => {}
            }
        }
    }
    m.messages = n;
    m.headless |= headless;
    if let Some(t) = title {
        m.title = t;
    }
    if is_note_title(&m.title) {
        m.title = first_words_title(&first_user_messages(path));
    }
    if let Ok(md) = std::fs::metadata(path) {
        if let Ok(t) = md.modified() {
            let t: time::OffsetDateTime = t.into();
            m.updated = t.format(&time::format_description::well_known::Rfc3339).unwrap_or_default();
        }
    }
    Some(m)
}
