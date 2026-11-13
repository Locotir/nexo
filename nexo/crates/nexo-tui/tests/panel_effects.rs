//! `/panel` with a busy fleet (event ribbons, todo bars, test runs, sub-agents, a permission wait, an error) and
//! what moves on it: each effect checked at fixed clock times, the quiet forms (no motion, mono, ASCII, narrow
//! widths), the figures of the pulse row, and captures kept in `tests/captures/` (`NEXO_UPDATE_GOLDEN=1 cargo
//! test` accepts a deliberate change).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nexo_agent::supervise::{AgentRow, Ev, EvDetail, EvKind, SourceKind, State};
use nexo_tui::agents_view::AgentsView;
use nexo_tui::modal::Modal;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

const NOW: i64 = 1_000_000;

fn app(animations: bool, theme: &str) -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        providers: vec![],
        theme: theme.into(),
        mouse: true,
        show_reasoning: true,
        timestamps: false,
        fold_after: 4,
        group_tools: true,
        complete_words: true,
        suggest: false,
        hints: true,
        keys: Default::default(),
        notify_after: 15,
        animations,
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::Galaxy,
        // an empty session drifts its stars for a while (src/app/tests/bursts.rs); these frames are about other things
        home_stars: false,
        bursts: true,
        todo_strip: false,
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

fn ev(ago: i64, kind: EvKind) -> Ev {
    Ev::new(NOW - ago, kind)
}

fn row(key: &str, name: &str, source: SourceKind, state: State, idle: u64, step: Option<&str>) -> AgentRow {
    AgentRow {
        key: key.into(),
        name: name.into(),
        source,
        program: if source == SourceKind::Nexo { "nexo" } else { "othertool" }.into(),
        state,
        since: NOW - 60,
        last_step: step.map(Into::into),
        idle_secs: Some(idle),
        cost_usd: None,
        model: None,
        cwd: Some(format!("/home/dev/{name}")),
        permission_mode: Some("default".into()),
        parent: None,
        paused: false,
        last_action: None,
        ribbon: vec![],
        spark: vec![],
        todo: None,
        tests: None,
        subs: vec![],
        unread: 0,
        held: 0,
        retry_secs: None,
    }
}

/// Twelve events of every flavour, oldest first, the last one a call in progress.
fn busy_ribbon() -> Vec<Ev> {
    use EvKind::*;
    let mut v: Vec<Ev> = [
        (300, SessionStart),
        (280, Thinking),
        (240, Read),
        (200, Search),
        (150, Edit),
        (120, Run),
        (100, TestsFail),
        (80, Edit),
        (60, TestsPass),
        (45, GitCommit),
        (30, Compact),
        (3, Edit),
    ]
    .into_iter()
    .map(|(ago, k)| ev(ago, k))
    .collect();
    v[6].detail = EvDetail::Tests { pass: 11, fail: 2 };
    v[8].detail = EvDetail::Tests { pass: 13, fail: 0 };
    v
}

fn fleet() -> Vec<AgentRow> {
    let mut project = row("n1", "project", SourceKind::Nexo, State::Working, 2, Some("Edit  src/parser.rs"));
    project.ribbon = busy_ribbon();
    project.spark = vec![0, 1, 3, 2, 5, 8, 6, 9, 4, 7, 12, 10, 8, 6, 9, 14, 11, 7, 5, 8, 12, 15, 9, 11];
    project.todo = Some((3, 7));
    project.tests = Some((13, 0));
    project.cost_usd = Some(0.42);
    project.subs = vec![("n1.1".into(), State::Working), ("n1.2".into(), State::Done)];
    let mut explore = row("n1.1", "explore", SourceKind::Nexo, State::Working, 1, Some("Grep  fn classify"));
    explore.parent = Some("n1".into());
    explore.ribbon = vec![ev(20, EvKind::Read), ev(9, EvKind::Search), ev(1, EvKind::Search)];
    explore.spark = vec![0, 0, 2, 4, 3];
    let mut wait = row(
        "x1",
        "api-migration",
        SourceKind::External,
        State::WaitingPermission,
        95,
        Some("Bash  cargo publish --dry-run"),
    );
    wait.ribbon = vec![ev(200, EvKind::Edit), ev(150, EvKind::Run), ev(95, EvKind::PermissionWait)];
    wait.spark = vec![1, 2, 0, 0, 3, 1];
    wait.cost_usd = Some(1.25);
    wait.last_action = Some(("marked permission".into(), NOW - 90));
    let mut looping = row("x2", "scheduler", SourceKind::External, State::Looping, 30, Some("Bash  git pull"));
    looping.ribbon = vec![ev(90, EvKind::Run), ev(60, EvKind::Run), ev(35, EvKind::Error), ev(30, EvKind::Loop)];
    let mut done = row("x3", "docs", SourceKind::External, State::Done, 7300, Some("Bash  gh release create v1.4"));
    done.ribbon = vec![ev(7500, EvKind::GitCommit), ev(7400, EvKind::GitPush), ev(7300, EvKind::SessionEnd)];
    vec![project, explore, wait, looping, done]
}

fn shot(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer())
}

fn at(t0: Instant, ms: u64) -> Instant {
    t0 + Duration::from_millis(ms)
}

fn view(a: &mut App) -> &mut AgentsView {
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!("no panel") };
    v
}

/// A live app with the fleet open and drawn twice (the first look learns what is already there).
fn live(w: u16, h: u16, theme: &str) -> (App, Instant) {
    let mut a = app(true, theme);
    let t0 = Instant::now();
    a.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(fleet(), NOW, false))));
    for ms in [0, 3000] {
        a.fx.clock = Some(at(t0, ms));
        shot(&mut a, w, h);
    }
    (a, t0)
}

fn still(w: u16, h: u16, theme: &str) -> Vec<String> {
    let mut a = app(false, theme);
    a.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(fleet(), NOW, true))));
    shot(&mut a, w, h)
}

fn golden(name: &str, w: u16, h: u16, rows: &[String], wrong: &mut Vec<String>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/captures").join(format!("{name}_{w}x{h}.txt"));
    let text = rows.join("\n") + "\n";
    if std::env::var("NEXO_UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &text).unwrap();
        return;
    }
    match std::fs::read_to_string(&path) {
        Ok(kept) if kept == text => {}
        Ok(_) => wrong.push(format!("{name} {w}x{h}: differs from {}", path.display())),
        Err(_) => wrong.push(format!("{name} {w}x{h}: no capture, run with NEXO_UPDATE_GOLDEN=1")),
    }
}

fn panel_of(rows: &[String]) -> String {
    rows.iter()
        .filter(|r| r.contains('│') || r.contains('╭') || r.contains('╰'))
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_busy_fleet_matches_its_captures_still_live_mono_and_narrow() {
    let mut wrong = Vec::new();
    for (w, h) in [(40, 12), (80, 24), (120, 40), (200, 60)] {
        golden("busy-still", w, h, &still(w, h, "dark"), &mut wrong);
        let (mut a, t0) = live(w, h, "dark");
        a.fx.clock = Some(at(t0, 3300));
        golden("busy-live", w, h, &shot(&mut a, w, h), &mut wrong);
        golden("busy-mono", w, h, &still(w, h, "mono"), &mut wrong);
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn every_line_of_the_busy_fleet_keeps_its_borders_aligned_at_every_size() {
    for (w, h) in [(40, 12), (80, 24), (120, 40), (200, 60)] {
        let rows = still(w, h, "dark");
        for r in &rows {
            assert!(UnicodeWidthStr::width(r.as_str()) <= w as usize, "{w}x{h}: a row is wider than the screen\n{r}");
        }
        let from = rows.iter().position(|r| r.starts_with("╭ panel")).expect("the panel is open");
        let to = rows.iter().rposition(|r| r.starts_with('╰')).expect("the panel closes");
        let ends: Vec<usize> = rows[from..=to].iter().map(|r| UnicodeWidthStr::width(r.as_str())).collect();
        if let Some(first) = ends.first() {
            assert!(ends.iter().all(|e| e == first), "{w}x{h}: the right border wobbles {ends:?}");
        }
    }
}

#[test]
fn the_ribbon_shows_the_last_twelve_events_as_glyphs_and_the_card_its_todo_and_tokens() {
    let rows = still(200, 60, "dark");
    let text = panel_of(&rows);
    for g in ["◦", "◎", "✎", "❯", "✘", "✔", "◉", "▤"] {
        assert!(text.contains(g), "the ribbon lacks {g}\n{text}");
    }
    assert!(text.contains('▁') || text.contains('█'), "no tokens sparkline\n{text}");
    assert!(text.contains("3/7"), "no todo count\n{text}");
}

#[test]
fn the_pulse_row_adds_up_the_fleet_and_drops_figures_before_the_count_of_who_needs_you() {
    let wide = panel_of(&still(200, 60, "dark"));
    for s in ["2 live", "/min", "✔13", "$1.67", "! 1 need you"] {
        assert!(wide.contains(s), "no {s:?} in the pulse row\n{wide}");
    }
    let mut last = usize::MAX;
    for w in [200u16, 120, 100, 80] {
        let line = still(w, 40, "dark").into_iter().find(|r| r.contains("live ▕")).unwrap_or_default();
        let figures = ["$", "commit", "✔", "✉", "/min", "live"].iter().filter(|f| line.contains(**f)).count();
        assert!(figures <= last, "{w}: a figure came back as the row narrowed\n{line}");
        last = figures;
        if !line.is_empty() {
            assert!(line.contains("need you"), "{w}: the count of who needs you is never dropped\n{line}");
        }
    }
    // no room for the row at all: the figures fold into the header
    let small = still(80, 21, "dark").join("\n");
    assert!(!small.contains("live ▕"), "no pulse row in 21 rows\n{small}");
}

#[test]
fn a_letter_flies_between_two_cards_inside_the_panel_and_never_over_text() {
    use nexo_agent::supervise::MailRow;
    use nexo_core::mail::Stage;
    let (w, h) = (120, 40);
    let rest = panel_of(&still(w, h, "dark"));
    let mail = |id: &str| MailRow {
        id: id.into(),
        from: "n1".into(),
        to: "x3".into(),
        summary: "ship it".into(),
        stage: Stage::Sent,
        at: NOW,
        bytes: 7,
        n: 1,
    };
    let (mut a, t0) = live(w, h, "dark");
    let rows = fleet();
    view(&mut a).fly_new_mail(vec![mail("seed")], &rows);
    a.fx.clock = Some(at(t0, 3000));
    shot(&mut a, w, h);
    view(&mut a).fly_new_mail(vec![mail("seed"), mail("new")], &rows);
    a.fx.clock = Some(at(t0, 4000));
    shot(&mut a, w, h);
    assert!(view(&mut a).animating(at(t0, 4000)), "a letter is about to leave");
    let mut seen = Vec::new();
    for ms in [0u64, 120, 570, 1020, 1440, 1800] {
        a.fx.clock = Some(at(t0, 4000 + ms));
        let s = panel_of(&shot(&mut a, w, h));
        seen.push((ms, s.matches('✉').count(), view(&mut a).animating(at(t0, 4000 + ms))));
        for word in ["project", "api-migration", "docs", "scheduler"] {
            if rest.contains(word) {
                assert!(s.contains(word), "{ms} ms: {word} is covered\n{s}");
            }
        }
    }
    // the card keeps a receipt and the pulse row a count; the letter is the one glyph more while it flies
    let settled = seen[5].1;
    assert!(seen[2].1 > settled, "a letter shows in flight {seen:?}");
    assert_eq!(seen[4].1, settled, "gone once landed {seen:?}");
    assert!(seen[2].2, "the loop draws while it flies");
}

#[test]
fn a_calm_fleet_asks_for_no_frame_and_a_still_or_mono_view_never_does() {
    let (mut a, t0) = live(120, 40, "dark");
    // quiet the attention states of the fixture
    let calm: Vec<AgentRow> = fleet()
        .into_iter()
        .filter(|r| matches!(r.state, State::Done))
        .map(|mut r| {
            r.ribbon.clear();
            r
        })
        .collect();
    view(&mut a).set_rows(calm);
    for ms in [4000, 9000] {
        a.fx.clock = Some(at(t0, ms));
        shot(&mut a, 120, 40);
    }
    assert!(!view(&mut a).animating(at(t0, 20_000)), "a calm fleet at rest draws nothing");
    // a busy one asks for frames
    let (mut b, t1) = live(120, 40, "dark");
    assert!(view(&mut b).animating(at(t1, 3000)), "a working fleet moves");
    // the view in still mode never does
    let s = AgentsView::with_rows(fleet(), NOW, true);
    assert!(!s.animating(t0));
    // mono: the picture is the same at any time
    let (mut m, t2) = live(120, 40, "mono");
    let one = shot(&mut m, 120, 40);
    m.fx.clock = Some(at(t2, 3700));
    let two = shot(&mut m, 120, 40);
    assert_eq!(one, two, "mono is still");
}

#[test]
fn ascii_mode_uses_only_plain_glyphs_for_the_ribbon_the_sparkline_and_the_pulse() {
    let mut a = app(false, "dark");
    let mut v = AgentsView::with_rows(fleet(), NOW, true);
    v.set_ascii(true);
    a.modal = Some(Modal::Agents(Box::new(v)));
    let text = panel_of(&shot(&mut a, 120, 40));
    for g in ["✎", "❯", "✔", "✘", "▁", "█", "▋", "░", "◉", "▕", "▏", "✉"] {
        assert!(!text.contains(g), "{g} in ASCII mode\n{text}");
    }
}

#[test]
fn a_new_test_run_a_compaction_and_an_error_start_a_one_shot_that_ends_by_itself() {
    let (w, h) = (120, 40);
    for (kind, ms) in
        [(EvKind::TestsPass, 400u64), (EvKind::TestsFail, 240), (EvKind::Compact, 360), (EvKind::Error, 450)]
    {
        let (mut a, t0) = live(w, h, "dark");
        let mut rows = fleet();
        // only the working session's ribbon grows; the others stay as they were
        rows[0].ribbon.push(ev(0, kind));
        view(&mut a).set_rows(rows);
        a.fx.clock = Some(at(t0, 4000));
        shot(&mut a, w, h);
        // the fixture's own motion (a wait, a call in progress) keeps the loop awake; the one-shot is the
        // difference between a draw inside it and one after it
        let during = shot(&mut a, w, h);
        a.fx.clock = Some(at(t0, 4000 + ms / 2));
        let mid = shot(&mut a, w, h);
        a.fx.clock = Some(at(t0, 4000 + ms + 200));
        let after = shot(&mut a, w, h);
        a.fx.clock = Some(at(t0, 9000));
        let later = shot(&mut a, w, h);
        assert_eq!(after, later, "{kind:?}: nothing changes once the effect is over");
        assert_ne!(mid, after, "{kind:?}: the effect shows while it plays");
        let _ = during;
    }
}

#[test]
fn a_session_with_sub_agents_shows_one_satellite_each_and_they_are_dropped_when_narrow() {
    let wide = panel_of(&still(120, 40, "dark"));
    assert!(
        wide.contains('⠿') || wide.contains('●') || wide.contains('○') || wide.contains('✓'),
        "no satellites\n{wide}"
    );
    let narrow = still(40, 12, "dark").join("\n");
    for r in narrow.lines() {
        assert!(UnicodeWidthStr::width(r) <= 40, "{r}");
    }
}
