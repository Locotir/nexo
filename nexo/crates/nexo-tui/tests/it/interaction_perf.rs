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
            // Markdown assistant messages need a hard break; a soft newline renders as one row.
            let text = format!("message {i}, first line  \nsecond line of the same message");
            a.push(if i % 2 == 0 {
                Block::User { text, at: None }
            } else {
                Block::Assistant { text, streaming: false, at: None, started: None, took_ms: None }
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
