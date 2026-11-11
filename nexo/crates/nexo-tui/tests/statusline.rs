//! The status bar's pieces and their order come from `[ui] status_line`; empty means all of them in
//! the default order. Checked at 40x12, 80x24 and 200x60.

use std::path::PathBuf;

use nexo_core::agent::AgentEvent;
use nexo_tui::ui::{rows_of, status_pieces};
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app(pieces: &[&str]) -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        animations: false,
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::Galaxy,
        home_stars: true,
        bursts: true,
        todo_strip: false,
        status_line: pieces.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a.handle_agent(AgentEvent::Ready {
        session_id: "20261102-100000-ef56".into(),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        cwd: PathBuf::from("/tmp/project"),
        permission_mode: nexo_core::permission::PermissionMode::Ask,
    });
    a.effort = Some("high".into());
    a.cost = 0.0123;
    a.cost_shown = 0.0123;
    a
}

/// The status bar row: the one that starts with the brand.
fn bar(a: &mut App, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    let rows = rows_of(t.backend().buffer());
    assert_eq!(rows.len(), h as usize);
    rows.into_iter().find(|r| r.trim_start().starts_with("nexo")).expect("the status bar is on screen")
}

#[test]
fn empty_list_means_every_piece_and_unknown_names_are_dropped() {
    assert_eq!(status_pieces(&[]).len(), 12);
    let c = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(status_pieces(&c(&["cost", "Model", "cost", "nope"])), vec!["cost", "model"]);
    assert!(status_pieces(&c(&["nope"])).is_empty());
}

#[test]
fn default_bar_shows_model_effort_mode_context_and_cost() {
    for (w, h) in [(80, 24), (200, 60)] {
        let b = bar(&mut app(&[]), w, h);
        for want in ["deepseek", "high", "ask", "%", "$0.0123"] {
            assert!(b.contains(want), "{w}x{h}: `{want}` missing in {b:?}");
        }
    }
}

#[test]
fn listed_pieces_only_and_in_the_listed_order() {
    for (w, h) in [(80, 24), (200, 60)] {
        let b = bar(&mut app(&["context", "model", "cost"]), w, h);
        assert!(!b.contains("ask") && !b.contains("high") && !b.contains("Σ"), "{w}x{h}: {b:?}");
        let (pct, model) = (b.find('%').expect("context"), b.find("deepseek").expect("model"));
        assert!(pct < model, "context before model: {b:?}");
        assert!(b.contains("$0.0123"), "{b:?}");
    }
    let b = bar(&mut app(&["tokens"]), 200, 60);
    assert!(b.contains("billed") && !b.contains('$') && !b.contains("deepseek"), "{b:?}");
}

#[test]
fn any_list_keeps_the_bar_one_row_at_the_small_size() {
    for list in [&["model"][..], &["cost"], &["mode", "context", "curator", "mem", "cache", "tokens", "cost"]] {
        let b = bar(&mut app(list), 40, 12);
        assert!(b.chars().count() <= 40, "{b:?}");
    }
}
