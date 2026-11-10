//! The screen at the four sizes people really use: 40x12 (a split pane), 80x24 (the classic terminal),
//! 120x40 and 200x60 (a maximised window). Each scene is drawn at each size and compared with the text
//! kept in `tests/captures/`; a deliberate change is accepted with `NEXO_UPDATE_GOLDEN=1 cargo test`, and the
//! diff of those files is the review. Whatever the size, the same things must hold: the frame has
//! exactly the rows asked for, the input line and the status bar are on screen, and nothing is cut by a
//! box left open.

use std::path::PathBuf;

use nexo_core::agent::{AgentEvent, Level, TurnEnd};
use nexo_core::message::Message;
use nexo_core::permission::PermissionRequest;
use nexo_core::stream::Usage;
use nexo_core::tool::{ToolDisplay, ToolKind, ToolOutput};
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

const SIZES: [(u16, u16); 4] = [(40, 12), (80, 24), (120, 40), (200, 60)];

fn app() -> App {
    // the captures must not depend on the other tools' folders in this machine's home
    std::env::set_var("NEXO_COMPAT_HOME", "");
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
        keys: Default::default(),
        notify_after: 15,
        animations: false,
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
    a.handle_agent(AgentEvent::Ready {
        session_id: "20261102-100000-ef56".into(),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        cwd: PathBuf::from("/tmp/project"),
        permission_mode: nexo_core::permission::PermissionMode::Ask,
    });
    a
}

fn conversation(a: &mut App) {
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("fix the failing test in the parser") });
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::ReasoningDelta { text: "the test expects a sorted list".into() });
    a.handle_agent(AgentEvent::TextDelta {
        text: "I will **read** the test first.\n\n- the input is `3 1 2`\n- the output must be `1 2 3`\n\n```rust\nfn main() {}\n```\n"
            .into(),
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
    a.handle_agent(AgentEvent::AssistantDone { message: Message::assistant("done") });
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
    a.handle_agent(AgentEvent::Notice { level: Level::Info, text: "compacted 2 messages".into() });
    // the wall clock never reaches a capture
    a.turn_started = None;
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    // the turn and thinking times are read from the wall clock: pin them so the captures stay the
    // same on every run (a slow runner once drew the thinking line as 1ms)
    for b in a.blocks.iter_mut() {
        match b {
            nexo_tui::render::blocks::Block::Usage { elapsed_ms, .. } => *elapsed_ms = 0,
            nexo_tui::render::blocks::Block::Reasoning { elapsed_ms, .. } => *elapsed_ms = Some(0),
            _ => {}
        }
    }
}

fn permission(a: &mut App) {
    a.handle_agent(AgentEvent::PermissionAsk {
        request: PermissionRequest {
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
}

fn trust_modal(home: bool) -> nexo_tui::modal::Modal {
    let (path, warning) = if home {
        (
            "/home/ana",
            Some(
                "This is your home folder: trusting it trusts every folder below it that is not a project of its own."
                    .to_string(),
            ),
        )
    } else {
        ("/home/ana/work/a-long-project-name/with/a/deep/folder/under/it", None)
    };
    nexo_tui::modal::Modal::Trust(nexo_tui::modal::Trust::new(
        path.into(),
        "Project hooks and MCP servers will run.".into(),
        warning,
    ))
}

fn scene(name: &str) -> App {
    let mut a = app();
    match name {
        "empty" => {}
        "conversation" => conversation(&mut a),
        "palette" => {
            conversation(&mut a);
            a.input.set_text("/");
            a.refresh_palette();
        }
        "palette-filtered" => {
            conversation(&mut a);
            a.input.set_text("/mo");
            a.refresh_palette();
        }
        "palette-recent" => {
            // commands run lately are listed once, on top, and not again under their group
            conversation(&mut a);
            a.input.set_text("/");
            a.palette = Some(nexo_tui::palette::Palette::commands_full(
                "",
                1,
                Vec::new(),
                vec!["fork".into(), "tree".into(), "resume".into()],
            ));
        }
        "at-files" => {
            // the file picker on a narrow screen: the name stays whole, the folder is what is cut
            conversation(&mut a);
            a.input.set_text("@ta");
            let files = ["inventory", "mod", "realm", "realm_game", "small"]
                .map(|n| format!("crates/nexo-bench/src/tasks/{n}.rs"));
            a.palette = Some(nexo_tui::palette::Palette::files("ta", 1, files.to_vec()));
        }
        "permission" => {
            conversation(&mut a);
            permission(&mut a);
        }
        "trust" => a.modal = Some(trust_modal(false)),
        "trust-home" => a.modal = Some(trust_modal(true)),
        "typing" => {
            conversation(&mut a);
            a.input.set_text("a question that goes on and on and on, longer than the narrow screen is wide, to see how the editor wraps it\nand a second line");
        }
        _ => unreachable!("{name}"),
    }
    a
}

const SCENES: [&str; 10] = [
    "empty",
    "conversation",
    "palette",
    "palette-filtered",
    "palette-recent",
    "at-files",
    "permission",
    "typing",
    "trust",
    "trust-home",
];

fn capture(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    let rows = rows_of(t.backend().buffer());
    if let Ok(p) = t.get_cursor_position() {
        assert!(p.x < w && p.y < h, "cursor at {p:?} outside {w}x{h}");
    }
    rows
}

fn file_of(scene: &str, w: u16, h: u16) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/captures").join(format!("{scene}_{w}x{h}.txt"))
}

#[test]
fn every_scene_at_the_four_sizes_matches_its_capture() {
    let update = std::env::var("NEXO_UPDATE_GOLDEN").is_ok();
    let mut wrong = Vec::new();
    for scene_name in SCENES {
        for (w, h) in SIZES {
            let mut a = scene(scene_name);
            let rows = capture(&mut a, w, h);
            let text = rows.join("\n") + "\n";
            let path = file_of(scene_name, w, h);
            if update {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, &text).unwrap();
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(kept) if kept == text => {}
                Ok(kept) => wrong.push(format!(
                    "{scene_name} {w}x{h}: differs from {}
{}",
                    path.display(),
                    first_change(&kept, &text)
                )),
                Err(_) => wrong.push(format!("{scene_name} {w}x{h}: no capture, run with NEXO_UPDATE_GOLDEN=1")),
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn at_every_size_the_input_and_the_status_bar_are_on_screen() {
    for scene_name in ["empty", "conversation", "typing"] {
        for (w, h) in SIZES {
            let mut a = scene(scene_name);
            let rows = capture(&mut a, w, h);
            assert_eq!(rows.len(), h as usize, "{scene_name} {w}x{h}: rows");
            let all = rows.join("\n");
            assert!(all.contains('›'), "{scene_name} {w}x{h}: no input line\n{all}");
            // the model or a stand-in for it shows in the bars, shortened but never gone
            assert!(
                all.contains("deepseek") || all.contains("chat") || all.contains("ask"),
                "{scene_name} {w}x{h}: no status bar\n{all}"
            );
        }
    }
}

#[test]
fn a_window_over_the_text_is_whole_at_every_size() {
    for scene_name in ["palette", "palette-filtered", "permission", "trust", "trust-home"] {
        for (w, h) in SIZES {
            let mut a = scene(scene_name);
            let rows = capture(&mut a, w, h);
            let all = rows.join("\n");
            let marker = match scene_name {
                "permission" => "allow",
                "trust" | "trust-home" => "No, don't trust",
                _ => "/",
            };
            assert!(all.contains(marker), "{scene_name} {w}x{h}: the window is missing\n{all}");
            assert!(all.contains('›'), "{scene_name} {w}x{h}: the input line is hidden under the window\n{all}");
        }
    }
}

#[test]
fn growing_the_window_never_shows_less() {
    // the same conversation seen larger has at least as many words on screen as seen smaller
    let mut seen = Vec::new();
    for (w, h) in SIZES {
        let mut a = scene("conversation");
        let rows = capture(&mut a, w, h);
        let words: std::collections::HashSet<String> = rows
            .join(" ")
            .split_whitespace()
            .filter(|s| s.chars().all(char::is_alphanumeric))
            .map(String::from)
            .collect();
        seen.push((w, h, words.len()));
    }
    for pair in seen.windows(2) {
        assert!(
            pair[1].2 >= pair[0].2,
            "{}x{} shows {} words, {}x{} only {}",
            pair[1].0,
            pair[1].1,
            pair[1].2,
            pair[0].0,
            pair[0].1,
            pair[0].2
        );
    }
}

// The two flaws below are written down in docs/TUI-COMPARE.md (findings 1 and 3). They are ignored so
// the suite stays green; the lane that fixes one removes its `ignore`.

#[test]
fn the_permission_window_leaves_the_input_line_alone() {
    for (w, h) in SIZES {
        let mut a = scene("permission");
        let rows = capture(&mut a, w, h);
        let input = rows.iter().find(|r| r.starts_with('›')).expect("an input line");
        assert!(!input.contains('╰') && !input.contains('│'), "{w}x{h}: the window runs over the input: {input:?}");
    }
}

#[test]
fn a_long_line_in_the_editor_is_wrapped_not_hidden() {
    for (w, h) in SIZES {
        let mut a = scene("typing");
        let all = capture(&mut a, w, h).join(" ");
        for word in ["question", "narrow", "wraps", "second"] {
            assert!(all.contains(word), "{w}x{h}: the word {word:?} of the draft is not on screen");
        }
    }
}

/// The reasoning level sits on the rule above the editor, at the right, and the draft stays whole.
#[test]
fn the_thinking_level_sits_on_the_editor_rule() {
    for (w, h) in SIZES {
        let mut a = scene("empty");
        a.effort = Some("high".into());
        let rows = capture(&mut a, w, h);
        let input = rows.iter().position(|r| r.starts_with('›')).expect("an input line");
        let rule = &rows[input - 1];
        assert!(rule.contains("◆ high") && rule.trim_end().ends_with('─'), "{w}x{h}: no level on the rule: {rule:?}");
    }
}

/// Narrowing or widening the window re-wraps the text, but the block at the top of the view stays
/// the block the person was reading.
#[test]
fn a_new_width_keeps_the_block_at_the_top_of_the_view() {
    let mut a = app();
    for i in 0..12 {
        a.handle_agent(AgentEvent::History {
            index: i,
            message: Message::user(format!("question {i}: {}", "words to wrap ".repeat(12))),
        });
    }
    a.follow = false;
    a.layout(100);
    let target = 6;
    a.scroll = a.offsets[target] + 1;
    for w in [60, 40, 100, 80] {
        a.layout(w);
        let top = match a.offsets.binary_search(&a.scroll) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        assert_eq!(top, target, "at width {w} the view moved to block {top}");
    }
}

/// Down to 24 columns the hint bar loses whole hints from the end, never half of one, and the first
/// hint (the key that sends) survives while the bar can hold it.
#[test]
fn narrow_bars_drop_whole_hints_and_keep_the_first() {
    for w in 24..=50u16 {
        let mut a = scene("empty");
        let rows = capture(&mut a, w, 24);
        let hints = rows
            .iter()
            .rev()
            .find(|r| r.contains("Enter send") || r.contains("Esc") || r.contains("↑↓"))
            .cloned()
            .unwrap_or_default()
            .trim_end()
            .to_string();
        assert!(hints.chars().count() <= w as usize, "{w}: hint bar wider than the window: {hints:?}");
        if w >= 30 {
            assert!(hints.contains("Enter send"), "{w}: the first hint is gone: {hints:?}");
        }
        for full in ["Ctrl+J newline", "/ commands", "@ files", "! shell", "Shift+↑ blocks"] {
            let (key, _) = full.split_once(' ').unwrap();
            if hints.contains(key) {
                assert!(hints.contains(full), "{w}: {full:?} is cut: {hints:?}");
            }
        }
    }
}
#[test]
fn a_narrow_activity_line_drops_whole_segments() {
    for w in 24..=80u16 {
        let mut a = scene("empty");
        a.handle_agent(AgentEvent::TurnStart { turn: 1 });
        a.streamed_chars = 480;
        let rows = capture(&mut a, w, 24);
        let line = rows.iter().find(|r| r.contains("Thinking…")).unwrap_or_else(|| panic!("{w}: no activity line"));
        let words = line.trim_end_matches(['─', ' ']);
        assert!(!words.ends_with('·'), "{w}: a separator is left alone: {line:?}");
        if line.contains("Esc") {
            assert!(line.contains("Esc interrupt"), "{w}: the hint is cut: {line:?}");
        }
        if line.contains("↓") {
            assert!(line.contains("tok"), "{w}: the count is cut: {line:?}");
        }
    }
    let mut a = scene("empty");
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.streamed_chars = 480;
    let wide = capture(&mut a, 80, 24).into_iter().find(|r| r.contains("Thinking…")).unwrap();
    assert!(wide.contains("tok · Esc interrupt"), "{wide:?}");
}

fn press(a: &mut App, code: crossterm::event::KeyCode) {
    nexo_tui::event::key(a, crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE));
}

fn select_edit(a: &mut App) {
    a.selected = a.blocks.iter().position(|b| format!("{b:?}").contains("Diff"));
    assert!(a.selected.is_some(), "the scene has an edit");
}

#[test]
fn the_diff_key_opens_the_viewer_and_esc_closes_it() {
    use crossterm::event::KeyCode::{Esc, Tab, F};
    // closed by default, also with the edit selected; the key bar offers the key
    let mut a = scene("conversation");
    select_edit(&mut a);
    let wide = capture(&mut a, 200, 60);
    assert!(a.diffview.is_none(), "closed by default");
    assert!(wide.iter().any(|r| r.contains("F4 diff")), "hint visible");
    // Tab never opens it
    press(&mut a, Tab);
    assert!(a.diffview.is_none(), "Tab never opens the viewer");
    // the key opens it
    press(&mut a, F(4));
    assert!(a.diffview.is_some(), "open after the key");
    // Esc closes it first, the selection stays; a second Esc lets the selection go
    press(&mut a, Esc);
    assert!(a.diffview.is_none(), "closed after Esc");
    assert!(a.selected.is_some(), "the selection stays");
    press(&mut a, Esc);
    assert!(a.selected.is_none(), "the second Esc lets the selection go");
}

/// A panel opened on a wide window and left behind by a narrower one is not drawn, answers no
/// click, and the person is told once.
#[test]
fn a_panel_hidden_by_a_narrow_window_leaves_no_hit_area_and_says_so_once() {
    let mut a = app();
    a.show_files = true;
    capture(&mut a, 120, 40);
    assert!(a.files_area.width > 0, "the panel is drawn at 120 columns");
    capture(&mut a, 90, 40);
    assert_eq!(a.files_area, ratatui::layout::Rect::default());
    let note = a.status.as_ref().map(|(t, _)| t.clone());
    assert_eq!(note.as_deref(), Some("panel hidden under 100 columns"));
    a.status = None;
    capture(&mut a, 90, 40);
    assert!(a.status.is_none(), "the note is not repeated on every frame");
    capture(&mut a, 120, 40);
    assert!(a.files_area.width > 0);
}

#[test]
fn the_trust_question_shows_the_path_and_what_a_yes_enables() {
    for (w, h) in [(80, 24), (120, 40), (200, 60)] {
        let all = capture(&mut scene("trust"), w, h).join("\n");
        for text in [
            "Do you trust the files in this folder?",
            "Project hooks and MCP servers will run.",
            "Yes, trust this folder",
        ] {
            assert!(all.contains(text), "{w}x{h}: {text:?} is missing\n{all}");
        }
    }
    let all = capture(&mut scene("trust-home"), 120, 40).join("\n");
    assert!(all.contains("trusting it trusts every folder below it"), "{all}");
}

#[test]
fn the_trust_question_keeps_both_answers_on_a_short_screen() {
    for (w, h) in [(40, 12), (30, 9)] {
        let all = capture(&mut scene("trust-home"), w, h).join("\n");
        assert!(all.contains("Yes, trust") && all.contains("No, don't"), "{w}x{h}: an answer is cut\n{all}");
    }
}

#[test]
fn the_trust_question_takes_arrows_enter_y_n_and_esc() {
    use crossterm::event::KeyCode::{Char, Down, Enter, Esc, Up};
    let answer = |keys: &[crossterm::event::KeyCode]| {
        let mut a = scene("trust");
        for k in keys {
            press(&mut a, *k);
        }
        (a.trust_answer, a.modal.is_some())
    };
    assert_eq!(answer(&[]), (None, true), "nothing is decided by itself");
    assert_eq!(answer(&[Enter]), (Some(true), false), "yes is the first row");
    assert_eq!(answer(&[Down, Enter]), (Some(false), false));
    assert_eq!(answer(&[Down, Up, Enter]), (Some(true), false));
    assert_eq!(answer(&[Char('y')]), (Some(true), false));
    assert_eq!(answer(&[Char('N')]), (Some(false), false));
    assert_eq!(answer(&[Esc]), (Some(false), false));
}

/// The first row that changed, kept and now, so a failure on CI shows what moved.
fn first_change(kept: &str, now: &str) -> String {
    let (k, n): (Vec<&str>, Vec<&str>) = (kept.lines().collect(), now.lines().collect());
    let row = (0..k.len().max(n.len())).find(|&i| k.get(i) != n.get(i)).unwrap_or(0);
    let show = |v: &[&str]| v.get(row).map_or("<no row>".to_string(), |s| format!("{:?}", s.trim_end()));
    format!("  row {}: kept {}\n  row {}: now  {}", row + 1, show(&k), row + 1, show(&n))
}

#[test]
fn a_differing_capture_names_the_first_row_that_changed() {
    let msg = first_change("a\nb\nc\n", "a\nB\nc\n");
    assert!(msg.contains("row 2: kept \"b\""), "{msg}");
    assert!(msg.contains("row 2: now  \"B\""), "{msg}");
    assert!(first_change("a\n", "a\nb\n").contains("kept <no row>"));
}
