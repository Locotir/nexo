use super::*;

#[test]
fn target_is_unknown_until_resolved_and_reaches_live_and_late_readers() {
    let subs = Subs::default();
    let mut rx = subs.changes();
    let run = subs.begin("call#1", "explore", "task", false);
    assert_eq!(subs.list()[0].provider, None);
    run.target("xai", "grok-test");
    let info = &subs.list()[0];
    assert_eq!((info.provider.as_deref(), info.model.as_deref()), (Some("xai"), Some("grok-test")));
    assert!(kinds(&mut rx).iter().any(|e| matches!(e, SubChange::State(i) if i.model.as_deref() == Some("grok-test"))));
    let mut old = serde_json::to_value(info).unwrap();
    old.as_object_mut().unwrap().remove("provider");
    old.as_object_mut().unwrap().remove("model");
    let old: SubInfo = serde_json::from_value(old).unwrap();
    assert!(old.provider.is_none() && old.model.is_none());
}

fn kinds(rx: &mut broadcast::Receiver<SubChange>) -> Vec<SubChange> {
    let mut out = Vec::new();
    while let Ok(c) = rx.try_recv() {
        out.push(c);
    }
    out
}

#[test]
fn items_events_and_transcript_agree_and_a_run_ends_once() {
    let subs = Subs::new();
    let mut rx = subs.changes();
    let run = subs.begin("a#1", "explore", "look at x\nmore", false);
    run.item(SubItem::Text { text: "found it".into() });
    run.end(SubState::Ok);
    run.end(SubState::Failed);
    let evs = kinds(&mut rx);
    assert!(matches!(&evs[0], SubChange::State(i) if i.state == SubState::Attached && i.title == "look at x"));
    let from_events: Vec<SubItem> = evs
        .iter()
        .filter_map(|c| match c {
            SubChange::Item { item, .. } => Some(item.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(subs.transcript("a#1").unwrap().items, from_events);
    assert!(matches!(evs.last(), Some(SubChange::State(i)) if i.state == SubState::Ok));
    assert_eq!(evs.iter().filter(|c| matches!(c, SubChange::State(_))).count(), 2);
}

#[test]
fn the_newest_200_items_stay_and_the_counter_says_how_many_went() {
    let subs = Subs::new();
    let run = subs.begin("a#1", "explore", "t", true);
    for i in 0..299 {
        run.item(SubItem::Text { text: format!("{i}") });
    }
    let t = subs.transcript("a#1").unwrap();
    assert_eq!((t.items.len(), t.dropped), (KEEP_ITEMS, 100));
    assert_eq!(t.items.last(), Some(&SubItem::Text { text: "298".into() }));
}

#[test]
fn a_long_text_is_cut_to_the_byte_bound_on_a_character_boundary() {
    let subs = Subs::new();
    let run = subs.begin("a#1", "explore", "t", true);
    run.item(SubItem::Text { text: "é".repeat(512 * 1024) });
    let t = subs.transcript("a#1").unwrap();
    let Some(SubItem::Text { text }) = t.items.last() else { panic!("no text item") };
    assert!(text.len() <= KEEP_BYTES, "{}", text.len());
    assert!(text.ends_with("bytes cut]") && text.starts_with('é'));
}

#[test]
fn tool_payloads_are_bounded_and_live_matches_snapshot() {
    let subs = Subs::new();
    let mut rx = subs.changes();
    let run = subs.begin("a", "explore", "task", true);
    run.item(SubItem::Call {
        id: "x".into(),
        name: "read".into(),
        summary: "s".into(),
        args: "é".repeat(KEEP_TOOL_BYTES),
    });
    run.item(SubItem::Result {
        id: "x".into(),
        ok: true,
        bytes: KEEP_TOOL_BYTES * 2,
        output: "é".repeat(KEEP_TOOL_BYTES),
    });
    let t = subs.transcript("a").unwrap();
    let evs: Vec<_> = kinds(&mut rx)
        .into_iter()
        .filter_map(|c| match c {
            SubChange::Item { item, .. } => Some(item),
            _ => None,
        })
        .collect();
    assert_eq!(t.items, evs);
    assert!(
        matches!(&t.items[1], SubItem::Call { args, .. } if args.len() <= KEEP_TOOL_BYTES && args.ends_with("bytes cut]"))
    );
    assert!(
        matches!(&t.items[2], SubItem::Result { output, bytes, .. } if output.len() <= KEEP_TOOL_BYTES && output.ends_with("bytes cut]") && *bytes == KEEP_TOOL_BYTES * 2)
    );
    assert!(t.items.iter().map(SubItem::size).sum::<usize>() <= KEEP_BYTES);
}

#[test]
fn partial_is_snapshotted_bounded_and_reset_between_attempts() {
    let subs = Subs::new();
    let run = subs.begin("a", "general", "task", true);
    run.delta("first ");
    run.delta("attempt");
    assert_eq!(subs.transcript("a").unwrap().partial, "first attempt");
    run.reset_partial();
    assert!(subs.transcript("a").unwrap().partial.is_empty());
    run.delta(&"é".repeat(KEEP_BYTES));
    let partial = subs.transcript("a").unwrap().partial;
    assert!(partial.len() <= KEEP_BYTES && partial.ends_with("bytes cut]"));
    run.item(SubItem::Text { text: "finished".into() });
    assert!(subs.transcript("a").unwrap().partial.is_empty());
}

#[test]
fn many_texts_stay_within_the_byte_bound() {
    let subs = Subs::new();
    let run = subs.begin("a#1", "explore", "t", true);
    for _ in 0..10 {
        run.item(SubItem::Text { text: "x".repeat(100 * 1024) });
    }
    let t = subs.transcript("a#1").unwrap();
    let bytes: usize = t.items.iter().map(SubItem::size).sum();
    assert!(bytes <= KEEP_BYTES && t.dropped >= 7, "{bytes} {}", t.dropped);
}

#[test]
fn only_the_newest_32_ended_runs_stay_and_running_ones_always() {
    let subs = Subs::new();
    let live = subs.begin("live", "explore", "t", true);
    for i in 0..40 {
        subs.begin(&format!("r{i}"), "explore", "t", true).end(SubState::Ok);
    }
    let ids: Vec<String> = subs.list().into_iter().map(|i| i.id).collect();
    assert_eq!(ids.len(), KEEP_RUNS + 1);
    assert!(ids.contains(&"live".to_string()) && ids.contains(&"r39".to_string()) && !ids.contains(&"r0".to_string()));
    drop(live);
}

#[test]
fn a_long_run_that_ends_last_is_kept_and_the_oldest_ended_one_goes() {
    let subs = Subs::new();
    let long = subs.begin("long", "explore", "t", true);
    for i in 0..KEEP_RUNS {
        subs.begin(&format!("s{i}"), "explore", "t", true).end(SubState::Ok);
    }
    long.item(SubItem::Text { text: "the whole report".into() });
    long.end(SubState::Ok);
    let ids: Vec<String> = subs.list().into_iter().map(|i| i.id).collect();
    assert_eq!(ids.len(), KEEP_RUNS);
    assert!(ids.contains(&"long".to_string()), "{ids:?}");
    assert!(!ids.contains(&"s0".to_string()) && ids.contains(&"s1".to_string()), "{ids:?}");
    assert_eq!(subs.transcript("long").unwrap().items.len(), 2);
}

#[test]
fn a_message_goes_to_a_running_run_only_and_a_dead_or_unknown_one_says_so() {
    let subs = Subs::new();
    let mut rx = subs.changes();
    let a = subs.begin("a", "explore", "t", true);
    let b = subs.begin("b", "explore", "t", true);
    assert!(subs.submit("a", "hello").is_ok());
    assert_eq!(b.take_inbox(), Vec::<SubItem>::new());
    assert_eq!(a.take_inbox(), vec![SubItem::Person { text: "hello".into() }]);
    a.end(SubState::Ok);
    assert!(subs.submit("a", "late").is_err());
    assert!(subs.submit("zzz", "who").is_err());
    assert!(!a.has_mail());
    let nd: Vec<String> = kinds(&mut rx)
        .into_iter()
        .filter_map(|c| match c {
            SubChange::NotDelivered { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(nd, vec!["a".to_string(), "zzz".to_string()]);
}

#[test]
fn the_parent_s_message_waits_marked_as_its_and_a_refusal_is_only_its_reason() {
    let subs = Subs::new();
    let mut rx = subs.changes();
    let a = subs.begin("c#3", "general", "t", true);
    subs.submit_from_parent("c#3", "also check b.rs").unwrap();
    subs.submit("c#3", "and a.rs").unwrap();
    assert_eq!(
        a.take_inbox(),
        vec![SubItem::Parent { text: "also check b.rs".into() }, SubItem::Person { text: "and a.rs".into() }]
    );
    subs.submit_from_parent("c#3", "unread").unwrap();
    subs.submit_from_parent("c#3", "unread too").unwrap();
    // only the parent's own messages are counted, for the report the ending brings
    assert_eq!(a.end(SubState::Ok), 2);
    assert_eq!(a.end(SubState::Ok), 0, "ending twice counts nothing");
    assert_eq!(subs.submit_from_parent("c#3", "late").unwrap_err(), "that sub-agent has already ended");
    assert!(!kinds(&mut rx).iter().any(|c| matches!(c, SubChange::NotDelivered { .. })));
}

#[test]
fn a_message_not_read_when_the_run_ends_is_reported_not_kept() {
    let subs = Subs::new();
    let mut rx = subs.changes();
    let a = subs.begin("a", "explore", "t", true);
    subs.submit("a", "too late").unwrap();
    a.end(SubState::Failed);
    assert!(!a.has_mail());
    let evs = kinds(&mut rx);
    assert!(evs
        .iter()
        .any(|c| matches!(c, SubChange::NotDelivered { id, text, .. } if id == "a" && text == "too late")));
}

#[test]
fn a_run_dropped_without_an_end_is_killed_and_nobody_listening_changes_nothing() {
    let subs = Subs::new();
    // no receiver at all
    let a = subs.begin("a", "explore", "t", false);
    a.item(SubItem::Text { text: "x".into() });
    drop(a);
    let i = &subs.list()[0];
    assert_eq!(i.state, SubState::Killed);
    assert!(i.ended_ms.is_some());
    assert_eq!(subs.transcript("a").unwrap().items.len(), 2);
}

#[test]
fn a_receiver_that_falls_behind_loses_events_and_the_run_is_whole() {
    let subs = Subs::new();
    let mut rx = subs.changes();
    let a = subs.begin("a", "explore", "t", true);
    for i in 0..(CHANGES + 100) {
        a.item(SubItem::Text { text: format!("{i}") });
    }
    assert!(matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Lagged(_))));
    assert_eq!(subs.transcript("a").unwrap().items.len(), KEEP_ITEMS);
}

#[test]
fn spending_adds_up_and_going_to_the_background_is_one_event() {
    let subs = Subs::new();
    let mut rx = subs.changes();
    let a = subs.begin("a", "explore", "t", false);
    a.spent(&Usage { input: 10, output: 5, cache_read: 3, ..Default::default() }, 0.5);
    a.spent(&Usage { input: 1, output: 1, ..Default::default() }, 0.25);
    subs.to_background("a");
    subs.to_background("a");
    let i = &subs.list()[0];
    assert_eq!((i.tokens_in, i.tokens_out, i.cost_usd, i.state), (14, 6, 0.75, SubState::Background));
    let states = kinds(&mut rx).into_iter().filter(|c| matches!(c, SubChange::State(_))).count();
    assert_eq!(states, 2);
}

#[test]
fn clear_forgets_every_run() {
    let subs = Subs::new();
    subs.begin("a", "explore", "t", true).end(SubState::Ok);
    subs.clear();
    assert!(subs.list().is_empty() && subs.transcript("a").is_none());
}

#[test]
fn a_run_that_winds_down_after_a_clear_tells_nobody() {
    let subs = Subs::new();
    let old = subs.begin("old", "explore", "t", true);
    subs.clear();
    let mut rx = subs.changes();
    old.item(SubItem::Text { text: "late".into() });
    old.delta("late");
    old.spent(&Usage::default(), 0.5);
    old.end(SubState::Failed);
    drop(old);
    assert!(kinds(&mut rx).is_empty());
    assert!(subs.list().is_empty());
    // a run of the new session is heard
    let new = subs.begin("new", "explore", "t", true);
    new.end(SubState::Ok);
    assert!(!kinds(&mut rx).is_empty());
    assert_eq!(subs.list().len(), 1);
}

#[test]
fn a_run_can_carry_its_own_title_and_its_meter_outlives_the_list() {
    let subs = Subs::new();
    let run = subs.begin_titled("a", "explore", "flow · Find · find #2\nignored", "the task\nmore", true);
    assert_eq!(subs.list()[0].title, "flow · Find · find #2");
    let meter = run.meter();
    run.spent(&Usage { input: 7, output: 3, ..Default::default() }, 0.0);
    run.end(SubState::Ok);
    // 32 runs ended after it push it out of the list
    for i in 0..KEEP_RUNS + 1 {
        subs.begin(&format!("b{i}"), "explore", "t", true).end(SubState::Ok);
    }
    assert!(subs.list().iter().all(|r| r.id != "a"));
    assert_eq!(meter.tokens(), (7, 3));
}
