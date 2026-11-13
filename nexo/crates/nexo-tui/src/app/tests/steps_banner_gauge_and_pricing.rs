//! Turn steps, the banner, the todo result, the gauge and pricing.

use super::*;

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
    a.handle_agent(AgentEvent::Notice { level: Level::Info, text: "hello".into(), problem: None });
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
