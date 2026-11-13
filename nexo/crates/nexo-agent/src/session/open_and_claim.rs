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
            outputs: BTreeMap::new(),
            usage: Usage::default(),
            cost: 0.0,
            file: Some(file),
            goal: None,
            goal_state: GoalState::Active,
            prefix: None,
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
            prefix: None,
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
        let mut prefix = None;
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
                Ok(Line::Prefix(p)) => prefix = Some(p),
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
            prefix,
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
}
