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

/// The session most recently touched in `cwd`.
pub fn latest_for(cwd: &Path) -> Option<SessionMeta> {
    list().into_iter().find(|m| m.cwd == cwd)
}

fn read_meta(path: &Path) -> Option<SessionMeta> {
    let f = File::open(path).ok()?;
    let mut r = BufReader::new(f);
    let mut first = String::new();
    r.read_line(&mut first).ok()?;
    let Line::Meta(mut m) = serde_json::from_str(&first).ok()? else { return None };
    let mut n = 0;
    let mut title = None;
    for l in r.lines().map_while(Result::ok) {
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
        std::fs::create_dir_all(dir)?;
        // an id is the second it was made in and 4 random hex digits: two sessions born in the same
        // second can draw the same one, and the later takes another instead of going unsaved
        let mut tries = 0;
        let (id, path, mut file) = loop {
            let id = new_id();
            let path = dir.join(format!("{id}.jsonl"));
            match OpenOptions::new().create_new(true).append(true).open(&path) {
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
        writeln!(file, "{}", serde_json::to_string(&Line::Meta(meta.clone()))?)?;
        Ok(Session {
            meta,
            path,
            messages: Vec::new(),
            outputs: BTreeMap::new(),
            usage: Usage::default(),
            cost: 0.0,
            file: Some(file),
            goal: None,
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
        }
    }

    pub fn open(id: &str) -> anyhow::Result<Session> {
        Session::open_path(&dir().join(format!("{id}.jsonl")))
    }

    /// Reads a session file; a truncated last line is ignored.
    pub fn open_path(path: &Path) -> anyhow::Result<Session> {
        let text = std::fs::read_to_string(path)?;
        let mut lines = text.lines().peekable();
        let first = lines.next().ok_or_else(|| anyhow::anyhow!("empty session file {}", path.display()))?;
        let Line::Meta(mut meta) = serde_json::from_str(first)? else {
            anyhow::bail!("{} does not start with a meta line", path.display());
        };
        let mut messages = Vec::new();
        let mut outputs = BTreeMap::new();
        let mut usage = Usage::default();
        let mut cost = 0.0;
        let mut goal = None;
        for l in lines {
            match serde_json::from_str::<Line>(l) {
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
        let mut file = OpenOptions::new().append(true).open(path)?;
        // a process killed mid-write leaves a last line without its end: close it, so that the
        // next message starts on a line of its own
        if !text.ends_with('\n') {
            file.write_all(b"\n")?;
        }
        Ok(Session { meta, path: path.to_path_buf(), messages, outputs, usage, cost, file: Some(file), goal })
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

    fn write(&mut self, line: &Line) -> anyhow::Result<()> {
        if let Some(f) = &mut self.file {
            writeln!(f, "{}", serde_json::to_string(line)?)?;
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
    /// were spent.
    pub fn rewind(&mut self, keep: usize) -> anyhow::Result<()> {
        let keep = keep.min(self.messages.len());
        self.messages.truncate(keep);
        let ids: std::collections::BTreeSet<String> =
            self.messages.iter().flat_map(|m| m.tool_calls().map(|(id, _, _)| id.to_string())).collect();
        self.outputs.retain(|id, _| ids.contains(id));
        self.rewrite()
    }

    /// Writes the whole file again from memory: to a temporary file next to it, renamed over it.
    fn rewrite(&mut self) -> anyhow::Result<()> {
        self.meta.messages = self.messages.len();
        self.meta.updated = now();
        if self.file.is_none() {
            return Ok(());
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        {
            let mut f = File::create(&tmp)?;
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
        }
        // Windows refuses to replace a file that is open: the handle closes first
        self.file = None;
        std::fs::rename(&tmp, &self.path)?;
        self.file = Some(OpenOptions::new().append(true).open(&self.path)?);
        Ok(())
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
        f.goal = self.goal.clone();
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
}
