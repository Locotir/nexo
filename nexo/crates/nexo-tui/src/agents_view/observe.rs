//! Read-only observation of the bounded event feed already exposed by the supervisor.
use super::*;
use nexo_agent::supervise::EvDetail;

impl AgentsView {
    /// Fall back to the existing bounded observation for sessions outside this process.
    pub fn observe_selected(&mut self, key: &str) {
        let Some(row) = self.selected_row().filter(|r| r.key == key) else { return };
        self.start_history(&row);
        self.observing = Some(row.key);
        self.observe_scroll = 0;
        self.observe_max = 0;
    }

    pub(super) fn start_history(&mut self, row: &AgentRow) {
        self.history_rx = None;
        self.history = None;
        if self.panel.is_none() || row.source != SourceKind::Nexo {
            return;
        }
        let id = row.key.clone();
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            self.history = Some(Err("No persisted session history for this agent (it may be a sub-agent).".into()));
            return;
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.history_rx = Some(Arc::new(std::sync::Mutex::new(rx)));
        std::thread::spawn(move || loop {
            let result = nexo_agent::session::history(&id).map_err(|e| e.to_string());
            match tx.try_send(result) {
                Ok(()) | Err(std::sync::mpsc::TrySendError::Full(_)) => {}
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => break,
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        });
    }

    pub(super) fn poll_history(&mut self) -> bool {
        let Some(rx) = &self.history_rx else { return false };
        let Ok(rx) = rx.try_lock() else { return false };
        let Ok(next) = rx.try_recv() else { return false };
        let changed = self.history.as_ref() != Some(&next);
        self.history = Some(next);
        changed
    }

    pub(super) fn observe_key(&mut self, key: KeyEvent) -> ModalOut {
        if matches!(
            key.code,
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') | KeyCode::PageUp | KeyCode::PageDown
        ) {
            self.observe_scroll = self.observe_scroll.min(self.observe_max);
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                self.observing = None;
                self.history_rx = None;
                self.history = None;
            }
            KeyCode::Up | KeyCode::Char('k') => self.observe_scroll = self.observe_scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.observe_scroll = self.observe_scroll.saturating_add(1),
            KeyCode::PageUp => self.observe_scroll = self.observe_scroll.saturating_sub(10),
            KeyCode::PageDown => self.observe_scroll = self.observe_scroll.saturating_add(10),
            KeyCode::Home | KeyCode::Char('g') => self.observe_scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.observe_scroll = usize::MAX,
            KeyCode::Char('n') => {
                if self.selected_key().as_ref() == self.observing.as_ref() {
                    self.start_nudge();
                } else {
                    self.note = Some("session no longer available; no message sent".into());
                }
            }
            _ => {}
        }
        ModalOut::Nothing
    }

    pub(super) fn draw_observation(&mut self, f: &mut Frame, body: Rect, theme: &Theme) {
        let row = self.rows.iter().find(|r| Some(&r.key) == self.observing.as_ref()).cloned().map(tidy);
        let mut lines = Vec::new();
        if let Some(row) = row {
            lines.push(format!("{} · {} · {}", row.name, row.program, chip(row.state).1));
            if let Some(model) = &row.model {
                lines.push(format!("Model: {model}"));
            }
            lines.push(format!("Last step: {}", row.last_step.as_deref().unwrap_or("unavailable")));
            if let Some(result) = row.last_conclusion {
                lines.push(format!("Last result: {result}"));
            }
            if row.source == SourceKind::Nexo && self.panel.is_some() {
                lines.push("Saved history (last 256 KiB; updated every second; not live output; tool arguments and images omitted):".into());
                match &self.history {
                    None => lines.push("Loading saved history…".into()),
                    Some(Err(e)) => lines.push(format!("Saved history unavailable: {e}")),
                    Some(Ok(snapshot)) => {
                        if snapshot.older_omitted {
                            lines.push("Earlier records omitted (256 KiB limit).".into());
                        }
                        if snapshot.partial_record_omitted {
                            lines.push(
                                "A record crossing the 256 KiB limit was omitted entirely (too large or truncated)."
                                    .into(),
                            );
                        }
                        if snapshot.incomplete_tail {
                            lines.push("Incomplete final record not shown yet.".into());
                        }
                        if snapshot.records.is_empty() && !snapshot.partial_record_omitted && !snapshot.incomplete_tail
                        {
                            lines.push("No saved messages in this window.".into());
                        }
                        lines.extend(snapshot.records.iter().cloned());
                    }
                }
            }
            lines.push("Recent observed events (up to 12; not a transcript):".into());
            if row.ribbon.is_empty() {
                lines.push("No events available yet.".into());
            }
            for event in row.ribbon {
                let detail = match event.detail {
                    EvDetail::None => String::new(),
                    EvDetail::Progress { done, total } => format!(" · {done}/{total}"),
                    EvDetail::Tests { pass, fail } => format!(" · {pass} passed, {fail} failed"),
                    EvDetail::Usd(n) => format!(" · ${n:.4}"),
                    EvDetail::Tokens(n) => format!(" · {n} tokens"),
                    EvDetail::Peer(s) | EvDetail::Text(s) => format!(" · {}", crate::render::wrap::one_line(&s)),
                    EvDetail::Wait { secs } => format!(" · {secs}s"),
                };
                let repeat = if event.n > 1 { format!(" ×{}", event.n) } else { String::new() };
                lines.push(format!("{} · {:?}{repeat}{detail}", event.at, event.kind));
            }
        } else {
            lines.push("Session no longer available. Esc returns to the panel.".into());
        }
        // Scroll over wrapped display rows, not logical events: long and multiline content must remain reachable.
        let wrapped: Vec<Line> = lines
            .iter()
            .flat_map(|s| crate::render::wrap::wrap_plain(s, body.width.max(1) as usize, theme.style(theme.fg), &[]))
            .collect();
        let max = wrapped.len().saturating_sub(body.height as usize);
        self.observe_max = max;
        let scroll = if self.observe_scroll == usize::MAX {
            max
        } else {
            self.observe_scroll = self.observe_scroll.min(max);
            self.observe_scroll
        };
        let shown: Vec<Line> = wrapped.into_iter().skip(scroll).take(body.height as usize).collect();
        f.render_widget(Paragraph::new(Text::from(shown)), body);
    }
}
