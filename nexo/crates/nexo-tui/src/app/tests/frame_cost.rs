//! The cost of a frame on a long history: a steady frame and a drag of the window edge.

use super::*;
use std::time::Instant as Clock;

const TEXT: &str = "A paragraph of **markdown** with `code` and a [link](https://example.com), long enough to wrap \
                    over several rows when the window is narrow, so that a new width changes its height.\n\n\
                    - first item of a list\n- second item of a list that is a little longer than the first\n\n\
                    ```rust\nfn main() { println!(\"hello\"); }\n```";

fn long_history(blocks: usize) -> App {
    let (mut a, _rx) = bare();
    for i in 0..blocks / 2 {
        a.push(Block::User { text: format!("question {i}: how does this work?"), at: None });
        a.push(Block::Assistant {
            text: format!("Answer {i}. {TEXT}"),
            streaming: false,
            at: None,
            started: None,
            took_ms: None,
        });
    }
    a
}

fn frame(t: &mut ratatui::Terminal<ratatui::backend::TestBackend>, a: &mut App) {
    crate::draw_frame(t, a).unwrap();
}

/// How many blocks hold rendered lines.
fn rendered(a: &App) -> usize {
    cached(a)
}

#[test]
fn a_drag_of_the_window_edge_renders_only_what_is_near_the_view() {
    let mut a = long_history(3000);
    a.opts.animations = false;
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
    frame(&mut t, &mut a);
    let mid = a.total_lines / 2;
    a.follow = false;
    a.scroll = mid;
    frame(&mut t, &mut a);

    let start = Clock::now();
    for w in (60..120).rev() {
        t.backend_mut().resize(w, 36);
        t.resize(ratatui::layout::Rect::new(0, 0, w, 36)).unwrap();
        frame(&mut t, &mut a);
    }
    let drag = start.elapsed();

    let start = Clock::now();
    for _ in 0..50 {
        frame(&mut t, &mut a);
    }
    let steady = start.elapsed() / 50;
    let start = Clock::now();
    for _ in 0..50 {
        a.layout(119);
    }
    let layout = start.elapsed() / 50;
    let start = Clock::now();
    for _ in 0..50 {
        a.evict_far(a.scroll, 36);
    }
    let evict = start.elapsed() / 50;
    eprintln!("layout alone {layout:?}, eviction alone {evict:?}");
    eprintln!("3000 blocks: drag of 60 widths {drag:?}, steady frame {steady:?}, rendered blocks {}", rendered(&a));
    assert!(rendered(&a) < 400, "{} blocks hold lines after the drag", rendered(&a));
}

#[test]
fn the_line_at_the_top_stays_under_the_reader_through_a_resize() {
    let mut a = long_history(600);
    a.opts.animations = false;
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    frame(&mut t, &mut a);
    a.follow = false;
    a.scroll = a.total_lines / 2;
    frame(&mut t, &mut a);
    let top_block = |a: &App| a.offsets.partition_point(|o| *o <= a.scroll).checked_sub(1);
    let before = top_block(&a).unwrap();
    for w in [90u16, 77, 64, 50, 80, 100] {
        t.backend_mut().resize(w, 30);
        t.resize(ratatui::layout::Rect::new(0, 0, w, 30)).unwrap();
        frame(&mut t, &mut a);
        assert_eq!(top_block(&a), Some(before), "width {w}");
    }
    // the estimate gives way to the real height: once settled the offsets are exact
    let exact: usize = (0..a.blocks.len())
        .map(|i| {
            let n = a.shown(i, 99);
            n + a.gap_after(i, n)
        })
        .sum();
    assert_eq!(a.total_lines, exact);
}
