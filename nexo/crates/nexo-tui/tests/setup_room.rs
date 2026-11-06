//! The first step of the setup uses the room the screen has, and the scan note goes away.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_core::agent::AgentEvent;
use nexo_core::ProviderConfig;
use nexo_tui::modal::Modal;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, event, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

/// A clean home: nothing chosen, 21 providers and no key for any of them.
fn clean() -> App {
    std::env::set_var("NEXO_AUTH_FILE", "/nonexistent/nexo-test-auth.json");
    let (tx, _rx) = mpsc::unbounded_channel();
    let providers = (0..21)
        .map(|i| {
            (
                format!("prov{i:02}"),
                ProviderConfig::new("openai-chat", "https://x.example", Some(&format!("NEXO_T_ROOM_{i}"))),
            )
        })
        .collect();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        providers,
        theme: "dark".into(),
        animations: false,
        ..TuiOptions::default()
    };
    App::new(opts, tx)
}

fn press(a: &mut App, code: KeyCode) {
    event::key(a, KeyEvent::new(code, KeyModifiers::NONE));
}

fn screen(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer())
}

fn count(rows: &[String], needle: &str) -> usize {
    rows.iter().filter(|r| r.contains(needle)).count()
}

#[test]
fn the_dialog_takes_the_height_the_screen_gives_it() {
    for (w, h) in [(100u16, 30u16), (120, 40)] {
        let mut a = clean();
        press(&mut a, KeyCode::Enter);
        let rows = screen(&mut a, w, h);
        let top = rows.iter().position(|r| r.contains("set up a model")).expect("title");
        let bottom = rows.iter().rposition(|r| r.contains('╰')).expect("bottom edge");
        assert!(
            bottom - top + 1 >= (h as usize) * 3 / 4 - 1,
            "{w}x{h}: dialog only {} rows\n{}",
            bottom - top + 1,
            rows.join("\n")
        );
        // closed group: the own server row is in view, not behind a scrollbar
        let own = rows
            .iter()
            .find(|r| r.contains("set up your own server"))
            .unwrap_or_else(|| panic!("{w}x{h}\n{}", rows.join("\n")));
        assert!(own.trim_end().ends_with("│") && !own.contains('█'), "{own}");
    }
}

#[test]
fn the_open_group_of_others_shows_its_rows_or_scrolls_to_them() {
    for (w, h, all) in [(120u16, 40u16, true), (100, 30, false)] {
        let mut a = clean();
        press(&mut a, KeyCode::Enter);
        // the group of the others is the first row; open it
        press(&mut a, KeyCode::Right);
        let rows = screen(&mut a, w, h);
        let shown = count(&rows, "set up prov");
        if all {
            assert_eq!(shown, 21, "{w}x{h}\n{}", rows.join("\n"));
        } else {
            assert!(shown >= 15, "{w}x{h}: {shown} rows\n{}", rows.join("\n"));
            // the rest is one End away
            press(&mut a, KeyCode::End);
            let rows = screen(&mut a, w, h);
            assert!(count(&rows, "set up your own server") == 1, "{}", rows.join("\n"));
        }
    }
}

#[test]
fn the_scan_note_goes_when_nothing_was_found() {
    let mut a = clean();
    press(&mut a, KeyCode::Enter);
    let note = "Looking for Ollama";
    assert!(count(&screen(&mut a, 120, 40), note) == 1);
    a.handle_agent(AgentEvent::LocalServers { found: vec![] });
    assert!(matches!(a.modal, Some(Modal::Setup(_))));
    assert_eq!(count(&screen(&mut a, 120, 40), note), 0);
}
