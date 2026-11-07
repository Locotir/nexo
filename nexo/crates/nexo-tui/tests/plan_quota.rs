//! What is left of a subscription plan: a compact item in the status bar, the detail in `/usage`, and
//! nothing for a provider that runs on a key.

use std::path::PathBuf;

use nexo_core::ProviderConfig;
use nexo_providers::usage::{Meter, PlanUsage};
use nexo_tui::commands::Command;
use nexo_tui::theme::Theme;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app(login: Option<&str>) -> App {
    std::env::set_var("NEXO_T_QUOTA_KEY", "k");
    let mut cfg = ProviderConfig::new("openai-chat", "https://api.example", Some("NEXO_T_QUOTA_KEY"));
    cfg.login = login.map(String::from);
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "p".into(),
        model: "m".into(),
        providers: vec![("p".into(), cfg)],
        theme: "dark".into(),
        animations: false,
        ..TuiOptions::default()
    };
    let mut a = App::new(opts, tx);
    a.unlock();
    a
}

fn meter(slot: &str, label: &str, pct: f64, minutes: u64, in_secs: i64) -> Meter {
    Meter {
        slot: slot.into(),
        label: label.into(),
        used_pct: pct,
        counts: None,
        window_minutes: Some(minutes),
        resets_at: Some(nexo_core::auth::now() as i64 + in_secs),
    }
}

fn plan(five: f64, week: f64) -> PlanUsage {
    PlanUsage {
        kind: "codex".into(),
        plan: Some("plus".into()),
        meters: vec![meter("primary", "5h", five, 300, 2 * 3600), meter("secondary", "wk", week, 10_080, 4 * 86_400)],
        credits: None,
        seen: 0,
    }
}

fn draw(a: &mut App, width: u16) -> Terminal<TestBackend> {
    let mut t = Terminal::new(TestBackend::new(width, 30)).unwrap();
    draw_frame(&mut t, a).unwrap();
    t
}

fn screen(a: &mut App, width: u16) -> String {
    rows_of(draw(a, width).backend().buffer()).join("\n")
}

#[test]
fn the_status_bar_shows_both_windows_with_their_reset() {
    let mut a = app(Some("codex"));
    a.plan_usage = Some(plan(37.0, 12.0));
    let s = screen(&mut a, 140);
    assert!(s.contains("5h 37% ↻2h · wk 12% ↻4d"), "{s}");
    // a narrower bar keeps the figures and drops the resets; a very narrow one drops the item
    let s = screen(&mut a, 80);
    assert!(s.contains("5h 37% · wk 12%") && !s.contains('↻'), "{s}");
    let s = screen(&mut a, 50);
    assert!(!s.contains("5h 37%"), "{s}");
}

#[test]
fn a_window_over_80_percent_turns_warm_and_a_spent_one_red() {
    let theme = Theme::pick("dark");
    let mut a = app(Some("codex"));
    a.plan_usage = Some(plan(85.0, 100.0));
    let t = draw(&mut a, 140);
    let rows = rows_of(t.backend().buffer());
    let (y, row) = rows.iter().enumerate().find(|(_, r)| r.contains("5h 85%")).expect("the item");
    let colour_of = |needle: &str| {
        let x = row[..row.find(needle).unwrap()].chars().count() as u16;
        t.backend().buffer()[(x, y as u16)].fg
    };
    assert_eq!(colour_of("5h 85%"), theme.warn);
    assert_eq!(colour_of("wk 100%"), theme.error);
}

#[test]
fn the_usage_window_has_the_detail_of_each_window() {
    let mut a = app(Some("codex"));
    a.plan_usage = Some(plan(37.0, 12.0));
    a.run_command(Command::Cost);
    let s = screen(&mut a, 140);
    assert!(s.contains("plus plan") || s.contains("ChatGPT · plus plan"), "{s}");
    assert!(s.contains("37%") && s.contains("resets in 2h"), "{s}");
    assert!(s.contains("12%") && s.contains("resets in 4d"), "{s}");
}

#[test]
fn a_provider_on_a_key_shows_nothing() {
    let mut a = app(None);
    a.sync_plan_usage();
    assert!(a.plan_usage.is_none());
    let s = screen(&mut a, 140);
    assert!(!s.contains("5h") && !s.contains('↻'), "{s}");
}

#[test]
fn a_reset_window_leaves_the_bar() {
    let mut a = app(Some("codex"));
    let mut p = plan(37.0, 12.0);
    p.meters[0].resets_at = Some(nexo_core::auth::now() as i64 - 60);
    a.plan_usage = Some(p);
    let s = screen(&mut a, 140);
    assert!(!s.contains("5h 37%") && s.contains("wk 12%"), "{s}");
}
