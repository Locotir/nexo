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
fn assistant_reply_rows_stay_plain() {
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
        assert_eq!(a.hover, Hover::Nothing, "the model's reply is not highlighted");
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
    assert_eq!(a.hover, Hover::Head(0));
}

#[test]
fn hovered_link_is_distinct_while_its_message_stays_highlighted() {
    use ratatui::style::Modifier;
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(Block::User { text: "first line\nsee https://example.com/abc now".into(), at: None });
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let area = a.transcript_area;
    let y = area.y + 1;
    let col = a.line_text(a.offsets[0] + 1).unwrap().find("https").unwrap() as u16;
    let x = area.x + col;
    let before = term.backend().buffer().clone();
    a.pointer_moved(x, y);
    assert!(matches!(a.hover, Hover::Link { .. }));
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let after = term.backend().buffer();
    assert_ne!(before[(area.x + 2, area.y)].style(), after[(area.x + 2, area.y)].style());
    assert!(after[(x, y)].modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn monochrome_link_hover_differs_from_message_hover() {
    use ratatui::style::Modifier;
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.theme = crate::theme::Theme::mono();
    a.push(Block::User { text: "first line\nsee https://example.com/abc now".into(), at: None });
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let area = a.transcript_area;
    let y = area.y + 1;
    let col = a.line_text(a.offsets[0] + 1).unwrap().find("https").unwrap() as u16;
    let x = area.x + col;
    a.pointer_moved(x, y);
    assert!(matches!(a.hover, Hover::Link { .. }));
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let buf = term.backend().buffer();
    assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
    assert!(!buf[(area.x + 2, y)].modifier.contains(Modifier::BOLD));
    assert_ne!(buf[(x, y)].style(), buf[(area.x + 2, y)].style());
}

#[test]
fn selection_remains_visible_over_hovered_link() {
    use ratatui::style::Modifier;
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(Block::User { text: "first line\nsee https://example.com/abc now".into(), at: None });
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let area = a.transcript_area;
    let line = a.offsets[0] + 1;
    let col = a.line_text(line).unwrap().find("https").unwrap();
    let (x, y) = (area.x + col as u16, area.y + 1);
    let mut selection = Selection::new(Pos { line, col });
    selection.head = Pos { line, col: col + 5 };
    a.selection = Some(selection);
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let selected_bg = term.backend().buffer()[(x, y)].bg;
    a.pointer_moved(x, y);
    assert!(matches!(a.hover, Hover::Link { .. }));
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let selected_link = &term.backend().buffer()[(x, y)];
    assert_eq!(selected_link.bg, selected_bg);
    assert!(selected_link.modifier.contains(Modifier::UNDERLINED));
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
        assert_eq!(a.hover, Hover::Nothing, "the reply below stays plain");
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
fn conversation_click_and_multiline_selection_work_with_hover() {
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
    assert_eq!(a.hover, Hover::Nothing, "the reply is not highlighted, yet still clicks and folds");
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
fn conversation_rows_highlight_including_pending() {
    for width in [24, 100] {
        for block in [
            Block::User {
                text: "first line\nsecond line with enough words to wrap on narrow screens".into(),
                at: None,
            },
            Block::Pending { text: "first line\nsecond line with enough words to wrap on narrow screens".into() },
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
                assert_eq!(a.hover, Hover::Head(0));
                assert_eq!(a.dirty, j == 0, "only entering the block needs repaint");
                term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
                for row in 0..n {
                    let y = area.y + row as u16;
                    assert!(
                        (area.x..area.right() - 1)
                            .any(|x| before[(x, y)].style() != term.backend().buffer()[(x, y)].style()),
                        "row {row}"
                    );
                }
            }
        }
    }
}

#[test]
fn assistant_link_remains_interactive_with_block_hover() {
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

/// TestBackend timings isolate event handling and CPU drawing, not PTY/display latency.
#[test]
#[ignore = "manual before/after hover latency measurement; no wall-clock assertions"]
fn measure_vertical_hover_on_wrapped_paragraph() {
    use std::time::{Duration, Instant};
    for (history, scope) in [(1_000, "last_row"), (1_000, "logical_line"), (5_000, "last_row"), (5_000, "logical_line")]
    {
        let (mut a, _rx) = bare();
        a.opts.animations = false;
        for i in 0..history {
            a.push(Block::User { text: format!("previous request {i}"), at: None });
        }
        let paragraph =
            a.push(Block::User { text: "plain text with several words and no address ".repeat(100), at: None });
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 36)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
        a.scroll_to_line(a.offsets[paragraph], false);
        term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
        let (x, y) = (a.transcript_area.x + 4, a.transcript_area.y);
        let mut events = Vec::new();
        let mut draws = Vec::new();
        let mut combined = Vec::new();
        // Warm layout/cache; alternate rows so a last-row-only negative cache cannot hide the cost.
        for i in 0..220 {
            if scope == "last_row" {
                // Reproduce the previous negative-cache scope on the same fixture/build.
                a.looks[paragraph].no_link_rows = a.pointer_pos.map(|(x, y)| {
                    let local = a.pos_at(x, y).line - a.offsets[paragraph];
                    (local, local + 1)
                });
            }
            let start = Instant::now();
            wheel(&mut a, MouseEventKind::Moved, x, y + (i % 20));
            let event = start.elapsed();
            let draw = Instant::now();
            term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
            let drawing = draw.elapsed();
            let total = start.elapsed();
            assert_eq!(a.hover, Hover::Head(paragraph));
            if i >= 20 {
                events.push(event);
                draws.push(drawing);
                combined.push(total);
            }
        }
        let report = |name: &str, mut samples: Vec<Duration>| {
            samples.sort_unstable();
            let at = |p: usize| samples[(samples.len() * p).div_ceil(100) - 1];
            eprintln!(
                "wrapped hover {history} blocks, scope={scope}, {name}: n={} p50={:?} p95={:?} max={:?}",
                samples.len(),
                at(50),
                at(95),
                samples.last().unwrap()
            );
        };
        report("event", events);
        report("draw", draws);
        report("event+draw", combined);
    }
}

#[test]
fn no_link_cache_covers_the_logical_line_and_resets_on_render() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    let paragraph = a.push(Block::User { text: "plain words without a link ".repeat(80), at: None });
    a.transcript_area = Rect::new(0, 1, 80, 40);
    a.layout(80);
    a.scroll_to_top();
    let n = a.cache[paragraph].as_ref().unwrap().1;
    assert!((20..64).contains(&n), "fixture must fit the complete backward scan: {n}");
    assert!(a.link_hit(5, 2).is_none());
    assert_eq!(a.looks[paragraph].no_link_rows, Some((0, n)));
    for row in [0, 10, n - 1, 1] {
        a.dirty = false;
        a.pointer_moved(5, row as u16 + 1);
        assert_eq!(a.looks[paragraph].no_link_rows, Some((0, n)));
        assert_eq!(a.hover, Hover::Head(paragraph));
        assert_eq!(a.dirty, row == 0);
    }
    // A resize and a new render cannot reuse row numbers from a previous width.
    a.layout(60);
    assert_eq!(a.looks[paragraph].no_link_rows, None);
    assert!(a.link_hit(5, 2).is_none());
    assert!(a.looks[paragraph].no_link_rows.is_some());
    if let Block::User { text, .. } = &mut a.blocks[paragraph] {
        *text = "see https://example.com/new".into();
    }
    a.invalidate(paragraph);
    a.layout(60);
    let text = a.line_text(0).unwrap();
    let col = text.find("https").unwrap() as u16;
    assert_eq!(a.link_hit(col, 1).unwrap().url, "https://example.com/new");
    assert_eq!(a.looks[paragraph].no_link_rows, None);
}

#[test]
fn no_link_cache_does_not_hide_adjacent_or_split_urls() {
    for text in [
        format!("{}\nhttps://example.com/end", "plain words ".repeat(30)),
        format!("{}https://example.com/end", "word ".repeat(15)),
        format!("{}WWW.example.com/end", "word ".repeat(15)),
    ] {
        let (mut a, _rx) = bare();
        a.opts.animations = false;
        a.push(Block::User { text, at: None });
        a.transcript_area = Rect::new(0, 1, 80, 40);
        a.layout(80);
        a.scroll_to_top();
        a.pointer_moved(0, 1);
        let mut found = false;
        for row in 0..a.total_lines.min(40) {
            for col in 0..80 {
                found |= a.link_hit(col, row as u16 + 1).is_some();
            }
        }
        assert!(found, "a negative answer on another row must not hide the URL: {:?}", a.blocks[0]);
    }
}

#[test]
fn no_link_cache_keeps_single_row_scope_for_truncated_logical_lines() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(Block::User { text: "plain words without links ".repeat(600), at: None });
    a.transcript_area = Rect::new(0, 1, 80, 40);
    a.layout(80);
    a.scroll_to_top();
    assert!(a.link_hit(5, 2).is_none());
    assert_eq!(a.looks[0].no_link_rows, Some((1, 2)));
    a.scroll_to_line(100, false);
    assert!(a.link_hit(5, 2).is_none());
    assert_eq!(a.looks[0].no_link_rows, Some((101, 102)));
}

#[test]
fn no_link_cache_keeps_urls_whose_prefix_is_soft_wrapped() {
    for url in ["https://example.com/end", "WWW.example.com/end"] {
        let (mut a, _rx) = bare();
        a.opts.animations = false;
        a.push(Block::User { text: format!("plain\n{url}"), at: None });
        a.transcript_area = Rect::new(0, 1, 7, 40);
        a.layout(7);
        a.scroll_to_top();
        assert!(a.link_hit(0, 1).is_none());
        let rows = a.cache[0].as_ref().unwrap().2.as_ref().unwrap();
        assert!(!rows.iter().any(|line| crate::render::markdown::text_of(line).contains(url)));
        let mut found = false;
        for row in 1..a.total_lines.min(40) {
            for col in 0..7 {
                found |= a.link_hit(col, row as u16 + 1).is_some();
            }
        }
        assert!(found, "a split URL prefix must survive the negative fast path: {url}");
    }
}

#[test]
fn no_link_cache_preserves_unicode_cells_and_urls() {
    for width in [18, 80] {
        let (mut a, _rx) = bare();
        a.opts.animations = false;
        let url = "https://example.com/路径/🙂/e\u{301}";
        a.push(Block::User {
            text: format!("{}\n漢字 🙂 e\u{301} {url} done", "漢字 👩‍💻 e\u{301} plain ".repeat(6)),
            at: None,
        });
        a.transcript_area = Rect::new(0, 1, width, 100);
        a.layout(width as usize);
        a.scroll_to_top();
        assert!(a.link_hit(2, 1).is_none());
        let negative = a.looks[0].no_link_rows.unwrap();
        assert!(negative.1 > 1, "Unicode paragraph must wrap");
        let mut found = false;
        for row in 0..a.total_lines {
            for col in 0..width {
                let cached = a.link_hit(col, row as u16 + 1);
                a.looks[0].no_link_rows = None;
                let cold = a.link_hit(col, row as u16 + 1);
                assert_eq!(cached, cold, "width={width}, row={row}, col={col}");
                if let Some(hit) = cached {
                    assert_eq!(hit.url, url);
                    found = true;
                }
                a.looks[0].no_link_rows = Some(negative);
            }
        }
        assert!(found, "Unicode URL remains hittable at width {width}");
    }
}

#[test]
fn no_link_cache_preserves_wrapped_markdown_label() {
    use ratatui::style::Modifier;
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    let url = "https://example.com/guide";
    a.push(Block::Assistant {
        text: format!("plain paragraph without links\n\n漢字 🙂 [read the wrapped 漢字 guide 🙂 now]({url})"),
        streaming: false,
        at: None,
        started: None,
        took_ms: None,
    });
    a.transcript_area = Rect::new(0, 1, 24, 80);
    a.layout(24);
    a.scroll_to_top();
    assert!(a.link_hit(0, 1).is_none());
    let negative = a.looks[0].no_link_rows.unwrap();
    let rows = a.cache[0].as_ref().unwrap().2.as_ref().unwrap();
    let mut label_cells = Vec::new();
    for (row, line) in rows.iter().enumerate() {
        let mut col = 0;
        for span in &line.spans {
            let width = crate::render::wrap::width(&span.content);
            if span.style.add_modifier.contains(Modifier::UNDERLINED) {
                label_cells.extend((col..col + width).map(|col| (row, col)));
            }
            col += width;
        }
    }
    let label_rows: std::collections::BTreeSet<_> = label_cells.iter().map(|&(row, _)| row).collect();
    assert!(label_rows.len() >= 2, "the Markdown label itself must wrap");
    // Compare every label cell against an uncached lookup: existing wrapped-label
    // hit-testing limitations are independent of the negative-cache optimization.
    let mut found = false;
    for (row, col) in label_cells {
        a.looks[0].no_link_rows = Some(negative);
        let cached = a.link_hit(col as u16, row as u16 + 1);
        a.looks[0].no_link_rows = None;
        let cold = a.link_hit(col as u16, row as u16 + 1);
        assert_eq!(cached, cold, "row={row}, col={col}");
        if let Some(hit) = cached {
            assert_eq!(hit.url, url, "row={row}, col={col}");
            found = true;
        }
    }
    assert!(found, "wrapped Markdown label must retain its existing link hits");
}

#[test]
fn no_link_cache_stays_block_relative_when_previous_block_grows() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    let previous = a.push(Block::User { text: "short".into(), at: None });
    let target = a.push(Block::User {
        text: format!("{}\nhttps://example.com/after-growth", "plain words ".repeat(12)),
        at: None,
    });
    a.transcript_area = Rect::new(0, 1, 30, 100);
    a.layout(30);
    a.scroll_to_top();
    let old_offset = a.offsets[target];
    assert!(a.link_hit(2, old_offset as u16 + 1).is_none());
    let negative = a.looks[target].no_link_rows.unwrap();
    if let Block::User { text, .. } = &mut a.blocks[previous] {
        *text = "growing previous block ".repeat(10);
    }
    a.invalidate(previous);
    a.layout(30);
    a.scroll_to_top();
    assert!(a.offsets[target] > old_offset);
    assert_eq!(a.looks[target].no_link_rows, Some(negative));
    assert!(a.link_hit(2, a.offsets[target] as u16 + 1).is_none());
    let mut found = false;
    for row in a.offsets[target]..a.total_lines {
        for col in 0..30 {
            if let Some(hit) = a.link_hit(col, row as u16 + 1) {
                assert_eq!(hit.url, "https://example.com/after-growth");
                found = true;
            }
        }
    }
    assert!(found, "moving a cached block must not hide its later URL");
}

#[test]
fn no_link_cache_scope_changes_at_exactly_64_and_65_rows() {
    for n in [64, 65] {
        let (mut a, _rx) = bare();
        a.opts.animations = false;
        // User rows reserve two cells for the bar: eight unbroken ASCII cells per row.
        a.push(Block::User { text: "x".repeat(8 * n), at: None });
        a.transcript_area = Rect::new(0, 1, 10, 100);
        a.layout(10);
        a.scroll_to_top();
        assert_eq!(a.cache[0].as_ref().unwrap().1, n, "exact boundary fixture");
        for row in [0, n - 1, 1] {
            a.looks[0].no_link_rows = None;
            assert!(a.link_hit(3, row as u16 + 1).is_none());
            let expected = if n == 64 { (0, 64) } else { (row, row + 1) };
            assert_eq!(a.looks[0].no_link_rows, Some(expected), "n={n}, row={row}");
            // Repeated queries use the same safe range.
            assert!(a.link_hit(3, row as u16 + 1).is_none());
            assert_eq!(a.looks[0].no_link_rows, Some(expected));
        }
    }
}
