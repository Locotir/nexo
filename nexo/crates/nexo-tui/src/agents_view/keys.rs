//! Keys, clicks and the wheel in the panel. The keys depend on the kind of the selected row, and no key means
//! two things on one row: lanes start, stop, hold and pause; sub-agents take a message or stop; sessions take
//! mail and are observed. The arrows move in the tree, or scroll the pane when the keys are there.

use nexo_core::agent::AgentCommand;

use super::tree::Item;
use super::*;

/// Where a message typed in the panel goes when the supervisor does not carry it.
pub(super) enum Route {
    /// The run of a sub-agent of this session.
    Run(String),
    /// Another nexo session, by its id, through the mailbox.
    Mail(String),
}

impl Route {
    pub(super) fn command(self, text: String) -> AgentCommand {
        let text = text.trim().to_string();
        match self {
            Route::Run(id) => AgentCommand::SubSubmit { id, text },
            Route::Mail(to) => AgentCommand::SendMail { to, text },
        }
    }
}

impl AgentsView {
    /// Whether text is being typed here (a message, a form, a filter): Ctrl+C then acts as Esc.
    pub fn typing(&self) -> bool {
        self.edit.is_some() || self.form.is_some() || self.finding
    }

    pub fn key(&mut self, key: KeyEvent) -> ModalOut {
        if let Some(t) = self.confirm.take() {
            self.answer_typing(t, matches!(key.code, KeyCode::Enter | KeyCode::Char('y')));
            return ModalOut::Nothing;
        }
        // the same keys as the typing question above: Enter and y confirm, Esc and n cancel, the rest wait
        if let Some(id) = self.kill_ask {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.kill_ask = None;
                    self.note = Some(format!("stopping job {id}"));
                    return ModalOut::Send(AgentCommand::KillJob { id: Some(id) });
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.kill_ask = None;
                    self.note = Some(format!("job {id} keeps running"));
                }
                _ => {}
            }
            return ModalOut::Nothing;
        }
        if self.edit.is_some() {
            return self.edit_key(key);
        }
        if self.form.is_some() {
            self.form_key(key);
            return ModalOut::Nothing;
        }
        if self.finding {
            self.find_key(key);
            return ModalOut::Nothing;
        }
        self.note = None;
        self.follow = true;
        if self.policy_page {
            return self.policy_key(key);
        }
        if self.observing.is_some() {
            return self.observe_key(key);
        }
        let kind = self.selected_kind();
        let lane = kind == Some(Kind::Lane);
        let on_lanes = lane || matches!(self.selected_item(), Some(Item::Head { group: Group::Lanes, .. }));
        let n = self.items().len();
        match key.code {
            KeyCode::Esc if self.focus || self.open => {
                self.focus = false;
                self.open = false;
            }
            // a kept text filter goes first; the next Esc steps back
            KeyCode::Esc if self.find.is_some() => {
                let keep = self.selected_item().map(|it| (it.id(), it.row().map(|r| r.key.clone())));
                self.find = None;
                self.retarget(keep);
            }
            KeyCode::Esc | KeyCode::Char('q') => return ModalOut::Close,
            KeyCode::Enter => return self.enter(),
            KeyCode::Tab => {
                if self.wide() || self.open {
                    self.focus = !self.focus;
                } else if self.selected_row().is_some() {
                    self.open = true;
                    self.focus = true;
                }
            }
            KeyCode::BackTab => self.focus = false,
            KeyCode::Up if self.focus => self.pane_scroll = self.pane_scroll.saturating_sub(1),
            KeyCode::Down if self.focus => self.pane_scroll = (self.pane_scroll + 1).min(self.pane_max),
            KeyCode::PageUp if self.focus => self.pane_scroll = self.pane_scroll.saturating_sub(self.pane_page()),
            KeyCode::PageDown if self.focus => {
                self.pane_scroll = (self.pane_scroll + self.pane_page()).min(self.pane_max)
            }
            KeyCode::Up => self.move_to(self.selected as i64 - 1),
            KeyCode::Down => self.move_to(self.selected as i64 + 1),
            KeyCode::PageUp => self.move_to(self.selected as i64 - self.tree_area.height.max(1) as i64),
            KeyCode::PageDown => self.move_to(self.selected as i64 + self.tree_area.height.max(1) as i64),
            KeyCode::Home => self.move_to(0),
            KeyCode::End => self.move_to(n as i64 - 1),
            KeyCode::Left => self.fold(Some(true)),
            KeyCode::Right => self.fold(Some(false)),
            KeyCode::Char(' ') => self.fold(None),
            KeyCode::Char('/') => {
                self.finding = true;
                self.find.get_or_insert_with(String::new);
            }
            KeyCode::Char('f') => self.cycle_show(),
            KeyCode::Char('F') => self.cycle_filter(),
            KeyCode::Char('P') if self.panel.is_some() => {
                self.policy_page = true;
                self.policy_scroll = 0;
            }
            KeyCode::Char('z') if !matches!(kind, Some(Kind::Sub | Kind::Job)) => self.toggle_pause(false),
            KeyCode::Char('Z') => self.toggle_pause(true),
            KeyCode::Char('s') if lane => self.lane_start(false),
            KeyCode::Char('S') if lane => self.lane_start(true),
            KeyCode::Char('x') if lane => self.lane_stop(),
            KeyCode::Char('o') if lane => return self.lane_open(Instant::now()),
            KeyCode::Char('H') if lane => self.lane_hold(),
            KeyCode::Char('p') if on_lanes => self.lanes_pause(),
            KeyCode::Char('a') if on_lanes => self.lane_add(),
            KeyCode::Char('n') => self.start_nudge(),
            KeyCode::Char('k') if kind == Some(Kind::Sub) => match self.local_job() {
                Some(id) if self.selected_row().is_some_and(|r| r.state != State::Done) => self.kill_ask = Some(id),
                Some(id) => self.note = Some(format!("job {id} has ended")),
                None => self.note = Some("not a job of this session".into()),
            },
            _ => {}
        }
        ModalOut::Nothing
    }

    /// Lines a page of the pane moves by.
    fn pane_page(&self) -> usize {
        (self.pane_area.height as usize).saturating_sub(1).max(1)
    }

    /// The cursor to tree line `to` (clamped). A new selection shows its pane from the top.
    pub(super) fn move_to(&mut self, to: i64) {
        let n = self.items().len() as i64;
        if n == 0 {
            return;
        }
        let to = to.clamp(0, n - 1) as usize;
        if to != self.selected {
            self.selected = to;
            self.pane_scroll = 0;
            self.tails_asked = None;
        }
        self.follow = true;
    }

    /// Enter: a group's header folds or unfolds; in a narrow window the pane opens first; a lane moves the keys
    /// to its pane; a session is observed, a sub-agent's or a job's output opens.
    pub(super) fn enter(&mut self) -> ModalOut {
        match self.selected_item() {
            None => ModalOut::Nothing,
            Some(Item::Head { .. }) => {
                self.fold(None);
                ModalOut::Nothing
            }
            Some(Item::Row { row, kind, .. }) => {
                if !self.wide() && !self.open {
                    self.open = true;
                    self.pane_scroll = 0;
                    return ModalOut::Nothing;
                }
                if kind == Kind::Lane {
                    self.focus = true;
                    return ModalOut::Nothing;
                }
                let key = row.key.strip_prefix("nexo:").unwrap_or(&row.key).to_string();
                ModalOut::ObserveAgent { key, nexo: row.source == SourceKind::Nexo }
            }
        }
    }

    /// ← folds (`Some(true)`), → unfolds (`Some(false)`), Space flips (`None`) the selected group or parent row.
    /// ← on a row with nothing to fold goes up to its group's header.
    pub(super) fn fold(&mut self, want: Option<bool>) {
        let Some(item) = self.selected_item() else { return };
        let keep = Some((item.id(), item.row().map(|r| r.key.clone())));
        match &item {
            Item::Head { group, folded, .. } => {
                if want.is_none_or(|w| w != *folded) {
                    let id = group.id();
                    if !self.flipped.remove(&id) {
                        self.flipped.insert(id);
                    }
                }
            }
            Item::Row { row, kids, folded, .. } if *kids > 0 && want.is_none_or(|w| w != *folded) => {
                if *folded {
                    self.collapsed.remove(&row.key);
                } else {
                    self.collapsed.insert(row.key.clone());
                }
            }
            Item::Row { group, .. } if want == Some(true) => {
                let id = format!("h:{}", group.id());
                if let Some(i) = self.items().iter().position(|it| it.id() == id) {
                    self.move_to(i as i64);
                }
                return;
            }
            Item::Row { .. } => return,
        }
        self.retarget(keep);
    }

    /// `f`: everything, then what works, then what needs the person.
    pub(super) fn cycle_show(&mut self) {
        let keep = self.selected_item().map(|it| (it.id(), it.row().map(|r| r.key.clone())));
        self.show = self.show.next();
        self.retarget(keep);
        self.note = Some(format!("showing {}", self.show.label()));
    }

    /// `F`: all, then nexo only, then external only, kept for the rest of the session.
    pub(super) fn cycle_filter(&mut self) {
        let keep = self.selected_item().map(|it| (it.id(), it.row().map(|r| r.key.clone())));
        self.filter = self.filter.next();
        if let Some(m) = &self.memory {
            m.store(self.filter.to_u8(), Ordering::Relaxed);
        }
        self.retarget(keep);
        self.scroll = 0;
        self.note = Some(format!("showing {}", self.filter.label()));
    }

    /// Where a message for `row` goes without the supervisor: a running sub-agent of this session into its run,
    /// another nexo session (or a lane's) as mail. None for every other row.
    pub(super) fn mail_route(&self, row: &AgentRow) -> Option<Route> {
        match Kind::of(row) {
            Kind::Sub => {
                let job: u32 = row.key.rsplit_once('.')?.1.parse().ok()?;
                let run = self.runs.iter().find(|(id, _)| *id == job)?.1.clone();
                Some(Route::Run(run))
            }
            Kind::Peer | Kind::Lane => Some(Route::Mail(row.key.strip_prefix("nexo:")?.to_string())),
            _ => None,
        }
    }

    /// A key while the text filter is typed: it narrows the tree as it goes; Enter keeps it, Esc clears it.
    fn find_key(&mut self, key: KeyEvent) {
        let keep = self.selected_item().map(|it| (it.id(), it.row().map(|r| r.key.clone())));
        let text = self.find.get_or_insert_with(String::new);
        match key.code {
            KeyCode::Esc => {
                self.find = None;
                self.finding = false;
            }
            KeyCode::Enter => {
                self.finding = false;
                if text.is_empty() {
                    self.find = None;
                }
            }
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => text.clear(),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => text.push(c),
            _ => {}
        }
        self.retarget(keep);
    }

    pub(super) fn policy_key(&mut self, key: KeyEvent) -> ModalOut {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('P') => self.policy_page = false,
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
        match Kind::of(&row) {
            // a running sub-agent of this session takes a message into its run
            Kind::Sub => {
                if self.mail_route(&row).is_none() {
                    self.note = Some("only a running sub-agent takes a message; k stops a job".into());
                    return;
                }
            }
            Kind::Job => {
                self.note = Some("a job of another session takes no message; write to its session".into());
                return;
            }
            // other nexo sessions and lanes get mail through the mailbox, no supervisor needed
            Kind::Peer | Kind::Lane if self.mail_route(&row).is_none() => {
                self.note = Some(format!("{} has no session yet: s starts it", row.name));
                return;
            }
            Kind::Peer | Kind::Lane => {}
            // a session of another program that can be typed into needs no supervisor
            Kind::This | Kind::Other => {
                if self.panel.is_none() && self.typing_address(&row).is_none() {
                    return self.needs_supervisor();
                }
            }
        }
        let text = self.drafts.remove(&row.key).unwrap_or_default();
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
                let kept = text
                    .trim_end()
                    .char_indices()
                    .rev()
                    .find(|(_, c)| c.is_whitespace())
                    .map_or(0, |(i, c)| i + c.len_utf8());
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
                // a sub-agent of this session gets it in its run, another nexo session as mail
                if let Some(cmd) = self.mail_route(&row).map(|r| r.command(text.clone())) {
                    self.fly_key = Some(row.key.clone());
                    self.note = Some(format!("message sent to {}", row.name));
                    return ModalOut::Send(cmd);
                }
                // another program gets it as typed input, and only after a yes
                if let Some(address) = self.typing_address(&row) {
                    self.note = Some(format!("type it into {} ({address}) as its input? Enter yes · Esc no", row.name));
                    self.confirm = Some(Typing { key: row.key, name: row.name, address, text });
                    return ModalOut::Nothing;
                }
                if let Some(panel) = self.panel.clone() {
                    match panel.nudge(&row.key, &text) {
                        Ok(()) => {
                            self.fly_key = Some(row.key.clone());
                            self.note = Some(format!("message queued for {}; awaiting delivery", row.name));
                        }
                        Err(e) => {
                            self.drafts.insert(row.key.clone(), text);
                            self.note = Some(format!("message not confirmed: {e}; draft kept"));
                        }
                    }
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

    /// `z` pauses or resumes the supervisor's watch on the selected row, `Z` on every row.
    pub(super) fn toggle_pause(&mut self, all: bool) {
        let Some(panel) = self.panel.clone() else { return self.needs_supervisor() };
        if all {
            let on = !panel.paused_all();
            panel.pause(None, on);
            self.note = Some(
                if on { "supervisor paused for all sessions" } else { "supervisor resumed for all sessions" }.into(),
            );
        } else if let Some(row) = self.selected_row() {
            panel.pause(Some(&row.key), !row.paused);
            self.note = Some(format!("supervisor {} for {}", if row.paused { "resumed" } else { "paused" }, row.name));
        } else {
            self.note = Some("no session to pause".into());
        }
        self.refresh();
    }

    /// A click on the close box closes every window back to the chat. A click outside is Esc: one level up,
    /// a message being typed is kept (`n` picks it up again). A click on a tree line selects it, a second one
    /// soon after is Enter; a click in the pane moves the keys there.
    pub fn click(&mut self, x: u16, y: u16) -> ModalOut {
        self.click_at(x, y, Instant::now())
    }

    pub(super) fn click_at(&mut self, x: u16, y: u16, now: Instant) -> ModalOut {
        if contains(self.close, x, y) {
            return ModalOut::CloseAll;
        }
        let typing = self.edit.is_some() || self.confirm.is_some() || self.form.is_some() || self.finding;
        if !contains(self.window, x, y) {
            return self.key(KeyEvent::from(KeyCode::Esc));
        }
        if typing || self.policy_page || self.observing.is_some() {
            return ModalOut::Nothing;
        }
        if contains(self.pane_area, x, y) {
            self.focus = true;
            return ModalOut::Nothing;
        }
        let Some(i) = self.lines.iter().find(|(_, r)| contains(*r, x, y)).map(|(i, _)| *i) else {
            return ModalOut::Nothing;
        };
        self.focus = false;
        let double = self.last_click.is_some_and(|(j, t)| j == i && now.duration_since(t).as_millis() < DOUBLE_MS);
        self.move_to(i as i64);
        if double {
            self.last_click = None;
            return self.enter();
        }
        self.last_click = Some((i, now));
        ModalOut::Nothing
    }

    /// The pointer moved: the close box lights under it, and the wheel follows it into the pane or the tree.
    /// True when the view must repaint.
    pub fn on_hover(&mut self, x: u16, y: u16) -> bool {
        self.over_pane = contains(self.pane_area, x, y);
        let on = contains(self.close, x, y);
        std::mem::replace(&mut self.hover, on) != on
    }

    /// The wheel: the policy page and an observation by lines, the pane under the pointer by lines, else the
    /// tree, which lets go of the selection until it moves again.
    pub fn scroll_by(&mut self, delta: i32) {
        if self.policy_page {
            self.policy_scroll = (self.policy_scroll as i32 + delta).max(0) as usize;
            return;
        }
        if self.observing.is_some() {
            if delta != 0 {
                self.observe_scroll = self.observe_scroll.min(self.observe_max).saturating_add_signed(delta as isize);
            }
            return;
        }
        if self.over_pane || (self.open && !self.wide()) {
            self.pane_scroll = self.pane_scroll.saturating_add_signed(delta as isize).min(self.pane_max);
            return;
        }
        self.scroll = (self.scroll as i32 + delta).max(0) as usize;
        self.follow = false;
    }
}

#[cfg(all(test, unix))]
#[path = "typing_tests.rs"]
mod typing_tests;
