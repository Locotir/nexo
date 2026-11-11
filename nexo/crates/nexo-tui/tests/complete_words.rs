//! The ghost word of the draft: → takes it at the end of a word, and only then.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_core::agent::AgentEvent;
use nexo_core::message::Message;
use nexo_core::tool::ToolOutput;
use nexo_tui::input::VimMode;
use nexo_tui::{draw_frame, event, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

const HASH: &str = "e23b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b85";

fn app_with(theme: &str, complete: bool) -> App {
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
        fold_after: 3,
        group_tools: false,
        complete_words: complete,
        suggest: false,
        keys: Default::default(),
        status_line: vec![],
        status_command: String::new(),
        notify_after: 15,
        animations: false,
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::Galaxy,
        home_stars: true,
        bursts: true,
        todo_strip: false,
        vim: false,
        prompt: None,
        session_id: Some("20261102-100000-ef56".into()),
        sessions: vec![],
        skills: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a
}

fn app() -> App {
    let mut a = app_with("dark", true);
    a.handle_agent(AgentEvent::ToolCall {
        id: "c1".into(),
        name: "bash".into(),
        args: serde_json::json!({ "command": "sha256sum release.tar" }),
        summary: "bash: sha256sum".into(),
    });
    a.handle_agent(AgentEvent::ToolDone {
        id: "c1".into(),
        output: ToolOutput::ok(format!("{HASH}  release.tar")),
        elapsed_ms: 5,
    });
    a.handle_agent(AgentEvent::TextDelta { text: "La configuración del servidor, in Madrid.".into() });
    a.handle_agent(AgentEvent::AssistantDone {
        message: Message::assistant("La configuración del servidor, in Madrid."),
    });
    a
}

fn press(a: &mut App, code: KeyCode) {
    event::key(a, KeyEvent::new(code, KeyModifiers::NONE));
}

fn type_str(a: &mut App, s: &str) {
    for c in s.chars() {
        press(a, KeyCode::Char(c));
    }
}

fn draw(a: &mut App, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    t
}

#[test]
fn right_completes_hash() {
    let mut a = app();
    type_str(&mut a, "e23");
    assert_eq!(a.complete_suffix().as_deref(), Some(&HASH[3..]));
    press(&mut a, KeyCode::Right);
    assert_eq!(a.input.full_text(), HASH);
    assert_eq!(a.complete_suffix(), None, "nothing more to add");
}

#[test]
fn right_completes_word() {
    let mut a = app();
    type_str(&mut a, "configu");
    press(&mut a, KeyCode::Right);
    assert_eq!(a.input.full_text(), "configuración");
    // the typed case is kept for plain words, and a name keeps its own
    a.input.clear();
    type_str(&mut a, "Madr");
    assert_eq!(a.complete_suffix().as_deref(), Some("id"), "the tail of the word, the typed capital kept");
}

#[test]
fn tab_completes_like_right() {
    let mut a = app();
    type_str(&mut a, "configu");
    press(&mut a, KeyCode::Tab);
    assert_eq!(a.input.full_text(), "configuración");
    a.input.clear();
    type_str(&mut a, "e23");
    press(&mut a, KeyCode::Tab);
    assert_eq!(a.input.full_text(), HASH);
}

#[test]
fn right_with_block_selected_does_not() {
    let mut a = app();
    type_str(&mut a, "configu");
    let last = a.blocks.len() - 1;
    a.selected = Some(last);
    press(&mut a, KeyCode::Right);
    assert_eq!(a.input.full_text(), "configu", "the block keeps the arrows");
}

#[test]
fn right_in_middle_of_word_moves_cursor() {
    let mut a = app();
    type_str(&mut a, "configu");
    press(&mut a, KeyCode::Left);
    press(&mut a, KeyCode::Left);
    assert_eq!(a.complete_suffix(), None, "the cursor is inside the word");
    press(&mut a, KeyCode::Right);
    assert_eq!(a.input.full_text(), "configu");
    assert_eq!(a.input.editor.cursor(), (0, 6));
}

#[test]
fn right_moves_the_cursor_when_there_is_no_ghost() {
    let mut a = app();
    type_str(&mut a, "zzz qqq");
    press(&mut a, KeyCode::Left);
    press(&mut a, KeyCode::Right);
    assert_eq!(a.input.editor.cursor(), (0, 7));
    assert_eq!(a.input.full_text(), "zzz qqq");
}

#[test]
fn palette_open_keeps_keys() {
    let mut a = app();
    type_str(&mut a, "/mo");
    assert!(a.palette.is_some());
    assert_eq!(a.complete_suffix(), None);
    press(&mut a, KeyCode::Right);
    assert!(a.input.full_text().starts_with("/mo"));
    assert!(a.input.full_text().len() < 8, "{:?}", a.input.full_text());
}

#[test]
fn vim_normal_no_ghost_insert_yes() {
    let mut a = app();
    a.input.vim = Some(VimMode::Insert);
    type_str(&mut a, "configu");
    assert_eq!(a.complete_suffix().as_deref(), Some("ración"));
    a.input.vim = Some(VimMode::Normal);
    assert_eq!(a.complete_suffix(), None);
}

#[test]
fn off_switch_means_no_ghost() {
    let mut a = app_with("dark", false);
    a.handle_agent(AgentEvent::TextDelta { text: "configuración".into() });
    a.handle_agent(AgentEvent::AssistantDone { message: Message::assistant("configuración") });
    type_str(&mut a, "configu");
    assert!(a.words.is_empty());
    assert_eq!(a.complete_suffix(), None);
    press(&mut a, KeyCode::Right);
    assert_eq!(a.input.full_text(), "configu");
}

#[test]
fn multiline_second_line() {
    let mut a = app();
    type_str(&mut a, "first line here");
    event::key(&mut a, KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
    type_str(&mut a, "see configu");
    // no ghost in a draft of several lines: it would read as part of what was written
    assert_eq!(a.complete_suffix(), None);
}

#[test]
fn a_path_typed_from_its_middle_and_a_slash_command_word() {
    let mut a = app();
    type_str(&mut a, "src/configu");
    assert_eq!(a.complete_suffix().as_deref(), Some("ración"), "falls back to the piece after the slash");
    a.input.clear();
    type_str(&mut a, "/release");
    assert_eq!(a.complete_suffix(), None, "the first word of a command is the palette's");
}

#[test]
fn replay_indexes() {
    let mut a = app_with("dark", true);
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("deploy the deployment pipeline") });
    a.handle_agent(AgentEvent::History { index: 1, message: Message::assistant("see the configuración file") });
    type_str(&mut a, "deploym");
    assert_eq!(a.complete_suffix().as_deref(), Some("ent"));
    a.input.clear();
    type_str(&mut a, "configu");
    assert_eq!(a.complete_suffix().as_deref(), Some("ración"));
    a.handle_agent(AgentEvent::HistoryCleared);
    assert_eq!(a.complete_suffix(), None, "a cleared transcript forgets its words");
}

fn ghost_cells(a: &mut App, t: &Terminal<TestBackend>) -> (String, ratatui::style::Style) {
    let buf = t.backend().buffer();
    for y in (0..buf.area.height).rev() {
        let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect();
        if let Some(at) = row.find("configu") {
            let col = row[..at].chars().count() as u16 + 7;
            let s: String = (col..col + 6).map(|x| buf[(x, y)].symbol().to_string()).collect();
            return (s, buf[(col + 1, y)].style());
        }
    }
    let _ = a;
    panic!("the draft is not on the screen");
}

#[test]
fn the_ghost_is_drawn_dim_in_both_themes() {
    for theme in ["dark", "light"] {
        let mut a = app_with(theme, true);
        a.handle_agent(AgentEvent::TextDelta { text: "configuración".into() });
        a.handle_agent(AgentEvent::AssistantDone { message: Message::assistant("configuración") });
        type_str(&mut a, "configu");
        let t = draw(&mut a, 60, 16);
        let (ghost, style) = ghost_cells(&mut a, &t);
        assert_eq!(ghost, "ración", "{theme}");
        let dim = a.theme.dim();
        assert_eq!(style.fg, dim.fg, "{theme}: the ghost uses the dim colour");
        assert_ne!(style.fg, a.theme.base().fg, "{theme}: and it differs from the typed text");
    }
}

#[test]
fn the_ghost_survives_ascii_mode_and_stays_plain() {
    let mut a = app();
    type_str(&mut a, "configu");
    let mut t = draw(&mut a, 60, 16);
    nexo_tui::ascii::fold(t.current_buffer_mut());
    let buf = t.backend().buffer();
    let text: String = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("configuración") || text.contains("configuracion"), "{text}");
}

#[test]
fn a_long_ghost_is_cut_with_an_ellipsis_and_right_takes_it_whole() {
    let mut a = app();
    for w in [60u16, 70] {
        a.input.clear();
        let _ = draw(&mut a, w, 16);
        type_str(&mut a, "check e23");
        let s = a.complete_suffix().expect("the hash still has a ghost");
        assert_eq!(s, &HASH[3..]);
        let t = draw(&mut a, w, 16);
        let buf = t.backend().buffer();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .filter(|r| r.contains("check e23"))
            .collect();
        assert!(rows.iter().any(|r| r.contains('…')), "{rows:?}");
    }
    press(&mut a, KeyCode::Right);
    assert_eq!(a.input.full_text(), format!("check {HASH}"));
}

#[test]
fn a_picked_block_hides_the_ghost() {
    let mut a = app();
    type_str(&mut a, "configu");
    assert!(a.complete_suffix().is_some());
    a.selected = Some(a.blocks.len() - 1);
    assert_eq!(a.complete_suffix(), None);
}

#[test]
fn a_shell_command_the_person_ran_feeds_the_words() {
    let mut a = app_with("dark", true);
    let hash = "a768d4a88fb4ace4c9e1611f082dd5065b6da219bf45dd934519a24beb7b502f";
    a.handle_agent(AgentEvent::ShellOutput {
        command: "echo nexo | sha256sum".into(),
        output: format!("{hash}  -\n"),
        exit_code: Some(0),
    });
    type_str(&mut a, "a768");
    press(&mut a, KeyCode::Right);
    assert_eq!(a.input.full_text(), hash);
}
