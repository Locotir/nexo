//! The /diff viewer drawn from a fixed in-memory diff set (no git) at three sizes; a deliberate
//! change is accepted with `NEXO_UPDATE_GOLDEN=1 cargo test`.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_tui::diffview::collect::{all_added, parse_unified, Area, Collected, FileDiff, Status};
use nexo_tui::diffview::{render, DiffView, Out};
use nexo_tui::theme::Theme;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

const SAMPLE: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 111..222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,7 +1,8 @@ fn main
 use std::io;

 fn main() {
-    let x = 1;
+    let x = 2;
+    let y = x * 2;
     println!(\"{x}\");
 }

@@ -30,3 +31,3 @@ fn helper
 fn helper() -> u32 {
-    41
+    42
 }
diff --git a/README.md b/README.md
index 333..444 100644
--- a/README.md
+++ b/README.md
@@ -1,2 +1,3 @@
 # Title
+A new line.
 Text.
diff --git a/old.txt b/new.txt
similarity index 80%
rename from old.txt
rename to new.txt
--- a/old.txt
+++ b/new.txt
@@ -1 +1 @@
-before
+after
diff --git a/logo.png b/logo.png
Binary files a/logo.png and b/logo.png differ
";

fn view() -> DiffView {
    let mut files = parse_unified(SAMPLE, Area::Unstaged);
    files[1].area = Area::Staged;
    files.push(all_added("notes/todo.txt", "one\ntwo\nthree\n", Area::Untracked, Status::Untracked));
    let mut big = FileDiff::new("data/huge.csv", Area::Untracked);
    big.status = Status::Untracked;
    big.note = Some("too large to show (4200 KB)".into());
    files.push(big);
    DiffView::from_collected(Collected { files, note: None, error: None, root: PathBuf::from("/repo") })
}

fn capture(dv: &mut DiffView, w: u16, h: u16) -> String {
    let theme = Theme::dark();
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| render::draw(f, dv, f.area(), &theme)).unwrap();
    let buf = t.backend().buffer().clone();
    let mut rows = Vec::new();
    for y in 0..h {
        let mut s = String::new();
        for x in 0..w {
            s.push_str(buf[(x, y)].symbol());
        }
        rows.push(s.trim_end().to_string());
    }
    rows.join("\n") + "\n"
}

fn press(dv: &mut DiffView, c: KeyCode) -> Out {
    dv.key(KeyEvent::new(c, KeyModifiers::NONE))
}

fn check(name: &str, w: u16, h: u16, text: &str) {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/captures").join(format!("diffview_{name}_{w}x{h}.txt"));
    if std::env::var("NEXO_UPDATE_GOLDEN").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let kept = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("no capture {}: run with NEXO_UPDATE_GOLDEN=1", path.display()));
    assert_eq!(kept, text, "{name} {w}x{h} differs from {}", path.display());
}

#[test]
fn unified_at_three_sizes() {
    for (w, h) in [(80, 24), (120, 40), (200, 60)] {
        let mut dv = view();
        let t = capture(&mut dv, w, h);
        assert!(t.contains("5 files") || t.contains("6 files"), "{t}");
        check("unified", w, h, &t);
    }
}

#[test]
fn side_by_side_at_200() {
    let mut dv = view();
    capture(&mut dv, 200, 60);
    press(&mut dv, KeyCode::Char('s'));
    let t = capture(&mut dv, 200, 60);
    assert!(t.lines().any(|l| l.matches('│').count() == 2), "{t}");
    check("side", 200, 60, &t);
    // narrower than 100 columns the wish waits
    let mut dv = view();
    press(&mut dv, KeyCode::Char('s'));
    let t = capture(&mut dv, 80, 24);
    assert!(t.lines().all(|l| l.matches('│').count() <= 1));
}

#[test]
fn navigation_folding_and_second_file() {
    let mut dv = view();
    capture(&mut dv, 120, 40);
    press(&mut dv, KeyCode::Enter);
    press(&mut dv, KeyCode::Char('f'));
    let t = capture(&mut dv, 120, 40);
    assert!(t.contains("▸ @@ -1,7 +1,8 @@") && t.contains("▸ @@ -30,3 +31,3 @@"), "{t}");
    check("folded", 120, 40, &t);
    press(&mut dv, KeyCode::Char('f'));
    press(&mut dv, KeyCode::Char('n'));
    let t = capture(&mut dv, 120, 40);
    assert!(t.contains("hunk 2/2"), "{t}");
    press(&mut dv, KeyCode::Enter);
    press(&mut dv, KeyCode::Down);
    let t = capture(&mut dv, 120, 40);
    assert!(t.contains("A new line."), "{t}");
    match press(&mut dv, KeyCode::Char('y')) {
        Out::Copy(p) => assert!(p.contains("+A new line.")),
        o => panic!("{o:?}"),
    }
    assert_eq!(press(&mut dv, KeyCode::Char('q')), Out::Close);
}

#[test]
fn empty_and_error_states() {
    let mut dv = DiffView::from_collected(Collected::default());
    assert!(capture(&mut dv, 80, 24).contains("No changes"));
    let mut dv = DiffView::from_collected(Collected { error: Some("fatal: bad object".into()), ..Default::default() });
    assert!(capture(&mut dv, 80, 24).contains("fatal: bad object"));
    let mut dv = DiffView::from_collected(Collected {
        note: Some("not a git repo: showing this session's edits".into()),
        ..Default::default()
    });
    assert!(capture(&mut dv, 80, 24).contains("not a git repo"));
}

#[test]
fn small_sizes_keep_the_esc_hint() {
    // the viewer never hides how to leave, whatever the size
    for (w, h) in [(30, 9), (40, 12), (24, 6), (1, 1)] {
        let mut dv = view();
        let t = capture(&mut dv, w, h);
        if w >= 24 && h >= 6 {
            assert!(t.contains("Esc close"), "{w}x{h}: {t}");
        }
        assert!(!t.contains("q close"), "{w}x{h}: {t}");
    }
}

#[test]
fn tab_and_brackets_jump_between_files() {
    let mut dv = view();
    capture(&mut dv, 120, 40);
    assert_eq!(dv.sel, 0);
    press(&mut dv, KeyCode::Tab);
    assert_eq!(dv.sel, 1);
    press(&mut dv, KeyCode::Char(']'));
    assert_eq!(dv.sel, 2);
    press(&mut dv, KeyCode::Char('['));
    press(&mut dv, KeyCode::BackTab);
    assert_eq!(dv.sel, 0);
    press(&mut dv, KeyCode::BackTab);
    assert_eq!(dv.sel, 0, "no wrap before the first file");
    let last = dv.files.len() - 1;
    for _ in 0..20 {
        press(&mut dv, KeyCode::Tab);
    }
    assert_eq!(dv.sel, last);
    // Enter moves between the list and the body; Tab keeps jumping files
    press(&mut dv, KeyCode::Enter);
    press(&mut dv, KeyCode::Char('['));
    assert_eq!(dv.sel, last - 1);
    assert_eq!(press(&mut dv, KeyCode::Esc), Out::Close);
}

#[test]
fn scrolling_the_body_and_a_missing_file_list() {
    let mut dv = view();
    capture(&mut dv, 80, 10);
    press(&mut dv, KeyCode::Enter);
    press(&mut dv, KeyCode::PageDown);
    assert!(dv.scroll > 0 || dv.current().unwrap().line_count() < 8);
    press(&mut dv, KeyCode::Home);
    assert_eq!(dv.scroll, 0);
    let mut none = DiffView::from_collected(Collected::default());
    for c in [KeyCode::Tab, KeyCode::BackTab, KeyCode::Char(']'), KeyCode::PageDown, KeyCode::Char('n')] {
        assert_eq!(press(&mut none, c), Out::None);
    }
}

#[test]
fn a_real_repo_and_a_plain_directory() {
    use nexo_tui::diffview::collect::collect;
    let git = |d: &std::path::Path, a: &[&str]| {
        let ok = std::process::Command::new("git").args(a).current_dir(d).output().unwrap().status.success();
        assert!(ok, "git {a:?}");
    };
    let base = std::env::temp_dir().join(format!("nexo-diffview-{}", std::process::id()));
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    let c = collect(&repo, Vec::new());
    assert!(c.files.is_empty() && c.error.is_none(), "empty repo: {c:?}");
    let mut dv = DiffView::from_collected(c);
    assert!(capture(&mut dv, 80, 24).contains("No changes"));
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "x"]);
    std::fs::write(repo.join("a.txt"), "two\n").unwrap();
    std::fs::write(repo.join("b.txt"), "new\n").unwrap();
    let c = collect(&repo, Vec::new());
    let areas: Vec<Area> = c.files.iter().map(|f| f.area).collect();
    assert_eq!(areas, vec![Area::Unstaged, Area::Untracked]);
    let plain = base.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let mut dv = DiffView::from_collected(collect(&plain, Vec::new()));
    let t = capture(&mut dv, 80, 24);
    assert!(t.contains("not a git repo"), "{t}");
    let _ = std::fs::remove_dir_all(&base);
}
