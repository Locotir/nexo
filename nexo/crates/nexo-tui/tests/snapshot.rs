//! The whole layout drawn on a test backend: what is where, not its colours.

use std::path::PathBuf;

use nexo_core::agent::{AgentEvent, Level, TurnEnd};
use nexo_core::message::Message;
use nexo_core::permission::PermissionRequest;
use nexo_core::stream::Usage;
use nexo_core::tool::{TodoItem, TodoStatus, ToolDisplay, ToolKind, ToolOutput};
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::Terminal;
use tokio::sync::mpsc;

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
        group_tools: false,
        complete_words: true,
        suggest: false,
        hints: true,
        keys: Default::default(),
        notify_after: 15,
        // Pop-ins depend on the clock; snapshots check the settled layout.
        animations: false,
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::Galaxy,
        home_stars: true,
        bursts: true,
        todo_strip: false,
        status_line: vec![],
        status_command: String::new(),
        vim: false,
        prompt: None,
        session_id: Some("20261027-120000-ab12".into()),
        sessions: vec![],
        skills: vec![],
    };
    let mut a = App::new(opts, tx);
    // the tests may run as root; the bar under test is a normal user's
    a.root = false;
    a.handle_agent(AgentEvent::Ready {
        session_id: "20261027-120000-ab12".into(),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        cwd: PathBuf::from("/tmp/project"),
        permission_mode: nexo_core::permission::PermissionMode::Ask,
    });
    a
}

fn fill(a: &mut App) {
    // a long conversation: the banner has scrolled away, the rows count from the first message
    a.drop_banner();
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("fix the failing test") });
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::ReasoningDelta { text: "the test expects a sorted list".into() });
    a.handle_agent(AgentEvent::TextDelta {
        text: "I will **read** the test first.\n\n```rust\nfn main() {}\n```\n".into(),
    });
    a.handle_agent(AgentEvent::ToolCall {
        id: "c1".into(),
        name: "read".into(),
        args: serde_json::json!({"path": "tests/a.rs"}),
        summary: "tests/a.rs".into(),
    });
    a.handle_agent(AgentEvent::ToolStart { id: "c1".into() });
    a.handle_agent(AgentEvent::ToolDone {
        id: "c1".into(),
        output: ToolOutput::ok("fn a() {}\nfn b() {}").with_display(ToolDisplay::File {
            path: "tests/a.rs".into(),
            text: "fn a() {}\nfn b() {}".into(),
            start_line: 1,
        }),
        elapsed_ms: 12,
    });
    a.handle_agent(AgentEvent::ToolCall {
        id: "c2".into(),
        name: "edit".into(),
        args: serde_json::json!({"path": "src/lib.rs"}),
        summary: "src/lib.rs".into(),
    });
    a.handle_agent(AgentEvent::ToolStart { id: "c2".into() });
    a.handle_agent(AgentEvent::ToolDone {
        id: "c2".into(),
        output: ToolOutput::ok("edited").with_display(ToolDisplay::Diff {
            path: "src/lib.rs".into(),
            old: "let x = 1;\n".into(),
            new: "let x = 2;\n".into(),
            start_line: 1,
        }),
        elapsed_ms: 3,
    });
    a.handle_agent(AgentEvent::AssistantDone { message: Message::assistant("I will **read** the test first.") });
    a.handle_agent(AgentEvent::ModelLimits {
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        pricing: None,
        context_window: Some(9000),
        max_output: None,
        source: "config".into(),
    });
    a.handle_agent(AgentEvent::Usage {
        turn: Usage { input: 1200, output: 340, cache_read: 5000, ..Default::default() },
        call: Usage { input: 1200, output: 340, cache_read: 5000, ..Default::default() },
        session: Usage { input: 1200, output: 340, cache_read: 5000, ..Default::default() },
        cost_usd: 0.0031,
    });
    a.handle_agent(AgentEvent::Context {
        view_tokens: 6540,
        history_tokens: 9000,
        history_bytes: 36_000,
        view_bytes: 26_160,
        messages_in_view: 3,
        messages_total: 4,
        policy: "curator".into(),
        note: None,
        label: "curator +1 in".into(),
        detail: None,
    });
    a.handle_agent(AgentEvent::Notice { level: Level::Info, text: "compacted 2 messages".into(), problem: None });
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
}

fn draw(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer())
}

#[test]
fn the_layout_at_100_by_30() {
    let mut a = app();
    fill(&mut a);
    let rows = draw(&mut a, 100, 30);
    let all = rows.join("\n");
    // at 100 columns the directory is the first piece to go (the memory is kept)
    assert!(
        rows[rows.len() - 1].starts_with(" nexo │ deepseek/deepseek-chat │ ask │ ━━━━━──━ 69% 6.2k/9.0k curator"),
        "{}",
        rows[rows.len() - 1]
    );
    assert!(!rows[rows.len() - 1].contains('…'), "pieces are dropped, never cut: {}", rows[rows.len() - 1]);
    a.root = true;
    let as_root = draw(&mut a, 100, 30);
    assert!(
        as_root[as_root.len() - 1].starts_with(" nexo # root │ deepseek/deepseek-chat │ ask │"),
        "{}",
        as_root[as_root.len() - 1]
    );
    let narrow_root = draw(&mut a, 40, 30);
    assert!(
        narrow_root[narrow_root.len() - 1].starts_with(" nexo # root │"),
        "root never drops out: {}",
        narrow_root[narrow_root.len() - 1]
    );
    a.root = false;
    a.handle_agent(nexo_core::agent::AgentEvent::UpdateAvailable { behind: 3 });
    let upd = draw(&mut a, 100, 30);
    assert!(
        upd[upd.len() - 1].starts_with(" nexo  ⬆ update  │ deepseek/deepseek-chat │ ask │"),
        "the update badge sits by the brand: {}",
        upd[upd.len() - 1]
    );
    assert!(upd.join("\n").contains("╭─ ⬆ Update ready"), "{}", upd.join("\n"));
    assert!(upd.iter().all(|r| r.chars().count() <= 100));
    a.update_behind = None;
    let wide = draw(&mut a, 170, 30);
    assert!(
        wide[wide.len() - 1].contains("│ /tmp/project │"),
        "wide enough for the directory: {}",
        wide[wide.len() - 1]
    );
    assert!(wide[wide.len() - 1].contains("81% cached · 1 call"), "{}", wide[wide.len() - 1]);
    let mid = draw(&mut a, 150, 30);
    let mid = &mid[mid.len() - 1];
    assert!(
        mid.contains("81% cached") && !mid.contains("/tmp/project"),
        "the directory goes before the cache share: {mid}"
    );
    assert!(
        wide[wide.len() - 1].contains("━━━━━──━ 69% 6.2k/9.0k curator +1 in │ session 9.0k tok"),
        "{}",
        wide[wide.len() - 1]
    );
    let narrow = draw(&mut a, 60, 20);
    assert!(
        narrow[narrow.len() - 1].starts_with(" nexo │ ")
            && narrow[narrow.len() - 1].contains("chat │ ask │ ━━━━━──━ 69%"),
        "model, mode and gauge stay: {}",
        narrow[narrow.len() - 1]
    );
    assert!(
        !narrow[narrow.len() - 1].contains("session "),
        "the memory goes before the gauge: {}",
        narrow[narrow.len() - 1]
    );
    assert!(rows[rows.len() - 1].contains("$0.0031"));
    assert!(all.contains("▌ fix the failing test"));
    assert!(all.contains("▸ thinking · 1 line"));
    assert!(all.contains("◆ I will read the test first."));
    assert!(all.contains("─ rust"));
    assert!(all.contains("fn main() {}"));
    assert!(all.contains("✓ read tests/a.rs · 2 lines"), "{all}");
    assert!(all.contains("✓ read tests/a.rs · 2 lines  12ms ▸"), "a finished read folds to its head: {all}");
    assert!(!all.contains("│ 1│ fn a() {}"), "{all}");
    assert!(all.contains("✓ edit src/lib.rs · +1 −1"));
    assert!(all.contains("- let x = 1;"));
    assert!(all.contains("+ let x = 2;"));
    assert!(all.contains("◇ compacted 2 messages"));
    assert!(all.contains("1 step   ↑ 6.2k  ↓  340  hit 81%  $0.0031  1 read 1 edit"), "{all}");
    assert!(rows[27].starts_with("›   ask anything"), "an empty editor says what it takes: {}", rows[27]);
    assert!(rows[rows.len() - 2].contains("Enter send"), "{}", rows[rows.len() - 2]);
    assert!(
        !rows[rows.len() - 2].contains("…ab12") && !rows[rows.len() - 2].trim_end().ends_with('·'),
        "the id gives way whole to the keys: {}",
        rows[rows.len() - 2]
    );
}

#[test]
fn a_selected_block_has_a_bar_down_its_margin_in_every_theme() {
    let mut a = app();
    fill(&mut a);
    let rows = draw(&mut a, 100, 30);
    // the steps of the turn sit under the reply, the result under its call
    let at = |rows: &[String], s: &str| rows.iter().position(|r| r.contains(s));
    let read = at(&rows, "   ✓ read tests/a.rs").expect("read card indented");
    assert!(rows[read + 1].starts_with("   ✓ edit src/lib.rs"), "one-line steps read as a list: {rows:#?}");
    assert!(rows.iter().any(|r| r == "   └ 1 + let x = 2;"), "{rows:#?}");
    a.select_last();
    for theme in ["nexo", "mono"] {
        a.set_theme(theme, false);
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        draw_frame(&mut t, &mut a).unwrap();
        let rows = rows_of(t.backend().buffer());
        let head = at(&rows, "✓ edit src/lib.rs").unwrap();
        assert!(rows[head].starts_with("┃  ✓ edit"), "{theme}: {}", rows[head]);
        assert!(rows[head + 1].starts_with("┃  │ @@"), "{theme}: {}", rows[head + 1]);
        assert!(rows[head + 3].starts_with("┃  └ 1 + let x = 2;"), "{theme}: {}", rows[head + 3]);
        assert!(!rows[head + 4].starts_with('┃'), "the bar is the block's height: {}", rows[head + 4]);
        let cell = &t.backend().buffer()[(6, head as u16)];
        if theme == "mono" {
            assert!(cell.modifier.contains(ratatui::style::Modifier::REVERSED), "{theme}");
        } else {
            assert_eq!(cell.bg, a.theme.selection_bg, "{theme}");
        }
        assert!(rows[rows.len() - 2].contains("↑↓ move · → open · ← fold"), "{}", rows[rows.len() - 2]);
    }
}

#[test]
fn modals_and_palette_draw_on_top() {
    let mut a = app();
    fill(&mut a);
    a.handle_agent(AgentEvent::PermissionAsk {
        request: PermissionRequest {
            agent: None,
            id: "p1".into(),
            tool: "bash".into(),
            kind: ToolKind::Exec,
            summary: "cargo test".into(),
            detail: ToolDisplay::Shell {
                command: "cargo test".into(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
            },
            suggested_rules: vec!["bash(cargo *)".into()],
        },
    });
    let rows = draw(&mut a, 100, 30);
    let all = rows.join("\n");
    assert!(all.contains(" permission · bash "));
    assert!(all.contains("$ cargo test"));
    assert!(all.contains("1  bash(cargo *)"));
    assert!(all.contains("[y] allow"));
    assert!(rows[rows.len() - 2].contains("y/Enter allow · a always · n/Esc deny"));
    a.modal = None;
    a.input.set_text("/mo");
    a.refresh_palette();
    let rows = draw(&mut a, 100, 30);
    let all = rows.join("\n");
    assert!(all.contains(" /model"));
    assert!(all.contains("set one up"));
    assert!(rows[27].contains("› /mo"));
    // an alias: the command it stands for comes first, the alias shown dim beside it
    a.input.set_text("/ses");
    a.refresh_palette();
    let rows = draw(&mut a, 100, 30);
    let row = rows.iter().find(|r| r.contains("/resume")).cloned().unwrap_or_default();
    assert!(row.contains("/resume  (sessions)"), "{row}");
    assert!(row.contains("pick a session to resume"), "{row}");
    assert!(!rows.join("\n").contains("/sessions "), "sessions is not a row of its own");
}

#[test]
fn the_ground_is_painted_with_the_themes_colours_everywhere() {
    let mut a = app();
    fill(&mut a);
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    draw_frame(&mut t, &mut a).unwrap();
    let buf = t.backend().buffer();
    let bg = a.theme.bg;
    // RGB on a 24-bit terminal, an index of the cube elsewhere (the test environment may be either); never the terminal's own
    assert!(matches!(bg, Color::Rgb(..) | Color::Indexed(_)), "{bg:?}");
    // an empty cell of the transcript, the editor's empty tail and a cell past a short line
    for (x, y) in [(99, 10), (60, 27), (90, 3)] {
        assert_eq!(buf[(x, y)].bg, bg, "cell {x},{y} keeps the terminal's colour");
    }
    assert_eq!(buf[(5, 28)].bg, a.theme.bar_bg, "the hints bar has its own band");
    assert_eq!(buf[(5, 29)].bg, a.theme.bar_bg);
    a.toggle_files(120);
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    draw_frame(&mut t, &mut a).unwrap();
    assert_eq!(t.backend().buffer()[(115, 20)].bg, bg, "the panel's empty cells too");
    a.input.set_text("/mo");
    a.refresh_palette();
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    draw_frame(&mut t, &mut a).unwrap();
    let rows = rows_of(t.backend().buffer());
    let y = rows.iter().position(|r| r.contains(" /model")).expect("a palette row") as u16;
    let palette_bg = t.backend().buffer()[(70, y)].bg;
    assert!(palette_bg == a.theme.code_bg || palette_bg == a.theme.selection_bg, "{palette_bg:?}");
}

#[test]
fn the_panel_shows_the_goal_and_the_todo_list_above_the_files() {
    let mut a = app();
    fill(&mut a);
    a.handle_agent(AgentEvent::Goal {
        text: Some("pass the weight tests".into()),
        round: 1,
        max: 3,
        state: Default::default(),
    });
    a.handle_agent(AgentEvent::ToolCall {
        id: "t1".into(),
        name: "todo".into(),
        args: serde_json::json!({}),
        summary: "3 tasks".into(),
    });
    a.handle_agent(AgentEvent::ToolDone {
        id: "t1".into(),
        output: ToolOutput::ok("3 tasks").with_display(ToolDisplay::Todo {
            items: vec![
                TodoItem { text: "read the test".into(), status: TodoStatus::Done },
                TodoItem { text: "fix the conversion".into(), status: TodoStatus::InProgress },
                TodoItem { text: "run the suite".into(), status: TodoStatus::Pending },
            ],
        }),
        elapsed_ms: 1,
    });
    a.toggle_files(120);
    let rows = draw(&mut a, 120, 30);
    let all = rows.join("\n");
    assert!(rows[rows.len() - 1].contains("ask │ ◎ goal"), "{}", rows[rows.len() - 1]);
    assert!(all.contains("│ goal"), "{all}");
    assert!(all.contains("◎ pass the weight tests"), "{all}");
    assert!(all.contains("round 1/3"), "{all}");
    assert!(all.contains("│ ≡ todo") && all.contains("1/3"), "{all}");
    assert!(all.contains("● read the test") && all.contains("◐ fix the conversion") && all.contains("○ run the suite"));
    assert!(all.contains("│ changed files (1)"), "{all}");
    let goal_row = rows.iter().position(|r| r.contains("│ goal")).unwrap();
    let todo_row = rows.iter().position(|r| r.contains("≡ todo")).unwrap();
    let files_row = rows.iter().position(|r| r.contains("│ changed files")).unwrap();
    assert!(goal_row < todo_row && todo_row < files_row);
    // the panel lists the tasks already, so the strip above the draft stays out
    assert!(all.contains("✓ todo 3 items · 1/3 done") && !all.contains("to-do 1/3"), "the card, no strip: {all}");
    assert!(
        !rows[rows.len() - 2].contains("todo 1/3"),
        "the status bar does not repeat the count: {}",
        rows[rows.len() - 2]
    );
}

#[test]
fn the_banner_shows_on_an_empty_session_and_the_model_card_after_a_question() {
    let mut a = app();
    let rows = draw(&mut a, 100, 30);
    let all = rows.join("\n");
    assert!(all.contains("╭──") && all.contains("nexo "), "{all}");
    assert!(all.contains("deepseek/deepseek-chat · ask") && all.contains("●─╮"), "{all}");
    assert!(all.contains("/resume picks a session"), "{all}");
    assert!(
        rows[rows.len() - 1].contains(" ~0%"),
        "the gauge is there before the window is known: {}",
        rows[rows.len() - 1]
    );
    a.handle_agent(AgentEvent::ModelLimits {
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        pricing: Some(nexo_core::pricing::Pricing { input: 0.28, output: 0.42, cache_read: 0.028, cache_write: 0.0 }),
        context_window: Some(128_000),
        max_output: Some(8192),
        source: "catalogue".into(),
    });
    let rows = draw(&mut a, 100, 30);
    assert!(rows.join("\n").contains("╭──"), "limits at start keep the banner");
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("hi") });
    a.handle_agent(AgentEvent::ModelLimits {
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        pricing: Some(nexo_core::pricing::Pricing { input: 0.28, output: 0.42, cache_read: 0.028, cache_write: 0.0 }),
        context_window: Some(128_000),
        max_output: Some(8192),
        source: "catalogue".into(),
    });
    let rows = draw(&mut a, 120, 30);
    let all = rows.join("\n");
    assert!(all.contains("╭──"), "the banner stays on top of the first message");
    assert!(
        all.contains("◈ model deepseek/deepseek-chat · 128k context · 8.2k max reply · $0.28 in · $0.42 out · $0.028 cached · from catalogue"),
        "{all}"
    );
    assert_eq!(nexo_tui::ui::gauge_cells(0.42, 8), ("███▍".to_string(), 4));
}

#[test]
fn tiny_terminals_do_not_panic() {
    let mut a = app();
    fill(&mut a);
    for (w, h) in [(20, 5), (8, 3), (1, 1), (120, 2), (40, 12)] {
        let _ = draw(&mut a, w, h);
    }
    a.input.set_text("/");
    a.refresh_palette();
    let _ = draw(&mut a, 20, 5);
    a.run_command(nexo_tui::commands::Command::Help);
    let _ = draw(&mut a, 20, 5);
}

#[test]
fn a_long_transcript_scrolls_and_shows_what_arrived() {
    let mut a = app();
    for i in 0..400 {
        a.handle_agent(AgentEvent::History { index: i, message: Message::user(format!("line {i}")) });
    }
    let rows = draw(&mut a, 60, 20);
    assert!(rows.iter().any(|r| r.contains("▌ line 399")), "follows the end");
    a.scroll_by(-30);
    a.handle_agent(AgentEvent::History { index: 400, message: Message::user("late one") });
    let rows = draw(&mut a, 60, 20);
    let all = rows.join("\n");
    assert!(all.contains("↓ 1 new"), "one new message, not its rows: {all}");
    a.open_search();
    a.search.as_mut().unwrap().query = "line 12".into();
    a.rematch();
    let rows = draw(&mut a, 60, 20);
    assert!(rows.iter().any(|r| r.contains("search: line 12")));
    assert!(rows.iter().any(|r| r.contains("1/11")));
}

#[test]
fn the_files_panel_and_a_diff_card() {
    let mut a = app();
    fill(&mut a);
    a.toggle_files(120);
    let rows = draw(&mut a, 120, 30);
    let all = rows.join("\n");
    assert!(all.contains("changed files (1)"), "{all}");
    assert!(rows.iter().any(|r| r.contains("src/lib.rs") && r.contains("+1 −1")), "{all}");
    assert!(rows.iter().any(|r| r.contains("│ changed files")), "the panel has a border");
    a.toggle_files(120);
    let rows = draw(&mut a, 120, 30);
    assert!(!rows.join("\n").contains("changed files"));
    a.handle_agent(AgentEvent::ShellOutput {
        command: "git --no-pager diff".into(),
        output: "diff --git a/x b/x\n@@ -1 +1 @@\n-old\n+new\n".into(),
        exit_code: Some(0),
    });
    let rows = draw(&mut a, 120, 30);
    let all = rows.join("\n");
    assert!(all.contains("$ git --no-pager diff") && all.contains("+new") && all.contains("-old"), "{all}");
    a.handle_agent(AgentEvent::TurnStart { turn: 2 });
    a.handle_agent(AgentEvent::Queued { pending: 2 });
    let rows = draw(&mut a, 120, 30);
    assert!(rows[rows.len() - 2].contains("2 queued"), "{}", rows[0]);
    // while busy, the line above the editor says what the agent does
    assert!(
        rows[26].starts_with("─ ") && rows[26].contains("Thinking…") && rows[26].contains("Esc interrupt"),
        "{}",
        rows[26]
    );
    assert!(rows[27].contains("joins the turn at its next step"), "{}", rows[27]);
    let rows = draw(&mut a, 50, 12);
    assert!(!rows[rows.len() - 1].contains("/tmp/project"), "narrow bars drop the directory: {}", rows[rows.len() - 1]);
}

#[test]
fn the_context_map_shows_what_the_call_sends() {
    use nexo_core::{CellKind, CellState, ContextCell};
    let mut a = app();
    fill(&mut a);
    let cell = |kind, tokens, state| ContextCell { kind, tokens, state, zone: 0 };
    a.handle_agent(AgentEvent::ContextMap {
        cells: vec![
            cell(CellKind::Request, 40, CellState::Dropped),
            cell(CellKind::Call, 30, CellState::Whole),
            cell(CellKind::Output, 900, CellState::Shrunk),
            cell(CellKind::Request, 20, CellState::Whole),
        ],
        zones: Vec::new(),
        calls: Vec::new(),
        call_tokens: Vec::new(),
    });
    a.toggle_live(120);
    assert!(a.show_live);
    let rows = draw(&mut a, 120, 30);
    let panel: Vec<&str> = rows.iter().map(|r| r.split('│').next_back().unwrap_or("")).collect();
    let all = panel.join("\n");
    assert!(all.contains("context live"), "{all}");
    assert!(all.contains("sent 950 of 990"), "{all}");
    assert!(all.contains("·█▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒█"), "{all}");
    assert!(all.contains("1 cell ≈ 50 tok · curator"), "{all}");
    // narrow: the toggle refuses and says why
    let mut b = app();
    b.toggle_live(80);
    assert!(!b.show_live);
}

#[test]
fn the_context_map_shows_the_curators_memory_by_topic() {
    use nexo_core::{CellKind, CellState, ContextCell, ContextZone};
    let mut a = app();
    fill(&mut a);
    let cell = |kind, tokens, state, zone| ContextCell { kind, tokens, state, zone };
    let zone = |label: &str, active, card| ContextZone { label: label.into(), active, card };
    a.handle_agent(AgentEvent::ContextMap {
        cells: vec![
            cell(CellKind::Request, 50, CellState::Whole, 0),
            cell(CellKind::Output, 500, CellState::Dropped, 1),
            cell(CellKind::Output, 300, CellState::Dropped, 2),
            cell(CellKind::Output, 100, CellState::Recalled, 2),
            cell(CellKind::Request, 50, CellState::Whole, 3),
            cell(CellKind::Output, 200, CellState::Shrunk, 3),
        ],
        zones: vec![
            zone("anchor", true, false),
            zone("fix the parser", false, true),
            zone("old deploy notes", false, false),
            zone("write the docs", true, false),
        ],
        calls: Vec::new(),
        call_tokens: Vec::new(),
    });
    a.toggle_live(120);
    let rows = draw(&mut a, 120, 40);
    let panel: Vec<&str> = rows.iter().map(|r| r.split('│').next_back().unwrap_or("").trim()).collect();
    let all = panel.join("\n");
    let at = |s: &str| panel.iter().position(|l| l.contains(s)).unwrap_or_else(|| panic!("{s} not in\n{all}"));
    // anchor, the topic in progress, then the others newest first
    assert!(at("◆ anchor") < at("● write the docs") && at("● write the docs") < at("◐ old"), "{all}");
    assert!(at("◐ old") < at("▭ fix the parser"), "{all}");
    assert!(panel[at("● write the docs")].contains("250/250 now"), "{all}");
    assert!(panel[at("▭ fix the parser")].contains("0/500 card"), "{all}");
    assert!(panel[at("● write the docs") + 1].starts_with("█▒▒▒▒"), "{all}");
    assert!(panel[at("◐ old") + 1].starts_with("······░░"), "{all}");
    assert!(panel[at("◐ old")].ends_with("100/400 back"), "{all}");
    assert!(all.contains("░ back · out"), "{all}");
}

fn curated_items() -> Vec<nexo_core::CuratedItem> {
    use nexo_core::{CuratedItem, CuratedKind, CuratedState};
    let it = |kind, label: &str, tokens, state| CuratedItem { kind, label: label.into(), tokens, state };
    vec![
        it(CuratedKind::Rule, "never touch main", 300, CuratedState::In),
        it(CuratedKind::Thread, "parser rewrite", 12_400, CuratedState::In),
        it(CuratedKind::Recalled, "src/lex.rs", 900, CuratedState::Trimmed),
        it(CuratedKind::Memory, "old notes", 2_000, CuratedState::Out),
    ]
}

#[test]
fn the_context_window_lists_what_the_curator_added_sorted_with_a_total() {
    let mut a = app();
    fill(&mut a);
    // before any call comes with the curator the section is not there
    a.run_command(nexo_tui::commands::Command::Context);
    settled(&mut a);
    let all = draw(&mut a, 120, 60).join("\n");
    assert!(!all.contains("Added by the curator"), "{all}");
    a.modal = None;
    a.handle_agent(AgentEvent::Curated { items: curated_items() });
    a.run_command(nexo_tui::commands::Command::Context);
    settled(&mut a);
    let rows = draw(&mut a, 120, 60);
    let all = rows.join("\n");
    let at = |s: &str| rows.iter().position(|l| l.contains(s)).unwrap_or_else(|| panic!("{s} not in\n{all}"));
    assert!(all.contains("Added by the curator (last call)"), "{all}");
    assert!(at("thread") < at("recalled") && at("recalled") < at("rule") && at("rule") < at("total"), "{all}");
    assert!(rows[at("thread")].contains("parser rewrite") && rows[at("thread")].contains("12.4k"), "{all}");
    assert!(rows[at("recalled")].contains("~900"), "{all}");
    assert!(rows[at("total")].contains("13.6k"), "{all}");
    assert!(at("Left out") > at("total") && rows[at("Left out") + 1].contains("old notes"), "{all}");
    // a call with nothing from the curator says so
    a.modal = None;
    a.handle_agent(AgentEvent::Curated { items: Vec::new() });
    a.run_command(nexo_tui::commands::Command::Context);
    settled(&mut a);
    let all = draw(&mut a, 120, 60).join("\n");
    assert!(all.contains("The curator added nothing to this call."), "{all}");
}

#[test]
fn f3_lists_the_curator_rows_of_a_call_in_the_live_panel() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut a = app();
    fill(&mut a);
    a.term_width = 120;
    let f3 = |a: &mut App| nexo_tui::event::key(a, KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));
    // nothing yet: a hint, no panel
    f3(&mut a);
    assert!(!a.show_live && a.live_rows.is_none());
    a.handle_agent(AgentEvent::Curated { items: curated_items() });
    a.handle_agent(AgentEvent::Curated { items: Vec::new() });
    f3(&mut a);
    assert!(a.show_live);
    assert_eq!(a.live_rows, Some(0));
    let all = draw(&mut a, 120, 40).join("\n");
    assert!(all.contains("curator rows") && all.contains("The curator added nothing"), "{all}");
    // again: the call before, with its rows
    f3(&mut a);
    assert_eq!(a.live_rows, Some(1));
    let all = draw(&mut a, 120, 40).join("\n");
    assert!(all.contains("curator rows · 1 back") && all.contains("parser rewrite"), "{all}");
    // after the oldest, the map again
    f3(&mut a);
    assert_eq!(a.live_rows, None);
}

/// The open window as if it had finished growing and filling in.
fn settled(a: &mut App) {
    if let Some(nexo_tui::modal::Modal::Info(i)) = &mut a.modal {
        let past = std::time::Instant::now() - std::time::Duration::from_secs(5);
        i.opened = past;
        i.shown = past;
    }
}

fn mouse(a: &mut App, kind: crossterm::event::MouseEventKind, x: u16, y: u16) {
    let ev = crossterm::event::MouseEvent { kind, column: x, row: y, modifiers: crossterm::event::KeyModifiers::NONE };
    nexo_tui::event::handle(a, crossterm::event::Event::Mouse(ev));
}

#[test]
fn context_and_usage_open_a_window_with_tabs_and_a_close_button() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
    use nexo_tui::modal::Modal;
    let mut a = app();
    fill(&mut a);
    let session = Usage { input: 2000, output: 500, cache_read: 6000, ..Default::default() };
    a.handle_agent(AgentEvent::Usage { turn: session, call: session, session, cost_usd: 0.0123 });
    a.run_command(nexo_tui::commands::Command::Context);
    settled(&mut a);
    let rows = draw(&mut a, 120, 40);
    let all = rows.join("\n");
    assert!(all.contains("[✗]"), "{all}");
    assert!(all.contains(" 1 Context ") && all.contains(" 2 Usage ") && all.contains(" 3 Session "), "{all}");
    assert!(all.contains("◆ in use") && all.contains("◇ free"), "{all}");
    assert!(all.contains("8.0k / 9.0k tokens"), "{all}");
    assert!(all.contains("Esc close"), "{all}");
    // the second tab: cost, bars, the calls
    nexo_tui::event::key(&mut a, KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
    settled(&mut a);
    let all = draw(&mut a, 120, 40).join("\n");
    assert!(all.contains("$0.0123") && all.contains("from cache") && all.contains("cache hit  75%"), "{all}");
    assert!(all.contains("sent per call · 2 calls") && all.contains('█'), "{all}");
    // the third: who and where
    nexo_tui::event::key(&mut a, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    let all = draw(&mut a, 120, 40).join("\n");
    assert!(all.contains("deepseek-chat") && all.contains("/tmp/project"), "{all}");
    // a tab is clicked; the pointer on [✗] lights it; a click on it closes
    let Some(Modal::Info(i)) = &a.modal else { panic!("the window is open") };
    let (tab1, close) = (i.tabs[0], i.close);
    mouse(&mut a, MouseEventKind::Down(MouseButton::Left), tab1.x + 1, tab1.y);
    assert!(matches!(&a.modal, Some(Modal::Info(i)) if i.tab == 0));
    mouse(&mut a, MouseEventKind::Moved, close.x + 1, close.y);
    assert!(matches!(&a.modal, Some(Modal::Info(i)) if i.hover));
    mouse(&mut a, MouseEventKind::Down(MouseButton::Left), close.x + 1, close.y);
    assert!(a.modal.is_none(), "[✗] closes");
    // /usage opens on its tab; a click outside the window closes it too
    a.run_command(nexo_tui::commands::Command::Cost);
    assert!(matches!(&a.modal, Some(Modal::Info(i)) if i.tab == 1));
    draw(&mut a, 120, 40);
    mouse(&mut a, MouseEventKind::Down(MouseButton::Left), 0, 20);
    assert!(a.modal.is_none(), "a click outside closes");
    // it opens small and grows: the first frame does not panic at any size
    a.run_command(nexo_tui::commands::Command::Context);
    for (w, h) in [(30, 8), (44, 12), (200, 60)] {
        draw(&mut a, w, h);
    }
}

#[test]
fn the_pointer_lights_a_row_of_the_slash_window_and_a_click_takes_it() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let mut a = app();
    fill(&mut a);
    a.input.set_text("/");
    a.refresh_palette();
    a.palette_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(5));
    draw(&mut a, 100, 30);
    // the third row: the pointer lights it without moving the selection
    let (r, k) = a.palette_rows[2];
    mouse(&mut a, MouseEventKind::Moved, r.x + 3, r.y);
    assert_eq!(a.hover, nexo_tui::app::Hover::Palette(k));
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    draw_frame(&mut t, &mut a).unwrap();
    let buf = t.backend().buffer();
    let lit = buf[(r.x + 60, r.y)].bg;
    assert_ne!(lit, buf[(r.x + 60, a.palette_rows[3].0.y)].bg, "the row under the pointer stands out");
    assert_eq!(a.palette.as_ref().unwrap().selected, 0);
    // section titles are not rows: no title line is clickable
    let rows = rows_of(buf);
    for (rect, _) in &a.palette_rows {
        assert!(rows[rect.y as usize].contains(" /"), "{}", rows[rect.y as usize]);
    }
    // a click on /theme runs it: the theme picker opens
    let theme = a.palette_rows.iter().find(|(rr, _)| rows[rr.y as usize].contains(" /theme ")).map(|x| x.0);
    let theme = theme.unwrap_or_else(|| {
        // further down the list: type to bring it up
        a.input.set_text("/them");
        a.refresh_palette();
        draw(&mut a, 100, 30);
        a.palette_rows[0].0
    });
    mouse(&mut a, MouseEventKind::Down(MouseButton::Left), theme.x + 3, theme.y);
    assert!(a.palette.is_none(), "the window closes");
    assert!(a.modal.is_some(), "and /theme opens its picker");
}

#[test]
fn a_tool_card_says_where_its_output_stands() {
    use nexo_core::CallMark;
    let mut a = app();
    fill(&mut a);
    a.handle_agent(AgentEvent::ContextMap {
        cells: Vec::new(),
        zones: Vec::new(),
        calls: vec![("c1".into(), CallMark::Cached), ("c2".into(), CallMark::Out)],
        call_tokens: Vec::new(),
    });
    let rows = draw(&mut a, 120, 30);
    let row = |s: &str| rows.iter().find(|r| r.contains(s)).unwrap_or_else(|| panic!("{s}:\n{}", rows.join("\n")));
    // the word sits at the right of the head, the head itself unchanged
    assert!(row("tests/a.rs").trim_end().ends_with("● cached"), "{}", rows.join("\n"));
    // set aside, the usual state of an older card, is the glyph alone
    let aside = row("src/lib.rs").trim_end().to_string();
    assert!(aside.ends_with(" ○") && !aside.contains("set aside"), "{}", rows.join("\n"));
    // a new call moves the words: the card is drawn again
    a.handle_agent(AgentEvent::ContextMap {
        cells: Vec::new(),
        zones: Vec::new(),
        calls: vec![("c1".into(), CallMark::Back)],
        call_tokens: Vec::new(),
    });
    let rows = draw(&mut a, 120, 30);
    let row = |s: &str| rows.iter().find(|r| r.contains(s)).unwrap().trim_end().to_string();
    assert!(row("tests/a.rs").ends_with("↺ recalled"), "{}", rows.join("\n"));
    assert!(!row("src/lib.rs").ends_with("○"), "{}", rows.join("\n"));
    // narrow, the head keeps its room and the word goes
    let rows = draw(&mut a, 30, 30);
    assert!(!rows.join("\n").contains("brought"), "{}", rows.join("\n"));
}

#[test]
fn a_tool_card_adds_the_price_of_its_output_only_with_full_prices_and_room() {
    use nexo_core::pricing::Pricing;
    use nexo_core::CallMark;
    let limits = |pricing| AgentEvent::ModelLimits {
        provider: "p".into(),
        model: "m".into(),
        pricing,
        context_window: Some(128_000),
        max_output: Some(8192),
        source: "config".into(),
    };
    let map = || AgentEvent::ContextMap {
        cells: Vec::new(),
        zones: Vec::new(),
        calls: vec![("c1".into(), CallMark::Cached), ("c2".into(), CallMark::Sent)],
        call_tokens: vec![("c1".into(), 5000), ("c2".into(), 1000)],
    };
    let full = Pricing { input: 3.0, output: 15.0, cache_read: 0.3, cache_write: 0.0 };
    let text = |a: &mut App, w| draw(a, w, 30).join("\n");
    let mut a = app();
    fill(&mut a);
    a.handle_agent(limits(Some(full)));
    a.handle_agent(map());
    // 5000 tokens at the cached rate, 1000 at the full one
    let wide = text(&mut a, 120);
    assert!(wide.contains("● cached 0.00150 $"), "{wide}");
    assert!(wide.contains("● full price 0.00300 $"), "{wide}");
    // narrow: the command is cut with an ellipsis, the word and the price stay
    let narrow = text(&mut a, 50);
    assert!(narrow.contains("● cached 0.00150 $") && narrow.contains("● full price 0.00300 $"), "{narrow}");
    // too narrow for the price: the word alone
    let tight = text(&mut a, 34);
    assert!(tight.contains("● cached") && !tight.contains("0.00150"), "{tight}");
    // a price missing (no cached rate): nothing extra, the word stays
    a.handle_agent(limits(Some(Pricing { cache_read: 0.0, ..full })));
    let part = text(&mut a, 120);
    assert!(part.contains("● cached") && !part.contains("0.00150"), "{part}");
    a.handle_agent(limits(None));
    let none = text(&mut a, 120);
    assert!(none.contains("● full price") && !none.contains("0.00300"), "{none}");
}

#[test]
fn the_keys_row_goes_blank_while_a_draft_is_typed_and_nothing_moves() {
    let mut a = app();
    let rows = draw(&mut a, 100, 30);
    assert!(rows[28].contains("Enter send"), "an empty draft shows the keys: {}", rows[28]);
    let before = rows[..27].to_vec();
    a.input.set_text("hello");
    let rows = draw(&mut a, 100, 30);
    assert!(!rows.join("\n").contains("Enter send"), "typing hides them");
    // the row keeps its line: the draft and the text above stay where they were
    assert!(rows[27].starts_with("› hello"), "the draft stays put: {}", rows[27]);
    assert!(rows[28].trim().is_empty(), "their line is blank: {}", rows[28]);
    assert_eq!(rows[..27], before[..], "the text above does not move");
    // a list that is open keeps its keys
    a.input.set_text("/mo");
    a.refresh_palette();
    let rows = draw(&mut a, 100, 30);
    assert!(rows[28].contains("↑↓ choose"), "{}", rows[28]);
    // and the draft cleared brings them back
    a.input.set_text("");
    a.refresh_palette();
    let rows = draw(&mut a, 100, 30);
    assert!(rows[28].contains("Enter send"), "{}", rows[28]);
}

#[test]
fn the_jobs_window_lists_running_then_ended_jobs_with_their_output_and_opens_one() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
    use nexo_core::jobs::{JobInfo, JobKind, JobStatus, JobTail};
    use nexo_tui::modal::Modal;
    let job = |id, label: &str, status, elapsed_ms| JobInfo {
        id,
        kind: JobKind::Shell,
        label: label.into(),
        status,
        elapsed_ms,
        lines: 3,
    };
    let mut a = app();
    fill(&mut a);
    a.handle_agent(AgentEvent::Jobs {
        jobs: vec![
            job(1, "cargo test", JobStatus::Done { ok: false, how: "exit 101".into() }, 42_000),
            job(2, "npm run dev", JobStatus::Running, 125_000),
            job(3, "make", JobStatus::Done { ok: true, how: "exit 0".into() }, 3_000),
        ],
    });
    // a click on the indicator opens the window
    let all = draw(&mut a, 120, 30);
    let (row, text) = all.iter().enumerate().find(|(_, r)| r.contains("1 job")).expect("the indicator shows");
    let col = text.chars().position(|c| c == '⚙').unwrap() as u16;
    mouse(&mut a, MouseEventKind::Down(MouseButton::Left), col + 1, row as u16);
    assert!(matches!(&a.modal, Some(Modal::Jobs(_))));
    a.handle_agent(AgentEvent::JobTails {
        tails: vec![
            JobTail {
                id: 1,
                ended_ago_ms: Some(90_000),
                text: "running 2 tests\ntest a ... ok\ntest b ... FAILED".into(),
            },
            JobTail { id: 2, ended_ago_ms: None, text: "ready on :3000".into() },
            JobTail { id: 3, ended_ago_ms: Some(5_000), text: "built".into() },
        ],
    });
    let all = draw(&mut a, 120, 30).join("\n");
    assert!(all.contains("background jobs") && all.contains("1 running"), "{all}");
    let (run, ended, one) = (
        all.find("npm run dev").expect("running"),
        all.find("ended").expect("a heading for the ended ones"),
        all.find("cargo test").expect("ended"),
    );
    assert!(run < ended && ended < one, "running first, then the ended:\n{all}");
    assert!(all.contains("running 2 min 5 s") && all.contains("ready on :3000"), "{all}");
    assert!(
        all.contains("done (exit 0) after 3 s, 5 s ago") && all.contains("done (exit 101) after 42 s, 1 min 30 s ago"),
        "{all}"
    );
    assert!(all.contains("test b ... FAILED") && !all.contains("running 2 tests"), "the last lines only:\n{all}");
    // Down, Enter: the second row (the newest ended job, 3) opens with its output
    nexo_tui::event::key(&mut a, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    nexo_tui::event::key(&mut a, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let all = draw(&mut a, 120, 30).join("\n");
    assert!(all.contains("job 3") && all.contains("built") && all.contains("3 lines of output"), "{all}");
    assert!(!all.contains("npm run dev"), "{all}");
    // Esc goes back to the list, Esc again closes
    nexo_tui::event::key(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(draw(&mut a, 120, 30).join("\n").contains("npm run dev"));
    nexo_tui::event::key(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(a.modal.is_none());
    // a click on a row opens that job
    a.run_command(nexo_tui::commands::Command::Jobs(None));
    draw(&mut a, 120, 30);
    let r = a.picker_rows.iter().find(|(_, i)| *i == 2).map(|(r, _)| *r).expect("rows are clickable");
    mouse(&mut a, MouseEventKind::Down(MouseButton::Left), r.x + 2, r.y);
    assert!(matches!(&a.modal, Some(Modal::Jobs(v)) if v.detail && v.current().map(|j| j.id) == Some(1)));
    // small screens do not panic
    for (w, h) in [(30, 8), (44, 12), (200, 60)] {
        draw(&mut a, w, h);
    }
}

#[test]
fn the_permission_dialog_at_80x24_is_drawn_over_clear_ground_not_over_the_welcome_box() {
    let mut a = app();
    a.handle_agent(AgentEvent::PermissionAsk {
        request: PermissionRequest {
            agent: None,
            id: "p1".into(),
            tool: "bash".into(),
            kind: ToolKind::Exec,
            summary: "cargo test".into(),
            detail: ToolDisplay::Shell {
                command: "cargo test".into(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
            },
            suggested_rules: vec!["bash(cargo *)".into()],
        },
    });
    let rows = draw(&mut a, 80, 24);
    let all = rows.join("\n");
    assert!(all.contains(" permission · bash ") && all.contains("[y] allow"), "{all}");
    // nothing of the welcome box (its title and corner marks) shows around the dialog
    assert!(!all.contains("nexo ╭") && !all.contains("/resume picks a session"), "{all}");
    let top = rows.iter().position(|r| r.contains(" permission · bash ")).unwrap();
    for r in &rows[..top] {
        assert!(r.trim().is_empty(), "welcome box left above the dialog: {r:?}");
    }
    for r in &rows[top..top + 9] {
        let inner: String = r.chars().skip(1).take(78).collect();
        assert!(!inner.contains('●'), "welcome box cell beside the dialog: {r:?}");
    }
}

#[test]
fn the_permission_title_of_a_sub_agent_fits_a_60_column_screen_and_the_main_agents_is_unchanged() {
    let ask = |agent: Option<&str>| PermissionRequest {
        agent: agent.map(str::to_string),
        id: "p1".into(),
        tool: "bash".into(),
        kind: ToolKind::Exec,
        summary: "cargo test".into(),
        detail: ToolDisplay::Shell {
            command: "cargo test".into(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
        },
        suggested_rules: vec![],
    };
    // the main agent: the title is what it always was
    let mut a = app();
    a.handle_agent(AgentEvent::PermissionAsk { request: ask(None) });
    let rows = draw(&mut a, 60, 24);
    assert!(rows.iter().any(|r| r.contains("╭ permission · bash ─")), "{}", rows.join("\n"));
    // a sub-agent with a name far longer than the room: the title is cut with an ellipsis and stays on one line
    let mut a = app();
    let name = "a-sub-agent-with-a-name-that-goes-on-and-on";
    a.handle_agent(AgentEvent::PermissionAsk { request: ask(Some(name)) });
    let rows = draw(&mut a, 60, 24);
    let top = rows.iter().find(|r| r.contains("permission · a-sub")).unwrap_or_else(|| panic!("{}", rows.join("\n")));
    assert!(top.contains("… · bash ") && !top.contains(name), "{top}");
    assert!(top.trim_end().ends_with('╮') && top.trim_end().chars().count() <= 60, "{top}");
}
