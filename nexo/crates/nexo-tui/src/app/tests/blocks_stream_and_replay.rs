//! Blocks that fold, fade in, stream and replay; layout, scroll and search.

use super::*;

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
fn the_reasoning_of_a_reply_sent_again_is_closed_and_not_joined_to_the_new_one() {
    let (mut a, _rx) = bare();
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::AssistantStart);
    a.handle_agent(AgentEvent::ReasoningDelta { text: "first try".into() });
    let said =
        "the stream was cut while the model reasoned; that reasoning is dropped and the request sent again (1/2)";
    a.handle_agent(AgentEvent::Notice { level: Level::Warn, text: said.into(), problem: None });
    a.handle_agent(AgentEvent::AssistantStart);
    assert!(!a.blocks[0].is_live(), "no spinner left on the reply that was dropped");
    a.handle_agent(AgentEvent::ReasoningDelta { text: "second try".into() });
    a.handle_agent(AgentEvent::TextDelta { text: "done".into() });
    a.handle_agent(AgentEvent::AssistantDone { message: Message::assistant("done") });
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    assert!(matches!(&a.blocks[0], Block::Reasoning { text, streaming: false, .. } if text == "first try"));
    assert!(matches!(&a.blocks[1], Block::Notice { level: Level::Warn, .. }));
    assert!(matches!(&a.blocks[2], Block::Reasoning { text, streaming: false, .. } if text == "second try"));
    assert!(matches!(&a.blocks[3], Block::Assistant { text, streaming: false, .. } if text == "done"));
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
