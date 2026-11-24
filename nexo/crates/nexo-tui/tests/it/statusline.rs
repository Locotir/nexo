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
fn empty_list_means_default_pieces_and_unknown_names_are_dropped() {
    assert_eq!(status_pieces(&[]).len(), 11);
    assert!(!status_pieces(&[]).contains(&"cache"));
    let c = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(status_pieces(&c(&["cost", "Model", "cost", "nope"])), vec!["cost", "model"]);
    assert!(status_pieces(&c(&["nope"])).is_empty());
    assert_eq!(status_pieces(&c(&["cache"])), vec!["cache"]);
}

#[test]
fn default_bar_shows_model_effort_mode_context_and_cost() {
    for (w, h) in [(80, 24), (200, 60)] {
        let b = bar(&mut app(&[]), w, h);
        for want in ["deepseek", "high", "ask", "%"] {
            assert!(b.contains(want), "{w}x{h}: `{want}` missing in {b:?}");
        }
        // the session name (here the time it began) at the right end keeps its place on a narrow bar, and
        // pushes the cost off it (9caf7618); where both fit, both show
        assert!(b.ends_with("10:00"), "{w}x{h}: the name is missing in {b:?}");
        assert_eq!(
            b.contains("$0.0123"),
            w >= 200,
            "{w}x{h}: the cost is shown only where the name leaves room: {b:?}"
        );
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

/// Removing the always-visible call count must not discard the underlying usage history.
/// Exercise the event-to-render path, rather than only checking the configured piece names.
#[test]
fn completed_calls_stay_out_of_the_status_bar_at_every_size_and_detail() {
    use nexo_core::{config::Detail, Usage};
    use std::time::Instant;

    for detail in [Detail::Full, Detail::Quiet] {
        for pieces in [&[][..], &["model", "tokens", "cost"][..]] {
            let mut a = app(pieces);
            a.opts.detail = detail;
            let call = Usage { input: 1000, output: 100, ..Default::default() };
            for n in 1..=3 {
                let session = Usage { input: n * 1000, output: n * 100, ..Default::default() };
                a.handle_agent(AgentEvent::Usage { turn: session, call, session, cost_usd: 0.0123 });
                assert_eq!(a.call_inputs.len(), n as usize);
                for busy in [false, true] {
                    a.busy = busy;
                    a.turn_started = busy.then(Instant::now);
                    for (w, h) in [(40, 12), (80, 24), (200, 60)] {
                        let b = bar(&mut a, w, h);
                        assert!(!b.contains("call"), "{detail:?}, busy={busy}, {w}x{h}: {b:?}");
                        if w == 200 && (detail == Detail::Full || !pieces.is_empty()) {
                            assert!(b.contains("billed") && b.contains("$0.0123"), "{b:?}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn call_count_and_per_call_inputs_remain_available_on_demand() {
    use nexo_core::Usage;
    use nexo_tui::commands::Command;

    let mut a = app(&[]);
    for input in [1000, 2000, 3000] {
        let call = Usage { input, output: 100, ..Default::default() };
        a.handle_agent(AgentEvent::Usage { turn: call, call, session: call, cost_usd: 0.0123 });
    }
    let history = a.call_inputs.clone();
    assert_eq!(history, vec![1000, 2000, 3000]);
    assert!(!bar(&mut a, 200, 60).contains("call"));

    a.run_command(Command::Cost);
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    draw_frame(&mut t, &mut a).unwrap();
    let screen = rows_of(t.backend().buffer()).join("\n");
    assert!(screen.contains("sent per call · 3 calls"), "{screen}");
    assert_eq!(a.call_inputs, history, "opening usage must not consume the call history");
}

#[test]
fn any_list_keeps_the_bar_one_row_at_the_small_size() {
    for list in [&["model"][..], &["cost"], &["mode", "context", "curator", "mem", "cache", "tokens", "cost"]] {
        let b = bar(&mut app(list), 40, 12);
        assert!(b.chars().count() <= 40, "{b:?}");
    }
}
