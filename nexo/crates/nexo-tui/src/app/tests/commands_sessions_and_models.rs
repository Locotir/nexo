//! Commands that reach the agent, sessions, rewind and the model picker.

use super::*;

#[test]
fn a_first_request_from_the_command_line_is_sent_or_kept_until_a_model_is_chosen() {
    let (mut a, mut rx) = app();
    a.start_with("fix the failing test");
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. }) if text == "fix the failing test"));
    let (mut a, mut rx) = app();
    a.opts.provider.clear();
    a.start_with("fix the failing test");
    assert!(rx.try_recv().is_err(), "nothing to send it to yet");
    assert_eq!(a.input.full_text(), "fix the failing test");
}

#[test]
fn an_image_dropped_as_its_path_is_attached() {
    let d = tempfile::tempdir().unwrap();
    let shot = d.path().join("shot.png");
    std::fs::write(&shot, b"png").unwrap();
    let (mut a, mut rx) = app();
    a.input.set_text(&format!("why is this red? '{}'", shot.display()));
    a.submit();
    match rx.try_recv() {
        Ok(AgentCommand::Submit { attachments, .. }) => assert_eq!(attachments, vec![Attachment { path: shot }]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn closing_on_a_session_with_a_request_says_how_to_resume_it() {
    let (mut a, _rx) = app();
    a.session_id = Some("20261108-050000-ab12".into());
    assert_eq!(a.resume_hint(), None, "nothing said, nothing to resume");
    a.push(Block::User { text: "hi".into(), at: None });
    assert_eq!(a.resume_hint().as_deref(), Some("nexo --resume 20261108-050000-ab12   (or nexo -c here)"));
}
#[test]
fn commands_and_submissions_reach_the_agent() {
    let (mut a, mut rx) = app();
    a.input.set_text("hello there");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. }) if text == "hello there"));
    a.input.set_text("/permissions auto");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetPermissionMode(PermissionMode::Auto))));
    a.input.set_text("!ls -la");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Shell { command }) if command == "ls -la"));
    a.input.set_text("/model");
    a.submit();
    assert!(matches!(a.modal, Some(Modal::Setup(_))));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ScanLocal)), "the setup looks for local servers");
    a.handle_agent(AgentEvent::LocalServers { found: vec![("ollama".into(), "Ollama 0.12.6".into())] });
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |a: &mut App, code: KeyCode| {
        let out = a.modal.as_mut().unwrap().key(KeyEvent::new(code, KeyModifiers::NONE));
        a.modal_out(out);
    };
    key(&mut a, KeyCode::Down);
    key(&mut a, KeyCode::Enter);
    assert!(a.modal.is_none());
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetModel { model, .. }) if model == "n"));
    // the setup of a new one: the running server is asked at once, its answer lands in the modal
    a.input.set_text("/model");
    a.submit();
    let _ = rx.try_recv();
    a.handle_agent(AgentEvent::LocalServers { found: vec![("ollama".into(), "Ollama 0.12.6".into())] });
    for c in "set up oll".chars() {
        key(&mut a, KeyCode::Char(c));
    }
    key(&mut a, KeyCode::Enter);
    let Ok(AgentCommand::Probe { name, seq, .. }) = rx.try_recv() else { panic!("a probe") };
    assert_eq!(name, "ollama");
    a.handle_agent(AgentEvent::Probed {
        name: "ollama".into(),
        seq,
        result: Ok(nexo_core::ServerProbe {
            kind: "ollama".into(),
            protocol: "openai-chat".into(),
            base_url: "http://localhost:11434/v1".into(),
            models: vec![nexo_core::ModelInfo { id: "qwen3:8b".into(), ..Default::default() }],
            ..Default::default()
        }),
    });
    let Some(Modal::Setup(st)) = &a.modal else { panic!("the setup stays open") };
    assert_eq!(st.step, crate::modal::setup::Step::Model);
    // a server found running is asked at once: no address step to walk back through
    key(&mut a, KeyCode::Esc);
    key(&mut a, KeyCode::Esc);
    assert!(a.modal.is_none(), "Esc walks back and out");
    // /effort: the levels the model takes
    a.handle_agent(AgentEvent::Effort { model: "m".into(), current: None, levels: vec!["low".into(), "high".into()] });
    a.input.set_text("/effort");
    a.submit();
    let Some(Modal::Picker(p)) = &a.modal else { panic!("an effort picker") };
    assert_eq!(p.items.iter().map(|i| i.value.as_str()).collect::<Vec<_>>(), ["default", "low", "high"]);
    a.modal_out(ModalOut::Pick { kind: PickKind::Effort, value: "high".into() });
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetEffort { effort: Some(e) }) if e == "high"));
    a.input.set_text("/effort default");
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetEffort { effort: None })));
    a.input.set_text("/nope");
    a.submit();
    assert!(matches!(a.blocks.last(), Some(Block::Notice { .. })));
    a.input.set_text("/mo");
    a.refresh_palette();
    assert!(a.palette.is_some());
    a.palette_accept(false);
    assert_eq!(a.input.text(), "/model ");
}

#[test]
fn fast_asks_the_agent_and_the_mark_follows_its_answer() {
    let (mut a, mut rx) = app();
    for (line, on) in [("/fast", None), ("/fast on", Some(true)), ("/FAST off", Some(false))] {
        a.input.set_text(line);
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetFast { on: got }) if got == on), "{line}");
    }
    a.input.set_text("/fast sideways");
    a.submit();
    assert!(rx.try_recv().is_err());
    assert!(matches!(a.blocks.last(), Some(Block::Notice { .. })));
    assert!(!a.fast);
    a.handle_agent(AgentEvent::Fast { on: true });
    assert!(a.fast);
    a.handle_agent(AgentEvent::Fast { on: false });
    assert!(!a.fast);
}

#[test]
fn enter_on_an_empty_draft_with_a_queue_sends_it_now() {
    let (mut a, mut rx) = app();
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.input.set_text("second thought");
    a.submit();
    assert_eq!(a.queued, 1);
    while rx.try_recv().is_ok() {}
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::InterruptRun)));
    // nothing waiting: an empty Enter does not stop the turn
    a.queued = 0;
    a.submit();
    assert!(rx.try_recv().is_err());
}

#[test]
fn history_clears_and_the_session_and_queue_follow_the_agent() {
    let (mut a, _rx) = app();
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("first") });
    a.handle_agent(AgentEvent::History { index: 1, message: Message::assistant("ok") });
    a.handle_agent(AgentEvent::Usage {
        turn: Usage { input: 5, ..Default::default() },
        call: Usage { input: 5, ..Default::default() },
        session: Usage { input: 5, ..Default::default() },
        cost_usd: 0.01,
    });
    a.transcript_area = Rect::new(0, 0, 80, 10);
    a.layout(80);
    a.selection = Some(Selection::new(Pos { line: 0, col: 0 }));
    a.handle_agent(AgentEvent::Queued { pending: 2 });
    assert_eq!(a.queued, 2);
    a.handle_agent(AgentEvent::HistoryCleared);
    assert!(a.has_welcome() && a.blocks.len() == 1, "only the banner is left");
    assert!(a.user_messages.is_empty() && a.selection.is_none());
    assert_eq!(a.total_lines, 0);
    assert_eq!(a.cost, 0.0);
    assert_eq!(a.queued, 0);
    a.handle_agent(AgentEvent::SessionChanged { session_id: "s2".into(), title: "the other one".into() });
    assert_eq!(a.session_id.as_deref(), Some("s2"));
    assert_eq!(a.session_title, "the other one");
    a.handle_agent(AgentEvent::Queued { pending: 0 });
    assert_eq!(a.queued, 0);
}

#[test]
fn msg_writes_to_a_session_and_the_transcript_follows_the_receipts() {
    use nexo_core::mail::{PeerInfo, Stage};
    let (mut a, mut rx) = app();
    assert_eq!(commands::parse("/msg api look at parser.rs"), Some(Command::Msg(Some("api look at parser.rs".into()))));
    a.run_command(Command::Msg(Some("api look at parser.rs".into())));
    assert!(
        matches!(rx.try_recv(), Ok(AgentCommand::SendMail { to, text }) if to == "api" && text == "look at parser.rs")
    );
    // a session and no text: nothing goes out
    a.run_command(Command::Msg(Some("api".into())));
    assert!(rx.try_recv().is_err() && matches!(a.blocks.last(), Some(Block::Notice { .. })));
    // no arguments: the sessions are asked for and the palette opens on them
    let peer = |id: &str, name: &str, me: bool| PeerInfo {
        id: id.into(),
        name: name.into(),
        cwd: "/home/dev/api".into(),
        pid: 1,
        external: false,
        bypass: false,
        sendable: true,
        me,
    };
    a.handle_agent(AgentEvent::Peers {
        peers: vec![peer("aaaa1111", "me", true), peer("bbbb2222", "api", false), peer("cccc3333", "two words", false)],
    });
    a.run_command(Command::Msg(None));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ListPeers)));
    assert_eq!(a.input.text(), "/msg ");
    let v: Vec<String> = a.peer_values().into_iter().map(|(v, _)| v).collect();
    assert_eq!(v, ["api", "cccc3333"], "not itself; a name with spaces is offered by its id");

    // the receipts move the line along and never back
    let stage = |a: &App| {
        a.blocks.iter().rev().find_map(|b| if let Block::Mail { stage, .. } = b { Some(*stage) } else { None })
    };
    a.handle_agent(AgentEvent::MailSent {
        id: "m1".into(),
        to: "bbbb2222".into(),
        to_name: "api".into(),
        summary: "look at parser.rs".into(),
        bytes: 17,
    });
    assert_eq!(stage(&a), Some(Stage::Sent));
    a.handle_agent(AgentEvent::MailStage { id: "m1".into(), stage: Stage::Delivered });
    assert_eq!(stage(&a), Some(Stage::Delivered));
    a.handle_agent(AgentEvent::MailStage { id: "m1".into(), stage: Stage::Read });
    a.handle_agent(AgentEvent::MailStage { id: "m1".into(), stage: Stage::Delivered });
    assert_eq!(stage(&a), Some(Stage::Read));
    let lines = a.blocks.iter().rev().find(|b| matches!(b, Block::Mail { .. })).unwrap();
    let text: String = lines
        .render(80, &a.theme, a.render_opts())
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("✉ -> api: look at parser.rs") && text.contains("✓✓ read"), "{text}");

    // a mail that comes in is marked, and a refusal warns
    a.handle_agent(AgentEvent::MailArrived {
        id: "m2".into(),
        from: "bbbb2222".into(),
        from_name: "api".into(),
        summary: "done".into(),
        held: false,
    });
    assert!(matches!(a.blocks.last(), Some(Block::Mail { outgoing: false, .. })));
    a.handle_agent(AgentEvent::MailDelivered { ids: vec!["m2".into()] });
    assert_eq!(stage(&a), Some(Stage::Delivered));
    a.handle_agent(AgentEvent::MailFailed { to: "web".into(), reason: "no such session".into() });
    assert!(
        matches!(a.blocks.last(), Some(Block::Notice { level: Level::Warn, text }) if text.contains("no such session"))
    );
}

#[test]
fn fork_picks_a_cut_point_and_tree_jumps_between_sessions() {
    let (mut a, mut rx) = app();
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("first ask") });
    a.handle_agent(AgentEvent::History { index: 1, message: Message::assistant("ok") });
    a.run_command(Command::Fork);
    let Some(Modal::Picker(p)) = &a.modal else { panic!("no fork picker") };
    assert_eq!(p.kind, PickKind::Fork);
    a.modal_out(ModalOut::Pick { kind: PickKind::Fork, value: "0".into() });
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Fork { index: Some(0) })));
    a.modal_out(ModalOut::Pick { kind: PickKind::Fork, value: "all".into() });
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Fork { index: None })));
    a.run_command(Command::Tree);
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShowTree)));
    a.handle_agent(AgentEvent::SessionTree { entries: vec![(0, "s1".into(), "root".into())] });
    assert!(a.modal.is_none(), "a lone session has no tree to show");
    a.handle_agent(AgentEvent::SessionTree {
        entries: vec![(0, "s1".into(), "root".into()), (1, "s2".into(), "root (fork)".into())],
    });
    assert!(matches!(&a.modal, Some(Modal::Picker(p)) if p.kind == PickKind::Tree));
    a.modal_out(ModalOut::Pick { kind: PickKind::Tree, value: "s2".into() });
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Resume { id }) if id == "s2"));
}

#[test]
fn a_folder_taken_with_tab_lists_its_own_folders_next() {
    let (mut a, _rx) = app();
    a.input.set_text("/add-dir sr");
    a.refresh_palette();
    assert_eq!(a.palette.as_ref().and_then(|p| p.current()), Some("src/"));
    a.palette_accept(false);
    assert_eq!(a.input.text(), "/add-dir src/");
    let next = a.palette.as_ref().and_then(|p| p.current()).unwrap_or("");
    assert!(next.starts_with("src/") && next.len() > 4, "the folders of src/ are listed: {next:?}");
}

#[test]
fn the_esc_that_interrupted_a_turn_does_not_also_open_the_rewind_picker() {
    let (mut a, mut rx) = app();
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("an ask") });
    a.busy = true;
    a.esc();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Interrupt)));
    // the turn ends before the second press of a quick Esc Esc
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Interrupted });
    a.esc();
    assert!(a.modal.is_none(), "a press that follows the interrupt is part of it");
}

#[test]
fn the_rewind_picker_lists_the_persons_messages_newest_first_and_rewinds() {
    let (mut a, mut rx) = app();
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("first ask\nwith detail") });
    a.handle_agent(AgentEvent::History { index: 1, message: Message::assistant("ok") });
    a.handle_agent(AgentEvent::History { index: 2, message: Message::user("second ask") });
    a.esc();
    assert!(a.modal.is_none(), "the first Esc only hints");
    a.esc();
    let Some(Modal::Picker(p)) = &a.modal else { panic!("no picker") };
    assert_eq!(p.kind, PickKind::Rewind);
    assert_eq!(p.items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), vec!["second ask", "first ask"]);
    assert_eq!(p.items[1].value, "0");
    assert_eq!(
        p.items.iter().map(|i| i.hint.as_str()).collect::<Vec<_>>(),
        vec!["#2", "#1"],
        "requests are counted, not messages"
    );
    a.modal_out(ModalOut::Pick { kind: PickKind::Rewind, value: "0".into() });
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Rewind { index: 0 })));
    assert_eq!(a.input.text(), "first ask\nwith detail");
    a.input.clear();
    a.input.set_text("/rewind");
    a.submit();
    assert!(matches!(a.modal, Some(Modal::Picker(_))));
    a.modal = None;
    a.busy = true;
    a.run_command(Command::Rewind);
    assert!(a.modal.is_none(), "no rewind while a turn runs");
}

#[test]
fn sessions_resume_and_export_and_diff_reach_the_agent() {
    let (mut a, mut rx) = app();
    a.opts.sessions = vec![("20261027-1".into(), "one".into())];
    a.run_command(Command::Sessions);
    a.modal_out(ModalOut::Pick { kind: PickKind::Session, value: "20261027-1".into() });
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Resume { id }) if id == "20261027-1"));
    a.input.set_text("/export notes.md");
    a.submit();
    assert!(
        matches!(rx.try_recv(), Ok(AgentCommand::Export { path: Some(p), format: None }) if p == std::path::Path::new("notes.md"))
    );
    a.input.set_text("/diff");
    a.submit();
    assert!(a.diffview.is_some());
    assert!(rx.try_recv().is_err());
}

#[test]
fn the_clock_keeps_beating_while_the_diff_view_reads_the_changes() {
    use crate::frame::Load;
    let (mut a, _rx) = app();
    // the banner comes in from the very first frame, before any tick
    assert_eq!(a.load(), Load::Motion, "the intro asks for frames at once");
    a.fx.clock = Some(a.opened + std::time::Duration::from_millis(crate::render::blocks::INTRO_MS + 200));
    assert_eq!(a.load(), Load::Idle, "then the clock rests");
    a.opts.animations = false;
    a.fx.clock = Some(a.opened);
    assert_eq!(a.load(), Load::Idle, "no intro without animations");
    let mut v = crate::diffview::DiffView::from_collected(Default::default());
    v.loading = true;
    a.diffview = Some(v);
    assert_eq!(a.load(), Load::Beat, "the answer of the collection is taken on a tick");
    a.diffview = Some(crate::diffview::DiffView::from_collected(Default::default()));
    assert_eq!(a.load(), Load::Idle, "once it answered, nothing asks for the clock");
}

#[test]
fn enter_on_a_command_that_needs_its_argument_waits_for_it() {
    let (mut a, mut rx) = app();
    a.session_title = "fix the parser".into();
    a.input.set_text("/ren");
    a.refresh_palette();
    a.palette_accept(true);
    assert_eq!(a.input.text(), "/rename ");
    assert!(rx.try_recv().is_err() && !a.blocks.iter().any(|b| matches!(b, Block::Notice { .. })));
    assert_eq!(a.palette.as_ref().and_then(|p| p.current()), Some("fix the parser"), "its list opens");
}

#[test]
fn picking_a_model_of_another_provider_switches_the_provider_too() {
    // /model luna, arrow to the codex entry, Enter: the provider comes with the model
    let (mut a, mut rx) = two_luna();
    a.input.set_text("/model luna");
    a.refresh_palette();
    assert_eq!(a.palette.as_ref().and_then(|p| p.current()), Some("refuseless/luna"));
    a.palette.as_mut().unwrap().selected = 1;
    assert_eq!(a.palette.as_ref().and_then(|p| p.current()), Some("codex/luna"));
    a.palette_accept(true);
    let Ok(AgentCommand::SetModel { provider, model }) = rx.try_recv() else { panic!("no SetModel") };
    assert_eq!((provider.as_deref(), model.as_str()), (Some("codex"), "luna"));
    a.handle_agent(AgentEvent::ModelChanged { provider: "codex".into(), model: "luna".into() });
    assert_eq!(a.provider, "codex");
    assert!(title(&a).contains("codex/luna") && !title(&a).contains("refuseless/luna"), "{}", title(&a));
}

#[test]
fn a_model_name_only_another_provider_has_goes_to_that_provider() {
    let (mut a, mut rx) = two_luna();
    a.opts.models.retain(|(p, m)| !(p == "refuseless" && m == "luna"));
    // Enter on the typed name, and the command typed whole
    a.input.set_text("/model luna");
    a.refresh_palette();
    a.palette_accept(true);
    assert!(
        matches!(rx.try_recv(), Ok(AgentCommand::SetModel { provider: Some(p), model }) if p == "codex" && model == "luna")
    );
    a.run_command(Command::Model(Some("luna".into())));
    assert!(
        matches!(rx.try_recv(), Ok(AgentCommand::SetModel { provider: Some(p), model }) if p == "codex" && model == "luna")
    );
    // a name the provider in use has stays there; one nobody lists keeps the provider in use
    a.run_command(Command::Model(Some("deepseek-chat".into())));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetModel { provider: None, model }) if model == "deepseek-chat"));
    a.run_command(Command::Model(Some("brand-new".into())));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetModel { provider: None, model }) if model == "brand-new"));
}

#[test]
fn the_model_list_marks_only_the_entry_in_use() {
    let (a, _rx) = two_luna();
    let v = palette::arg_values("model", &a.opts.models, "refuseless/luna", &[]);
    let used: Vec<&str> = v.iter().filter(|(_, h)| h == "in use").map(|(n, _)| n.as_str()).collect();
    assert_eq!(used, ["refuseless/luna"]);
}
