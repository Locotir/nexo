use super::*;

impl Session {
    /// The file is no longer the one this session wrote, as far as it left it: longer, or (on Unix,
    /// where files are told apart by device and inode) replaced.
    pub(super) fn file_changed(&self) -> bool {
        let Ok(now) = std::fs::metadata(&self.path) else { return false };
        now.len() != self.size || self.replaced(&now)
    }

    /// Another file now stands at the path: on Unix, told apart by device and inode.
    #[cfg(unix)]
    fn replaced(&self, now: &std::fs::Metadata) -> bool {
        use std::os::unix::fs::MetadataExt;
        self.file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .is_some_and(|mine| (now.dev(), now.ino()) != (mine.dev(), mine.ino()))
    }

    /// Elsewhere a replaced file is not told apart from the one this session wrote.
    #[cfg(not(unix))]
    fn replaced(&self, _now: &std::fs::Metadata) -> bool {
        false
    }

    /// Remembers the length of the file as this session left it.
    pub(super) fn mark(&mut self) {
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
    pub(super) fn ensure_open(&mut self) -> anyhow::Result<()> {
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

    /// The prefix this session sends before its messages, when one was saved.
    pub fn prefix(&self) -> Option<&Prefix> {
        self.prefix.as_ref()
    }

    /// Saves the prefix (the last one saved is the one in force).
    pub fn record_prefix(&mut self, prefix: &Prefix) -> anyhow::Result<()> {
        self.prefix = Some(prefix.clone());
        self.write(&Line::Prefix(prefix.clone()))
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
}
