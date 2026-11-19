use super::*;

impl Session {
    /// Keeps the first `keep` messages (and the outputs of the calls they
    /// make) and rewrites the file to hold only them: written to a temporary
    /// file next to it and renamed over it, so a crash leaves either the old
    /// file or the new one, never half of each. Usage and cost stay: they
    /// were spent. When the file cannot be rewritten, memory is left as it was, matching the file.
    pub fn rewind(&mut self, keep: usize) -> anyhow::Result<()> {
        let keep = keep.min(self.messages.len());
        let (messages, stamps, outputs, stop) =
            (self.messages.clone(), self.stamps.clone(), self.outputs.clone(), self.stop.clone());
        // a list set by a call that goes is not the one the kept messages leave
        let list_dropped = self.messages[keep..].iter().any(|m| m.tool_calls().any(|(_, name, _)| name == "todo"));
        self.stop = self.stop_at(keep);
        self.messages.truncate(keep);
        self.stamps.truncate(keep);
        let ids: std::collections::BTreeSet<String> =
            self.messages.iter().flat_map(|m| m.tool_calls().map(|(id, _, _)| id.to_string())).collect();
        self.outputs.retain(|id, _| ids.contains(id));
        let done = self.rewrite();
        self.track(&done);
        // a file replaced but not opened again holds the kept messages: memory stays as they are
        if done.as_ref().is_err_and(|e| !e.is::<NotReopened>()) {
            (self.messages, self.stamps, self.outputs, self.stop) = (messages, stamps, outputs, stop);
            self.meta.messages = self.messages.len();
        }
        if list_dropped && (done.is_ok() || done.as_ref().is_err_and(|e| e.is::<NotReopened>())) {
            // the saved list is the dropped messages'; the tool takes the kept ones', or none, as the session opens
            if let Some(d) = self.path.parent() {
                let _ = std::fs::remove_file(nexo_tools::todo::file_in(d, &self.meta.id));
            }
        }
        done
    }

    /// The stop still to be said after the first `keep` messages. Cut after all of them, the one in force. Cut
    /// before a request, the line that request carried: the person had stopped the one before it, and with the cut
    /// it is again the last; a stop recorded after it is of a request that goes.
    fn stop_at(&self, keep: usize) -> Option<String> {
        match self.messages.get(keep) {
            None => self.stop.clone(),
            Some(m) => nexo_core::agent::stop_line_of(&m.text()).map(str::to_string),
        }
    }

    /// Writes the whole file again from memory, except the usage and model lines, which are copied from the file
    /// as it stands: to a temporary file next to it, renamed over it.
    pub(super) fn rewrite(&mut self) -> anyhow::Result<()> {
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
            // a result this file holds as the output printed it carries the text for the output line too
            let results = result_texts(&self.messages);
            // the calls already paid keep their line (time, model, kind, held tokens), so what was spent
            // before a compaction or a rewind can still be told call by call
            let (copied, copied_cost) = copy_detail(&self.path, &mut f)?;
            let mut written = std::collections::BTreeSet::new();
            for (index, m) in self.messages.iter().enumerate() {
                writeln!(
                    f,
                    "{}",
                    serde_json::to_string(&Line::Message {
                        index,
                        ts: self.stamps.get(index).cloned().unwrap_or_else(now),
                        message: m.clone()
                    })?
                )?;
                for (id, _, _) in m.tool_calls() {
                    if let Some(out) = self.outputs.get(id) {
                        written.insert(id.to_string());
                        writeln!(
                            f,
                            "{}",
                            serde_json::to_string(&output_line(id, out, held_by_result(&results, id, out)))?
                        )?;
                    }
                }
            }
            // outputs of calls whose messages were replaced (compaction) stay, as they did in the log
            for (id, out) in self.outputs.iter().filter(|(id, _)| !written.contains(*id)) {
                writeln!(f, "{}", serde_json::to_string(&output_line(id, out, held_by_result(&results, id, out)))?)?;
            }
            // Only what has no line of its own is summed: a usage write that failed (memory counts it
            // first) or a file that could not be read. The copied lines carry the rest.
            let rest = Usage {
                input: self.usage.input.saturating_sub(copied.input),
                output: self.usage.output.saturating_sub(copied.output),
                cache_read: self.usage.cache_read.saturating_sub(copied.cache_read),
                cache_write: self.usage.cache_write.saturating_sub(copied.cache_write),
                cache_write_1h: self.usage.cache_write_1h.saturating_sub(copied.cache_write_1h),
                transformations: self.usage.transformations.saturating_sub(copied.transformations),
                reasoning: self.usage.reasoning.saturating_sub(copied.reasoning),
            };
            // the same lines added in another order differ in the last digits: that is not a cost
            let rest_cost = (self.cost - copied_cost).max(0.0);
            if !rest.is_empty() || rest_cost > 1e-9 {
                writeln!(
                    f,
                    "{}",
                    serde_json::to_string(&Line::Usage {
                        usage: rest,
                        cost_usd: rest_cost,
                        ts: Some(now()),
                        model: None,
                        kind: Some(UsageKind::Sum),
                        held: None,
                        fallback: None,
                    })?
                )?;
            }
            if self.goal.is_some() {
                // the words and the progress too: a compaction or a rewind must not lose them
                writeln!(f, "{}", serde_json::to_string(&self.goal_line())?)?;
            }
            if let Some(c) = &self.context {
                writeln!(f, "{}", serde_json::to_string(&Line::Context(c.clone()))?)?;
            }
            if let Some(p) = &self.prefix {
                writeln!(f, "{}", serde_json::to_string(&Line::Prefix(p.clone()))?)?;
            }
            if let Some(line) = &self.stop {
                writeln!(f, "{}", serde_json::to_string(&Line::Stop { ts: now(), line: line.clone() })?)?;
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
                // every output is in the file now, in the form its result allows
                self.held.clear();
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
        for (i, m) in self.messages[..keep].iter().enumerate() {
            f.append_at(m, self.stamps.get(i).cloned().unwrap_or_else(now))?;
            for (id, _, _) in m.tool_calls() {
                if let Some(out) = self.outputs.get(id) {
                    f.hold_output(id, out); // written with the result that answers it, below
                }
            }
        }
        f.write_held_whole(); // a call kept without its result
        if f.meta.title.is_empty() && !self.meta.title.is_empty() {
            f.set_title(&format!("{} (fork)", self.meta.title))?;
        }
        if self.goal.is_some() {
            // as it stands, on disk too, so a resumed fork keeps it: the person's words and the progress are the
            // original's, not the restatement's text and the time of the fork
            f.record_goal_all(
                self.goal.clone(),
                self.goal_state.clone(),
                self.goal_original.clone(),
                self.goal_progress.clone(),
            )?;
        }
        if let Some(p) = &self.prefix {
            f.record_prefix(p)?; // the fork goes on with the same prefix, so the same cache
        }
        if let Some(line) = self.stop_at(keep) {
            f.record_stop(&line)?; // its next request says the person stopped the one before it
        }
        // the task list goes with a fork of the whole conversation; a shorter one may not match the list that was
        // saved, so it starts without a file and the tool reads its list from the messages kept, as it opens it
        if keep == self.messages.len() {
            copy_todo(&dir, &self.meta.id, &f.meta.id);
        }
        Ok(f)
    }
}

/// Copies the task list saved with session `from` to session `to` (best effort: the list is only a shortcut to
/// the work in progress, and the fork works without it). It goes through a temporary file, so `to` never holds
/// a cut copy.
fn copy_todo(dir: &Path, from: &str, to: &str) {
    let (a, b) = (nexo_tools::todo::file_in(dir, from), nexo_tools::todo::file_in(dir, to));
    if !a.is_file() {
        return;
    }
    let tmp = b.with_extension(format!("json.{}.tmp", std::process::id()));
    if std::fs::copy(&a, &tmp).and_then(|_| std::fs::rename(&tmp, &b)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Copies the `usage` and `model` lines of the file as it stands to `out`, trimmed of surrounding whitespace and
/// in order, and returns what the usage lines add up to. A line is told by how it starts (the writer puts `type`
/// first, as `read_meta` relies on) and kept only when it still parses, so a line cut by a crash is not carried
/// along. Lines of an earlier `sum` are copied like the others, so a second rewrite folds nothing. A file that
/// cannot be read copies nothing: the caller then sums from memory.
fn copy_detail(path: &Path, out: &mut impl Write) -> anyhow::Result<(Usage, f64)> {
    const USAGE: &[u8] = br#"{"type":"usage""#;
    const MODEL: &[u8] = br#"{"type":"model""#;
    let (mut total, mut cost) = (Usage::default(), 0.0);
    let Ok(file) = File::open(path) else { return Ok((total, cost)) };
    for line in BufReader::new(file).split(b'\n').map_while(Result::ok) {
        let line = line.trim_ascii();
        if !line.starts_with(USAGE) && !line.starts_with(MODEL) {
            continue;
        }
        match serde_json::from_slice::<Line>(line) {
            Ok(Line::Usage { usage, cost_usd, .. }) => {
                total += usage;
                cost += cost_usd;
            }
            Ok(Line::Model { .. }) => {}
            _ => continue,
        }
        out.write_all(line)?;
        out.write_all(b"\n")?;
    }
    Ok((total, cost))
}
