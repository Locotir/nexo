//! Session lists, changed files, selecting and walking the blocks.

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
    assert!(rows.contains("Del again delete") && rows.contains("· 2 · Del delete"), "{rows}");
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
