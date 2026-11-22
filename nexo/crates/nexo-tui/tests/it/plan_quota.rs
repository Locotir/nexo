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
        observed_at: Some(nexo_core::auth::now() as i64),
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
    assert!(s.contains("5h 63% ↻2h · wk 88% ↻4d"), "{s}");
    // a narrower bar keeps the figures and drops the resets; a very narrow one drops the item
    let s = screen(&mut a, 80);
    assert!(s.contains("5h 63% · wk 88%") && !s.contains('↻'), "{s}");
    let s = screen(&mut a, 50);
    assert!(!s.contains("5h 63%"), "{s}");
}

#[test]
fn a_window_over_80_percent_turns_warm_and_a_spent_one_red() {
    let theme = Theme::pick("dark");
    let mut a = app(Some("codex"));
    let mut p = plan(85.0, 100.0);
    // High usage, but on track to reach the reset: only exhaustion should be red here.
    p.meters[0].resets_at = Some(nexo_core::auth::now() as i64 + 10 * 60);
    a.plan_usage = Some(p);
    let t = draw(&mut a, 140);
    let rows = rows_of(t.backend().buffer());
    let (y, row) = rows.iter().enumerate().find(|(_, r)| r.contains("5h 15%")).expect("the item");
    let colour_of = |needle: &str| {
        let x = row[..row.find(needle).unwrap()].chars().count() as u16;
        t.backend().buffer()[(x, y as u16)].fg
    };
    assert_eq!(colour_of("5h 15%"), theme.warn);
    assert_eq!(colour_of("wk 0%"), theme.error);
}

#[test]
fn the_usage_window_has_the_detail_of_each_window() {
    let mut a = app(Some("codex"));
    a.plan_usage = Some(plan(37.0, 12.0));
    a.run_command(Command::Cost);
    let s = screen(&mut a, 140);
    assert!(s.contains("plus plan") || s.contains("ChatGPT · plus plan"), "{s}");
    assert!(s.contains("63% left") && s.contains("resets in 2h"), "{s}");
    assert!(s.contains("88% left") && s.contains("resets in 4d"), "{s}");
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
    assert!(!s.contains("5h 63%") && s.contains("wk 88%"), "{s}");
}

fn xai_plan(used: f64) -> PlanUsage {
    PlanUsage {
        kind: "xai".into(),
        plan: None,
        meters: vec![meter("primary", "wk", used, 10_080, 3 * 86_400)],
        credits: None,
        seen: 0,
    }
}

#[test]
fn a_supergrok_login_shows_what_is_left_in_the_bar_and_in_usage() {
    let mut a = app(Some("xai"));
    a.plan_usage = Some(xai_plan(8.0));
    let s = screen(&mut a, 140);
    assert!(s.contains("wk 92% ↻3d"), "{s}");
    a.run_command(Command::Cost);
    let s = screen(&mut a, 140);
    assert!(s.contains("SuperGrok plan") && s.contains("92% left") && s.contains("resets in 3d"), "{s}");
}

#[test]
fn per_minute_limits_of_a_key_show_in_usage_but_not_in_the_bar() {
    let mut a = app(None);
    let mut limits = xai_plan(0.0);
    limits.kind = "limits:api.example".into();
    limits.meters = vec![Meter {
        slot: "requests".into(),
        label: "req".into(),
        used_pct: 10.0,
        counts: Some((6, 60)),
        window_minutes: None,
        resets_at: None,
        observed_at: Some(nexo_core::auth::now() as i64),
    }];
    a.plan_usage = Some(limits);
    let s = screen(&mut a, 140);
    assert!(!s.contains("req 54/60"), "{s}");
    a.run_command(Command::Cost);
    let s = screen(&mut a, 140);
    assert!(s.contains("api.example rate limits") && s.contains("54/60"), "{s}");
}

#[test]
fn usage_says_so_in_one_line_when_there_is_no_figure() {
    let mut a = app(Some("xai"));
    a.run_command(Command::Cost);
    let s = screen(&mut a, 140);
    assert!(s.contains("no plan or rate-limit figure from p"), "{s}");
    assert!(s.contains("this session"), "{s}");
}

#[test]
fn fast_pace_is_visible_and_conservative() {
    let mut a = app(Some("xai"));
    let mut p = xai_plan(50.0);
    let now = nexo_core::auth::now() as i64;
    // One quarter of the window elapsed, half of its allowance consumed.
    p.seen = now;
    p.meters[0].resets_at = Some(now + 7 * 86_400 * 3 / 4);
    a.plan_usage = Some(p.clone());
    let t = draw(&mut a, 140);
    let rows = rows_of(t.backend().buffer());
    let (y, row) = rows.iter().enumerate().find(|(_, r)| r.contains("wk 50%")).unwrap();
    assert!(row.contains("wk 50% ↻5d6h 󰈸 0% in 1d18h"), "{row}");
    let x = row[..row.find("wk 50%").unwrap()].chars().count() as u16;
    assert_eq!(t.backend().buffer()[(x, y as u16)].fg, Theme::pick("dark").error);
    p.meters[0].observed_at = Some(now - 3601);
    a.plan_usage = Some(p);
    assert!(!screen(&mut a, 140).contains('󰈸'));
}

#[test]
fn codex_multi_window_pace_is_independent_per_slot() {
    let mut a = app(Some("codex"));
    let now = nexo_core::auth::now() as i64;
    let mut p = plan(80.0, 20.0);
    p.meters[0].resets_at = Some(now + 4 * 3600);
    p.meters[1].resets_at = Some(now + 4 * 86400);
    a.plan_usage = Some(p.clone());
    let s = screen(&mut a, 180);
    assert!(s.contains("5h 20% ↻4h 󰈸 0% in 15m") && s.matches('󰈸').count() == 1, "{s}");
    p.meters[0].observed_at = Some(now - 3601);
    a.plan_usage = Some(p);
    assert!(!screen(&mut a, 180).contains('󰈸'));
}
