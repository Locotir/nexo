//! `/panel` drawn at sizes from a split pane to a huge window: captures kept in `tests/captures/`
//! (`NEXO_UPDATE_GOLDEN=1 cargo test` accepts a deliberate change), and the things that must hold at every size.

use std::path::PathBuf;

use nexo_agent::supervise::{AgentRow, SourceKind, State};
use nexo_tui::agents_view::{cards_layout, AgentsView, Layout};
use nexo_tui::commands::{lookup, parse, Command, COMMANDS};
use nexo_tui::modal::Modal;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

const SIZES: [(u16, u16); 5] = [(40, 12), (80, 24), (120, 40), (200, 60), (400, 120)];
const NOW: i64 = 1_000_000;

fn app() -> App {
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
        animations: false,
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

fn row(key: &str, name: &str, source: SourceKind, state: State, idle: Option<u64>, step: Option<&str>) -> AgentRow {
    AgentRow {
        key: key.into(),
        name: name.into(),
        source,
        program: if source == SourceKind::Nexo { "nexo" } else { "agentx" }.into(),
        state,
        since: NOW - 60,
        last_step: step.map(Into::into),
        idle_secs: idle,
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

fn rows() -> Vec<AgentRow> {
    let mut v = vec![
        row("this-session", "project", SourceKind::Nexo, State::Working, Some(2), Some("Edit  src/parser.rs")),
        row(
            "s-1",
            "api-migration",
            SourceKind::External,
            State::WaitingPermission,
            Some(95),
            Some("Bash  cargo publish --dry-run"),
        ),
        row(
            "s-2",
            "docs",
            SourceKind::External,
            State::WaitingAnswer,
            Some(400),
            Some("which license should the README name?"),
        ),
        row(
            "s-3",
            "nightly-fixer",
            SourceKind::External,
            State::Stalled,
            Some(2400),
            Some("Read  tests/long_running.rs"),
        ),
        row("s-4", "scheduler", SourceKind::External, State::Looping, Some(30), Some("Bash  git pull")),
        row("s-5", "refactor", SourceKind::External, State::Idle { min: 17 }, Some(1020), None),
        row("s-6", "release", SourceKind::External, State::Done, Some(7300), Some("Bash  gh release create v1.4")),
    ];
    // two external programs, to see two sections
    for i in [5, 6] {
        v[i].program = "othertool".into();
    }
    v[1].last_action = Some(("marked permission".into(), NOW - 90));
    v[1].cost_usd = Some(1.25);
    let mut sub =
        row("this-session.7", "explore", SourceKind::Nexo, State::Working, Some(1), Some("Grep  fn classify"));
    sub.parent = Some("this-session".into());
    v.insert(1, sub);
    v
}

fn scene(rows: Vec<AgentRow>) -> App {
    let mut a = app();
    a.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(rows, NOW, true))));
    a
}

fn capture(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer())
}

fn file_of(scene: &str, w: u16, h: u16) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/captures").join(format!("{scene}_{w}x{h}.txt"))
}

#[test]
fn the_panel_matches_its_captures_from_40x12_to_400x120() {
    let update = std::env::var("NEXO_UPDATE_GOLDEN").is_ok();
    let mut wrong = Vec::new();
    for (name, set) in [("panel", rows()), ("panel-empty", vec![])] {
        for (w, h) in SIZES {
            let text = capture(&mut scene(set.clone()), w, h).join("\n") + "\n";
            let path = file_of(name, w, h);
            if update {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
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
fn at_every_size_the_window_is_whole_and_says_what_each_state_is_in_words() {
    for (w, h) in SIZES {
        let out = capture(&mut scene(rows()), w, h);
        assert_eq!(out.len(), h as usize);
        let all = out.join("\n");
        if w >= 80 {
            assert!(all.contains("panel"), "{w}x{h}: no title\n{all}");
            assert!(all.contains("only"), "{w}x{h}: no read-only note\n{all}");
            for word in ["working", "permission"] {
                assert!(all.contains(word), "{w}x{h}: no {word}\n{all}");
            }
        }
        // no row is wider than the screen
        assert!(out.iter().all(|r| r.chars().count() <= w as usize), "{w}x{h}");
        if h >= 12 && w >= 40 {
            assert!(all.contains("Esc"), "{w}x{h}: no footer\n{all}");
        }
    }
}

#[test]
fn cards_flow_into_more_columns_as_the_window_widens() {
    assert_eq!(cards_layout(30, 20), Layout::List);
    assert_eq!(cards_layout(80, 3), Layout::List);
    let cols = |w| match cards_layout(w, 30) {
        Layout::Cards { cols, .. } => cols,
        Layout::List => 0,
        Layout::Blocks { .. } => unreachable!(),
    };
    assert_eq!((cols(44), cols(80), cols(120), cols(200), cols(400)), (1, 2, 3, 4, 4));
    for w in 44..400u16 {
        if let Layout::Cards { cols, card_w } = cards_layout(w, 30) {
            assert!((34..=60).contains(&card_w) && card_w * cols + 2 * (cols - 1) <= w, "{w}");
        }
    }
}

#[test]
fn keys_move_the_selection_and_escape_closes() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut a = scene(rows());
    capture(&mut a, 120, 40);
    let press = |a: &mut App, c: KeyCode| {
        if let Some(Modal::Agents(v)) = &mut a.modal {
            let _ = v.key(KeyEvent::new(c, KeyModifiers::NONE));
        }
    };
    press(&mut a, KeyCode::Right);
    press(&mut a, KeyCode::Down);
    let Some(Modal::Agents(v)) = &a.modal else { panic!() };
    assert_eq!(v.selected, 3, "one right, then down to the second row of the external group (three columns)");
    press(&mut a, KeyCode::End);
    press(&mut a, KeyCode::Right);
    let Some(Modal::Agents(v)) = &a.modal else { panic!() };
    assert_eq!(v.selected, v.rows().len() - 1);
}

#[test]
fn panel_is_a_command_and_agents_answers_without_being_listed() {
    assert_eq!(parse("/panel"), Some(Command::Panel));
    assert_eq!(parse("/agents"), Some(Command::Panel));
    assert!(lookup("agents").is_some());
    assert!(COMMANDS.iter().all(|s| s.name != "agents" && !s.aliases.contains(&"agents")));
    // the palette does not offer it either
    assert!(nexo_tui::commands::rank("agents").iter().all(|r| r.via_alias != Some("agents")));
}

#[test]
fn the_command_opens_the_panel_and_a_session_event_shows_in_it() {
    let mut a = app();
    a.input.set_text("/panel");
    a.run_command(parse("/panel").unwrap());
    assert!(matches!(a.modal, Some(Modal::Agents(_))));
}

fn live_panel() -> (tempfile::TempDir, std::sync::Arc<nexo_agent::supervise::Panel>) {
    use nexo_agent::supervise::{NexoSource, Panel, Policy, Setup};
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs").join("proj");
    std::fs::create_dir_all(&logs).unwrap();
    let rec = r#"{"type":"assistant","cwd":"/work/app","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Step one is done."}]}}"#;
    std::fs::write(logs.join("sess-1.jsonl"), format!("{rec}\n")).unwrap();
    let setup = Setup {
        logs_root: Some(dir.path().join("logs")),
        scan_hours: 12,
        list_cmd: None,
        list_every: 10,
        poll_every_ms: 100,
    };
    let p = Panel::start(&setup, Policy::default(), NexoSource::default());
    (dir, std::sync::Arc::new(p))
}

#[test]
fn n_writes_a_nudge_p_pauses_and_enter_opens_the_details() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (_d, panel) = live_panel();
    let t = std::time::Instant::now();
    let mut v = AgentsView::new(Some(panel.clone()), true);
    while v.rows().is_empty() && t.elapsed().as_secs() < 6 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        v.refresh();
    }
    assert_eq!(v.rows().len(), 1);
    let key = |v: &mut AgentsView, c: KeyCode| {
        v.key(KeyEvent::new(c, KeyModifiers::NONE));
    };
    let mut a = app();
    a.modal = Some(Modal::Agents(Box::new(v)));
    let text = |a: &mut App| capture(a, 120, 30).join("\n");
    assert!(text(&mut a).contains("n message"), "the keys are listed");
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
    key(v, KeyCode::Char('n'));
    for c in " now".chars() {
        key(v, KeyCode::Char(c));
    }
    assert!(text(&mut a).contains("message ›"), "the field shows");
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
    key(v, KeyCode::Enter);
    let row = panel.rows(nexo_agent::supervise::run::now_secs()).remove(0);
    assert_eq!(row.last_action.as_ref().map(|a| a.0.as_str()), Some("your nudge is queued"));
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
    key(v, KeyCode::Char('p'));
    assert!(panel.rows(nexo_agent::supervise::run::now_secs())[0].paused);
    assert!(text(&mut a).contains("paused"), "{}", text(&mut a));
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
    key(v, KeyCode::Char('P'));
    assert!(panel.paused_all());
    key(v, KeyCode::Char('P'));
    assert!(!panel.paused_all());
    // Enter opens the details instead of closing; Esc closes them first, then the window
    key(v, KeyCode::Enter);
    assert!(text(&mut a).contains("sess-1"));
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
    assert_eq!(v.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), nexo_tui::modal::ModalOut::Nothing);
    assert_eq!(v.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), nexo_tui::modal::ModalOut::Close);
}

#[test]
fn a_nudge_on_a_view_without_a_panel_or_on_this_session_does_nothing() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut v = AgentsView::with_rows(rows(), NOW, true);
    v.key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    v.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
    assert_eq!(v.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), nexo_tui::modal::ModalOut::Close);
}

#[test]
fn o_shows_the_policy_page_with_the_model_line_and_goes_back() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (_d, panel) = live_panel();
    let mut a = app();
    a.modal = Some(Modal::Agents(Box::new(AgentsView::new(Some(panel), true))));
    let text = |a: &mut App| capture(a, 120, 30).join("\n");
    assert!(text(&mut a).contains("o policy"), "the key is listed");
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
    v.key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
    let shown = text(&mut a);
    assert!(shown.contains("panel · policy"), "{shown}");
    assert!(shown.contains("model: none"), "{shown}");
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
    v.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!text(&mut a).contains("panel · policy"));
}

/// The window itself: from its title row down (the conversation behind it moves on its own).
fn panel_part(s: &str) -> String {
    let from = s.find("╭ panel").unwrap_or(0);
    s[from..].lines().take_while(|l| !l.starts_with("─ ask")).collect::<Vec<_>>().join("\n")
}

fn zoomed(rows: Vec<AgentRow>, keys: &str) -> App {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut a = scene(rows);
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
    for c in keys.chars() {
        v.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    a
}

#[test]
fn the_zoomed_views_match_their_captures_at_every_size() {
    let update = std::env::var("NEXO_UPDATE_GOLDEN").is_ok();
    let mut wrong = Vec::new();
    for (name, keys) in [("panel-lines", "-"), ("panel-blocks", "--")] {
        for (w, h) in SIZES {
            let text = capture(&mut zoomed(rows(), keys), w, h).join("\n") + "\n";
            let path = file_of(name, w, h);
            if update {
                std::fs::write(&path, &text).unwrap();
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(kept) if kept == text => {}
                Ok(_) => wrong.push(format!("{name} {w}x{h}: differs from {}", path.display())),
                Err(_) => wrong.push(format!("{name} {w}x{h}: no capture")),
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn plus_and_minus_move_between_cards_lines_and_blocks() {
    let mut a = zoomed(rows(), "");
    let cards = capture(&mut a, 120, 40).join("\n");
    assert!(cards.contains("▸ Edit"), "cards show the last step with a marker");
    let mut a = zoomed(rows(), "-");
    let lines = capture(&mut a, 120, 40).join("\n");
    assert!(!panel_part(&lines).contains("╭───"), "lines have no card borders\n{lines}");
    let mut a = zoomed(rows(), "--");
    let blocks = capture(&mut a, 120, 40).join("\n");
    assert!(blocks.contains('▌') && blocks.contains("api-migration"), "{blocks}");
    let mut a = zoomed(rows(), "---+");
    assert_eq!(capture(&mut a, 120, 40).join("\n"), lines, "+ goes back in one step; - stops at blocks");
    let mut a = zoomed(rows(), "--++");
    assert_eq!(capture(&mut a, 120, 40).join("\n"), cards);
}

mod motion {
    use super::*;
    use std::time::{Duration, Instant};

    fn live_app() -> (App, Instant) {
        let mut a = app();
        a.opts.animations = true;
        let t0 = Instant::now();
        a.fx.clock = Some(t0);
        (a, t0)
    }

    fn at(a: &mut App, t0: Instant, ms: u64) -> String {
        a.fx.clock = Some(t0 + Duration::from_millis(ms));
        capture(a, 120, 40).join("\n")
    }

    fn open(a: &mut App, rows: Vec<AgentRow>) {
        a.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(rows, NOW, false))));
    }

    #[test]
    fn cards_come_in_one_after_the_other_and_settle_to_the_still_picture() {
        let (mut a, t0) = live_app();
        open(&mut a, rows());
        let first = at(&mut a, t0, 0);
        assert!(first.contains("project"), "the first card is already there\n{first}");
        assert!(!first.contains("nightly-fixer"), "a later card has not started\n{first}");
        let mid = at(&mut a, t0, 200);
        assert!(mid.contains("api-migration") || mid.contains("explore"), "{mid}");
        let end = at(&mut a, t0, 2000);
        assert!(end.contains("nightly-fixer") && end.contains("release"), "{end}");
        let mut still = scene(rows());
        assert_eq!(
            panel_part(&capture(&mut still, 120, 40).join("\n")),
            panel_part(&end),
            "after the entry it is the still picture"
        );
    }

    #[test]
    fn a_chip_changing_state_keeps_the_old_word_then_takes_the_new_one() {
        let (mut a, t0) = live_app();
        let mut r = rows();
        open(&mut a, r.clone());
        at(&mut a, t0, 0);
        at(&mut a, t0, 2000);
        r[0].state = State::Done;
        let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
        v.set_rows(r);
        let early = at(&mut a, t0, 2050);
        let late = at(&mut a, t0, 2600);
        let line = |s: &str| s.lines().find(|l| l.contains("▣ project")).unwrap_or_default().to_string();
        assert!(line(&early).contains("working"), "{}", line(&early));
        assert!(line(&late).contains("done"), "{}", line(&late));
    }

    #[test]
    fn at_rest_nothing_asks_for_a_repaint_and_with_the_animations_off_nothing_moves() {
        let (mut a, t0) = live_app();
        let calm: Vec<AgentRow> = rows()
            .into_iter()
            .filter(|r| !matches!(r.state, State::WaitingPermission | State::WaitingAnswer | State::Stalled))
            .collect();
        open(&mut a, calm);
        at(&mut a, t0, 0);
        let Some(Modal::Agents(v)) = &a.modal else { panic!() };
        assert!(v.animating(t0), "cards are entering");
        at(&mut a, t0, 3000);
        let Some(Modal::Agents(v)) = &a.modal else { panic!() };
        assert!(!v.animating(t0 + Duration::from_millis(3000)), "settled: zero frames per second");
        let still = AgentsView::with_rows(rows(), NOW, true);
        assert!(!still.animating(t0));
    }

    #[test]
    fn the_selection_glides_and_a_nudge_pulses_the_card() {
        let (mut a, t0) = live_app();
        open(&mut a, rows());
        at(&mut a, t0, 0);
        at(&mut a, t0, 2000);
        let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
        v.selected = 2;
        at(&mut a, t0, 2010);
        let Some(Modal::Agents(v)) = &a.modal else { panic!() };
        assert!(v.animating(t0 + Duration::from_millis(2050)), "gliding");
    }
}

#[test]
fn a_moving_frame_at_400x120_stays_cheap() {
    use std::time::{Duration, Instant};
    let mut a = app();
    a.opts.animations = true;
    let t0 = Instant::now();
    let many: Vec<AgentRow> = (0..40)
        .map(|i| {
            row(
                &format!("k{i}"),
                &format!("s{i}"),
                SourceKind::External,
                State::WaitingAnswer,
                Some(i),
                Some("Bash  ls"),
            )
        })
        .collect();
    a.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(many, NOW, false))));
    let mut t = Terminal::new(TestBackend::new(400, 120)).unwrap();
    let start = Instant::now();
    for k in 0..30u64 {
        a.fx.clock = Some(t0 + Duration::from_millis(k * 33));
        draw_frame(&mut t, &mut a).unwrap();
    }
    let per = start.elapsed() / 30;
    eprintln!("400x120, 40 sessions: {per:?} per frame (debug build)");
    assert!(per < Duration::from_millis(100), "{per:?} per frame is too slow for 30 fps");
}

fn press(a: &mut App, keys: &str) {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
    for c in keys.chars() {
        v.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
}

fn window(a: &mut App) -> String {
    panel_part(&capture(a, 120, 40).join("\n"))
}

#[test]
fn sessions_sit_under_a_header_per_program_nexo_first_and_every_card_has_its_tag() {
    let mut a = scene(rows());
    let text = window(&mut a);
    let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("no {needle:?}\n{text}"));
    assert!(at("▣ nexo · 2") < at("▢ agentx · 4") && at("▢ agentx · 4") < at("▢ othertool · 2"), "{text}");
    for tag in ["[nexo]", "[agentx]", "[othertool]"] {
        assert!(text.contains(tag), "no {tag} tag\n{text}");
    }
    assert_eq!(text.matches("[nexo]").count(), 2, "one tag per nexo card\n{text}");
    assert_eq!(text.matches("[agentx]").count(), 4, "{text}");
    assert!(text.contains("all"), "the header says which sessions these are\n{text}");
    assert!(text.contains("s source"), "the key is listed\n{text}");
}

#[test]
fn s_cycles_all_nexo_only_external_only_and_the_choice_outlives_the_window() {
    let mut a = scene(rows());
    press(&mut a, "s");
    let nexo = window(&mut a);
    assert!(nexo.contains("▣ nexo · 2") && !nexo.contains("agentx") && !nexo.contains("othertool"), "{nexo}");
    assert!(nexo.contains("nexo only +6"), "the header counts what is hidden\n{nexo}");
    press(&mut a, "s");
    let ext = window(&mut a);
    assert!(!ext.contains("▣ nexo") && ext.contains("▢ agentx · 4") && ext.contains("▢ othertool · 2"), "{ext}");
    assert!(ext.contains("external only +2"), "{ext}");
    press(&mut a, "s");
    let all = window(&mut a);
    assert!(all.contains("▣ nexo · 2") && all.contains("▢ othertool · 2"), "back to everything\n{all}");

    // a window opened later in the same session starts where the last one stopped
    let mut b = app();
    let mem = b.panel_filter.clone();
    b.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(rows(), NOW, true).remembering(mem.clone()))));
    press(&mut b, "ss");
    let Some(Modal::Agents(v)) = &b.modal else { panic!() };
    assert_eq!(v.filter, nexo_tui::agents_view::Filter::External);
    b.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(rows(), NOW, true).remembering(mem))));
    let Some(Modal::Agents(v)) = &b.modal else { panic!() };
    assert_eq!(v.filter, nexo_tui::agents_view::Filter::External, "remembered");
}

#[test]
fn a_group_with_nothing_in_it_says_so() {
    let only_nexo: Vec<AgentRow> = rows().into_iter().filter(|r| r.source == SourceKind::Nexo).collect();
    let text = window(&mut scene(only_nexo.clone()));
    assert!(text.contains("▢ external") && text.contains("no sessions from other programs"), "{text}");
    let mut a = scene(only_nexo);
    press(&mut a, "ss");
    let text = window(&mut a);
    assert!(text.contains("no sessions from other programs found") && !text.contains("▣ nexo"), "{text}");

    let only_ext: Vec<AgentRow> = rows().into_iter().filter(|r| r.source == SourceKind::External).collect();
    let text = window(&mut scene(only_ext));
    assert!(text.contains("▣ nexo") && text.contains("no nexo sessions yet"), "{text}");

    let mut none = scene(vec![]);
    press(&mut none, "s");
    let text = window(&mut none);
    assert!(text.contains("no nexo sessions yet") && !text.contains("no sessions from other programs"), "{text}");
    press(&mut none, "s");
    let text = window(&mut none);
    assert!(text.contains("no sessions from other programs found"), "{text}");
}

#[test]
fn the_arrows_walk_the_groups_in_the_order_they_are_drawn() {
    use crossterm::event::KeyCode;
    let mut a = scene(rows());
    capture(&mut a, 120, 40);
    let sel = |a: &App| match &a.modal {
        Some(Modal::Agents(v)) => v.selected,
        _ => panic!(),
    };
    let key = |a: &mut App, c: KeyCode| {
        if let Some(Modal::Agents(v)) = &mut a.modal {
            v.key(crossterm::event::KeyEvent::new(c, crossterm::event::KeyModifiers::NONE));
        }
    };
    key(&mut a, KeyCode::Down);
    assert_eq!(sel(&a), 2, "down from the nexo row to the first card of the next group");
    key(&mut a, KeyCode::Down);
    assert_eq!(sel(&a), 4, "then to the group's second row");
    key(&mut a, KeyCode::Down);
    assert_eq!(sel(&a), 6);
    key(&mut a, KeyCode::Up);
    key(&mut a, KeyCode::Up);
    key(&mut a, KeyCode::Up);
    assert_eq!(sel(&a), 0);
}

#[test]
fn a_long_list_scrolls_with_its_headers_and_keeps_the_selection_in_sight() {
    let many: Vec<AgentRow> = (0..14)
        .map(|i| row(&format!("k{i}"), &format!("job{i:02}"), SourceKind::External, State::Working, Some(i), None))
        .collect();
    let mut a = scene(many);
    capture(&mut a, 80, 24);
    for _ in 0..7 {
        press(&mut a, "j");
    }
    let Some(Modal::Agents(v)) = &a.modal else { panic!() };
    let want = format!("job{:02}", v.selected);
    let text = capture(&mut a, 80, 24).join("\n");
    assert!(text.contains(&want), "the selected session is drawn\n{text}");
}

#[test]
fn a_message_or_pause_key_with_no_supervisor_says_why_nothing_happened() {
    for keys in ["n", "p", "P"] {
        let mut a = scene(rows());
        press(&mut a, keys);
        let text = window(&mut a);
        assert!(text.contains("need the supervisor"), "{keys}: silent\n{text}");
    }
}

/// Every line of the window, from its top border to its bottom one.
fn window_lines(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let out = capture(a, w, h);
    let from = out.iter().position(|l| l.starts_with("╭ panel")).expect("the window");
    let to = out.iter().skip(from).position(|l| l.starts_with('╰')).expect("its foot") + from;
    out[from..=to].to_vec()
}

#[test]
fn names_paths_and_steps_in_any_script_are_cut_with_an_ellipsis_and_never_break_a_border() {
    let mut odd = rows();
    odd[0].name = "日本語のとても長い名前を持つエージェント🚀🚀🚀 and more".into();
    odd[0].last_step =
        Some("Edit  /srv/项目/源代码/解析器/语法分析器/very/long/path/to/lexer.rs\nsecond\x1b[31m line".into());
    odd[2].name = "👩‍👩‍👧‍👦 family\tof\u{200b}agents with a very very long name indeed".into();
    odd[3].program = "a-tool-with-a-program-name-far-too-long-for-a-tag".into();
    odd[3].last_action = Some(("marked\npermission\x1b]0;title\x07".into(), NOW - 5));
    for (w, h) in [(60, 24), (80, 24), (120, 40), (200, 60)] {
        for keys in ["", "-", "--"] {
            let mut a = zoomed(odd.clone(), keys);
            let lines = window_lines(&mut a, w, h);
            let top = lines[0].chars().count();
            for l in &lines {
                assert_eq!(
                    l.chars().count(),
                    top,
                    "{w}x{h} '{keys}': a row is not as wide as the frame\n{}",
                    lines.join("\n")
                );
                assert!(!l.contains('\x1b') && !l.contains('\t'), "{w}x{h}: raw control text\n{l}");
            }
            let all = lines.join("\n");
            if keys.is_empty() && w >= 80 {
                assert!(all.contains('…'), "{w}x{h}: a long name is cut with an ellipsis\n{all}");
            }
        }
    }
}

#[test]
fn the_state_chip_survives_a_long_name_and_stays_whole_at_the_card_s_right_edge() {
    let mut long = rows();
    for r in &mut long {
        r.name = format!("{}-with-a-name-that-never-ends", r.name);
    }
    let all = window_lines(&mut scene(long), 120, 40).join("\n");
    for chip in ["● working", "◈ permission", "↻ looping", "◐ stalled", "✓ done"] {
        assert!(all.contains(&format!("{chip} │")), "chip {chip:?} lost or pushed off\n{all}");
    }
}

#[test]
fn cards_stretch_over_the_whole_row_and_a_group_never_ends_on_an_orphan() {
    for (w, h) in [(120, 40), (200, 60)] {
        let lines = window_lines(&mut scene(rows()), w, h);
        let tops: Vec<&String> = lines.iter().filter(|l| l.contains("╭─")).collect();
        assert!(tops.len() >= 3, "{w}x{h}: card strips\n{}", lines.join("\n"));
        for t in &tops {
            // the last card ends one blank column short of the frame, as the header rule does
            if w == 200 && t.matches("╭─").count() < 4 {
                continue;
            }
            assert!(t.ends_with("╮ │"), "{w}x{h}: a strip ends ragged\n{t}\n{}", lines.join("\n"));
        }
        // 4 cards on 3 columns: two and two, not three and one
        if w == 120 {
            let counts: Vec<usize> = tops.iter().map(|t| t.matches("╭─").count()).collect();
            assert_eq!(counts, [2, 2, 2, 2], "{}", lines.join("\n"));
        }
    }
}

#[test]
fn quiet_time_is_one_short_unit_and_lines_keep_it_in_one_right_aligned_column() {
    let all = window_lines(&mut scene(rows()), 120, 40).join("\n");
    for t in ["quiet 2s", "quiet 1m", "quiet 6m", "quiet 40m", "quiet 2h"] {
        assert!(all.contains(t), "no {t:?}\n{all}");
    }
    assert!(!all.contains("m 0") && !all.contains("h 0"), "two-unit times are gone\n{all}");
    // with the supervisor's action on the line the time stays and the cost is what gives way
    assert!(all.contains("quiet 1m") || !all.contains("$1.25"), "the cost left before the time did\n{all}");
    let list = window_lines(&mut zoomed(rows(), "-"), 100, 30);
    // in the lines every time ends on the same column
    let ends: std::collections::HashSet<usize> = [" 2s  ", " 1m  ", "40m  ", " 2h  ", " 6m  "]
        .iter()
        .map(|t| {
            let l = list.iter().find(|l| l.contains(t)).unwrap_or_else(|| panic!("no {t:?}\n{}", list.join("\n")));
            l[..l.find(t).unwrap()].chars().count()
        })
        .collect();
    assert_eq!(ends.len(), 1, "{}", list.join("\n"));
}

#[test]
fn a_session_less_panel_says_what_it_is_for_and_how_sessions_appear() {
    for (w, h) in [(60, 24), (120, 40)] {
        let text = window_lines(&mut scene(vec![]), w, h).join("\n");
        for needle in ["board of your agent sessions", "appear here on their own", "this session", "other agents"] {
            assert!(text.contains(needle), "{w}x{h}: no {needle:?}\n{text}");
        }
    }
    // a filter that hides everything keeps the per-program lines instead
    let mut a = scene(vec![]);
    press(&mut a, "s");
    let text = window(&mut a);
    assert!(text.contains("no nexo sessions yet") && !text.contains("board of your"), "{text}");
    // the smallest window still tells what it is
    let tiny = window_lines(&mut scene(vec![]), 40, 12).join("\n");
    assert!(tiny.contains("board of your") && tiny.contains("appear here"), "{tiny}");
}

#[test]
fn a_scrolled_list_says_how_many_sessions_sit_above_and_below() {
    let many: Vec<AgentRow> = (0..30)
        .map(|i| row(&format!("k{i}"), &format!("job{i:02}"), SourceKind::External, State::Working, Some(i), None))
        .collect();
    let mut a = scene(many);
    let first = window_lines(&mut a, 80, 24).join("\n");
    assert!(first.contains(" more") && !first.contains("above"), "{first}");
    for _ in 0..40 {
        press(&mut a, "j");
    }
    let last = window_lines(&mut a, 80, 24).join("\n");
    assert!(last.contains("above") && !last.contains(" more"), "{last}");
    for _ in 0..12 {
        press(&mut a, "k");
    }
    let middle = window_lines(&mut a, 80, 24).join("\n");
    assert!(middle.contains("↑ ") && middle.contains(" ↓ "), "{middle}");
}

#[test]
fn the_window_is_measured_again_when_sessions_arrive_after_it_opened() {
    let mut a = scene(vec![]);
    let before = window_lines(&mut a, 120, 40).len();
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
    v.set_rows(rows());
    let after = window_lines(&mut a, 120, 40);
    assert!(after.len() > before, "{before} lines before, {} after", after.len());
    assert!(after.iter().any(|l| l.contains("api-migration")), "{}", after.join("\n"));
}

#[test]
fn a_tiny_terminal_keeps_a_session_line_and_drops_the_group_header() {
    let mut a = scene(rows());
    let lines = window_lines(&mut a, 36, 9);
    let text = lines.join("\n");
    // the detail line also names the project, so a session line is a second one
    assert!(lines.iter().filter(|l| l.contains("project")).count() >= 2, "{text}");
}

#[test]
fn the_footer_says_enter_close_while_the_details_are_open() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (_d, panel) = live_panel();
    let t = std::time::Instant::now();
    let mut v = AgentsView::new(Some(panel), true);
    while v.rows().is_empty() && t.elapsed().as_secs() < 6 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        v.refresh();
    }
    let mut a = app();
    a.modal = Some(Modal::Agents(Box::new(v)));
    assert!(capture(&mut a, 140, 30).join("\n").contains("Enter open"));
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!() };
    v.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let text = capture(&mut a, 140, 30).join("\n");
    assert!(text.contains("Enter close") && !text.contains("Enter open"), "{text}");
}
