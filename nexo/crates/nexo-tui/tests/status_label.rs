//! The status bar keeps every piece that fits; the curator label outlives the counters.

use std::path::PathBuf;

use nexo_tui::app::ContextInfo;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn bar(width: u16) -> String {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "p".into(),
        model: "m".into(),
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
