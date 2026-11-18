use super::*;

impl App {
    pub(super) fn on_ready(
        &mut self,
        session_id: String,
        provider: String,
        model: String,
        cwd: PathBuf,
        permission_mode: PermissionMode,
    ) {
        self.session_id = Some(session_id);
        // nothing chosen yet: the agent runs on a placeholder that the person is never shown
        if !self.nothing_chosen() {
            self.provider = provider;
            self.model = model;
        }
        // a resume can land in another project: `@` must list that project's files, not the old one's
        if self.cwd != cwd {
            self.files = None;
        }
        self.cwd = cwd;
        self.permission_mode = permission_mode;
        self.read_branch();
        self.refresh_welcome();
    }

    pub(super) fn on_history(&mut self, index: usize, message: Message) {
        // the curator answers with a `Context` once the loaded history is counted
        self.context_wait = self.context.is_none();
        // a conversation being loaded: its words are indexed on the frames that follow, not in this burst
        self.words.defer(self.context_wait);
        let t = crate::trace::span("history");
        self.history(index, message);
        drop(t);
        if index % 2000 == 1999 || index == 372 {
            crate::trace::totals(&format!("history {}", index + 1));
        }
    }

    pub(super) fn on_history_cleared(&mut self) {
        self.prep = None;
        self.clear_transcript()
    }

    pub(super) fn on_session_changed(&mut self, session_id: String, title: String) {
        if self.session_id.as_deref() != Some(session_id.as_str()) {
            self.flash(&format!("session: {}", if title.is_empty() { session_id.clone() } else { title.clone() }));
        }
        self.session_id = Some(session_id);
        self.session_title = title;
    }

    pub(super) fn on_session_deleted(&mut self, id: String, title: String) {
        self.opts.sessions.retain(|r| r.id != id);
        // a list read before the delete may have put the row back into an open picker
        if let Some(Modal::Picker(p)) = &mut self.modal {
            if p.kind == PickKind::Session {
                p.remove(&id);
            }
        }
        let name = if title.is_empty() { id.clone() } else { title };
        self.flash(&format!("deleted {name} · /resume undelete brings it back"));
        self.trashed.push(id);
    }

    pub(super) fn on_session_restored(&mut self, id: String, title: String) {
        self.trashed.retain(|i| *i != id);
        let name = if title.is_empty() { id.clone() } else { title.clone() };
        if !self.opts.sessions.iter().any(|r| r.id == id) {
            // where it ran and how long it is come with the next list read from disk
            self.opts.sessions.insert(0, SessionRow { id, title, ..SessionRow::default() });
        }
        self.flash(&format!("restored {name}"));
    }

    /// The rows of the sessions list: a chip line after the id, see [`crate::palette::session_hint`].
    pub(in crate::app) fn session_items(&self) -> Vec<PickItem> {
        let now = nexo_core::mailbox::now_ms();
        self.opts
            .sessions
            .iter()
            .map(|r| PickItem { label: r.id.clone(), value: r.id.clone(), hint: crate::palette::session_hint(r, now) })
            .collect()
    }

    /// Opens the sessions picker, or says there is nothing to resume.
    pub(in crate::app) fn open_sessions(&mut self) {
        if self.opts.sessions.is_empty() {
            self.notice(Level::Info, "no earlier sessions");
        } else {
            let items = self.session_items();
            self.modal = Some(Modal::Picker(Picker::new(PickKind::Session, "Sessions", items, None)));
        }
    }

    /// The list read again from disk (`/resume` asked): it replaces what the interface knew, and an open sessions
    /// picker takes the new rows with its filter and its cursor where they were. A session deleted from here a
    /// moment ago may still be in a list read before the delete: it stays out.
    pub(super) fn on_session_list(&mut self, mut sessions: Vec<SessionRow>) {
        sessions.retain(|r| !self.trashed.contains(&r.id));
        self.opts.sessions = sessions;
        let asked = std::mem::take(&mut self.sessions_asked);
        let items = self.session_items();
        if let Some(Modal::Picker(p)) = &mut self.modal {
            if p.kind == PickKind::Session {
                p.replace_items(items);
            }
            return;
        }
        // the picker opens only where nothing else is on show: the read takes a while, and a window the person
        // opened since (a model switch, a permission prompt) keeps the screen
        if asked && self.modal.is_none() {
            self.open_sessions();
        }
    }

    pub(super) fn on_session_tree(&mut self, entries: Vec<(usize, String, String)>) {
        if entries.len() < 2 {
            self.notice(Level::Info, "no forks yet: /fork starts one");
        } else {
            let here = self.session_id.clone().unwrap_or_default();
            let n = entries.len();
            let items = entries
                .into_iter()
                .map(|(depth, id, title)| {
                    let mark = if id == here { " ◀ here" } else { "" };
                    let branch = if depth == 0 { String::new() } else { format!("{}└ ", "  ".repeat(depth - 1)) };
                    PickItem { label: format!("{branch}{id}{mark}"), value: id, hint: title }
                })
                .collect();
            self.modal =
                Some(Modal::Picker(Picker::new(PickKind::Tree, &format!("Sessions · {n} · Enter jump"), items, None)));
        }
    }

    pub(super) fn on_trash(&mut self, sessions: Vec<(String, String)>) {
        if sessions.is_empty() {
            self.notice(Level::Info, "the trash is empty");
        } else {
            let n = sessions.len();
            let items = sessions
                .into_iter()
                .map(|(id, title)| PickItem { label: id.clone(), value: id, hint: title })
                .collect();
            self.modal =
                Some(Modal::Picker(Picker::new(PickKind::Trash, &format!("Trash · {n} · Enter restore"), items, None)));
        }
    }

    pub(super) fn on_memory_shared(&mut self, pending: Vec<(String, String, usize)>) {
        if let Some((_, title, tok)) = pending.iter().find(|p| !self.shared.iter().any(|s| s.0 == p.0)) {
            self.flash(&format!(
                "⊕ {} · {} tok join your next message",
                if title.is_empty() { "memory" } else { title },
                crate::render::blocks::fmt_k(*tok as u64)
            ));
        }
        self.shared = pending;
    }

    pub(super) fn on_queued(&mut self, pending: usize) {
        self.queued = pending;
        self.trim_queue();
    }

    pub(super) fn on_idle(&mut self) {
        if self.prep.take().is_some() {
            self.busy = false;
        }
    }
}
