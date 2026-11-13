//! `[ui] detail = "quiet"` (the default), the notes of the transcript: no curator notice, a turn that ends with
//! its time and cost (the whole line opens on demand), `/goal` in a few words, the folded thinking line with no
//! line count, the tool group as counts, `/diff` outside git in four words. `detail = "full"` keeps every old text.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nexo_core::agent::{AgentEvent, GoalState, Level, TurnEnd};
use nexo_core::config::Detail;
use nexo_core::stream::Usage;
use nexo_core::tool::{ToolDisplay, ToolOutput};
use nexo_tui::app::Goal;
use nexo_tui::commands::{Command, GoalArg};
use nexo_tui::render::blocks::Block;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

const CURATOR: &str = "the curator kept the view at 7.4k tokens (history 14.9k)";

fn app(detail: Detail) -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        theme: "dark".into(),
        timestamps: false,
        suggest: false,
        hints: true,
        animations: false,
        group_tools: true,
        detail,
        session_id: Some("20261027-120000-ab12".into()),
        ..TuiOptions::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}

/// The turn starts 14 s before it ends on the real clock, so the tenths of its time depend on how fast the
/// machine is: `14.0s` and `14.1s` both read `14.Ns` here, and everything else is compared exactly.
fn steady(s: &str) -> String {
    let mut out = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if s[i..].starts_with("14.") && b.get(i + 3).is_some_and(u8::is_ascii_digit) && b.get(i + 4) == Some(&b's') {
            out.push_str("14.Ns");
            i += 5;
        } else {
            let c = s[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

fn screen(a: &mut App, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    steady(&rows_of(t.backend().buffer()).join("\n"))
}

/// Rows the transcript takes at 100 columns.
fn transcript_rows(a: &mut App) -> usize {
    a.layout(100);
    a.total_lines
}

fn call(a: &mut App, id: &str, name: &str, summary: &str, output: ToolOutput) {
    a.handle_agent(AgentEvent::ToolCall {
        id: id.into(),
        name: name.into(),
        args: serde_json::json!({ "command": summary }),
        summary: summary.into(),
    });
    a.handle_agent(AgentEvent::ToolStart { id: id.into() });
    a.handle_agent(AgentEvent::ToolDone { id: id.into(), output, elapsed_ms: 40 });
}

/// A turn of one model call with `calls` tool calls, ended 14 seconds after it began.
fn turn(a: &mut App, calls: &[(&str, &str)]) {
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.turn_started = Some(Instant::now() - Duration::from_secs(14));
    for (i, (name, summary)) in calls.iter().enumerate() {
        call(a, &format!("c{i}"), name, summary, ToolOutput::ok("ok"));
    }
    let used = Usage { input: 2000, output: 412, cache_read: 12_000, ..Default::default() };
    a.handle_agent(AgentEvent::Usage { turn: used, call: used, session: used, cost_usd: 0.0042 });
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
}

fn usage_index(a: &App) -> usize {
    a.blocks.iter().position(|b| matches!(b, Block::Usage { .. })).expect("the turn left its line")
}

fn notices(a: &App) -> Vec<String> {
    a.blocks
        .iter()
        .filter_map(|b| match b {
            Block::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn item1_the_curator_notice_is_left_out_in_quiet_and_stays_in_full() {
    let mut q = app(Detail::Quiet);
    let before = transcript_rows(&mut q);
    q.notice(Level::Info, CURATOR);
    assert_eq!(transcript_rows(&mut q), before, "the notice takes no row");
    let s = screen(&mut q, 100, 30);
    assert!(!s.contains("trimmed") && !s.contains("curator"), "{s}");
    // a warning of the same words is never hidden
    q.notice(Level::Warn, CURATOR);
    assert!(transcript_rows(&mut q) > before, "a warning shows");
    assert!(screen(&mut q, 100, 30).contains("▲ context trimmed to 7.4k tokens (history 14.9k)"));

    let mut f = app(Detail::Full);
    f.notice(Level::Info, CURATOR);
    assert!(screen(&mut f, 100, 30).contains("◇ context trimmed to 7.4k tokens (history 14.9k)"));
}

#[test]
fn item2_the_turn_ends_with_time_and_cost_and_the_whole_line_opens_on_demand() {
    let mut q = app(Detail::Quiet);
    turn(&mut q, &[("bash", "cargo test")]);
    let s = screen(&mut q, 100, 30);
    assert!(s.lines().any(|l| l.trim() == "14.Ns · $0.0042"), "{s}");
    assert!(!s.contains("hit") && !s.contains("step") && !s.contains('↑'), "{s}");
    // a click or the expand key goes through toggle_block
    let i = usage_index(&q);
    assert!(q.foldable(i));
    q.toggle_block(i);
    let s = screen(&mut q, 100, 30);
    assert!(s.contains("1 step") && s.contains("hit") && s.contains("$0.0042") && s.contains("1 run"), "{s}");
    q.toggle_block(i);
    assert!(!screen(&mut q, 100, 30).contains("hit"), "it folds back");

    let mut f = app(Detail::Full);
    turn(&mut f, &[("bash", "cargo test")]);
    let s = screen(&mut f, 100, 30);
    assert!(s.contains("1 step") && s.contains("hit") && s.contains("↑"), "{s}");
    assert!(!f.foldable(usage_index(&f)), "nothing to open when it is whole already");
}

#[test]
fn item2_without_a_cost_the_line_is_the_time_only() {
    let mut q = app(Detail::Quiet);
    q.handle_agent(AgentEvent::TurnStart { turn: 1 });
    q.turn_started = Some(Instant::now() - Duration::from_secs(14));
    let used = Usage { input: 100, output: 10, ..Default::default() };
    q.handle_agent(AgentEvent::Usage { turn: used, call: used, session: used, cost_usd: 0.0 });
    q.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    let s = screen(&mut q, 100, 30);
    assert!(s.lines().any(|l| l.trim() == "14.Ns"), "{s}");
}

#[test]
fn item3_goal_says_goal_set_paused_or_off_and_no_keys() {
    let mut q = app(Detail::Quiet);
    q.run_command(Command::Goal(GoalArg::Show));
    q.goal = Some(Goal { text: "make the tests pass".into(), round: 0, max: 5, state: GoalState::Active });
    q.run_command(Command::Goal(GoalArg::Show));
    q.goal = Some(Goal { text: "make the tests pass".into(), round: 0, max: 5, state: GoalState::Paused("x".into()) });
    q.run_command(Command::Goal(GoalArg::Show));
    assert_eq!(notices(&q), ["goal off", "goal set", "goal paused"]);

    let mut f = app(Detail::Full);
    f.goal = Some(Goal { text: "make the tests pass".into(), round: 0, max: 5, state: GoalState::Active });
    f.run_command(Command::Goal(GoalArg::Show));
    assert_eq!(notices(&f), ["goal: make the tests pass · /goal pause holds it, /goal off clears it"]);
}

#[test]
fn item4_the_folded_thinking_line_has_no_line_count() {
    let think = || Block::Reasoning {
        text: "a\nb\nc".into(),
        streaming: false,
        started: None,
        elapsed_ms: Some(431),
        folded: true,
    };
    let mut q = app(Detail::Quiet);
    q.push(think());
    assert!(screen(&mut q, 100, 30).contains("▸ thinking · 431ms"));
    assert!(!screen(&mut q, 100, 30).contains("lines"));
    let mut f = app(Detail::Full);
    f.push(think());
    assert!(screen(&mut f, 100, 30).contains("▸ thinking · 3 lines · 431ms"));
}

#[test]
fn item5_the_tool_group_line_is_counts_only() {
    let edit = ToolOutput::ok("edited").with_display(ToolDisplay::Diff {
        path: "a.rs".into(),
        old: "a\n".into(),
        new: "b\n".into(),
        start_line: 1,
    });
    let build = |detail| {
        let mut a = app(detail);
        a.handle_agent(AgentEvent::TurnStart { turn: 1 });
        call(&mut a, "t", "todo", "todo", ToolOutput::ok("ok"));
        call(&mut a, "r", "read", "a.rs", ToolOutput::ok("ok"));
        call(&mut a, "e", "edit", "a.rs", edit.clone());
        call(&mut a, "b", "bash", "cargo test", ToolOutput::ok("ok"));
        a
    };
    let mut q = build(Detail::Quiet);
    let s = screen(&mut q, 100, 30);
    assert!(s.contains("todo, 1 read, 1 edit +1 -1, 1 run"), "{s}");
    assert!(!s.contains("Called") && !s.contains("shell command"), "{s}");
    let mut f = build(Detail::Full);
    let s = screen(&mut f, 100, 30);
    assert!(s.contains("Called todo 1 time, read 1 file, made 1 edit +1 −1, ran 1 shell command"), "{s}");
}

#[test]
fn item6_diff_outside_git_says_no_git_session_edits() {
    let dir = tempfile::tempdir().unwrap();
    // the folder must be outside any repository; a temp folder inside one cannot show what this checks
    let inside = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir.path())
        .output()
        .is_ok_and(|o| o.status.success());
    if inside {
        eprintln!("skipped: the temp folder {} is inside a git repository", dir.path().display());
        return;
    }
    let q = nexo_tui::diffview::collect::collect_for(dir.path(), Vec::new(), Detail::Quiet);
    assert_eq!(q.note.as_deref(), Some("no git: session edits"));
    let f = nexo_tui::diffview::collect::collect_for(dir.path(), Vec::new(), Detail::Full);
    assert_eq!(f.note.as_deref(), Some("not a git repo: showing this session's edits"));
}

#[test]
fn a_plain_one_tool_turn_takes_fewer_rows_in_quiet() {
    let rows = |detail| {
        let mut a = app(detail);
        let before = transcript_rows(&mut a);
        a.notice(Level::Info, CURATOR);
        turn(&mut a, &[("bash", "cargo test")]);
        transcript_rows(&mut a) - before
    };
    let (quiet, full) = (rows(Detail::Quiet), rows(Detail::Full));
    eprintln!("one-tool turn rows: full {full}, quiet {quiet}");
    assert!(quiet < full, "quiet {quiet} full {full}");
}

/// The block's rows as text, at `w` columns.
fn usage_rows(plan: bool, cost: f64, w: usize) -> Vec<String> {
    let b = Block::Usage {
        usage: Usage { input: 2000, output: 412, ..Default::default() },
        cost_usd: cost,
        elapsed_ms: 14_400,
        tally: Default::default(),
        plan,
        folded: true,
    };
    let opts = nexo_tui::render::blocks::RenderOpts {
        show_reasoning: true,
        fold_after: 3,
        spinner: 0,
        intro_ms: None,
        interpreted: false,
        timestamps: false,
        fx: false,
        detail: Detail::Quiet,
    };
    b.render(w, &nexo_tui::theme::Theme::dark(), opts)
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
        .collect()
}

#[test]
fn the_quiet_turn_line_of_a_plan_user_is_one_row_at_any_width() {
    let cases: [(usize, &str); 5] = [
        (60, "  14.4s · ≈$0.0042 API value · included in plan"),
        (40, "  14.4s · ≈$0.0042"),
        (36, "  14.4s · ≈$0.0042"),
        (20, "  14.4s · ≈$0.0042"),
        (12, "  14.4s"),
    ];
    for (w, want) in cases {
        assert_eq!(usage_rows(true, 0.0042, w), [want], "plan user at {w} columns");
    }
}

#[test]
fn the_quiet_turn_line_of_an_api_key_user_is_one_row_at_any_width() {
    let cases: [(usize, &str); 3] = [(60, "  14.4s · $0.0042"), (20, "  14.4s · $0.0042"), (12, "  14.4s")];
    for (w, want) in cases {
        assert_eq!(usage_rows(false, 0.0042, w), [want], "API key user at {w} columns");
    }
}

#[test]
fn under_the_time_itself_the_quiet_turn_line_is_cut_with_an_ellipsis_never_wrapped() {
    let theme = nexo_tui::theme::Theme::dark();
    let line = |plan, w| {
        nexo_tui::render::summary::plain(&nexo_tui::render::summary::short_line(&theme, 0.0042, 14_400, plan, w))
    };
    assert_eq!(line(true, 5), "  14…");
    assert_eq!(line(false, 5), "  14…");
    assert_eq!(line(true, 0), "");
    assert_eq!(line(false, 7), "  14.4s");
}

#[test]
fn shift_up_skips_the_time_line_and_a_click_still_opens_it() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let mut q = app(Detail::Quiet);
    turn(&mut q, &[("bash", "cargo test"), ("read", "a.rs")]);
    let s = screen(&mut q, 100, 30);
    nexo_tui::event::key(&mut q, KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
    let sel = q.selected.expect("a block is selected");
    assert!(matches!(q.blocks[sel], Block::Group { .. }), "the tool group, not the time line: {:?}", q.blocks[sel]);
    // Tab from there goes round without stopping on the time line either
    nexo_tui::event::key(&mut q, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert!(!matches!(q.blocks[q.selected.unwrap()], Block::Usage { .. }));

    // two clicks on the time line, as on any block: the first selects, the second opens
    let row = s.lines().position(|l| l.trim() == "14.Ns · $0.0042").expect("the time line is drawn") as u16;
    for _ in 0..2 {
        let ev = |kind| MouseEvent { kind, column: 4, row, modifiers: KeyModifiers::NONE };
        nexo_tui::event::mouse(&mut q, ev(MouseEventKind::Down(MouseButton::Left)));
        nexo_tui::event::mouse(&mut q, ev(MouseEventKind::Up(MouseButton::Left)));
        screen(&mut q, 100, 30);
    }
    let s = screen(&mut q, 100, 30);
    assert!(s.contains("1 step") && s.contains("hit"), "the click opened the whole line:\n{s}");
}

#[test]
fn item3_goal_waiting_and_done_in_quiet() {
    let mut q = app(Detail::Quiet);
    for state in [GoalState::Waiting, GoalState::Done] {
        q.goal = Some(Goal { text: "make the tests pass".into(), round: 0, max: 5, state });
        q.run_command(Command::Goal(GoalArg::Show));
    }
    assert_eq!(notices(&q), ["goal set · waiting for your answer", "goal met"]);
}
