//! The motion of a streaming reply and the blocks around it, at fixed injected times (80x24): fresh ink,
//! the thinking header settling, a failed card's flash, the lit note of a finished card, the person's
//! message fading in and the rail of an interrupted block. Each settles to what animations-off draws.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nexo_core::agent::{AgentEvent, TurnEnd};
use nexo_core::tool::ToolOutput;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app(animations: bool) -> (App, Instant) {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        theme: "dark".into(),
        animations,
        // an empty session drifts its stars for a while (src/app/tests/bursts.rs); these frames are about other things
        home_stars: false,
        ..TuiOptions::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    let t0 = Instant::now();
    a.fx.clock = Some(t0);
    a.tick();
    (a, t0)
}

/// Draws at `t0 + ms`, after a tick at that time.
fn at(a: &mut App, t0: Instant, ms: u64) -> Buffer {
    a.fx.clock = Some(t0 + Duration::from_millis(ms));
    a.tick();
    let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
    draw_frame(&mut t, a).unwrap();
    t.backend().buffer().clone()
}

fn symbols(b: &Buffer) -> Vec<String> {
    rows_of(b)
}

/// The cells of the first row of `b` that holds `needle`.
fn row(b: &Buffer, needle: &str) -> Vec<ratatui::buffer::Cell> {
    let y = symbols(b).iter().position(|r| r.contains(needle)).unwrap_or_else(|| panic!("no row with {needle}"));
    (0..b.area.width).map(|x| b[(x, y as u16)].clone()).collect()
}

/// The columns where two rows differ.
fn diff(a: &[ratatui::buffer::Cell], b: &[ratatui::buffer::Cell]) -> Vec<usize> {
    (0..a.len()).filter(|i| a[*i] != b[*i]).collect()
}

fn text(r: &[ratatui::buffer::Cell]) -> String {
    r.iter().map(|c| c.symbol()).collect()
}

fn delta(a: &mut App, s: &str) {
    a.handle_agent(AgentEvent::TextDelta { text: s.into() });
}

fn call(a: &mut App) {
    a.handle_agent(AgentEvent::ToolCall {
        id: "c1".into(),
        name: "read".into(),
        args: serde_json::json!({"path": "a.rs"}),
        summary: "read a.rs".into(),
    });
    a.handle_agent(AgentEvent::ToolStart { id: "c1".into() });
}

fn done(a: &mut App, t0: Instant, out: ToolOutput) {
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    call(a);
    let _ = at(a, t0, 400);
    a.handle_agent(AgentEvent::ToolDone { id: "c1".into(), output: out, elapsed_ms: 5 });
}

#[test]
fn fresh_ink_is_dimmer_at_the_tail_only_and_settles() {
    let (mut a, t0) = app(true);
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    delta(&mut a, "The quick brown");
    let _ = at(&mut a, t0, 400);
    delta(&mut a, " fox jumps over");
    let fresh = row(&at(&mut a, t0, 410), "quick");
    let settled = row(&at(&mut a, t0, 800), "quick");
    assert_eq!(text(&fresh), text(&settled), "same letters in the same cells");
    let cols = diff(&fresh, &settled);
    assert!(!cols.is_empty(), "the tail is dimmer while it lands");
    let first = settled.iter().position(|c| c.symbol() == "b").unwrap() + 4;
    assert!(cols.iter().all(|c| *c >= first), "only the end of the line moves: {cols:?}");
    assert!(!a.fx.ink.alive(a.now()), "the ink asks for no more frames once it settled");
    // the stream ends: the final frame is the one animations-off draws
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    let end_on = row(&at(&mut a, t0, 5000), "quick");
    let (mut b, u0) = app(false);
    b.handle_agent(AgentEvent::TurnStart { turn: 1 });
    delta(&mut b, "The quick brown");
    let _ = at(&mut b, u0, 400);
    delta(&mut b, " fox jumps over");
    let _ = at(&mut b, u0, 410);
    let _ = at(&mut b, u0, 800);
    b.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    assert_eq!(end_on, row(&at(&mut b, u0, 5000), "quick"));
}

/// The rows of `b` that show something, with the caret glyph taken out.
fn inked(b: &Buffer) -> Vec<String> {
    symbols(b).iter().map(|r| r.replace('▍', "").trim_end().to_string()).filter(|r| !r.trim().is_empty()).collect()
}

/// A streamed reply of `n` letters, drawn at 3 s.
fn streamed(animations: bool, n: usize) -> Vec<String> {
    let (mut a, t0) = app(animations);
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    delta(&mut a, &"x".repeat(n));
    inked(&at(&mut a, t0, 3000))
}

#[test]
fn the_caret_never_adds_a_line() {
    // the widest line that still fits on one row of the text area: one letter more wraps
    let fits =
        (1..80).take_while(|n| streamed(false, *n).iter().filter(|r| r.contains("xx")).count() < 2).last().unwrap();
    assert!(fits > 60, "the text area is wide: {fits}");
    for n in [fits - 1, fits, fits + 1] {
        let (on, off) = (streamed(true, n), streamed(false, n));
        assert_eq!(on.len(), off.len(), "{n} letters: the same rows are shown with and without motion");
        // the status bar spins differently on purpose; every row above it is the same
        assert_eq!(on[..on.len() - 1], off[..off.len() - 1], "{n} letters: the same text on every row");
    }
}

#[test]
fn nothing_asks_for_frames_once_the_stream_is_over() {
    for reason in [TurnEnd::Done, TurnEnd::Interrupted, TurnEnd::Error] {
        let (mut a, t0) = app(true);
        a.handle_agent(AgentEvent::TurnStart { turn: 1 });
        delta(&mut a, "Working on it");
        let _ = at(&mut a, t0, 400);
        delta(&mut a, " now");
        let _ = at(&mut a, t0, 410);
        assert!(a.tick_fx(), "the fresh ink asks for frames while it lands");
        a.handle_agent(AgentEvent::TurnEnd { reason });
        // frames as they come: the last one of an effect is the settled one, then none are asked for
        for ms in [600, 1200, 5000] {
            let _ = at(&mut a, t0, ms);
        }
        assert!(!a.tick_fx(), "{reason:?}: no frames once the effects ended");
        assert!(!a.fx.ink.alive(a.now()));
    }
}

/// The column of the colon of the last "OK:" in `r`.
fn colon_of(r: &[ratatui::buffer::Cell]) -> usize {
    text(r).chars().collect::<Vec<_>>().windows(3).rposition(|w| w == ['O', 'K', ':']).expect("OK: on the row") + 2
}

#[test]
fn fresh_ink_reaches_a_word_that_looks_like_a_time_stamp() {
    // the newest word is "OK:", its colon on byte 2 as in "12:34"; a real time stamp sits at the right edge
    let (mut a, t0) = app(true);
    assert!(a.timestamps);
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    delta(&mut a, "Sure");
    let _ = at(&mut a, t0, 400);
    delta(&mut a, ", OK:");
    let fresh = row(&at(&mut a, t0, 410), "OK:");
    let settled = row(&at(&mut a, t0, 800), "OK:");
    assert_eq!(text(&fresh), text(&settled));
    let tail = text(&settled).trim_end().chars().rev().take(5).collect::<String>();
    assert!(tail.contains(':'), "the time stamp is on the row: {tail}");
    let cols = diff(&fresh, &settled);
    assert!(cols.contains(&colon_of(&settled)), "the new word is dim while it lands: {cols:?}");
    let stamp = text(&settled).trim_end().chars().count() - 5;
    assert!(cols.iter().all(|c| *c < stamp), "the time stamp keeps its colour: {cols:?}");
}

#[test]
fn fresh_ink_reaches_a_line_too_narrow_for_a_time_stamp() {
    let (mut a, t0) = app(true);
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    delta(&mut a, &"x".repeat(70));
    let _ = at(&mut a, t0, 400);
    delta(&mut a, " OK:");
    let fresh = row(&at(&mut a, t0, 410), "OK:");
    let settled = row(&at(&mut a, t0, 800), "OK:");
    assert_eq!(text(&fresh), text(&settled));
    assert!(text(&settled).replace('▍', "").trim_end().ends_with("OK:"), "no stamp fits on this row");
    assert!(diff(&fresh, &settled).contains(&colon_of(&settled)), "the new word is dim while it lands");
}

#[test]
fn a_failed_card_flashes_its_mark_then_settles() {
    let (mut a, t0) = app(true);
    done(&mut a, t0, ToolOutput::error("nope"));
    let start = row(&at(&mut a, t0, 401), "read");
    let dim = row(&at(&mut a, t0, 490), "read");
    let settled = row(&at(&mut a, t0, 2000), "read");
    assert_eq!(text(&dim), text(&settled), "colour only");
    let moved = diff(&dim, &settled);
    assert!(!moved.is_empty() && moved.iter().all(|c| *c < 6), "only the mark: {moved:?}");
    assert_ne!(start, dim);
    assert!(!a.tick_fx());
    let (mut b, u0) = app(false);
    done(&mut b, u0, ToolOutput::error("nope"));
    let _ = at(&mut b, u0, 490);
    assert_eq!(settled, row(&at(&mut b, u0, 2000), "read"));
}

#[test]
fn the_note_of_a_finished_card_lights_once() {
    let (mut a, t0) = app(true);
    done(&mut a, t0, ToolOutput::ok("a\nb\nc"));
    let mid = row(&at(&mut a, t0, 550), "read");
    let settled = row(&at(&mut a, t0, 2000), "read");
    assert_eq!(text(&mid), text(&settled));
    let moved = diff(&mid, &settled);
    assert!(!moved.is_empty(), "the note is lit at 150 ms");
    assert!(!a.tick_fx());
    let (mut b, u0) = app(false);
    done(&mut b, u0, ToolOutput::ok("a\nb\nc"));
    let _ = at(&mut b, u0, 550);
    assert_eq!(settled, row(&at(&mut b, u0, 2000), "read"));
}

#[test]
fn the_thinking_header_settles_when_the_answer_starts() {
    let (mut a, t0) = app(true);
    a.show_reasoning = true;
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::ReasoningDelta { text: "let me think".into() });
    let _ = at(&mut a, t0, 10);
    delta(&mut a, "Done.");
    let _ = at(&mut a, t0, 20);
    let mid = row(&at(&mut a, t0, 120), "thinking");
    let settled = row(&at(&mut a, t0, 3000), "thinking");
    assert_eq!(text(&mid), text(&settled));
    assert!(!diff(&mid, &settled).is_empty(), "the header is still easing");
    assert!(!a.tick_fx());
}

#[test]
fn the_rail_of_an_interrupted_block_warms_once() {
    let (mut a, t0) = app(true);
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    delta(&mut a, "Working on it");
    let _ = at(&mut a, t0, 10);
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Interrupted });
    let mid = row(&at(&mut a, t0, 150), "Working");
    let settled = row(&at(&mut a, t0, 3000), "Working");
    assert_eq!(text(&mid), text(&settled));
    assert!(!diff(&mid, &settled).is_empty());
    assert!(!a.tick_fx());
}

#[test]
fn an_interrupt_during_a_tool_leaves_the_finished_reply_alone() {
    let (mut a, t0) = app(true);
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    delta(&mut a, "Finished reply");
    let _ = at(&mut a, t0, 400);
    call(&mut a);
    let _ = at(&mut a, t0, 500);
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Interrupted });
    let mid = row(&at(&mut a, t0, 650), "Finished");
    let settled = row(&at(&mut a, t0, 3500), "Finished");
    assert_eq!(mid, settled, "the rail of a reply that was done stays as it was");
    assert!(!a.tick_fx());
}

#[test]
fn an_interrupt_warms_only_the_reply_it_stopped_in() {
    let (mut a, t0) = app(true);
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    delta(&mut a, "First reply");
    let _ = at(&mut a, t0, 400);
    call(&mut a);
    a.handle_agent(AgentEvent::ToolDone { id: "c1".into(), output: ToolOutput::ok("fine"), elapsed_ms: 5 });
    let _ = at(&mut a, t0, 500);
    delta(&mut a, "Second reply");
    let _ = at(&mut a, t0, 600);
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Interrupted });
    let (mid, settled) = (at(&mut a, t0, 750), at(&mut a, t0, 3600));
    assert_eq!(row(&mid, "First"), row(&settled, "First"), "the earlier reply is not touched");
    let moved = diff(&row(&mid, "Second"), &row(&settled, "Second"));
    assert!(!moved.is_empty() && moved.iter().all(|c| *c < 3), "only the rail of the second reply: {moved:?}");
    assert!(!a.tick_fx());
}

#[test]
fn the_message_of_the_person_fades_in_from_the_input_background() {
    let (mut a, t0) = app(true);
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.push(nexo_tui::render::blocks::Block::User { text: "hello there".into(), at: None });
    let mid = row(&at(&mut a, t0, 60), "hello there");
    let settled = row(&at(&mut a, t0, 2000), "hello there");
    assert_eq!(text(&mid), text(&settled));
    assert!(!diff(&mid, &settled).is_empty());
    assert!(!a.tick_fx());
}
