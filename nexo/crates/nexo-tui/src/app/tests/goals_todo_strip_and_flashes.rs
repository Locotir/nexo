//! Goal lines, the todo strip, history and status flashes.

use super::*;

#[test]
fn a_restated_goal_reads_as_what_the_person_wants() {
    let g = |text: &str| Goal { text: text.into(), round: 0, max: 5, state: GoalState::Active };
    assert_eq!(g("Want: green tests\nWhy: release\nHow: x\nDone when: y").headline(), "green tests");
    assert_eq!(g("ship it").headline(), "ship it");
    let mut a = app().0;
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("[goal set] tests pls") });
    a.handle_agent(AgentEvent::History { index: 2, message: Message::user("[goal: continue]") });
    let notices: Vec<&str> = a
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Notice { text, .. } => Some(text.as_str()),
            Block::User { .. } => panic!("a goal request of the loop shows as a user message"),
            _ => None,
        })
        .collect();
    assert_eq!(notices, ["◎ goal set: tests pls", "◎ goal · continuing"]);
    assert!(a.user_messages.is_empty(), "nothing of the loop to rewind to");
}

#[test]
fn a_long_goal_shows_in_a_few_lines_and_the_rest_waits_in_the_goal_window() {
    let (mut a, _rx) = app();
    let long = format!("make it fast {}", "and keep every test green ".repeat(40));
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user(format!("[goal set] {long}")) });
    let notice = a
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .expect("the goal shows as a notice");
    assert!(notice.starts_with("◎ goal set: make it fast and keep every test green"), "{notice}");
    assert!(notice.chars().count() < 260, "a few lines, not a wall: {}", notice.chars().count());
    assert!(notice.contains("goal window"), "{notice}");
}

#[test]
fn a_goal_round_shows_as_one_line_with_its_round() {
    let (mut a, _rx) = app();
    let goal =
        |round: u32| AgentEvent::Goal { text: Some("tests pls".into()), round, max: 5, state: GoalState::Active };
    a.handle_agent(goal(0));
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("[goal set] tests pls") });
    a.handle_agent(goal(1));
    a.handle_agent(AgentEvent::History { index: 2, message: Message::user("[goal: continue]") });
    let notices: Vec<&str> = a
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Notice { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(notices, ["◎ goal set: tests pls", "◎ goal · round 1/5 · continuing"]);
    assert!(!notices.iter().any(|n| n.contains("goal: continuing")));
}

#[test]
fn goal_and_rename_offer_what_they_hold_now_to_edit() {
    let (mut a, _rx) = app();
    a.input.set_text("/rename ");
    a.refresh_palette();
    assert!(a.palette.is_none(), "no name yet: nothing to offer");
    a.session_title = "fix the parser".into();
    a.goal = Some(Goal { text: "pass the tests".into(), round: 0, max: 5, state: GoalState::Active });
    a.input.set_text("/goal ");
    a.refresh_palette();
    let p = a.palette.as_ref().unwrap();
    assert_eq!((p.len(), p.current()), (4, Some("pass the tests")), "the goal now, then pause, resume and off");
    a.palette_accept(false);
    assert_eq!(a.input.text(), "/goal pass the tests");
    assert!(a.palette.is_none(), "free text from here on");
    a.input.set_text("/rename ");
    a.refresh_palette();
    a.palette_accept(false);
    assert_eq!(a.input.text(), "/rename fix the parser");
    a.session_title = "Fix: the parser, again!".into();
    a.input.set_text("/export ");
    a.refresh_palette();
    a.palette_accept(false);
    assert_eq!(a.input.text(), "/export fix-the-parser-again.md");
    // Enter on the bare command still shows the goal, it does not resend it
    a.input.set_text("/goal ");
    a.refresh_palette();
    a.palette_accept(true);
    assert!(matches!(a.blocks.last(), Some(Block::Notice { .. })));
}

#[test]
fn esc_twice_keeps_the_draft_in_history_like_ctrl_c() {
    let (mut a, _rx) = app();
    a.input.set_text("a long message");
    a.esc();
    assert_eq!(a.input.full_text(), "a long message", "the first Esc only hints");
    a.esc();
    assert!(a.input.is_empty());
    assert_eq!(a.input.history().last().map(String::as_str), Some("a long message"));
    assert!(a.status.as_ref().is_some_and(|s| s.0.contains("brings it back")), "{:?}", a.status);
}

/// `agent.close_on_verified`: the turn ends on the reply that holds the answer and its last checks. The text is
/// on screen as the answer, the check cards follow it, and the app is idle.
#[test]
fn a_turn_closed_on_a_reply_with_its_checks_shows_the_text_as_the_answer() {
    let (mut a, _rx) = bare();
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    let said = "Fixed the parser; tests pass.";
    a.handle_agent(AgentEvent::TextDelta { text: said.into() });
    a.handle_agent(AgentEvent::ToolCall {
        id: "c1".into(),
        name: "bash".into(),
        args: serde_json::json!({"command": "cargo test"}),
        summary: "cargo test".into(),
    });
    a.handle_agent(AgentEvent::ToolStart { id: "c1".into() });
    a.handle_agent(AgentEvent::ToolDone { id: "c1".into(), output: ToolOutput::ok("test result: ok"), elapsed_ms: 5 });
    a.handle_agent(AgentEvent::AssistantDone {
        message: Message::new(
            Role::Assistant,
            vec![Part::text(said), Part::tool_call("c1", "bash", serde_json::json!({"command": "cargo test"}))],
        ),
    });
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    let answers: Vec<&str> = a
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Assistant { text, streaming: false, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(answers, vec![said]);
    assert!(a.blocks.iter().any(|b| matches!(b, Block::Tool { status: ToolStatus::Done, .. })));
    assert!(!a.busy);
}

#[test]
fn a_saved_instruction_flashes_in_the_status_bar_and_its_card_stays_in_sight_in_a_folded_group() {
    let (mut a, _rx) = bare();
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    let call = |a: &mut App, id: &str, name: &str, out: ToolOutput| {
        a.handle_agent(AgentEvent::ToolCall {
            id: id.into(),
            name: name.into(),
            args: serde_json::json!({}),
            summary: name.into(),
        });
        a.handle_agent(AgentEvent::ToolStart { id: id.into() });
        a.handle_agent(AgentEvent::ToolDone { id: id.into(), output: out, elapsed_ms: 1 });
    };
    call(&mut a, "r1", "read", ToolOutput::ok("x"));
    let saved = ToolOutput::ok("noted 1 line; 1 new rule saved").with_display(ToolDisplay::Remembered {
        scope: "project".into(),
        items: vec!["tests run with `make check`".into()],
        path: "/p/AGENTS.md".into(),
    });
    call(&mut a, "n1", "note", saved);
    assert_eq!(
        a.status.as_ref().map(|s| s.0.as_str()),
        Some("✦ remembered (this project): tests run with `make check`")
    );
    let at =
        |a: &App, id: &str| a.blocks.iter().position(|b| matches!(b, Block::Tool { id: i, .. } if i == id)).unwrap();
    // the group of calls is folded: the read is out of sight, the saved instruction is not
    assert!(a.blocks.iter().any(|b| matches!(b, Block::Group { folded: true, .. })));
    assert!(a.hidden(at(&a, "r1")));
    assert!(!a.hidden(at(&a, "n1")));
    // a plain note (nothing saved) does not flash
    a.status = None;
    call(&mut a, "n2", "note", ToolOutput::ok("noted 1 line"));
    assert!(a.status.is_none());
}

#[test]
fn the_todo_strip_follows_the_list_and_the_turn() {
    use nexo_core::tool::{TodoItem, TodoStatus, ToolDisplay};
    let (mut a, _rx) = app();
    let list = |s: [TodoStatus; 2]| ToolDisplay::Todo {
        items: vec![TodoItem { text: "a".into(), status: s[0] }, TodoItem { text: "b".into(), status: s[1] }],
    };
    let send = |a: &mut App, id: &str, d: ToolDisplay| {
        a.handle_agent(AgentEvent::ToolCall {
            id: id.into(),
            name: "todo".into(),
            args: serde_json::json!({}),
            summary: "todo".into(),
        });
        a.handle_agent(AgentEvent::ToolDone {
            id: id.into(),
            output: ToolOutput::ok("x").with_display(d),
            elapsed_ms: 0,
        });
    };
    send(&mut a, "1", list([TodoStatus::InProgress, TodoStatus::Pending]));
    assert!(a.todo_strip_on());
    send(&mut a, "2", list([TodoStatus::Done, TodoStatus::InProgress]));
    assert!(
        matches!(a.blocks.iter().rev().find(|b| matches!(b, Block::Tool { .. })),
        Some(Block::Tool { summary, .. }) if summary == "todo ✓ a · ▸ b" || summary == "todo ✓ a"),
        "{:?}",
        a.blocks.last()
    );
    send(&mut a, "3", list([TodoStatus::Done, TodoStatus::Done]));
    assert!(a.todo_strip_on(), "a finished list stays until the turn ends");
    a.handle_agent(AgentEvent::TurnEnd { reason: nexo_core::agent::TurnEnd::Done });
    assert!(!a.todo_strip_on());
    a.handle_agent(AgentEvent::TurnStart { turn: 2 });
    assert!(!a.todo_strip_on(), "a stale finished list is not shown in a new turn");
}

#[test]
fn a_resumed_session_gets_its_unfinished_list_back() {
    let (mut a, _rx) = app();
    let m = Message::new(
        Role::Assistant,
        vec![Part::tool_call(
            "t1",
            "todo",
            serde_json::json!({"items": [{"text": "a", "status": "done"}, {"text": "b"}]}),
        )],
    );
    a.handle_agent(AgentEvent::History { index: 0, message: m });
    assert_eq!(a.todo.len(), 2);
    assert!(a.todo_strip_on());
}

#[test]
fn the_interrupting_flash_goes_when_the_turn_has_stopped() {
    let (mut a, _rx) = app();
    a.busy = true;
    a.ctrl_c();
    assert!(a.status.as_ref().is_some_and(|(t, _)| t.starts_with("interrupting")));
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Interrupted });
    assert!(a.status.is_none(), "{:?}", a.status);
}

#[test]
fn a_window_that_opens_drops_the_esc_again_hint_meant_for_the_draft() {
    let (mut a, _rx) = app();
    a.flash("Esc again to rewind to an earlier message");
    a.run_command(crate::commands::Command::Help);
    assert!(a.modal.is_some());
    a.note_popups();
    assert!(a.status.is_none(), "{:?}", a.status);
}
