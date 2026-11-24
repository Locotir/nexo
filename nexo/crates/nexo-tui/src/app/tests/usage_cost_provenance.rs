//! Usage totals have no billing basis or group identity; they must not become group charges.

use super::*;

fn providers(a: &mut App) {
    let api = nexo_core::ProviderConfig::new("openai", "http://api", None);
    let mut plan = api.clone();
    plan.login = Some("codex".into());
    a.opts.providers = vec![("api".into(), api), ("plan".into(), plan)];
    a.opts.animations = false;
}

fn model(a: &mut App, provider: &str) {
    a.handle_agent(AgentEvent::ModelChanged { provider: provider.into(), model: "m".into() });
}

// The same message/tool event sequence replay_history emits, also used for live tool rounds.
fn group_events(a: &mut App, index: usize, prefix: &str) -> usize {
    let calls: Vec<_> = (0..2).map(|n| (format!("{prefix}-{n}"), serde_json::json!({"command": "true"}))).collect();
    a.handle_agent(AgentEvent::History {
        index,
        message: Message::new(
            Role::Assistant,
            calls.iter().map(|(id, args)| Part::tool_call(id, "bash", args.clone())).collect(),
        ),
    });
    for (id, args) in calls {
        a.handle_agent(AgentEvent::ToolCall { id: id.clone(), name: "bash".into(), args, summary: "true".into() });
        a.handle_agent(AgentEvent::ToolDone { id, output: ToolOutput::ok(""), elapsed_ms: 0 });
    }
    a.blocks.iter().rposition(|b| matches!(b, Block::Group { .. })).expect("events create a tool group")
}

fn uncharged(a: &mut App, group: usize) {
    assert_eq!(a.looks[group].spent, 0.0, "unattributed totals must not be assigned to a group");
    let text = head(a, group);
    assert!(!text.contains('$') && !text.contains("API"), "unproven billing basis: {text}");
}

#[test]
fn usage_cost_provider_switch_before_result_does_not_relabel_or_attribute() {
    for (from, to) in [("api", "plan"), ("plan", "api")] {
        let (mut a, _rx) = bare();
        providers(&mut a);
        model(&mut a, from);
        a.handle_agent(AgentEvent::TurnStart { turn: 1 });
        a.handle_agent(AgentEvent::AssistantStart);
        let group = group_events(&mut a, 0, "in-flight");
        model(&mut a, to);
        let usage = Usage { input: 100, output: 10, ..Default::default() };
        a.handle_agent(AgentEvent::Usage { turn: usage, call: usage, session: usage, cost_usd: 2.0 });
        assert_eq!(a.cost, 2.0, "session accounting remains intact");
        assert_eq!(a.session_usage, usage);
        uncharged(&mut a, group);
        a.handle_agent(AgentEvent::GroupCost {
            call_ids: vec!["in-flight-0".into(), "in-flight-1".into()],
            cost_usd: 2.0,
            plan: from == "plan",
        });
        let text = head(&mut a, group);
        assert!(text.contains("$2.00"), "known costs remain visible: {text}");
        assert_eq!(text.contains("API"), from == "plan", "use request basis: {text}");
    }
}

#[test]
fn usage_cost_other_provider_side_call_never_charges_main_group() {
    let (mut a, _rx) = bare();
    providers(&mut a);
    model(&mut a, "api");
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    let group = group_events(&mut a, 0, "main");
    let main = Usage { input: 100, output: 10, ..Default::default() };
    a.handle_agent(AgentEvent::Usage { turn: main, call: main, session: main, cost_usd: 1.0 });
    a.handle_agent(AgentEvent::GroupCost {
        call_ids: vec!["main-0".into(), "main-1".into()],
        cost_usd: 1.0,
        plan: false,
    });
    // StateHost::spent uses an empty `call`, even when the worker bills another provider.
    let side = Usage { input: 200, output: 20, ..Default::default() };
    a.handle_agent(AgentEvent::Usage { turn: side, call: Usage::default(), session: main + side, cost_usd: 3.0 });
    assert_eq!(a.cost, 3.0);
    assert_eq!(a.turn_usage, main + side);
    assert_eq!(a.session_usage, main + side);
    assert_eq!(a.looks[group].spent, 1.0, "side call does not pollute the known main cost");
    let text = head(&mut a, group);
    assert!(text.contains("$1.00") && !text.contains("API"), "{text}");
}

#[test]
fn usage_cost_replay_two_turns_does_not_charge_session_total_to_last_group() {
    let (mut a, _rx) = bare();
    providers(&mut a);
    a.handle_agent(AgentEvent::HistoryCleared);
    model(&mut a, "api");
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("first turn") });
    let first = group_events(&mut a, 1, "first");
    model(&mut a, "plan");
    a.handle_agent(AgentEvent::History { index: 2, message: Message::user("second turn") });
    let last = group_events(&mut a, 3, "last");
    assert_ne!(first, last);
    let total = Usage { input: 300, output: 30, ..Default::default() };
    // Exact aggregate event shape from replay_history, with no per-turn/provider ledger.
    a.handle_agent(AgentEvent::Usage { turn: Usage::default(), call: Usage::default(), session: total, cost_usd: 7.0 });
    assert_eq!(a.cost, 7.0);
    assert_eq!(a.session_usage, total);
    assert!(a.turn_usage.is_empty());
    uncharged(&mut a, first);
    uncharged(&mut a, last);
}

// Live producer order: assistant message, tool lifecycle, then the request's cost event.
fn live_tools(a: &mut App, calls: &[(&str, &str)], usd: f64, plan: bool) {
    a.handle_agent(AgentEvent::AssistantStart);
    a.handle_agent(AgentEvent::AssistantDone {
        message: Message::new(
            Role::Assistant,
            calls.iter().map(|(id, name)| Part::tool_call(*id, *name, serde_json::json!({}))).collect(),
        ),
    });
    for (id, name) in calls {
        a.handle_agent(AgentEvent::ToolCall {
            id: (*id).into(),
            name: (*name).into(),
            args: serde_json::json!({}),
            summary: (*name).into(),
        });
        a.handle_agent(AgentEvent::ToolDone { id: (*id).into(), output: ToolOutput::ok(""), elapsed_ms: 0 });
    }
    a.handle_agent(AgentEvent::GroupCost {
        call_ids: calls.iter().map(|(id, _)| (*id).into()).collect(),
        cost_usd: usd,
        plan,
    });
}

#[test]
fn usage_cost_late_group_labels_only_known_subtotal_not_total() {
    for plan in [false, true] {
        let (mut a, _rx) = bare();
        providers(&mut a);
        a.handle_agent(AgentEvent::TurnStart { turn: 1 });
        live_tools(&mut a, &[("first", "read")], 2.0, plan);
        assert!(!a.blocks.iter().any(|b| matches!(b, Block::Group { .. })));
        live_tools(&mut a, &[("second", "read")], 3.0, plan);
        let group = a.blocks.iter().position(|b| matches!(b, Block::Group { .. })).unwrap();
        assert_eq!(a.looks[group].spent, 3.0, "do not invent retrospective attribution");
        let text = head(&mut a, group);
        assert!(text.contains("known subtotal") && text.contains("$3.00"), "{text}");
        assert!(!text.contains("$5.00"), "not a complete total: {text}");
        assert_eq!(text.contains("API"), plan);
    }
}

#[test]
fn usage_cost_mixed_groups_and_skill_never_claim_partial_cost_is_total() {
    let (mut a, _rx) = bare();
    providers(&mut a);
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    live_tools(&mut a, &[("a", "read"), ("b", "read")], 2.0, false);
    let first = a.blocks.iter().position(|b| matches!(b, Block::Group { .. })).unwrap();
    // The skill splits the request across two tool groups. No arbitrary apportionment.
    live_tools(&mut a, &[("c", "read"), ("skill", "skill"), ("d", "read"), ("e", "read")], 9.0, false);
    let second = a.blocks.iter().rposition(|b| matches!(b, Block::Group { .. })).unwrap();
    assert_ne!(first, second);
    assert_eq!(a.looks[first].spent, 2.0);
    uncharged(&mut a, second);
    live_tools(&mut a, &[("f", "read")], 3.0, false);
    assert_eq!(a.looks[second].spent, 3.0);
    for group in [first, second] {
        let text = head(&mut a, group);
        assert!(text.contains("known subtotal"), "omitted mixed request is not a total: {text}");
        assert!(!text.contains("$9.00") && !text.contains("$12.00"), "{text}");
    }
    // A request mixing one group and skill also cannot allocate its full price to the group.
    live_tools(&mut a, &[("g", "read"), ("skill-2", "skill")], 7.0, false);
    assert_eq!(a.looks[second].spent, 3.0);
    assert!(head(&mut a, second).contains("known subtotal"));
}

#[test]
fn usage_cost_same_provider_is_still_not_group_provenance() {
    let (mut a, _rx) = bare();
    providers(&mut a);
    model(&mut a, "api");
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    let group = group_events(&mut a, 0, "earlier");
    a.handle_agent(AgentEvent::AssistantStart);
    let usage = Usage { input: 100, output: 10, ..Default::default() };
    a.handle_agent(AgentEvent::Usage { turn: usage, call: usage, session: usage, cost_usd: 2.0 });
    uncharged(&mut a, group);
}

#[test]
fn usage_cost_exact_ids_charge_original_group_not_latest_and_hide_mixed_basis() {
    let (mut a, _rx) = bare();
    providers(&mut a);
    let first = group_events(&mut a, 0, "original");
    a.handle_agent(AgentEvent::History { index: 1, message: Message::user("next turn") });
    let last = group_events(&mut a, 2, "newer");
    a.handle_agent(AgentEvent::GroupCost {
        call_ids: vec!["original-0".into(), "original-1".into()],
        cost_usd: 2.0,
        plan: false,
    });
    assert!(head(&mut a, first).contains("$2.00"));
    uncharged(&mut a, last);
    for ids in [vec!["missing".into()], vec!["original-0".into(), "newer-1".into()]] {
        a.handle_agent(AgentEvent::GroupCost { call_ids: ids, cost_usd: 9.0, plan: false });
        assert_eq!(a.looks[first].spent, 2.0);
        uncharged(&mut a, last);
    }
    a.handle_agent(AgentEvent::GroupCost {
        call_ids: vec!["original-0".into(), "original-1".into()],
        cost_usd: 3.0,
        plan: true,
    });
    assert_eq!(a.looks[first].spent, 5.0);
    assert!(!head(&mut a, first).contains('$'), "do not label mixed billing as cash or equivalent");
}
