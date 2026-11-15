//! `/panel` keys and pointer inside the app: the selection following its session, a message that survives Esc
//! and a changed fleet, the wheel, clicks outside and the key hints at narrow widths. The rows are other nexo
//! sessions, which take a message through the mailbox and need no supervisor.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_agent::supervise::{AgentRow, SourceKind, State};
use nexo_tui::agents_view::AgentsView;
use nexo_tui::modal::{Modal, ModalOut};
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

const NOW: i64 = 1_000_000;

fn app() -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        providers: vec![],
        theme: "dark".into(),
        mouse: true,
        show_reasoning: true,
        timestamps: false,
        fold_after: 4,
        group_tools: true,
        complete_words: true,
        suggest: false,
        hints: true,
        keys: Default::default(),
        notify_after: 15,
        animations: false,
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::Galaxy,
        home_stars: true,
        bursts: true,
        todo_strip: false,
        status_line: vec![],
        status_command: String::new(),
        vim: false,
        prompt: None,
        session_id: Some("20261102-100000-ef56".into()),
        sessions: vec![],
        skills: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}


/// Another live nexo session named `name`.
fn job(name: &str) -> AgentRow {
    AgentRow {
        key: format!("nexo:{name}"),
        name: name.into(),
        source: SourceKind::Nexo,
        program: "nexo".into(),
        state: State::Working,
        since: NOW - 60,
        last_step: None,
        last_conclusion: None,
        idle_secs: Some(3),
        cost_usd: None,
        model: None,
        cwd: None,
        permission_mode: None,
        parent: None,
        paused: false,
        last_action: None,
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

fn jobs(n: usize) -> Vec<AgentRow> {
    (0..n).map(|i| job(&format!("job{i:03}"))).collect()
}

fn scene(rows: Vec<AgentRow>) -> App {
    let mut a = app();
    a.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(rows, NOW, true))));
    a
}

fn capture(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer())
}

fn view(a: &mut App) -> &mut AgentsView {
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!("no panel") };
    v
}

fn press(a: &mut App, code: KeyCode) -> ModalOut {
    view(a).key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn type_in(a: &mut App, text: &str) {
    for c in text.chars() {
        press(a, KeyCode::Char(c));
    }
}

fn live_panel() -> (tempfile::TempDir, std::sync::Arc<nexo_agent::supervise::Panel>) {
    use nexo_agent::supervise::{NexoSource, Panel, Policy, Setup};
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs").join("proj");
    std::fs::create_dir_all(&logs).unwrap();
    let setup = Setup {
        logs_root: Some(dir.path().join("logs")),
        scan_hours: 12,
        list_cmd: None,
        list_every: 10,
        poll_every_ms: 100,
    };
    let p = Panel::start(&setup, Policy::default(), NexoSource::default());
    (dir, std::sync::Arc::new(p))
}

/// A view with a live supervisor but rows of our own (the poller finds nothing in the empty logs).
fn supervised(rows: Vec<AgentRow>) -> (tempfile::TempDir, App) {
    let (d, panel) = live_panel();
    let mut v = AgentsView::new(Some(panel), true);
    v.set_rows(rows);
    let mut a = app();
    a.modal = Some(Modal::Agents(Box::new(v)));
    (d, a)
}

fn text(a: &mut App, w: u16, h: u16) -> String {
    capture(a, w, h).join("\n")
}


#[test]
fn the_selection_follows_its_session_when_the_others_come_and_go() {
    let mut a = scene(jobs(4));
    capture(&mut a, 120, 40);
    while view(&mut a).selected_key().as_deref() != Some("nexo:job002") {
        press(&mut a, KeyCode::Down);
    }
    // a session sorting ahead of it appears and one before it disappears: still job002
    let mut now = jobs(4);
    now.remove(0);
    now.insert(1, job("job-new"));
    now.insert(0, job("job-newer"));
    view(&mut a).set_rows(now);
    assert_eq!(view(&mut a).selected_key().as_deref(), Some("nexo:job002"));
    // the selected session goes away: the selection stays in range, on a neighbour
    let rest: Vec<AgentRow> = jobs(4).into_iter().filter(|r| r.key != "nexo:job002").collect();
    view(&mut a).set_rows(rest);
    assert!(view(&mut a).selected_key().is_some());
    view(&mut a).set_rows(vec![]);
    assert_eq!(view(&mut a).selected_key(), None);
}

#[test]
fn page_keys_move_by_what_the_window_shows_and_stop_at_the_ends() {
    let mut a = scene(jobs(60));
    capture(&mut a, 80, 24);
    // the cursor starts on the first session, under its group's header
    let start = view(&mut a).selected;
    press(&mut a, KeyCode::PageDown);
    let first = view(&mut a).selected;
    assert!(first >= start + 2, "a page moves at least a row: {first}");
    capture(&mut a, 80, 24);
    press(&mut a, KeyCode::PageUp);
    assert_eq!(view(&mut a).selected, start);
    press(&mut a, KeyCode::Up);
    press(&mut a, KeyCode::Up);
    assert_eq!(view(&mut a).selected, 0, "the arrows stop at the top");
    press(&mut a, KeyCode::End);
    let last = view(&mut a).selected;
    press(&mut a, KeyCode::Down);
    assert_eq!(view(&mut a).selected, last, "and at the bottom");
}

#[test]
fn with_no_sessions_every_key_is_harmless() {
    let mut a = scene(vec![]);
    capture(&mut a, 80, 24);
    for c in [KeyCode::Down, KeyCode::Right, KeyCode::Left, KeyCode::PageDown, KeyCode::End, KeyCode::Enter] {
        assert_eq!(press(&mut a, c), ModalOut::Nothing);
    }
    for c in ['n', 's', 'x', 'o', 'H', 'k', 'z'] {
        assert_eq!(press(&mut a, KeyCode::Char(c)), ModalOut::Nothing, "{c}");
    }
    assert_eq!(press(&mut a, KeyCode::Char('q')), ModalOut::Close);
}

#[test]
fn escape_puts_a_half_written_message_aside_and_n_picks_it_up() {
    let mut a = scene(vec![job("alpha")]);
    capture(&mut a, 120, 30);
    press(&mut a, KeyCode::Char('n'));
    type_in(&mut a, "check the migration");
    let shown = text(&mut a, 120, 30);
    assert!(shown.contains("message › check the migration"), "new messages start empty: {shown}");
    assert_eq!(press(&mut a, KeyCode::Esc), ModalOut::Nothing, "Esc backs out of the field only");
    let shown = text(&mut a, 120, 30);
    assert!(!shown.contains("message ›") && shown.contains("message kept"), "{shown}");
    press(&mut a, KeyCode::Char('n'));
    let shown = text(&mut a, 120, 30);
    assert!(shown.contains("message › check the migration"), "{shown}");
}

#[test]
fn a_message_goes_to_the_session_it_was_started_for_even_if_the_fleet_changes() {
    let mut a = scene(vec![job("alpha"), job("beta")]);
    capture(&mut a, 120, 30);
    press(&mut a, KeyCode::Char('n'));
    type_in(&mut a, "x");
    // the session it is for disappears: Enter keeps the field open and says so
    view(&mut a).set_rows(vec![job("beta")]);
    assert_eq!(press(&mut a, KeyCode::Enter), ModalOut::Nothing);
    let shown = text(&mut a, 120, 30);
    assert!(shown.contains("message ›") && shown.contains("session gone"), "{shown}");
    // Esc still backs out
    press(&mut a, KeyCode::Esc);
    assert!(!text(&mut a, 120, 30).contains("session gone"));
}

#[test]
fn a_long_message_shows_its_end_and_the_hint_gives_way_at_40_columns() {
    let mut a = scene(vec![job("alpha")]);
    capture(&mut a, 40, 12);
    press(&mut a, KeyCode::Char('n'));
    view(&mut a).key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    type_in(&mut a, "a very long message that does not fit the narrow window ENDMARK");
    let shown = text(&mut a, 40, 12);
    assert!(shown.contains("ENDMARK▏"), "the typing end is in sight\n{shown}");
    assert!(shown.contains("message ›"), "{shown}");
}

#[test]
fn a_click_outside_does_not_throw_away_a_message_being_written() {
    let mut a = scene(vec![job("alpha")]);
    // a window smaller than the screen, so a click can land outside it
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    t.draw(|f| view(&mut a).draw(f, ratatui::layout::Rect::new(4, 2, 100, 24), &nexo_tui::theme::Theme::dark(), std::time::Instant::now(), false)).unwrap();
    press(&mut a, KeyCode::Char('n'));
    type_in(&mut a, "hi");
    assert_eq!(view(&mut a).click(0, 0), ModalOut::Nothing);
    // with nothing being written, outside closes like the other dialogs
    press(&mut a, KeyCode::Esc);
    assert_eq!(view(&mut a).click(0, 0), ModalOut::Close);
}

#[test]
fn the_wheel_scrolls_the_tree_and_the_selection_stays_put() {
    let mut a = scene(jobs(80));
    capture(&mut a, 80, 24);
    assert_eq!(view(&mut a).scroll, 0);
    let at = view(&mut a).selected;
    for _ in 0..5 {
        view(&mut a).scroll_by(3);
    }
    capture(&mut a, 80, 24);
    assert!(view(&mut a).scroll >= 3, "the view moved: {}", view(&mut a).scroll);
    assert_eq!(view(&mut a).selected, at, "the selection did not move");
    let shown = text(&mut a, 80, 24);
    assert!(shown.contains("↑ "), "the marker says there is more above\n{shown}");
    let before = view(&mut a).scroll;
    view(&mut a).scroll_by(-3);
    capture(&mut a, 80, 24);
    assert!(view(&mut a).scroll < before);
    // a key brings the selection back into sight
    view(&mut a).scroll_by(30);
    capture(&mut a, 80, 24);
    press(&mut a, KeyCode::Down);
    capture(&mut a, 80, 24);
    let key = view(&mut a).selected_key().unwrap();
    let want = key.strip_prefix("nexo:").unwrap().to_string();
    assert!(text(&mut a, 80, 24).contains(&want), "the selected session is back in sight");
}

#[test]
fn the_wheel_on_the_policy_page_scrolls_the_page_not_the_sessions() {
    let (_d, mut a) = supervised(jobs(3));
    capture(&mut a, 120, 12);
    press(&mut a, KeyCode::Char('P'));
    capture(&mut a, 120, 12);
    let tree_at = view(&mut a).scroll;
    view(&mut a).scroll_by(3);
    assert_eq!(view(&mut a).scroll, tree_at, "the tree scroll is untouched");
    assert_eq!(view(&mut a).policy_scroll(), 3);
}

#[test]
fn the_key_hint_fits_and_keeps_esc_at_every_width() {
    for w in [30u16, 36, 40, 50, 60, 80, 120] {
        let mut a = scene(jobs(3));
        let shown = capture(&mut a, w, 14);
        assert!(shown.iter().any(|l| l.contains("Esc")), "no Esc hint at {w}\n{shown:#?}");
    }
    let mut a = scene(jobs(3));
    assert!(text(&mut a, 120, 24).contains("⏎ observe"), "Enter is listed");
    let mut a = scene(jobs(3));
    assert!(text(&mut a, 90, 24).contains("⏎ details"), "under 110 columns Enter opens the pane");
}
