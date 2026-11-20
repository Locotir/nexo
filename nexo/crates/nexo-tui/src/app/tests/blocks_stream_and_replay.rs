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
fn a_notice_during_the_stream_leaves_one_thinking_and_one_reply() {
    let (mut a, _rx) = bare();
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::AssistantStart);
    a.handle_agent(AgentEvent::ReasoningDelta { text: "reading ".into() });
    // `/goal` set while the model thinks: the agent says so with a notice and the turn goes on
    let said = "goal set: pass the tests · checked when this turn ends; the model reads it from the next turn";
    a.handle_agent(AgentEvent::Notice { level: Level::Info, text: said.into(), problem: None });
    a.handle_agent(AgentEvent::ReasoningDelta { text: "the file".into() });
    a.handle_agent(AgentEvent::TextDelta { text: "it ".into() });
    a.handle_agent(AgentEvent::Notice { level: Level::Info, text: "another".into(), problem: None });
    a.handle_agent(AgentEvent::TextDelta { text: "works".into() });
    a.handle_agent(AgentEvent::AssistantDone { message: Message::assistant("it works") });
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    assert!(matches!(&a.blocks[0], Block::Reasoning { text, streaming: false, .. } if text == "reading the file"));
    assert!(matches!(&a.blocks[1], Block::Notice { .. }));
    assert!(matches!(&a.blocks[2], Block::Assistant { text, streaming: false, .. } if text == "it works"));
    assert!(matches!(&a.blocks[3], Block::Notice { .. }));
    assert_eq!(a.blocks.len(), 4, "{:?}", a.blocks.len());
    assert!(!a.blocks.iter().any(|b| b.is_live()), "no spinner left");
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
    assert_eq!(a.unseen(), 1, "one message arrived, however many rows it takes");
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
fn a_streaming_reply_that_grows_while_scrolled_up_counts_as_one_new_message() {
    let (mut a, _rx) = app();
    long_answer(&mut a, 0);
    frame(&mut a, 60, 20);
    a.scroll_to_line(0, false);
    frame(&mut a, 60, 20);
    assert_eq!(a.unseen(), 0);
    let at = a.push(Block::Assistant { text: "first".into(), streaming: true, at: None, started: None, took_ms: None });
    frame(&mut a, 60, 20);
    assert_eq!(a.unseen(), 1);
    let more: Vec<String> = (0..20).map(|i| format!("more {i}")).collect();
    if let Block::Assistant { text, .. } = &mut a.blocks[at] {
        text.push_str(&format!("\n\n{}", more.join("\n\n")));
    }
    a.cache[at] = None;
    let rows = frame(&mut a, 60, 20);
    assert_eq!(a.unseen(), 1, "twenty more lines of the same reply are still one message");
    assert!(rows.iter().any(|r| r.contains("↓ 1 new")), "{rows:?}");
    long_answer(&mut a, 100);
    let rows = frame(&mut a, 60, 20);
    assert_eq!(a.unseen(), 2);
    assert!(rows.iter().any(|r| r.contains("↓ 2 new")), "{rows:?}");
}

#[test]
fn thinking_counts_from_the_request_and_says_nothing_when_the_provider_sent_no_text() {
    use std::time::Duration;
    let (mut a, _rx) = bare();
    let t0 = Instant::now();
    a.fx.clock = Some(t0);
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::AssistantStart);
    // a provider that streams its summary at the end: the events come together, 8 s after the request
    a.fx.clock = Some(t0 + Duration::from_secs(8));
    a.handle_agent(AgentEvent::ReasoningDelta { text: "**Plan**".into() });
    a.fx.clock = Some(t0 + Duration::from_millis(8002));
    a.handle_agent(AgentEvent::TextDelta { text: "ok".into() });
    assert!(matches!(&a.blocks[0], Block::Reasoning { elapsed_ms: Some(8002), .. }), "{:?}", a.blocks[0]);
    assert_eq!(head(&mut a, 0).trim(), "▸ thinking · 1 line · 8.0s");
    // encrypted reasoning: only a paragraph break comes, and there is no line at all
    a.handle_agent(AgentEvent::AssistantStart);
    a.handle_agent(AgentEvent::ReasoningDelta { text: "\n".into() });
    a.handle_agent(AgentEvent::TextDelta { text: "again".into() });
    let i = a.blocks.iter().rposition(|b| matches!(b, Block::Reasoning { .. })).unwrap();
    assert!(a.lines(i, 80).is_empty());
}

#[test]
fn a_history_of_many_calls_closes_its_replies_without_going_over_every_block_at_each_call() {
    let (mut a, _rx) = bare();
    let calls = 400;
    crate::app::submit::BLOCKS_SCANNED.with(|n| n.set(0));
    for k in 0..calls {
        let id = format!("c{k}");
        a.handle_agent(AgentEvent::AssistantStart);
        if k % 4 == 0 {
            a.handle_agent(AgentEvent::ReasoningDelta { text: "hm".into() });
        }
        a.handle_agent(AgentEvent::TextDelta { text: format!("step {k}") });
        a.handle_agent(AgentEvent::ToolCall {
            id: id.clone(),
            name: "read".into(),
            args: serde_json::json!({"path":"a"}),
            summary: "a".into(),
        });
        a.handle_agent(AgentEvent::ToolDone { id, output: ToolOutput::ok("x"), elapsed_ms: 1 });
    }
    let scanned = crate::app::submit::BLOCKS_SCANNED.with(|n| n.get());
    // every reply and reasoning block is closed, as before
    assert!(a.blocks.iter().all(|b| !b.is_live()), "a block is left streaming");
    assert_eq!(a.blocks.iter().filter(|b| matches!(b, Block::Assistant { .. })).count(), calls);
    // a block is gone over about once per reply that closes, never once per call for every block before it
    let blocks = a.blocks.len();
    assert!(scanned <= 4 * blocks + 4 * calls, "{scanned} blocks gone over for {blocks} blocks and {calls} calls");
}

#[test]
fn a_resumed_history_is_indexed_on_the_frames_after_it_is_shown_and_not_while_it_arrives() {
    let (mut a, _rx) = bare();
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("start") });
    let calls = 120;
    // the person's first message is queued too
    let mut total = "start".len();
    for k in 0..calls {
        let id = format!("c{k}");
        let out = (0..60).map(|i| format!("resumedword{k:03}x{i:03} ")).collect::<String>();
        total += out.len();
        a.handle_agent(AgentEvent::ToolCall {
            id: id.clone(),
            name: "read".into(),
            args: serde_json::json!({"path":"a"}),
            summary: "a".into(),
        });
        a.handle_agent(AgentEvent::ToolDone { id, output: ToolOutput::ok(&out), elapsed_ms: 0 });
    }
    // the history is on the screen's side at once; nothing of it went through the index yet
    assert_eq!(a.words.stats.bytes_scanned, 0, "the burst indexed words");
    assert_eq!(a.words.pending(), calls + 1);
    assert!(a.complete_suffix().is_none());
    // the clock keeps ticking fast while words wait, so the index does not crawl at the idle rate
    assert_eq!(a.load(), crate::frame::Load::Work);
    // even with animations off the load asks for frames, which the pacer then gives at frame rate
    a.opts.animations = false;
    assert_eq!(a.load(), crate::frame::Load::Work);
    // each frame indexes a slice of time, at least one step, and the work ends
    let mut frames = 0;
    let mut seen = 0;
    while a.words.pending() > 0 {
        a.tick_frame(false);
        let now = a.words.stats.bytes_scanned;
        assert!(now > seen, "a frame moved the index on");
        seen = now;
        frames += 1;
        assert!(frames <= total / crate::complete::STEP + calls, "{frames} frames for {total} bytes");
    }
    assert_eq!(a.words.stats.bytes_scanned, total as u64, "every word is indexed once");
    assert!(a.words.suggest("resumedword07").is_some(), "the completions work once the index is done");
}

fn queued_history(n: usize) -> (nexo_core::agent::EventRx, mpsc::UnboundedSender<AgentEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    for i in 0..n {
        tx.send(AgentEvent::History { index: i, message: Message::user(format!("m{i}")) }).unwrap();
    }
    (rx, tx)
}

#[test]
fn a_pass_with_no_time_left_handles_one_event_and_leaves_the_rest_queued() {
    let (mut a, _rx) = bare();
    let (mut rx, _tx) = queued_history(50);
    let first = rx.try_recv().unwrap();
    assert_eq!(a.drain_events(first, &mut rx, std::time::Duration::ZERO), 1);
    assert_eq!(rx.len(), 49, "the others wait for the next pass, after a draw and the keys");
}

#[test]
fn a_pass_with_time_takes_everything_queued_behind_the_first() {
    let (mut a, _rx) = bare();
    let (mut rx, _tx) = queued_history(50);
    let first = rx.try_recv().unwrap();
    assert_eq!(a.drain_events(first, &mut rx, std::time::Duration::from_secs(30)), 50);
    assert!(rx.is_empty());
}

#[test]
fn a_call_about_a_shell_job_carries_its_whole_command() {
    use nexo_core::jobs::{JobInfo, JobKind, JobStatus};
    let (mut a, _rx) = bare();
    let cmd = "cd /home/u/nexo && ls -d target\necho done";
    let job = |id, kind, label: &str| JobInfo {
        id,
        kind,
        label: label.into(),
        status: JobStatus::Running,
        elapsed_ms: 0,
        lines: 0,
        held: false,
    };
    a.handle_agent(AgentEvent::Jobs { jobs: vec![job(1, JobKind::Shell, cmd), job(2, JobKind::Agent, "audit")] });
    for (id, job) in [("j1", 1), ("j2", 2), ("j3", 9)] {
        a.handle_agent(AgentEvent::ToolCall {
            id: id.into(),
            name: "jobs".into(),
            args: serde_json::json!({"action": "output", "id": job, "wait": 600}),
            summary: "jobs: waiting".into(),
        });
    }
    let command = |id: &str| match &a.blocks[a.tool_index(id).unwrap()] {
        Block::Tool { args, .. } => args.get("command").and_then(|c| c.as_str()).map(String::from),
        _ => None,
    };
    assert_eq!(command("j1").as_deref(), Some(cmd));
    // a sub-agent has no command, and an unknown job has nothing to show
    assert_eq!(command("j2"), None);
    assert_eq!(command("j3"), None);
}

#[test]
fn a_message_that_opens_or_goes_back_to_a_topic_carries_its_line() {
    use nexo_core::{CellKind, CellState, ContextCell, ContextZone};
    let (mut a, _rx) = bare();
    let users: Vec<usize> = ["fix the NAS", "and the guard", "make the ad", "back to the NAS"]
        .iter()
        .map(|t| a.push(Block::User { text: (*t).into(), at: None }))
        .collect();
    let zone = |l: &str| ContextZone { label: l.into(), active: true, card: false };
    let zones = vec![zone("anchor"), zone("fix the NAS"), zone("make the ad")];
    let cells = [1, 1, 2, 1]
        .iter()
        .map(|&z| ContextCell { kind: CellKind::Request, tokens: 10, state: CellState::Whole, zone: z })
        .collect();
    a.handle_agent(AgentEvent::ContextMap { cells, zones, calls: Vec::new(), call_tokens: Vec::new() });
    let head =
        |a: &mut App, i: usize| a.lines(i, 80)[0].spans.iter().map(|s| s.content.to_string()).collect::<String>();
    assert!(head(&mut a, users[0]).contains("new topic"));
    assert!(!head(&mut a, users[1]).contains("──"), "the same topic goes on without a line");
    assert!(head(&mut a, users[2]).contains("new topic"));
    // the way back says how much of the earlier topic the model carries again
    assert!(head(&mut a, users[3]).contains("↺ earlier topic · 2 msgs"), "{}", head(&mut a, users[3]));
}
