//! Pasted text and images, questions, plans, modes and suggestions.

use super::*;

#[test]
fn a_paste_that_comes_as_keys_is_one_draft_and_a_long_one_is_sent_whole() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    let key = |c: KeyCode, kind| Event::Key(KeyEvent::new_with_kind(c, KeyModifiers::NONE, kind));
    // the Windows console: each character a press and a release, Enter between the lines
    let text: String = (1..=10).map(|n| format!("row {n}\n")).collect();
    let mut batch = Vec::new();
    for c in text.chars() {
        let code = if c == '\n' { KeyCode::Enter } else { KeyCode::Char(c) };
        batch.push(key(code, KeyEventKind::Press));
        batch.push(key(code, KeyEventKind::Release));
    }
    let (mut a, mut rx) = app();
    assert!(crate::event::may_start_paste(&batch[0]) && a.takes_paste());
    let pasted = crate::event::paste_burst(&batch, false).expect("a burst of keys is a paste");
    assert_eq!(pasted, text);
    crate::event::handle(&mut a, Event::Paste(pasted));
    assert!(rx.try_recv().is_err(), "its Enters did not send anything");
    assert_eq!(a.input.text(), "[Pasted text #1 +10 lines]");
    a.input.insert_paste(" what does it print?");
    a.submit();
    match rx.try_recv() {
        Ok(AgentCommand::Submit { text: sent, .. }) => {
            assert!(sent.starts_with("row 1\nrow 2\n") && sent.ends_with("row 10\n what does it print?"), "{sent}")
        }
        other => panic!("{other:?}"),
    }
    // typing is not a paste: a lone key, or keys with Ctrl among them
    assert!(crate::event::paste_burst(&[key(KeyCode::Char('a'), KeyEventKind::Press)], false).is_none());
    let ctrl = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(crate::event::paste_burst(&[batch[0].clone(), batch[2].clone(), batch[4].clone(), ctrl], false).is_none());
    // AltGr (Ctrl+Alt) characters are typed text
    let at = Event::Key(KeyEvent::new(KeyCode::Char('@'), KeyModifiers::CONTROL | KeyModifiers::ALT));
    assert_eq!(crate::event::paste_burst(&[at.clone(), at.clone(), at], false).as_deref(), Some("@@@"));
    // one line and its Enter at once is a paste too: a big paste on Windows comes in
    // pieces, and a piece that is one line must not send it
    let piece: Vec<Event> = "fn main() {\n"
        .chars()
        .map(|c| if c == '\n' { KeyCode::Enter } else { KeyCode::Char(c) })
        .map(|c| key(c, KeyEventKind::Press))
        .collect();
    assert_eq!(crate::event::paste_burst(&piece, false).as_deref(), Some("fn main() {\n"));
    // the tail of a paste is the paste, however short: a lone Enter included
    assert!(crate::event::paste_burst(&[key(KeyCode::Enter, KeyEventKind::Press)], false).is_none());
    assert_eq!(crate::event::paste_burst(&[key(KeyCode::Enter, KeyEventKind::Press)], true).as_deref(), Some("\n"));
    // the mouse moving through it does not break it
    let moved = Event::Mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Moved,
        column: 1,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });
    let mut with_mouse = piece.clone();
    with_mouse.insert(2, moved);
    assert_eq!(crate::event::paste_burst(&with_mouse, false).as_deref(), Some("fn main() {\n"));
    // bracketed paste's markers that come as keys (Windows Terminal) are dropped
    let marked: Vec<Event> = "\x1b[200~a\nb\x1b[201~"
        .chars()
        .map(|c| match c {
            '\n' => KeyCode::Enter,
            '\x1b' => KeyCode::Esc,
            c => KeyCode::Char(c),
        })
        .map(|c| key(c, KeyEventKind::Press))
        .collect();
    assert!(crate::event::may_start_paste(&marked[0]));
    let got = crate::event::paste_burst(&marked, false).expect("a marked paste");
    assert_eq!(crate::event::unmark(&got), "a\nb");
    // a lone Esc is a key, not a paste
    assert!(crate::event::paste_burst(&[key(KeyCode::Esc, KeyEventKind::Press)], true).is_none());
}

#[test]
fn a_question_is_answered_with_the_arrows_a_digit_or_words_of_ones_own() {
    let (mut a, mut rx) = app();
    a.handle_agent(AgentEvent::Question { question: question("q1", false, None) });
    assert!(matches!(&a.modal, Some(Modal::Question(_))));
    press(&mut a, KeyCode::Down);
    press(&mut a, KeyCode::Enter);
    assert_eq!(answer_of(&mut rx), Some(("q1".into(), Some("green".into()))));
    assert!(a.modal.is_none());

    a.handle_agent(AgentEvent::Question { question: question("q2", false, None) });
    press(&mut a, KeyCode::Char('3'));
    assert_eq!(answer_of(&mut rx), Some(("q2".into(), Some("blue".into()))));

    // the last row takes an answer of one's own
    a.handle_agent(AgentEvent::Question { question: question("q3", false, None) });
    press(&mut a, KeyCode::Char('4'));
    for c in "teal".chars() {
        press(&mut a, KeyCode::Char(c));
    }
    press(&mut a, KeyCode::Enter);
    assert_eq!(answer_of(&mut rx), Some(("q3".into(), Some("teal".into()))));

    // Esc closes it without an answer
    a.handle_agent(AgentEvent::Question { question: question("q4", false, None) });
    press(&mut a, KeyCode::Esc);
    assert_eq!(answer_of(&mut rx), Some(("q4".into(), None)));
}

#[test]
fn several_options_are_ticked_and_sent_together() {
    let (mut a, mut rx) = app();
    a.handle_agent(AgentEvent::Question { question: question("m", true, None) });
    press(&mut a, KeyCode::Char(' '));
    press(&mut a, KeyCode::Down);
    press(&mut a, KeyCode::Down);
    press(&mut a, KeyCode::Char(' '));
    assert!(a.modal.is_some(), "a tick does not send");
    press(&mut a, KeyCode::Enter);
    assert_eq!(answer_of(&mut rx), Some(("m".into(), Some("red, blue".into()))));
}

#[test]
fn questions_wait_behind_an_open_modal_and_go_when_resolved_elsewhere() {
    let (mut a, _rx) = app();
    a.handle_agent(AgentEvent::Question { question: question("a", false, None) });
    a.handle_agent(AgentEvent::Question { question: question("b", false, None) });
    a.handle_agent(AgentEvent::QuestionResolved { id: "a".into(), answer: None });
    assert!(matches!(&a.modal, Some(Modal::Question(q)) if q.q.id == "b"));
    a.handle_agent(AgentEvent::QuestionResolved { id: "b".into(), answer: None });
    assert!(a.modal.is_none());
}

#[test]
fn a_plan_shows_as_markdown_above_its_options() {
    let (mut a, _rx) = app();
    a.handle_agent(AgentEvent::Question { question: question("p", false, Some("# Plan\n\n1. **read** the parser")) });
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let screen = crate::ui::rows_of(term.backend().buffer()).join("\n");
    assert!(screen.contains(" plan "), "{screen}");
    assert!(screen.contains("read the parser") && !screen.contains("**read**"), "{screen}");
    assert!(screen.contains("❯ 1. red") && screen.contains("Tell the model what to change"), "{screen}");
    assert_eq!(a.picker_rows.len(), 4, "three options and the row of one's own");
}

#[test]
fn a_question_and_its_options_read_as_markdown() {
    let (mut a, _rx) = app();
    let mut q = question("m", false, None);
    q.question = "## Which **fast** parser?".into();
    q.options[0].label = "**Fast** `nom`".into();
    q.options[0].description = "uses *less* memory".into();
    a.handle_agent(AgentEvent::Question { question: q });
    screen(&mut a, 100, 44); // the dialog pops in on this frame
    a.fx.clock = Some(a.now() + std::time::Duration::from_secs(2));
    let screen = screen(&mut a, 100, 44).join("\n");
    assert!(!screen.contains("##") && !screen.contains("**") && !screen.contains('`'), "{screen}");
    assert!(screen.contains("Which fast parser?"), "{screen}");
    assert!(screen.contains("1. Fast nom") && screen.contains("uses less memory"), "{screen}");
}

#[test]
fn a_slash_word_is_known_unknown_or_still_being_typed() {
    use crate::ui::{slash_word, SlashWord};
    assert_eq!(slash_word("/plan add a cache", &[]), Some(("/plan", SlashWord::Known)));
    assert_eq!(slash_word("/pla", &[]), Some(("/pla", SlashWord::Typing)));
    assert_eq!(slash_word("/nosuch x", &[]), Some(("/nosuch", SlashWord::Unknown)));
    assert_eq!(slash_word("/etc/hosts is odd", &[]), None);
    assert_eq!(slash_word("hello /plan", &[]), None);
}

#[test]
fn plan_mode_shows_on_the_rule_and_a_known_command_is_coloured() {
    let (mut a, _rx) = app();
    a.handle_agent(AgentEvent::PermissionModeChanged { mode: PermissionMode::Plan });
    // past the wipe: the rule has its new colour and name
    a.fx.clock = Some(a.now() + std::time::Duration::from_secs(2));
    a.input.set_text("/plan x");
    let backend = ratatui::backend::TestBackend::new(100, 20);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let buf = term.backend().buffer().clone();
    let rows = crate::ui::rows_of(&buf);
    assert!(rows.iter().any(|r| r.contains("◇ plan")), "{}", rows.join("\n"));
    let e = a.editor_area;
    let cell = &buf[(e.x, e.y)];
    assert_eq!(cell.symbol(), "/");
    assert!(cell.modifier.contains(ratatui::style::Modifier::BOLD), "the command word is lit");
}

#[test]
fn shift_tab_walks_the_modes_through_plan_and_slash_plan_sends_the_text() {
    let (mut a, mut rx) = app();
    let mut modes = Vec::new();
    for m in [PermissionMode::Ask, PermissionMode::AcceptEdits, PermissionMode::Plan] {
        a.permission_mode = m;
        crate::event::key(&mut a, KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        if let Ok(AgentCommand::SetPermissionMode(n)) = rx.try_recv() {
            modes.push(n);
        }
    }
    assert_eq!(modes, [PermissionMode::AcceptEdits, PermissionMode::Plan, PermissionMode::Ask]);
    a.permission_mode = PermissionMode::Ask;
    a.input.set_text("/plan add a cache");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetPermissionMode(PermissionMode::Plan))));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. }) if text == "add a cache"));
}
