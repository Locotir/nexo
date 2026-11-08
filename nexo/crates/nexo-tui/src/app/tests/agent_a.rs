use super::*;

#[test]
fn the_last_code_block_is_found() {
    let text = "run this:\n```bash\ncargo test\n```\nthen\n````rust\nfn a() {}\n```\nx\n````\nend";
    assert_eq!(last_code_block(text).as_deref(), Some("fn a() {}\n```\nx"));
    assert_eq!(last_code_block("```\ncut off"), None);
    assert_eq!(last_code_block("no code"), None);
}

#[test]
fn ctrl_c_clears_a_draft_first_and_the_title_follows_the_turn() {
    let (mut a, _rx) = app();
    a.input.set_text("half a thought");
    a.ctrl_c();
    assert!(a.input.is_empty());
    assert_eq!(a.input.history().last().map(String::as_str), Some("half a thought"));
    assert!(!a.should_quit);
    a.ctrl_c();
    assert!(a.should_quit);

    let (mut a, _rx) = app();
    assert_eq!(a.terminal_title(), "nexo");
    a.session_title = "fix the parser".into();
    assert_eq!(a.terminal_title(), "nexo · fix the parser");
    a.busy = true;
    assert!(a.terminal_title().ends_with(" nexo · fix the parser"));
    assert_ne!(a.terminal_title(), "nexo · fix the parser");
}

#[test]
fn a_long_command_is_cut_so_the_cache_mark_and_price_still_show() {
    use crate::render::wrap::width;
    let theme = Theme::dark();
    let text = |l: &Line| l.spans.iter().map(|s| s.content.to_string()).collect::<String>();
    let head = || Line::from(vec![Span::raw("▸ bash  "), Span::raw("cargo test -p nexo-bench ".repeat(6))]);
    // too long for the line: the command ends in … and the mark and price close the line
    let mut l = head();
    mark_head(&mut l, CallMark::Cached, Some(0.00012), 60, &theme);
    let t = text(&l);
    assert!(t.ends_with("● cached 0.00012 $") && t.contains('…'), "{t}");
    assert_eq!(width(&t), 60);
    // no price known: the word alone
    let mut l = head();
    mark_head(&mut l, CallMark::Sent, None, 50, &theme);
    assert!(text(&l).ends_with("● full price") && width(&text(&l)) == 50, "{}", text(&l));
    // a short command is left whole
    let mut l = Line::from(vec![Span::raw("▸ bash  "), Span::raw("ls")]);
    mark_head(&mut l, CallMark::Cached, Some(0.00012), 60, &theme);
    assert!(text(&l).starts_with("▸ bash  ls ") && !text(&l).contains('…'));
    // a window too narrow for any of it keeps the head as it was
    let mut l = head();
    mark_head(&mut l, CallMark::Cached, Some(0.00012), 20, &theme);
    assert_eq!(text(&l), text(&head()));
}

#[test]
fn an_update_shows_its_card_once_and_a_later_check_only_when_more_came_out() {
    let (mut a, _rx) = app();
    let cards = |a: &App| a.blocks.iter().filter(|b| matches!(b, Block::Update { .. })).count();
    a.update_ready(2);
    a.update_ready(2);
    a.update_ready(1);
    assert_eq!((cards(&a), a.update_behind), (1, Some(2)));
    a.update_ready(5);
    assert_eq!((cards(&a), a.update_behind), (1, Some(5)), "a toast, not a second card");
}

#[test]
fn two_tool_calls_form_one_group_marker_and_a_single_call_does_not() {
    let (mut a, _rx) = bare();
    a.push(Block::User { text: "go".into(), at: None });
    let one = a.push(card("a", 2));
    assert_eq!(groups(&a), 0);
    assert_eq!(one, 1);
    let two = a.push(card("b", 2));
    assert_eq!(groups(&a), 1);
    assert!(matches!(a.blocks[1], Block::Group { folded: true, len: 2 }));
    assert_eq!(two, 3, "the call is returned where it now is");
    a.push(card("c", 2));
    assert!(matches!(a.blocks[1], Block::Group { len: 3, .. }), "a third call joins the same group");
    assert_eq!(groups(&a), 1);
    assert_eq!(a.blocks.len(), 5);
    assert!(!a.hidden(1) && a.hidden(2) && a.hidden(3) && a.hidden(4));
}

#[test]
fn reasoning_between_calls_joins_the_group_but_trailing_reasoning_does_not() {
    let (mut a, _rx) = bare();
    a.push(card("a", 1));
    a.push(thinking());
    a.push(card("b", 1));
    assert!(matches!(a.blocks[0], Block::Group { len: 3, .. }), "{:?}", a.blocks.len());
    a.push(thinking());
    assert!(matches!(a.blocks[0], Block::Group { len: 3, .. }), "thinking after the last call is outside");
    assert!(!a.hidden(5));
    a.push(Block::Assistant { text: "x".into(), streaming: false, at: None, started: None, took_ms: None });
    a.push(card("c", 1));
    assert_eq!(groups(&a), 1, "an answer breaks the run");
}

#[test]
fn the_agreed_plan_and_opened_thinking_stay_visible_in_a_folded_group() {
    let (mut a, _rx) = bare();
    a.push(card("a", 1));
    let mut open = thinking();
    if let Block::Reasoning { folded, .. } = &mut open {
        *folded = false;
    }
    a.push(open);
    let mut plan = card("p", 1);
    if let Block::Tool { name, .. } = &mut plan {
        *name = "exit_plan".into();
    }
    a.push(plan);
    a.push(card("b", 1));
    assert!(a.hidden(1) && !a.hidden(2), "thinking the user opened is not hidden");
    assert!(!a.hidden(3), "the plan is not hidden");
    assert!(a.hidden(4));
}

#[test]
fn group_tools_off_creates_no_marker() {
    let (mut a, _rx) = bare();
    a.opts.group_tools = false;
    a.push(card("a", 1));
    a.push(card("b", 1));
    assert_eq!(groups(&a), 0);
    assert_eq!(a.blocks.len(), 2);
}

#[test]
fn a_running_member_stays_visible_under_a_folded_header() {
    let (mut a, _rx) = bare();
    a.push(card("a", 1));
    a.push(running("b"));
    a.layout(80);
    assert!(a.hidden(1) && !a.hidden(2));
    let h = head(&mut a, 0);
    assert!(h.contains("Running"), "{h}");
    assert_eq!(a.offsets[1], a.offsets[2], "the finished call takes no room");
}

#[test]
fn group_header_changes_tense_when_the_last_call_finishes() {
    let (mut a, _rx) = bare();
    a.push(card("a", 1));
    a.push(running("b"));
    a.layout(80);
    assert!(head(&mut a, 0).contains("Running"), "{}", head(&mut a, 0));
    if let Block::Tool { status, output, .. } = &mut a.blocks[2] {
        *status = ToolStatus::Done;
        *output = Some(ToolOutput::ok("ok"));
    }
    a.invalidate(2);
    a.layout(80);
    let h = head(&mut a, 0);
    assert!(h.contains("Ran 2 shell commands"), "{h}");
    assert!(a.hidden(2), "folded again once everything finished");
}

#[test]
fn enter_on_a_group_toggles_it_and_inner_cards_keep_their_own_fold() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(card("a", 3));
    a.push(card("b", 3));
    a.layout(80);
    a.selected = Some(0);
    press(&mut a, KeyCode::Enter);
    assert!(matches!(a.blocks[0], Block::Group { folded: false, .. }));
    a.layout(80);
    assert!(!a.hidden(1) && !a.hidden(2));
    assert!(matches!(a.blocks[1], Block::Tool { folded: true, .. }), "the cards stay folded inside");
    assert!(a.offsets[2] > a.offsets[1]);
    press(&mut a, KeyCode::Enter);
    a.layout(80);
    assert!(a.hidden(1) && a.hidden(2));
    assert_eq!(a.selected, Some(0));
}

#[test]
fn click_on_the_header_selects_then_toggles() {
    let (mut a, _rx) = bare();
    a.push(card("a", 1));
    a.push(card("b", 1));
    a.layout(80);
    a.click_block(0, 0);
    assert_eq!(a.selected, Some(0));
    assert!(a.is_folded(0));
    a.click_block(0, 0);
    assert!(!a.is_folded(0));
}

#[test]
fn arrow_navigation_skips_hidden_members() {
    let (mut a, _rx) = bare();
    a.push(Block::User { text: "a long\nmessage\nof lines".into(), at: None });
    a.push(card("a", 3));
    a.push(card("b", 3));
    a.push(Block::Assistant {
        text: "done\n\nmore\n\nlines".into(),
        streaming: false,
        at: None,
        started: None,
        took_ms: None,
    });
    a.layout(80);
    a.selected = None;
    for _ in 0..4 {
        a.select_next(true);
        if let Some(s) = a.selected {
            assert!(!a.hidden(s), "{s} is hidden");
        }
    }
}

#[test]
fn collapse_keeps_scroll_inside_the_transcript() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(Block::User { text: "go".into(), at: None });
    for id in ["a", "b", "c", "d"] {
        a.push(card(id, 20));
    }
    a.transcript_area = Rect::new(0, 1, 80, 6);
    a.layout(80);
    a.toggle_block(1);
    for k in 2..6 {
        a.blocks[k].set_fold(false);
        a.invalidate(k);
    }
    a.layout(80);
    a.follow = false;
    a.scroll = a.offsets[5] + 2;
    a.toggle_block(1);
    a.layout(80);
    a.settle(6);
    assert!(a.scroll <= a.max_scroll(6), "{} > {}", a.scroll, a.max_scroll(6));
    assert!(a.scroll <= a.offsets[1] + 1);
}

#[test]
fn ctrl_o_flips_every_group_and_card() {
    let (mut a, _rx) = bare();
    a.push(card("a", 2));
    a.push(card("b", 2));
    a.fold_all();
    assert!(matches!(a.blocks[0], Block::Group { folded: false, .. }), "everything was folded: it opens");
    assert!(matches!(a.blocks[1], Block::Tool { folded: false, .. }));
    a.fold_all();
    assert!(matches!(a.blocks[0], Block::Group { folded: true, .. }));
    assert!(matches!(a.blocks[2], Block::Tool { folded: true, .. }));
}

#[test]
fn search_hit_in_a_folded_group_counts_once_and_unfolds_on_jump() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(card("a", 2));
    a.push(card("b", 2));
    a.transcript_area = Rect::new(0, 1, 80, 10);
    a.layout(80);
    a.open_search();
    a.search.as_mut().unwrap().query = "line".into();
    a.rematch();
    assert_eq!(a.search.as_ref().unwrap().matches.len(), 1, "one hit for the whole folded group");
    a.search_first();
    assert!(matches!(a.blocks[0], Block::Group { folded: false, .. }));
    a.layout(80);
    assert!(!a.search.as_ref().unwrap().matches.is_empty());
}

#[test]
fn a_folded_group_is_one_copyable_line() {
    let (mut a, _rx) = bare();
    a.push(card("a", 1));
    a.push(card("b", 1));
    a.layout(80);
    let t = a.line_text(0).unwrap();
    assert!(t.contains("Ran 2 shell commands"), "{t}");
}

#[test]
fn replayed_session_groups_like_a_live_one() {
    let (mut a, _rx) = bare();
    a.push(card("a", 1));
    a.push(thinking());
    a.push(card("b", 1));
    a.push(card("c", 1));
    assert!(matches!(a.blocks[0], Block::Group { len: 4, .. }));
    assert_eq!(a.blocks.len(), 5);
}

#[test]
fn animations_off_opens_instantly() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(card("a", 4));
    a.push(card("b", 4));
    a.layout(80);
    a.toggle_block(0);
    a.layout(80);
    let full = a.height(1, 80);
    assert_eq!(a.shown(1, 80), full);
}

#[test]
fn a_long_finished_group_costs_nothing_per_frame() {
    let (mut a, _rx) = bare();
    for n in 0..200 {
        a.push(card(&format!("c{n}"), 3));
    }
    a.layout(80);
    a.layout(80);
    let rendered = a.cache.iter().filter(|c| matches!(c, Some((_, _, Some(_), _)))).count();
    assert!(rendered <= 2, "hidden calls are not rendered: {rendered}");
    assert!(a.total_lines <= 2, "one line and its gap: {}", a.total_lines);
}

#[test]
fn one_key_opens_and_folds_thinking_with_the_tool_cards() {
    let (mut a, _rx) = app();
    let i = a.push(Block::Reasoning {
        text: "a\nb\nc".into(),
        streaming: false,
        started: None,
        elapsed_ms: Some(5),
        folded: true,
    });
    a.fold_all();
    assert!(!a.is_folded(i), "everything folded, so the key opens it");
    a.fold_all();
    assert!(a.is_folded(i));
}

#[test]
fn any_long_block_folds_to_its_first_line_and_opens_again() {
    let (mut a, _rx) = app();
    let text = (1..=6).map(|i| format!("paragraph {i}")).collect::<Vec<_>>().join("\n\n");
    let i = a.push(Block::Assistant { text, streaming: false, at: None, started: None, took_ms: None });
    a.layout(60);
    let open = a.lines(i, 60).len();
    assert!(open > 2);
    assert!(a.foldable(i));
    a.toggle_block(i);
    let folded: Vec<String> = a.lines(i, 60).iter().map(crate::render::markdown::text_of).collect();
    assert_eq!(folded.len(), 2, "{folded:?}");
    assert!(folded[0].contains("paragraph 1"));
    assert!(folded[1].contains(&format!("▸ {} more lines", open - 1)), "{folded:?}");
    a.toggle_block(i);
    assert_eq!(a.lines(i, 60).len(), open);
    let one = a.push(Block::Notice { level: Level::Info, text: "short".into() });
    assert!(!a.foldable(one), "a one-line block has nothing to fold");
}

#[test]
fn a_new_block_fades_in_and_an_opened_one_grows() {
    let (mut a, _rx) = app();
    let text = (1..=8).map(|i| format!("p{i}")).collect::<Vec<_>>().join("\n\n");
    let i = a.push(Block::Assistant { text, streaming: false, at: None, started: None, took_ms: None });
    assert!(a.fade(i) < 1.0, "a fresh block starts faint");
    a.layout(60);
    a.toggle_block(i);
    a.toggle_block(i);
    let full = a.lines(i, 60).len();
    assert!(a.shown(i, 60) < full, "an opening block grows over a few frames");
    a.looks[i].opened = None;
    a.looks[i].born = None;
    assert_eq!(a.shown(i, 60), full);
    assert_eq!(a.fade(i), 1.0);
}

#[test]
fn streams_into_blocks_and_closes_them() {
    let (mut a, _rx) = bare();
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::ReasoningDelta { text: "hm".into() });
    a.handle_agent(AgentEvent::TextDelta { text: "Hel".into() });
    a.handle_agent(AgentEvent::TextDelta { text: "lo".into() });
    a.handle_agent(AgentEvent::ToolCall {
        id: "c1".into(),
        name: "read".into(),
        args: serde_json::json!({"path":"a"}),
        summary: "a".into(),
    });
    a.handle_agent(AgentEvent::ToolStart { id: "c1".into() });
    a.handle_agent(AgentEvent::ToolDone { id: "c1".into(), output: ToolOutput::ok("x"), elapsed_ms: 5 });
    a.handle_agent(AgentEvent::AssistantDone { message: Message::assistant("Hello") });
    a.handle_agent(AgentEvent::Usage {
        turn: Usage { input: 1, output: 1, ..Default::default() },
        call: Usage { input: 1, output: 1, ..Default::default() },
        session: Usage::default(),
        cost_usd: 0.0,
    });
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    assert!(matches!(a.blocks[0], Block::Reasoning { streaming: false, .. }));
    assert!(matches!(&a.blocks[1], Block::Assistant { text, streaming: false, .. } if text == "Hello"));
    assert!(matches!(a.blocks[2], Block::Tool { status: ToolStatus::Done, .. }));
    assert!(matches!(a.blocks[3], Block::Usage { .. }));
    assert!(!a.busy);
}

#[test]
fn web_cards_are_summarised_on_replay() {
    assert_eq!(summarise("web_search", &serde_json::json!({"query": "tokio select"})), "web_search: tokio select");
    assert_eq!(
        summarise("web_fetch", &serde_json::json!({"url": "https://docs.rs/tokio/latest/?x=1"})),
        "web_fetch: docs.rs/tokio/latest"
    );
}

#[test]
fn a_replayed_call_keeps_one_card() {
    let (mut a, _rx) = app();
    let m = Message::new(
        Role::Assistant,
        vec![Part::tool_call("t1", "todo", serde_json::json!({"items": [{"text": "a"}]}))],
    );
    a.handle_agent(AgentEvent::History { index: 0, message: m });
    a.handle_agent(AgentEvent::ToolCall {
        id: "t1".into(),
        name: "todo".into(),
        args: serde_json::json!({"items": [{"text": "a"}]}),
        summary: "todo: 0/1 done".into(),
    });
    a.handle_agent(AgentEvent::ToolDone { id: "t1".into(), output: ToolOutput::ok("[ ] a"), elapsed_ms: 0 });
    let cards: Vec<&Block> = a.blocks.iter().filter(|b| matches!(b, Block::Tool { .. })).collect();
    assert_eq!(cards.len(), 1);
    assert!(matches!(cards[0], Block::Tool { summary, status: ToolStatus::Done, .. } if summary == "todo: 0/1 done"));
}

#[test]
fn history_replays_and_attaches_results() {
    let (mut a, _rx) = bare();
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("do it") });
    let m = Message::new(
        Role::Assistant,
        vec![Part::text("ok"), Part::tool_call("c9", "bash", serde_json::json!({"command": "ls"}))],
    );
    a.handle_agent(AgentEvent::History { index: 1, message: m });
    a.handle_agent(AgentEvent::History {
        index: 2,
        message: Message::tool_results(vec![Part::tool_result("c9", "a.rs\nb.rs", false)]),
    });
    assert_eq!(a.blocks.len(), 3);
    match &a.blocks[2] {
        Block::Tool { summary, output: Some(o), status, .. } => {
            assert_eq!(summary, "ls");
            assert_eq!(o.content, "a.rs\nb.rs");
            assert_eq!(*status, ToolStatus::Done);
        }
        b => panic!("{b:?}"),
    }
}

#[test]
fn a_new_width_keeps_the_block_at_the_top_of_the_view() {
    let (mut a, _rx) = app();
    for i in 0..30 {
        a.push(Block::User { text: format!("message {i} {}", "word ".repeat(30)), at: None });
    }
    a.transcript_area = Rect::new(0, 0, 80, 10);
    a.layout(79);
    a.follow = false;
    a.scroll = a.offsets[12];
    a.layout(40);
    assert_eq!(a.scroll, a.offsets[12], "block 12 stays at the top after a re-wrap");
    a.layout(120);
    assert_eq!(a.scroll, a.offsets[12]);
}

#[test]
fn layout_scroll_and_search() {
    let (mut a, _rx) = bare();
    for i in 0..50 {
        a.push(Block::User { text: format!("message number {i}"), at: None });
    }
    a.transcript_area = Rect::new(0, 0, 80, 10);
    a.layout(80);
    // a message of one's own starts a turn: two blank lines before it
    assert_eq!(a.total_lines, 49 * 3 + 2);
    a.settle(10);
    assert_eq!(a.scroll, 139);
    a.scroll_by(-3);
    assert!(!a.follow);
    assert_eq!(a.scroll, 136);
    a.push(Block::User { text: "late".into(), at: None });
    a.layout(80);
    a.settle(10);
    assert_eq!(a.unseen(), 3);
    a.scroll_to_end();
    assert_eq!(a.unseen(), 0);
    assert_eq!(a.block_at_line(0), Some(0));
    assert_eq!(a.block_at_line(1), None, "the separator belongs to nobody");
    assert_eq!(a.block_at_line(2), None);
    assert_eq!(a.block_at_line(3), Some(1));
    a.open_search();
    a.search.as_mut().unwrap().query = "number 4".into();
    a.rematch();
    assert_eq!(a.search.as_ref().unwrap().matches.len(), 11);
    a.search_jump(true);
    assert!(!a.follow);
}

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
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Probe { name, .. }) if name == "ollama"));
    a.handle_agent(AgentEvent::Probed {
        name: "ollama".into(),
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
    key(&mut a, KeyCode::Esc);
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
fn enter_on_an_empty_draft_with_a_queue_interrupts_the_turn() {
    let (mut a, mut rx) = app();
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.input.set_text("second thought");
    a.submit();
    assert_eq!(a.queued, 1);
    while rx.try_recv().is_ok() {}
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Interrupt)));
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
    assert_eq!(notices, ["◎ goal set: tests pls", "◎ goal · keeps going"]);
    assert!(a.user_messages.is_empty(), "nothing of the loop to rewind to");
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
