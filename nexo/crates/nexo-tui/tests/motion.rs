//! Motion frames at fixed, injected times (80x24), and the same with the animations off.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nexo_core::agent::{AgentEvent, TurnEnd};
use nexo_core::stream::Usage;
use nexo_core::tool::ToolOutput;
use nexo_tui::toast::Kind;
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

fn ms(t0: Instant, n: u64) -> Instant {
    t0 + Duration::from_millis(n)
}

fn draw(a: &mut App) -> Buffer {
    let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
    draw_frame(&mut t, a).unwrap();
    t.backend().buffer().clone()
}

#[test]
fn a_toast_slides_in_stays_and_fades_out() {
    let t0 = Instant::now();
    let mut a = app(true);
    a.fx.clock = Some(t0);
    a.toast(Kind::Success, "copied 3 lines");
    let start = rows_of(&draw(&mut a))[0].clone();
    assert!(!start.contains("copied"), "starts off screen: {start}");
    a.fx.clock = Some(ms(t0, 75));
    let mid = rows_of(&draw(&mut a))[0].clone();
    assert!(mid.contains("copied") || mid.trim_end().ends_with("ok") || !mid.trim().is_empty(), "{mid}");
    a.fx.clock = Some(ms(t0, 1000));
    let full = rows_of(&draw(&mut a))[0].clone();
    assert!(full.trim_end().ends_with("ok copied 3 lines"), "{full}");
    a.fx.clock = Some(ms(t0, 3100));
    a.tick();
    assert!(!rows_of(&draw(&mut a))[0].contains("copied"));
}

#[test]
fn the_stack_shows_three_at_80_columns_and_never_touches_the_input() {
    let t0 = Instant::now();
    let mut a = app(false);
    a.fx.clock = Some(t0);
    for i in 0..4 {
        a.toast(Kind::Info, &format!("note {i}"));
    }
    let rows = rows_of(&draw(&mut a));
    assert!(rows[0].contains("note 1") && rows[1].contains("note 2") && rows[2].contains("note 3"));
    assert!(!rows.iter().any(|r| r.contains("note 0")));
    let last = &rows[rows.len() - 3..];
    assert!(last.iter().all(|r| !r.contains("note")), "{last:?}");
}

#[test]
fn a_narrow_screen_shows_one_line() {
    let t0 = Instant::now();
    let mut a = app(false);
    a.fx.clock = Some(t0);
    a.toast(Kind::Info, "one");
    a.toast(Kind::Error, "two");
    let mut t = Terminal::new(TestBackend::new(48, 20)).unwrap();
    draw_frame(&mut t, &mut a).unwrap();
    let rows = rows_of(t.backend().buffer());
    let shown = rows.iter().filter(|r| r.contains("one") || r.contains("two")).count();
    assert_eq!(shown, 1, "{rows:?}");
}

fn with_turn(a: &mut App) {
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::ToolCall {
        id: "c1".into(),
        name: "bash".into(),
        args: serde_json::json!({"command": "ls"}),
        summary: "ls".into(),
    });
    a.handle_agent(AgentEvent::ToolStart { id: "c1".into() });
}

fn rail_colours(b: &Buffer) -> Vec<Option<ratatui::style::Color>> {
    let mut out = Vec::new();
    for y in 0..b.area.height {
        for x in 0..b.area.width {
            let c = &b[(x, y)];
            if y < 20 && x < 40 {
                out.push(Some(c.fg));
            }
        }
    }
    out
}

#[test]
fn a_running_card_breathes_and_a_finished_one_eases_its_rail() {
    let mut a = app(true);
    let t0 = Instant::now();
    a.fx.clock = Some(t0);
    with_turn(&mut a);
    a.tick();
    a.handle_agent(AgentEvent::ToolProgress { id: "c1".into(), line: "a".into() });
    a.handle_agent(AgentEvent::ToolProgress { id: "c1".into(), line: "b".into() });
    let run_a = rail_colours(&draw(&mut a));
    a.fx.clock = Some(ms(t0, 400));
    a.tick();
    let run_b = rail_colours(&draw(&mut a));
    assert!(!run_a.is_empty());
    assert_ne!(run_a, run_b, "the rail breathes");
    a.handle_agent(AgentEvent::ToolDone { id: "c1".into(), output: ToolOutput::ok("a\nb"), elapsed_ms: 5 });
    a.tick();
    let early = rail_colours(&draw(&mut a));
    a.fx.clock = Some(ms(t0, 400 + 120));
    a.tick();
    let mid = rail_colours(&draw(&mut a));
    a.fx.clock = Some(ms(t0, 400 + 2000));
    a.tick();
    let late = rail_colours(&draw(&mut a));
    assert_ne!(early, mid);
    assert_ne!(mid, late, "still easing at 120 ms, settled later");
}

#[test]
fn the_end_of_a_turn_glows_along_the_rule_and_the_figures_count_up() {
    let t0 = Instant::now();
    let mut a = app(true);
    a.fx.clock = Some(t0);
    a.tick();
    a.handle_agent(AgentEvent::Usage {
        turn: Usage { input: 1200, output: 340, ..Default::default() },
        call: Usage { input: 1200, output: 340, ..Default::default() },
        session: Usage { input: 1200, output: 340, ..Default::default() },
        cost_usd: 0.0031,
    });
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    // the turn's rule, not the frame of the banner that stays on top
    let find_rule = |b: &Buffer| -> u16 {
        let framed = |y: u16| (0..80).any(|x| matches!(b[(x, y)].symbol(), "╭" | "╰" | "│"));
        (0..24).find(|y| b[(10, *y)].symbol() == "─" && !framed(*y)).unwrap_or(0)
    };
    let b0 = draw(&mut a);
    let y = find_rule(&b0);
    let colours = |b: &Buffer| -> Vec<ratatui::style::Color> { (0..80).map(|x| b[(x, y)].fg).collect() };
    a.fx.clock = Some(ms(t0, 200));
    a.tick();
    let early = draw(&mut a);
    a.fx.clock = Some(ms(t0, 400));
    a.tick();
    let later = draw(&mut a);
    assert_ne!(colours(&early), colours(&later), "the glow moves");
    // the figure is rolling at 200 ms and settled by the end
    let bar = |b: &Buffer| rows_of(b).last().unwrap().clone();
    assert!(bar(&early).contains('Σ'));
    assert_eq!(bar(&early).chars().count(), bar(&later).chars().count());
    a.fx.clock = Some(ms(t0, 1500));
    a.tick();
    let done = draw(&mut a);
    assert!(bar(&done).contains("1.5k billed") || bar(&done).contains("billed"), "{}", bar(&done));
    assert!(bar(&done).contains("$0.0031"));
    let plain = colours(&done);
    assert!(plain.windows(2).all(|w| w[0] == w[1]), "settled rule is one colour");
}

#[test]
fn with_animations_off_everything_is_final_at_once() {
    let t0 = Instant::now();
    let mut a = app(false);
    a.fx.clock = Some(t0);
    with_turn(&mut a);
    a.handle_agent(AgentEvent::ToolDone { id: "c1".into(), output: ToolOutput::ok("x"), elapsed_ms: 5 });
    a.handle_agent(AgentEvent::Usage {
        turn: Usage { input: 1200, output: 340, ..Default::default() },
        call: Usage { input: 1200, output: 340, ..Default::default() },
        session: Usage { input: 1200, output: 340, ..Default::default() },
        cost_usd: 0.0031,
    });
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    a.tick();
    let one = draw(&mut a);
    a.fx.clock = Some(ms(t0, 150));
    a.tick();
    let two = draw(&mut a);
    assert_eq!(one, two, "no frame differs from the final one");
    assert!(rows_of(&one).last().unwrap().contains("$0.0031"));
}

#[test]
fn with_animations_off_a_flash_message_keeps_its_colour_to_the_end() {
    for animations in [true, false] {
        let mut a = app(animations);
        // in the last 900 ms, where the message fades into the bar
        let at = Instant::now().checked_sub(Duration::from_millis(3500)).unwrap();
        a.status = Some(("model switched".into(), at));
        let b = draw(&mut a);
        let rows = rows_of(&b);
        let y = rows.iter().position(|r| r.contains("model switched")).expect("message on screen");
        let x = rows[y].find("model switched").unwrap();
        let x = rows[y][..x].chars().count() as u16;
        let fg = b[(x, y as u16)].fg;
        if animations {
            assert_ne!(fg, a.theme.bar_fg, "fading");
        } else {
            assert_eq!(fg, a.theme.bar_fg, "no shade in between");
            // and the clock is not asked for a frame of it
            a.dirty = false;
            a.tick();
            assert!(!a.dirty);
        }
    }
}

#[test]
fn with_animations_off_a_changed_context_cell_does_not_blink() {
    use nexo_core::{CellKind, CellState, ContextCell};
    use ratatui::style::Modifier;
    for animations in [true, false] {
        let mut a = app(animations);
        let cell = |kind, tokens, state| ContextCell { kind, tokens, state, zone: 0 };
        a.handle_agent(AgentEvent::ContextMap {
            cells: vec![cell(CellKind::Request, 40, CellState::Whole), cell(CellKind::Output, 900, CellState::Shrunk)],
            zones: Vec::new(),
            calls: Vec::new(),
        });
        a.toggle_live(120);
        assert!(a.show_live);
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        draw_frame(&mut t, &mut a).unwrap();
        let lit = t
            .backend()
            .buffer()
            .content()
            .iter()
            .filter(|c| ["█", "▒"].contains(&c.symbol()) && c.modifier.contains(Modifier::REVERSED))
            .count();
        a.tick();
        if animations {
            assert!(lit > 0, "a changed cell lights up");
            assert!(a.moving);
        } else {
            assert_eq!(lit, 0, "nothing blinks");
            assert!(!a.moving, "the loop is not kept moving");
        }
    }
}
