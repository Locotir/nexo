//! The goal window and what nexo added to a message.

use super::*;

fn goal_with_log(a: &mut App, state: GoalState) {
    use nexo_core::agent::{GoalLog, GoalStop};
    let now = nexo_core::mailbox::now_ms();
    a.handle_agent(AgentEvent::GoalLog(GoalLog {
        original: Some("tests green pls".into()),
        brief: Some(
            "Want: every test passes\nWhy: the release is blocked\nHow: fix the code\nDone when: 0 failed".into(),
        ),
        started_ms: now - 125_000,
        steps: 7,
        cost_usd: 0.0123,
        stops: vec![GoalStop { at_ms: now - 60_000, why: "interrupted".into() }],
        last: Some("Fixed the parser".into()),
    }));
    a.handle_agent(AgentEvent::Goal {
        text: Some(
            "Want: every test passes\nWhy: the release is blocked\nHow: fix the code\nDone when: 0 failed".into(),
        ),
        round: 2,
        max: 5,
        state,
    });
}

#[test]
fn a_click_on_the_goal_opens_its_window_and_esc_closes_it() {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    let (mut a, _rx) = app();
    a.opts.animations = false;
    goal_with_log(&mut a, GoalState::Active);
    frame(&mut a, 120, 50);
    let goal =
        a.status_hits.iter().find(|(_, h)| *h == StatusHit::Goal).map(|(r, _)| *r).expect("the goal is on the bar");
    pointer(&mut a, K::Moved, goal.x, goal.y);
    assert_eq!(a.hover, Hover::Status(StatusHit::Goal));
    pointer(&mut a, K::Down(MouseButton::Left), goal.x + 1, goal.y);
    assert!(matches!(a.modal, Some(Modal::Goal(_))), "the goal opens its window, not the context");
    let all = frame(&mut a, 120, 50).join("\n");
    for want in [
        "◎ goal",
        "round 2/5",
        "running 2 min",
        "7 steps",
        "you asked",
        "tests green pls",
        "restated",
        "Want: every test passes",
        "Fixed the parser",
        "interruptions (1)",
        "1 min ago · interrupted",
    ] {
        assert!(all.contains(want), "missing {want:?} in:\n{all}");
    }
    assert!(!all.contains("Resume"), "a goal at work has nothing to resume:\n{all}");
    press(&mut a, KeyCode::Esc);
    assert!(a.modal.is_none(), "Esc closes it");
    // with no goal there is nothing to click
    a.handle_agent(AgentEvent::Goal { text: None, round: 0, max: 5, state: Default::default() });
    frame(&mut a, 120, 50);
    assert!(a.status_hits.iter().all(|(_, h)| *h != StatusHit::Goal));
}

#[test]
fn the_goal_window_of_a_paused_goal_resumes_it_with_a_click_or_a_key() {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    let (mut a, mut rx) = app();
    a.opts.animations = false;
    goal_with_log(&mut a, GoalState::Paused("interrupted".into()));
    frame(&mut a, 120, 50);
    a.open_goal();
    let all = frame(&mut a, 120, 50).join("\n");
    assert!(all.contains("paused: interrupted") && all.contains("Resume"), "{all}");
    // the button is the row the pointer was given
    let button = a.picker_rows.first().map(|(r, _)| *r).expect("the Resume button is a row");
    pointer(&mut a, K::Down(MouseButton::Left), button.x + 2, button.y);
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ResumeGoal)));
    assert!(a.modal.is_none(), "the window closes");
    // the key does the same
    a.open_goal();
    frame(&mut a, 120, 50);
    press(&mut a, KeyCode::Char('r'));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ResumeGoal)));
    assert!(a.modal.is_none());
}

#[test]
fn the_goal_window_follows_the_agent_and_closes_when_the_goal_is_cleared() {
    let (mut a, _rx) = app();
    a.opts.animations = false;
    goal_with_log(&mut a, GoalState::Active);
    a.open_goal();
    frame(&mut a, 120, 30);
    goal_with_log(&mut a, GoalState::Paused("the turn failed".into()));
    let all = frame(&mut a, 120, 30).join("\n");
    assert!(all.contains("paused: the turn failed") && all.contains("Resume"), "{all}");
    a.handle_agent(AgentEvent::Goal { text: None, round: 0, max: 5, state: Default::default() });
    assert!(a.modal.is_none() && a.goal_log == Default::default());
}

#[test]
fn the_goal_window_gives_a_long_line_room_on_a_wide_terminal_and_stays_compact_on_a_small_one() {
    use nexo_core::agent::GoalLog;
    let long = "x".repeat(150);
    let open = |cols: u16| {
        let (mut a, _rx) = app();
        a.opts.animations = false;
        goal_with_log(&mut a, GoalState::Active);
        a.handle_agent(AgentEvent::GoalLog(GoalLog { original: Some(long.clone()), ..a.goal_log.clone() }));
        a.open_goal();
        frame(&mut a, cols, 50);
        a.dialog_size.modal.drawn.width
    };
    assert_eq!(open(100), 96, "a small terminal keeps today's window");
    let wide = open(240);
    assert!(wide > 96 && wide <= 160, "a wide one grows it, up to two thirds: {wide}");
    assert!(open(200) <= wide, "and it scales with the terminal");
}

#[test]
fn the_goal_window_grows_when_the_goal_gets_more_to_say() {
    use nexo_core::agent::GoalStop;
    let (mut a, _rx) = app();
    a.opts.animations = false;
    goal_with_log(&mut a, GoalState::Active);
    a.open_goal();
    frame(&mut a, 120, 50);
    let before = a.dialog_size.modal.drawn.height;
    let mut log = a.goal_log.clone();
    let now = nexo_core::mailbox::now_ms();
    log.stops.extend((0..4).map(|i| GoalStop { at_ms: now - 1000 * i, why: format!("the turn failed {i}") }));
    // the agent tells the log, then the goal: the open window follows
    a.handle_agent(AgentEvent::GoalLog(log));
    a.handle_agent(AgentEvent::Goal {
        text: Some("Want: every test passes".into()),
        round: 3,
        max: 5,
        state: GoalState::Active,
    });
    frame(&mut a, 120, 50);
    assert!(a.dialog_size.modal.drawn.height > before, "the new stops are not left behind a scroll");
}

/// A user message and what nexo added to it, as the agent tells them.
fn said_with_additions(a: &mut App) {
    use nexo_core::agent::{AddedKind::*, AddedPart};
    let note = "[project at the start of this request]\nattached: 2 files";
    a.handle_agent(AgentEvent::History {
        index: 0,
        message: nexo_core::message::Message::user(format!("fix the bug\n\n{note}")),
    });
    let file =
        |p: &str| AddedPart::new(File, p, format!("@{p} (attached in full, as it is now):\n```rs\nfn a() {{}}\n```"));
    let long: String = (1..=20).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    a.handle_agent(AgentEvent::Added {
        index: 0,
        parts: vec![
            AddedPart::new(Hook, "hook context", "it is monday"),
            AddedPart::new(StartNote, "project", note),
            file("a.rs"),
            AddedPart::new(File, "b.rs", long),
        ],
    });
}

#[test]
fn a_line_under_the_message_says_what_nexo_added_and_opens_to_the_text() {
    for theme in ["dark", "light"] {
        let (mut a, _rx) = app();
        a.opts.animations = false;
        a.set_theme(theme, false);
        said_with_additions(&mut a);
        let all = frame(&mut a, 100, 40).join("\n");
        assert!(all.contains("+ nexo added · ") && all.contains("(hook, start note, 2 files)"), "{theme}: {all}");
        assert!(!all.contains("it is monday") && !all.contains("line 5"), "folded: only the line, {theme}: {all}");
        // the start note left the person's block: it is said in the line
        assert!(all.contains("fix the bug") && !all.contains("attached: 2 files"), "{theme}: {all}");
        // Enter on the selected line opens it: each text with its label, a long one cut
        let at = a.blocks.iter().position(|b| matches!(b, Block::Added { .. })).unwrap();
        a.toggle_block(at);
        let all = frame(&mut a, 100, 60).join("\n");
        for want in [
            "- nexo added",
            "hook context · hook",
            "it is monday",
            "project · start note",
            "b.rs · file",
            "line 12",
            "… 8 more lines",
        ] {
            assert!(all.contains(want), "{theme}: {want} in {all}");
        }
        assert!(!all.contains("line 13"), "{theme}: {all}");
        a.toggle_block(at);
        assert!(!frame(&mut a, 100, 40).join("\n").contains("it is monday"));
    }
}

#[test]
fn a_message_with_nothing_added_has_no_line_and_a_supervisor_note_says_it_rides_one_request() {
    let (mut a, _rx) = app();
    a.opts.animations = false;
    a.handle_agent(AgentEvent::History { index: 0, message: nexo_core::message::Message::user("just words") });
    a.handle_agent(AgentEvent::Added { index: 0, parts: vec![] });
    assert!(!a.blocks.iter().any(|b| matches!(b, Block::Added { .. })));
    assert!(!frame(&mut a, 100, 30).join("\n").contains("nexo added"));
    let mut p =
        nexo_core::agent::AddedPart::new(nexo_core::agent::AddedKind::Supervisor, "supervisor", "check the tests");
    p.request_only = true;
    a.handle_agent(AgentEvent::Added { index: 0, parts: vec![p] });
    let all = frame(&mut a, 100, 30).join("\n");
    assert!(all.contains("(supervisor note) · this request only"), "{all}");
    assert!(all.contains("just words"), "the person's block stays as it was");
}

#[test]
fn sent_asks_the_agent_and_the_answer_opens_a_window_with_the_sections() {
    use nexo_core::agent::SentSection;
    let (mut a, mut rx) = app();
    a.opts.animations = false;
    for cmd in ["/sent", "/request"] {
        a.input.set_text(cmd);
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShowSent)), "{cmd}");
    }
    let sec = |h: &str, depth: u8, tag: Option<&str>, text: &str| SentSection {
        heading: h.into(),
        depth,
        tag: tag.map(str::to_string),
        tokens: 12,
        text: text.into(),
        collapse: false,
    };
    a.handle_agent(AgentEvent::Sent {
        model: "m1".into(),
        sections: vec![
            sec("System prompt", 0, None, ""),
            sec("instruction files", 1, None, "Use tabs."),
            sec("#1 user", 0, None, "fix it"),
            sec("#1 user · NUDGE", 0, Some("curator"), "[curator] Before the next step, note it"),
        ],
    });
    assert!(matches!(a.modal, Some(Modal::Sent(_))));
    let all = frame(&mut a, 120, 40).join("\n");
    for want in [
        "request · m1 · ~",
        "System prompt",
        "instruction files",
        "Use tabs.",
        "#1 user · NUDGE",
        "[curator]",
        "search",
    ] {
        assert!(all.contains(want), "{want} in {all}");
    }
    press(&mut a, KeyCode::Esc);
    assert!(a.modal.is_none());
}

/// A resumed session's message as it was stored: the words, the start note and a file, all in one text.
fn stored_with_additions(a: &mut App) {
    let t = "Hola cómo estás?\n\n[project at the start of this request]\nattached: 1 files, 1883 tokens\n\n\
             @1-builtin-instructions.md (attached in full, as it is now):\n```md\nuno\ndos\ntres\n```";
    a.handle_agent(AgentEvent::History { index: 0, message: nexo_core::message::Message::user(t) });
}

#[test]
fn the_view_shows_only_what_the_person_typed_until_the_additions_are_opened() {
    for theme in ["dark", "light"] {
        // live: the words as typed, what nexo added told as it was built
        let (mut live, _rx) = app();
        live.opts.animations = false;
        live.set_theme(theme, false);
        said_with_additions(&mut live);
        // resumed: the stored message holds all of it in one text, split at its markers
        let (mut old, _rx) = app();
        old.opts.animations = false;
        old.set_theme(theme, false);
        stored_with_additions(&mut old);
        for (what, a) in [("live", &mut live), ("resumed", &mut old)] {
            let all = frame(a, 100, 40).join("\n");
            assert!(all.contains("+ nexo added · ") && all.contains("start note"), "{what} {theme}: {all}");
            for hidden in ["project at the start", "attached: ", "builtin-instructions", "uno"] {
                assert!(!all.contains(hidden), "{what} {theme}: {hidden:?} shows by default: {all}");
            }
            let words = if what == "live" { "fix the bug" } else { "Hola cómo estás?" };
            let Some(Block::User { text, .. }) = a.blocks.iter().find(|b| matches!(b, Block::User { .. })) else {
                panic!("no message")
            };
            assert_eq!(text, words, "{what} {theme}: the message holds only the typed words");
        }
        let all = frame(&mut old, 100, 40).join("\n");
        assert!(all.contains("(start note, file)"), "{theme}: {all}");
    }
}

#[test]
fn right_opens_what_was_added_to_the_selected_message_and_left_folds_it_back() {
    for theme in ["dark", "light"] {
        let (mut a, _rx) = app();
        a.opts.animations = false;
        a.set_theme(theme, false);
        stored_with_additions(&mut a);
        frame(&mut a, 100, 40);
        let user = a.blocks.iter().position(|b| matches!(b, Block::User { .. })).unwrap();
        // a one-line message with something added can be selected: it has a line to open
        a.selected = Some(user);
        press(&mut a, KeyCode::Right);
        let all = frame(&mut a, 100, 60).join("\n");
        for want in [
            "- nexo added",
            "project at the start of the request · start note",
            "attached: 1 files, 1883 tokens",
            "1-builtin-instructions.md · file",
            "uno",
        ] {
            assert!(all.contains(want), "{theme}: {want} in {all}");
        }
        assert_eq!(a.selected, Some(user), "the message stays selected");
        press(&mut a, KeyCode::Right);
        assert!(
            frame(&mut a, 100, 60).join("\n").contains("attached: 1 files, 1883 tokens"),
            "→ on an open one keeps it"
        );
        press(&mut a, KeyCode::Left);
        let all = frame(&mut a, 100, 60).join("\n");
        assert!(all.contains("+ nexo added") && !all.contains("attached: 1 files") && !all.contains("uno"), "{all}");
        assert!(all.contains("Hola cómo estás?"));
    }
}

#[test]
fn a_double_click_on_the_message_opens_what_was_added_and_another_folds_it() {
    let (mut a, _rx) = app();
    a.opts.animations = false;
    stored_with_additions(&mut a);
    a.transcript_area = Rect::new(0, 1, 100, 30);
    a.layout(100);
    a.settle(30);
    let user = a.blocks.iter().position(|b| matches!(b, Block::User { .. })).unwrap();
    let added = a.companion(user).expect("a line under the message");
    let row = |a: &App, i: usize| (a.offsets[i] - a.scroll) as u16 + 1;
    let open = |a: &App| matches!(a.blocks[added], Block::Added { folded: false, .. });
    // one click only selects
    let r = row(&a, user);
    a.press_at(4, r);
    a.release_at(4, r);
    assert_eq!(a.selected, Some(user));
    assert!(!open(&a), "one click only selects");
    // the second, at once, opens it, and the word under it is not what it selects
    a.press_at(4, r);
    a.release_at(4, r);
    assert!(open(&a));
    assert!(a.selection.is_none());
    a.layout(100);
    a.settle(30);
    // a double click on the line of the additions folds it back
    std::thread::sleep(std::time::Duration::from_millis(450));
    let r = row(&a, added);
    a.press_at(4, r);
    a.release_at(4, r);
    assert!(open(&a), "a single click leaves it as it is");
    a.press_at(4, r);
    a.release_at(4, r);
    assert!(!open(&a));
}
