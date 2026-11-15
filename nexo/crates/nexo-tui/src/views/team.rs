//! `/team`: the lanes of `team.toml` in a small window, the table `nexo team status` prints (name, state, last
//! round and its age, spend today). Keys: `o` stops the lane's runner at its next idle point and, once it has
//! stopped, switches this session to the lane's own (as `/resume` does), `s` starts the lane, `x` stops it
//! after its round, `p` pauses or resumes every lane, `r` reads the disk again. The disk is read on a thread of
//! its own, so waiting for a runner never holds the interface up.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use nexo_core::agent::AgentCommand;
use nexo_core::team::state::{self, LaneState};
use nexo_core::team::{self, spawn, Cells, Started, Stopped};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::modal::ModalOut;
use crate::render::wrap::{truncate, width, wrap_plain};
use crate::theme::Theme;

/// How often the table is read again.
const REFRESH: Duration = Duration::from_secs(2);
/// How often it is read while a lane is being asked to stop.
const WAITING: Duration = Duration::from_millis(300);
/// How long `o` waits for a runner to reach an idle point, as `nexo team open` does.
const OPEN_WAIT: Duration = Duration::from_secs(60);
/// A read that has not answered in this long is forgotten.
const READ_GIVE_UP: Duration = Duration::from_secs(10);

/// What one read of the disk found.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Why there is no table: no `team.toml`, or it does not read.
    note: Option<String>,
    rows: Vec<Cells>,
    paused: bool,
}

/// Reads `team.toml` and every lane's state.
pub fn read() -> Snapshot {
    let paused = state::paused();
    let file = nexo_core::config::team_file();
    if !file.exists() {
        let note = format!("no lanes: create {} (an example is in docs/TEAM.md)", nexo_core::paths::shown(&file));
        return Snapshot { note: Some(note), rows: Vec::new(), paused };
    }
    match team::read_lanes() {
        Err(e) => Snapshot { note: Some(e), rows: Vec::new(), paused },
        Ok(read) => {
            let now = nexo_core::mailbox::now_ms();
            let rows = team::table(&read.team, now, &nexo_agent::budget::today(), nexo_agent::budget::local_offset());
            let note = rows.is_empty().then(|| format!("no lanes in {}", nexo_core::paths::shown(&file)));
            Snapshot { note, rows, paused }
        }
    }
}

/// A lane asked to stop so that this session can take its place.
#[derive(Debug, Clone)]
struct Wait {
    name: String,
    since: Instant,
}

#[derive(Debug, Clone)]
pub struct TeamView {
    snap: Option<Snapshot>,
    pub selected: usize,
    pub scroll: usize,
    /// A read on its way (since `last_read`): the thread leaves what it found here.
    reading: Option<Arc<Mutex<Option<Snapshot>>>>,
    last_read: Instant,
    /// Read again at the next look, whatever the clock says.
    stale: bool,
    wait: Option<Wait>,
    /// What the last key did, or why it did not.
    said: Option<String>,
    /// Starts a lane's runner: the detached `nexo team run <name>`; a test puts a stand-in here.
    starter: Starter,
}

/// Starts the runner of a lane and gives its pid.
pub type Starter = fn(&str) -> std::io::Result<u32>;

impl TeamView {
    pub fn new(now: Instant) -> TeamView {
        TeamView {
            snap: None,
            selected: 0,
            scroll: 0,
            reading: None,
            last_read: now,
            stale: true,
            wait: None,
            said: None,
            starter: spawn::runner,
        }
    }

    /// The view with `snap` already read (what a test, or a caller that has it, starts from).
    pub fn with(snap: Snapshot, now: Instant) -> TeamView {
        let mut v = TeamView::new(now);
        v.apply(snap);
        v.stale = false;
        v
    }

    /// The lanes as last read.
    pub fn rows(&self) -> &[Cells] {
        self.snap.as_ref().map_or(&[], |s| &s.rows)
    }

    fn current(&self) -> Option<&Cells> {
        self.rows().get(self.selected)
    }

    fn paused(&self) -> bool {
        self.snap.as_ref().is_some_and(|s| s.paused)
    }

    /// Takes a read of the disk; the cursor stays on the same lane when it can.
    fn apply(&mut self, snap: Snapshot) {
        let name = self.current().map(|c| c.name.clone());
        self.snap = Some(snap);
        self.selected = name.and_then(|n| self.rows().iter().position(|c| c.name == n)).unwrap_or(self.selected);
        self.selected = self.selected.min(self.rows().len().saturating_sub(1));
    }

    /// Looks at the disk again when it is due, takes what a read on its way found, and when a lane that was
    /// asked to stop has stopped, gives the session to resume. Called on every beat of the interface; it
    /// never waits.
    pub fn poll(&mut self, now: Instant) -> Option<String> {
        if let Some(cell) = &self.reading {
            let found = cell.lock().ok().and_then(|mut c| c.take());
            if let Some(snap) = found {
                self.reading = None;
                self.apply(snap);
                return self.finish_wait(now);
            }
            // a thread that never answered (it panicked) must not hold the table for good
            if now.duration_since(self.last_read) >= READ_GIVE_UP {
                self.reading = None;
            }
        }
        let every = if self.wait.is_some() { WAITING } else { REFRESH };
        if self.reading.is_none() && (self.stale || now.duration_since(self.last_read) >= every) {
            let cell = Arc::new(Mutex::new(None));
            self.reading = Some(cell.clone());
            self.last_read = now;
            self.stale = false;
            std::thread::spawn(move || {
                let snap = read();
                if let Ok(mut c) = cell.lock() {
                    *c = Some(snap);
                }
            });
        }
        None
    }

    /// The lane being waited for has stopped: its session, to resume. Or the minute is over.
    fn finish_wait(&mut self, now: Instant) -> Option<String> {
        let wait = self.wait.clone()?;
        let row = self.rows().iter().find(|c| c.name == wait.name).cloned();
        match row {
            Some(c) if c.alive => {
                if now.duration_since(wait.since) >= OPEN_WAIT {
                    self.wait = None;
                    self.said = Some(format!(
                        "{} is still in its round; it stops when the round ends, then press o again",
                        wait.name
                    ));
                }
                None
            }
            Some(Cells { session: Some(id), .. }) => {
                self.wait = None;
                Some(id)
            }
            _ => {
                self.wait = None;
                self.said = Some(format!("{} has no session yet: press s to start it", wait.name));
                None
            }
        }
    }

    pub fn key(&mut self, key: KeyEvent, now: Instant) -> ModalOut {
        let last = self.rows().len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return ModalOut::Close,
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = last,
            KeyCode::Char('r') => {
                self.said = None;
                self.stale = true;
            }
            KeyCode::Char('p') => self.pause(),
            KeyCode::Char('s') => self.start(),
            KeyCode::Char('x') => self.stop(),
            KeyCode::Char('o') => return self.open(now),
            _ => {}
        }
        ModalOut::Nothing
    }

    fn pause(&mut self) {
        let on = !self.paused();
        self.said = Some(match team::set_marker(&state::paused_file(), on) {
            Ok(()) if on => "paused: no lane starts a round".into(),
            Ok(()) => "resumed".into(),
            Err(e) => format!("cannot change the pause: {e}"),
        });
        self.stale = true;
    }

    fn start(&mut self) {
        let Some(name) = self.current().map(|c| c.name.clone()) else { return };
        self.said = Some(match team::start(&name, self.starter) {
            Ok(Started::Held) => format!("{name} is held: nexo team unhold {name}"),
            Ok(Started::Paused) => "all lanes are paused: press p".into(),
            Ok(Started::AlreadyRunning) => format!("{name} is already running"),
            Ok(Started::Started(..)) => format!("started {name}"),
            Err(e) => format!("cannot start {name}: {e}"),
        });
        self.stale = true;
    }

    fn stop(&mut self) {
        let Some(name) = self.current().map(|c| c.name.clone()) else { return };
        self.said = Some(match team::stop(&name, false) {
            Ok(Stopped::NotRunning) => format!("{name} is not running"),
            Ok(Stopped::Asked) => format!("stopping {name} after its round"),
            Ok(Stopped::Ended) => format!("stopped {name}"),
            Err(e) => format!("cannot stop {name}: {e}"),
        });
        self.stale = true;
    }

    /// Asks the lane's runner to stop at its next idle point; once it has, `poll` hands over its session.
    fn open(&mut self, now: Instant) -> ModalOut {
        let Some(name) = self.current().map(|c| c.name.clone()) else { return ModalOut::Nothing };
        let st = LaneState::load(&name);
        if st.live_pid(&name).is_none() {
            return match st.session_id {
                Some(id) => ModalOut::SendAndClose(AgentCommand::Resume { id }),
                None => {
                    self.said = Some(format!("{name} has no session yet: press s to start it"));
                    ModalOut::Nothing
                }
            };
        }
        if let Err(e) = state::touch(&state::stop_file(&name)) {
            self.said = Some(format!("cannot ask {name} to stop: {e}"));
            return ModalOut::Nothing;
        }
        self.said = Some(format!("waiting for {name} to reach an idle point (up to 60 s)"));
        self.wait = Some(Wait { name, since: now });
        self.stale = true;
        ModalOut::Nothing
    }

    /// What the last key said, if it said anything.
    pub fn said(&self) -> Option<&str> {
        self.said.as_deref()
    }

    /// Starts runners with `starter` instead of this program.
    pub fn with_starter(mut self, starter: Starter) -> TeamView {
        self.starter = starter;
        self
    }

    /// The lane being waited for, when `o` is waiting on one.
    pub fn waiting_for(&self) -> Option<&str> {
        self.wait.as_ref().map(|w| w.name.as_str())
    }

    pub fn scroll_by(&mut self, delta: i32) {
        if !self.rows().is_empty() {
            self.selected = (self.selected as i32 + delta).clamp(0, self.rows().len() as i32 - 1) as usize;
        }
    }

    pub fn title(&self) -> String {
        match (&self.snap, self.paused()) {
            (Some(_), true) => " team · paused ".into(),
            _ => " team ".into(),
        }
    }

    /// The keys, in the row pinned under the body; the longest set that fits `w`.
    pub fn keys(&self, w: usize, theme: &Theme) -> Line<'static> {
        let pause = if self.paused() { "resume" } else { "pause" };
        let long: Vec<(&str, &str)> =
            vec![("o", "open"), ("s", "start"), ("x", "stop"), ("p", pause), ("r", "reload"), ("Esc", "close")];
        let short: Vec<(&str, &str)> = vec![("o", ""), ("s", ""), ("x", ""), ("p", ""), ("r", ""), ("Esc", "")];
        for set in [long, short] {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (k, what) in set {
                if !spans.is_empty() {
                    spans.push(Span::styled(" · ", theme.dim2()));
                }
                spans.push(Span::styled(k.to_string(), theme.style(theme.accent)));
                if !what.is_empty() {
                    spans.push(Span::styled(format!(" {what}"), theme.dim()));
                }
            }
            if spans.iter().map(|s| width(&s.content)).sum::<usize>() <= w {
                return Line::from(spans);
            }
        }
        Line::from(Span::styled("Esc close", theme.dim()))
    }

    /// The body at `w` columns: the lanes, or the one line that says why there are none; then what the last key did.
    pub fn lines(&self, w: usize, theme: &Theme) -> Vec<Line<'static>> {
        let Some(snap) = &self.snap else {
            return vec![Line::from(Span::styled("reading…", theme.dim()))];
        };
        let mut lines: Vec<Line<'static>> = match &snap.note {
            Some(note) => wrap_plain(note, w, Default::default(), &[]).into_iter().map(dimmed(theme)).collect(),
            None => snap.rows.iter().enumerate().map(|(i, c)| row(c, w, theme, i == self.selected)).collect(),
        };
        // the two rows are always there once the lanes are, so the window keeps its size when a key answers
        if snap.note.is_none() {
            lines.push(Line::default());
            let said = self.said.as_deref().map(|s| truncate(s, w)).unwrap_or_default();
            lines.push(Line::from(Span::styled(said, theme.dim())));
        }
        lines
    }
}

fn dimmed(theme: &Theme) -> impl Fn(Line<'static>) -> Line<'static> + '_ {
    move |mut l| {
        for s in &mut l.spans {
            s.style = theme.dim();
        }
        l
    }
}

/// `▸ WINDOWS      running a round          last ok 3m ago       $0.13`
fn row(c: &Cells, w: usize, theme: &Theme, sel: bool) -> Line<'static> {
    let text = format!(
        "{} {:<12} {:<24} {:<20} ${:.2}",
        if sel { "▸" } else { " " },
        truncate(&c.name, 12),
        truncate(&c.state, 24),
        truncate(&c.last, 20),
        c.spend
    );
    let text = truncate(&text, w);
    let pad = w.saturating_sub(width(&text));
    let tone = if c.state.starts_with("failed") {
        theme.error
    } else if c.state == "running a round" {
        theme.success
    } else if c.state == "paused" || c.state == "held" {
        theme.warn
    } else if !c.alive {
        theme.dim
    } else {
        theme.fg
    };
    let style = if sel { theme.selection().add_modifier(Modifier::BOLD) } else { theme.style(tone) };
    Line::from(vec![Span::styled(text, style), Span::styled(" ".repeat(pad), style)])
}

impl TeamView {
    /// What the dialog draws for this view: title, body lines and the rows pinned under it.
    pub(crate) fn modal_body(&self, inner_w: usize, theme: &Theme) -> (String, Vec<Line<'static>>, Vec<Line<'static>>) {
        let pinned = vec![Line::default(), self.keys(inner_w, theme)];
        (self.title(), self.lines(inner_w, theme), pinned)
    }

    /// The scroll offset of the dialog's body of `body_len` lines shown in `body_h` rows: the selected lane stays in view.
    pub(crate) fn modal_scroll(&mut self, body_h: usize, body_len: usize) -> usize {
        self.scroll = (self.selected + 1).saturating_sub(body_h).min(body_len.saturating_sub(body_h));
        self.scroll
    }

    /// What makes this dialog the same dialog from frame to frame: the shape of what it shows.
    pub(crate) fn size_hash(&self, h: &mut impl std::hash::Hasher) {
        std::hash::Hash::hash(&(14u8, self.snap.is_some(), self.rows().len()), h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(name: &str, alive: bool, session: Option<&str>) -> Cells {
        Cells {
            name: name.into(),
            alive,
            state: if alive { "idle (mail only)".into() } else { "stopped".into() },
            last: "no round yet".into(),
            spend: 0.0,
            session: session.map(String::from),
        }
    }

    fn view(rows: Vec<Cells>, now: Instant) -> TeamView {
        TeamView::with(Snapshot { note: None, rows, paused: false }, now)
    }

    fn waiting(v: &mut TeamView, name: &str, since: Instant) {
        v.wait = Some(Wait { name: name.into(), since });
    }

    #[test]
    fn a_lane_asked_to_stop_hands_over_its_session_once_it_has_stopped() {
        let t0 = Instant::now();
        let mut v = view(vec![lane("A", true, Some("s-1"))], t0);
        waiting(&mut v, "A", t0);
        assert_eq!(v.finish_wait(t0 + Duration::from_secs(5)), None, "still in its round");
        assert_eq!(v.waiting_for(), Some("A"));
        v.apply(Snapshot { note: None, rows: vec![lane("A", false, Some("s-1"))], paused: false });
        assert_eq!(v.finish_wait(t0 + Duration::from_secs(6)), Some("s-1".to_string()));
        assert_eq!(v.waiting_for(), None);
    }

    #[test]
    fn a_runner_still_in_its_round_after_a_minute_is_said_and_not_waited_for_any_longer() {
        let t0 = Instant::now();
        let mut v = view(vec![lane("A", true, Some("s-1"))], t0);
        waiting(&mut v, "A", t0);
        assert_eq!(v.finish_wait(t0 + OPEN_WAIT), None);
        assert_eq!(v.waiting_for(), None);
        assert!(v.said().is_some_and(|s| s.contains("A is still in its round") && s.contains("press o again")));
    }

    #[test]
    fn a_lane_that_stopped_without_ever_having_a_session_says_so_instead_of_opening() {
        let t0 = Instant::now();
        let mut v = view(vec![lane("A", false, None)], t0);
        waiting(&mut v, "A", t0);
        assert_eq!(v.finish_wait(t0), None);
        assert!(v.said().is_some_and(|s| s.contains("no session yet")));
    }

    #[test]
    fn the_cursor_moves_over_the_lanes_and_stays_on_its_lane_when_the_table_is_read_again() {
        let t0 = Instant::now();
        let mut v = view(vec![lane("A", false, None), lane("B", false, None), lane("C", false, None)], t0);
        v.key(KeyEvent::from(KeyCode::Down), t0);
        v.key(KeyEvent::from(KeyCode::Char('j')), t0);
        v.key(KeyEvent::from(KeyCode::Down), t0);
        assert_eq!(v.current().map(|c| c.name.as_str()), Some("C"));
        v.apply(Snapshot { note: None, rows: vec![lane("C", false, None), lane("A", false, None)], paused: false });
        assert_eq!(v.current().map(|c| c.name.as_str()), Some("C"), "the same lane after the rows moved");
        v.key(KeyEvent::from(KeyCode::Home), t0);
        assert_eq!(v.selected, 0);
        assert!(matches!(v.key(KeyEvent::from(KeyCode::Esc), t0), ModalOut::Close));
    }

    #[test]
    fn the_keys_row_names_the_keys_and_shortens_to_letters_when_narrow() {
        let theme = Theme::dark();
        let t0 = Instant::now();
        let text = |l: Line<'static>| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>();
        let mut v = view(vec![lane("A", false, None)], t0);
        let long = text(v.keys(80, &theme));
        assert_eq!(long, "o open · s start · x stop · p pause · r reload · Esc close");
        v.snap.as_mut().unwrap().paused = true;
        assert!(text(v.keys(80, &theme)).contains("p resume"));
        assert_eq!(text(v.keys(24, &theme)), "o · s · x · p · r · Esc");
        assert_eq!(text(v.keys(10, &theme)), "Esc close");
    }
}
