//! `[ui] status_command`: the person's script draws the status bar. nexo sends it a JSON of the
//! session on stdin and shows its first line, ANSI colours included, cut with `…` at the width.
//! It never holds the interface: a slow script is cut at 500 ms, a failing one leaves the usual
//! pieces (or the last good line). The scripts are in `tests/status_command/`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nexo_core::agent::{AgentEvent, TurnEnd};
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use tokio::sync::mpsc;

fn script(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/status_command").join(name);
    format!("sh '{}'", p.display())
}

fn app(command: &str) -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        animations: false,
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::Galaxy,
        home_stars: true,
        bursts: true,
        todo_strip: false,
        status_command: command.to_string(),
        ..Default::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a.handle_agent(AgentEvent::Ready {
        session_id: "20261102-140000-ab12".into(),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        cwd: PathBuf::from("/tmp/project"),
        permission_mode: nexo_core::permission::PermissionMode::Ask,
    });
    a.handle_agent(AgentEvent::ModelLimits {
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        pricing: None,
        context_window: Some(1000),
        max_output: None,
        source: "config".into(),
    });
    a.last_call_input = 420;
    a.cost = 0.0123;
    a.cost_shown = 0.0123;
    a
}

/// Ticks as the interface does until `done` or the time is up; the longest tick.
fn tick_until(a: &mut App, limit: Duration, done: impl Fn(&App) -> bool) -> Duration {
    let start = Instant::now();
    let mut longest = Duration::ZERO;
    while !done(a) && start.elapsed() < limit {
        let t = Instant::now();
        a.tick();
        longest = longest.max(t.elapsed());
        std::thread::sleep(Duration::from_millis(20));
    }
    longest
}

fn line(a: &App) -> Option<String> {
    a.status_cmd.as_ref().and_then(|s| s.line.clone())
}

fn screen(a: &mut App, w: u16, h: u16) -> (Vec<String>, Terminal<TestBackend>) {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    let rows = rows_of(t.backend().buffer());
    assert_eq!(rows.len(), h as usize);
    (rows, t)
}

#[test]
fn the_script_gets_the_session_and_its_first_line_is_the_bar() {
    let mut a = app(&script("echo.sh"));
    tick_until(&mut a, Duration::from_secs(30), |a| line(a).is_some());
    let want = "deepseek-chat on deepseek | ctx 42% | $0.0123 | 20261102-140000-ab12 | turns 0";
    let got = line(&a).unwrap_or_else(|| panic!("no line within 30 s: {:?}", a.status_cmd.as_ref().unwrap().error));
    assert_eq!(nexo_tui::status_cmd::visible(&got), want);
    let update = std::env::var("NEXO_UPDATE_GOLDEN").is_ok();
    for (w, h) in [(80u16, 24u16), (40, 12)] {
        let (rows, t) = screen(&mut a, w, h);
        let bar = rows.last().unwrap();
        assert!(bar.chars().count() <= w as usize, "{bar:?}");
        assert!(!rows.iter().any(|r| r.contains("second line")), "only the first line: {rows:?}");
        if w == 80 {
            assert!(bar.contains(want), "{bar:?}");
            // the colours of the script reach the screen
            let buf = t.backend().buffer();
            let y = h - 1;
            let x = bar.find("deepseek-chat").unwrap() as u16;
            assert_eq!(buf[(x, y)].fg, Color::Indexed(5));
            assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
            let x = bar.find("42%").unwrap() as u16;
            assert_eq!(buf[(x, y)].fg, Color::Indexed(2));
        } else {
            // cut at the width, the cut marked
            assert!(bar.trim_end().ends_with('…'), "{bar:?}");
            assert!(bar.contains("deepseek-chat on"), "{bar:?}");
        }
        let text = rows.join("\n") + "\n";
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/captures/status-command_{w}x{h}.txt"));
        if update {
            std::fs::write(&path, &text).unwrap();
        } else {
            let kept = std::fs::read_to_string(&path).expect("no capture: run with NEXO_UPDATE_GOLDEN=1");
            assert_eq!(kept, text, "differs from {}", path.display());
        }
    }
}

#[test]
fn the_script_runs_again_when_a_turn_ends_or_the_model_changes() {
    let mut a = app(&script("echo.sh"));
    tick_until(&mut a, Duration::from_secs(30), |a| line(a).is_some());
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    tick_until(&mut a, Duration::from_secs(30), |a| line(a).is_some_and(|l| l.contains("turns 1")));
    assert!(line(&a).unwrap().contains("turns 1"), "{:?}", line(&a));
    a.model = "deepseek-reasoner".into();
    tick_until(&mut a, Duration::from_secs(30), |a| line(a).is_some_and(|l| l.contains("reasoner")));
    assert!(line(&a).unwrap().contains("deepseek-reasoner"), "{:?}", line(&a));
}

#[test]
fn a_slow_script_never_freezes_the_interface() {
    let mut a = app(&script("slow.sh"));
    let started = Instant::now();
    let longest = tick_until(&mut a, Duration::from_millis(1500), |_| false);
    assert!(longest < Duration::from_millis(100), "a tick took {longest:?}");
    // cut at 500 ms: nothing is left running and the bar keeps its pieces
    assert!(!a.status_cmd.as_ref().unwrap().is_running(), "still running after {:?}", started.elapsed());
    assert!(line(&a).is_none());
    let (rows, _) = screen(&mut a, 80, 24);
    assert!(rows.last().unwrap().trim_start().starts_with("nexo"), "{:?}", rows.last());
}

#[test]
fn a_failing_script_leaves_the_usual_pieces() {
    let mut a = app(&script("fail.sh"));
    tick_until(&mut a, Duration::from_secs(2), |a| !a.status_cmd.as_ref().unwrap().is_running());
    tick_until(&mut a, Duration::from_millis(300), |_| false);
    assert!(line(&a).is_none());
    for (w, h) in [(80, 24), (40, 12)] {
        let (rows, _) = screen(&mut a, w, h);
        let bar = rows.last().unwrap();
        assert!(bar.trim_start().starts_with("nexo") && bar.contains("deepseek"), "{w}x{h}: {bar:?}");
        assert!(!rows.iter().any(|r| r.contains("half a line") || r.contains("exited")), "no error on screen");
    }
}

#[test]
fn a_script_that_fails_later_keeps_its_last_good_line() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("status-once-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mark = dir.join("ran");
    let _ = std::fs::remove_file(&mark);
    let mut a = app(&format!("{} '{}'", script("once.sh"), mark.display()));
    tick_until(&mut a, Duration::from_secs(30), |a| line(a).is_some());
    assert_eq!(line(&a).as_deref(), Some("first and last good line"));
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    // past the one-second gap the second run happens and fails
    tick_until(&mut a, Duration::from_millis(2000), |_| false);
    assert!(!a.status_cmd.as_ref().unwrap().is_running());
    assert_eq!(line(&a).as_deref(), Some("first and last good line"));
    let (rows, _) = screen(&mut a, 80, 24);
    assert!(rows.last().unwrap().contains("first and last good line"), "{:?}", rows.last());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_scripts_colours_stay_only_on_24_bit_and_italics_never_show() {
    for degraded in [false, true] {
        let mut a = app(&script("echo.sh"));
        if degraded {
            // a 256- or 16-colour terminal: the theme is mapped and the bar keeps its own colours
            a.theme = a.theme.clone().downsampled();
            assert!(a.theme.degraded);
        }
        tick_until(&mut a, Duration::from_secs(30), |a| line(a).is_some());
        assert!(line(&a).is_some(), "no line within 30 s");
        let (rows, t) = screen(&mut a, 80, 24);
        let bar = rows.last().unwrap();
        let buf = t.backend().buffer();
        let x = bar.find("deepseek-chat").unwrap() as u16;
        let cell = &buf[(x, 23)];
        assert!(cell.modifier.contains(Modifier::BOLD), "the script's bold stays");
        if degraded {
            assert_eq!((cell.fg, cell.bg), (a.theme.bar_fg, a.theme.bar_bg));
        } else {
            assert_eq!(cell.fg, Color::Indexed(5));
        }
    }
    let mut a = app("printf '\\033[3mslanted\\033[0m plain\\n'");
    tick_until(&mut a, Duration::from_secs(30), |a| line(a).is_some());
    assert!(line(&a).is_some_and(|l| l.contains("slanted")), "{:?}", line(&a));
    let (_, t) = screen(&mut a, 80, 24);
    assert!(t.backend().buffer().content().iter().all(|c| !c.modifier.contains(Modifier::ITALIC)));
}
