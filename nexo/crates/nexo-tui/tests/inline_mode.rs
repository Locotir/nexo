//! The inline mode: a finished turn is written above the live zone (the terminal's own scrollback) once,
//! in order, and the zone keeps only the draft and the turn in progress.

use std::path::PathBuf;

use nexo_tui::inline::{flush, settled_count, take_settled};
use nexo_tui::render::blocks::Block;
use nexo_tui::{App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};
use tokio::sync::mpsc;

fn app() -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![],
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
        session_id: None,
        sessions: vec![],
        skills: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}

fn turn(a: &mut App, q: &str, r: &str) {
    a.push(Block::User { text: q.into(), at: None });
    a.push(Block::Assistant { text: r.into(), streaming: false, at: None, started: None, took_ms: None });
}

fn text(lines: &[ratatui::text::Line]) -> String {
    lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>().join("\n")
}

#[test]
fn an_idle_session_has_everything_settled() {
    let mut a = app();
    turn(&mut a, "first", "answer one");
    a.busy = false;
    assert_eq!(settled_count(&a), 3, "the banner, then the turn");
}

#[test]
fn a_running_turn_stays_live_from_its_request_on() {
    let mut a = app();
    turn(&mut a, "first", "answer one");
    a.push(Block::User { text: "second".into(), at: None });
    a.push(Block::Assistant { text: "partial".into(), streaming: true, at: None, started: None, took_ms: None });
    a.busy = true;
    assert_eq!(settled_count(&a), 3, "the banner and the first turn");
}

#[test]
fn settled_rows_are_taken_once_and_in_order() {
    let mut a = app();
    turn(&mut a, "first", "answer one");
    a.busy = false;
    let first = text(&take_settled(&mut a, 80));
    assert!(first.contains("first") && first.contains("answer one"));
    assert!(first.find("first") < first.find("answer one"));
    assert!(take_settled(&mut a, 80).is_empty(), "nothing is written twice");
    turn(&mut a, "second", "answer two");
    let next = text(&take_settled(&mut a, 80));
    assert!(next.contains("second") && !next.contains("first"));
}

#[test]
fn flushed_blocks_leave_the_live_layout() {
    let mut a = app();
    turn(&mut a, "first", "answer one");
    a.busy = false;
    a.layout(79);
    assert!(a.total_lines > 0);
    let _ = take_settled(&mut a, 80);
    a.layout(79);
    assert_eq!(a.total_lines, 0, "the live zone no longer draws what is in the scrollback");
}

#[test]
fn flush_writes_above_the_viewport_at_both_sizes() {
    for (w, h) in [(80u16, 24u16), (40, 12)] {
        let backend = TestBackend::new(w, h);
        let mut t = Terminal::with_options(backend, TerminalOptions { viewport: Viewport::Inline(8.min(h)) }).unwrap();
        let mut a = app();
        turn(&mut a, "a question", "an answer");
        a.busy = false;
        flush(&mut t, &mut a).unwrap();
        let shown = format!("{}", t.backend());
        assert!(shown.contains("a question"), "{w}x{h}: {shown}");
        assert_eq!(a.inline_flushed, 3, "the banner goes to the scrollback too");
    }
}

#[test]
fn scrollback_dump_shows_the_summary_line_for_a_folded_group() {
    let mut a = app();
    a.push(Block::User { text: "go".into(), at: None });
    for id in ["a", "b"] {
        a.push(Block::Tool {
            id: id.into(),
            name: "bash".into(),
            summary: format!("bash: run {id}"),
            args: serde_json::Value::Null,
            status: nexo_tui::render::blocks::ToolStatus::Done,
            progress: vec![],
            output: Some(nexo_core::tool::ToolOutput::ok("out")),
            started: None,
            elapsed_ms: None,
            folded: true,
        });
    }
    a.busy = false;
    let dump = nexo_tui::inline::scrollback_text(&mut a, 80);
    assert!(dump.contains("Ran 2 shell commands"), "{dump}");
    assert!(!dump.contains("run a"), "{dump}");
    let rows = take_settled(&mut a, 80);
    let t = text(&rows);
    assert!(t.contains("Ran 2 shell commands") && !t.contains("run b"), "{t}");
}
