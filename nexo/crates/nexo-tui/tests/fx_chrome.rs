//! The chrome's motion at fixed, injected times: the welcome card coming in, the context gauge easing,
//! the scrollbar thumb lighting, a new chip and a rejected draft, a new branch in the status bar.
//! Each: a mid-effect frame differs from the settled one only in style, the settled frame is the
//! animations-off frame, and the loop rests once the effect is over.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nexo_core::agent::{AgentEvent, TurnEnd};
use nexo_core::ProviderConfig;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app(animations: bool) -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        theme: "dark".into(),
        animations,
        ..TuiOptions::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}

fn locked(animations: bool) -> App {
    std::env::set_var("NEXO_AUTH_FILE", "/nonexistent/nexo-test-auth.json");
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut c = ProviderConfig::new("openai-chat", "https://codex.example", None);
    c.login = Some("codex".into());
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "codex".into(),
        model: "m".into(),
        providers: vec![("codex".to_string(), c)],
        theme: "dark".into(),
        animations,
        ..TuiOptions::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}

fn ms(t0: Instant, n: u64) -> Instant {
    t0 + Duration::from_millis(n)
}

fn draw_at(a: &mut App, t0: Instant, n: u64, w: u16) -> Buffer {
    a.fx.clock = Some(ms(t0, n));
    a.tick_fx();
    let mut t = Terminal::new(TestBackend::new(w, 24)).unwrap();
    draw_frame(&mut t, a).unwrap();
    t.backend().buffer().clone()
}

/// The bottom rows: the draft, the keys and the status bar (the banner above fades by itself).
fn foot(b: &Buffer) -> Vec<ratatui::buffer::Cell> {
    let w = b.area.width as usize;
    b.content()[w * (b.area.height as usize - 5)..].to_vec()
}

fn symbols(b: &Buffer) -> Vec<String> {
    b.content().iter().map(|c| c.symbol().to_string()).collect()
}

fn find(b: &Buffer, needle: &str) -> Option<(u16, u16)> {
    let w = b.area.width;
    rows_of(b).iter().enumerate().find_map(|(y, r)| {
        let at = r.find(needle)?;
        Some((r[..at].chars().count() as u16 % w, y as u16))
    })
}

fn limits(a: &mut App, window: u32) {
    a.handle_agent(AgentEvent::ModelLimits {
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        pricing: None,
        context_window: Some(window),
        max_output: None,
        source: "config".into(),
    });
}

#[test]
fn the_welcome_card_comes_in_line_by_line_once() {
    let t0 = Instant::now();
    let mut a = locked(true);
    let first = draw_at(&mut a, t0, 0, 80);
    let mid = draw_at(&mut a, t0, 120, 80);
    let done = draw_at(&mut a, t0, 400, 80);
    assert_eq!(symbols(&first), symbols(&done), "colour only");
    assert_eq!(symbols(&mid), symbols(&done));
    let (x, y) = find(&done, "Set up a model").unwrap();
    assert_ne!(first[(x, y)].fg, done[(x, y)].fg, "the card starts at the ground colour");
    assert_ne!(mid[(x, y)].fg, done[(x, y)].fg, "and is on its way");
    assert!(!a.tick_fx(), "it rests");
    // a card that came in does not come in again
    let again = draw_at(&mut a, t0, 2000, 80);
    assert_eq!(again, done);
    let mut off = locked(false);
    assert_eq!(draw_at(&mut off, t0, 0, 80), done, "animations off draw the settled card at once");
    assert!(!off.tick_fx());
}

fn gauge_app(animations: bool, t0: Instant) -> (App, Buffer) {
    let mut a = app(animations);
    limits(&mut a, 1000);
    a.last_call_input = 300;
    a.last_call_cached = 100;
    let b = draw_at(&mut a, t0, 0, 120);
    (a, b)
}

#[test]
fn the_gauge_eases_to_a_new_fill_and_fades_to_a_new_band() {
    let t0 = Instant::now();
    let (mut a, first) = gauge_app(true, t0);
    assert!(!a.tick_fx(), "the first look is the value");
    a.last_call_input = 800;
    a.last_call_cached = 100;
    let start = draw_at(&mut a, t0, 1000, 120);
    let mid = draw_at(&mut a, t0, 1120, 120);
    let done = draw_at(&mut a, t0, 1000 + 400, 120);
    assert_eq!(symbols(&mid), symbols(&done), "the gauge only changes colour while it eases");
    assert_ne!(mid, done, "in between it is not the settled gauge");
    assert_ne!(start, done);
    assert_ne!(first, done);
    assert!(!a.tick_fx(), "it rests");
    let (mut off, _) = gauge_app(false, t0);
    off.last_call_input = 800;
    off.last_call_cached = 100;
    let o = draw_at(&mut off, t0, 1000, 120);
    assert_eq!(foot(&o), foot(&done), "off: the settled gauge at once");
}

fn long_chat(a: &mut App) {
    a.drop_banner();
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    let text: String = (0..60).map(|i| format!("line {i} of a long answer\n\n")).collect();
    a.handle_agent(AgentEvent::TextDelta { text });
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
}

#[test]
fn the_scrollbar_thumb_lights_while_the_view_scrolls_and_fades_back() {
    let t0 = Instant::now();
    let mut a = app(true);
    long_chat(&mut a);
    let rest = draw_at(&mut a, t0, 0, 80);
    let thumb = |b: &Buffer| -> Vec<ratatui::style::Color> {
        (0..24).filter(|y| b[(79, *y)].symbol() == "┃").map(|y| b[(79, y)].fg).collect()
    };
    let rest_c = thumb(&rest);
    assert!(!rest_c.is_empty(), "the bar is drawn");
    a.scroll_by(-6);
    let mut lit = false;
    for n in [1000, 1060, 1120, 1200, 1300] {
        let b = draw_at(&mut a, t0, n, 80);
        lit |= thumb(&b).first() != rest_c.first();
    }
    assert!(lit, "the thumb is lit while the view moves");
    let after = draw_at(&mut a, t0, 3000, 80);
    assert_eq!(thumb(&after).first(), thumb(&after).last());
    assert!(!a.tick_fx(), "it rests");
    let mut off = app(false);
    long_chat(&mut off);
    draw_at(&mut off, t0, 0, 80);
    off.scroll_by(-6);
    draw_at(&mut off, t0, 1000, 80);
    assert_eq!(draw_at(&mut off, t0, 3000, 80), after, "the settled bar is the animations-off bar");
}

#[test]
fn a_new_paste_chip_is_lit_once() {
    let t0 = Instant::now();
    let mut a = app(true);
    draw_at(&mut a, t0, 0, 80);
    a.input.insert_paste("one\ntwo\nthree\nfour\nfive");
    let mid = draw_at(&mut a, t0, 100, 80);
    let done = draw_at(&mut a, t0, 600, 80);
    let (x, y) = find(&done, "[Pasted text").unwrap();
    assert_eq!(symbols(&mid), symbols(&done));
    assert_ne!(mid[(x + 2, y)].fg, done[(x + 2, y)].fg, "the chip is lit");
    assert!(!a.tick_fx(), "it rests");
    let mut off = app(false);
    draw_at(&mut off, t0, 0, 80);
    off.input.insert_paste("one\ntwo\nthree\nfour\nfive");
    assert_eq!(foot(&draw_at(&mut off, t0, 100, 80)), foot(&done));
}

#[test]
fn a_draft_that_names_no_command_flashes_the_rule() {
    let t0 = Instant::now();
    let mut a = app(true);
    a.input.set_text("/nosuchcommand now");
    let before = draw_at(&mut a, t0, 0, 80);
    a.fx.clock = Some(ms(t0, 1000));
    a.submit();
    let mid = draw_at(&mut a, t0, 1150, 80);
    let (_, y) = find(&mid, "/nosuchcommand now").expect("the draft stays");
    let rule = y - 1;
    let colours = |b: &Buffer| (0..80u16).map(|x| b[(x, rule)].fg).collect::<Vec<_>>();
    let peak = draw_at(&mut a, t0, 1000 + 150, 80);
    assert_ne!(colours(&peak), colours(&before), "the rule flashes");
    let done = draw_at(&mut a, t0, 1000 + 700, 80);
    assert_eq!(colours(&done), colours(&before), "and rests where it was");
    assert!(!a.tick_fx(), "it rests");
}

#[test]
fn a_new_branch_is_lit_but_one_found_after_the_start_is_not() {
    let t0 = Instant::now();
    let mut a = app(true);
    draw_at(&mut a, t0, 0, 160);
    a.branch = Some("main".into());
    let found = draw_at(&mut a, t0, 100, 160);
    let (x, y) = find(&found, "⎇ main").expect("the branch shows");
    let settled = draw_at(&mut a, t0, 5000, 160);
    assert_eq!(found[(x + 3, y)].fg, settled[(x + 3, y)].fg, "a branch found late is not a change");
    a.branch = Some("dev".into());
    let mid = draw_at(&mut a, t0, 5100, 160);
    let done = draw_at(&mut a, t0, 9000, 160);
    let (x, y) = find(&done, "⎇ dev").unwrap();
    assert_ne!(mid[(x + 3, y)].fg, done[(x + 3, y)].fg, "the new branch is lit");
    assert_eq!(symbols(&mid), symbols(&done));
    assert!(!a.tick_fx(), "it rests");
}
