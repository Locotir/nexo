//! The curator condensing the context: the map's cells step down, a light runs to the label, the figure counts
//! up, and none of it touches the text or keeps the clock busy once it is over.

use super::*;
use crate::frame::Load;
use nexo_core::{CellKind, CellState, ContextCell};
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use std::time::Duration;

const LABEL: &str = "curator -2.4k tok";

fn ctx(view: u64, history: u64, label: &str) -> AgentEvent {
    AgentEvent::Context {
        view_tokens: view,
        history_tokens: history,
        history_bytes: history * 4,
        view_bytes: view * 4,
        messages_in_view: 1,
        messages_total: 1,
        policy: "curator".into(),
        note: None,
        label: label.into(),
        detail: None,
    }
}

fn map(states: &[CellState]) -> AgentEvent {
    let kinds = [CellKind::Request, CellKind::Reply, CellKind::Call, CellKind::Output];
    let cells = states
        .iter()
        .enumerate()
        .map(|(i, &state)| ContextCell { kind: kinds[i % 4], tokens: 50, state, zone: 0 })
        .collect();
    AgentEvent::ContextMap { cells, zones: Vec::new(), calls: Vec::new(), call_tokens: Vec::new() }
}

/// An app with the live map open over some settled text, and the moment (`t0`) its last call arrived: the
/// curator letting six messages go when `cut`, or the same map arriving one message longer.
fn session(cut: bool) -> (App, Instant) {
    use CellState::*;
    let (mut a, _rx) = app();
    let t0 = a.opened + Duration::from_secs(10);
    a.show_live = true;
    fx_at(&mut a, t0 - Duration::from_secs(5), 0);
    a.push(Block::User { text: "tidy the parser".into(), at: None });
    a.push(Block::Assistant {
        text: "Done: the parser reads the header once.".into(),
        streaming: false,
        at: None,
        started: None,
        took_ms: None,
    });
    a.handle_agent(ctx(4000, 4000, "curator"));
    a.handle_agent(map(&[Whole; 10]));
    fx_at(&mut a, t0, 0);
    if cut {
        a.handle_agent(ctx(1600, 4100, LABEL));
        a.handle_agent(map(&[Dropped, Dropped, Dropped, Dropped, Shrunk, Shrunk, Whole, Whole, Whole, Whole, Whole]));
    } else {
        a.handle_agent(ctx(4100, 4100, "curator"));
        a.handle_agent(map(&[Whole; 11]));
    }
    // the gauge eases a quarter of the way on each tick: settle it, it is not what these tests look at
    for _ in 0..80 {
        a.tick();
    }
    a.map_at = None;
    // the motion runs from the frame that first shows the cut: here, one drawn at once
    a.cut_on_screen();
    (a, t0)
}

fn row(b: &Buffer, y: u16) -> String {
    (0..b.area.width).map(|x| b[(x, y)].symbol()).collect()
}

/// The row of the rule over the draft: the last row of plain `─` above the bar.
fn rule_row(b: &Buffer) -> u16 {
    (0..b.area.height).rev().find(|&y| row(b, y).contains("──────────")).expect("the rule")
}

fn curator_label(a: &App) -> String {
    let segs = crate::ui::title_segments(a, 200).into_iter().flatten();
    segs.map(|s| s.text).find(|t| t.contains("curator")).expect("the curator label").trim_start().to_string()
}

#[test]
fn a_cut_condenses_the_map_runs_a_light_to_the_label_and_leaves_the_text_alone() {
    let ((mut a, t0), (mut quiet, q0), (mut still, s0)) = (session(true), session(false), session(true));
    still.opts.animations = false;
    assert_eq!(a.fx.cut.map(|c| c.0), Some(2400));
    let (mut stepped, mut lit_rule, mut landed) = (false, false, false);
    for ms in (0..=1400).step_by(50) {
        fx_at(&mut a, t0, ms);
        fx_at(&mut quiet, q0, ms);
        let (b, q) = (buf_of(&mut a, 200, 60), buf_of(&mut quiet, 200, 60));
        // the text is never drawn over: every cell left of the panel and above the rule is the quiet one's
        let (panel, rule) = (a.files_area.x, rule_row(&b));
        for y in 0..rule {
            for x in 0..panel {
                assert_eq!(b[(x, y)], q[(x, y)], "{ms} ms: cell ({x}, {y}) of the transcript changed");
            }
        }
        // the rule keeps its glyphs: the light is colour only
        assert_eq!(row(&b, rule), row(&q, rule), "{ms} ms");
        stepped |= (0..rule).any(|y| row(&b, y).contains('▓'));
        lit_rule |= (0..200).any(|x| b[(x, rule)].modifier.contains(Modifier::BOLD) && b[(x, rule)] != q[(x, rule)]);
        // the label keeps its width while its figure counts up
        let label = curator_label(&a);
        assert_eq!(label.chars().count(), LABEL.chars().count(), "{ms} ms: {label:?}");
        if ms == 1000 {
            let col = a.fx.cut_col.get().expect("the bar drew the label");
            landed = b[(col, rule)].modifier.contains(Modifier::BOLD);
        }
    }
    assert!(stepped, "a cell showed the step between whole and cut");
    assert!(lit_rule, "the light ran along the rule");
    assert!(landed, "the light landed over the label's figure at one second");
    // over: everything above the bar is the screen without the motion, cell for cell
    fx_at(&mut a, t0, 1500);
    fx_at(&mut still, s0, 1500);
    let (b, s) = (buf_of(&mut a, 200, 60), buf_of(&mut still, 200, 60));
    for y in 0..59 {
        for x in 0..200 {
            assert_eq!(b[(x, y)], s[(x, y)], "cell ({x}, {y})");
        }
    }
    assert_eq!(curator_label(&a), LABEL);
}

#[test]
fn the_cells_that_were_cut_condense_oldest_first_and_the_rest_stay_settled() {
    let (mut a, t0) = session(true);
    let glyphs = |a: &mut App, ms: u64| -> String {
        fx_at(a, t0, ms);
        let b = buf_of(a, 200, 60);
        let x0 = a.files_area.x + 2;
        let y = (0..60).find(|&y| matches!(b[(x0, y)].symbol(), "█" | "▓" | "▒" | "░" | "·")).expect("the map");
        (x0..x0 + 11).map(|x| b[(x, y)].symbol()).collect()
    };
    assert_eq!(glyphs(&mut a, 0), "███████████");
    let early = glyphs(&mut a, 100);
    assert!(early.starts_with('▓'), "the oldest cell steps first: {early}");
    assert!(early.ends_with("█████"), "the cells kept whole never move: {early}");
    assert_eq!(glyphs(&mut a, 1200), "····▒▒█████");
    // the motion off: the end state at once
    a.opts.animations = false;
    assert_eq!(glyphs(&mut a, 0), "····▒▒█████");
}

#[test]
fn the_cut_asks_for_frames_while_it_moves_then_the_beat_then_nothing() {
    let (mut a, t0) = session(true);
    for ms in [0, 700, 1300] {
        fx_at(&mut a, t0, ms);
        a.tick();
        assert_eq!(a.load(), Load::Motion, "{ms} ms: the map condenses, the light runs");
    }
    fx_at(&mut a, t0, 1500);
    a.tick();
    assert_eq!(a.load(), Load::Beat, "the label fades back on the beat");
    fx_at(&mut a, t0, crate::motion::CUT_MS + 200);
    a.tick();
    assert_eq!(a.load(), Load::Idle, "then the clock rests");
    // with the motion off nothing asks for frames, and the label still goes back by itself
    let (mut b, t0) = session(true);
    b.opts.animations = false;
    fx_at(&mut b, t0, 0);
    b.tick();
    assert_ne!(b.load(), Load::Motion);
    assert_eq!(b.condense_ms(), None);
    fx_at(&mut b, t0, crate::motion::CUT_MS + 200);
    b.tick();
    assert_eq!(b.load(), Load::Idle);
}

#[test]
fn a_cut_whose_first_frame_is_late_starts_its_motion_on_that_frame() {
    let (mut a, t0) = session(true);
    a.fx.cut_shown = false;
    // the frame that brings the cut also lays out a long tool output and reaches the screen 400 ms later
    fx_at(&mut a, t0, 400);
    assert_eq!(a.condense_ms(), Some(0), "until it is seen the cut stands at its start");
    screen(&mut a, 200, 60);
    fx_at(&mut a, t0, 500);
    assert_eq!(a.condense_ms(), Some(100));
    assert!(a.cut_fading(), "the label fades from that frame too");
}
