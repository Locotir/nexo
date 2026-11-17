//! The status bar says nothing about the cache until it lets a call down; `/context` has the share.

use std::path::PathBuf;

use nexo_core::agent::AgentEvent;
use nexo_core::stream::Usage;
use nexo_tui::modal::Modal;
use nexo_tui::overlay::Info;
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
        animations: false,
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
    a
}

/// A model call that sent `fresh` tokens at full price and `cached` from the cache.
fn call(a: &mut App, fresh: u64, cached: u64) {
    let c = Usage { input: fresh, cache_read: cached, ..Default::default() };
    let mut session = a.session_usage;
    session.input += fresh;
    session.cache_read += cached;
    a.handle_agent(AgentEvent::Usage { turn: c, call: c, session, cost_usd: 0.01 });
}

fn rows(a: &mut App, w: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, 40)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer())
}

fn bar(a: &mut App, w: u16) -> String {
    rows(a, w).into_iter().find(|r| r.trim_start().starts_with("nexo")).expect("the status bar is on screen")
}

#[test]
fn a_normal_cached_session_shows_no_cache_figure() {
    let mut a = app();
    call(&mut a, 2_000, 40_000);
    call(&mut a, 2_000, 41_000);
    let b = bar(&mut a, 200);
    assert!(!b.contains("cached") && !b.contains("cache miss"), "{b}");
    assert!(b.contains("2 calls"), "{b}");
}

#[test]
fn a_miss_after_cached_calls_shows_the_chip_with_the_full_price_part() {
    let mut a = app();
    call(&mut a, 2_000, 40_000);
    call(&mut a, 31_000, 3_000);
    let b = bar(&mut a, 200);
    assert!(b.contains("cache miss · 31k full price · 2 calls"), "{b}");
}

#[test]
fn a_provider_that_never_reports_the_cache_never_shows_the_chip() {
    let mut a = app();
    call(&mut a, 30_000, 0);
    call(&mut a, 32_000, 0);
    let b = bar(&mut a, 200);
    assert!(!b.contains("cache miss"), "{b}");
}

#[test]
fn a_miss_under_eight_thousand_full_price_tokens_shows_no_chip() {
    let mut a = app();
    call(&mut a, 1_000, 40_000);
    call(&mut a, 7_900, 100);
    let b = bar(&mut a, 200);
    assert!(!b.contains("cache miss"), "{b}");
}

#[test]
fn the_chip_leaves_with_the_next_call_served_from_the_cache() {
    let mut a = app();
    call(&mut a, 2_000, 40_000);
    call(&mut a, 31_000, 3_000);
    assert!(bar(&mut a, 200).contains("cache miss"));
    call(&mut a, 3_000, 33_000);
    let b = bar(&mut a, 200);
    assert!(!b.contains("cache miss"), "{b}");
}

#[test]
fn the_chip_goes_with_the_cache_piece_when_the_bar_is_narrow() {
    let mut a = app();
    call(&mut a, 2_000, 40_000);
    call(&mut a, 31_000, 3_000);
    let narrow = bar(&mut a, 50);
    assert!(!narrow.contains("cache miss"), "{narrow}");
}

#[test]
fn context_has_one_line_with_the_cache_share_and_the_last_call() {
    let mut a = app();
    let none = {
        a.modal = Some(Modal::Info(Box::new(Info::new(0).still(true))));
        rows(&mut a, 100).join("\n")
    };
    assert!(!none.contains("of session"), "no usage, no line: {none}");
    call(&mut a, 2_000, 41_000);
    a.modal = Some(Modal::Info(Box::new(Info::new(0).still(true))));
    let all = rows(&mut a, 100).join("\n");
    assert!(all.contains("95% of session · last call 41k cached, 2k full price"), "{all}");
    // a resume or a rewind clears the last call but keeps the session's usage: no call is described then
    a.last_call_input = 0;
    a.last_call_cached = 0;
    a.modal = Some(Modal::Info(Box::new(Info::new(0).still(true))));
    let resumed = rows(&mut a, 100).join("\n");
    assert!(resumed.contains("95% of session") && !resumed.contains("last call"), "{resumed}");
}

#[test]
fn new_content_on_a_small_cached_prefix_is_not_a_lost_cache() {
    let mut a = app();
    call(&mut a, 1_000, 7_000);
    // the cache served all the previous call sent; the 25k file read is new
    call(&mut a, 26_000, 7_000);
    let b = bar(&mut a, 200);
    assert!(!b.contains("cache miss"), "{b}");
}

#[test]
fn a_miss_shows_its_chip_at_common_widths_and_the_count_returns_when_it_fits() {
    let mut a = app();
    call(&mut a, 2_000, 40_000);
    call(&mut a, 31_000, 3_000);
    let b = bar(&mut a, 120);
    assert!(b.contains("cache miss · 31k full price"), "the chip is on at 120 columns: {b}");
    let wide = bar(&mut a, 200);
    assert!(wide.contains("cache miss · 31k full price · 2 calls"), "{wide}");
    // the count is its own piece: where only it fits, a session with no miss still shows it
    let mut ok = app();
    call(&mut ok, 2_000, 40_000);
    call(&mut ok, 2_000, 41_000);
    assert!(bar(&mut ok, 120).contains("2 calls"));
}
