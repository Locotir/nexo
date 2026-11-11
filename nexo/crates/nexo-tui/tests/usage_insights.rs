//! "What's contributing to your usage?" in the Usage window: the section under the figures, its
//! three states (reading, empty, ready), the d/w/m period keys and the reading started by the tab.
//! The captures are in `tests/captures/` (`NEXO_UPDATE_GOLDEN=1 cargo test` accepts a deliberate
//! change). Nothing here reads the person's own sessions: the figures are made up, or the
//! sessions are written in a folder of the test's own.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_agent::session_scan::{Insight, InsightKind, Insights, Period, SessionRow, Tally};
use nexo_tui::insights::{Ranked, Scanner};
use nexo_tui::modal::Modal;
use nexo_tui::overlay::Info;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app(scanner: Scanner) -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        animations: false,
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::Galaxy,
        ..Default::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a.insights = scanner;
    a
}

fn sample() -> Ranked {
    let rows = vec![
        Insight { share: 0.62, kind: InsightKind::LongContext { sessions: 4, calls: 31 } },
        Insight {
            share: 0.41,
            kind: InsightKind::ToolOutput { tool: "bash".into(), top: Some("cargo test".into()), runs: 18 },
        },
        Insight { share: 0.24, kind: InsightKind::CacheMisses { misses: 23, tokens: 310_000, rebuilds: 2 } },
        Insight { share: 0.80, kind: InsightKind::Model { model: "model-x".into(), cost: 4.10, dearer: true } },
        Insight { share: 0.22, kind: InsightKind::CuratorLeftOut { tokens: 1_200_000 } },
    ];
    let top = vec![
        SessionRow { id: "a".into(), project: "login".into(), when: "today".into(), cost: 1.82, cache_hit: Some(71) },
        SessionRow {
            id: "b".into(),
            project: "exporter".into(),
            when: "yesterday".into(),
            cost: 0.94,
            cache_hit: Some(88),
        },
        SessionRow {
            id: "c".into(),
            project: "a-folder-name-far-too-long-for-its-column".into(),
            when: "Oct 23".into(),
            cost: 0.1,
            cache_hit: None,
        },
    ];
    let totals = Tally { calls: 120, cost: 5.12, ..Tally::default() };
    let week = Insights { rows, top, totals, sessions: 9 };
    // today: nothing was used; 30 days: used, and nothing stands out
    let quiet = Insights { sessions: 2, totals: Tally { calls: 4, ..Tally::default() }, ..Insights::default() };
    Ranked { by_period: [Insights::default(), week, quiet], partial: None }
}

fn open(a: &mut App) {
    a.modal = Some(Modal::Info(Box::new(Info::new(1).still(true))));
}

fn scroll_to_end(a: &mut App) {
    if let Some(Modal::Info(i)) = &mut a.modal {
        i.scroll = usize::MAX / 2;
    }
}

/// Scrolls so the section's heading is the first row of the body (or as near as the content allows).
fn scroll_to_section(a: &mut App, w: u16, h: u16) {
    let mut best = 0;
    for s in 0..300 {
        if let Some(Modal::Info(i)) = &mut a.modal {
            i.scroll = s;
        }
        let rows = draw(a, w, h);
        if rows.iter().any(|r| r.contains("What's contributing to your usage")) {
            best = s;
        } else if best > 0 {
            break;
        }
    }
    if let Some(Modal::Info(i)) = &mut a.modal {
        i.scroll = best;
    }
}

fn press(a: &mut App, c: char) {
    nexo_tui::event::key(a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}

fn draw(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    // twice: the window is sized on the first frame it has its figures for
    draw_frame(&mut t, a).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer())
}

fn check(name: &str, w: u16, h: u16, rows: &[String]) {
    let text = rows.join("\n") + "\n";
    for r in rows {
        assert!(r.chars().count() <= w as usize, "{name} {w}x{h}: {r:?}");
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/captures")
        .join(format!("usage-insights-{name}_{w}x{h}.txt"));
    if std::env::var("NEXO_UPDATE_GOLDEN").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let kept = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("no capture {}: run with NEXO_UPDATE_GOLDEN=1", path.display()));
    assert_eq!(kept, text, "{name} {w}x{h} differs from {}", path.display());
}

fn period_of(a: &App) -> Period {
    match &a.modal {
        Some(Modal::Info(i)) => i.period,
        _ => panic!("no window"),
    }
}

#[test]
fn the_week_at_two_sizes() {
    for (w, h) in [(80u16, 24u16), (120, 40)] {
        let mut a = app(Scanner::with(sample()));
        open(&mut a);
        scroll_to_section(&mut a, w, h);
        let rows = draw(&mut a, w, h);
        let dump = rows.join("\n");
        assert!(dump.contains("What's contributing to your usage"), "{dump}");
        assert!(dump.contains("Long context"), "{dump}");
        assert!(dump.contains("Esc close"), "{dump}");
        check("week", w, h, &rows);
        scroll_to_end(&mut a);
        let rows = draw(&mut a, w, h);
        assert!(rows.join("\n").contains("Shares overlap"), "{}", rows.join("\n"));
        check("week-end", w, h, &rows);
    }
}

#[test]
fn while_it_reads_the_section_says_so_and_shows_no_figures() {
    for (w, h) in [(80u16, 24u16), (120, 40)] {
        let mut a = app(Scanner::default());
        open(&mut a);
        scroll_to_section(&mut a, w, h);
        let rows = draw(&mut a, w, h);
        let dump = rows.join("\n");
        assert!(dump.contains("Scanning local sessions"), "{dump}");
        assert!(!dump.contains("Long context"), "{dump}");
        check("scanning", w, h, &rows);
    }
}

#[test]
fn an_empty_period_and_a_quiet_one_are_said_apart() {
    for (key, name) in [('d', "today"), ('m', "month")] {
        for (w, h) in [(80u16, 24u16), (120, 40)] {
            let mut a = app(Scanner::with(sample()));
            open(&mut a);
            press(&mut a, key);
            scroll_to_section(&mut a, w, h);
            let rows = draw(&mut a, w, h);
            check(name, w, h, &rows);
        }
    }
}

#[test]
fn d_w_m_change_the_period_on_the_usage_tab_only() {
    let mut a = app(Scanner::with(sample()));
    open(&mut a);
    assert_eq!(period_of(&a), Period::Week);
    press(&mut a, 'd');
    assert_eq!(period_of(&a), Period::Day);
    press(&mut a, 'm');
    assert_eq!(period_of(&a), Period::Month);
    press(&mut a, 'w');
    assert_eq!(period_of(&a), Period::Week);
    // on another tab the keys do nothing
    a.modal = Some(Modal::Info(Box::new(Info::new(0).still(true))));
    press(&mut a, 'd');
    assert_eq!(period_of(&a), Period::Week);
}

#[test]
fn the_window_is_sized_again_when_the_period_changes() {
    let mut a = app(Scanner::with(sample()));
    open(&mut a);
    let week = draw(&mut a, 120, 40);
    press(&mut a, 'd');
    let day = draw(&mut a, 120, 40);
    assert_ne!(week, day);
    let tall = |rows: &[String]| rows.iter().filter(|r| r.contains('│')).count();
    assert!(tall(&day) <= tall(&week), "{}", day.join("\n"));
}

fn write_session(dir: &std::path::Path) {
    use nexo_agent::session::{Session, UsageKind, UsageMeta};
    use nexo_core::message::Message;
    use nexo_core::stream::Usage;
    let mut s = Session::create_in(dir, &PathBuf::from("/tmp/project"), "mock", "m1").unwrap();
    s.append(&Message::user("hello there")).unwrap();
    for input in [10_000u64, 180_000, 190_000] {
        let meta = UsageMeta { model: Some("m1".into()), kind: Some(UsageKind::Main), held: None };
        s.record_usage_meta(Usage { input, output: 100, ..Default::default() }, 0.5, meta).unwrap();
    }
}

#[test]
fn the_usage_tab_starts_the_reading_and_the_tick_takes_it() {
    let dir = tempfile::tempdir().unwrap();
    write_session(dir.path());
    let mut a = app(Scanner::in_dir(Some(dir.path().to_path_buf())));
    // another tab: nothing is read
    a.modal = Some(Modal::Info(Box::new(Info::new(0).still(true))));
    a.tick();
    assert!(!a.insights.running());
    assert_eq!(a.insights.version, 0);
    // the usage tab: the reading starts, and the figures come in by the tick
    open(&mut a);
    a.tick();
    let start = Instant::now();
    while a.insights.version == 0 && start.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(5));
        a.tick();
    }
    assert_eq!(a.insights.version, 1);
    scroll_to_end(&mut a);
    let rows = draw(&mut a, 120, 40);
    let dump = rows.join("\n");
    assert!(dump.to_lowercase().contains("long context"), "{dump}");
}

#[test]
fn the_window_can_always_be_closed_by_its_footer_and_never_overflows() {
    for (w, h) in [(60u16, 24u16), (80, 24), (120, 40)] {
        for key in ['d', 'w', 'm'] {
            let mut a = app(Scanner::with(sample()));
            open(&mut a);
            press(&mut a, key);
            for end in [false, true] {
                if end {
                    scroll_to_end(&mut a);
                }
                let rows = draw(&mut a, w, h);
                for r in &rows {
                    assert!(r.chars().count() <= w as usize, "{w}x{h}: {r:?}");
                }
                assert!(rows.iter().any(|r| r.contains("Esc close")), "{w}x{h} {key}: {}", rows.join("\n"));
            }
        }
    }
}

#[test]
fn the_biggest_sessions_show_folders_and_days_never_what_was_typed() {
    let mut a = app(Scanner::with(sample()));
    open(&mut a);
    scroll_to_end(&mut a);
    let dump = draw(&mut a, 120, 40).join("\n");
    assert!(dump.contains("login · today"), "{dump}");
    assert!(!dump.contains("fix login flow"), "{dump}");
}
