//! The diff side panel and the `/diff` viewer in motion: a mid-effect frame differs from the settled one in
//! style only, the settled frame equals the animations-off frame, and nothing asks for a frame afterwards.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_tui::app::ChangedFile;
use nexo_tui::diffview::collect::{parse_unified, Area, Collected};
use nexo_tui::diffview::DiffView;
use nexo_tui::fx_diff::{CHANGE_MS, JUMP_MS, NEW_MS, OPEN_MS};
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
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        providers: vec![],
        theme: "dark".into(),
        mouse: true,
        show_reasoning: true,
        timestamps: false,
        fold_after: 4,
        group_tools: true,
        complete_words: true,
        suggest: false,
        keys: Default::default(),
        notify_after: 15,
        animations,
        status_line: vec![],
        status_command: String::new(),
        vim: false,
        prompt: None,
        session_id: Some("20261102-100000-ef56".into()),
        sessions: vec![],
        skills: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}

fn cf(path: &str, a: usize, r: usize) -> ChangedFile {
    ChangedFile { path: path.into(), added: a, removed: r }
}

fn frame(t: &mut Terminal<TestBackend>, a: &mut App, at: Instant) -> Buffer {
    a.fx.clock = Some(at);
    a.tick_fx();
    draw_frame(t, a).unwrap();
    t.backend().buffer().clone()
}

fn text(b: &Buffer) -> String {
    b.content().iter().map(|c| c.symbol()).collect()
}

fn ms(t0: Instant, n: u64) -> Instant {
    t0 + Duration::from_millis(n)
}

#[test]
fn the_panel_fades_in_and_settles_to_the_still_frame() {
    let t0 = Instant::now();
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut a = app(true);
    a.changed = vec![cf("src/a.rs", 3, 1)];
    frame(&mut t, &mut a, t0);
    a.show_files = true;
    let first = frame(&mut t, &mut a, ms(t0, 10));
    let mid = frame(&mut t, &mut a, ms(t0, 10 + OPEN_MS / 2));
    let settled = frame(&mut t, &mut a, ms(t0, 10 + OPEN_MS + 20));
    assert_ne!(first, settled);
    assert_ne!(mid, settled);
    assert_eq!(text(&mid), text(&settled), "style only");
    assert!(!a.tick_fx(), "rests after the fade");
    let mut s = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut b = app(false);
    b.changed = vec![cf("src/a.rs", 3, 1)];
    b.show_files = true;
    let still = frame(&mut s, &mut b, t0);
    same(&settled, &still);
    assert!(!b.tick_fx());
}

#[test]
fn an_edited_file_is_lit_and_its_figures_roll() {
    let t0 = Instant::now();
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut a = app(true);
    a.changed = vec![cf("src/a.rs", 3, 1)];
    a.show_files = true;
    frame(&mut t, &mut a, t0);
    let calm = frame(&mut t, &mut a, ms(t0, 1000));
    a.changed = vec![cf("src/a.rs", 40, 1)];
    let start = frame(&mut t, &mut a, ms(t0, 1100));
    let mid = frame(&mut t, &mut a, ms(t0, 1200));
    assert!(a.tick_fx());
    let end = frame(&mut t, &mut a, ms(t0, 1100 + CHANGE_MS + 20));
    assert_ne!(mid, end);
    assert_ne!(calm, end);
    assert_ne!(text(&start), text(&end), "the figure rolls");
    assert!(!a.tick_fx());
    let mut s = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut b = app(false);
    b.changed = vec![cf("src/a.rs", 40, 1)];
    b.show_files = true;
    same(&end, &frame(&mut s, &mut b, t0));
}

#[test]
fn a_new_file_fades_in() {
    let t0 = Instant::now();
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut a = app(true);
    a.changed = vec![cf("src/a.rs", 3, 1)];
    a.show_files = true;
    frame(&mut t, &mut a, t0);
    frame(&mut t, &mut a, ms(t0, 1000));
    a.changed.insert(0, cf("src/new.rs", 9, 0));
    let mid = frame(&mut t, &mut a, ms(t0, 1050));
    let end = frame(&mut t, &mut a, ms(t0, 1050 + NEW_MS + 20));
    assert_ne!(mid, end);
    assert_eq!(text(&mid), text(&end));
    assert!(!a.tick_fx());
}

const TWO_HUNKS: &str = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,3 @@ fn one
 a
-b
+c
@@ -30,3 +30,3 @@ fn two
 x
-y
+z
";

#[test]
fn the_viewer_lights_the_hunk_it_jumps_to() {
    let t0 = Instant::now();
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut a = app(true);
    let files = parse_unified(TWO_HUNKS, Area::Unstaged);
    a.diffview =
        Some(DiffView::from_collected(Collected { files, note: None, error: None, root: PathBuf::from("/repo") }));
    frame(&mut t, &mut a, t0);
    a.diffview_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(a.tick_fx(), "a jump asked for is about to move");
    let mid = frame(&mut t, &mut a, ms(t0, 100));
    let mid2 = frame(&mut t, &mut a, ms(t0, 200));
    let end = frame(&mut t, &mut a, ms(t0, 100 + JUMP_MS + 20));
    assert_ne!(mid, end);
    assert_ne!(mid2, end);
    assert_eq!(text(&mid), text(&end));
    assert!(!a.tick_fx());
    let mut s = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut b = app(false);
    b.diffview = Some(DiffView::from_collected(Collected {
        files: parse_unified(TWO_HUNKS, Area::Unstaged),
        note: None,
        error: None,
        root: PathBuf::from("/repo"),
    }));
    frame(&mut s, &mut b, t0);
    b.diffview_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    same(&end, &frame(&mut s, &mut b, t0));
}

#[test]
fn the_file_list_highlight_glides() {
    let t0 = Instant::now();
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut a = app(true);
    let mut files = parse_unified(TWO_HUNKS, Area::Unstaged);
    files.extend(parse_unified(&TWO_HUNKS.replace("main.rs", "lib.rs"), Area::Unstaged));
    a.diffview =
        Some(DiffView::from_collected(Collected { files, note: None, error: None, root: PathBuf::from("/r") }));
    frame(&mut t, &mut a, t0);
    a.diffview_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    frame(&mut t, &mut a, ms(t0, 500));
    let mid = frame(&mut t, &mut a, ms(t0, 540));
    let end = frame(&mut t, &mut a, ms(t0, 800));
    assert_ne!(mid, end);
    assert!(!a.tick_fx());
}

#[test]
fn small_terminals_do_not_panic() {
    let t0 = Instant::now();
    for (w, h) in [(100, 10), (100, 6), (60, 20), (20, 5)] {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut a = app(true);
        a.changed = vec![cf("src/a.rs", 3, 1)];
        a.show_files = true;
        frame(&mut t, &mut a, t0);
        a.changed.push(cf("b.rs", 1, 1));
        frame(&mut t, &mut a, ms(t0, 20));
    }
}

/// Equal in the panel columns (the banner at the left has its own intro).
fn same(a: &Buffer, b: &Buffer) {
    let diff: Vec<String> = a
        .content()
        .iter()
        .zip(b.content())
        .enumerate()
        .filter(|(i, (x, y))| i % 120 >= 90 && x != y)
        .take(6)
        .map(|(i, (x, y))| {
            format!(
                "cell {} ({},{}): {:?}/{:?}/{:?} vs {:?}/{:?}/{:?}",
                i,
                i % 120,
                i / 120,
                x.symbol(),
                x.fg,
                x.bg,
                y.symbol(),
                y.fg,
                y.bg
            )
        })
        .collect();
    assert!(diff.is_empty(), "{diff:#?}");
}
