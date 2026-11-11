//! Sending a request: pickers, the queue, MCP mentions and attachments.

use super::*;

impl App {
    /// Esc Esc or `/rewind`: pick an earlier message to go back to.
    pub fn open_rewind_picker(&mut self) {
        if self.busy {
            self.notice(Level::Info, "rewind when the turn has ended");
            return;
        }
        if self.user_messages.is_empty() {
            self.notice(Level::Info, "nothing to rewind to yet");
            return;
        }
        let items = self
            .user_messages
            .iter()
            .enumerate()
            .rev()
            // the hint is the request's number as the person counts them, not its place among all messages
            .map(|(n, (i, text))| PickItem {
                label: text.lines().next().unwrap_or("").to_string(),
                value: i.to_string(),
                hint: format!("#{}", n + 1),
            })
            .collect();
        self.modal = Some(Modal::Picker(Picker::new(PickKind::Rewind, "Rewind to before…", items, None)));
        self.dirty = true;
    }

    /// `/fork`: pick where the new session starts (an earlier message, or the whole conversation).
    pub fn open_fork_picker(&mut self) {
        if self.busy {
            self.notice(Level::Info, "fork when the turn has ended");
            return;
        }
        let mut items =
            vec![PickItem { label: "the whole conversation".into(), value: "all".into(), hint: String::new() }];
        items.extend(self.user_messages.iter().rev().map(|(i, text)| PickItem {
            label: text.lines().next().unwrap_or("").to_string(),
            value: i.to_string(),
            hint: format!("before #{i}"),
        }));
        self.modal = Some(Modal::Picker(Picker::new(PickKind::Fork, "Fork from…", items, None)));
        self.dirty = true;
    }

    pub(super) fn tool_index(&self, id: &str) -> Option<usize> {
        self.blocks.iter().rposition(|b| matches!(b, Block::Tool { id: i, .. } if i == id))
    }

    pub(super) fn close_reasoning(&mut self) {
        for i in (0..self.blocks.len()).rev() {
            if let Block::Reasoning { streaming, started, elapsed_ms, .. } = &mut self.blocks[i] {
                if *streaming {
                    *streaming = false;
                    *elapsed_ms = started.map(|s| s.elapsed().as_millis() as u64);
                    self.cache[i] = None;
                }
                break;
            }
        }
    }

    pub(super) fn close_streaming(&mut self) {
        self.close_reasoning();
        let now = self.now();
        for i in 0..self.blocks.len() {
            if let Block::Assistant { streaming, started, took_ms, text, .. } = &mut self.blocks[i] {
                if *streaming {
                    *streaming = false;
                    *took_ms = started.map(|s| now.saturating_duration_since(s).as_millis() as u64);
                    self.words.feed(text, crate::complete::Src::Other);
                    self.cache[i] = None;
                }
            }
        }
    }

    pub(super) fn next_permission(&mut self) {
        if self.modal.is_none() {
            if let Some(r) = self.pending_permissions.pop_front() {
                self.modal = Some(Modal::permission(r));
            } else if let Some(q) = self.pending_questions.pop_front() {
                self.modal = Some(Modal::question(q));
            }
        }
    }

    // ------------------------------------------------------------ the person's actions

    /// Whether a paste goes somewhere now: the draft, or the model setup's field.
    pub fn takes_paste(&self) -> bool {
        match &self.modal {
            None => !self.search.as_ref().is_some_and(|s| s.focused),
            Some(m) => matches!(m, Modal::Setup(_)),
        }
    }

    /// The grey ghost of the draft: what finishes the word before the cursor from the words of the
    /// session. A pure function of the draft and the index, so it cannot go stale. None while a block
    /// is picked (the arrows are the block's). The painter cuts it to the row; → takes it whole.
    pub fn complete_suffix(&self) -> Option<String> {
        if !self.opts.complete_words
            || self.palette.is_some()
            || self.modal.is_some()
            || self.selected.is_some()
            || self.words.is_empty()
        {
            return None;
        }
        let typed = self.input.word_before_cursor()?;
        self.words.suggest(&typed)
    }

    /// The suggested next message as it shows now: only in an empty draft, with no window, palette
    /// or picked block holding the keys.
    pub fn suggestion_shown(&self) -> Option<&str> {
        if self.palette.is_some() || self.modal.is_some() || self.selected.is_some() || !self.input.is_empty() {
            return None;
        }
        self.suggestion.as_deref()
    }

    /// Tab or → on an empty draft that shows a suggestion: the whole suggestion becomes the draft.
    /// False (and nothing changes) when none shows, so the key keeps its other use.
    pub fn accept_suggestion(&mut self) -> bool {
        let Some(text) = self.suggestion_shown().map(str::to_string) else { return false };
        self.suggestion = None;
        self.input.set_text(&text);
        self.refresh_palette();
        self.dirty = true;
        true
    }

    /// Enter on the draft.
    pub fn submit(&mut self) {
        let text = self.input.full_text();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            // Enter again with a request waiting: stop the turn and run the queued request now
            if self.busy && self.queued > 0 {
                self.send(AgentCommand::InterruptRun);
                self.flash("sending your queued request now");
                return;
            }
            if let Some(i) = self.selected {
                self.toggle_block(i);
            }
            return;
        }
        self.suggestion = None;
        self.palette = None;
        // a command of one's own sends its text as the request
        if let Some(rest) = trimmed.strip_prefix('/') {
            let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            let name = name.to_ascii_lowercase();
            // a skill: its folder and instructions are the request (read now, so edits count)
            if let Some(s) = self.skills.iter().find(|s| s.name.eq_ignore_ascii_case(&name)).cloned() {
                if self.custom_commands.iter().any(|c| c.name == name && c.path == s.file()) {
                    match s.instructions(args) {
                        Ok(text) => {
                            self.input.push_history(trimmed);
                            self.input.set_text(&text);
                            return self.submit();
                        }
                        Err(e) => {
                            self.notice(Level::Warn, format!("cannot read skill {}: {e}", s.name));
                            return;
                        }
                    }
                }
            }
            // a prompt of an MCP server: the agent asks the server and sends the result as the request
            if let Some((server, prompt)) = self.mcp_prompt_of(&name) {
                self.input.push_history(trimmed);
                self.input.clear();
                self.send(AgentCommand::McpPrompt { server, prompt, args: args.trim().to_string() });
                self.scroll_to_end();
                return;
            }
            if let Some(c) = self.custom_commands.iter().find(|c| c.name == name).cloned() {
                self.input.push_history(trimmed);
                self.input.set_text(&c.expand(args));
                return self.submit();
            }
        }
        if trimmed.starts_with('/') && !starts_with_path(trimmed) {
            if let Some(cmd) = commands::parse(trimmed) {
                if let Command::Unknown(_) = cmd {
                    // warn, but the draft stays to be fixed
                    self.fx.reject_at = self.fx_on().then(|| self.now());
                    self.run_command(cmd);
                    return;
                }
                self.input.push_history(trimmed);
                self.input.clear();
                self.run_command(cmd);
                return;
            }
        }
        if let Some(cmd) = trimmed.strip_prefix('!') {
            let cmd = cmd.trim();
            if !cmd.is_empty() {
                self.input.push_history(trimmed);
                self.input.clear();
                self.send(AgentCommand::Shell { command: cmd.to_string() });
            }
            return;
        }
        self.input.push_history(trimmed);
        self.input.clear();
        // no model yet: a request has nowhere to go, the setup opens
        if self.locked {
            return self.open_setup();
        }
        let attachments = self.attachments_in(trimmed);
        if self.busy {
            self.queued += 1;
            self.queued_msgs.push(trimmed.to_string());
            self.flash("queued: joins the turn at its next step · Enter again to stop the turn");
        }
        self.pending_stamps.push_back(trimmed.to_string());
        if self.pending_stamps.len() > 20 {
            self.pending_stamps.pop_front();
        }
        self.send(AgentCommand::Submit { text: trimmed.to_string(), attachments });
        self.scroll_to_end();
    }

    /// The texts kept for the queue never outnumber the requests the agent says wait: what started
    /// (the oldest) leaves.
    pub(super) fn trim_queue(&mut self) {
        let extra = self.queued_msgs.len().saturating_sub(self.queued);
        if extra > 0 {
            self.queued_msgs.drain(..extra);
        }
    }

    /// Take queued request `index` back out of the queue and into the draft, to edit it and send it
    /// again (the agent drops it from its own queue). Only into an empty draft.
    pub fn unqueue(&mut self, index: usize) {
        if index >= self.queued_msgs.len() {
            return;
        }
        if !self.input.is_empty() {
            self.flash("send or clear the draft first, then take the queued message back");
            return;
        }
        let text = self.queued_msgs.remove(index);
        self.queued = self.queued.saturating_sub(1);
        if let Some(i) = self.pending_stamps.iter().position(|t| *t == text) {
            self.pending_stamps.remove(i);
        }
        self.send(AgentCommand::Unqueue { index, text: text.clone() });
        self.input.set_text(&text);
        self.refresh_palette();
        self.dirty = true;
        self.flash("queued message back in the draft: edit it, Enter send again");
    }

    /// A list to show: one that is not finished, or one that just finished in this turn.
    pub fn todo_strip_on(&self) -> bool {
        let all = crate::todo_strip::done_count(&self.todo) == self.todo.len();
        !self.todo.is_empty() && (!all || self.todo_done_live)
    }

    /// The list as rows above the draft (`[ui] todo_strip = true`).
    pub fn todo_rows_on(&self) -> bool {
        self.opts.todo_strip && self.todo_strip_on()
    }

    /// `[ui] detail = "quiet"` (the default): the screen leaves out the key hints and the doubled figures.
    pub fn quiet(&self) -> bool {
        self.opts.detail == nexo_core::config::Detail::Quiet
    }

    /// The list as a count in the status bar (the default): one place only, never beside the rows.
    pub fn todo_chip_on(&self) -> bool {
        !self.opts.todo_strip && self.todo_strip_on()
    }

    /// `/todo`: the task list in its own window, grouped by state.
    pub fn open_todo(&mut self) {
        self.modal = Some(Modal::Todo(Box::new(crate::todo_view::TodoView::new(self.todo.clone()))));
    }

    /// A click on `◎ goal`: the goal's window.
    pub fn open_goal(&mut self) {
        match &self.goal {
            Some(g) => {
                self.modal =
                    Some(Modal::Goal(Box::new(crate::goal_view::GoalView::new(g.clone(), self.goal_log.clone()))));
            }
            None => self.notice(Level::Info, "no goal set: /goal <text> keeps the agent on it across turns"),
        }
    }

    /// The agent told the goal again: an open window follows it, and closes when the goal is cleared.
    pub(super) fn sync_goal_view(&mut self) {
        let Some(Modal::Goal(v)) = &mut self.modal else { return };
        match &self.goal {
            Some(g) => v.set(g.clone(), self.goal_log.clone()),
            None => self.modal = None,
        }
    }

    /// `/mcp`: the window with the MCP servers (asks the agent for a fresh listing).
    pub fn open_mcp(&mut self) {
        self.modal = Some(Modal::Mcp(Box::new(crate::mcp_view::McpView::new(self.mcp.clone()))));
        self.send(AgentCommand::ShowMcp);
    }

    /// The servers changed: the prompts become the commands `/mcp__server__prompt` (the old ones go).
    pub(super) fn set_mcp(&mut self, servers: Vec<nexo_core::agent::McpServerInfo>) {
        self.custom_commands.retain(|c| c.origin != "mcp");
        for s in &servers {
            for p in &s.prompts {
                let name = format!("mcp__{}__{}", s.name, p.name).to_ascii_lowercase();
                if crate::commands::lookup(&name).is_some() || self.custom_commands.iter().any(|c| c.name == name) {
                    continue;
                }
                let hint = p
                    .args
                    .iter()
                    .map(|a| if a.required { format!("<{}>", a.name) } else { format!("[{}]", a.name) })
                    .collect::<Vec<_>>()
                    .join(" ");
                let description = if p.description.is_empty() {
                    format!("prompt of {}", s.name)
                } else {
                    format!("{} ({})", p.description, s.name)
                };
                self.custom_commands.push(crate::custom::CustomCommand {
                    name,
                    description,
                    body: String::new(),
                    path: std::path::PathBuf::new(),
                    hint,
                    allowed_tools: String::new(),
                    origin: "mcp".into(),
                });
            }
        }
        self.mcp = servers;
    }

    /// `server:uri` for each resource and template the connected servers list.
    pub(super) fn mcp_mentions(&self) -> Vec<String> {
        let mut out = Vec::new();
        for s in self.mcp.iter().filter(|s| s.status == nexo_core::agent::McpStatus::Connected) {
            for u in s.resources.iter().chain(&s.templates) {
                out.push(format!("{}:{u}", s.name));
            }
        }
        out
    }

    /// The server and prompt a command name stands for (`mcp__server__prompt`).
    pub(super) fn mcp_prompt_of(&self, name: &str) -> Option<(String, String)> {
        for s in &self.mcp {
            for p in &s.prompts {
                if format!("mcp__{}__{}", s.name, p.name).eq_ignore_ascii_case(name) {
                    return Some((s.name.clone(), p.name.clone()));
                }
            }
        }
        None
    }

    /// `/jobs`, a click on `⚙ N jobs`: the window with every background job and its output.
    pub fn open_jobs(&mut self) {
        self.modal = Some(Modal::Jobs(Box::new(crate::jobs_view::JobsView::new(self.jobs.clone()))));
        self.send(AgentCommand::JobTails);
    }

    /// `/queue`: the requests waiting for the turn to end; Enter takes one back into the draft.
    pub fn open_queue(&mut self) {
        if self.queued_msgs.is_empty() {
            return self.notice(Level::Info, "nothing is queued");
        }
        let items = self
            .queued_msgs
            .iter()
            .enumerate()
            .map(|(i, t)| PickItem {
                label: t.lines().next().unwrap_or("").to_string(),
                value: i.to_string(),
                hint: String::new(),
            })
            .collect();
        self.modal = Some(Modal::Picker(Picker::new(PickKind::Queue, "Queued · Enter edit", items, None)));
    }

    /// `/shared-memory list`: the memory of other sessions that goes with the next message.
    pub fn open_shared(&mut self) {
        if self.shared.is_empty() {
            return self.notice(Level::Info, "no shared memory waits for the next message");
        }
        let items = self
            .shared
            .iter()
            .map(|(id, title, tok)| PickItem {
                label: if title.is_empty() { id.clone() } else { title.clone() },
                value: id.clone(),
                hint: format!("{id} · {} tok", crate::render::blocks::fmt_k(*tok as u64)),
            })
            .collect();
        self.modal =
            Some(Modal::Picker(Picker::new(PickKind::Shared, "Shared memory · joins your next message", items, None)));
    }

    /// A click on a piece of the keys row.
    pub fn open_hint(&mut self, hit: HintHit) {
        match hit {
            HintHit::Todo => self.open_todo(),
            HintHit::Jobs => self.run_command(Command::Jobs(None)),
            HintHit::Mem => self.open_shared(),
            HintHit::Queued => self.open_queue(),
            HintHit::Session => self.run_command(Command::Sessions),
        }
    }

    pub(super) fn attachments_in(&self, text: &str) -> Vec<Attachment> {
        let mut out = Vec::new();
        for tok in text.split_whitespace() {
            if let Some(p) = tok.strip_prefix('@') {
                let p = p.trim_end_matches([',', '.', ';', ':', ')', '"', '\'']);
                if p.is_empty() {
                    continue;
                }
                let path = self.cwd.join(p);
                if path.is_file() && !out.iter().any(|a: &Attachment| a.path == path) {
                    out.push(Attachment { path });
                }
            }
        }
        // an image dropped onto the terminal comes as its path: attached as `@path` would be
        for path in crate::dropped::image_paths(text, &nexo_core::paths::home_dir()) {
            if !out.iter().any(|a: &Attachment| a.path == path) {
                out.push(Attachment { path });
            }
        }
        out
    }

    /// The provider and model a `/model` argument names: `provider/model` as said, else a bare
    /// name stays on the provider in use unless only another provider lists it.
    pub(super) fn model_target(&self, name: String) -> (Option<String>, String) {
        if let Some((p, m)) = name.split_once('/') {
            return (Some(p.to_string()), m.to_string());
        }
        let has = |p: &str| self.opts.models.iter().any(|(q, m)| q == p && *m == name);
        if has(&self.provider) {
            return (None, name);
        }
        let mut owners = self.opts.models.iter().filter(|(_, m)| *m == name).map(|(p, _)| p);
        match (owners.next(), owners.next()) {
            (Some(p), None) => (Some(p.clone()), name),
            _ => (None, name),
        }
    }
}
