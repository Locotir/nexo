//! The message field of `/panel` (`n`): what the person types stays in sight, can be cleared and pasted into.

use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use nexo_agent::supervise::{NexoSource, Panel, Policy, Setup};
use nexo_tui::agents_view::AgentsView;
use nexo_tui::modal::Modal;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

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

fn live_panel() -> (tempfile::TempDir, Arc<Panel>) {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs").join("proj");
    std::fs::create_dir_all(&logs).unwrap();
    // a record from now: older finished sessions leave the panel
    let now = time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap();
    let rec = format!(
        "{}{now}{}",
        r#"{"type":"assistant","timestamp":""#,
        r#"","cwd":"/work/app","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Step one is done."}]}}"#
    );
    std::fs::write(logs.join("sess-1.jsonl"), format!("{rec}\n")).unwrap();
    let setup = Setup {
        logs_root: Some(dir.path().join("logs")),
        scan_hours: 12,
        list_cmd: None,
        list_every: 10,
        poll_every_ms: 100,
    };
    (dir, Arc::new(Panel::start(&setup, Policy::default(), NexoSource::default())))
}

/// A panel window with one session, the message field open.
fn writing() -> (tempfile::TempDir, Arc<Panel>, App) {
    let (d, panel) = live_panel();
    let t = std::time::Instant::now();
    let mut v = AgentsView::new(Some(panel.clone()), true);
    while v.rows().is_empty() && t.elapsed().as_secs() < 6 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        v.refresh();
    }
    assert_eq!(v.rows().len(), 1);
    let mut a = app();
    a.modal = Some(Modal::Agents(Box::new(v)));
    press(&mut a, KeyCode::Char('n'), KeyModifiers::NONE);
    (d, panel, a)
}

fn press(a: &mut App, code: KeyCode, mods: KeyModifiers) {
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!("the panel is closed") };
    v.key(KeyEvent::new(code, mods));
}

fn type_in(a: &mut App, text: &str) {
    for c in text.chars() {
        press(a, KeyCode::Char(c), KeyModifiers::NONE);
    }
}

/// The footer line of the window, where the field is.
fn field(a: &mut App, w: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, 30)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer()).into_iter().find(|l| l.contains("message ›")).unwrap_or_default()
}

#[test]
fn a_long_message_keeps_its_end_and_the_cursor_in_sight() {
    let (_d, _p, mut a) = writing();
    press(&mut a, KeyCode::Char('u'), KeyModifiers::CONTROL);
    type_in(&mut a, &format!("{} the very end", "word ".repeat(60)));
    let line = field(&mut a, 100);
    assert!(line.contains("the very end▏"), "the end of what was typed is hidden: {line}");
    assert!(line.contains("Enter send · Esc cancel"), "the hint is cut: {line}");
    let line = field(&mut a, 50);
    assert!(line.contains("Esc cancel") && line.contains("end▏"), "{line}");
}

#[test]
fn ctrl_u_clears_the_field_and_ctrl_w_takes_the_last_word() {
    let (_d, _p, mut a) = writing();
    assert!(field(&mut a, 120).len() > "message ›".len(), "the field starts from the suggested text");
    press(&mut a, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert!(field(&mut a, 120).contains("message › ▏"), "{}", field(&mut a, 120));
    type_in(&mut a, "check the tests now");
    press(&mut a, KeyCode::Char('w'), KeyModifiers::CONTROL);
    assert!(field(&mut a, 120).contains("message › check the tests ▏"), "{}", field(&mut a, 120));
    press(&mut a, KeyCode::Char('w'), KeyModifiers::CONTROL);
    press(&mut a, KeyCode::Char('w'), KeyModifiers::CONTROL);
    press(&mut a, KeyCode::Char('w'), KeyModifiers::CONTROL);
    press(&mut a, KeyCode::Char('w'), KeyModifiers::CONTROL);
    assert!(field(&mut a, 120).contains("message › ▏"), "{}", field(&mut a, 120));
}

#[test]
fn a_paste_lands_in_the_field_on_one_line_and_is_dropped_elsewhere() {
    let (_d, panel, mut a) = writing();
    press(&mut a, KeyCode::Char('u'), KeyModifiers::CONTROL);
    nexo_tui::event::handle(&mut a, Event::Paste("first line\r\nsecond\tline\n".into()));
    assert!(field(&mut a, 120).contains("message › first line second line ▏"), "{}", field(&mut a, 120));
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    let row = panel.rows(nexo_agent::supervise::run::now_secs()).remove(0);
    assert_eq!(row.last_action.as_ref().map(|a| a.0.as_str()), Some("your nudge is queued"));
}

#[test]
fn n_and_p_with_no_session_in_the_list_say_so() {
    let panel = Arc::new(Panel::start(&Setup::none(), Policy::default(), NexoSource::default()));
    for keys in ['n', 'p'] {
        let v = AgentsView::new(Some(panel.clone()), true);
        assert!(v.rows().is_empty());
        let mut a = app();
        a.modal = Some(Modal::Agents(Box::new(v)));
        press(&mut a, KeyCode::Char(keys), KeyModifiers::NONE);
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        draw_frame(&mut t, &mut a).unwrap();
        let text = rows_of(t.backend().buffer()).join("\n");
        let said = if keys == 'n' { "no session selected" } else { "no session to pause" };
        assert!(text.contains(said), "{keys}: silent\n{text}");
        assert!(!text.contains("message ›"), "{keys}: a field opened on nothing");
    }
}
