use super::*;

#[test]
fn the_argument_list_offers_running_jobs_and_deleted_sessions() {
    use nexo_core::jobs::{JobInfo, JobKind, JobStatus};
    let (mut a, mut rx) = app();
    a.handle_agent(AgentEvent::Jobs {
        jobs: vec![JobInfo {
            id: 7,
            kind: JobKind::Shell,
            label: "cargo build".into(),
            status: JobStatus::Running,
            elapsed_ms: 0,
            lines: 0,
        }],
    });
    a.input.set_text("/jobs k");
    a.refresh_palette();
    let p = a.palette.as_ref().expect("the values of /jobs");
    assert_eq!(p.len(), 2, "kill (all) and kill 7");
    // down to the running job, Tab writes it, Enter stops that one only
    a.palette.as_mut().unwrap().selected = 1;
    a.palette_accept(false);
    assert_eq!(a.input.text(), "/jobs kill 7");
    a.refresh_palette();
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::KillJob { id: Some(7) })));
    // nothing deleted in this run: no list, the line is free text
    a.input.set_text("/undelete ");
    a.refresh_palette();
    assert!(a.palette.is_none());
    a.handle_agent(AgentEvent::SessionDeleted { id: "s9".into(), title: "old".into() });
    a.input.set_text("/undelete ");
    a.refresh_palette();
    assert_eq!(a.palette.as_ref().and_then(|p| p.current()), Some("s9"));
    // /resume: a word of the title finds the session, Enter opens it
    a.opts.sessions = vec![("20261101-1".into(), "parser fix".into()), ("20261102-1".into(), "docs".into())];
    a.input.set_text("/resume pars");
    a.refresh_palette();
    assert_eq!(a.palette.as_ref().and_then(|p| p.current()), Some("20261101-1"));
    a.palette_accept(true);
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Resume { id }) if id == "20261101-1"));
    // typed whole, by title, without the list
    a.run_command(Command::Resume("Docs".into()));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Resume { id }) if id == "20261102-1"));
    a.run_command(Command::Resume("nope".into()));
    assert!(rx.try_recv().is_err(), "nothing sent for an unknown session");
    assert!(matches!(a.blocks.last(), Some(Block::Notice { .. })));
}

#[test]
fn a_session_is_deleted_with_two_presses_and_comes_back_with_undelete() {
    let (mut a, mut rx) = app();
    a.opts.sessions = vec![("s1".into(), "first".into()), ("s2".into(), "second".into())];
    a.run_command(Command::Sessions);
    press(&mut a, KeyCode::Delete);
    assert!(rx.try_recv().is_err(), "one press only marks the row");
    let Some(Modal::Picker(p)) = &a.modal else { panic!("the picker stays") };
    assert_eq!(p.armed.as_deref(), Some("s1"));
    let rows = frame(&mut a, 70, 16).join("\n");
    assert!(rows.contains("Del again deletes") && rows.contains("· 2 · Del deletes"), "{rows}");
    press(&mut a, KeyCode::Esc);
    assert!(a.modal.is_some(), "Esc first unmarks, the picker stays");
    press(&mut a, KeyCode::Delete);
    press(&mut a, KeyCode::Delete);
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::DeleteSession { id }) if id == "s1"));
    let Some(Modal::Picker(p)) = &a.modal else { panic!("the picker stays open for more") };
    assert_eq!(p.current().map(|i| i.value.as_str()), Some("s2"), "the next row takes its place");
    a.handle_agent(AgentEvent::SessionDeleted { id: "s1".into(), title: "first".into() });
    assert_eq!(a.opts.sessions.len(), 1);
    assert_eq!(a.trashed, ["s1"]);
    a.modal = None;
    a.run_command(Command::Undelete(None));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::RestoreSession { id }) if id == "s1"));
    a.handle_agent(AgentEvent::SessionRestored { id: "s1".into(), title: "first".into() });
    assert_eq!(a.opts.sessions[0].0, "s1");
    assert!(a.trashed.is_empty());
    a.run_command(Command::Undelete(None));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ListTrash)), "nothing from this run: ask what the trash holds");
    a.handle_agent(AgentEvent::Trash { sessions: vec![] });
    assert!(a.modal.is_none());
    a.handle_agent(AgentEvent::Trash { sessions: vec![("old".into(), "an old one".into())] });
    let rows = frame(&mut a, 70, 16).join("\n");
    assert!(rows.contains("Trash · 1") && rows.contains("an old one"), "{rows}");
    press(&mut a, KeyCode::Enter);
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::RestoreSession { id }) if id == "old"));
    // other pickers never delete
    a.modal = Some(Modal::Picker(Picker::new(
        PickKind::Model,
        "Model",
        vec![PickItem { label: "m".into(), value: "m".into(), hint: String::new() }],
        None,
    )));
    press(&mut a, KeyCode::Delete);
    press(&mut a, KeyCode::Delete);
    assert!(rx.try_recv().is_err());
}

#[test]
fn mem_shares_another_sessions_memory_and_shows_it_waiting() {
    let (mut a, mut rx) = app();
    a.session_id = Some("here".into());
    a.opts.sessions = vec![("here".into(), "this one".into()), ("s1".into(), "the parser".into())];
    a.input.set_text("/shared-memory");
    a.submit();
    let Some(Modal::Picker(p)) = &a.modal else { panic!("a picker") };
    assert_eq!(p.items.len(), 1, "this session is not offered");
    press(&mut a, KeyCode::Enter);
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShareMemory { id: Some(i), focus: None }) if i == "s1"));
    a.handle_agent(AgentEvent::MemoryShared { pending: vec![("s1".into(), "the parser".into(), 2100)] });
    assert!(crate::ui::hint_right_text(&a).contains("⊕ mem 1 · 2.1k"), "{}", crate::ui::hint_right_text(&a));
    let rows = frame(&mut a, 120, 20).join("\n");
    assert!(rows.contains("what «the parser» knows goes with your next message"), "{rows}");
    a.input.set_text("/shared-memory s1 the tokenizer tabs");
    a.submit();
    assert!(
        matches!(rx.try_recv(), Ok(AgentCommand::ShareMemory { id: Some(i), focus: Some(f) }) if i == "s1" && f == "the tokenizer tabs")
    );
    a.input.set_text("/shared-memory off");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShareMemory { id: None, .. })));
    a.handle_agent(AgentEvent::MemoryShared { pending: vec![] });
    assert!(!crate::ui::hint_right_text(&a).contains("mem"));
}

#[test]
fn changed_files_add_up_newest_first() {
    let (mut a, _rx) = app();
    let edit = |id: &str, path: &str, old: &str, new: &str| {
        (
            AgentEvent::ToolCall {
                id: id.into(),
                name: "edit".into(),
                args: serde_json::json!({"path": path}),
                summary: path.into(),
            },
            AgentEvent::ToolDone {
                id: id.into(),
                output: ToolOutput::ok("edited").with_display(ToolDisplay::Diff {
                    path: path.into(),
                    old: old.into(),
                    new: new.into(),
                    start_line: 1,
                }),
                elapsed_ms: 1,
            },
        )
    };
    for (c, d) in [
        edit("e1", "src/a.rs", "x\n", "y\nz\n"),
        edit("e2", "src/b.rs", "1\n2\n", "1\n"),
        edit("e3", "src/a.rs", "y\n", "w\n"),
    ] {
        a.handle_agent(c);
        a.handle_agent(d);
    }
    assert_eq!(
        a.changed,
        vec![
            ChangedFile { path: "src/a.rs".into(), added: 3, removed: 2 },
            ChangedFile { path: "src/b.rs".into(), added: 0, removed: 1 }
        ]
    );
    // replayed history carries only the text of the result
    let m = Message::new(Role::Assistant, vec![Part::tool_call("h1", "edit", serde_json::json!({"path": "src/c.rs"}))]);
    a.handle_agent(AgentEvent::History { index: 5, message: m });
    a.handle_agent(AgentEvent::History {
        index: 6,
        message: Message::tool_results(vec![Part::tool_result("h1", "edited src/c.rs: +4 −1 lines", false)]),
    });
    assert_eq!(a.changed[0], ChangedFile { path: "src/c.rs".into(), added: 4, removed: 1 });
    assert_eq!(parse_counts("edited a: +12 -3 lines"), Some((12, 3)));
    assert_eq!(parse_counts("no counts here"), None);
    a.toggle_files(80);
    assert!(!a.show_files, "too narrow");
    a.toggle_files(120);
    assert!(a.show_files);
}

#[test]
fn a_drag_selects_and_a_click_toggles() {
    let (mut a, _rx) = bare();
    for i in 0..5 {
        a.push(Block::User { text: format!("message {i}"), at: None });
    }
    let out = ToolOutput::ok((1..=10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"));
    a.push(Block::Tool {
        id: "t".into(),
        name: "bash".into(),
        summary: "ls".into(),
        args: serde_json::Value::Null,
        status: ToolStatus::Done,
        progress: vec![],
        output: Some(out),
        started: None,
        elapsed_ms: None,
        folded: true,
    });
    a.transcript_area = Rect::new(0, 1, 80, 20);
    a.layout(80);
    a.settle(20);
    // a click on the tool card selects it; a second click on its head opens it
    let card_line = a.offsets[5];
    let row = (card_line - a.scroll) as u16 + 1;
    a.press_at(3, row);
    a.release_at(3, row);
    assert_eq!(a.selected, Some(5));
    assert!(matches!(a.blocks[5], Block::Tool { folded: true, .. }), "one click only selects");
    a.press_at(3, row);
    a.release_at(3, row);
    assert!(matches!(a.blocks[5], Block::Tool { folded: false, .. }));
    a.layout(80);
    a.settle(20);
    // a click on its body keeps it selected and open; one on another block selects that
    let body = (a.offsets[5] + 3 - a.scroll) as u16 + 1;
    a.press_at(6, body);
    a.release_at(6, body);
    assert!(matches!(a.blocks[5], Block::Tool { folded: false, .. }));
    let first = (a.offsets[4] - a.scroll) as u16 + 1;
    a.press_at(4, first);
    a.release_at(4, first);
    assert_eq!(a.selected, Some(4));
    a.esc();
    assert_eq!(a.selected, None, "Esc lets the block go");
    // a drag from the first message to the second selects both lines
    a.press_at(2, 1);
    a.drag_to(5, 4);
    let sel = a.selection.clone().unwrap();
    assert_eq!(sel.range(), (Pos { line: 0, col: 2 }, Pos { line: 3, col: 5 }));
    assert_eq!(a.line_text(0).as_deref(), Some(" ▌ message 0"));
    a.release_at(5, 4);
    assert!(a.selection.as_ref().is_some_and(|s| !s.dragging), "the selection stays until Esc");
    a.esc();
    assert!(a.selection.is_none());
}

#[test]
fn the_arrows_walk_the_blocks_and_open_and_fold_them() {
    let (mut a, _rx) = bare();
    a.opts.group_tools = false;
    a.input.push_history("an earlier request");
    a.push(Block::User { text: "run them".into(), at: None });
    for id in ["a", "b", "c"] {
        a.push(card(id, 30));
    }
    a.push(Block::User { text: "thanks".into(), at: None });
    a.transcript_area = Rect::new(0, 1, 80, 4);
    a.layout(80);
    a.settle(4);
    // ↑ on an empty draft is still the history
    press(&mut a, KeyCode::Up);
    assert_eq!(a.input.full_text(), "an earlier request");
    assert_eq!(a.selected, None);
    a.input.clear();
    // Shift+↑ steps into the blocks at the last one that folds (the one-line message does not)
    crate::event::key(&mut a, KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
    assert_eq!(a.selected, Some(3));
    press(&mut a, KeyCode::Up);
    assert_eq!(a.selected, Some(2));
    // → opens it, and the view keeps it in sight
    press(&mut a, KeyCode::Right);
    assert!(matches!(a.blocks[2], Block::Tool { folded: false, .. }));
    a.layout(80);
    a.reveal_step();
    assert_eq!(a.scroll, a.offsets[2], "taller than the view: its head at the top");
    assert!(!a.follow);
    press(&mut a, KeyCode::Right);
    assert!(matches!(a.blocks[2], Block::Tool { folded: false, .. }), "→ on an open block keeps it open");
    // ← folds it
    press(&mut a, KeyCode::Left);
    assert!(matches!(a.blocks[2], Block::Tool { folded: true, .. }));
    // ↑ stops at the first; ↓ walks down and past the last leaves for the draft
    press(&mut a, KeyCode::Up);
    press(&mut a, KeyCode::Up);
    assert_eq!(a.selected, Some(1));
    a.scroll_by(-1000);
    assert!(!a.follow);
    press(&mut a, KeyCode::Down);
    press(&mut a, KeyCode::Down);
    assert_eq!(a.selected, Some(3));
    press(&mut a, KeyCode::Down);
    assert_eq!(a.selected, None);
    assert!(a.follow, "back at the draft, the view follows the end again");
    // Enter toggles the selected one; typing lets it go and goes to the draft
    crate::event::key(&mut a, KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
    press(&mut a, KeyCode::Enter);
    assert!(matches!(a.blocks[3], Block::Tool { folded: false, .. }));
    press(&mut a, KeyCode::Char('x'));
    assert_eq!(a.selected, None);
    assert_eq!(a.input.full_text(), "x");
    // with a draft, the arrows are the editor's
    press(&mut a, KeyCode::Left);
    assert!(matches!(a.blocks[3], Block::Tool { folded: false, .. }));
}

#[test]
fn the_steps_of_a_turn_sit_under_the_reply_and_one_line_steps_read_as_a_list() {
    let (mut a, _rx) = bare();
    a.opts.group_tools = false;
    a.push(Block::User { text: "go".into(), at: None });
    a.push(Block::Assistant { text: "On it.".into(), streaming: false, at: None, started: None, took_ms: None });
    a.push(card("a", 2));
    a.push(card("b", 2));
    if let Block::Tool { folded, .. } = &mut a.blocks[3] {
        *folded = false;
    }
    a.push(Block::User { text: "next".into(), at: None });
    a.layout(80);
    let text = |a: &mut App, i: usize| -> Vec<String> {
        a.lines(i, 80).iter().map(crate::render::markdown::text_of).collect()
    };
    assert_eq!(text(&mut a, 0), vec![" ▌ go"], "a margin of one column");
    assert_eq!(text(&mut a, 1), vec![" ◆ On it."]);
    let sh = nexo_core::shell::shell().label();
    assert!(text(&mut a, 2)[0].starts_with(&format!("   ✓ {sh} run a")), "{:?}", text(&mut a, 2));
    let open = text(&mut a, 3);
    assert!(open[0].starts_with(&format!("   ✓ {sh} run b")), "{open:?}");
    assert_eq!(open[1], "   │ b line 1", "the result under its call, behind a guide");
    assert_eq!(open[2], "   └ b line 2", "and the guide ends in a corner");
    // no blank line between a one-line step and the next; one after the open card; two
    // before the next message of the person
    assert_eq!(a.offsets[3], a.offsets[2] + 1);
    assert_eq!(a.offsets[4], a.offsets[3] + open.len() + 2);
    // a narrow terminal keeps every column for the text
    let narrow: Vec<String> = a.lines(3, 20).iter().map(crate::render::markdown::text_of).collect();
    assert!(narrow[0].starts_with(&format!("✓ {sh}")), "{narrow:?}");
}

#[test]
fn the_banner_stays_on_top_of_the_transcript_and_facts_follow_the_agent() {
    let (mut a, _rx) = app();
    assert!(a.has_welcome());
    a.handle_agent(AgentEvent::Notice { level: Level::Info, text: "hello".into() });
    assert!(a.has_welcome(), "a notice sits under the banner");
    a.handle_agent(AgentEvent::PermissionModeChanged { mode: PermissionMode::Auto });
    assert!(matches!(&a.blocks[0], Block::Welcome { permission_mode, .. } if permission_mode == "yolo"));
    let bar = crate::ui::title_segments(&a, 200);
    let yolo = bar.iter().flatten().find(|s| s.text == "▲ YOLO").expect("the mode that never asks reads YOLO");
    assert_eq!(yolo.style.fg, Some(a.theme.error), "and is red");
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("go") });
    assert!(a.has_welcome(), "the banner stays and scrolls away with the transcript");
    assert!(matches!(&a.blocks[1], Block::Notice { .. }) && !a.fresh());
    a.handle_agent(AgentEvent::HistoryCleared);
    assert!(a.has_welcome());
}

#[test]
fn a_todo_result_fills_the_list_and_the_goal_follows_the_agent() {
    use nexo_core::tool::TodoStatus;
    let (mut a, mut rx) = app();
    let items = vec![
        TodoItem { text: "read".into(), status: TodoStatus::Done },
        TodoItem { text: "fix".into(), status: TodoStatus::InProgress },
    ];
    a.handle_agent(AgentEvent::ToolCall {
        id: "t1".into(),
        name: "todo".into(),
        args: serde_json::json!({}),
        summary: "2 tasks".into(),
    });
    a.handle_agent(AgentEvent::ToolDone {
        id: "t1".into(),
        output: ToolOutput::ok("ok").with_display(ToolDisplay::Todo { items: items.clone() }),
        elapsed_ms: 1,
    });
    assert_eq!(a.todo, items);
    assert_eq!(crate::render::blocks::todo_counts(&a.todo), (1, 2));
    a.run_command(Command::Goal(GoalArg::Set("pass the tests".into())));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetGoal { text: Some(t) }) if t == "pass the tests"));
    a.handle_agent(AgentEvent::Goal {
        text: Some("pass the tests".into()),
        round: 0,
        max: 3,
        state: Default::default(),
    });
    assert_eq!(a.goal.as_ref().map(|g| g.text.as_str()), Some("pass the tests"));
    a.run_command(Command::Goal(GoalArg::Show));
    assert!(matches!(a.blocks.last(), Some(Block::Notice { text, .. }) if text.contains("pass the tests")));
    a.handle_agent(AgentEvent::Goal {
        text: Some("pass the tests".into()),
        round: 2,
        max: 3,
        state: Default::default(),
    });
    assert_eq!(a.status.as_ref().map(|s| s.0.clone()), Some("goal: continuing, round 2/3".into()));
    // the agent stops for an answer, then pauses: each change is said, and /goal shows why
    let goal = |state: GoalState| AgentEvent::Goal { text: Some("pass the tests".into()), round: 0, max: 3, state };
    a.handle_agent(goal(GoalState::Waiting));
    assert_eq!(a.status.as_ref().map(|s| s.0.clone()), Some("goal: waiting for your answer".into()));
    a.handle_agent(goal(GoalState::Paused("interrupted".into())));
    assert_eq!(a.status.as_ref().map(|s| s.0.clone()), Some("goal paused · /goal resume".into()));
    a.run_command(Command::Goal(GoalArg::Show));
    assert!(
        matches!(a.blocks.last(), Some(Block::Notice { text, .. }) if text.contains("paused (interrupted) · /goal resume"))
    );
    a.run_command(Command::Goal(GoalArg::Resume));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ResumeGoal)));
    a.run_command(Command::Goal(GoalArg::Pause));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::PauseGoal)));
    a.handle_agent(goal(GoalState::Done));
    assert_eq!(a.status.as_ref().map(|s| s.0.clone()), Some("goal met ✓".into()));
    a.run_command(Command::Goal(GoalArg::Clear));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetGoal { text: None })));
    a.handle_agent(AgentEvent::Goal { text: None, round: 0, max: 3, state: Default::default() });
    assert!(a.goal.is_none());
    a.handle_agent(AgentEvent::HistoryCleared);
    assert!(a.todo.is_empty(), "a new transcript starts without a list");
}

#[test]
fn a_cut_of_the_view_lights_the_curator_label_and_goes_away() {
    let (mut a, _rx) = app();
    let t0 = a.opened;
    a.fx.clock = Some(t0);
    let ctx = |view: u64, history: u64| AgentEvent::Context {
        view_tokens: view,
        history_tokens: history,
        history_bytes: history * 4,
        view_bytes: view * 4,
        messages_in_view: 1,
        messages_total: 1,
        policy: "curator".into(),
        note: None,
        label: "curator".into(),
        detail: None,
    };
    let bar = |a: &App| crate::ui::title_segments(a, 200).iter().flatten().map(|s| s.text.clone()).collect::<String>();
    a.handle_agent(ctx(5000, 5000));
    a.handle_agent(ctx(5200, 5400));
    assert!(a.fx.cut.is_none(), "a view that grew is no cut");
    a.handle_agent(ctx(2000, 5600));
    assert_eq!(a.fx.cut.map(|c| c.0), Some(3200));
    // the label says what the curator did (the agent writes the figure in it); the bar repeats no number
    let lit = |a: &App| {
        let segs = crate::ui::title_segments(a, 200);
        let label = segs.iter().flatten().find(|s| s.text.contains("curator")).expect("the curator label");
        label.style.add_modifier.contains(ratatui::style::Modifier::BOLD)
    };
    assert!(lit(&a), "{}", bar(&a));
    assert!(!bar(&a).contains("−3.2k"), "{}", bar(&a));
    assert!(a.tick_fx(), "the light keeps the frames coming until it is gone");
    a.fx.clock = Some(t0 + std::time::Duration::from_millis(crate::motion::CUT_MS + 200));
    assert!(!lit(&a), "{}", bar(&a));
    // a rewind takes the history down too: not the curator
    a.fx.cut = None;
    a.handle_agent(ctx(500, 1000));
    assert!(a.fx.cut.is_none());
}

#[test]
fn the_gauge_uses_the_last_call_and_the_window() {
    let (mut a, _rx) = app();
    assert_eq!(a.gauge(), None);
    a.handle_agent(AgentEvent::Context {
        view_tokens: 400,
        history_tokens: 1000,
        history_bytes: 4000,
        view_bytes: 1600,
        messages_in_view: 1,
        messages_total: 1,
        policy: "plain".into(),
        note: None,
        label: "plain".into(),
        detail: None,
    });
    assert_eq!(a.gauge(), None, "no window known: no share");
    assert_eq!(a.ctx_tokens(), Some(400));
    a.handle_agent(AgentEvent::ModelLimits {
        provider: "p".into(),
        model: "m".into(),
        pricing: None,
        context_window: Some(1000),
        max_output: None,
        source: "config".into(),
    });
    assert_eq!(a.gauge(), Some((400, 1000)), "no usage yet: the view over the window");
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    let u = |i: u64| AgentEvent::Usage {
        turn: Usage { input: i, ..Default::default() },
        call: Usage { input: i, ..Default::default() },
        session: Usage { input: i, ..Default::default() },
        cost_usd: 0.0,
    };
    a.handle_agent(u(1000));
    a.handle_agent(u(2500));
    assert_eq!(a.last_call_input, 2500, "each event is one call: the last one is what was sent");
    assert_eq!(a.turn_usage.input, 3500, "the turn adds its calls up");
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("go") });
    a.handle_agent(AgentEvent::ModelLimits {
        provider: "p".into(),
        model: "m".into(),
        pricing: None,
        context_window: Some(128_000),
        max_output: None,
        source: "preset".into(),
    });
    assert_eq!(a.gauge(), Some((2500, 128_000)));
    assert!(matches!(a.blocks.last(), Some(Block::Model { .. })));
    a.handle_agent(AgentEvent::TurnStart { turn: 2 });
    a.handle_agent(u(3000));
    assert_eq!(a.last_call_input, 3000);
    // a call the client retried: the turn counts both attempts, the gauge the one that answered
    a.handle_agent(AgentEvent::Usage {
        turn: Usage { input: 5000, ..Default::default() },
        call: Usage { input: 2000, ..Default::default() },
        session: Usage::default(),
        cost_usd: 0.0,
    });
    assert_eq!((a.last_call_input, a.call_inputs.last().copied()), (2000, Some(2000)));
    assert_eq!(a.turn_usage.input, 3000 + 5000, "the turn counts what was spent");
}

#[test]
fn pricing_parses_and_reaches_the_agent() {
    assert_eq!(
        parse_pricing("in=0.28 out=0.42 cache=0.028").unwrap(),
        Pricing { input: 0.28, output: 0.42, cache_read: 0.028, cache_write: 0.0 }
    );
    assert_eq!(parse_pricing("input=$1, output=2").unwrap().output, 2.0);
    assert!(parse_pricing("in=x").is_err());
    assert!(parse_pricing("").is_err());
    assert!(parse_pricing("foo=1").is_err());
    let (mut a, mut rx) = app();
    a.input.set_text("/pricing in=1 out=2");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetPricing { pricing }) if pricing.output == 2.0));
    a.input.set_text("/pricing");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShowModel)));
    a.input.set_text("/mouse");
    a.submit();
    assert_eq!(a.mouse_request, Some(false));
    assert!(!a.mouse_on);
}

#[test]
fn the_theme_picker_previews_and_restores() {
    let (mut a, _rx) = app();
    assert_eq!(a.theme.name, "dark");
    a.run_command(Command::Theme(None));
    if let Some(Modal::Picker(p)) = &mut a.modal {
        p.selected = crate::theme::NAMES.iter().position(|n| *n == "nord").unwrap();
    }
    a.modal_out(ModalOut::Nothing);
    assert_eq!(a.theme.name, "nord", "the hovered theme shows");
    a.modal_out(ModalOut::Close);
    assert_eq!(a.theme.name, "dark", "Esc restores");
    a.set_theme("gruvbox-dark", false);
    assert_eq!(a.theme.name, "gruvbox");
    a.set_theme("nope", false);
    assert!(matches!(a.blocks.last(), Some(Block::Notice { .. })));
}

#[test]
fn undo_init_status_tools_and_the_editor_key() {
    let (mut a, mut rx) = app();
    a.input.set_text("/undo");
    a.submit();
    assert!(rx.try_recv().is_err(), "nothing to undo yet");
    a.handle_agent(AgentEvent::History { index: 0, message: nexo_core::message::Message::user("make it blue") });
    a.input.set_text("/undo");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Rewind { index: 0 })));
    assert_eq!(a.input.text(), "make it blue", "the request comes back to the editor");
    a.input.set_text("/init");
    a.submit();
    // the session is named after the command first
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Rename { .. })));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. }) if text.contains("AGENTS.md")));
    a.input.set_text("/status");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShowStatus)));
    a.input.set_text("/tools");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShowTools)));
    crate::event::key(
        &mut a,
        crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('g'), crossterm::event::KeyModifiers::CONTROL),
    );
    assert!(a.draft_edit_request);
}

#[test]
fn a_command_of_ones_own_is_offered_and_sends_its_text() {
    let (mut a, mut rx) = app();
    a.custom_commands = vec![crate::custom::CustomCommand {
        name: "triage".into(),
        description: "review against our rules".into(),
        body: "Review $ARGUMENTS against CONTRIBUTING.md.".into(),
        path: PathBuf::from("triage.md"),
        hint: String::new(),
        allowed_tools: String::new(),
        origin: String::new(),
    }];
    a.input.set_text("/tri");
    a.refresh_palette();
    let p = a.palette.as_ref().expect("the palette");
    let (rows, _) = p.rows(20);
    let row = rows.iter().find(|r| r.label == "triage").expect("the command is offered");
    assert!(row.hint.contains("review against our rules") && row.hint.ends_with("· custom"));
    a.input.set_text("/triage src/lib.rs");
    a.submit();
    assert!(
        matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. }) if text == "Review src/lib.rs against CONTRIBUTING.md.")
    );
}

#[test]
fn a_skill_is_a_command_that_sends_its_instructions_and_is_listed() {
    let (mut a, mut rx) = app();
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("lint-fix");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), "---\nname: lint-fix\ndescription: fix lint\n---\nRun the linter.\n").unwrap();
    let skills = vec![nexo_core::skills::Skill {
        name: "lint-fix".into(),
        description: "fix lint".into(),
        dir,
        allowed_tools: vec![],
        source: ".claude/skills".into(),
    }];
    a.set_skills(skills);
    a.input.set_text("/lint");
    a.refresh_palette();
    let (rows, _) = a.palette.as_ref().expect("the palette").rows(20);
    assert!(rows.iter().any(|r| r.label == "lint-fix"));
    a.input.set_text("/lint-fix src");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. })
        if text.contains("Run the linter.") && text.ends_with("Arguments: src")));
    a.run_command(Command::Skills);
    let shown = format!("{:?}", a.blocks.last());
    assert!(shown.contains("/lint-fix") && shown.contains(".claude/skills"), "{shown}");
}

#[test]
fn ctrl_r_finds_an_earlier_request_and_the_history_outlives_the_run() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("prompt-history.jsonl");
    let (mut a, _rx) = app();
    a.input.persist_history(file.clone());
    for t in ["fix the parser", "add a test\nfor the parser", "run the suite"] {
        a.input.set_text(t);
        a.submit();
    }
    crate::event::key(
        &mut a,
        crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('r'), crossterm::event::KeyModifiers::CONTROL),
    );
    let Some(Modal::Picker(p)) = &a.modal else { panic!("a history picker") };
    assert_eq!(p.items[0].label, "run the suite", "newest first");
    assert_eq!(p.items[1].hint, "+1 lines");
    a.modal_out(ModalOut::Pick { kind: PickKind::History, value: "fix the parser".into() });
    assert_eq!(a.input.text(), "fix the parser");
    // a new run starts from the file
    let (mut b, _rx2) = app();
    b.input.persist_history(file);
    assert_eq!(b.input.history().len(), 3);
    assert_eq!(b.input.history()[1], "add a test\nfor the parser");
}

#[test]
fn the_cost_rolls_up_like_a_meter_and_settles_on_the_spend() {
    let (mut a, _rx) = app();
    a.opts.animations = true;
    let t0 = Instant::now();
    a.fx.clock = Some(t0);
    a.tick();
    a.cost = 0.0123;
    let first = frame(&mut a, 120, 10).last().unwrap().clone();
    assert!(first.contains("$0.0000"), "starts where it was: {first}");
    let mut seen = vec![];
    for ms in (0..=500u64).step_by(25) {
        a.fx.clock = Some(t0 + std::time::Duration::from_millis(ms));
        a.tick();
        seen.push(a.cost_shown);
    }
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "never runs backwards");
    assert!(seen.iter().any(|c| *c > 0.0 && *c < 0.0123), "passes through the figures between");
    let last = frame(&mut a, 120, 10).last().unwrap().clone();
    assert!(last.contains("$0.0123"), "{last}");
    // without animations the figure is the spend at once
    a.opts.animations = false;
    a.cost = 0.5;
    assert!(frame(&mut a, 120, 10).last().unwrap().contains("$0.5000"));
}

#[test]
fn budget_commands_and_the_spend_in_the_title() {
    let (mut a, mut rx) = app();
    for (typed, want) in [
        ("/budget", None),
        ("/budget 1.5", Some(Some(1.5))),
        ("/budget $2", Some(Some(2.0))),
        ("/budget off", Some(None)),
    ] {
        a.input.set_text(typed);
        a.submit();
        match (rx.try_recv(), want) {
            (Ok(AgentCommand::ShowBudget), None) => {}
            (Ok(AgentCommand::SetBudget { limit }), Some(l)) => assert_eq!(limit, l, "{typed}"),
            (other, _) => panic!("{typed}: {other:?}"),
        }
    }
    a.input.set_text("/budget lots");
    a.submit();
    assert!(rx.try_recv().is_err(), "not an amount: nothing sent");
    a.cost = 0.9;
    a.cost_shown = 0.9;
    a.handle_agent(AgentEvent::Budget { session_cost: 0.9, day_cost: 1.2, session_limit: Some(1.0), day_limit: None });
    let backend = ratatui::backend::TestBackend::new(120, 10);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let top = crate::ui::rows_of(term.backend().buffer()).last().unwrap().clone();
    assert!(top.contains("$0.9000/1.00"), "{top}");
}

#[test]
fn a_pasted_image_is_kept_and_goes_with_the_request() {
    let d = tempfile::tempdir().unwrap();
    std::env::set_var("NEXO_CACHE_DIR", d.path());
    // a 2×1 picture: one red pixel, one transparent
    let png = crate::clipboard::encode_png(2, 1, &[255, 0, 0, 255, 0, 0, 0, 0]).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    let back = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!((back.width(), back.height(), back.get_pixel(0, 0).0), (2, 1, [255, 0, 0, 255]));
    assert!(crate::clipboard::encode_png(3, 3, &[0; 4]).is_err(), "pixels that do not fill the size");
    let (mut a, mut rx) = app();
    a.input.set_text("what is wrong here");
    a.attach_png(&png);
    std::env::remove_var("NEXO_CACHE_DIR");
    let draft = a.input.text();
    let path = draft.split_whitespace().find_map(|w| w.strip_prefix('@')).unwrap().to_string();
    assert!(path.starts_with(&d.path().join("pastes").display().to_string()) && path.ends_with(".png"), "{draft}");
    assert_eq!(std::fs::read(&path).unwrap(), png);
    a.submit();
    match rx.try_recv() {
        Ok(AgentCommand::Submit { attachments, .. }) => assert_eq!(attachments[0].path, PathBuf::from(&path)),
        other => panic!("{other:?}"),
    }
}

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
