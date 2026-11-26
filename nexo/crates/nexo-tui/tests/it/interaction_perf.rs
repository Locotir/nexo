//! Event to completed TestBackend frame on a long conversation. No sleeps, model or physical terminal.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use nexo_core::agent::AgentEvent;
use nexo_tui::app::Hover;
use nexo_tui::render::blocks::{AgentRun, Block, ToolStatus};
use nexo_tui::{draw_frame, event, App, TuiOptions};
use ratatui::{backend::TestBackend, Terminal};

const CARDS: usize = 1000;
const N: usize = 20;
type Term = Terminal<TestBackend>;

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE })
}

// The timer excludes fixture setup, event construction, and the assertions inspecting the result.
fn frame(app: &mut App, term: &mut Term, action: impl FnOnce(&mut App)) -> Duration {
    let start = Instant::now();
    action(app);
    draw_frame(term, app).unwrap();
    start.elapsed()
}

fn measure(label: &str, sample: impl FnMut(usize) -> Duration) {
    measure_for(label, CARDS, sample);
}

fn measure_for(label: &str, blocks: usize, mut sample: impl FnMut(usize) -> Duration) {
    let mut times = Vec::with_capacity(N);
    for i in 0..N {
        times.push(sample(i));
        // Do not spend minutes collecting the rest if a seconds-per-frame regression comes back.
        if times.len() == 3 && times.iter().all(|t| *t >= Duration::from_secs(1)) {
            break;
        }
    }
    report(label, blocks, &mut times);
}

fn report(label: &str, blocks: usize, times: &mut [Duration]) {
    times.sort_unstable();
    let n = times.len();
    let ms = |p: usize| times[(n * p).div_ceil(100) - 1].as_secs_f64() * 1000.0;
    let (p50, p95, max) = (ms(50), ms(95), ms(100));
    // Bypass libtest's capture only for this short metric, so a green cargo test reports timings too.
    writeln!(io::stderr().lock(), "TUI {label:12} {blocks} blocks n={n} p50={p50:.3} p95={p95:.3} max={max:.3} ms")
        .unwrap();
    // A stall guard, not a frame-rate target. A slow outlier (CI preemption) cannot fail the test.
    assert!(p50 < 500.0, "{label}: median event-to-frame {p50:.2} ms (p95 {p95:.2}, max {max:.2})");
}

#[test]
fn long_chat_interaction_latency() {
    let cwd = tempfile::tempdir().unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let opts = TuiOptions { cwd: cwd.path().into(), animations: false, group_tools: true, ..Default::default() };
    let mut a = App::new(opts, tx);
    a.unlock();
    a.drop_banner();
    // Grouped launches with separate run records, as in the original multi-second hover/redraw regression.
    for i in 0..CARDS {
        a.push(Block::Tool {
            id: format!("t{i}"),
            name: "task".into(),
            summary: "inspect".into(),
            args: Default::default(),
            status: ToolStatus::Done,
            progress: vec![],
            output: None,
            started: None,
            elapsed_ms: None,
            folded: true,
        });
    }
    let heads = [a.blocks.len() - 2, a.blocks.len() - 1];
    for i in 0..CARDS {
        a.push(Block::Agent(AgentRun {
            id: format!("t{i}#{i}"),
            profile: "general".into(),
            model: "local/test".into(),
            effort: None,
            title: format!("task {i}"),
            running: true,
        }));
    }
    a.push(Block::User { text: "plain words ".repeat(60), at: None });
    let mut term = Term::new(TestBackend::new(100, 36)).unwrap();
    a.follow = true;
    draw_frame(&mut term, &mut a).unwrap();
    assert!(a.view_top() > 0);
    let area = a.transcript_area;
    let x = area.x + 1;
    let rows = heads.map(|h| {
        let off = a.offsets[h] - a.view_top();
        assert!(off < usize::from(area.height), "both foldable heads must be visible");
        area.y + off as u16
    });
    measure("hover", |i| {
        let (h, y) = (heads[i % 2], rows[i % 2]);
        let row =
            |t: &Term| (area.x..area.right() - 1).map(|c| t.backend().buffer()[(c, y)].style()).collect::<Vec<_>>();
        let before = row(&term);
        let ev = mouse(MouseEventKind::Moved, x, y);
        let d = frame(&mut a, &mut term, |a| event::handle(a, ev));
        assert_eq!(a.hover, Hover::Head(h));
        assert_ne!(row(&term), before, "the row actually lights up");
        d
    });
    measure("redraw", |_| frame(&mut a, &mut term, |_| {}));
    // A scheduled beat invalidates live run cards even without input. Keep it separate from a warm redraw.
    measure("tick", |_| {
        let before = a.spinner;
        let d = frame(&mut a, &mut term, |a| a.tick_frame(true));
        assert_ne!(a.spinner, before);
        d
    });
    measure("scroll", |i| {
        let top = a.view_top();
        let wheel = if i % 2 == 0 { MouseEventKind::ScrollUp } else { MouseEventKind::ScrollDown };
        let ev = mouse(wheel, x, rows[0]);
        let d = frame(&mut a, &mut term, |a| event::handle(a, ev));
        assert_ne!(a.view_top(), top);
        d
    });
    measure("typing", |i| {
        let ev = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let d = frame(&mut a, &mut term, |a| event::handle(a, ev));
        assert_eq!(a.input.text(), "x".repeat(i + 1));
        d
    });
    measure("resize", |i| {
        let w = if i % 2 == 0 { 80 } else { 100 };
        term.backend_mut().resize(w, 36); // the terminal changed size before delivering its notification
        let d = frame(&mut a, &mut term, |a| event::handle(a, Event::Resize(w, 36)));
        assert_eq!(a.term_width, w);
        assert!(!a.force_clear, "the resize was painted");
        d
    });
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    measure("stream", |i| {
        let word = format!("part{i:02}");
        let ev = AgentEvent::TextDelta { text: format!(" {word}") };
        let d = frame(&mut a, &mut term, |a| a.handle_agent(ev));
        assert!(nexo_tui::ui::rows_of(term.backend().buffer()).iter().any(|r| r.contains(&word)));
        d
    });
}

/// Exercise the real hover mutation and completed backend frame, not hover_at hit-testing alone.
/// Warm caches deliberately: this isolates pointer-driven invalidation from initial history layout.
#[test]
fn multiline_pointer_to_frame_latency() {
    for blocks in [1000, 5000] {
        let cwd = tempfile::tempdir().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let opts = TuiOptions { cwd: cwd.path().into(), animations: false, ..Default::default() };
        let mut a = App::new(opts, tx);
        a.unlock();
        a.drop_banner();
        for i in 0..blocks {
            a.push(Block::Shell {
                command: format!("echo work-{i}"),
                output: "first line\nsecond line".into(),
                exit_code: Some(0),
            });
        }
        let mut term = Term::new(TestBackend::new(100, 36)).unwrap();
        a.follow = true;
        draw_frame(&mut term, &mut a).unwrap();
        assert_eq!(a.hover, Hover::Nothing);
        assert!(a.view_top() > 0);
        let area = a.transcript_area;
        let heads = [blocks - 2, blocks - 1];
        let rows = heads.map(|h| {
            let off = a.offsets[h] - a.view_top();
            assert!(off + 1 < usize::from(area.height), "both message lines must be visible");
            area.y + off as u16
        });
        // Check actual rendered styles on BOTH lines, including removal from the previous message.
        let styles = |t: &Term, y: u16| {
            (y..=y + 1)
                .map(|row| {
                    (area.x..area.right() - 1).map(|x| t.backend().buffer()[(x, row)].style()).collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        let baseline = rows.map(|y| styles(&term, y));
        let mut pointer_times = Vec::with_capacity(N);
        let mut draw_times = Vec::with_capacity(N);
        measure_for("pointer+draw", blocks, |i| {
            let target = i % 2;
            // The run loop cleared the previous frame's repaint request.
            a.dirty = false;
            // Enter via the continuation line: a first-line-only highlight must fail this test.
            let start = Instant::now();
            a.pointer_moved(area.x + 2, rows[target] + 1);
            let pointer = start.elapsed();
            let requested_frame = a.dirty;
            let draw_start = Instant::now();
            draw_frame(&mut term, &mut a).unwrap();
            let draw = draw_start.elapsed();
            let total = start.elapsed();
            pointer_times.push(pointer);
            draw_times.push(draw);
            assert!(requested_frame, "changing the hovered message must request a frame");
            assert_eq!(a.hover, Hover::Head(heads[target]));
            let lit = styles(&term, rows[target]);
            for (line, unlit) in lit.iter().zip(&baseline[target]) {
                assert_ne!(line, unlit, "every message line must actually light up");
            }
            assert_eq!(styles(&term, rows[1 - target]), baseline[1 - target], "previous hover must clear");
            total
        });
        // These phases distinguish slow hit-testing from redraw work without instrumenting production code.
        report("pointer", blocks, &mut pointer_times);
        report("hover-draw", blocks, &mut draw_times);
    }
}

/// Selection was missing from the timed interaction cases. Alternate extension and
/// contraction so a no-op drag or a frame that keeps stale highlighting cannot pass.
/// Uses the same broad median stall guard as the other cases, not an FPS target.
#[test]
fn multiline_selection_drag_to_frame_latency() {
    use crossterm::event::MouseButton;

    for blocks in [1000, 5000] {
        let cwd = tempfile::tempdir().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let opts = TuiOptions { cwd: cwd.path().into(), animations: false, ..Default::default() };
        let mut a = App::new(opts, tx);
        a.unlock();
        a.drop_banner();
        for i in 0..blocks {
            a.push(Block::User { text: format!("history {i}"), at: None });
        }
        let selected = a.push(Block::User {
            text: "first selection line\nsecond selection line\nthird selection line".into(),
            at: None,
        });
        let mut term = Term::new(TestBackend::new(100, 36)).unwrap();
        a.follow = true;
        draw_frame(&mut term, &mut a).unwrap();
        assert!(a.view_top() > 0);
        let area = a.transcript_area;
        let y = area.y + (a.offsets[selected] - a.view_top()) as u16;
        assert!(y + 2 < area.bottom(), "all three selection rows must be visible");
        let x = area.x + 5;
        event::handle(&mut a, mouse(MouseEventKind::Down(MouseButton::Left), area.x + 2, y));
        draw_frame(&mut term, &mut a).unwrap();
        // Pressing puts the pointer over this message, so its unselected style
        // includes hover. Compare contraction against that same visual state.
        assert_eq!(a.hover, Hover::Head(selected));
        let unselected = term.backend().buffer()[(x, y + 2)].style();
        assert_ne!(unselected.bg, Some(a.theme.highlight_bg));
        let mut drag_times = Vec::with_capacity(N);
        let mut draw_times = Vec::with_capacity(N);
        measure_for("select+draw", blocks, |i| {
            let extended = i % 2 == 0;
            let end_y = y + if extended { 2 } else { 1 };
            let ev = mouse(MouseEventKind::Drag(MouseButton::Left), x + 8, end_y);
            a.dirty = false;
            let start = Instant::now();
            event::handle(&mut a, ev);
            let drag = start.elapsed();
            let requested_frame = a.dirty;
            let draw_start = Instant::now();
            draw_frame(&mut term, &mut a).unwrap();
            let drawing = draw_start.elapsed();
            let total = start.elapsed();
            drag_times.push(drag);
            draw_times.push(drawing);
            assert!(requested_frame, "each drag must request its changed selection frame");
            assert_eq!(a.selection.as_ref().unwrap().head.line, a.offsets[selected] + if extended { 2 } else { 1 });
            assert_eq!(term.backend().buffer()[(x, y + 1)].bg, a.theme.highlight_bg);
            if extended {
                assert_eq!(term.backend().buffer()[(x, y + 2)].bg, a.theme.highlight_bg);
            } else {
                assert_eq!(term.backend().buffer()[(x, y + 2)].style(), unselected, "contraction clears old highlight");
            }
            let copied = a.selection_text().unwrap();
            assert!(copied.contains("first selection line"));
            assert!(copied.contains("second"));
            assert_eq!(copied.contains("third"), extended);
            total
        });
        report("select-event", blocks, &mut drag_times);
        report("select-draw", blocks, &mut draw_times);
        // Do not release: production mouse-up copies to the system clipboard.
        // This benchmark intentionally measures drag/paint without that external side effect.
    }
}

/// Unlike the ignored model-switch diagnostic, this runs in the ordinary suite and
/// times ModelLimits dispatch and its completed frame separately on mixed history.
#[test]
fn model_limits_event_to_frame_latency() {
    for blocks in [1000, 5000] {
        let cwd = tempfile::tempdir().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let opts = TuiOptions { cwd: cwd.path().into(), animations: false, group_tools: false, ..Default::default() };
        let mut a = App::new(opts, tx);
        a.unlock();
        a.drop_banner();
        a.provider = "local".into();
        a.model = "initial".into();
        for i in 0..blocks {
            match i % 3 {
                0 => {
                    a.push(Block::User { text: format!("history {i}"), at: None });
                }
                1 => {
                    a.push(Block::Assistant {
                        text: "## Investigation\n\nSome **markdown** and `code`.\n\n```rust\nfn main() {}\n```".into(),
                        streaming: false,
                        at: None,
                        started: None,
                        took_ms: None,
                    });
                }
                _ => {
                    let id = format!("priced-{i}");
                    a.push(Block::Tool {
                        id: id.clone(),
                        name: "read".into(),
                        summary: "inspect source".into(),
                        args: Default::default(),
                        status: ToolStatus::Done,
                        progress: vec![],
                        output: Some(nexo_core::ToolOutput::ok("first line\nsecond line")),
                        started: None,
                        elapsed_ms: Some(1),
                        folded: false,
                    });
                    a.marks.insert(id.clone(), nexo_core::CallMark::Sent);
                    a.call_tokens.insert(id, 1000);
                }
            }
        }
        let mut term = Term::new(TestBackend::new(100, 36)).unwrap();
        a.follow = true;
        draw_frame(&mut term, &mut a).unwrap();
        assert!(a.view_top() > 0);
        let mut event_times = Vec::with_capacity(N);
        let mut draw_times = Vec::with_capacity(N);
        measure_for("limits+draw", blocks, |i| {
            let model = format!("limits-{i:02}");
            let window = if i % 2 == 0 { 128_000 } else { 272_000 };
            let ev = AgentEvent::ModelLimits {
                provider: "local".into(),
                model: model.clone(),
                pricing: Some(nexo_core::Pricing { input: (i + 1) as f64, output: 5.0, ..Default::default() }),
                context_window: Some(window),
                max_output: Some(8192),
                source: "catalogue".into(),
            };
            a.dirty = false;
            let start = Instant::now();
            a.handle_agent(ev);
            let dispatch = start.elapsed();
            let requested_frame = a.dirty;
            let draw_start = Instant::now();
            draw_frame(&mut term, &mut a).unwrap();
            let drawing = draw_start.elapsed();
            let total = start.elapsed();
            event_times.push(dispatch);
            draw_times.push(drawing);
            assert!(requested_frame, "new limits must request a frame");
            assert_eq!(a.model, model);
            assert_eq!(a.limits.as_ref().unwrap().context_window, Some(window));
            assert!(matches!(a.blocks.last(), Some(Block::Model { model: current, .. }) if current == &model));
            let rows = nexo_tui::ui::rows_of(term.backend().buffer()).join("\n");
            assert!(rows.contains(&format!("local/{model}")), "new model card must actually be painted: {rows}");
            total
        });
        report("limits-event", blocks, &mut event_times);
        report("limits-draw", blocks, &mut draw_times);
    }
}

#[test]
fn skill_load_event_to_frame_latency() {
    let cwd = tempfile::tempdir().unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let opts = TuiOptions { cwd: cwd.path().into(), animations: false, group_tools: true, ..Default::default() };
    let mut a = App::new(opts, tx);
    a.unlock();
    a.drop_banner();
    a.provider = "local".into();
    a.model = "test".into();
    let clock = Instant::now();
    a.fx.clock = Some(clock);
    for i in 0..CARDS {
        a.push(Block::User { text: format!("history {i}"), at: None });
    }
    let mut term = Term::new(TestBackend::new(100, 36)).unwrap();
    a.follow = true;
    draw_frame(&mut term, &mut a).unwrap();
    measure("skill", |i| {
        let id = format!("skill{i}");
        let call = AgentEvent::ToolCall {
            id: id.clone(),
            name: "skill".into(),
            args: serde_json::json!({"name":"Verify"}),
            summary: "skill Verify".into(),
        };
        let start = AgentEvent::ToolStart { id: id.clone() };
        // Large private body: folding must not render it just to throw it away.
        let done = AgentEvent::ToolDone {
            id,
            output: nexo_core::ToolOutput::ok("PRIVATE BODY\n".repeat(10000)),
            elapsed_ms: 12,
        };
        let elapsed = frame(&mut a, &mut term, |a| {
            a.handle_agent(call);
            a.handle_agent(start);
            a.handle_agent(done);
            a.fx.clock = Some(clock + Duration::from_millis((i as u64 + 1) * 1000));
            a.scroll_to_end();
        });
        let rows = nexo_tui::ui::rows_of(term.backend().buffer()).join("\n");
        assert!(rows.contains("Skill · Verify | loaded") && !rows.contains("PRIVATE BODY"), "{rows}");
        elapsed
    });
}

/// Search typing rescans the conversation; measure it separately from painting.
#[test]
fn search_edit_to_frame_latency() {
    for blocks in [1000, 5000] {
        let cwd = tempfile::tempdir().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = App::new(TuiOptions { cwd: cwd.path().into(), animations: false, ..Default::default() }, tx);
        a.unlock();
        a.drop_banner();
        for i in 0..blocks {
            let word = if i % 10 == 0 { "needlex" } else { "needle" };
            a.push(Block::User { text: format!("history {i} {word}"), at: None });
        }
        let mut term = Term::new(TestBackend::new(100, 36)).unwrap();
        draw_frame(&mut term, &mut a).unwrap();
        a.open_search();
        for c in "needle".chars() {
            event::handle(&mut a, Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        draw_frame(&mut term, &mut a).unwrap();
        let mut event_times = Vec::with_capacity(N);
        let mut draw_times = Vec::with_capacity(N);
        measure_for("search+draw", blocks, |i| {
            let narrow = i % 2 == 0;
            let key = if narrow { KeyCode::Char('x') } else { KeyCode::Backspace };
            let ev = Event::Key(KeyEvent::new(key, KeyModifiers::NONE));
            a.dirty = false;
            let start = Instant::now();
            event::handle(&mut a, ev);
            let dispatch = start.elapsed();
            let requested_frame = a.dirty;
            let draw_start = Instant::now();
            draw_frame(&mut term, &mut a).unwrap();
            let drawing = draw_start.elapsed();
            let total = start.elapsed();
            event_times.push(dispatch);
            draw_times.push(drawing);
            let search = a.search.as_ref().unwrap();
            let expected = if narrow { "needlex" } else { "needle" };
            assert!(requested_frame);
            assert_eq!(search.query, expected);
            assert_eq!(search.matches.len(), if narrow { blocks / 10 } else { blocks });
            let rows = nexo_tui::ui::rows_of(term.backend().buffer()).join("\n");
            assert!(rows.contains(&format!("search: {expected}")), "{rows}");
            total
        });
        report("search-event", blocks, &mut event_times);
        report("search-draw", blocks, &mut draw_times);
    }
}

/// A resize invalidates layout across the history, unlike a warm redraw.
#[test]
fn long_history_resize_to_frame_latency() {
    for blocks in [1000, 5000] {
        let cwd = tempfile::tempdir().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = App::new(TuiOptions { cwd: cwd.path().into(), animations: false, ..Default::default() }, tx);
        a.unlock();
        a.drop_banner();
        for i in 0..blocks {
            a.push(Block::User { text: format!("history {i} {}", "wrapping text ".repeat(12)), at: None });
        }
        let mut term = Term::new(TestBackend::new(100, 36)).unwrap();
        a.follow = true;
        draw_frame(&mut term, &mut a).unwrap();
        let mut events = Vec::with_capacity(N);
        let mut draws = Vec::with_capacity(N);
        measure_for("resize+draw", blocks, |i| {
            let width = if i % 2 == 0 { 80 } else { 100 };
            term.backend_mut().resize(width, 36);
            a.dirty = false;
            let start = Instant::now();
            event::handle(&mut a, Event::Resize(width, 36));
            let dispatch = start.elapsed();
            let requested = a.dirty;
            let draw_start = Instant::now();
            draw_frame(&mut term, &mut a).unwrap();
            let drawing = draw_start.elapsed();
            let total = start.elapsed();
            events.push(dispatch);
            draws.push(drawing);
            assert!(requested);
            assert_eq!(a.term_width, width);
            assert!(!a.force_clear);
            let rows = nexo_tui::ui::rows_of(term.backend().buffer()).join("\n");
            assert!(rows.contains(&format!("history {}", blocks - 1)), "latest message missing: {rows}");
            total
        });
        report("resize-event", blocks, &mut events);
        report("resize-draw", blocks, &mut draws);
    }
}

/// Streaming deltas must update the visible answer even with a long history.
/// Time event dispatch separately from the completed TestBackend frame.
#[test]
fn long_history_stream_to_frame_latency() {
    for blocks in [1000, 5000] {
        let cwd = tempfile::tempdir().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = App::new(TuiOptions { cwd: cwd.path().into(), animations: false, ..Default::default() }, tx);
        a.unlock();
        a.drop_banner();
        for i in 0..blocks {
            a.push(Block::User { text: format!("history {i}"), at: None });
        }
        let mut term = Term::new(TestBackend::new(100, 36)).unwrap();
        a.follow = true;
        a.handle_agent(AgentEvent::TurnStart { turn: 1 });
        draw_frame(&mut term, &mut a).unwrap();
        assert!(a.view_top() > 0);
        let mut events = Vec::with_capacity(N);
        let mut draws = Vec::with_capacity(N);
        measure_for("stream+draw", blocks, |i| {
            let word = format!(" chunk{i:02}");
            let ev = AgentEvent::TextDelta { text: word.clone() };
            a.dirty = false;
            let start = Instant::now();
            a.handle_agent(ev);
            let dispatch = start.elapsed();
            let requested = a.dirty;
            let draw_start = Instant::now();
            draw_frame(&mut term, &mut a).unwrap();
            let drawing = draw_start.elapsed();
            let total = start.elapsed();
            events.push(dispatch);
            draws.push(drawing);
            assert!(requested, "streaming delta must request a frame");
            let rows = nexo_tui::ui::rows_of(term.backend().buffer()).join("\n");
            assert!(rows.contains(word.trim()), "new delta must be painted: {rows}");
            total
        });
        report("stream-event", blocks, &mut events);
        report("stream-draw", blocks, &mut draws);
    }
}

/// Drawing progress and typing while the progress cue is on screen: same 100×36 terminal and scene.
#[test]
fn progress_animation_frame_and_typing_latency() {
    let cwd = tempfile::tempdir().unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let opts = TuiOptions { cwd: cwd.path().into(), animations: true, home_stars: false, ..Default::default() };
    let mut a = App::new(opts, tx);
    a.theme = nexo_tui::theme::Theme::nexo();
    a.unlock();
    a.drop_banner();
    for i in 0..CARDS {
        a.push(Block::User { text: format!("history {i}"), at: None });
    }
    let t0 = Instant::now();
    a.fx.clock = Some(t0);
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::ToolCall {
        id: "bench".into(),
        name: "bash".into(),
        args: serde_json::json!({}),
        summary: String::new(),
    });
    a.handle_agent(AgentEvent::ToolStart { id: "bench".into() });
    let mut term = Term::new(TestBackend::new(100, 36)).unwrap();
    draw_frame(&mut term, &mut a).unwrap();
    let mut draws = Vec::with_capacity(N);
    let mut keys = Vec::with_capacity(N);
    for i in 0..N {
        let base = t0 + Duration::from_secs(5 + i as u64 * 2);
        a.fx.clock = Some(base);
        a.handle_agent(AgentEvent::ToolProgress { id: "bench".into(), line: format!("step {i}") });
        a.fx.clock = Some(base + Duration::from_millis(80));
        assert!(a.fx.pulses.alive(a.now()), "measuring a live progress pulse");
        draws.push(frame(&mut a, &mut term, |_| {}));
        let ev = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        keys.push(frame(&mut a, &mut term, |a| event::handle(a, ev)));
        let y = a.editor_area.y;
        let draft: String = (0..100).map(|x| term.backend().buffer()[(x, y)].symbol()).collect();
        assert!(draft.contains(&"x".repeat(i + 1)), "{draft}");
    }
    report("pulse-frame", CARDS, &mut draws);
    report("pulse-type", CARDS, &mut keys);
}
