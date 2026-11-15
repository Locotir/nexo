//! The actions on `nexo team` lanes, moved here from the old `/team` window: start (now or at its clock),
//! stop after the round, hold, pause every lane, open the lane in this session, and add a lane with the
//! questions `nexo team add` asks. Every action goes through `nexo_core::team`, the same code the CLI runs.

use std::time::{Duration, Instant};

use nexo_core::agent::AgentCommand;
use nexo_core::config::Lane;
use nexo_core::team::add::{self, FIELDS};
use nexo_core::team::state::{self, LaneState};
use nexo_core::team::{self, Started, Stopped};

use super::*;

/// How often the lane's state file is read while `o` waits for its runner to stop.
pub(super) const WAITING: Duration = Duration::from_millis(300);
/// How long `o` waits for a runner to reach an idle point, as `nexo team open` does.
pub(super) const OPEN_WAIT: Duration = Duration::from_secs(60);

/// Starts the runner of a lane and gives its pid.
pub type Starter = fn(&str) -> std::io::Result<u32>;

/// A new lane being asked about, one question at a time (the ones `nexo team add` asks).
#[derive(Debug, Clone)]
pub(super) struct Form {
    pub lane: Lane,
    pub at: usize,
    pub typed: String,
    pub problem: Option<String>,
}

/// A lane asked to stop so that this session can take its place.
#[derive(Debug, Clone)]
pub(super) struct Wait {
    pub name: String,
    pub since: Instant,
    pub last_look: Instant,
}

impl AgentsView {
    /// The name of the selected lane.
    pub(super) fn lane_name(&self) -> Option<String> {
        let row = self.selected_row()?;
        Some(match &row.lane {
            Some(l) => l.name.clone(),
            None => row.key.strip_prefix("lane:")?.to_string(),
        })
    }

    /// `s` (`now` false) or `S` (`now`: the first round starts at once).
    pub(super) fn lane_start(&mut self, now: bool) {
        let Some(name) = self.lane_name() else { return };
        let starter = if now { self.starter_now } else { self.starter };
        self.note = Some(match team::start(&name, starter) {
            Ok(Started::Held) => format!("{name} is held: press H to unhold it"),
            Ok(Started::Paused) => "all lanes are paused: press p".into(),
            Ok(Started::AlreadyRunning) => format!("{name} is already running"),
            Ok(Started::Started(..)) if now => format!("started {name}: a round starts now"),
            Ok(Started::Started(..)) => format!("started {name}"),
            Err(e) => format!("cannot start {name}: {e}"),
        });
    }

    /// `x`: the runner ends after the round in flight.
    pub(super) fn lane_stop(&mut self) {
        let Some(name) = self.lane_name() else { return };
        self.note = Some(match team::stop(&name, false) {
            Ok(Stopped::NotRunning) => format!("{name} is not running"),
            Ok(Stopped::Asked) => format!("stopping {name} after its round"),
            Ok(Stopped::Ended) => format!("stopped {name}"),
            Err(e) => format!("cannot stop {name}: {e}"),
        });
    }

    /// `H`: a held lane starts no round until it is unheld.
    pub(super) fn lane_hold(&mut self) {
        let Some(name) = self.lane_name() else { return };
        let on = !state::held(&name);
        self.note = Some(match team::set_marker(&state::hold_file(&name), on) {
            Ok(()) if on => format!("held: {name}"),
            Ok(()) => format!("unheld: {name}"),
            Err(e) => format!("cannot change the hold of {name}: {e}"),
        });
    }

    /// `p`: no lane starts a round while every lane is paused.
    pub(super) fn lanes_pause(&mut self) {
        let on = !state::paused();
        self.note = Some(match team::set_marker(&state::paused_file(), on) {
            Ok(()) if on => {
                self.lanes_paused = true;
                "paused: no lane starts a round".into()
            }
            Ok(()) => {
                self.lanes_paused = false;
                "resumed: lanes start their rounds again".into()
            }
            Err(e) => format!("cannot change the pause: {e}"),
        });
    }

    /// `o`: asks the lane's runner to stop at its next idle point; once it has, the panel hands its session over
    /// to this one (as `/resume` does). A lane whose runner is not alive is resumed at once.
    pub(super) fn lane_open(&mut self, now: Instant) -> ModalOut {
        let Some(name) = self.lane_name() else { return ModalOut::Nothing };
        let st = LaneState::load(&name);
        if st.live_pid(&name).is_none() {
            return match st.session_id {
                Some(id) => ModalOut::SendAndClose(AgentCommand::Resume { id }),
                None => {
                    self.note = Some(format!("{name} has no session yet: press s to start it"));
                    ModalOut::Nothing
                }
            };
        }
        if let Err(e) = state::touch(&state::stop_file(&name)) {
            self.note = Some(format!("cannot ask {name} to stop: {e}"));
            return ModalOut::Nothing;
        }
        self.note = Some(format!("waiting for {name} to reach an idle point (up to 60 s)"));
        self.wait = Some(Wait { name, since: now, last_look: now });
        ModalOut::Nothing
    }

    /// While `o` waits: reads the lane's state now and then. True when the note changed.
    pub(super) fn poll_wait(&mut self, now: Instant) -> bool {
        let Some(w) = self.wait.as_mut() else { return false };
        if now.duration_since(w.last_look) < WAITING {
            return false;
        }
        w.last_look = now;
        let w = w.clone();
        let st = LaneState::load(&w.name);
        let alive = st.live_pid(&w.name).is_some();
        self.finish_wait(&w, alive, st.session_id, now)
    }

    /// What became of a lane being waited for: stopped with a session (it is handed over), stopped without one,
    /// or still in its round after the minute.
    pub(super) fn finish_wait(&mut self, w: &Wait, alive: bool, session: Option<String>, now: Instant) -> bool {
        if alive {
            if now.duration_since(w.since) >= OPEN_WAIT {
                self.wait = None;
                self.note =
                    Some(format!("{} is still in its round; it stops when the round ends, then press o again", w.name));
                return true;
            }
            return false;
        }
        self.wait = None;
        match session {
            Some(id) => self.resume = Some(id),
            None => self.note = Some(format!("{} has no session yet: press s to start it", w.name)),
        }
        true
    }

    /// The session of a lane that stopped for this one to take it, once (the app resumes it).
    pub fn take_resume(&mut self) -> Option<String> {
        self.resume.take()
    }

    /// The lane `o` is waiting for.
    pub fn waiting_for(&self) -> Option<&str> {
        self.wait.as_ref().map(|w| w.name.as_str())
    }

    /// Starts runners with these instead of this program (tests start a stand-in).
    pub fn with_starters(mut self, starter: Starter, starter_now: Starter) -> AgentsView {
        self.starter = starter;
        self.starter_now = starter_now;
        self
    }

    /// `a`: the questions of a new lane, in the pane.
    pub(super) fn lane_add(&mut self) {
        self.form = Some(Form { lane: Lane::default(), at: 0, typed: String::new(), problem: None });
        self.focus = true;
        self.open = true;
    }

    /// A key while a new lane is being asked about: typing, Enter for the answer, Esc to give up.
    pub(super) fn form_key(&mut self, key: KeyEvent) {
        let Some(form) = self.form.as_mut() else { return };
        match key.code {
            KeyCode::Esc => {
                self.form = None;
                self.focus = false;
            }
            KeyCode::Backspace => {
                form.typed.pop();
                form.problem = None;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                form.typed.push(c);
                form.problem = None;
            }
            KeyCode::Enter => self.answer_form(),
            _ => {}
        }
    }

    /// Takes the typed answer to the question in view: the next question, or the lane written to `team.toml`.
    fn answer_form(&mut self) {
        let Some(form) = self.form.as_mut() else { return };
        let field = FIELDS[form.at];
        let existing = match team::read_lanes() {
            Ok(f) => f.team,
            Err(e) => {
                form.problem = Some(e);
                return;
            }
        };
        let mut lane = form.lane.clone();
        add::set(&mut lane, field, &form.typed);
        if let Some(p) = add::problem(&existing, &lane, field) {
            form.problem = Some(p);
            return;
        }
        form.lane = lane;
        form.typed.clear();
        form.at += 1;
        if form.at < FIELDS.len() {
            return;
        }
        let lane = form.lane.clone();
        match add::add(&lane) {
            Ok(()) => {
                self.note = Some(format!("added {}: press s to start it", lane.name));
                self.form = None;
                self.focus = false;
            }
            Err(e) => {
                form.at = 0;
                form.problem = Some(e);
            }
        }
    }

    /// The questions of a new lane, as the pane shows them.
    pub(super) fn form_lines(&self, w: usize, theme: &Theme) -> Vec<Line<'static>> {
        let Some(form) = &self.form else { return Vec::new() };
        let field = FIELDS[form.at];
        let shown = if form.typed.is_empty() {
            match field.default() {
                Some(d) => Span::styled(format!("{d} (default)"), theme.dim()),
                None => Span::raw(""),
            }
        } else {
            Span::raw(form.typed.clone())
        };
        let ask = Line::from(vec![
            Span::styled(format!("{}: ", field.label()), theme.style(theme.accent)),
            shown,
            Span::raw("▏"),
        ]);
        let problem = form.problem.as_deref().map(|p| truncate(p, w)).unwrap_or_default();
        vec![
            Line::from(Span::styled(
                format!("new lane · question {} of {}", form.at + 1, FIELDS.len()),
                theme.bold(theme.accent),
            )),
            Line::default(),
            ask,
            Line::from(Span::styled(truncate(field.hint(), w), theme.dim())),
            Line::from(Span::styled(problem, theme.style(theme.error))),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting(name: &str, since: Instant) -> (AgentsView, Wait) {
        let mut v = AgentsView::with_rows(Vec::new(), 0, true);
        let w = Wait { name: name.into(), since, last_look: since };
        v.wait = Some(w.clone());
        (v, w)
    }

    #[test]
    fn a_lane_asked_to_stop_hands_over_its_session_once_it_has_stopped() {
        let t0 = Instant::now();
        let (mut v, w) = waiting("A", t0);
        assert!(!v.finish_wait(&w, true, Some("s-1".into()), t0 + Duration::from_secs(5)), "still in its round");
        assert_eq!(v.waiting_for(), Some("A"));
        assert!(v.finish_wait(&w, false, Some("s-1".into()), t0 + Duration::from_secs(6)));
        assert_eq!(v.take_resume(), Some("s-1".to_string()));
        assert_eq!(v.take_resume(), None, "handed over once");
        assert_eq!(v.waiting_for(), None);
    }

    #[test]
    fn a_runner_still_in_its_round_after_a_minute_is_said_and_not_waited_for_any_longer() {
        let t0 = Instant::now();
        let (mut v, w) = waiting("A", t0);
        assert!(v.finish_wait(&w, true, Some("s-1".into()), t0 + OPEN_WAIT));
        assert_eq!(v.waiting_for(), None);
        assert!(v.said().is_some_and(|s| s.contains("A is still in its round") && s.contains("press o again")));
        assert_eq!(v.take_resume(), None);
    }

    #[test]
    fn a_lane_that_stopped_without_ever_having_a_session_says_so_instead_of_opening() {
        let t0 = Instant::now();
        let (mut v, w) = waiting("A", t0);
        assert!(v.finish_wait(&w, false, None, t0));
        assert!(v.said().is_some_and(|s| s.contains("no session yet")));
        assert_eq!(v.take_resume(), None);
    }
}
