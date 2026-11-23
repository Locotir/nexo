use super::*;
use crossterm::event::{MouseEvent, MouseEventKind};

fn wheel(a: &mut App, kind: MouseEventKind, x: u16, y: u16) {
    crate::event::mouse(a, MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
}

#[test]
fn wheel_updates_hover_without_pointer_motion() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    for i in 0..20 {
        a.push(Block::Shell { command: format!("echo {i}"), output: "done".into(), exit_code: Some(0) });
    }
    a.transcript_area = Rect::new(0, 1, 80, 6);
    a.layout(80);
    a.settle(6);
    a.scroll_to_top();
    a.pointer_moved(2, 1);
    assert_eq!(a.hover, Hover::Head(0));
    wheel(&mut a, MouseEventKind::ScrollDown, 2, 1);
    let expected = a.hover_at(2, 1);
    assert_eq!(a.hover, expected);
    assert_ne!(a.hover, Hover::Head(0));
    wheel(&mut a, MouseEventKind::ScrollUp, 2, 1);
    assert_eq!(a.hover, Hover::Head(0));
}

#[test]
fn animated_wheel_updates_hover_each_frame_without_pointer_motion() {
    let (mut a, _rx) = bare();
    a.opts.animations = true;
    let t0 = a.now();
    a.fx.clock = Some(t0);
    for i in 0..30 {
        a.push(Block::Shell { command: format!("echo {i}"), output: "done".into(), exit_code: Some(0) });
    }
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    a.scroll_to_line(0, false);
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let (x, y) = (a.transcript_area.x + 2, a.transcript_area.y);
    a.pointer_moved(x, y);
    let initial = a.hover;
    wheel(&mut a, MouseEventKind::ScrollDown, x, y);
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    for ms in [60, 120, 500] {
        a.fx.clock = Some(t0 + std::time::Duration::from_millis(ms));
        term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
        let expected = a.hover_at(x, y);
        assert_eq!(a.hover, expected, "hover at {ms}ms");
    }
    assert_ne!(a.hover, initial);
}

#[test]
fn drag_and_release_track_pointer_for_later_frames() {
    use crossterm::event::MouseButton;
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(Block::User { text: "first\nsecond".into(), at: None });
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let (x, y) = (a.transcript_area.x + 2, a.transcript_area.y);
    wheel(&mut a, MouseEventKind::Moved, x, y);
    wheel(&mut a, MouseEventKind::Down(MouseButton::Left), x, y);
    wheel(&mut a, MouseEventKind::Drag(MouseButton::Left), x + 1, y + 1);
    assert_eq!(a.pointer_pos, Some((x + 1, y + 1)));
    wheel(&mut a, MouseEventKind::Up(MouseButton::Left), x + 2, y + 1);
    assert_eq!(a.pointer_pos, Some((x + 2, y + 1)));
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let expected = a.hover_at(x + 2, y + 1);
    assert_eq!(a.hover, expected);
}

#[test]
fn assistant_wrapped_rows_do_not_hover() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(Block::Assistant {
        text: "a long assistant response that wraps over several rows of the transcript".into(),
        streaming: false,
        at: None,
        started: None,
        took_ms: None,
    });
    a.transcript_area = Rect::new(0, 1, 20, 20);
    a.layout(20);
    a.settle(20);
    assert_eq!(a.block_at_line(a.offsets[0] + 1), Some(0));
    for y in 1..=4 {
        a.pointer_moved(2, y);
        assert_eq!(a.hover, Hover::Nothing);
    }
}

#[test]
fn link_hover_takes_priority_over_message_hover() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(Block::User { text: "line one\nsee https://example.com/abc now".into(), at: None });
    a.transcript_area = Rect::new(0, 1, 80, 20);
    a.layout(80);
    a.settle(20);
    let line = a.offsets[0] + 1;
    let text = a.line_text(line).unwrap();
    let col = text.find("https").unwrap() as u16 + 2;
    a.pointer_moved(col, 1 + line as u16);
    assert!(matches!(a.hover, Hover::Link { .. }));
    a.pointer_moved(2, 1 + line as u16);
    assert_eq!(a.hover, Hover::Nothing);
}

#[test]
fn selection_remains_visible_over_message_hover() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(Block::User { text: "first line\nsecond line".into(), at: None });
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let x = a.transcript_area.x + 3;
    let y = a.transcript_area.y + (a.offsets[0] - a.view_top()) as u16;
    let mut selection = Selection::new(Pos { line: a.offsets[0], col: 3 });
    selection.head = Pos { line: a.offsets[0], col: 6 };
    a.selection = Some(selection);
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let selected = term.backend().buffer()[(x, y)].bg;
    a.pointer_moved(x, y + 1);
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    assert_eq!(term.backend().buffer()[(x, y)].bg, selected);
}

#[test]
#[ignore]
fn measure_hover_on_long_history() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    for i in 0..3000 {
        a.push(Block::User { text: format!("message {i} with several words\nsecond line"), at: None });
    }
    a.transcript_area = Rect::new(0, 1, 80, 30);
    a.layout(80);
    a.settle(30);
    let start = std::time::Instant::now();
    for i in 0..10000 {
        a.pointer_moved(4, 1 + (i % 25) as u16);
    }
    eprintln!("hover: {:?} per event over 3000 blocks", start.elapsed() / 10000);
}

#[test]
fn work_blocks_hover_on_all_content_rows_and_clear_on_reply() {
    use crate::render::blocks::{AgentRun, ToolStatus};
    let work = vec![
        Block::Shell { command: "echo work".into(), output: "first\nsecond".into(), exit_code: Some(0) },
        Block::Reasoning {
            text: "thinking  \nmore thought".into(),
            streaming: false,
            started: None,
            elapsed_ms: None,
            folded: false,
        },
        Block::Tool {
            id: "skill-1".into(),
            name: "skill".into(),
            summary: "load testing".into(),
            args: serde_json::json!({"name":"testing"}),
            status: ToolStatus::Done,
            progress: vec![],
            output: None,
            started: None,
            elapsed_ms: None,
            folded: false,
        },
        Block::Tool {
            id: "tool-1".into(),
            name: "bash".into(),
            summary: "check".into(),
            args: serde_json::json!({"command":"true"}),
            status: ToolStatus::Done,
            progress: vec![],
            output: None,
            started: None,
            elapsed_ms: None,
            folded: false,
        },
        Block::Agent(AgentRun {
            id: "run-1".into(),
            profile: "explore".into(),
            model: "mock/model".into(),
            effort: None,
            title: "investigate".into(),
            running: false,
        }),
    ];
    for block in work {
        let (mut a, _rx) = bare();
        a.opts.animations = false;
        a.push(block);
        a.push(Block::Assistant {
            text: "reply to you  \nsecond line".into(),
            streaming: false,
            at: None,
            started: None,
            took_ms: None,
        });
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
        let area = a.transcript_area;
        let n = a.shown(0, a.last_width);
        let before = term.backend().buffer().clone();
        a.pointer_moved(area.x + 2, area.y + (n - 1) as u16);
        assert_eq!(a.hover, Hover::Head(0), "{:?}", a.blocks[0]);
        term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
        for j in 0..n {
            let y = area.y + j as u16;
            assert!(
                (area.x..area.right() - 1).any(|x| before[(x, y)].style() != term.backend().buffer()[(x, y)].style()),
                "work row {j}"
            );
        }
        a.pointer_moved(area.x + 2, area.y + a.offsets[1] as u16);
        assert_eq!(a.hover, Hover::Nothing);
        term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
        for j in 0..n {
            let y = area.y + j as u16;
            for x in area.x..area.right() - 1 {
                assert_eq!(before[(x, y)].style(), term.backend().buffer()[(x, y)].style());
            }
        }
    }
}

#[test]
fn conversation_click_and_multiline_selection_work_without_hover() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(Block::Assistant {
        text: "first line  \nsecond line".into(),
        streaming: false,
        at: None,
        started: None,
        took_ms: None,
    });
    a.transcript_area = Rect::new(0, 1, 80, 20);
    a.layout(80);
    a.settle(20);
    a.pointer_moved(2, 1);
    assert_eq!(a.hover, Hover::Nothing);
    a.click_block(0, 0);
    assert_eq!(a.selected, Some(0));
    a.click_block(0, 0);
    assert!(a.is_folded(0));
    a.click_block(0, 0);
    assert!(!a.is_folded(0));
    a.layout(80);
    let mut selection = Selection::new(Pos { line: a.offsets[0], col: 0 });
    selection.head = Pos { line: a.offsets[0] + a.shown(0, a.last_width) - 1, col: 80 };
    a.selection = Some(selection);
    let text = a.selection_text().unwrap();
    assert!(text.contains("first line"));
    assert!(text.contains("second line"));
}

#[test]
fn conversation_rows_render_unchanged_including_pending_and_streaming() {
    for width in [24, 100] {
        for block in [
            Block::User {
                text: "first line\nsecond line with enough words to wrap on narrow screens".into(),
                at: None,
            },
            Block::Pending { text: "first line\nsecond line with enough words to wrap on narrow screens".into() },
            Block::Assistant {
                text: "first line  \nsecond line with enough words to wrap on narrow screens".into(),
                streaming: false,
                at: None,
                started: None,
                took_ms: None,
            },
            Block::Assistant {
                text: "first line  \nsecond line with enough words to wrap on narrow screens".into(),
                streaming: true,
                at: None,
                started: None,
                took_ms: None,
            },
        ] {
            let (mut a, _rx) = bare();
            a.opts.animations = false;
            a.push(block);
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 30)).unwrap();
            term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
            let area = a.transcript_area;
            let n = a.shown(0, a.last_width);
            let before = term.backend().buffer().clone();
            for j in 0..n {
                a.dirty = false;
                a.pointer_moved(area.x + 2, area.y + j as u16);
                assert_eq!(a.hover, Hover::Nothing);
                assert!(!a.dirty, "plain message motion needs no repaint");
                term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
                for row in 0..n {
                    for x in area.x..area.right() - 1 {
                        let y = area.y + row as u16;
                        assert_eq!(before[(x, y)].style(), term.backend().buffer()[(x, y)].style());
                    }
                }
            }
        }
    }
}

#[test]
fn assistant_link_remains_interactive_without_block_hover() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(Block::Assistant {
        text: "see https://example.com/abc now".into(),
        streaming: false,
        at: None,
        started: None,
        took_ms: None,
    });
    a.transcript_area = Rect::new(0, 1, 80, 20);
    a.layout(80);
    a.settle(20);
    let line = a.offsets[0];
    let col = a.line_text(line).unwrap().find("https").unwrap() as u16 + 2;
    a.pointer_moved(col, 1);
    assert!(matches!(a.hover, Hover::Link { .. }));
    assert!(a.link_hit(col, 1).is_some());
    a.pointer_moved(0, 1);
    assert_eq!(a.hover, Hover::Nothing);
}
