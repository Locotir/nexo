//! `/panel` inside the app, drawn at sizes from a split pane to a huge window: captures kept in `tests/captures/`
//! (`NEXO_UPDATE_GOLDEN=1 cargo test` accepts a deliberate change), the command, the supervisor's keys, and the
//! things that must hold at every size. The mission control itself (rows of every kind, keys per kind, the narrow
//! mode, rest) is tested in `panel.rs`.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_supervise::{AgentRow, SourceKind, State};
use nexo_tui::agents_view::AgentsView;
use nexo_tui::commands::{lookup, parse, Command, COMMANDS};
use nexo_tui::modal::{Modal, ModalOut};
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
        hints: true,
        keys: Default::default(),
        notify_after: 15,
        animations: false,
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

fn row(key: &str, name: &str, source: SourceKind, state: State, idle: Option<u64>, step: Option<&str>) -> AgentRow {
    AgentRow {
        key: key.into(),
        name: name.into(),
        source,
        program: if source == SourceKind::Nexo { "nexo" } else { "agentx" }.into(),
        state,
        since: NOW - 60,
        last_step: step.map(Into::into),
        last_conclusion: None,
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
        ctx_pct: None,
        goal: None,
        lane: None,
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

fn key(a: &mut App, c: KeyCode) -> ModalOut {
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!("the panel is open") };
    v.key(KeyEvent::new(c, KeyModifiers::NONE))
}

fn press(a: &mut App, keys: &str) {
    for c in keys.chars() {
        key(a, KeyCode::Char(c));
    }
}

fn view(a: &mut App) -> &mut AgentsView {
    let Some(Modal::Agents(v)) = &mut a.modal else { panic!("the panel is open") };
    v
}

/// Every line of the window, from its top border to its bottom one.
fn window_lines(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let out = capture(a, w, h);
    let from = out.iter().position(|l| l.starts_with("╭ panel")).expect("the window");
    let to = out.iter().skip(from).position(|l| l.starts_with('╰')).expect("its foot") + from;
    out[from..=to].to_vec()
}

fn window(a: &mut App) -> String {
    window_lines(a, 120, 40).join("\n")
}

/// A live panel reading one session of another program from a temporary log folder.
fn live_panel() -> (tempfile::TempDir, std::sync::Arc<nexo_supervise::Panel>) {
    use nexo_supervise::{NexoSource, Panel, Policy, Setup};
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

/// The live panel's view once its poller has seen the log; generous, a loaded machine's poller can be slow.
fn live_view() -> (tempfile::TempDir, std::sync::Arc<nexo_supervise::Panel>, AgentsView) {
    let (d, panel) = live_panel();
    let t = std::time::Instant::now();
    let mut v = AgentsView::new(Some(panel.clone()), true);
    while v.rows().is_empty() && t.elapsed().as_secs() < 60 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        v.refresh();
    }
    assert_eq!(v.rows().len(), 1);
    // the row is another program's, whose group starts folded: unfold it and put the cursor on the row
    v.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    v.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(v.selected_key().is_some());
    (d, panel, v)
}

fn peers(n: usize) -> Vec<AgentRow> {
    (0..n)
        .map(|i| {
            row(&format!("nexo:k{i}"), &format!("job{i:02}"), SourceKind::Nexo, State::Working, Some(i as u64), None)
        })
        .collect()
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
fn at_every_size_the_window_is_whole_and_keeps_its_title_and_its_way_out() {
    for (w, h) in SIZES {
        let out = capture(&mut scene(rows()), w, h);
        assert_eq!(out.len(), h as usize);
        assert!(out.iter().all(|r| r.chars().count() <= w as usize), "{w}x{h}");
        let all = out.join("\n");
        assert!(all.contains("panel"), "{w}x{h}: no title\n{all}");
        if h >= 12 && w >= 40 {
            assert!(all.contains("Esc"), "{w}x{h}: no footer\n{all}");
        }
        if w >= 80 {
            assert!(all.contains("read-only"), "{w}x{h}: the header says there is no supervisor\n{all}");
        }
    }
}

#[test]
fn panel_is_a_command_and_agents_answers_without_being_listed() {
    assert_eq!(parse("/panel"), Some(Command::Panel));
    assert_eq!(parse("/agents"), Some(Command::Panel));
    assert!(lookup("agents").is_some());
    assert!(COMMANDS.iter().all(|s| s.name != "agents" && !s.aliases.contains(&"agents")));
    // typed whole, the palette finds the command it stands for
    let ranked = nexo_tui::commands::rank("agents");
    assert_eq!(COMMANDS[ranked[0].index].name, "panel");
}

#[test]
fn the_command_opens_the_panel_and_team_opens_it_on_its_lanes() {
    // a supervisor of temporary logs: the command would otherwise start one that reads the real ones
    let (_d, panel) = live_panel();
    let mut a = app();
    a.panel = Some(panel.clone());
    a.input.set_text("/panel");
    a.run_command(parse("/panel").unwrap());
    assert!(matches!(a.modal, Some(Modal::Agents(_))));
    let mut a = app();
    a.panel = Some(panel);
    a.run_command(parse("/team").unwrap());
    assert!(matches!(a.modal, Some(Modal::Agents(_))), "/team is the panel now");
    let text = window(&mut a);
    assert!(text.contains("LANES"), "the lanes group is there even with no lane yet\n{text}");
}

#[test]
fn n_writes_a_nudge_z_pauses_and_enter_observes_until_esc() {
    let (_d, panel, v) = live_view();
    let mut a = app();
    a.modal = Some(Modal::Agents(Box::new(v)));
    let text = |a: &mut App| capture(a, 120, 30).join("\n");
    assert!(text(&mut a).contains("n message"), "the keys are listed");
    press(&mut a, "n now");
    assert!(text(&mut a).contains("message ›"), "the field shows");
    key(&mut a, KeyCode::Enter);
    let row = panel.rows(nexo_supervise::run::now_secs()).remove(0);
    assert_eq!(row.last_action.as_ref().map(|a| a.0.as_str()), Some("your nudge is queued"));
    press(&mut a, "z");
    assert!(panel.rows(nexo_supervise::run::now_secs())[0].paused);
    assert!(text(&mut a).contains("supervision paused"), "{}", text(&mut a));
    press(&mut a, "Z");
    assert!(panel.paused_all());
    press(&mut a, "Z");
    assert!(!panel.paused_all());
    // Enter observes instead of closing; Esc leaves the observation first, then the window
    let out = key(&mut a, KeyCode::Enter);
    a.modal_out(out);
    assert!(text(&mut a).contains("observation"));
    assert_eq!(key(&mut a, KeyCode::Esc), ModalOut::Nothing);
    assert_eq!(key(&mut a, KeyCode::Esc), ModalOut::Close);
}

#[test]
fn a_nudge_or_pause_on_a_view_without_a_supervisor_says_why_nothing_happened() {
    for keys in ["n", "z", "Z"] {
        let mut a = scene(rows());
        press(&mut a, keys);
        let text = window(&mut a);
        assert!(text.contains("need the supervisor"), "{keys}: silent\n{text}");
        assert_eq!(key(&mut a, KeyCode::Esc), ModalOut::Close, "{keys}: nothing is left open");
    }
}

#[test]
fn capital_p_shows_the_policy_page_with_the_model_line_and_goes_back() {
    let (_d, panel) = live_panel();
    let mut a = app();
    a.modal = Some(Modal::Agents(Box::new(AgentsView::new(Some(panel), true))));
    assert!(capture(&mut a, 160, 30).join("\n").contains("P policy"), "the key is listed");
    press(&mut a, "P");
    let shown = capture(&mut a, 120, 30).join("\n");
    assert!(shown.contains("panel · policy"), "{shown}");
    assert!(shown.contains("model: none"), "{shown}");
    key(&mut a, KeyCode::Esc);
    assert!(!capture(&mut a, 120, 30).join("\n").contains("panel · policy"));
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
                &format!("nexo:k{i}"),
                &format!("s{i}"),
                SourceKind::Nexo,
                State::WaitingPermission,
                Some(i),
                Some("Bash  ls"),
            )
        })
        .collect();
    a.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(many, NOW, false))));
    let mut t = Terminal::new(TestBackend::new(400, 120)).unwrap();
    // best of five rounds: a busy machine slows some rounds, only a slow renderer slows them all
    let mut per = Duration::MAX;
    for round in 0..5u64 {
        let start = Instant::now();
        for k in 0..30u64 {
            a.fx.clock = Some(t0 + Duration::from_millis((round * 30 + k) * 33));
            draw_frame(&mut t, &mut a).unwrap();
        }
        per = per.min(start.elapsed() / 30);
    }
    eprintln!("400x120, 40 sessions: {per:?} per frame (debug build)");
    assert!(per < Duration::from_millis(100), "{per:?} per frame is too slow for 30 fps");
}

#[test]
fn programs_come_after_this_session_folded_to_their_count_and_a_waiting_row_still_shows() {
    let text = window(&mut scene(rows()));
    let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("no {needle:?}\n{text}"));
    assert!(at("THIS SESSION") < at("AGENTX · 4") && at("AGENTX · 4") < at("OTHERTOOL · 2"), "{text}");
    assert!(!text.contains("refactor"), "a folded program shows no rows\n{text}");
    assert!(at("NEEDS YOU") < at("api-migration"), "the session waiting on a permission is under NEEDS YOU\n{text}");
}

#[test]
fn capital_f_cycles_all_nexo_only_external_only_and_the_choice_outlives_the_window() {
    let mut a = scene(rows());
    press(&mut a, "F");
    let nexo = window(&mut a);
    assert!(nexo.contains("project") && !nexo.contains("AGENTX") && !nexo.contains("OTHERTOOL"), "{nexo}");
    assert!(nexo.contains("nexo only"), "the header says what is shown\n{nexo}");
    press(&mut a, "F");
    let ext = window(&mut a);
    assert!(!ext.contains("THIS SESSION") && ext.contains("AGENTX · 4"), "{ext}");
    press(&mut a, "F");
    let all = window(&mut a);
    assert!(all.contains("THIS SESSION") && all.contains("OTHERTOOL · 2"), "back to everything\n{all}");

    // a window opened later in the same session starts where the last one stopped
    let mut b = app();
    let mem = b.panel_filter.clone();
    b.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(rows(), NOW, true).remembering(mem.clone()))));
    press(&mut b, "FF");
    assert_eq!(view(&mut b).filter, nexo_tui::agents_view::Filter::External);
    b.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(rows(), NOW, true).remembering(mem))));
    assert_eq!(view(&mut b).filter, nexo_tui::agents_view::Filter::External, "remembered");
}

#[test]
fn a_long_list_scrolls_keeps_the_selection_in_sight_and_says_how_many_lines_are_out_of_it() {
    let mut a = scene(peers(30));
    let first = window_lines(&mut a, 80, 24).join("\n");
    assert!(first.contains(" more") && !first.contains("above"), "{first}");
    for _ in 0..40 {
        key(&mut a, KeyCode::Down);
    }
    let last = window_lines(&mut a, 80, 24).join("\n");
    let at = view(&mut a).selected_key();
    let want = view(&mut a).rows().iter().find(|r| Some(&r.key) == at.as_ref()).map(|r| r.name.clone());
    assert!(last.contains(want.as_deref().unwrap()), "the selected session is drawn\n{last}");
    assert!(last.contains("above") && !last.contains(" more"), "{last}");
    // far enough up that the tree scrolls back: lines are then out of sight on both sides
    for _ in 0..20 {
        key(&mut a, KeyCode::Up);
    }
    let middle = window_lines(&mut a, 80, 24).join("\n");
    assert!(middle.contains("↑ ") && middle.contains(" ↓ "), "{middle}");
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
        let lines = window_lines(&mut scene(odd.clone()), w, h);
        let top = lines[0].chars().count();
        for l in &lines {
            assert_eq!(l.chars().count(), top, "{w}x{h}: a row is not as wide as the frame\n{}", lines.join("\n"));
            assert!(!l.contains('\x1b') && !l.contains('\t'), "{w}x{h}: raw control text\n{l}");
        }
        let all = lines.join("\n");
        assert!(all.contains('…'), "{w}x{h}: a long name is cut with an ellipsis\n{all}");
    }
}

#[test]
fn a_session_less_panel_says_what_it_is_for_and_how_rows_appear() {
    for (w, h) in [(60, 24), (120, 40)] {
        let text = window_lines(&mut scene(vec![]), w, h).join("\n");
        for needle in ["Nothing running besides this session", "appear here on their own", "nexo team lanes"] {
            assert!(text.contains(needle), "{w}x{h}: no {needle:?}\n{text}");
        }
    }
    // the smallest window still tells what it is
    let tiny = window_lines(&mut scene(vec![]), 40, 12).join("\n");
    assert!(tiny.contains("Nothing running"), "{tiny}");
}

#[test]
fn rows_that_arrive_after_the_window_opened_are_drawn() {
    let mut a = scene(vec![]);
    assert!(!window(&mut a).contains("api-migration"));
    view(&mut a).set_rows(rows());
    let after = window(&mut a);
    assert!(after.contains("api-migration"), "{after}");
}

#[test]
fn a_tiny_terminal_keeps_a_session_line() {
    let mut a = scene(rows());
    let text = window_lines(&mut a, 36, 9).join("\n");
    assert!(text.contains("project") || text.contains("api-migr"), "{text}");
}

#[test]
fn the_footer_offers_observe_then_back_from_the_observation() {
    let (_d, _panel, v) = live_view();
    let mut a = app();
    a.modal = Some(Modal::Agents(Box::new(v)));
    assert!(capture(&mut a, 140, 30).join("\n").contains("⏎ observe"));
    let out = key(&mut a, KeyCode::Enter);
    a.modal_out(out);
    let text = capture(&mut a, 140, 30).join("\n");
    assert!(text.contains("Esc back") && text.contains("observation"), "{text}");
}

#[test]
fn the_footer_of_a_wide_window_ends_on_esc_close_not_a_bare_esc() {
    let (_d, _panel, v) = live_view();
    let mut a = app();
    a.modal = Some(Modal::Agents(Box::new(v)));
    let text = capture(&mut a, 200, 30);
    let foot = text.iter().find(|l| l.contains("z pause watch")).expect("the full key list shows at 200 columns");
    assert!(foot.contains("P policy · Esc close"), "{foot}");
}
