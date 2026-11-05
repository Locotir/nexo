//! The look shared by the windows: clear ground around them, keys in a row of their own, a small
//! empty state.

use std::path::PathBuf;

use nexo_tui::commands::Command;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
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
        group_tools: true,
        keys: Default::default(),
        status_line: vec![],
        status_command: String::new(),
        notify_after: 15,
        animations: false,
        vim: false,
        session_id: Some("20261027-120000-ab12".into()),
        sessions: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}

fn draw(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer())
}

#[test]
fn help_and_jobs_leave_nothing_of_the_welcome_card_beside_them() {
    for (w, h) in [(60u16, 24u16), (100, 32)] {
        for cmd in [Command::Help, Command::Jobs(None)] {
            let mut a = app();
            a.run_command(cmd.clone());
            let text = draw(&mut a, w, h).join("\n");
            assert!(!text.contains("nexo 0.1.0") && !text.contains("●─╮"), "{w}x{h} {cmd:?}\n{text}");
        }
    }
}

#[test]
fn an_empty_jobs_window_is_small_and_keeps_its_keys_in_their_own_row() {
    let mut a = app();
    a.run_command(Command::Jobs(None));
    let rows = draw(&mut a, 60, 24);
    let top = rows.iter().position(|r| r.contains("background jobs")).unwrap();
    let bottom = rows.iter().position(|r| r.contains('╰') && r.contains('╯')).unwrap();
    assert!(bottom - top <= 7, "a box for one line of text\n{}", rows.join("\n"));
    let title = &rows[top];
    assert!(!title.contains("Enter opens") && title.contains('╮'), "{title:?}");
    let all = rows.join("\n");
    assert!(all.contains("No background jobs") && all.contains("Esc"), "{all}");
}

#[test]
fn the_theme_picker_leaves_the_chat_in_sight() {
    for cmd in [Command::Theme(None), Command::Model(None)] {
        let mut a = app();
        a.run_command(cmd.clone());
        let text = draw(&mut a, 140, 40).join("\n");
        assert!(text.contains("nexo 0.1.0"), "{cmd:?}: the screen behind the picker is not wiped\n{text}");
    }
}
