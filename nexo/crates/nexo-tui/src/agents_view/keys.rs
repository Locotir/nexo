//! Keys, clicks and the wheel in the panel.

use super::*;

impl AgentsView {
    pub fn key(&mut self, key: KeyEvent) -> ModalOut {
        if let Some(t) = self.confirm.take() {
            self.answer_typing(t, matches!(key.code, KeyCode::Enter | KeyCode::Char('y')));
            return ModalOut::Nothing;
        }
        if self.edit.is_some() {
            return self.edit_key(key);
        }
        self.note = None;
        self.follow = true;
        if self.policy_page {
            return self.policy_key(key);
        }
        let n = self.shown().len();
        match key.code {
            KeyCode::Esc if self.open => self.open = false,
            KeyCode::Esc | KeyCode::Char('q') => return ModalOut::Close,
            KeyCode::Enter => self.open = !self.open && n > 0,
            KeyCode::Char('s') => self.cycle_filter(),
            KeyCode::Char('o') if self.panel.is_some() => {
                self.policy_page = true;
                self.policy_scroll = 0;
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.zoom = match self.zoom {
                    Zoom::Blocks => Zoom::List,
                    _ => Zoom::Cards,
                }
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.zoom = match self.zoom {
                    Zoom::Cards => Zoom::List,
                    _ => Zoom::Blocks,
                }
            }
            KeyCode::Char('n') => self.start_nudge(),
            KeyCode::Char('p') => self.toggle_pause(false),
            KeyCode::Char('P') => self.toggle_pause(true),
            KeyCode::Left | KeyCode::Char('h') => self.step(-1, false),
            KeyCode::Right | KeyCode::Char('l') => self.step(1, false),
            KeyCode::Tab => self.step(1, true),
            KeyCode::BackTab => self.step(-1, true),
            KeyCode::Up | KeyCode::Char('k') => self.step_rows(-1),
            KeyCode::Down | KeyCode::Char('j') => self.step_rows(1),
            KeyCode::Home | KeyCode::Char('g') => self.jump(0),
            KeyCode::End | KeyCode::Char('G') => self.jump(n.saturating_sub(1)),
            KeyCode::PageUp => self.step_rows(-(self.page_rows() as i32)),
            KeyCode::PageDown => self.step_rows(self.page_rows() as i32),
            _ => {}
        }
        ModalOut::Nothing
    }

    /// `s`: all, then nexo only, then external only, kept for the rest of the session.
    pub(super) fn cycle_filter(&mut self) {
        let keep = self.selected_row().map(|r| r.key);
        self.filter = self.filter.next();
        if let Some(m) = &self.memory {
            m.store(self.filter.to_u8(), Ordering::Relaxed);
        }
        // the same session stays selected when the new filter still lists it, else the first
        self.selected = 0;
        self.want_col = None;
        let still = keep.filter(|k| self.shown().iter().any(|r| &r.key == k));
        self.retarget(still);
        self.scroll = 0;
        self.open = false;
        self.note = Some(format!("showing {}", self.filter.label()));
    }

    /// One card back or on in reading order; `wrap` (Tab) goes round at the ends, the arrows stop there.
    pub(super) fn step(&mut self, delta: i32, wrap: bool) {
        let n = self.shown().len() as i32;
        if n == 0 {
            return;
        }
        let to = self.selected as i32 + delta;
        self.selected = if wrap { to.rem_euclid(n) } else { to.clamp(0, n - 1) } as usize;
        self.want_col = None;
    }

    /// To the card at `i` (Home, End).
    pub(super) fn jump(&mut self, i: usize) {
        self.selected = i;
        self.want_col = None;
    }

    /// How many rows of cards (or lines) the last draw showed: what a page moves by.
    pub(super) fn page_rows(&self) -> usize {
        let mut ys: Vec<u16> = self.cards.iter().map(|(_, r)| r.y).collect();
        ys.sort_unstable();
        ys.dedup();
        ys.len().max(1)
    }

    /// Moves the selection `delta` rows of cards (or lines) up or down, keeping its column even across a
    /// shorter row.
    pub(super) fn step_rows(&mut self, delta: i32) {
        let at = self
            .grid
            .iter()
            .enumerate()
            .find_map(|(r, row)| row.iter().position(|i| *i == self.selected).map(|c| (r, c)));
        let Some((r, c)) = at else { return };
        let c = *self.want_col.get_or_insert(c);
        let to = (r as i32 + delta).clamp(0, self.grid.len() as i32 - 1) as usize;
        let row = &self.grid[to];
        self.selected = row[c.min(row.len() - 1)];
    }

    pub(super) fn policy_key(&mut self, key: KeyEvent) -> ModalOut {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('o') => self.policy_page = false,
            KeyCode::Up | KeyCode::Char('k') => self.policy_scroll = self.policy_scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.policy_scroll = self.policy_scroll.saturating_add(1),
            KeyCode::PageUp => self.policy_scroll = self.policy_scroll.saturating_sub(10),
            KeyCode::PageDown => self.policy_scroll = self.policy_scroll.saturating_add(10),
            KeyCode::Home | KeyCode::Char('g') => self.policy_scroll = 0,
            // the page clamps the scroll to its last line when it draws
            KeyCode::End | KeyCode::Char('G') => self.policy_scroll = usize::MAX / 2,
            _ => {}
        }
        ModalOut::Nothing
    }

    /// The policy page: what is in force, the model's spend of this hour, and why anything was refused.
    pub(super) fn draw_policy(&mut self, f: &mut Frame, body: Rect, theme: &Theme) {
        let lines = self.panel.as_ref().map(|p| p.policy_lines()).unwrap_or_default();
        let max = lines.len().saturating_sub(body.height as usize);
        self.policy_scroll = self.policy_scroll.min(max);
        let shown: Vec<Line> = lines
            .iter()
            .skip(self.policy_scroll)
            .take(body.height as usize)
            .map(|l| {
                let text = truncate(l, body.width as usize);
                let style = if l.starts_with('!') {
                    theme.style(theme.warn)
                } else if l.starts_with('[') {
                    theme.bold(theme.accent)
                } else if l.starts_with("model") {
                    theme.style(theme.accent2)
                } else {
                    theme.dim()
                };
                Line::from(Span::styled(text, style))
            })
            .collect();
        f.render_widget(Paragraph::new(Text::from(shown)), body);
    }

    /// A key that acts on sessions, pressed with no supervisor running: say why nothing happened.
    pub(super) fn needs_supervisor(&mut self) {
        self.note = Some("messages and pauses need the supervisor: run `nexo supervise` in another terminal".into());
    }

    /// Where a message for `row` can be typed in: a session of another program with exactly one live match
    /// (same program, same folder) that has an inbox or a tmux pane. Nexo sessions go through the mailbox.
    pub(super) fn typing_address(&self, row: &AgentRow) -> Option<String> {
        if row.source == SourceKind::Nexo {
            return None;
        }
        let mut found = self.reach.find(&row.program, row.cwd.as_deref().unwrap_or(""));
        (found.len() == 1).then(|| found.remove(0).address)
    }

    /// The person answered the question before typing into another program: yes types it in, no keeps it.
    pub(super) fn answer_typing(&mut self, t: Typing, yes: bool) {
        if !yes {
            self.drafts.insert(t.key, t.text);
            self.note = Some("not sent; message kept: n picks it up again".into());
            return;
        }
        let sent = match nexo_core::reach::parse(&t.address) {
            Some(Ok(target)) => self.reach.send(&target, &t.text),
            _ => Err(format!("Not sent: {} is not an address nexo can type into", t.address)),
        };
        self.note = Some(match sent {
            Ok(_) => {
                self.fly_key = Some(t.key);
                format!("typed into {} and pressed Enter", t.name)
            }
            Err(e) => {
                self.drafts.insert(t.key, t.text);
                e
            }
        });
        self.refresh();
    }

    pub(super) fn start_nudge(&mut self) {
        let Some(row) = self.selected_row() else {
            return if self.panel.is_none() {
                self.needs_supervisor()
            } else {
                self.note = Some("no session selected".into())
            };
        };
        if row.parent.is_some() {
            self.note = Some("a sub-agent takes no message; write to its session".into());
            return;
        }
        // a session of another program that can be typed into needs no supervisor
        let template = match &self.panel {
            Some(panel) => panel.template(&row.key),
            None if self.typing_address(&row).is_some() => String::new(),
            None => return self.needs_supervisor(),
        };
        let text = self.drafts.remove(&row.key).unwrap_or(template);
        self.edit_for = Some((row.key.clone(), row.name.clone()));
        self.edit = Some(text);
    }

    pub(super) fn edit_key(&mut self, key: KeyEvent) -> ModalOut {
        let Some(text) = self.edit.as_mut() else { return ModalOut::Nothing };
        match key.code {
            // one level back: the half-written message is put aside, `n` on that session picks it up again
            KeyCode::Esc => {
                let text = self.edit.take().unwrap_or_default();
                if let Some((k, _)) = self.edit_for.take() {
                    if !text.trim().is_empty() {
                        self.drafts.insert(k, text);
                        self.note = Some("message kept: n picks it up again".into());
                    }
                }
            }
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => text.clear(),
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let kept = text.trim_end().rfind(char::is_whitespace).map_or(0, |i| i + 1);
                text.truncate(kept);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => text.push(c),
            KeyCode::Enter => {
                // it goes to the session it was started for, wherever the selection is now
                let for_key = self.edit_for.as_ref().map(|(k, _)| k.clone());
                let target = for_key.and_then(|k| self.rows.iter().find(|r| r.key == k).cloned());
                // that session went away meanwhile: the field stays open, nothing is sent
                let Some(row) = target else { return ModalOut::Nothing };
                let text = self.edit.take().unwrap_or_default();
                self.edit_for = None;
                if text.trim().is_empty() {
                    self.refresh();
                    return ModalOut::Nothing;
                }
                // another program gets it as typed input, and only after a yes
                if let Some(address) = self.typing_address(&row) {
                    self.note = Some(format!("type it into {} ({address}) as its input? Enter yes · Esc no", row.name));
                    self.confirm = Some(Typing { key: row.key, name: row.name, address, text });
                    return ModalOut::Nothing;
                }
                if let Some(panel) = self.panel.clone() {
                    panel.nudge(&row.key, &text);
                    self.fly_key = Some(row.key.clone());
                    self.note = Some(if row.source == SourceKind::Nexo {
                        format!("message sent to {}", row.name)
                    } else {
                        format!("nudge queued for {}; it goes out when the session stops", row.name)
                    });
                } else {
                    self.drafts.insert(row.key, text);
                    self.note =
                        Some(format!("{} cannot be typed into now; message kept: n picks it up again", row.name));
                }
                self.refresh();
            }
            _ => {}
        }
        ModalOut::Nothing
    }

    /// Text pasted while the message field is open: line breaks and tabs become one space each run, and it
    /// is dropped when no field is open.
    pub fn paste(&mut self, pasted: &str) {
        let Some(text) = self.edit.as_mut() else { return };
        let mut gap = false;
        for c in pasted.chars() {
            if c.is_control() {
                gap = true;
                continue;
            }
            if gap {
                text.push(' ');
                gap = false;
            }
            text.push(c);
        }
        if gap {
            text.push(' ');
        }
    }

    pub(super) fn toggle_pause(&mut self, all: bool) {
        let Some(panel) = self.panel.clone() else { return self.needs_supervisor() };
        if all {
            let on = !panel.paused_all();
            panel.pause(None, on);
            self.note = Some(if on { "all sessions paused" } else { "all sessions resumed" }.into());
        } else if let Some(row) = self.selected_row() {
            panel.pause(Some(&row.key), !row.paused);
            self.note = Some(format!("{} {}", row.name, if row.paused { "resumed" } else { "paused" }));
        } else {
            self.note = Some("no session to pause".into());
        }
        self.refresh();
    }

    /// A click on the close box closes. A click outside closes too, except while a message is being written
    /// (it would be lost). A click on a card selects it; a second click on the selected card opens its details.
    pub fn click(&mut self, x: u16, y: u16) -> ModalOut {
        if contains(self.close, x, y) {
            return ModalOut::Close;
        }
        if !contains(self.window, x, y) {
            return if self.edit.is_some() || self.confirm.is_some() { ModalOut::Nothing } else { ModalOut::Close };
        }
        if self.edit.is_some() || self.confirm.is_some() || self.policy_page {
            return ModalOut::Nothing;
        }
        if let Some(i) = self.cards.iter().find(|(_, r)| contains(*r, x, y)).map(|(i, _)| *i) {
            if self.selected == i {
                self.open = !self.open;
            } else {
                self.selected = i;
            }
            self.want_col = None;
            self.follow = true;
        }
        ModalOut::Nothing
    }

    pub fn on_hover(&mut self, x: u16, y: u16) -> bool {
        let on = contains(self.close, x, y);
        std::mem::replace(&mut self.hover, on) != on
    }

    /// The wheel: the policy page by lines, the sessions by a strip of cards (or lines), the selection staying
    /// where it is until it moves again.
    pub fn scroll_by(&mut self, delta: i32) {
        if self.policy_page {
            self.policy_scroll = (self.policy_scroll as i32 + delta).max(0) as usize;
            return;
        }
        let step = if matches!(self.drawn, Layout::List) { delta } else { delta.signum() };
        self.scroll = (self.scroll as i32 + step).max(0) as usize;
        self.follow = false;
    }
}

#[cfg(all(test, unix))]
#[path = "typing_tests.rs"]
mod typing_tests;
