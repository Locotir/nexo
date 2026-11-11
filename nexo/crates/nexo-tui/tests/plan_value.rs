//! A subscription's cost is the API value, included in the plan, not money spent.

use std::path::PathBuf;

use nexo_core::stream::Usage;
use nexo_core::{ModelInfo, ProviderConfig, ServerProbe};
use nexo_tui::commands::Command;
use nexo_tui::modal::setup::{Setup, Step};
use nexo_tui::render::blocks::Block;
use nexo_tui::theme::Theme;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn plan_cfg() -> ProviderConfig {
    let mut c = ProviderConfig::new("openai-chat", "https://codex.example", None);
    c.login = Some("codex".into());
    c
}

fn app(login: bool) -> App {
    std::env::set_var("NEXO_T_PLAN_KEY", "k");
    let cfg = if login {
        plan_cfg()
    } else {
        ProviderConfig::new("openai-chat", "https://api.example", Some("NEXO_T_PLAN_KEY"))
    };
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
    a.cost = 0.0123;
    a.cost_shown = 0.0123;
    a.session_usage = Usage { input: 1000, output: 200, ..Default::default() };
    a
}

fn screen(a: &mut App) -> String {
    let mut t = Terminal::new(TestBackend::new(140, 30)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer()).join("\n")
}

fn turn(a: &mut App) {
    let usage = Usage { input: 1800, output: 412, ..Default::default() };
    let plan = a.on_plan();
    a.push(Block::Usage { usage, cost_usd: 0.0042, elapsed_ms: 9000, tally: Default::default(), plan, folded: true });
}

#[test]
fn the_status_line_turn_summary_and_usage_panel_call_it_api_value() {
    let mut a = app(true);
    turn(&mut a);
    let s = screen(&mut a);
    assert!(s.contains("≈$0.0123 API value · included in plan"), "status line\n{s}");
    assert!(s.contains("≈$0.0042 API value · included in plan"), "turn summary\n{s}");
    a.run_command(Command::Cost);
    let s = screen(&mut a);
    assert!(s.contains("≈$0.0123 API value") && s.contains("this session"), "usage panel\n{s}");
}

#[test]
fn an_api_provider_still_shows_plain_dollars() {
    let mut a = app(false);
    turn(&mut a);
    let s = screen(&mut a);
    assert!(s.contains("$0.0123") && s.contains("$0.0042"), "{s}");
    assert!(!s.contains("API value") && !s.contains("included in plan"), "{s}");
}

#[test]
fn the_picker_price_column_reads_plan_for_a_login_provider() {
    let probe = ServerProbe {
        kind: "openai-compatible".into(),
        protocol: "openai-chat".into(),
        base_url: "https://codex.example".into(),
        models: vec![ModelInfo {
            id: "gpt-x".into(),
            pricing: Some(nexo_core::Pricing { input: 1.0, output: 2.0, cache_read: 0.0, cache_write: 0.0 }),
            ..Default::default()
        }],
        ..Default::default()
    };
    for (login, want) in [(true, "plan"), (false, "$1.00 / $2.00")] {
        let cfg = if login { plan_cfg() } else { ProviderConfig::new("openai-chat", "https://x.example", None) };
        let mut s = Setup::new(vec![("p".into(), cfg.clone())], vec![], "p/m");
        s.name = "p".into();
        s.cfg = cfg;
        s.step = Step::Address;
        s.busy = Some("looking".into());
        s.probed(Ok(probe.clone()));
        assert_eq!(s.step, Step::Model);
        let hint = s.list.current().unwrap().hint.clone();
        assert!(hint.contains(want), "{hint}");
        if login {
            assert!(!hint.contains('$'), "{hint}");
        }
        let _ = Theme::nexo();
    }
}
