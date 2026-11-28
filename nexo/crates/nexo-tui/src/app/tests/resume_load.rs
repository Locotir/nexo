//! The loading cover of a resumed session: the transcript is not drawn while the history arrives,
//! and the draws of the cover stay far under the old per-frame layout cost.

use std::time::Instant;

use nexo_core::agent::AgentEvent;
use nexo_core::message::{Message, Part, Role};
use nexo_core::stream::Usage;
use nexo_core::tool::ToolOutput;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use super::super::*;
use super::bare;
use crate::draw_frame;
use crate::ui::rows_of;
use tokio::sync::mpsc;

fn replay(n: usize) -> (App, mpsc::UnboundedReceiver<AgentCommand>) {
    let (mut a, rx) = bare();
    a.opts.animations = false;
    a.handle_agent(AgentEvent::HistoryCleared);
    for i in 0..n {
        let role = if i % 2 == 0 { Role::User } else { Role::Assistant };
        a.handle_agent(AgentEvent::History {
            index: i,
            message: match role {
                Role::User => Message::user(format!("message {i}: a short line of the resumed session")),
                _ => Message::assistant(format!("message {i}: a short line of the resumed session")),
            },
        });
    }
    (a, rx)
}

/// What one draw of the cover costs, summed over the draws a load makes. The old path spent
/// 587-967 ms in a single draw of 4000 messages; the cover must stay far under that.
#[test]
fn drawing_while_a_long_resume_loads_stays_under_the_old_stall() {
    let (mut a, _rx) = replay(4000);
    assert!(a.resume_load.is_some(), "the cover opens on the first message after a clear");
    replay_tail(&mut a, 4000);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    // the old path, measured on this same history: the transcript drawn on every frame while it loads
    let before = draw_old_path_ms();
    let mut draws = 0u32;
    let mut draw_us = 0u128;
    let start = Instant::now();
    while a.resume_load.is_some() {
        let t = Instant::now();
        draw_frame(&mut term, &mut a).unwrap();
        draw_us += t.elapsed().as_micros();
        draws += 1;
        a.tick_frame(false);
        // the cover must not spin forever if a tick wraps nothing
        assert!(draws < 4000, "the layout made no progress");
    }
    let ms = draw_us as f64 / 1000.0;
    eprintln!(
        "RESUME cover: BEFORE {before:.1} ms, AFTER {ms:.1} ms ({draws} draws, {:.0} ms all in)",
        start.elapsed().as_secs_f64() * 1000.0
    );
    // the old stall was one draw of 587 ms; every draw of the cover together must stay well under it
    assert!(ms < 200.0, "drawing the cover took {ms:.1} ms, the old single draw took 587 ms");
    // the frame the cover drops shows the transcript, never a blank one
    draw_frame(&mut term, &mut a).unwrap();
    let rows = rows_of(term.backend().buffer());
    let text = rows.join("\n");
    assert!(text.contains("message 3999"), "the last message is on screen once the cover drops:\n{text}");
    assert!(!text.contains("Loading session"), "the cover is gone");
}

/// The stall the cover replaces: the same 4000 messages, the transcript drawn on every frame while
/// the history is still arriving. Returns the milliseconds those draws took together.
fn draw_old_path_ms() -> f64 {
    let (mut a, _rx) = replay(4000);
    // the cover is what changed: take it off, so each frame draws the transcript as it used to
    a.resume_load = None;
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let mut draw_us = 0u128;
    // a frame every 8 messages, the same spacing the stall was measured at
    for end in (8..4000).step_by(8) {
        let t = Instant::now();
        draw_frame(&mut term, &mut a).unwrap();
        draw_us += t.elapsed().as_micros();
        let _ = end;
    }
    draw_us as f64 / 1000.0
}

/// The cover stays down for a session that was never cleared: its first message is not a replay.
#[test]
fn a_fresh_sessions_first_message_does_not_raise_the_cover() {
    let (mut a, _rx) = bare();
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("hello") });
    assert!(a.resume_load.is_none(), "a first message is not a replay");
}

#[test]
fn the_cover_at_eighty_by_twenty_four_names_the_load_and_the_count() {
    let (mut a, _rx) = replay(10);
    // half laid out, so the bar and the count have something to say
    if let Some(load) = &mut a.resume_load {
        load.done = 5;
    }
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    draw_frame(&mut term, &mut a).unwrap();
    let rows = rows_of(term.backend().buffer());
    let text = rows.join("\n");
    assert!(text.contains("Loading session"), "{text}");
    assert!(text.contains("45%"), "{text}");
    assert!(text.contains("5 / 11"), "{text}");
    assert!(text.contains("Esc cancels"), "{text}");
    // the card sits in the middle, not on the first row
    assert!(rows[0].trim().is_empty(), "the cover is centred, top row was {:?}", rows[0]);
    eprintln!("--- cover 80x24 ---\n{text}\n---");
}

#[test]
fn esc_while_loading_stops_the_resume_and_shows_what_is_laid_out() {
    let (mut a, mut rx) = replay(20);
    a.tick_frame(false);
    let laid = a.resume_load.as_ref().map(|l| l.done).unwrap_or(0);
    assert!(laid > 0 && laid < 20, "a tick lays out a slice, not the whole history ({laid})");
    a.esc();
    assert!(a.resume_load.is_none(), "Esc drops the cover");
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Interrupt)), "Esc interrupts the agent");
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    draw_frame(&mut term, &mut a).unwrap();
    let text = rows_of(term.backend().buffer()).join("\n");
    assert!(!text.contains("Loading session"), "the cover is gone:\n{text}");
    assert!(text.contains("message 19"), "the transcript is on screen:\n{text}");
}

/// What a replay ends with: usage, then the context of the resumed session.
fn replay_tail(a: &mut App, n: usize) {
    a.handle_agent(AgentEvent::Usage {
        turn: Usage::default(),
        call: Usage::default(),
        session: Usage::default(),
        cost_usd: 0.0,
    });
    a.handle_agent(AgentEvent::Context {
        view_tokens: n as u64 * 20,
        history_tokens: n as u64 * 20,
        history_bytes: n as u64 * 80,
        view_bytes: n as u64 * 80,
        messages_in_view: n,
        messages_total: n,
        policy: "curator".into(),
        note: None,
        label: "curator idle".into(),
        detail: None,
    });
}

/// Tool results fold into a group. The cover counts every block, hidden calls included, and the last
/// message is in view once the cover drops.
#[test]
fn grouped_tool_results_keep_the_height_the_cover_counted() {
    let (mut a, _rx) = bare();
    a.opts.group_tools = true;
    a.handle_agent(AgentEvent::HistoryCleared);
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("look around") });
    a.handle_agent(AgentEvent::History {
        index: 1,
        message: Message::new(
            Role::Assistant,
            vec![
                Part::tool_call("c1", "bash", serde_json::json!({"command": "ls src"})),
                Part::tool_call("c2", "bash", serde_json::json!({"command": "rg count"})),
            ],
        ),
    });
    a.handle_agent(AgentEvent::History {
        index: 2,
        message: Message::tool_results(vec![
            Part::tool_result("c1", "main.rs\nlib.rs\napp.rs", false),
            Part::tool_result("c2", "src/app.rs:12: count\nsrc/lib.rs:4: count", false),
        ]),
    });
    a.handle_agent(AgentEvent::ToolCall {
        id: "c1".into(),
        name: "bash".into(),
        args: serde_json::json!({"command": "ls src"}),
        summary: "ls src".into(),
    });
    a.handle_agent(AgentEvent::ToolDone {
        id: "c1".into(),
        output: ToolOutput::ok("main.rs\nlib.rs\napp.rs"),
        elapsed_ms: 0,
    });
    a.handle_agent(AgentEvent::ToolCall {
        id: "c2".into(),
        name: "bash".into(),
        args: serde_json::json!({"command": "rg count"}),
        summary: "rg count".into(),
    });
    a.handle_agent(AgentEvent::ToolDone {
        id: "c2".into(),
        output: ToolOutput::ok("src/app.rs:12: count\nsrc/lib.rs:4: count"),
        elapsed_ms: 0,
    });
    let width = a.last_width;
    // the cover wraps a slice per tick and keeps the height it counted for each block
    for _ in 0..8 {
        a.tick_frame(false);
    }
    let counted: Vec<Option<usize>> = (0..a.blocks.len())
        .map(|i| a.resume_load.as_ref().expect("a replay with tools raises the cover").height(i, width))
        .collect();
    for (i, counted) in counted.iter().enumerate() {
        let counted = counted.expect("the cover counted this block");
        assert_eq!(a.height(i, width), counted, "block {i} has the height the cover counted");
    }
    assert!(a.blocks.iter().any(|b| matches!(b, Block::Group { .. })), "the calls fold into a group");
    let n = a.blocks.len();
    replay_tail(&mut a, n);
    for _ in 0..8 {
        a.tick_frame(false);
    }
    assert!(a.resume_load.is_none(), "the cover drops once the grouped history is wrapped");
    a.transcript_area = ratatui::layout::Rect::new(0, 0, width as u16, 24);
    a.layout(width);
    let group = a.blocks.iter().position(|b| matches!(b, Block::Group { .. })).unwrap();
    assert_eq!(a.block_at_line(a.offsets[group]), Some(group), "the group is visible after layout");
}

#[test]
fn mid_session_replay_finishes_before_context_rebuild() {
    let (mut a, _rx) = bare();
    a.handle_agent(AgentEvent::HistoryCleared);
    a.handle_agent(AgentEvent::SessionChanged { session_id: "resumed".into(), title: String::new() });
    for i in 0..10 {
        a.handle_agent(AgentEvent::History { index: i, message: Message::user(format!("message {i}")) });
    }
    for _ in 0..8 {
        a.tick_frame(false);
    }
    assert!(a.resume_load.is_some(), "the cover stays until replay ends");
    a.handle_agent(AgentEvent::Usage {
        turn: Usage::default(),
        call: Usage::default(),
        session: Usage::default(),
        cost_usd: 0.0,
    });
    for _ in 0..8 {
        a.tick_frame(false);
    }
    assert!(a.resume_load.is_none(), "the usage summary ends the replay without waiting for context or Esc");
}
