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

fn job(key: &str, source: SourceKind) -> AgentRow {
    AgentRow {
        key: key.into(),
        name: key.into(),
        source,
        program: if source == SourceKind::Nexo { "nexo" } else { "agentx" }.into(),
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
    }
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

#[test]
fn enter_observes_the_selected_session_and_escape_returns() {
    use nexo_agent::supervise::{Ev, EvDetail, EvKind};
    let mut row = job("alpha", SourceKind::Nexo);
    row.last_step = Some("reading code".into());
    row.ribbon = vec![Ev::with(NOW, EvKind::Read, EvDetail::Text("src/main.rs".into()))];
    let mut a = scene(vec![row, job("beta", SourceKind::External)]);
    capture(&mut a, 100, 25);
    assert_eq!(press(&mut a, KeyCode::Enter), ModalOut::Nothing);
    let shown = capture(&mut a, 100, 25).join("\n");
    assert!(
        shown.contains("observation") && shown.contains("reading code") && shown.contains("src/main.rs"),
        "{shown}"
    );
    view(&mut a).set_rows(vec![job("beta", SourceKind::External)]);
    let shown = capture(&mut a, 100, 25).join("\n");
    assert!(shown.contains("Session no longer available"), "{shown}");
    assert_eq!(press(&mut a, KeyCode::Esc), ModalOut::Nothing);
    assert_eq!(press(&mut a, KeyCode::Esc), ModalOut::Close);
}

#[test]
fn observation_message_stays_with_its_target_when_selection_changes() {
    use nexo_agent::supervise::{NexoSource, Panel, Policy, Setup};
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs/proj");
    std::fs::create_dir_all(&logs).unwrap();
    let record = r#"{"type":"assistant","cwd":"/work/app","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Done."}]}}"#;
    for name in ["alpha", "beta"] {
        std::fs::write(logs.join(format!("{name}.jsonl")), format!("{record}\n")).unwrap();
    }
    let setup = Setup {
        logs_root: Some(dir.path().join("logs")),
        scan_hours: 12,
        list_cmd: None,
        list_every: 10,
        poll_every_ms: 100,
    };
    let panel = Arc::new(Panel::start(&setup, Policy::default(), NexoSource::default()));
    let mut v = AgentsView::new(Some(panel.clone()), true);
    let deadline = Instant::now() + Duration::from_secs(60);
    while v.rows().len() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        v.refresh();
    }
    assert_eq!(v.rows().len(), 2);
    let target = v.selected_key().unwrap();
    let other = v.rows().iter().find(|r| r.key != target).unwrap().key.clone();
    let mut a = app();
    a.modal = Some(Modal::Agents(Box::new(v)));
    let shown = capture(&mut a, 140, 30).join("\n");
    assert!(shown.contains("Enter observe"), "{shown}");
    press(&mut a, KeyCode::Enter);
    press(&mut a, KeyCode::Char('n'));
    let shown = capture(&mut a, 140, 30).join("\n");
    assert!(shown.contains("message › ▏"), "new message must be empty: {shown}");
    press(&mut a, KeyCode::Enter);
    assert!(
        panel
            .rows(nexo_agent::supervise::run::now_secs())
            .iter()
            .all(|r| { r.last_action.as_ref().is_none_or(|action| action.0 != "your nudge is queued") }),
        "an empty message must not be queued"
    );
    press(&mut a, KeyCode::Char('n'));
    for c in "check this".chars() {
        press(&mut a, KeyCode::Char(c));
    }
    let other_index = view(&mut a).rows().iter().position(|r| r.key == other).unwrap();
    view(&mut a).selected = other_index;
    press(&mut a, KeyCode::Enter);
    let rows = panel.rows(nexo_agent::supervise::run::now_secs());
    let action =
        |key: &str| rows.iter().find(|r| r.key == key).and_then(|r| r.last_action.as_ref()).map(|a| a.0.as_str());
    assert_eq!(action(&target), Some("your nudge is queued"));
    assert_ne!(action(&other), Some("your nudge is queued"));
}

#[test]
fn observation_does_not_message_a_replacement_selection() {
    let mut a = scene(vec![job("alpha", SourceKind::Nexo), job("beta", SourceKind::Nexo)]);
    capture(&mut a, 120, 30);
    press(&mut a, KeyCode::Enter);
    view(&mut a).set_rows(vec![job("beta", SourceKind::Nexo)]);
    press(&mut a, KeyCode::Char('n'));
    let shown = capture(&mut a, 120, 30).join("\n");
    assert!(!shown.contains("message ›"), "{shown}");
    assert!(shown.contains("no message sent"), "{shown}");
}

#[test]
fn persisted_session_reaches_observation() {
    use nexo_agent::supervise::{NexoSource, Panel, Policy, Setup};
    use std::time::{Duration, Instant};
    if std::env::var_os("NEXO_PANEL_HISTORY_CHILD").is_none() {
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "panel_observe::persisted_session_reaches_observation", "--nocapture"])
            .env("NEXO_PANEL_HISTORY_CHILD", "1")
            .env("NEXO_DATA_DIR", dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let mut session = nexo_agent::session::Session::create(std::path::Path::new("/tmp"), "test", "test").unwrap();
    session.append(&nexo_core::message::Message::user("history user marker")).unwrap();
    session.append(&nexo_core::message::Message::assistant("history assistant marker")).unwrap();
    session.record_output("call", &nexo_core::tool::ToolOutput::ok("tool output marker")).unwrap();
    let id = session.meta().id.clone();
    let panel = std::sync::Arc::new(Panel::start(&Setup::none(), Policy::default(), NexoSource::default()));
    let mut v = AgentsView::new(Some(panel), true);
    v.set_rows(vec![job(&id, SourceKind::Nexo)]);
    let mut a = app();
    a.modal = Some(Modal::Agents(Box::new(v)));
    press(&mut a, KeyCode::Enter);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        view(&mut a).refresh();
        view(&mut a).set_rows(vec![job(&id, SourceKind::Nexo)]);
        let shown = capture(&mut a, 160, 40).join("\n");
        if shown.contains("history assistant marker") {
            assert!(shown.contains("User: history user marker"), "{shown}");
            assert!(shown.contains("Tool output: tool output marker"), "{shown}");
            assert!(!shown.contains("\"role\""), "{shown}");
            break;
        }
        assert!(Instant::now() < deadline, "{shown}");
        std::thread::sleep(Duration::from_millis(30));
    }
    session.append(&nexo_core::message::Message::user("x".repeat(300_000))).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        view(&mut a).refresh();
        view(&mut a).set_rows(vec![job(&id, SourceKind::Nexo)]);
        let shown = capture(&mut a, 160, 40).join("\n");
        if shown.contains("record crossing the 256 KiB limit was omitted entirely") {
            assert!(!shown.contains("No saved messages in this window"), "{shown}");
            break;
        }
        assert!(Instant::now() < deadline, "{shown}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn end_follows_new_observation_rows_until_manual_scroll() {
    use nexo_agent::supervise::{Ev, EvDetail, EvKind};
    let make_row = |count: usize| {
        let mut row = job("alpha", SourceKind::External);
        row.ribbon =
            (0..count).map(|i| Ev::with(NOW, EvKind::Read, EvDetail::Text(format!("event-marker-{i}")))).collect();
        row
    };
    let mut a = scene(vec![make_row(8)]);
    press(&mut a, KeyCode::Enter);
    press(&mut a, KeyCode::End);
    let first = capture(&mut a, 100, 12).join("\n");
    assert!(first.contains("event-marker-7"), "{first}");
    view(&mut a).set_rows(vec![make_row(10)]);
    let grown = capture(&mut a, 100, 12).join("\n");
    assert!(grown.contains("event-marker-9"), "{grown}");
    press(&mut a, KeyCode::PageUp);
    capture(&mut a, 100, 12);
    view(&mut a).set_rows(vec![make_row(12)]);
    let stopped = capture(&mut a, 100, 12).join("\n");
    assert!(!stopped.contains("event-marker-11"), "{stopped}");
}
