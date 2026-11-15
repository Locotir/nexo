//! `/team`: the table read from a state folder, the line that says where `team.toml` goes, and what the keys
//! write: the pause file, the stop file, a started runner. Everything is in temporary folders; the person's own
//! `~/.nexo` is never read. The folders come from the environment, so one test at a time holds it.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use nexo_core::agent::AgentCommand;
use nexo_tui::modal::ModalOut;
use nexo_tui::team_view::TeamView;
use nexo_tui::theme::Theme;

static ENV: Mutex<()> = Mutex::new(());

/// A config and data folder in a temporary place, as `NEXO_CONFIG_DIR` and `NEXO_DATA_DIR`.
struct Lab {
    dir: tempfile::TempDir,
    _held: MutexGuard<'static, ()>,
}

impl Lab {
    fn new(team_toml: Option<&str>) -> Lab {
        let held = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("NEXO_CONFIG_DIR", dir.path());
        std::env::set_var("NEXO_DATA_DIR", dir.path().join("data"));
        if let Some(t) = team_toml {
            std::fs::write(dir.path().join("team.toml"), t).unwrap();
        }
        Lab { dir, _held: held }
    }

    fn team(&self, file: &str) -> std::path::PathBuf {
        self.dir.path().join("data/team").join(file)
    }

    fn write_state(&self, name: &str, json: &str) {
        std::fs::create_dir_all(self.dir.path().join("data/team")).unwrap();
        std::fs::write(self.team(&format!("{name}.json")), json).unwrap();
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        std::env::remove_var("NEXO_CONFIG_DIR");
        std::env::remove_var("NEXO_DATA_DIR");
    }
}

const LANES: &str = "[[lane]]\nname = \"WINDOWS\"\ndir = \"/tmp\"\nroutine = \"Go.\"\n\
                     [[lane]]\nname = \"IOS\"\ndir = \"/tmp\"\nroutine = \"Go.\"\n";

fn text(v: &TeamView) -> String {
    let theme = Theme::dark();
    v.lines(100, &theme)
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn press(v: &mut TeamView, c: char) -> ModalOut {
    v.key(KeyEvent::from(KeyCode::Char(c)), Instant::now())
}

/// Polls the way the interface does, a beat at a time, until `done` holds.
fn until(v: &mut TeamView, what: &str, done: impl Fn(&TeamView) -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !done(v) {
        v.poll(Instant::now());
        assert!(Instant::now() < end, "never arrived: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Until a read of the disk has arrived.
fn settle(v: &mut TeamView) {
    until(v, "the first read", |v| !text(v).contains("reading"));
}

#[test]
fn the_window_shows_each_lane_with_its_state_last_round_and_spend_today() {
    let lab = Lab::new(Some(LANES));
    let today = nexo_agent::budget::today();
    let long_ago = nexo_core::mailbox::now_ms() - 180_000;
    lab.write_state(
        "WINDOWS",
        &format!(
            "{{\"state\":\"idle\",\"last_round\":{{\"at\":{long_ago},\"result\":\"ok\",\"spend\":0.1}},\
             \"spend_day\":\"{today}\",\"spend_today\":0.5,\"session_id\":\"s-1\"}}"
        ),
    );
    lab.write_state("IOS", "{\"state\":\"failed\",\"why\":\"bad key\"}");
    let mut v = TeamView::new(Instant::now());
    assert!(text(&v).contains("reading"), "nothing is read before the first beat");
    settle(&mut v);
    let t = text(&v);
    let lines: Vec<&str> = t.lines().collect();
    assert!(lines[0].starts_with("▸ WINDOWS"), "{t}");
    assert!(lines[0].contains("stopped") && lines[0].contains("last ok 3m ago") && lines[0].contains("$0.50"), "{t}");
    assert!(
        lines[1].starts_with("  IOS") && lines[1].contains("failed: bad key") && lines[1].contains("no round yet"),
        "{t}"
    );
    assert!(!t.contains("s-1"), "the session id is the CLI's column, not the window's: {t}");
}

#[test]
fn without_a_team_file_the_window_says_where_to_create_it_in_one_line() {
    let lab = Lab::new(None);
    let mut v = TeamView::new(Instant::now());
    settle(&mut v);
    let t = text(&v);
    assert_eq!(t.lines().count(), 1, "{t}");
    assert!(t.contains("no lanes: create") && t.contains("team.toml") && t.contains("docs/TEAM.md"), "{t}");
    assert!(t.contains(&*nexo_core::paths::shown(&lab.dir.path().join("team.toml"))), "{t}");
    // a lane key does nothing without lanes, and the window closes
    assert!(matches!(press(&mut v, 's'), ModalOut::Nothing));
    assert!(matches!(press(&mut v, 'o'), ModalOut::Nothing));
    assert!(matches!(v.key(KeyEvent::from(KeyCode::Esc), Instant::now()), ModalOut::Close));
    assert!(!lab.team("paused").exists());
}

#[test]
fn p_pauses_and_resumes_every_lane_through_the_pause_file() {
    let lab = Lab::new(Some(LANES));
    let mut v = TeamView::new(Instant::now());
    settle(&mut v);
    press(&mut v, 'p');
    assert!(lab.team("paused").exists());
    assert!(v.said().is_some_and(|s| s.starts_with("paused")));
    until(&mut v, "the paused title", |v| v.title().contains("paused"));
    press(&mut v, 'p');
    assert!(!lab.team("paused").exists());
    assert_eq!(v.said(), Some("resumed"));
}

#[cfg(unix)]
fn stand_in(name: &str) -> std::io::Result<u32> {
    // `echo team run NAME` ends at once: what is checked is what the window asks the system to start
    nexo_core::team::spawn::runner_of(std::path::Path::new("/bin/echo"), name)
}

#[cfg(unix)]
#[test]
fn s_starts_the_selected_lane_detached_with_team_run_and_leaves_held_and_paused_ones_alone() {
    let lab = Lab::new(Some(LANES));
    let mut v = TeamView::new(Instant::now()).with_starter(stand_in);
    settle(&mut v);
    press(&mut v, 's');
    assert_eq!(v.said(), Some("started WINDOWS"));
    let log = lab.team("WINDOWS.log");
    let end = Instant::now() + Duration::from_secs(10);
    while !std::fs::read_to_string(&log).unwrap_or_default().contains("team run WINDOWS") {
        assert!(Instant::now() < end, "the stand-in never wrote its log");
        std::thread::sleep(Duration::from_millis(20));
    }
    let state = std::fs::read_to_string(lab.team("WINDOWS.json")).unwrap();
    assert!(state.contains("\"pid\"") && state.contains("\"idle\""), "{state}");

    // the second lane is held: start leaves it
    v.key(KeyEvent::from(KeyCode::Down), Instant::now());
    nexo_core::team::set_marker(&nexo_core::team::state::hold_file("IOS"), true).unwrap();
    press(&mut v, 's');
    assert_eq!(v.said(), Some("IOS is held: nexo team unhold IOS"));
    assert!(!lab.team("IOS.log").exists());
    nexo_core::team::set_marker(&nexo_core::team::state::hold_file("IOS"), false).unwrap();

    // and while every lane is paused
    press(&mut v, 'p');
    press(&mut v, 's');
    assert_eq!(v.said(), Some("all lanes are paused: press p"));
    assert!(!lab.team("IOS.log").exists());
}

#[test]
fn x_asks_nothing_of_a_lane_that_is_not_running_and_o_resumes_a_stopped_lane_at_once() {
    let lab = Lab::new(Some(LANES));
    lab.write_state("WINDOWS", "{\"state\":\"stopped\",\"session_id\":\"s-1\"}");
    let mut v = TeamView::new(Instant::now());
    settle(&mut v);
    press(&mut v, 'x');
    assert_eq!(v.said(), Some("WINDOWS is not running"));
    assert!(!lab.team("WINDOWS.stop").exists(), "no stop file is left for the next runner to find");
    // the lane has a session and no runner: the window switches to it as /resume would
    match press(&mut v, 'o') {
        ModalOut::SendAndClose(AgentCommand::Resume { id }) => assert_eq!(id, "s-1"),
        _ => panic!("o on a stopped lane resumes its session"),
    }
    // a lane that never ran has none
    v.key(KeyEvent::from(KeyCode::Down), Instant::now());
    assert!(matches!(press(&mut v, 'o'), ModalOut::Nothing));
    assert_eq!(v.said(), Some("IOS has no session yet: press s to start it"));
}
