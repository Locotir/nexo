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

    // typing anything hides it for good, even when the draft is emptied again
    a.input.clear();
    a.handle_agent(AgentEvent::Suggestion { text: "commit it".into() });
    press(&mut a, KeyCode::Char('x'));
    assert_eq!(a.input.text(), "x");
    assert_eq!(a.suggestion_shown(), None);
    a.input.clear();
    assert_eq!(a.suggestion, None);

    // a late suggestion is let go: a draft with text, a turn in progress
    a.input.set_text("hello");
    a.handle_agent(AgentEvent::Suggestion { text: "commit it".into() });
    assert_eq!(a.suggestion, None);
    a.input.clear();
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
    assert_eq!(a.suggestion, None);
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

#[test]
fn ctrl_click_on_a_link_opens_it_and_a_plain_click_does_not() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let click = |a: &mut App, x: u16, y: u16, modifiers: KeyModifiers| {
        for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
            crate::event::mouse(a, MouseEvent { kind, column: x, row: y, modifiers });
        }
    };
    let (mut a, _rx) = bare();
    a.push(Block::User { text: "see https://example.com/a. ok".into(), at: None });
    a.transcript_area = Rect::new(0, 1, 80, 20);
    a.layout(80);
    a.settle(20);
    let text = a.line_text(0).unwrap();
    let col = crate::select::width_of(&text[..text.find("https").unwrap()]) as u16;
    crate::links::take_opened();
    // a plain click on the link opens nothing; so does Ctrl+click beside it
    click(&mut a, col + 3, 1, KeyModifiers::NONE);
    click(&mut a, col - 2, 1, KeyModifiers::CONTROL);
    assert!(crate::links::take_opened().is_empty());
    assert_eq!(a.link_at(col + 3, 1).as_deref(), Some("https://example.com/a"));
    // Ctrl+click on it opens it
    click(&mut a, col + 3, 1, KeyModifiers::CONTROL);
    assert_eq!(crate::links::take_opened(), vec!["https://example.com/a".to_string()]);
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
