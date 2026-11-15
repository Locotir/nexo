//! Suggestions, history recall, links and error cards.

use super::*;

#[test]
fn a_suggestion_is_asked_for_at_a_normal_end_and_taken_whole_by_tab_or_right() {
    let (mut a, mut rx) = app();
    // off: a turn's end asks for nothing
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    assert!(!drain(&mut rx).contains(&AgentCommand::Suggest));
    a.opts.suggest = true;
    for reason in [TurnEnd::Interrupted, TurnEnd::Error, TurnEnd::MaxTurns] {
        a.handle_agent(AgentEvent::TurnEnd { reason });
    }
    assert!(!drain(&mut rx).contains(&AgentCommand::Suggest), "only a normal end asks");
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    assert!(drain(&mut rx).contains(&AgentCommand::Suggest));

    // Tab takes the whole suggestion; the suggestion is gone and the draft is its text
    a.handle_agent(AgentEvent::Suggestion { text: "  run the\ntests ".into() });
    assert_eq!(a.suggestion_shown(), Some("run the tests"));
    press(&mut a, KeyCode::Tab);
    assert_eq!(a.input.text(), "run the tests");
    assert_eq!(a.suggestion, None);

    // Right does the same
    a.input.clear();
    a.handle_agent(AgentEvent::Suggestion { text: "commit it".into() });
    press(&mut a, KeyCode::Right);
    assert_eq!(a.input.text(), "commit it");

    // Typing hides the suggestion, and deleting the draft shows it again.
    a.input.clear();
    a.handle_agent(AgentEvent::Suggestion { text: "commit it".into() });
    press(&mut a, KeyCode::Char('x'));
    assert_eq!(a.input.text(), "x");
    assert_eq!(a.suggestion_shown(), None);
    press(&mut a, KeyCode::Backspace);
    assert_eq!(a.suggestion_shown(), Some("commit it"));

    // A suggestion arriving while typing is retained, but not shown until the draft is empty.
    a.input.set_text("hello");
    a.handle_agent(AgentEvent::Suggestion { text: "run tests".into() });
    assert_eq!(a.suggestion_shown(), None);
    for _ in 0..5 {
        press(&mut a, KeyCode::Backspace);
    }
    assert_eq!(a.suggestion_shown(), Some("run tests"));
    a.suggestion = None;
    a.busy = true;
    a.handle_agent(AgentEvent::Suggestion { text: "commit it".into() });
    assert_eq!(a.suggestion, None);
    a.busy = false;

    // a new turn clears it, and so does a submit
    a.handle_agent(AgentEvent::Suggestion { text: "commit it".into() });
    a.handle_agent(AgentEvent::TurnStart { turn: 2 });
    assert_eq!(a.suggestion, None);
    a.busy = false;
    a.handle_agent(AgentEvent::Suggestion { text: "commit it".into() });
    a.input.set_text("go");
    a.suggestion = Some("commit it".into());
    a.submit();
    assert_eq!(a.suggestion, None);

    // Tab keeps its own use when the draft has text: the word is completed, here a command
    a.input.set_text("/he");
    a.suggestion = Some("commit it".into());
    a.refresh_palette();
    assert_eq!(a.suggestion_shown(), None);
    press(&mut a, KeyCode::Tab);
    assert!(a.input.text().starts_with("/help"), "{}", a.input.text());
}

#[test]
fn recalling_slash_commands_with_up_keeps_browsing_history() {
    let (mut a, _rx) = bare();
    a.input.push_history("/status");
    a.input.push_history("/cost");
    press(&mut a, KeyCode::Up);
    assert_eq!(a.input.full_text(), "/cost");
    assert!(a.palette.is_none(), "a recalled entry must not open the palette");
    press(&mut a, KeyCode::Up);
    assert_eq!(a.input.full_text(), "/status");
    assert!(a.palette.is_none());
    // typing on a recalled entry still opens it
    a.input.clear();
    press(&mut a, KeyCode::Char('/'));
    assert!(a.palette.is_some());
}

fn click_at(a: &mut App, x: u16, y: u16, modifiers: KeyModifiers) {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        crate::event::mouse(a, MouseEvent { kind, column: x, row: y, modifiers });
    }
}

/// An app with `md` as a reply laid out in a transcript `w` columns wide.
fn app_with_reply(md: &str, w: u16) -> App {
    let (mut a, _rx) = bare();
    a.push(Block::Assistant { text: md.into(), streaming: false, at: None, started: None, took_ms: None });
    a.transcript_area = Rect::new(0, 1, w, 20);
    a.layout(w as usize);
    a.settle(20);
    a
}

/// The screen cell `(x, y)` of the first `needle` in the laid-out rows.
fn find_cell(a: &mut App, needle: &str) -> (u16, u16) {
    for line in 0..30 {
        if let Some(t) = a.line_text(line) {
            if let Some(i) = t.find(needle) {
                return (crate::select::width_of(&t[..i]) as u16, 1 + line as u16);
            }
        }
    }
    panic!("{needle:?} is not on screen");
}

#[test]
fn a_click_on_a_bare_link_opens_it_and_so_does_ctrl_click() {
    let (mut a, _rx) = bare();
    a.push(Block::User { text: "see https://example.com/a. ok".into(), at: None });
    a.transcript_area = Rect::new(0, 1, 80, 20);
    a.layout(80);
    a.settle(20);
    let (col, row) = find_cell(&mut a, "https");
    crate::links::take_opened();
    // beside the link nothing opens
    click_at(&mut a, col - 2, row, KeyModifiers::NONE);
    click_at(&mut a, col - 2, row, KeyModifiers::CONTROL);
    assert!(crate::links::take_opened().is_empty());
    assert_eq!(a.link_at(col + 3, row).as_deref(), Some("https://example.com/a"));
    click_at(&mut a, col + 3, row, KeyModifiers::NONE);
    assert_eq!(crate::links::take_opened(), vec!["https://example.com/a".to_string()]);
    click_at(&mut a, col + 3, row, KeyModifiers::CONTROL);
    assert_eq!(crate::links::take_opened(), vec!["https://example.com/a".to_string()]);
    assert!(a.toasts.iter().any(|t| t.text.contains("opened example.com")), "a toast says it opened");
}

#[test]
fn a_click_on_a_markdown_link_opens_its_address_from_the_text_or_from_the_address() {
    let mut a = app_with_reply("Here is the first video: [watch the video](https://example.com/v/first?x=1) done", 90);
    let (col, row) = find_cell(&mut a, "watch");
    crate::links::take_opened();
    click_at(&mut a, col + 2, row, KeyModifiers::NONE);
    assert_eq!(crate::links::take_opened(), vec!["https://example.com/v/first?x=1".to_string()]);
    let (col, row) = find_cell(&mut a, "https");
    click_at(&mut a, col + 5, row, KeyModifiers::NONE);
    assert_eq!(crate::links::take_opened(), vec!["https://example.com/v/first?x=1".to_string()]);
    // the hover marks it
    a.pointer_moved(col + 5, row);
    assert!(matches!(a.hover, Hover::Link { .. }));
}

#[test]
fn a_link_wrapped_over_two_rows_opens_whole_from_either_row() {
    let url = "https://example.com/some/very/long/path/to/a/file-name.mp4";
    let mut a = app_with_reply(&format!("go {url} now"), 30);
    let rows: Vec<u16> = (0..6u16)
        .filter(|l| a.line_text(*l as usize).is_some_and(|t| t.contains("example") || t.contains("mp4")))
        .collect();
    assert!(rows.len() >= 2, "the link must wrap: {rows:?}");
    crate::links::take_opened();
    for r in &rows {
        click_at(&mut a, 5, 1 + r, KeyModifiers::NONE);
        assert_eq!(crate::links::take_opened(), vec![url.to_string()], "row {r}");
    }
}

#[test]
fn a_drag_over_a_link_selects_and_opens_nothing() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut a = app_with_reply("see https://example.com/abc now", 80);
    let (col, row) = find_cell(&mut a, "https");
    crate::links::take_opened();
    for (kind, x) in [
        (MouseEventKind::Down(MouseButton::Left), col + 2),
        (MouseEventKind::Drag(MouseButton::Left), col + 8),
        (MouseEventKind::Up(MouseButton::Left), col + 8),
    ] {
        crate::event::mouse(&mut a, MouseEvent { kind, column: x, row, modifiers: KeyModifiers::NONE });
    }
    assert!(crate::links::take_opened().is_empty());
}

#[cfg(unix)]
#[test]
fn a_link_to_a_program_opens_its_folder_and_a_plain_file_opens_itself() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let prog = dir.path().join("run me.sh");
    let doc = dir.path().join("notes.txt");
    std::fs::write(&prog, "#!/bin/sh\n").unwrap();
    std::fs::write(&doc, "hi").unwrap();
    std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(&doc, std::fs::Permissions::from_mode(0o644)).unwrap();
    let url = |p: &std::path::Path| crate::render::blocks::file_url(&p.to_string_lossy());
    let folder = url(dir.path());
    // the folder of a program, what opens in its place
    crate::links::take_opened();
    let mut a = app_with_reply(&format!("run {} now", url(&prog)), 200);
    let (col, row) = find_cell(&mut a, "file://");
    click_at(&mut a, col + 3, row, KeyModifiers::NONE);
    assert_eq!(crate::links::take_opened(), vec![folder.clone()]);
    assert!(a.toasts.iter().any(|t| t.text.contains("never run")), "the notice says why");
    let mut a = app_with_reply(&format!("read {} now", url(&doc)), 200);
    let (col, row) = find_cell(&mut a, "file://");
    click_at(&mut a, col + 3, row, KeyModifiers::NONE);
    assert_eq!(crate::links::take_opened(), vec![url(&doc)]);
    // a directory opens as it is
    let mut a = app_with_reply(&format!("in {folder} now"), 200);
    let (col, row) = find_cell(&mut a, "file://");
    click_at(&mut a, col + 3, row, KeyModifiers::NONE);
    assert_eq!(crate::links::take_opened(), vec![folder]);
}

#[test]
fn a_scheme_that_is_not_web_or_file_is_copied_not_opened() {
    use crate::links::{plan, Plan};
    assert!(matches!(plan("ftp://example.com/x"), Plan::Copy { .. }));
    assert!(matches!(plan("file://otherhost/share/x.exe"), Plan::Copy { .. }));
    assert!(matches!(plan("file:///definitely/not/here.txt"), Plan::Copy { .. }));
    assert!(matches!(plan("https://example.com"), Plan::Open { .. }));
    assert!(
        crate::links::program_ext(".ps1") && crate::links::program_ext(".exe") && !crate::links::program_ext(".txt")
    );
    let (mut a, _rx) = bare();
    crate::links::take_opened();
    a.open_link("ftp://example.com/x");
    assert!(crate::links::take_opened().is_empty());
}

#[test]
fn the_same_error_twice_in_a_turn_is_one_card_with_a_count_and_the_raw_text_opens_on_demand() {
    use nexo_core::{ErrorKind, Problem, ProblemCtx, ProviderError};
    let (mut a, _rx) = app();
    let e = ProviderError::from_http(429, r#"{"error":{"message":"Rate limit reached"}}"#);
    let p = Problem::of(&e, &ProblemCtx { provider: "openai".into(), ..Default::default() });
    let send = |a: &mut App, p: &Problem| {
        a.handle_agent(AgentEvent::Notice { level: Level::Error, text: p.plain(), problem: Some(Box::new(p.clone())) })
    };
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("go") });
    send(&mut a, &p);
    send(&mut a, &p);
    send(&mut a, &p);
    let cards: Vec<usize> = (0..a.blocks.len()).filter(|i| matches!(a.blocks[*i], Block::Problem { .. })).collect();
    assert_eq!(cards.len(), 1, "three of the same are one card");
    assert!(matches!(&a.blocks[cards[0]], Block::Problem { count: 3, folded: true, .. }));
    let closed: Vec<String> = a.lines(cards[0], 100).iter().map(crate::render::markdown::text_of).collect();
    assert!(closed.len() <= 3 && closed[0].contains("×3"), "{closed:?}");
    // another error is its own card
    let other = Problem::of(&ProviderError::new(ErrorKind::Overloaded, "busy"), &ProblemCtx::default());
    send(&mut a, &other);
    assert_eq!(a.blocks.iter().filter(|b| matches!(b, Block::Problem { .. })).count(), 2);
    // a new turn starts a new card for the same error
    a.handle_agent(AgentEvent::History { index: 1, message: Message::user("again") });
    send(&mut a, &p);
    assert_eq!(a.blocks.iter().filter(|b| matches!(b, Block::Problem { .. })).count(), 3);
    // the card opens to the raw text and folds back
    assert!(a.foldable(cards[0]));
    a.toggle_block(cards[0]);
    let open: Vec<String> = a.lines(cards[0], 100).iter().map(crate::render::markdown::text_of).collect();
    assert!(open.iter().any(|l| l.contains("HTTP 429")), "{open:?}");
    a.toggle_block(cards[0]);
    assert_eq!(a.lines(cards[0], 100).len(), closed.len());
}

#[test]
fn an_error_saved_by_an_older_nexo_loads_as_a_card_that_opens_to_all_of_its_text() {
    // the shape an old session holds: no `problem`, the whole text in one string
    let json = r#"{"event":"notice","level":"error","text":"mcp server `files` failed: could not start\nspawn error: No such file or directory\nPATH was /usr/bin:/bin"}"#;
    let ev: AgentEvent = serde_json::from_str(json).unwrap();
    let (mut a, _rx) = app();
    a.handle_agent(ev);
    let i = a.blocks.iter().position(|b| matches!(b, Block::Problem { .. })).expect("a card");
    let closed: Vec<String> = a.lines(i, 100).iter().map(crate::render::markdown::text_of).collect();
    assert!(closed.len() <= 3 && closed[0].contains("mcp server `files` failed"), "{closed:?}");
    assert!(a.foldable(i), "the text behind the first line opens");
    a.toggle_block(i);
    let open: Vec<String> = a.lines(i, 100).iter().map(crate::render::markdown::text_of).collect();
    let all = open.join("\n");
    assert!(all.contains("No such file or directory") && all.contains("PATH was /usr/bin:/bin"), "{open:?}");
    assert_eq!(a.blocks[i].copy_text().unwrap().lines().count(), 3, "copy gets the text once");
}
