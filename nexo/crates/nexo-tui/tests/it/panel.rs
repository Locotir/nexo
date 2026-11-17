//! The `/panel` mission control: rows of every kind drawn at 170x48 and 90x40, the keys each kind of row answers,
//! the narrow mode, rest, and the mono theme. Rows are built by hand. The lane keys act on temporary folders set as
//! `NEXO_CONFIG_DIR` and `NEXO_DATA_DIR`; the person's own `~/.nexo` is never read.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_core::agent::AgentCommand;
use nexo_supervise::{AgentRow, Ev, EvDetail, EvKind, LaneInfo, SourceKind, State};
use nexo_tui::agents_view::{AgentsView, Kind};
use nexo_tui::modal::ModalOut;
use nexo_tui::theme::Theme;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;

const NOW: i64 = 1_800_000_000;

fn row(key: &str, name: &str, state: State) -> AgentRow {
    AgentRow {
        key: key.into(),
        name: name.into(),
        source: SourceKind::Nexo,
        program: "nexo".into(),
        state,
        since: NOW - 180,
        last_step: None,
        last_conclusion: None,
        idle_secs: Some(5),
        cost_usd: None,
        model: None,
        cwd: None,
        permission_mode: None,
        parent: None,
        last_action: None,
        paused: false,
        ribbon: vec![],
        spark: vec![],
        todo: None,
        tests: None,
        subs: vec![],
        unread: 0,
        held: 0,
        retry_secs: None,
        ctx_pct: None,
        goal: None,
        lane: None,
    }
}

fn lane(key: &str, name: &str, state: &str) -> AgentRow {
    let mut r = row(key, name, if state == "running" { State::Working } else { State::Idle { min: 3 } });
    r.lane = Some(LaneInfo {
        name: name.into(),
        state: state.into(),
        runner: state == "running" || state == "idle",
        next_round: (state == "idle").then_some(NOW + 300),
        every_secs: Some(3600),
        last_round: Some((NOW - 800, "ok".into(), 0.4)),
        rounds: 3,
        failures: 1,
        spend_today: 2.58,
        cap_day: Some(4.0),
        why: None,
        dir: "/tmp/lane".into(),
        model: Some("xai/grok-4.7".into()),
        mode: Some("yolo".into()),
    });
    r
}

/// This session with two sub-agents, two lanes (one running, one idle with its next round), another nexo
/// session, and two sessions of another program, one of them waiting for the person.
fn fleet() -> Vec<AgentRow> {
    let mut this = row("s1", "paneltry", State::Working);
    this.model = Some("xai/grok-4".into());
    this.cost_usd = Some(0.84);
    this.ctx_pct = Some(42);
    this.goal = Some("make the panel".into());
    this.last_step = Some("Edit  src/panel.rs".into());
    this.ribbon = vec![Ev { at: NOW - 30, kind: EvKind::Edit, n: 2, detail: EvDetail::None }];
    let mut sub = row("s1.3", "audit grep caps", State::Working);
    sub.parent = Some("s1".into());
    sub.spark = vec![1, 3, 5, 7];
    let mut done = row("s1.4", "fix tests", State::Done);
    done.parent = Some("s1".into());
    let mut ios = lane("nexo:ios-1", "IOS", "running");
    ios.last_step = Some("bash  tools/asc/check".into());
    let windows = lane("lane:WINDOWS", "WINDOWS", "idle");
    let mut peer = row("nexo:ceo-1", "NEXO CEO", State::Idle { min: 3 });
    peer.cwd = Some("/home/dev/ceo".into());
    let mut c1 = row("claude:a", "claude-a", State::Working);
    c1.source = SourceKind::External;
    c1.program = "claude".into();
    let mut c2 = row("claude:b", "claude-b", State::WaitingPermission);
    c2.source = SourceKind::External;
    c2.program = "claude".into();
    vec![this, sub, done, ios, windows, peer, c1, c2]
}

fn paint(v: &mut AgentsView, w: u16, h: u16, theme: &Theme, now: Instant, fx: bool) -> Buffer {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| v.draw(f, f.area(), theme, now, fx)).unwrap();
    t.backend().buffer().clone()
}

fn lines(b: &Buffer) -> Vec<String> {
    (0..b.area.height).map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect::<String>()).collect()
}

fn press(v: &mut AgentsView, code: KeyCode) -> ModalOut {
    v.key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn select(v: &mut AgentsView, key: &str) {
    press(v, KeyCode::Home);
    for _ in 0..40 {
        if v.selected_key().as_deref() == Some(key) {
            return;
        }
        press(v, KeyCode::Down);
    }
    panic!("{key} is not in the tree");
}

fn find(text: &[String], what: &str) -> usize {
    text.iter().position(|l| l.contains(what)).unwrap_or_else(|| panic!("no {what:?} in\n{}", text.join("\n")))
}

#[test]
fn at_170_columns_the_tree_lists_every_group_in_order_beside_the_pane() {
    let mut v = AgentsView::with_rows(fleet(), NOW, true);
    let b = paint(&mut v, 170, 48, &Theme::dark(), Instant::now(), false);
    let text = lines(&b);
    let all = text.join("\n");
    let order = ["NEEDS YOU", "THIS SESSION", "LANES", "NEXO SESSIONS", "CLAUDE"].map(|g| find(&text, g));
    assert!(order.windows(2).all(|w| w[0] < w[1]), "groups out of order {order:?}\n{all}");
    // the other program is folded to one line with its count, and its row that waits is under NEEDS YOU
    assert!(text[order[4]].contains("CLAUDE · 2"), "{all}");
    assert!(!all.contains("claude-a"), "a folded group shows no rows\n{all}");
    assert!(find(&text, "claude-b") < order[1], "{all}");
    // sub-agents nest under this session with tree lines
    let sub = find(&text, "audit grep caps");
    assert!(sub > find(&text, "paneltry") && (text[sub].contains('├') || text[sub].contains('╰')), "{all}");
    // a running lane says its round, an idle one the clock of its next round
    assert!(text[find(&text, "IOS")].contains("round 4"), "{all}");
    assert!(text[find(&text, "WINDOWS")].contains('→'), "{all}");
    assert!(text[order[2]].contains("$5.16 of $8"), "the LANES header sums the spend and the caps\n{all}");
    // the pane: a divider joins the rules, and the header line counts the fleet
    assert!(text.iter().any(|l| l.contains('┬')) && text.iter().any(|l| l.contains('┴')), "{all}");
    assert!(text[1].contains("needs you"), "{all}");
    assert!(text.iter().all(|l| l.chars().count() == 170), "no line overflows");
}

#[test]
fn at_90_columns_the_tree_takes_the_whole_width_and_enter_opens_the_pane_until_esc() {
    let mut v = AgentsView::with_rows(fleet(), NOW, true);
    let theme = Theme::dark();
    let text = lines(&paint(&mut v, 90, 40, &theme, Instant::now(), false));
    assert!(!text.iter().any(|l| l.contains('┬')), "no pane beside the tree\n{}", text.join("\n"));
    find(&text, "THIS SESSION");
    find(&text, "NEXO SESSIONS");
    select(&mut v, "nexo:ios-1");
    assert!(v.footer_keys().contains(&"⏎ details"));
    assert!(matches!(press(&mut v, KeyCode::Enter), ModalOut::Nothing));
    assert!(v.details_open());
    let text = lines(&paint(&mut v, 90, 40, &theme, Instant::now(), false));
    let all = text.join("\n");
    assert!(all.contains("IOS · lane") && all.contains("$2.58 of $4 today"), "{all}");
    assert!(!all.contains("THIS SESSION"), "the pane takes the width\n{all}");
    assert!(matches!(press(&mut v, KeyCode::Esc), ModalOut::Nothing), "Esc goes back, it does not close");
    assert!(!v.details_open());
    let text = lines(&paint(&mut v, 90, 40, &theme, Instant::now(), false));
    find(&text, "THIS SESSION");
    assert!(matches!(press(&mut v, KeyCode::Esc), ModalOut::Close));
}

#[test]
fn the_footer_lists_only_the_keys_that_act_on_the_selected_kind_of_row() {
    let mut v = AgentsView::with_rows(fleet(), NOW, true);
    paint(&mut v, 170, 48, &Theme::dark(), Instant::now(), false);
    v.set_runs(vec![(3, "s1#3".into())]);
    let lane_keys = ["s start", "S round now", "x stop", "o open here", "H hold", "p pause lanes", "a add lane"];
    select(&mut v, "lane:WINDOWS");
    assert_eq!(v.selected_kind(), Some(Kind::Lane));
    let keys = v.footer_keys();
    assert!(lane_keys.iter().all(|k| keys.contains(k)), "{keys:?}");
    assert!(!keys.contains(&"n message") && !keys.contains(&"k stop"), "a lane without a session takes no mail");
    select(&mut v, "s1");
    let keys = v.footer_keys();
    assert!(keys.contains(&"n message") && keys.contains(&"⏎ back to chat"), "{keys:?}");
    assert!(!lane_keys.iter().any(|k| keys.contains(k)), "{keys:?}");
    select(&mut v, "s1.3");
    let keys = v.footer_keys();
    assert!(keys.contains(&"n message") && keys.contains(&"k stop") && keys.contains(&"⏎ live"), "{keys:?}");
    select(&mut v, "s1.4");
    let keys = v.footer_keys();
    assert!(!keys.contains(&"n message") && !keys.contains(&"k stop"), "a finished job: {keys:?}");
    select(&mut v, "nexo:ceo-1");
    assert!(v.footer_keys().contains(&"n message"));
    // no key means two things: every key of a row is listed once
    for key in ["s1", "s1.3", "nexo:ios-1", "lane:WINDOWS", "nexo:ceo-1"] {
        select(&mut v, key);
        let letters: Vec<&str> = v.footer_keys().iter().map(|k| k.split(' ').next().unwrap()).collect();
        let mut unique = letters.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), letters.len(), "{key}: {letters:?}");
    }
}

#[test]
fn n_on_a_running_sub_agent_writes_into_its_run_and_on_another_session_sends_mail() {
    let mut v = AgentsView::with_rows(fleet(), NOW, true);
    v.set_runs(vec![(3, "s1#3".into())]);
    let type_in = |v: &mut AgentsView, s: &str| -> ModalOut {
        for c in s.chars() {
            press(v, KeyCode::Char(c));
        }
        press(v, KeyCode::Enter)
    };
    select(&mut v, "s1.3");
    press(&mut v, KeyCode::Char('n'));
    match type_in(&mut v, "look at caps") {
        ModalOut::Send(AgentCommand::SubSubmit { id, text }) => {
            assert_eq!((id.as_str(), text.as_str()), ("s1#3", "look at caps"))
        }
        _ => panic!("a running sub-agent gets the message in its run"),
    }
    select(&mut v, "nexo:ceo-1");
    press(&mut v, KeyCode::Char('n'));
    match type_in(&mut v, "rebase please") {
        ModalOut::Send(AgentCommand::SendMail { to, text }) => {
            assert_eq!((to.as_str(), text.as_str()), ("ceo-1", "rebase please"))
        }
        _ => panic!("another nexo session gets mail"),
    }
    select(&mut v, "lane:WINDOWS");
    press(&mut v, KeyCode::Char('n'));
    assert!(v.said().is_some_and(|s| s.contains("no session yet")), "{:?}", v.said());
}

#[test]
fn k_stops_a_job_of_this_session_only_after_y() {
    let mut v = AgentsView::with_rows(fleet(), NOW, true);
    paint(&mut v, 170, 48, &Theme::dark(), Instant::now(), false);
    select(&mut v, "s1.3");
    assert!(matches!(press(&mut v, KeyCode::Char('k')), ModalOut::Nothing));
    let text = lines(&paint(&mut v, 170, 48, &Theme::dark(), Instant::now(), false)).join("\n");
    assert!(text.contains("stop job 3?"), "the footer asks\n{text}");
    assert!(matches!(press(&mut v, KeyCode::Char('n')), ModalOut::Nothing));
    assert!(v.said().is_some_and(|s| s.contains("keeps running")));
    press(&mut v, KeyCode::Char('k'));
    assert!(matches!(press(&mut v, KeyCode::Char('y')), ModalOut::Send(AgentCommand::KillJob { id: Some(3) })));
}

#[test]
fn at_rest_two_draws_are_equal_and_ask_for_no_frame() {
    let rest: Vec<AgentRow> = fleet()
        .into_iter()
        .map(|mut r| {
            r.state = State::Idle { min: 3 };
            for s in &mut r.subs {
                s.1 = State::Done;
            }
            if let Some(l) = r.lane.as_mut() {
                l.state = "idle".into();
                l.next_round = Some(NOW + 300);
            }
            r
        })
        .collect();
    let mut v = AgentsView::with_rows(rest, NOW, false);
    let theme = Theme::dark();
    let t0 = Instant::now();
    // the opening motion settles first
    paint(&mut v, 170, 48, &theme, t0, true);
    let a = paint(&mut v, 170, 48, &theme, t0 + Duration::from_secs(20), true);
    assert!(!v.animating(t0 + Duration::from_secs(20)), "nothing moves at rest");
    let b = paint(&mut v, 170, 48, &theme, t0 + Duration::from_secs(21), true);
    assert_eq!(lines(&a), lines(&b));
    assert_eq!(a, b, "colours too");
}

#[test]
fn a_stopped_or_failed_lane_counts_by_its_lane_word_whatever_its_last_observation_left() {
    // a lane without a runner keeps the state its spend note left (working); its lane word is what counts
    let mut failed = lane("lane:IOS", "IOS", "failed");
    failed.lane.as_mut().unwrap().runner = false;
    failed.state = State::Working;
    let mut stopped = lane("lane:WINDOWS", "WINDOWS", "stopped");
    stopped.lane.as_mut().unwrap().runner = false;
    stopped.state = State::Stalled;
    let mut v = AgentsView::with_rows(vec![failed, stopped], NOW, false);
    let theme = Theme::dark();
    let t0 = Instant::now();
    let text = lines(&paint(&mut v, 170, 48, &theme, t0, true));
    let head = &text[find(&text, "needs you")];
    assert!(head.contains("0 working") && head.contains("2 idle") && head.contains("1 needs you"), "{head}");
    paint(&mut v, 170, 48, &theme, t0 + Duration::from_secs(20), true);
    assert!(!v.animating(t0 + Duration::from_secs(20)), "a lane that runs nothing moves nothing");
}

#[test]
fn a_long_folder_keeps_its_end_in_the_row_and_in_the_pane() {
    let mut peer = row("nexo:p1", "work2", State::Idle { min: 1 });
    peer.cwd = Some("/tmp/a/very/long/folder/that/goes/on/and/on/until/it/cannot/fit/scratchpad/run1/work2".into());
    let mut l = lane("lane:IOS", "IOS", "stopped");
    l.lane.as_mut().unwrap().dir = format!("/srv/{}/run1/work", "deep/".repeat(30));
    let mut v = AgentsView::with_rows(vec![peer, l], NOW, false);
    let theme = Theme::dark();
    for (key, end) in [("nexo:p1", "run1/work2 "), ("lane:IOS", "run1/work ")] {
        select(&mut v, key);
        let text = lines(&paint(&mut v, 170, 48, &theme, Instant::now(), false));
        let at = &text[find(&text, "│ in      ")];
        assert!(at.contains('…') && at.contains(end), "the folder line keeps its end on one line: {at}");
    }
    let text = lines(&paint(&mut v, 170, 48, &theme, Instant::now(), false));
    assert!(text[find(&text, "work2 ")].contains("run1/work2"), "the row keeps the folder's end");
}

#[test]
fn the_mono_theme_draws_no_motion_even_while_rows_work_and_wait() {
    let mut v = AgentsView::with_rows(fleet(), NOW, false);
    let theme = Theme::mono();
    let t0 = Instant::now();
    // the app draws mono with the motion off
    let a = paint(&mut v, 170, 48, &theme, t0, false);
    let b = paint(&mut v, 170, 48, &theme, t0 + Duration::from_millis(700), false);
    assert_eq!(a, b);
    assert!(!v.animating(t0 + Duration::from_millis(800)));
}

#[test]
fn a_click_selects_a_line_and_a_second_click_on_it_is_enter() {
    let mut v = AgentsView::with_rows(fleet(), NOW, true);
    paint(&mut v, 170, 48, &Theme::dark(), Instant::now(), false);
    let (i, r) = v.line_rects().into_iter().find(|(_, r)| r.y > 6).unwrap();
    assert!(matches!(v.click(r.x + 3, r.y), ModalOut::Nothing));
    let key = v.selected_key();
    assert!(key.is_some(), "line {i} selects a row");
    let second = v.click(r.x + 3, r.y);
    assert!(!matches!(second, ModalOut::Close), "a double click acts on the row, it never closes the panel");
}

// ------------------------------------------------------------------ lane keys on temporary folders

static ENV: Mutex<()> = Mutex::new(());

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

static STARTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Records the lane instead of starting a runner; the pid it gives is no live process.
fn stand_in(name: &str) -> std::io::Result<u32> {
    STARTED.lock().unwrap().push(name.to_string());
    Ok(u32::MAX - 7)
}

fn lanes_view() -> AgentsView {
    let rows = vec![row("s1", "paneltry", State::Working), lane("lane:WINDOWS", "WINDOWS", "stopped"), {
        let mut r = lane("lane:IOS", "IOS", "stopped");
        r.lane.as_mut().unwrap().runner = false;
        r
    }];
    AgentsView::with_rows(rows, NOW, true).with_starters(stand_in, stand_in)
}

#[test]
fn s_on_a_lane_starts_its_runner_and_on_a_session_starts_nothing() {
    let lab = Lab::new(Some(LANES));
    STARTED.lock().unwrap().clear();
    let mut v = lanes_view();
    select(&mut v, "s1");
    press(&mut v, KeyCode::Char('s'));
    assert!(STARTED.lock().unwrap().is_empty(), "s on a session starts no lane");
    select(&mut v, "lane:WINDOWS");
    press(&mut v, KeyCode::Char('s'));
    assert_eq!(*STARTED.lock().unwrap(), ["WINDOWS"]);
    assert_eq!(v.said(), Some("started WINDOWS"));
    let state = std::fs::read_to_string(lab.team("WINDOWS.json")).unwrap();
    assert!(state.contains("\"pid\"") && state.contains("\"idle\""), "{state}");
    // a held lane is left alone, and so is every lane while they are paused
    nexo_core::team::set_marker(&nexo_core::team::state::hold_file("IOS"), true).unwrap();
    select(&mut v, "lane:IOS");
    press(&mut v, KeyCode::Char('s'));
    assert_eq!(v.said(), Some("IOS is held: press H to unhold it"));
    press(&mut v, KeyCode::Char('H'));
    assert_eq!(v.said(), Some("unheld: IOS"));
    press(&mut v, KeyCode::Char('p'));
    assert!(lab.team("paused").exists());
    press(&mut v, KeyCode::Char('S'));
    assert_eq!(v.said(), Some("all lanes are paused: press p"));
    assert_eq!(STARTED.lock().unwrap().len(), 1);
    press(&mut v, KeyCode::Char('p'));
    assert!(!lab.team("paused").exists());
}

#[test]
fn x_asks_nothing_of_a_lane_that_is_not_running_and_o_resumes_a_stopped_lane_at_once() {
    let lab = Lab::new(Some(LANES));
    lab.write_state("WINDOWS", "{\"state\":\"stopped\",\"session_id\":\"s-1\"}");
    let mut v = lanes_view();
    select(&mut v, "lane:WINDOWS");
    press(&mut v, KeyCode::Char('x'));
    assert_eq!(v.said(), Some("WINDOWS is not running"));
    assert!(!lab.team("WINDOWS.stop").exists(), "no stop file is left for the next runner to find");
    match press(&mut v, KeyCode::Char('o')) {
        ModalOut::SendAndClose(AgentCommand::Resume { id }) => assert_eq!(id, "s-1"),
        _ => panic!("o on a stopped lane resumes its session"),
    }
    select(&mut v, "lane:IOS");
    assert!(matches!(press(&mut v, KeyCode::Char('o')), ModalOut::Nothing));
    assert_eq!(v.said(), Some("IOS has no session yet: press s to start it"));
}

#[test]
fn a_asks_the_questions_of_a_new_lane_in_the_pane_and_writes_it_to_team_toml() {
    let lab = Lab::new(None);
    let mut v = lanes_view();
    select(&mut v, "lane:WINDOWS");
    press(&mut v, KeyCode::Char('a'));
    let theme = Theme::dark();
    let screen = |v: &mut AgentsView| lines(&paint(v, 170, 48, &theme, Instant::now(), false)).join("\n");
    assert!(screen(&mut v).contains("question 1 of"), "{}", screen(&mut v));
    let type_in = |v: &mut AgentsView, s: &str| {
        for c in s.chars() {
            press(v, KeyCode::Char(c));
        }
        press(v, KeyCode::Enter);
    };
    // a name with a space is refused and asked again
    type_in(&mut v, "my lane");
    assert!(screen(&mut v).contains("question 1 of"), "{}", screen(&mut v));
    for _ in 0..7 {
        press(&mut v, KeyCode::Backspace);
    }
    type_in(&mut v, "DOCS");
    type_in(&mut v, "/tmp");
    type_in(&mut v, "Tidy the docs.");
    type_in(&mut v, "1h");
    type_in(&mut v, "");
    assert!(!lab.dir.path().join("team.toml").exists(), "nothing is written before the last answer");
    type_in(&mut v, "");
    assert_eq!(v.said(), Some("added DOCS: press s to start it"));
    let file = std::fs::read_to_string(lab.dir.path().join("team.toml")).unwrap();
    assert!(file.contains("name = \"DOCS\"") && file.contains("every = \"1h\""), "{file}");
    // Esc in the questions gives them up, and the panel stays
    press(&mut v, KeyCode::Char('a'));
    press(&mut v, KeyCode::Char('D'));
    assert!(matches!(press(&mut v, KeyCode::Esc), ModalOut::Nothing));
    assert!(!screen(&mut v).contains("question 1 of"));
}
