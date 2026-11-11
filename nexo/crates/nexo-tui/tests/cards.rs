//! Tool cards fold and unfold (Ctrl+O), each with its status, its duration and a one-line summary
//! when folded. Checked on the screen at 80x24 and 60x16.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_core::agent::AgentEvent;
use nexo_core::tool::ToolOutput;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, event, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app() -> App {
    app_with(false)
}

fn app_with(group_tools: bool) -> App {
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
        fold_after: 3,
        group_tools,
        complete_words: true,
        suggest: false,
        keys: Default::default(),
        status_line: vec![],
        status_command: String::new(),
        notify_after: 15,
        animations: false,
        vim: false,
        prompt: None,
        session_id: Some("20261102-100000-ef56".into()),
        sessions: vec![],
        skills: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    let out = (1..=12).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
    for (id, summary, ok, ms) in [("c1", "bash: cargo test", true, 1500u64), ("c2", "bash: false", false, 40)] {
        a.handle_agent(AgentEvent::ToolCall {
            id: id.into(),
            name: "bash".into(),
            args: serde_json::json!({ "command": summary }),
            summary: summary.into(),
        });
        a.handle_agent(AgentEvent::ToolStart { id: id.into() });
        let o = if ok { ToolOutput::ok(out.clone()) } else { ToolOutput::error(out.clone()) };
        a.handle_agent(AgentEvent::ToolDone { id: id.into(), output: o, elapsed_ms: ms });
    }
    a
}

fn screen(a: &mut App, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    let rows = rows_of(t.backend().buffer());
    assert_eq!(rows.len(), h as usize);
    rows.join("\n")
}

/// The cards only: the hints row (flash message) and the status rule with the running timer
/// change on their own, so they are left out of the comparison.
fn cards(screen: &str) -> String {
    let rows: Vec<&str> = screen.lines().collect();
    let hints = rows.len().saturating_sub(2);
    rows.iter()
        .enumerate()
        .filter(|(i, l)| *i != hints && !l.starts_with('─'))
        .map(|(_, l)| *l)
        .collect::<Vec<_>>()
        .join("\n")
}

fn ctrl_o(a: &mut App) {
    event::key(a, KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
}

#[test]
fn cards_fold_and_unfold_with_status_duration_and_summary_at_two_sizes() {
    for (w, h) in [(80u16, 24u16), (60, 16)] {
        let mut a = app();
        let folded = cards(&screen(&mut a, w, h));
        assert!(folded.contains("cargo test"), "{w}x{h} summary lost:\n{folded}");
        assert!(folded.contains("1.5s"), "{w}x{h} duration lost:\n{folded}");
        assert!(folded.contains("✓") || folded.contains("ok"), "{w}x{h} ok state lost:\n{folded}");
        assert!(folded.contains("✗"), "{w}x{h} error state lost:\n{folded}");
        assert!(!folded.contains("line12"), "{w}x{h} the folded card shows the whole output:\n{folded}");
        // Ctrl+O toggles every body: whatever it does, twice brings the screen back
        ctrl_o(&mut a);
        let once = cards(&screen(&mut a, w, h));
        ctrl_o(&mut a);
        let twice = cards(&screen(&mut a, w, h));
        assert_ne!(folded, once, "{w}x{h} Ctrl+O changed nothing");
        assert!(once.contains("line12"), "{w}x{h} the opened card does not show its whole output:\n{once}");
        assert_eq!(folded, twice, "{w}x{h} Ctrl+O twice is not the start");
    }
}

fn press(a: &mut App, code: KeyCode, mods: KeyModifiers) {
    event::key(a, KeyEvent::new(code, mods));
}

#[test]
fn a_picked_block_takes_the_arrows_even_over_a_draft() {
    let mut a = app_with(true);
    a.input.set_text("half a thought");
    screen(&mut a, 80, 24);
    // a click picks a block (here the last one, as Shift+↑ does on an empty draft); ↑ walks to the group
    a.select_last();
    while a
        .selected
        .is_some_and(|_| !screen(&mut a, 80, 24).lines().any(|l| l.contains("2 shell commands") && l.starts_with('┃')))
    {
        let before = a.selected;
        press(&mut a, KeyCode::Up, KeyModifiers::NONE);
        if a.selected == before {
            break;
        }
    }
    let shown = screen(&mut a, 80, 24);
    assert!(shown.contains("2 shell commands") && !shown.contains("cargo test ·"), "{shown}");
    // → opens the group: both calls show, folded; ↓ steps onto the first, → shows its output
    press(&mut a, KeyCode::Right, KeyModifiers::NONE);
    let open = screen(&mut a, 80, 24);
    assert!(open.contains("cargo test") && open.contains("false"), "{open}");
    press(&mut a, KeyCode::Down, KeyModifiers::NONE);
    press(&mut a, KeyCode::Right, KeyModifiers::NONE);
    let out = screen(&mut a, 80, 24);
    assert!(out.contains("line1"), "{out}");
    // ← folds it again, and the draft was never touched
    press(&mut a, KeyCode::Left, KeyModifiers::NONE);
    assert!(screen(&mut a, 80, 24).matches("line1").count() < out.matches("line1").count());
    assert_eq!(a.input.text(), "half a thought");
}

#[test]
fn the_blocks_hint_shows_only_when_there_is_a_block_to_step_into() {
    let hints = |a: &mut App, w: u16, h: u16| {
        screen(a, w, h)
            .lines()
            .find(|l| l.contains("Enter send") || l.contains("Esc interrupt"))
            .unwrap_or_default()
            .to_string()
    };
    for (w, h) in [(120u16, 40u16), (200, 40)] {
        // an empty transcript: Shift+↑ would only say "nothing to select yet"
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut empty = App::new(TuiOptions { animations: false, ..TuiOptions::default() }, tx);
        empty.root = false;
        empty.unlock();
        let line = hints(&mut empty, w, h);
        assert!(line.contains("Enter send") && !line.contains("Shift+↑"), "{w}x{h}: {line}");
        // with the cards of a turn (it still runs: its own line of keys) the hint is back
        let mut full = app_with(true);
        let all = screen(&mut full, w, h);
        assert!(all.contains("Shift+↑ blocks"), "{w}x{h}: {all}");
    }
}
