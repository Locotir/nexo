//! Keyboard actions: history search, paste, undo, Ctrl+C, Esc and what the modals return.

use super::*;

impl App {
    /// `/prompt edit`: the prompt file opens in the editor (the run loop does the opening).
    /// Ctrl+R: the requests sent before, newest first, to search and take one back.
    pub fn open_history_search(&mut self) {
        // newest first; a request and a shell command with the same text are two rows
        let mut seen = std::collections::HashSet::new();
        let items: Vec<PickItem> = (0..self.input.history().len())
            .rev()
            .filter_map(|i| self.input.history_entry(i).map(|(h, shell)| (i, h, shell)))
            .filter(|(_, h, shell)| seen.insert((*h, *shell)))
            .map(|(i, h, _)| PickItem {
                label: crate::render::wrap::clean(h.lines().next().unwrap_or("")).chars().take(120).collect(),
                // the history index: the entry comes back with its shell mark
                value: i.to_string(),
                hint: if h.lines().count() > 1 { format!("+{} lines", h.lines().count() - 1) } else { String::new() },
            })
            .collect();
        if items.is_empty() {
            self.flash_level(Level::Warn, "no requests sent yet");
            return;
        }
        self.modal = Some(Modal::Picker(Picker::new(PickKind::History, "Earlier requests", items, None)));
    }

    /// Ctrl+V: an image in the clipboard is kept as a PNG and attached (`@path` in the draft);
    /// text is pasted as text.
    pub fn paste_clipboard(&mut self) {
        match crate::clipboard::paste() {
            Ok(crate::clipboard::Pasted::Image(png)) => self.attach_png(&png),
            Ok(crate::clipboard::Pasted::Text(t)) => {
                self.input.insert_paste(&t);
                self.refresh_palette();
            }
            Err(e) => self.flash_level(Level::Error, &format!("nothing to paste: {e}")),
        }
    }

    /// Keeps `png` and puts `@path` in the draft, so it goes as an image with the next request.
    pub fn attach_png(&mut self, png: &[u8]) {
        match crate::clipboard::save_pasted(png) {
            Ok(path) => {
                let sep = if self.input.text().is_empty() || self.input.text().ends_with(' ') { "" } else { " " };
                self.input.insert_paste(&format!("{sep}@{} ", path.display()));
                self.flash(&format!("image pasted ({} KB): it goes with the next request", png.len() / 1024));
            }
            Err(e) => self.notice(Level::Warn, format!("the image could not be kept: {e}")),
        }
    }

    /// `/undo`: back to before the last request, its files put back, its text in the editor.
    pub fn undo(&mut self) {
        if self.busy {
            self.notice(Level::Warn, "/undo waits for the turn to end (Esc interrupts it)");
            return;
        }
        match self.user_messages.last().cloned() {
            Some((index, text)) => {
                self.send(AgentCommand::Rewind { index });
                self.keep_draft();
                self.input.set_text(&text);
                self.flash("undone: the request is back in the editor");
            }
            None => self.flash_level(Level::Warn, "nothing to undo"),
        }
    }

    pub fn prompt_edit(&mut self) {
        let path = nexo_core::paths::prompt_file();
        if !path.is_file() {
            if let Some(d) = path.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            // the person's preferences, added after the built-in prompt
            if let Err(e) = std::fs::write(&path, nexo_agent::prompt::PREFS_TEMPLATE) {
                self.notice(Level::Error, format!("cannot write {}: {e}", path.display()));
                return;
            }
        }
        self.edit_request = Some(path);
    }

    /// `/memory [user]`: the instructions file the agent reads, in $EDITOR. The project's is the nearest
    /// one nexo reads (a new `AGENTS.md` here when there is none); the person's own starts, when new,
    /// with what the other tools' files said, so that nothing nexo read is lost.
    pub fn memory_edit(&mut self, user: bool) {
        let path = if user {
            let config = nexo_core::paths::config_dir();
            let (path, seed) = nexo_core::compat::user_file(&config, nexo_core::compat::foreign_home().as_deref());
            if let Some(seed) = seed {
                if let Err(e) = std::fs::create_dir_all(&config).and_then(|()| std::fs::write(&path, seed)) {
                    self.notice(Level::Error, format!("cannot write {}: {e}", path.display()));
                    return;
                }
            }
            path
        } else {
            nexo_core::compat::project_file(&self.cwd)
        };
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        // a file that is not there yet opens as an empty one: say so before the editor takes the screen
        if !path.exists() {
            let shown =
                path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
            self.notice(Level::Info, format!("no {shown} yet: the editor opens a new one, saving it creates the file"));
        }
        self.edit_request = Some(path);
    }

    /// `/note <text>`: the text as one more list item at the end of the project's instructions
    /// file (the one `/memory` opens), which the agent reads from the next request on.
    pub fn remember(&mut self, text: Option<&str>) {
        let Some(text) = text.map(str::trim).filter(|t| !t.is_empty()) else {
            return self.notice(Level::Warn, "/memory add <text>: say what to remember");
        };
        let path = nexo_core::compat::project_file(&self.cwd);
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        let gap = if old.is_empty() || old.ends_with('\n') { "" } else { "\n" };
        let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
        match std::fs::write(&path, format!("{old}{gap}- {line}\n")) {
            Ok(()) => {
                let shown = path.strip_prefix(&self.cwd).unwrap_or(&path).display().to_string();
                self.flash_toast(&format!("added to {shown}"));
            }
            Err(e) => self.notice(Level::Error, format!("cannot write {}: {e}", path.display())),
        }
    }

    /// `/logout [provider]`: forgets the key or sign-in stored for the provider, the current one by default.
    pub fn logout(&mut self, name: Option<&str>) {
        let (level, text) = sign_out(&self.opts.providers, &self.provider, name, &nexo_core::auth::file());
        self.notice(level, text);
    }

    /// `/mouse`: capture on or off (off lets the terminal select text by itself).
    pub fn toggle_mouse(&mut self) {
        self.mouse_on = !self.mouse_on;
        self.mouse_request = Some(self.mouse_on);
        self.flash(if self.mouse_on {
            "mouse on: wheel scrolls, drag copies"
        } else {
            "mouse off: the terminal's own selection works; /settings mouse to enable"
        });
    }

    /// Copies the last reply, or with `code` the last code block of the replies.
    pub fn copy_last(&mut self, code: bool) {
        let text = self.blocks.iter().rev().find_map(|b| match b {
            Block::Assistant { text, .. } if code => last_code_block(text),
            Block::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        });
        match text {
            Some(t) => match crate::clipboard::copy(&t) {
                Ok(how) => self.flash_toast(&format!("copied ({how})")),
                Err(e) => self.notice(Level::Warn, format!("copy failed: {e}")),
            },
            None if code => self.flash("no code block to copy yet"),
            None => self.flash_level(Level::Warn, "nothing to copy yet"),
        }
    }

    pub fn quit(&mut self) {
        // every unsent draft goes to history: the open view's, then the main chat's it holds
        self.keep_draft();
        if self.sub_view.is_some() {
            self.input.clear();
            self.close_sub_to(false);
            self.keep_draft();
        }
        self.send(AgentCommand::Shutdown);
        self.should_quit = true;
    }

    /// Puts an unsent draft in the history, so ↑ finds it after a clear or the next run.
    pub(super) fn keep_draft(&mut self) {
        let text = self.input.full_text();
        if !text.trim().is_empty() {
            if self.input.typed_bang() {
                self.input.push_shell_history(&text);
            } else {
                self.input.push_history(&text);
            }
        }
    }

    /// The terminal tab's title: the session's name, with a spinner frame while a turn runs.
    pub fn terminal_title(&self) -> String {
        let name = match self.session_title.trim() {
            "" => "nexo".to_string(),
            t => format!("nexo · {t}"),
        };
        if self.busy {
            let frames = crate::render::blocks::SPINNER;
            format!("{} {name}", frames[self.spinner % frames.len()])
        } else {
            name
        }
    }

    /// Ctrl+D on an empty box: quit, but while a turn runs it needs a second press within two seconds,
    /// because quitting drops the turn and its tool and the tokens already spent on them. It keeps its own
    /// timer: sharing Ctrl+C's would make a Ctrl+C meant to interrupt the turn quit instead.
    pub fn ctrl_d(&mut self) {
        if !self.busy {
            self.quit();
            return;
        }
        let now = self.now();
        if self.last_ctrl_d.is_some_and(|t| now.duration_since(t).as_secs_f32() < 2.0) {
            self.quit();
            return;
        }
        self.last_ctrl_d = Some(now);
        self.flash("turn running, Ctrl+D again to quit");
    }

    /// Ctrl+C: interrupt a turn, else clear the draft; twice within two seconds: quit.
    pub fn ctrl_c(&mut self) {
        let now = self.now();
        if self.last_ctrl_c.is_some_and(|t| now.duration_since(t).as_secs_f32() < 2.0) {
            self.quit();
            return;
        }
        self.last_ctrl_c = Some(now);
        // in a run's view Ctrl+C acts on that run: it stops it, else steps back as Esc does
        if self.in_sub() {
            match self.sub_view.as_ref().filter(|_| self.sub_running()).and_then(|v| super::sub::job_of_run(&v.id)) {
                Some(id) => {
                    self.send(AgentCommand::KillJob { id: Some(id) });
                    self.flash("stopping this sub-agent (Ctrl+C again to quit)");
                }
                None => {
                    self.keep_draft();
                    self.input.clear();
                    self.close_sub();
                    self.flash("Ctrl+C again to quit");
                }
            }
            return;
        }
        if self.busy {
            self.send(AgentCommand::StopBy(nexo_core::agent::Stopper::CtrlC));
            self.flash("interrupting… (Ctrl+C again to quit)");
        } else if !self.input.is_empty() {
            self.keep_draft();
            self.input.clear();
            self.palette = None;
            self.dirty = true;
            self.flash("draft cleared, ↑ brings it back (Ctrl+C again to quit)");
        } else {
            self.flash("Ctrl+C again to quit");
        }
    }

    /// Esc: close what is open, interrupt a turn, or clear the draft on a second press.
    pub fn esc(&mut self) {
        if self.selection.take().is_some() {
            self.press = None;
            self.drag_pos = None;
            self.dirty = true;
            return;
        }
        if self.palette.take().is_some() {
            // a lone "/" was only the way into the menu: leave nothing behind
            if self.input.text().trim() == "/" {
                self.input.clear();
            }
            self.dirty = true;
            return;
        }
        if self.search.as_ref().is_some_and(|s| s.focused) {
            self.close_search();
            return;
        }
        // a selected block is let go before anything else (the turn keeps running)
        if self.selected.take().is_some() {
            self.dirty = true;
            return;
        }
        // in a sub-agent's conversation Esc only clears the box, then goes back: it interrupts nothing
        if self.in_sub() {
            return self.esc_sub();
        }
        let now = self.now();
        // the quick presses after an Esc that closed a window were meant for the way back: they stop there
        if self.stepped_back_at.is_some_and(|t| now.saturating_duration_since(t) < STEP_BACK_WAIT) {
            self.stepped_back_at = Some(now);
            return;
        }
        let again = self.last_esc.is_some_and(|t| now.duration_since(t).as_secs_f32() < 1.5);
        self.last_esc = Some(now);
        let after_interrupt = again && std::mem::take(&mut self.esc_interrupted);
        if self.busy && !again {
            self.esc_interrupted = true;
            self.send(AgentCommand::Interrupt);
            self.flash("interrupting… (Esc again clears the draft)");
            return;
        }
        if self.search.is_some() {
            self.close_search();
            return;
        }
        if !self.input.is_empty() {
            if again {
                self.keep_draft();
                self.input.clear();
                self.flash("draft cleared, ↑ brings it back");
            } else {
                self.flash("Esc again to clear the draft");
            }
        } else if again && !after_interrupt && !self.busy && !self.user_messages.is_empty() {
            self.open_rewind_picker();
        } else {
            self.selected = None;
            if !self.busy && !self.user_messages.is_empty() {
                self.flash("Esc again to rewind to an earlier message");
            }
        }
        self.dirty = true;
    }

    /// `/settings`: one row per setting with its value, `at` the row selected.
    pub(super) fn settings_picker(&self, at: Option<&str>) -> Modal {
        let on = |b: bool| if b { "on" } else { "off" };
        let row = |label: &str, hint: String| PickItem { label: label.into(), value: label.into(), hint };
        let items = vec![
            row("mouse", on(self.mouse_on).into()),
            row("timestamps", on(self.timestamps).into()),
            row("vim", on(self.input.vim.is_some()).into()),
            row("theme", self.theme.name.to_string()),
            row("permissions", self.permission_mode.as_str().to_string()),
        ];
        Modal::Picker(Picker::new(PickKind::Settings, "Settings · Enter switches", items, at))
    }

    /// The window on show closes; the window it was opened from comes back, else a queued permission.
    pub(super) fn step_back_window(&mut self) {
        self.pop_window();
        self.next_permission();
    }

    /// The window on show closes; the window it was opened from comes back.
    fn pop_window(&mut self) {
        self.modal = None;
        if matches!(self.back.last(), Some(Back::Window(_))) {
            if let Some(Back::Window(m)) = self.back.pop() {
                self.modal = Some(m);
            }
        }
    }

    /// /settings on show reads its values again (a switch, or a picker it opened, changed one).
    pub(super) fn refresh_settings(&mut self) {
        if let Some(Modal::Picker(p)) = &self.modal {
            if p.kind == PickKind::Settings {
                let at = p.current().map(|i| i.value.clone());
                self.modal = Some(self.settings_picker(at.as_deref()));
            }
        }
    }

    pub fn modal_out(&mut self, out: ModalOut) {
        match out {
            ModalOut::Nothing => self.sync_theme_preview(),
            ModalOut::ObserveAgent { key, nexo } => {
                // Supervisor keys for local jobs are <current session>.<job number>.
                // Resolve only a unique known run; names and foreign sessions are not identities.
                let id = self.session_id.as_deref().filter(|_| nexo).and_then(|session| {
                    let job = key.strip_prefix(session)?.strip_prefix('.')?.parse::<u32>().ok()?;
                    if key != format!("{session}.{job}") {
                        return None;
                    }
                    let mut runs = self.sub_runs.iter().filter(|r| super::sub::job_of_run(&r.info.id) == Some(job));
                    let run = runs.next()?.info.id.clone();
                    runs.next().is_none().then_some(run)
                });
                if nexo && self.session_id.as_deref() == Some(key.as_str()) {
                    // this session's own row is the way back to its chat, from a run's or another session's
                    // conversation too; the windows on the way back are not shown again
                    self.modal = None;
                    self.close_sub_to(false);
                    self.back.clear();
                    self.next_permission();
                } else if let Some(id) = id {
                    // the panel stays on the way back: Esc in the run's view returns to it
                    if matches!(self.modal, Some(Modal::Agents(_))) {
                        self.open_sub(&id);
                        self.next_permission();
                    }
                } else if nexo && self.session_id.as_deref() != Some(&key) && !key.contains('.') {
                    #[cfg(unix)]
                    if matches!(self.modal, Some(Modal::Agents(_))) {
                        self.open_remote(&key);
                    }
                    #[cfg(not(unix))]
                    if let Some(Modal::Agents(view)) = &mut self.modal {
                        view.observe_selected(&key);
                    }
                } else if let Some(Modal::Agents(view)) = &mut self.modal {
                    view.observe_selected(&key);
                }
            }
            ModalOut::OpenSub(id) => {
                // the window that asked (/jobs) is where Esc in the run's view comes back to
                self.open_sub(&id);
                self.modal = None;
                self.next_permission();
            }
            ModalOut::CloseAll => {
                // every window on the way back closes; a run's view stays, Esc in it goes to the chat
                self.end_theme_preview(false);
                self.modal = None;
                self.back.retain(|b| matches!(b, Back::Sub(_)));
                for b in &mut self.back {
                    *b = Back::Sub(None);
                }
                self.next_permission();
            }
            ModalOut::Close => {
                self.end_theme_preview(false);
                self.step_back_window();
            }
            ModalOut::Permission { id, decision } => {
                // the card is in the main chat, whichever view the person is in
                self.on_main(|a| {
                    if let Some(i) = a.tool_index(&id) {
                        if matches!(decision, PermissionDecision::Deny { .. }) {
                            if let Block::Tool { status, .. } = &mut a.blocks[i] {
                                *status = ToolStatus::Denied;
                            }
                            a.invalidate(i);
                        }
                    }
                });
                self.send(AgentCommand::Permission { id, decision });
                self.modal = None;
                self.next_permission();
            }
            ModalOut::Trust(yes) => {
                self.modal = None;
                self.trust_answer = Some(yes);
            }
            ModalOut::Send(cmd) => {
                // the setup saves and stays open on its last screen; the switcher offers the model once the
                // save is confirmed (`ModelChanged`), never before
                self.send(cmd)
            }
            ModalOut::Copy(text) => match crate::clipboard::copy(&text) {
                Ok(how) => self.flash_toast(&format!("copied ({how})")),
                Err(e) => self.notice(Level::Warn, format!("copy failed: {e}")),
            },
            ModalOut::SendAndClose(cmd) => {
                self.pop_window();
                if let AgentCommand::SaveModel { name, model, .. } = &cmd {
                    self.flash(&format!("{name}/{model}: saving and switching…"));
                }
                if let AgentCommand::SetModel { provider, model } = cmd {
                    self.set_model(provider, model);
                } else {
                    self.send(cmd);
                }
                self.next_permission();
            }
            ModalOut::PickMany { kind, values } => {
                self.pop_window();
                if kind == PickKind::Memory {
                    self.flash(&format!("reading {} sessions…", values.len()));
                    self.send(AgentCommand::ShareMemories { ids: values });
                }
            }
            ModalOut::Pick { kind, value } => {
                // the window it was opened from comes back first: a pick that opens another window covers it
                self.pop_window();
                match kind {
                    PickKind::Model => {
                        let (provider, model) = self.model_target(value);
                        self.set_model(provider, model);
                    }
                    PickKind::Session => {
                        self.send(AgentCommand::Resume { id: value.clone() });
                        self.flash(&format!("resuming {value}…"));
                    }
                    PickKind::Rewind => {
                        if let Ok(index) = value.parse::<usize>() {
                            let text = self
                                .user_messages
                                .iter()
                                .find(|(i, _)| *i == index)
                                .map(|(_, t)| t.clone())
                                .unwrap_or_default();
                            self.send(AgentCommand::Rewind { index });
                            self.keep_draft();
                            self.input.set_text(&text);
                            self.flash("rewound: edit the message and send it again");
                        }
                    }
                    PickKind::Fork => {
                        let index = value.parse::<usize>().ok();
                        self.send(AgentCommand::Fork { index });
                        self.flash("forking…");
                    }
                    PickKind::Tree => {
                        if self.session_id.as_deref() != Some(value.as_str()) {
                            self.send(AgentCommand::Resume { id: value.clone() });
                            self.flash(&format!("jumping to {value}…"));
                        }
                    }
                    PickKind::Trash => self.send(AgentCommand::RestoreSession { id: value.clone() }),
                    PickKind::Memory => {
                        self.send(AgentCommand::ShareMemory { id: Some(value.clone()), focus: None });
                        self.flash(&format!("reading {value}…"));
                    }
                    PickKind::Theme => {
                        self.end_theme_preview(true);
                        self.set_theme(&value, true);
                    }
                    PickKind::Shared => {}
                    PickKind::Settings => {
                        let cmd = match value.as_str() {
                            "mouse" => Command::Mouse,
                            "timestamps" => Command::Timestamps,
                            "vim" => Command::Vim,
                            "theme" => Command::Theme(None),
                            _ => Command::Permissions(None),
                        };
                        // Enter switches and stays; a picker it opens comes back here
                        let at = Some(value.as_str());
                        self.modal = None;
                        self.run_command(cmd);
                        let settings = self.settings_picker(at);
                        match self.modal.take() {
                            Some(m) => {
                                self.back.push(Back::Window(settings));
                                self.modal = Some(m);
                            }
                            None => self.modal = Some(settings),
                        }
                    }
                    PickKind::Queue => {
                        if let Ok(i) = value.parse::<usize>() {
                            self.unqueue(i);
                        }
                    }
                    PickKind::History => {
                        // read the entry first: keeping the draft can drop the oldest one and shift the indexes
                        // (the picker's value is the entry's index; any other value is the request itself, not a shell command)
                        let entry = match value.parse::<usize>() {
                            Ok(i) => self.input.history_entry(i).map(|(t, s)| (t.to_string(), s)),
                            Err(_) => Some((value, false)),
                        };
                        self.keep_draft();
                        if let Some((text, shell)) = entry {
                            self.input.recall(&text, shell);
                        }
                        self.refresh_palette();
                    }
                    PickKind::Effort => {
                        let effort = (value != "default").then_some(value);
                        self.send(AgentCommand::SetEffort { effort });
                    }
                    PickKind::PermissionMode => {
                        if let Some(m) = PermissionMode::parse(&value) {
                            self.send(AgentCommand::SetPermissionMode(m));
                        }
                    }
                }
                self.next_permission();
            }
        }
        self.refresh_settings();
        self.dirty = true;
    }
}
