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
        let done = self.write_line_now(line);
        self.track(&done);
        done
    }

    fn write_line_now(&mut self, line: &Line) -> anyhow::Result<()> {
        self.ensure_open()?;
        self.claim()?;
        self.check_present()?;
        if let Some(f) = &mut self.file {
            if let Err(e) = write_line(f, line) {
                // a full disk can take half a line: the next one starts on a line of its own
                self.open_tail = true;
                return Err(e);
            }
        }
        self.mark();
        self.meta.updated = now();
        Ok(())
    }

    /// An open file keeps accepting writes after its name is gone or taken by a directory, and they
    /// reach nobody: that is a failure to save, not a success.
    fn check_present(&self) -> anyhow::Result<()> {
        if self.file.is_none() || self.path.as_os_str().is_empty() {
            return Ok(());
        }
        match std::fs::metadata(&self.path) {
            Ok(m) if m.is_file() => Ok(()),
            Ok(_) => anyhow::bail!("{} is not a file any more", self.path.display()),
            Err(e) => anyhow::bail!("the session file {} cannot be reached: {e}", self.path.display()),
        }
    }

    /// Records whether saving works, for the person to be told when that changes (see [`SaveNews`]).
    pub(super) fn track(&mut self, done: &anyhow::Result<()>) {
        match done {
            Err(e) if !self.unsaved => {
                self.unsaved = true;
                self.news = Some(SaveNews::Stopped(format!("{e:#}")));
            }
            Ok(()) if self.unsaved => {
                self.unsaved = false;
                // failed and fixed before anyone was told: nothing to say
                self.news = match self.news.take() {
                    Some(SaveNews::Stopped(_)) => None,
                    _ => Some(SaveNews::Resumed),
                };
            }
            _ => {}
        }
    }

    /// The change in saving not yet told to the person, once.
    pub fn take_save_news(&mut self) -> Option<SaveNews> {
        self.news.take()
    }

    /// How many lines of the file could not be read when the session was opened.
    pub fn skipped_lines(&self) -> usize {
        self.skipped
    }

    pub fn append(&mut self, m: &Message) -> anyhow::Result<()> {
        self.append_at(m, now())
    }

    /// [`Session::append`] for a message said at `ts`: a fork keeps the times of the messages it copies.
    pub(super) fn append_at(&mut self, m: &Message, ts: String) -> anyhow::Result<()> {
        let index = self.messages.len();
        self.messages.push(m.clone());
        self.stamps.push(ts.clone());
        self.meta.messages = self.messages.len();
        // the request that carries the stop line takes it
        if self.stop.as_deref().is_some_and(|l| m.text().starts_with(l)) {
            self.stop = None;
        }
        self.write_held_for(m);
        self.write(&Line::Message { index, ts, message: m.clone() })?;
        if self.meta.title.is_empty() && m.role == Role::User && !m.is_tool_results() {
            let t = title_of_message(&m.text());
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
        self.write(&Line::ToolOutput { call_id: call_id.into(), output, result_chars: None })
    }

    /// Keeps what a call printed in memory, to be saved with the tool-result message that answers the call (see
    /// [`Session::append`]). The file keeps the text once: in that message, when the output is that text, and in the
    /// output line otherwise (the model was sent a note, a digest or a line added to it, which leaves the line the
    /// only whole copy). Saved right away, it would be written before it is known which of the two it is.
    pub fn hold_output(&mut self, call_id: &str, output: &ToolOutput) {
        let output = ToolOutput { images: Vec::new(), ..output.clone() };
        self.outputs.insert(call_id.to_string(), output);
        self.held.insert(call_id.to_string());
    }

    /// Writes the held outputs the tool results of `m` answer, each without its text when `m` holds it as it is.
    pub(super) fn write_held_for(&mut self, m: &Message) {
        if self.held.is_empty() {
            return;
        }
        for part in &m.content {
            let Part::ToolResult { call_id, content, .. } = part else { continue };
            if !self.held.remove(call_id) {
                continue;
            }
            let Some(out) = self.outputs.get(call_id) else { continue };
            let line = output_line(call_id, out, first_text(content) == Some(out.content.as_str()));
            // a failed write is told through `track`, as for any line; the message after it fails the same way
            let _ = self.write(&line);
        }
    }

    /// Writes every held output whole: the calls they belong to have no result in this history.
    pub(super) fn write_held_whole(&mut self) {
        for id in std::mem::take(&mut self.held) {
            if let Some(out) = self.outputs.get(&id) {
                let line = output_line(&id, out, false);
                let _ = self.write(&line);
            }
        }
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

    /// The project map the model was last shown changed: only that is written, not the prefix around it. Needs a
    /// prefix saved before (see [`Session::prefix`]); without one, nothing of it is kept.
    pub fn record_shown(&mut self, shown: &str) -> anyhow::Result<()> {
        if let Some(p) = &mut self.prefix {
            p.shown = shown.to_string();
        }
        self.write(&Line::PrefixShown { shown: shown.to_string() })
    }

    /// The line that says the person stopped the latest request, while no request has carried it yet.
    pub fn stop(&self) -> Option<&str> {
        self.stop.as_deref()
    }

    /// The person stopped the latest request: `line` says when and while what ran. It stays, on disk too, until a
    /// message that starts with it is appended (the next request carries it), so a resume or a compaction before
    /// that does not lose it.
    pub fn record_stop(&mut self, line: &str) -> anyhow::Result<()> {
        self.stop = Some(line.to_string());
        self.write(&Line::Stop { ts: now(), line: line.to_string() })
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
        // compaction puts its summary at the front and keeps the tail of the old history, so a message that
        // survives sits at a different index but at the same distance from the end: those keep the time
        // they were said, the summary and anything else new is stamped now
        let kept = messages.iter().rev().zip(self.messages.iter().rev()).take_while(|(new, old)| new == old).count();
        let (len, old_len) = (messages.len(), self.messages.len());
        let stamps = (0..len)
            .map(|i| {
                if len - i <= kept {
                    self.stamps.get(old_len - (len - i)).cloned().unwrap_or_else(now)
                } else {
                    now()
                }
            })
            .collect();
        let (old, outputs) = (std::mem::replace(&mut self.messages, messages.to_vec()), self.outputs.clone());
        let old_stamps = std::mem::replace(&mut self.stamps, stamps);
        let done = self.rewrite();
        self.track(&done);
        // as in `rewind`: when the file was not replaced, memory keeps matching it
        if done.as_ref().is_err_and(|e| !e.is::<NotReopened>()) {
            (self.messages, self.stamps, self.outputs) = (old, old_stamps, outputs);
            self.meta.messages = self.messages.len();
        }
        done
    }

    /// The text of a tool result, for the parts of a message.
    pub fn result_text(parts: &[Part]) -> String {
        nexo_core::message::result_text(parts)
    }
}

/// The line for one output. With `in_result` (and some text), the text is left out and its length said instead:
/// the call's tool-result message holds it.
pub(super) fn output_line(call_id: &str, output: &ToolOutput, in_result: bool) -> Line {
    if in_result && !output.content.is_empty() {
        let result_chars = Some(output.content.chars().count());
        let output = ToolOutput {
            content: String::new(),
            is_error: output.is_error,
            display: output.display.clone(),
            images: Vec::new(),
        };
        return Line::ToolOutput { call_id: call_id.into(), output, result_chars };
    }
    Line::ToolOutput { call_id: call_id.into(), output: output.clone(), result_chars: None }
}

/// The text of each tool result among `messages` (the first text part of a result, which is what the output
/// of the call became), by call id.
pub(super) fn result_texts(messages: &[Message]) -> std::collections::HashMap<&str, &str> {
    let mut texts = std::collections::HashMap::new();
    for part in messages.iter().flat_map(|m| &m.content) {
        if let Part::ToolResult { call_id, content, .. } = part {
            if let Some(text) = first_text(content) {
                texts.insert(call_id.as_str(), text);
            }
        }
    }
    texts
}

/// Whether the message that answers `call_id` (one of those `results` came from) holds `output`'s text as it
/// is. Only then may a file written with that message leave the text out of the output line: a result that a
/// compaction shortened, or one that is not in the file, leaves the line the only whole copy.
pub(super) fn held_by_result(
    results: &std::collections::HashMap<&str, &str>,
    call_id: &str,
    output: &ToolOutput,
) -> bool {
    !output.content.is_empty() && results.get(call_id).is_some_and(|t| *t == output.content)
}

/// The text of a tool result: its first text part, which is what the output of the call became.
pub(super) fn first_text(content: &[Part]) -> Option<&str> {
    content.iter().find_map(|p| match p {
        Part::Text { text } => Some(text.as_str()),
        _ => None,
    })
}
