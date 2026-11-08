//! `/panel` keys and pointer: movement over the grid, the selection following its session, a message that
//! survives Esc and a changed fleet, the wheel, clicks and the key hints at narrow widths.

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
        keys: Default::default(),
        notify_after: 15,
        animations: false,
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

fn job(key: &str, source: SourceKind) -> AgentRow {
    AgentRow {
        key: key.into(),
        name: key.into(),
        source,
        program: if source == SourceKind::Nexo { "nexo" } else { "agentx" }.into(),
        state: State::Working,
        since: NOW - 60,
        last_step: None,
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
    }
}

fn jobs(n: usize) -> Vec<AgentRow> {
    (0..n).map(|i| job(&format!("job{i:03}"), SourceKind::External)).collect()
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
    press(&mut a, KeyCode::Right);
    press(&mut a, KeyCode::Right);
    assert_eq!(view(&mut a).selected, 2);
    // a session sorting ahead of it appears and one before it disappears: still job002
    let mut now = jobs(4);
    now.remove(0);
    now.insert(1, job("job-new", SourceKind::Nexo));
    now.insert(0, job("job-newer", SourceKind::Nexo));
    view(&mut a).set_rows(now);
    let v = view(&mut a);
    assert_eq!(v.selected_key().as_deref(), Some("job002"));
    // the selected session goes away: the selection stays in range, on a neighbour
    let rest: Vec<AgentRow> = jobs(4).into_iter().filter(|r| r.key != "job002").collect();
    view(&mut a).set_rows(rest);
    assert!(view(&mut a).selected_key().is_some());
    view(&mut a).set_rows(vec![]);
    assert_eq!(view(&mut a).selected, 0);
    assert_eq!(view(&mut a).selected_key(), None);
}

#[test]
fn tab_wraps_round_and_shift_tab_goes_back() {
    let mut a = scene(jobs(5));
    capture(&mut a, 120, 40);
    press(&mut a, KeyCode::BackTab);
    assert_eq!(view(&mut a).selected, 4, "back from the first goes to the last");
    press(&mut a, KeyCode::Tab);
    assert_eq!(view(&mut a).selected, 0, "on from the last goes to the first");
    // the arrows stop at the ends
    press(&mut a, KeyCode::Left);
    assert_eq!(view(&mut a).selected, 0);
    press(&mut a, KeyCode::Char('G'));
    press(&mut a, KeyCode::Right);
    assert_eq!(view(&mut a).selected, 4);
    press(&mut a, KeyCode::Char('g'));
    assert_eq!(view(&mut a).selected, 0);
}

#[test]
fn up_and_down_hold_their_column_across_a_short_row() {
    // 120 wide is three columns; a group of 5 fills its rows evenly: 3, 2 (nexo one-row group before)
    let mut rows = jobs(5);
    rows.insert(0, job("this", SourceKind::Nexo));
    let mut a = scene(rows);
    capture(&mut a, 120, 40);
    // this = 0, jobs = 1..=5: rows [1,2,3] [4,5]
    press(&mut a, KeyCode::Down);
    press(&mut a, KeyCode::Right);
    press(&mut a, KeyCode::Right);
    assert_eq!(view(&mut a).selected, 3, "last column of the first row");
    press(&mut a, KeyCode::Down);
    assert_eq!(view(&mut a).selected, 5, "the short row ends one column sooner");
    press(&mut a, KeyCode::Up);
    assert_eq!(view(&mut a).selected, 3, "back up to the column it came from");
}

#[test]
fn page_keys_move_by_what_the_window_shows() {
    let mut a = scene(jobs(60));
    capture(&mut a, 80, 24);
    press(&mut a, KeyCode::PageDown);
    let first = view(&mut a).selected;
    assert!(first >= 2, "a page moves at least a row: {first}");
    capture(&mut a, 80, 24);
    press(&mut a, KeyCode::PageUp);
    assert_eq!(view(&mut a).selected, 0);
    // one card, no grid to move over: nothing breaks
    let mut one = scene(jobs(1));
    capture(&mut one, 80, 24);
    for c in [KeyCode::PageDown, KeyCode::PageUp, KeyCode::Down, KeyCode::Up, KeyCode::Tab, KeyCode::End] {
        press(&mut one, c);
        assert_eq!(view(&mut one).selected, 0);
    }
}

#[test]
fn with_no_sessions_every_key_is_harmless() {
    let mut a = scene(vec![]);
    capture(&mut a, 80, 24);
    for c in
        [KeyCode::Down, KeyCode::Right, KeyCode::Tab, KeyCode::BackTab, KeyCode::PageDown, KeyCode::End, KeyCode::Enter]
    {
        assert_eq!(press(&mut a, c), ModalOut::Nothing);
    }
    assert_eq!(view(&mut a).selected, 0);
    assert_eq!(press(&mut a, KeyCode::Char('q')), ModalOut::Close);
}

#[test]
fn escape_puts_a_half_written_message_aside_and_n_picks_it_up() {
    let (_d, mut a) = supervised(vec![job("alpha", SourceKind::External)]);
    capture(&mut a, 120, 30);
    press(&mut a, KeyCode::Char('n'));
    // clear the template so the test does not depend on its words
    view(&mut a).key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    type_in(&mut a, "check the migration");
    assert_eq!(press(&mut a, KeyCode::Esc), ModalOut::Nothing, "Esc backs out of the field only");
    let shown = text(&mut a, 120, 30);
    assert!(!shown.contains("message ›") && shown.contains("message kept"), "{shown}");
    press(&mut a, KeyCode::Char('n'));
    let shown = text(&mut a, 120, 30);
    assert!(shown.contains("message › check the migration"), "{shown}");
}

#[test]
fn a_message_goes_to_the_session_it_was_started_for_even_if_the_fleet_changes() {
    let (_d, mut a) = supervised(vec![job("alpha", SourceKind::External), job("beta", SourceKind::External)]);
    capture(&mut a, 120, 30);
    press(&mut a, KeyCode::Char('n'));
    type_in(&mut a, "x");
    // the session it is for disappears: Enter keeps the field open and says so
    view(&mut a).set_rows(vec![job("beta", SourceKind::External)]);
    assert_eq!(press(&mut a, KeyCode::Enter), ModalOut::Nothing);
    let shown = text(&mut a, 120, 30);
    assert!(shown.contains("message ›") && shown.contains("session gone"), "{shown}");
    // Esc still backs out
    press(&mut a, KeyCode::Esc);
    assert!(!text(&mut a, 120, 30).contains("session gone"));
}

#[test]
fn a_long_message_shows_its_end_and_the_hint_gives_way_at_40_columns() {
    let (_d, mut a) = supervised(vec![job("alpha", SourceKind::External)]);
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
    let (_d, mut a) = supervised(vec![job("alpha", SourceKind::External)]);
    capture(&mut a, 120, 30);
    press(&mut a, KeyCode::Char('n'));
    type_in(&mut a, "hi");
    assert_eq!(view(&mut a).click(0, 0), ModalOut::Nothing);
    // with nothing being written, outside closes like the other dialogs
    press(&mut a, KeyCode::Esc);
    capture(&mut a, 120, 30);
    assert_eq!(view(&mut a).click(0, 0), ModalOut::Close);
}

#[test]
fn clicking_a_card_selects_it_and_clicking_it_again_opens_its_details() {
    let mut a = scene(jobs(4));
    capture(&mut a, 120, 40);
    let cards = view(&mut a).card_rects();
    let (i, r) = cards[2];
    assert_eq!(view(&mut a).click(r.x + 1, r.y + 1), ModalOut::Nothing);
    assert_eq!(view(&mut a).selected, i);
    assert!(!view(&mut a).details_open(), "one click only selects");
    view(&mut a).click(r.x + 1, r.y + 1);
    assert!(view(&mut a).details_open());
    view(&mut a).click(r.x + 1, r.y + 1);
    assert!(!view(&mut a).details_open(), "a third click folds them again");
}

#[test]
fn the_wheel_scrolls_the_cards_and_the_selection_stays_put() {
    let mut a = scene(jobs(80));
    capture(&mut a, 80, 24);
    assert_eq!(view(&mut a).scroll, 0);
    for _ in 0..5 {
        view(&mut a).scroll_by(3);
    }
    capture(&mut a, 80, 24);
    assert!(view(&mut a).scroll >= 3, "the view moved: {}", view(&mut a).scroll);
    assert_eq!(view(&mut a).selected, 0, "the selection did not move");
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
    let want = format!("job{:03}", view(&mut a).selected);
    assert!(text(&mut a, 80, 24).contains(&want), "the selected session is back in sight");
}

#[test]
fn the_wheel_on_the_policy_page_scrolls_the_page_not_the_sessions() {
    let (_d, mut a) = supervised(jobs(3));
    capture(&mut a, 120, 12);
    press(&mut a, KeyCode::Char('o'));
    capture(&mut a, 120, 12);
    let cards_at = view(&mut a).scroll;
    view(&mut a).scroll_by(3);
    assert_eq!(view(&mut a).scroll, cards_at, "the card scroll is untouched");
    assert_eq!(view(&mut a).policy_scroll(), 3);
}

#[test]
fn the_key_hint_fits_and_keeps_esc_at_every_width() {
    for w in [30u16, 36, 40, 50, 60, 80, 120] {
        let mut a = scene(jobs(3));
        let shown = capture(&mut a, w, 14);
        let foot = shown.iter().find(|l| l.contains("Esc")).unwrap_or_else(|| panic!("no Esc hint at {w}\n{shown:#?}"));
        assert!(foot.contains("Esc"), "{w}");
    }
    let mut a = scene(jobs(3));
    assert!(text(&mut a, 120, 24).contains("Enter open"), "Enter is listed");
}
