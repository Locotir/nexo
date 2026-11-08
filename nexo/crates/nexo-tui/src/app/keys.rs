//! Keyboard actions: history search, paste, undo, Ctrl+C, Esc and what the modals return.

use super::*;

impl App {
    /// `/prompt edit`: the prompt file opens in the editor (the run loop does the opening).
    /// Ctrl+R: the requests sent before, newest first, to search and take one back.
    pub fn open_history_search(&mut self) {
        let mut seen = std::collections::HashSet::new();
        let items: Vec<PickItem> = self
            .input
            .history()
            .iter()
            .rev()
            .filter(|h| seen.insert(h.as_str()))
            .map(|h| PickItem {
                label: h.lines().next().unwrap_or("").chars().take(120).collect(),
                value: h.clone(),
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
        self.keep_draft();
        self.send(AgentCommand::Shutdown);
        self.should_quit = true;
    }

    /// Puts an unsent draft in the history, so ↑ finds it after a clear or the next run.
    pub(super) fn keep_draft(&mut self) {
        let text = self.input.full_text();
        if !text.trim().is_empty() {
            self.input.push_history(&text);
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
            format!("{} {name}", frames[(self.spinner / 2) % frames.len()])
        } else {
            name
        }
    }

    /// Ctrl+C: interrupt a turn, else clear the draft; twice within two seconds: quit.
    pub fn ctrl_c(&mut self) {
        let now = self.now();
        if self.last_ctrl_c.is_some_and(|t| now.duration_since(t).as_secs_f32() < 2.0) {
            self.quit();
            return;
        }
        self.last_ctrl_c = Some(now);
        if self.busy {
            self.send(AgentCommand::Interrupt);
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
        let now = self.now();
        let again = self.last_esc.is_some_and(|t| now.duration_since(t).as_secs_f32() < 1.5);
        self.last_esc = Some(now);
        if self.busy && !again {
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
        } else if again && !self.busy && !self.user_messages.is_empty() {
            self.open_rewind_picker();
        } else {
            self.selected = None;
            if !self.busy && !self.user_messages.is_empty() {
                self.flash("Esc again to rewind to an earlier message");
            }
        }
        self.dirty = true;
    }

    pub fn modal_out(&mut self, out: ModalOut) {
        match out {
            ModalOut::Nothing => self.sync_theme_preview(),
            ModalOut::Close => {
                self.end_theme_preview(false);
                self.modal = None;
                self.next_permission();
            }
            ModalOut::Permission { id, decision } => {
                if let Some(i) = self.tool_index(&id) {
                    if matches!(decision, PermissionDecision::Deny { .. }) {
                        if let Block::Tool { status, .. } = &mut self.blocks[i] {
                            *status = ToolStatus::Denied;
                        }
                        self.invalidate(i);
                    }
                }
                self.send(AgentCommand::Permission { id, decision });
                self.modal = None;
                self.next_permission();
            }
            ModalOut::Trust(yes) => {
                self.modal = None;
                self.trust_answer = Some(yes);
            }
            ModalOut::Send(cmd) => self.send(cmd),
            ModalOut::Copy(text) => match crate::clipboard::copy(&text) {
                Ok(how) => self.flash_toast(&format!("copied ({how})")),
                Err(e) => self.notice(Level::Warn, format!("copy failed: {e}")),
            },
            ModalOut::SendAndClose(cmd) => {
                self.modal = None;
                if let AgentCommand::SaveModel { name, model, .. } = &cmd {
                    // the switcher offers it from now on
                    let pair = (name.clone(), model.clone());
                    if !self.opts.models.contains(&pair) {
                        self.opts.models.push(pair);
                    }
                    self.flash(&format!("{name}/{model}: saving and switching…"));
                }
                self.send(cmd);
                self.next_permission();
            }
            ModalOut::Pick { kind, value } => {
                self.modal = None;
                match kind {
                    PickKind::Model => {
                        let (provider, model) = self.model_target(value);
                        self.send(AgentCommand::SetModel { provider, model });
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
                    PickKind::Queue => {
                        if let Ok(i) = value.parse::<usize>() {
                            self.unqueue(i);
                        }
                    }
                    PickKind::History => {
                        self.input.set_text(&value);
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
        self.dirty = true;
    }
}
