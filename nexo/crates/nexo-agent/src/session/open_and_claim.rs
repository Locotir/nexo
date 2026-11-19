use super::*;

impl Session {
    pub fn create(cwd: &Path, provider: &str, model: &str) -> anyhow::Result<Session> {
        Session::create_in(&dir(), cwd, provider, model)
    }

    pub fn create_in(dir: &Path, cwd: &Path, provider: &str, model: &str) -> anyhow::Result<Session> {
        Session::create_linked(dir, cwd, provider, model, None, None)
    }

    pub(super) fn create_linked(
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
            stamps: Vec::new(),
            outputs: BTreeMap::new(),
            usage: Usage::default(),
            cost: 0.0,
            file: Some(file),
            goal: None,
            goal_state: GoalState::Active,
            prefix: None,
            context: None,
            stop: None,
            locked: true,
            touched: std::time::Instant::now(),
            open_tail: false,
            cut_tail: None,
            tail_removed: false,
            lost: None,
            closed: false,
            size,
            skipped: 0,
            unsaved: false,
            news: None,
            held: std::collections::BTreeSet::new(),
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
            stamps: Vec::new(),
            outputs: BTreeMap::new(),
            usage: Usage::default(),
            cost: 0.0,
            file: None,
            goal: None,
            goal_state: GoalState::Active,
            prefix: None,
            context: None,
            stop: None,
            locked: false,
            touched: std::time::Instant::now(),
            open_tail: false,
            cut_tail: None,
            tail_removed: false,
            lost: None,
            closed: false,
            size: 0,
            skipped: 0,
            unsaved: false,
            news: None,
            held: std::collections::BTreeSet::new(),
        }
    }

    pub fn open(id: &str) -> anyhow::Result<Session> {
        Session::open_path(&dir().join(format!("{}.jsonl", checked_id(id)?)))
    }

    /// Reads a session file; a truncated last line is ignored. Each line is read on its own, so a
    /// UTF-8 sequence cut by a crash spoils only the line it is in.
    pub fn open_path(path: &Path) -> anyhow::Result<Session> {
        // The cause goes into the text itself: callers print the outer error with `{e}`, which would hide a
        // chained io or serde error, and a bare "No such file or directory" does not say which file.
        let bytes = std::fs::read(path).map_err(|e| read_error(path, &e))?;
        let mut lines = bytes
            .split(|b| *b == b'\n')
            .map(|l| String::from_utf8_lossy(l.strip_suffix(b"\r").unwrap_or(l)))
            .filter(|l| !l.is_empty());
        let first = lines.next().ok_or_else(|| anyhow::anyhow!("empty session file {}", path.display()))?;
        // No advice here: `/new` means nothing to `/shared-memory` or a headless `--resume`, so the step to take belongs to the caller.
        let meta_line = serde_json::from_str(&first)
            .map_err(|e| anyhow::anyhow!("session file {} is damaged at line 1: {e}", path.display()))?;
        let Line::Meta(mut meta) = meta_line else {
            anyhow::bail!("{} does not start with a meta line", path.display());
        };
        let mut messages = Vec::new();
        let mut stamps: Vec<String> = Vec::new();
        let mut outputs = BTreeMap::new();
        let mut usage = Usage::default();
        let mut cost = 0.0;
        let mut goal = None;
        let mut goal_state = GoalState::Active;
        let mut prefix = None;
        let mut context = None;
        let mut stop = None;
        let mut skipped = 0;
        // outputs whose line left the text to the tool-result message: filled in from it once the messages are read
        let mut bare = std::collections::BTreeSet::new();
        for l in lines {
            match serde_json::from_str::<Line>(&l) {
                Ok(Line::Message { message, ts, .. }) => {
                    // the request that carried the stop line took it
                    if stop.as_deref().is_some_and(|l| message.text().starts_with(l)) {
                        stop = None;
                    }
                    stamps.push(ts);
                    messages.push(message)
                }
                Ok(Line::Stop { line, .. }) => stop = Some(line),
                Ok(Line::ToolOutput { call_id, output, result_chars }) => {
                    if result_chars.is_some() {
                        bare.insert(call_id.clone());
                    } else {
                        bare.remove(&call_id);
                    }
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
                Ok(Line::Prefix(p)) => prefix = Some(p),
                Ok(Line::Context(c)) => context = Some(c),
                // the map belongs to the prefix before it (one written after a rewrite already has the last)
                Ok(Line::PrefixShown { shown }) => {
                    if let Some(p) = &mut prefix {
                        p.shown = shown;
                    }
                }
                Ok(Line::Reset) => {
                    // the results this reset drops are the only place the text of those outputs was
                    fill_bare(&mut outputs, std::mem::take(&mut bare), &messages);
                    messages.clear();
                    stamps.clear();
                    stop = None;
                }
                Ok(Line::Headless) => meta.headless = true,
                Ok(Line::Meta(_)) => {}
                Err(_) => skipped += 1, // a cut line: whatever was being written when the process died
            }
        }
        fill_bare(&mut outputs, bare, &messages);
        if is_note_title(&meta.title) {
            meta.title = first_words_title(&messages);
        }
        meta.messages = messages.len();
        let file = OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|e| anyhow::anyhow!("session file {} cannot be written: {e}", path.display()))?;
        Ok(Session {
            meta,
            path: path.to_path_buf(),
            messages,
            stamps,
            outputs,
            usage,
            cost,
            file: Some(file),
            goal,
            goal_state,
            prefix,
            context,
            stop,
            locked: false,
            touched: std::time::Instant::now(),
            // a process killed mid-write leaves a last line without its end
            open_tail: bytes.last().is_some_and(|b| *b != b'\n'),
            cut_tail: cut_tail_start(&bytes),
            tail_removed: false,
            lost: None,
            closed: false,
            size: bytes.len() as u64,
            skipped,
            unsaved: false,
            news: None,
            held: std::collections::BTreeSet::new(),
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
            // a cut line is removed only while the file is as it was read: if anybody wrote since, what follows
            // the cut is theirs and the line stays, closed
            let cut = self.cut_tail.take().filter(|_| !self.file_changed());
            if let Some(f) = &mut self.file {
                if cut.is_some_and(|at| f.set_len(at).is_ok()) {
                    // the crash's leftover would be skipped and warned about at every later open
                    self.skipped = self.skipped.saturating_sub(1);
                    self.tail_removed = true;
                } else {
                    // so that the next line starts on a line of its own
                    f.write_all(b"\n")?;
                }
            }
            self.open_tail = false;
            self.mark();
        }
        Ok(())
    }
}

/// Words for a session file that could not be read: a missing one names the session, any other
/// cause is shown as it came. A missing one also says where the ids are listed, the `nexo --resume` case.
fn read_error(path: &Path, e: &std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        let id = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        anyhow::anyhow!("no session {id} at {}; `nexo sessions` lists them", path.display())
    } else {
        anyhow::anyhow!("session file {} cannot be read: {e}", path.display())
    }
}

/// The text of the outputs whose line left it out (see `Line::ToolOutput`), taken from the tool results among
/// `messages`. One whose result is not there (the file ended before it was appended) stays without text: the
/// display is all of it that was saved.
fn fill_bare(
    outputs: &mut BTreeMap<String, ToolOutput>,
    bare: std::collections::BTreeSet<String>,
    messages: &[Message],
) {
    if bare.is_empty() {
        return;
    }
    let texts = result_texts(messages);
    for id in bare {
        if let (Some(out), Some(text)) = (outputs.get_mut(&id), texts.get(id.as_str())) {
            out.content = (*text).to_string();
        }
    }
}

/// Where the last line of `bytes` starts when it is a line cut by a crash: it has no end and is not even a whole
/// JSON value. A last line that only lacks its end, or that is whole but of a type this nexo does not know (a
/// newer one wrote it), is not cut and is never removed.
fn cut_tail_start(bytes: &[u8]) -> Option<u64> {
    if bytes.last().is_none_or(|b| *b == b'\n') {
        return None;
    }
    let start = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let tail = String::from_utf8_lossy(&bytes[start..]);
    let tail = tail.trim();
    (!tail.is_empty() && serde_json::from_str::<serde_json::Value>(tail).is_err()).then_some(start as u64)
}
