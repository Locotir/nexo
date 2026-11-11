//! The status bar keeps every piece that fits; the curator label outlives the counters.

use std::path::PathBuf;

use nexo_tui::app::ContextInfo;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn bar(width: u16) -> String {
    bar_of(width, "m")
}

fn bar_of(width: u16, model: &str) -> String {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "p".into(),
        model: model.into(),
        theme: "dark".into(),
        animations: false,
        ..TuiOptions::default()
    };
    let mut a = App::new(opts, tx);
    a.unlock();
    a.context = Some(ContextInfo {
        view_tokens: 4_000,
        history_tokens: 30_000,
        policy: "curator".into(),
        label: "curator -12k tok".into(),
        ..ContextInfo::default()
    });
    let mut t = Terminal::new(TestBackend::new(width, 30)).unwrap();
    draw_frame(&mut t, &mut a).unwrap();
    let rows = rows_of(t.backend().buffer());
    rows.iter().rev().find(|r| r.contains(" nexo")).cloned().unwrap_or_default()
}

#[test]
fn at_80_columns_the_label_and_counts_stay_while_they_fit() {
    let b = bar(80);
    println!("{b}");
    assert!(b.contains("curator -12k tok"), "{b}");
}

#[test]
fn the_label_outlives_the_counts_as_the_bar_narrows() {
    for w in 40..=140u16 {
        let b = bar(w);
        let has_counts = b.contains('/') && b.contains('k');
        if has_counts && b.contains("%") {
            assert!(b.contains("curator -12k tok") || !b.contains("curator"), "{w}: {b}");
        }
    }
}

#[test]
fn a_long_model_name_ends_in_an_ellipsis_on_a_narrow_bar_instead_of_vanishing() {
    let long = "model-beta-with-a-really-long-name-for-the-bar";
    for w in [40u16, 50, 60] {
        let b = bar_of(w, long);
        assert!(b.contains("model-beta") && b.contains('…'), "{w}: {b}");
        assert!(b.contains("billed") || b.chars().count() <= w as usize, "{w}: {b}");
    }
    let b = bar_of(200, long);
    assert!(b.contains(long) && !b.contains('…'), "{b}");
}

#[test]
fn with_no_model_yet_the_bar_shows_no_bare_slash() {
    let b = bar_of(80, "");
    assert!(!b.contains("│ / │") && !b.contains(" / "), "{b}");
}

/// The bar of a session that ran a turn, in a repository.
fn bar_with_branch(width: u16) -> String {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/p"),
        provider: "p".into(),
        model: "m".into(),
        theme: "dark".into(),
        animations: false,
        ..TuiOptions::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a.unlock();
    a.branch = Some("main".into());
    a.context = Some(ContextInfo { view_tokens: 4_000, history_tokens: 30_000, ..ContextInfo::default() });
    a.session_usage.input = 2_000;
    a.session_usage.cache_read = 1_000;
    a.call_inputs = vec![2_000, 3_000];
    let mut t = Terminal::new(TestBackend::new(width, 30)).unwrap();
    draw_frame(&mut t, &mut a).unwrap();
    let rows = rows_of(t.backend().buffer());
    rows.iter().rev().find(|r| r.contains(" nexo")).cloned().unwrap_or_default()
}

#[test]
fn the_branch_and_the_directory_go_only_when_they_do_not_fit() {
    let mut seen_branch_without_dir = false;
    for w in 40..=140u16 {
        let b = bar_with_branch(w);
        // the free columns between what the bar says on the left and the billed tokens on the right
        // past this the model name itself is cut: the bar is down to what matters most
        if !b.contains("p/m") {
            continue;
        }
        let Some(right) = b.find(" Σ ").or_else(|| b.find(" $")) else { continue };
        let free = b[..right].chars().rev().take_while(|c| *c == ' ').count();
        let (dir, branch) = (b.contains("/tmp/p"), b.contains("⎇ main"));
        // " │ ⎇ main" is 9 columns, " │ /tmp/p" 9 more
        assert!(branch || free < 9, "{w}: {free} free columns, no branch: {b}");
        assert!(dir || free < 9 || !branch && free < 18, "{w}: {free} free columns, no directory: {b}");
        seen_branch_without_dir |= branch && !dir;
    }
    assert!(seen_branch_without_dir, "the branch comes back on its own where the directory does not fit");
}
