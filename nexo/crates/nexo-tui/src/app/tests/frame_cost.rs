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
    // the yardstick of the cost: wrapping the whole history at one width, which a drag must not do per width
    a.invalidate_all();
    let start = Clock::now();
    frame(&mut t, &mut a);
    let full = start.elapsed();
    eprintln!("one full re-render {full:?}");
    assert!(drag / 60 < full / 3, "a width of the drag cost {:?}, a full re-render {full:?}", drag / 60);
    assert!(steady < full / 20, "a steady frame cost {steady:?}, a full re-render {full:?}");
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

#[test]
fn a_steady_frame_at_a_new_width_does_not_search_the_history_again() {
    let mut a = long_history(600);
    a.opts.animations = true;
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    frame(&mut t, &mut a);
    a.follow = false;
    a.scroll = a.total_lines / 2;
    frame(&mut t, &mut a);
    t.backend_mut().resize(70, 30);
    t.resize(ratatui::layout::Rect::new(0, 0, 70, 30)).unwrap();
    // the blocks at the edge of the range settle over a few frames, each shifting the text above by a line
    for _ in 0..8 {
        frame(&mut t, &mut a);
    }
    assert!(a.estimated, "far blocks keep their estimate until the view comes near them");
    // a glide the reader started must run on, not be cut short by a frame that changed nothing
    let before = a.total_lines;
    let runs = a.near_runs;
    for _ in 0..3 {
        let now = a.now();
        a.scroll_fx.retarget(a.scroll as f64 + 3.0, now);
        a.layout(a.last_width);
        assert!(a.scroll_fx.is_moving(a.now()), "a steady layout snapped the scroll glide");
    }
    assert_eq!(a.total_lines, before);
    assert_eq!(a.near_runs, runs, "a layout that changed nothing searched the whole history again");
}

#[test]
fn far_blocks_estimated_at_a_new_width_settle_once_the_view_reaches_them() {
    let mut a = long_history(600);
    a.opts.animations = false;
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    frame(&mut t, &mut a);
    a.follow = false;
    a.scroll = 0;
    frame(&mut t, &mut a);
    t.backend_mut().resize(63, 30);
    t.resize(ratatui::layout::Rect::new(0, 0, 63, 30)).unwrap();
    frame(&mut t, &mut a);
    assert!(a.estimated);
    // walk the view down the history at the new width: every block it passes is wrapped for real
    while a.scroll < a.max_scroll(30) {
        a.scroll_by(25);
        frame(&mut t, &mut a);
    }
    for _ in 0..3 {
        frame(&mut t, &mut a);
    }
    let exact: usize = (0..a.blocks.len())
        .map(|i| {
            let n = a.shown(i, 63);
            n + a.gap_after(i, n)
        })
        .sum();
    assert_eq!(a.total_lines, exact, "offsets are exact once the view has been everywhere");
    assert!(!a.estimated, "nothing is left estimated");
}
