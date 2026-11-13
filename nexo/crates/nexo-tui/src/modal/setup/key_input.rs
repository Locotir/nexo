//! Keys and pastes in the setup, step by step.

use super::*;

impl Setup {
    pub fn key(&mut self, key: KeyEvent) -> SetupOut {
        if key.code == KeyCode::Esc {
            return self.back();
        }
        // while the browser is waited for, `c` copies the whole link
        if self.step == Step::Login && key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::NONE {
            if let Some((url, _)) = &self.prompt {
                return SetupOut::Copy(url.clone());
            }
        }
        if self.busy.is_some() {
            return SetupOut::Nothing;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match self.step {
            Step::Start => self.start_key(key),
            Step::Trouble => self.trouble_key(key.code),
            Step::Done => match key.code {
                KeyCode::Enter => SetupOut::Close,
                _ => SetupOut::Nothing,
            },
            Step::Provider | Step::Model | Step::Login => match key.code {
                KeyCode::Enter | KeyCode::Tab => self.pick(),
                KeyCode::Up => {
                    self.move_list(-1);
                    SetupOut::Nothing
                }
                KeyCode::Down => {
                    self.move_list(1);
                    SetupOut::Nothing
                }
                KeyCode::PageUp => {
                    self.move_list_clamped(-8);
                    SetupOut::Nothing
                }
                KeyCode::PageDown => {
                    self.move_list_clamped(8);
                    SetupOut::Nothing
                }
                KeyCode::Home => {
                    self.list.selected = 0;
                    SetupOut::Nothing
                }
                KeyCode::End => {
                    self.list.selected = self.list.ranked.len().saturating_sub(1);
                    SetupOut::Nothing
                }
                // the folds of the first list
                KeyCode::Right if self.step == Step::Provider => {
                    self.fold(true);
                    SetupOut::Nothing
                }
                KeyCode::Left if self.step == Step::Provider => {
                    self.fold(false);
                    SetupOut::Nothing
                }
                KeyCode::Char(' ')
                    if self.step == Step::Provider
                        && self.list.query.is_empty()
                        && self.list.current().is_some_and(|i| i.value.starts_with("group:")) =>
                {
                    self.toggle_current();
                    SetupOut::Nothing
                }
                KeyCode::Backspace => {
                    self.list.query.pop();
                    self.refilter();
                    SetupOut::Nothing
                }
                KeyCode::Char('u') if ctrl => {
                    self.list.query.clear();
                    self.refilter();
                    SetupOut::Nothing
                }
                KeyCode::Char(c) if !ctrl => {
                    self.list.query.push(c);
                    self.refilter();
                    SetupOut::Nothing
                }
                _ => SetupOut::Nothing,
            },
            Step::Address | Step::Key => match key.code {
                KeyCode::Enter => self.submit_field(),
                // the page where the key is made
                KeyCode::Char('o') if ctrl && self.step == Step::Key => {
                    if let Some(url) = keys::page(&self.name) {
                        crate::links::open(url);
                    }
                    SetupOut::Nothing
                }
                KeyCode::Backspace => {
                    self.field.pop();
                    SetupOut::Nothing
                }
                KeyCode::Char('u') if ctrl => {
                    self.field.clear();
                    SetupOut::Nothing
                }
                KeyCode::Char(c) if !ctrl => {
                    self.field.push(c);
                    SetupOut::Nothing
                }
                _ => SetupOut::Nothing,
            },
            Step::Review => self.review_key(key),
        }
    }

    /// Typed text pasted in one go (a key, an address, a search).
    pub fn paste(&mut self, text: &str) -> SetupOut {
        // escapes and control characters out; a key wrapped over lines is joined back; spaces and quotes trimmed
        let text = crate::render::wrap::clean(&text.replace('\r', "\n")).replace('\n', "");
        let text = keys::clean(&text);
        match self.step {
            // a key pasted before any provider was picked: the service is read from its start
            Step::Start | Step::Provider if keys::looks_like_key(&text) && self.busy.is_none() => {
                return self.key_pasted(text)
            }
            Step::Start => {
                self.error = None;
                self.field.push_str(&text);
            }
            Step::Address | Step::Key => self.field.push_str(&text),
            Step::Provider | Step::Model | Step::Login => {
                self.list.query.push_str(&text);
                self.refilter();
            }
            Step::Review => {
                if let Some(f) = self.fields.get_mut(self.selected) {
                    f.value.push_str(&text);
                }
            }
            Step::Trouble | Step::Done => {}
        }
        SetupOut::Nothing
    }

    pub(super) fn move_list(&mut self, d: i32) {
        let n = self.list.ranked.len() as i32;
        if n > 0 {
            self.list.selected = (self.list.selected as i32 + d).rem_euclid(n) as usize;
        }
    }

    /// A page up or down: stops at the ends instead of wrapping round.
    pub(super) fn move_list_clamped(&mut self, d: i32) {
        let last = self.list.ranked.len().saturating_sub(1) as i32;
        self.list.selected = (self.list.selected as i32 + d).clamp(0, last) as usize;
    }

    /// Opens a closed fold under the cursor, closes an open one.
    pub(super) fn toggle_current(&mut self) {
        let Some(g) = self.owner.get(self.list.selected).copied().flatten() else { return };
        let key = self.groups[g].key.clone();
        self.fold(!self.open.contains(&key));
    }

    pub(super) fn pick(&mut self) -> SetupOut {
        match self.step {
            Step::Provider => {
                // a key typed key by key (no bracketed paste) is a key, taken as a paste is
                if keys::starts_like_key(&self.list.query, 1) && keys::looks_like_key(&self.list.query) {
                    let typed = std::mem::take(&mut self.list.query);
                    self.refilter();
                    return self.key_pasted(keys::clean(&typed));
                }
                let Some(it) = self.list.current().cloned() else { return SetupOut::Nothing };
                // a header opens or closes (while filtering every fold with a match is open)
                if it.value.starts_with("group:") {
                    self.toggle_current();
                    return SetupOut::Nothing;
                }
                if let Some(pm) = it.value.strip_prefix("use:") {
                    // a model already set up needs no key: a pasted text held is let go, and the person
                    // is told before anything is sent (the wizard stays; Enter again switches)
                    if self.pending_key.take().is_some() {
                        self.error = Some(HELD_DROPPED.into());
                        return SetupOut::Nothing;
                    }
                    let (provider, model) = match pm.split_once('/') {
                        Some((p, m)) => (Some(p.to_string()), m.to_string()),
                        None => (None, pm.to_string()),
                    };
                    return SetupOut::SendAndClose(AgentCommand::SetModel { provider, model });
                }
                self.choose(&it.value)
            }
            Step::Start => {
                let Some(it) = self.list.current().cloned() else { return SetupOut::Nothing };
                if it.value == "more" {
                    self.came_from = Some(it.value.clone());
                    self.show_providers();
                    return SetupOut::Nothing;
                }
                self.choose(&it.value)
            }
            Step::Login => {
                let Some(it) = self.list.current().cloned() else { return SetupOut::Nothing };
                self.error = None;
                if it.value == "keep" {
                    self.step = Step::Address;
                    self.field = self.cfg.base_url.clone();
                    return self.probe_cmd();
                }
                self.busy = Some("waiting for the browser".to_string());
                if it.value == "codex-cli" {
                    self.busy = Some("reading the Codex CLI login".into());
                }
                SetupOut::Send(AgentCommand::Login { name: self.name.clone(), flow: it.value })
            }
            Step::Model => {
                let typed = self.list.query.trim().to_string();
                // a key typed here is not a model name: it is not sent as one
                if looks_like_secret(&typed) {
                    self.list.query.clear();
                    self.refilter();
                    self.error = Some("That looks like a key, not a model name. It was not kept.".into());
                    return SetupOut::Nothing;
                }
                let model = match self.list.current() {
                    Some(it) => it.value.clone(),
                    None if !typed.is_empty() => typed,
                    None => return SetupOut::Nothing,
                };
                self.model = model.clone();
                self.error = None;
                let Some(probe) = self.probe.clone() else { return SetupOut::Nothing };
                self.busy = Some(format!("testing {model}"));
                self.seq = next_seq();
                SetupOut::Send(AgentCommand::CheckModel {
                    name: self.name.clone(),
                    probe,
                    cfg: self.cfg.clone(),
                    model,
                    key: self.key.clone(),
                    seq: self.seq,
                })
            }
            _ => SetupOut::Nothing,
        }
    }

    pub(super) fn submit_field(&mut self) -> SetupOut {
        let text = keys::clean(&self.field);
        match self.step {
            Step::Address => {
                if text.is_empty() {
                    self.error = Some("type its address, e.g. localhost:11434".into());
                    return SetupOut::Nothing;
                }
                self.cfg.base_url = normalize_url(&text);
                self.probe_cmd()
            }
            Step::Key => {
                if let Some(var) = text.strip_prefix('$') {
                    if !std::env::var(var).is_ok_and(|k| !k.trim().is_empty()) {
                        self.error = Some(format!("${var} is not set in this shell"));
                        return SetupOut::Nothing;
                    }
                    self.cfg.api_key_env = Some(var.to_string());
                    self.cfg.api_key_cmd = None;
                    self.key = None;
                } else if let Some(cmd) = text.strip_prefix('!') {
                    self.cfg.api_key_cmd = Some(cmd.trim().to_string());
                    self.key = None;
                } else if !text.is_empty() {
                    self.key = Some(text);
                } else if self.cfg.resolve_key().is_none() {
                    self.error = Some("paste the key first".into());
                    return SetupOut::Nothing;
                }
                self.field.clear();
                self.probe_cmd()
            }
            _ => SetupOut::Nothing,
        }
    }

    pub(super) fn review_key(&mut self, key: KeyEvent) -> SetupOut {
        let effort_row = self.fields.len();
        let rows = self.fields.len() + usize::from(!self.efforts.is_empty());
        match key.code {
            KeyCode::Enter => {
                let (name, cfg) = self.result();
                // the setup stays open on the last screen until the save is confirmed
                self.expect_done(&name);
                SetupOut::Send(AgentCommand::SaveModel { name, cfg, model: self.model.clone(), key: self.key.clone() })
            }
            // the facts the test found, to change before saving
            KeyCode::Tab if !self.details => {
                self.details = true;
                SetupOut::Nothing
            }
            _ if !self.details => SetupOut::Nothing,
            KeyCode::Up | KeyCode::BackTab => {
                self.selected = (self.selected + rows - 1) % rows.max(1);
                SetupOut::Nothing
            }
            KeyCode::Down | KeyCode::Tab => {
                self.selected = (self.selected + 1) % rows.max(1);
                SetupOut::Nothing
            }
            KeyCode::Left | KeyCode::Right if self.selected == effort_row => {
                // the default, then each level
                let n = self.efforts.len() + 1;
                let at = self.effort.map(|i| i + 1).unwrap_or(0);
                let next = if key.code == KeyCode::Right { (at + 1) % n } else { (at + n - 1) % n };
                self.effort = next.checked_sub(1);
                SetupOut::Nothing
            }
            KeyCode::Backspace => {
                if let Some(f) = self.fields.get_mut(self.selected) {
                    f.value.pop();
                }
                SetupOut::Nothing
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(f) = self.fields.get_mut(self.selected) {
                    f.value.clear();
                }
                SetupOut::Nothing
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(f) = self.fields.get_mut(self.selected) {
                    f.value.push(c);
                }
                SetupOut::Nothing
            }
            _ => SetupOut::Nothing,
        }
    }
}
