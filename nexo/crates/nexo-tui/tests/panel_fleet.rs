//! `/panel` with a fleet of six sessions (two of ours, four from other tools, one with two sub-agents,
//! one waiting on a permission, one in a loop) and a letter in flight at 300 and 700 ms: captures in
//! `tests/captures/` (`NEXO_UPDATE_GOLDEN=1 cargo test` accepts a deliberate change) and the rules a
//! letter must keep.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nexo_agent::supervise::{AgentRow, SourceKind, State};
use nexo_tui::agents_view::AgentsView;
use nexo_tui::modal::Modal;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

const SIZES: [(u16, u16); 5] = [(40, 12), (80, 24), (120, 40), (200, 60), (400, 120)];
const NOW: i64 = 1_000_000;

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

fn row(key: &str, name: &str, source: SourceKind, state: State, idle: u64, step: Option<&str>) -> AgentRow {
    AgentRow {
        key: key.into(),
        name: name.into(),
        source,
        state,
        since: NOW - 60,
        last_step: step.map(Into::into),
        idle_secs: Some(idle),
        cost_usd: None,
        model: None,
        program: if source == SourceKind::Nexo { "nexo" } else { "agentx" }.into(),
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

/// Two of ours (one with two sub-agents) and four from other tools.
fn fleet() -> Vec<AgentRow> {
    let mut v = vec![
        row("n1", "project", SourceKind::Nexo, State::Working, 2, Some("Edit  src/parser.rs")),
        row("n1.1", "explore", SourceKind::Nexo, State::Working, 1, Some("Grep  fn classify")),
        row("n1.2", "tests", SourceKind::Nexo, State::Idle { min: 3 }, 190, Some("Bash  cargo test -p parser")),
        row("n2", "scribe", SourceKind::Nexo, State::Idle { min: 17 }, 1020, None),
        row(
            "x1",
            "api-migration",
            SourceKind::External,
            State::WaitingPermission,
            95,
            Some("Bash  cargo publish --dry-run"),
        ),
        row("x2", "scheduler", SourceKind::External, State::Looping, 30, Some("Bash  git pull")),
        row("x3", "docs", SourceKind::External, State::Done, 7300, Some("Bash  gh release create v1.4")),
        row("x4", "nightly-fixer", SourceKind::External, State::Stalled, 2400, Some("Read  tests/long_running.rs")),
    ];
    for k in [1, 2] {
        v[k].parent = Some("n1".into());
    }
    for k in [6, 7] {
        v[k].program = "othertool".into();
    }
    v[4].last_action = Some(("marked permission".into(), NOW - 90));
    v[4].cost_usd = Some(1.25);
    v
}

fn capture(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer())
}

/// The fleet on a live clock, settled, with a letter sent to `docs` at 2000 ms; captured `ms` after it left.
fn in_flight(w: u16, h: u16, ms: u64) -> Vec<String> {
    let mut a = app(true);
    let t0 = Instant::now();
    a.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(fleet(), NOW, false))));
    for at in [0, 2000] {
        a.fx.clock = Some(t0 + Duration::from_millis(at));
        capture(&mut a, w, h);
    }
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
    v.send_letter("x3");
    a.fx.clock = Some(t0 + Duration::from_millis(2000));
    capture(&mut a, w, h);
    a.fx.clock = Some(t0 + Duration::from_millis(2000 + ms));
    capture(&mut a, w, h)
}

fn still(w: u16, h: u16) -> Vec<String> {
    let mut a = app(false);
    a.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(fleet(), NOW, true))));
    capture(&mut a, w, h)
}

fn file_of(scene: &str, w: u16, h: u16) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/captures").join(format!("{scene}_{w}x{h}.txt"))
}

#[test]
fn the_fleet_matches_its_captures_still_and_with_a_letter_at_300_and_700_ms() {
    let update = std::env::var("NEXO_UPDATE_GOLDEN").is_ok();
    let mut wrong = Vec::new();
    for (w, h) in SIZES {
        for (name, shot) in [
            ("fleet", still(w, h)),
            ("fleet-letter300", in_flight(w, h, 300)),
            ("fleet-letter700", in_flight(w, h, 700)),
        ] {
            let text = shot.join("\n") + "\n";
            let path = file_of(name, w, h);
            if update {
                std::fs::write(&path, &text).unwrap();
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(kept) if kept == text => {}
                Ok(_) => wrong.push(format!("{name} {w}x{h}: differs from {}", path.display())),
                Err(_) => wrong.push(format!("{name} {w}x{h}: no capture, run with NEXO_UPDATE_GOLDEN=1")),
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn a_letter_shows_mid_flight_never_over_text_and_is_gone_with_the_motion_off() {
    for (w, h) in SIZES {
        let mid = in_flight(w, h, 500).join("\n");
        let rest = still(w, h).join("\n");
        // the pulse row keeps one mail glyph of its own; the letter is the one more
        let base = rest.matches('✉').count();
        assert_eq!(mid.matches('✉').count(), base + 1, "{w}x{h}: one letter in the air\n{mid}");
        let landed = in_flight(w, h, 1100).join("\n");
        assert_eq!(landed.matches('✉').count(), base, "{w}x{h}: gone once landed\n{landed}");
        // the letter replaces only rule, border and blank cells: every word of the still picture survives
        for word in ["api-migration", "scheduler", "nightly-fixer", "project"] {
            if rest.contains(word) {
                assert!(mid.contains(word), "{w}x{h}: {word} is covered\n{mid}");
            }
        }
    }
}

#[test]
fn the_view_asks_for_frames_only_while_the_letter_flies() {
    let t0 = Instant::now();
    let mut a = app(true);
    let calm: Vec<AgentRow> =
        fleet().into_iter().filter(|r| !matches!(r.state, State::WaitingPermission | State::Stalled)).collect();
    a.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(calm, NOW, false))));
    for at in [0, 3000] {
        a.fx.clock = Some(t0 + Duration::from_millis(at));
        capture(&mut a, 120, 40);
    }
    let at = |ms| t0 + Duration::from_millis(ms);
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
    assert!(!v.animating(at(3000)), "at rest: 0 fps");
    v.send_letter("x3");
    assert!(v.animating(at(3000)), "a letter is about to leave");
    a.fx.clock = Some(at(3000));
    capture(&mut a, 120, 40);
    let Some(Modal::Agents(v)) = &a.modal else { panic!() };
    assert!(v.animating(at(3500)), "in the air");
    assert!(v.animating(at(4100)), "the card pulses on landing");
    assert!(!v.animating(at(5300)), "settled again");
    let off = {
        let mut v = AgentsView::with_rows(fleet(), NOW, true);
        v.send_letter("x3");
        v
    };
    assert!(!off.animating(t0), "no motion, no frames");
}

#[test]
fn the_footer_lists_the_keys_and_still_fits_at_80_columns() {
    use nexo_agent::supervise::{NexoSource, Panel, Policy, Setup};
    let dir = tempfile::tempdir().unwrap();
    let setup = Setup {
        logs_root: Some(dir.path().join("logs")),
        scan_hours: 12,
        list_cmd: None,
        list_every: 10,
        poll_every_ms: 100,
    };
    let panel = std::sync::Arc::new(Panel::start(&setup, Policy::default(), NexoSource::default()));
    let mut a = app(false);
    a.modal = Some(Modal::Agents(Box::new(AgentsView::new(Some(panel), true))));
    let shot = capture(&mut a, 80, 24).join("\n");
    for key in ["n message", "o policy", "p pause", "+/- zoom", "Esc"] {
        assert!(shot.contains(key), "{key} is missing\n{shot}");
    }
}

#[test]
fn a_real_message_sends_a_letter_to_its_receiver_once_and_old_mail_does_not() {
    use nexo_agent::supervise::MailRow;
    use nexo_core::mail::Stage;
    let mail = |id: &str, to: &str| MailRow {
        id: id.into(),
        from: "x1".into(),
        to: to.into(),
        summary: "look at parser.rs".into(),
        stage: Stage::Sent,
        at: NOW,
        bytes: 17,
        n: 1,
    };
    let mut a = app(true);
    let t0 = Instant::now();
    let rows = fleet();
    a.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(rows.clone(), NOW, false))));
    let at = |ms| t0 + Duration::from_millis(ms);
    let settle = |a: &mut App, ms: u64| {
        a.fx.clock = Some(at(ms));
        capture(a, 120, 40);
    };
    settle(&mut a, 0);
    settle(&mut a, 3000);
    {
        let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
        // a permission wait breathes on its own; the letter is what this test follows
        let base = v.animating(at(3000));
        // the first look only learns what is already there
        v.fly_new_mail(vec![mail("old", "x3")], &rows);
        assert_eq!(v.animating(at(3000)), base, "old mail does not fly");
        // a new one flies to the receiver's card
        v.fly_new_mail(vec![mail("old", "x3"), mail("new", "x3")], &rows);
        assert!(v.animating(at(3000)), "a letter is about to leave");
    }
    settle(&mut a, 3000);
    settle(&mut a, 3500);
    {
        let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
        assert!(v.animating(at(3500)), "in the air");
    }
    settle(&mut a, 9000);
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
    let base = v.animating(at(9000));
    v.fly_new_mail(vec![mail("old", "x3"), mail("new", "x3")], &rows);
    assert_eq!(v.animating(at(9000)), base, "seen mail does not fly again");
    v.fly_new_mail(vec![mail("old", "x3"), mail("new", "x3"), mail("far", "nobody")], &rows);
    assert_eq!(v.animating(at(9000)), base, "no card, no letter");
}
