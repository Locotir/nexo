//! Messages as the view sends them (engine::view).

use super::*;

#[test]
fn a_long_paste_among_the_latest_messages_is_not_cut_around_on_every_request() {
    // a request answered in ten small steps, then a long log pasted into the next request: while the log
    // is among the protected latest messages no cut can take it, and cutting what is before it a little
    // on every request would make the provider read the whole view again each time
    let mut msgs = vec![json!({"role": "user", "content": "List the slots of listing.txt one range at a time."})];
    for k in 0..10 {
        msgs.extend(step(k, &format!("sed -n {k}p listing.txt"), &listing(10 * k, 10 * k + 9)));
    }
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Done: 100 slots listed."}]}));
    let log: String = (0..1500)
        .map(|k| format!("run {k} failed at {} with {}", word(k + 5000), word(k + 9000)))
        .collect::<Vec<_>>()
        .join("\n");
    msgs.push(json!({"role": "user", "content": format!("The run failed, here is its log:\n{log}")}));
    let paste = msgs.len() - 1;
    for k in 10..20 {
        msgs.extend(step(k, &format!("grep -n {} run.log", word(k + 5000)), &format!("{k}: run {k} failed")));
    }
    let mut e = Engine::new(params("nudge=0;eoq=2000;protect=12").unwrap());
    let mut seen = Vec::new();
    for n in (paste + 1..=msgs.len()).step_by(2) {
        e.sync(&msgs[..n]);
        let view = sent(&e.view());
        seen.push((n, e.stats.get("miss").copied().unwrap_or(0), view.contains(log.lines().nth(700).unwrap())));
    }
    let (held, after): (Vec<&(usize, i64, bool)>, Vec<_>) = seen.iter().partition(|s| s.0 <= paste + 12);
    assert!(held.iter().all(|s| s.1 == held[0].1 && s.2), "the view grows as sent: {seen:?}");
    assert!(e.stats.get("cap_stuck").is_some_and(|n| *n > 0), "{:?}", e.stats);
    // once the log is out of the tail a cut makes room, and the view is cut
    assert!(after.last().is_some_and(|s| s.1 > held[0].1), "{seen:?}");
}

#[test]
fn the_reasoning_of_the_latest_reply_stays_with_its_calls_whatever_the_tail() {
    let thought = (0..120).map(|k| format!("maybe {} then {}", word(k), word(k + 500))).collect::<Vec<_>>().join("\n");
    let msgs = vec![
        json!({"role": "user", "content": "Find slot 50."}),
        json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": thought, "signature": "s"},
                {"type": "tool_use", "id": "call_1", "name": "bash", "input": {"command": "cat listing.txt"}}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": listing(1, 120)}]}),
    ];
    let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
    whole.sync(&msgs);
    let cap = whole.tokens() - 1;
    // no tail kept whole and every part a guess: the reasoning of the reply the request goes back to stays
    let mut e =
        Engine::new(params(&format!("threshold=1;still=0;nudge=0;protect=0;eoq=0;target={cap};floor=0.5")).unwrap());
    e.sync(&msgs);
    let view = e.view();
    let kept = blocks(view[1].get("content")).iter().any(|b| get_str(b, "thinking") == thought);
    assert!(kept && e.stats.contains_key("cap_rewrites"), "{:?} {:?}", view[1], e.stats);
}

#[test]
fn a_long_argument_left_out_says_the_file_on_disk_is_current_only_for_a_write() {
    let script = (0..40).map(|k| format!("echo {} {}", word(k), word(k + 70))).collect::<Vec<_>>().join("\n");
    let mut msgs = vec![json!({"role": "user", "content": "Run the checks."})];
    msgs.extend(step(1, &script, "ok"));
    for k in 2..8 {
        msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
    }
    let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
    whole.sync(&msgs);
    let total = whole.tokens();
    // a floor that leaving most of the script is enough for: no message has to leave
    let cap = total - 1;
    let floor = (total - est_str(&script) / 2) as f64 / cap as f64;
    let mut e = Engine::new(params(&format!("threshold=1;still=0;nudge=0;eoq=0;target={cap};floor={floor}")).unwrap());
    e.sync(&msgs);
    let call = e.view()[1].to_string();
    assert!(call.contains("lines of this argument here are omitted"), "{call} {:?}", e.stats);
    assert!(!call.contains("the file on disk is current"), "{call}");
}

#[test]
fn a_list_of_changes_folds_each_long_text_like_one_edit_does() {
    let old = (0..40).map(|k| format!("let {} = {};", word(k), k)).collect::<Vec<_>>().join("\n");
    let new = (0..40).map(|k| format!("let {} = {};", word(k + 70), k)).collect::<Vec<_>>().join("\n");
    let input = json!({"path": "a.rs", "edits": [{"old_string": old, "new_string": new}, {"old_string": "a", "new_string": "b"}]});
    let mut msgs = vec![
        json!({"role": "user", "content": "Rename the bindings."}),
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": "call_1", "name": "edit", "input": input}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": "edited a.rs"}]}),
    ];
    for k in 2..8 {
        msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
    }
    let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
    whole.sync(&msgs);
    let total = whole.tokens();
    let cap = total - 1;
    let floor = (total - est_str(&old)) as f64 / cap as f64;
    let mut e = Engine::new(params(&format!("threshold=1;still=0;nudge=0;eoq=0;target={cap};floor={floor}")).unwrap());
    e.sync(&msgs);
    let call = e.view()[1].to_string();
    assert!(call.contains("lines this edit replaced here are omitted; they are no longer in the file"), "{call}");
    assert!(call.contains("lines written here are omitted; the file on disk is current"), "{call}");
    // the short change stays as it was
    assert!(call.contains(r#"{"old_string":"a","new_string":"b"}"#), "{call}");
}

#[test]
fn the_text_an_edit_replaced_is_not_said_to_be_in_the_file() {
    let old = (0..40).map(|k| format!("let {} = {};", word(k), k)).collect::<Vec<_>>().join("\n");
    let new = (0..40).map(|k| format!("let {} = {};", word(k + 70), k)).collect::<Vec<_>>().join("\n");
    let input = json!({"path": "a.rs", "old_string": old, "new_string": new});
    let mut msgs = vec![
        json!({"role": "user", "content": "Rename the bindings."}),
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": "call_1", "name": "edit", "input": input}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": "edited a.rs"}]}),
    ];
    for k in 2..8 {
        msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
    }
    let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
    whole.sync(&msgs);
    let total = whole.tokens();
    let cap = total - 1;
    let floor = (total - est_str(&old)) as f64 / cap as f64;
    let mut e = Engine::new(params(&format!("threshold=1;still=0;nudge=0;eoq=0;target={cap};floor={floor}")).unwrap());
    e.sync(&msgs);
    let call = e.view()[1].to_string();
    assert!(call.contains("lines this edit replaced here are omitted; they are no longer in the file"), "{call}");
    assert!(call.contains("lines written here are omitted; the file on disk is current"), "{call}");
}

#[test]
fn what_is_added_to_a_message_that_came_with_no_text_leaves_no_empty_text_block() {
    for content in [json!(""), json!([]), json!([{"type": "text", "text": ""}])] {
        let mut m = json!({"role": "user", "content": content});
        add_blocks(&mut m, [text_block("[curator] the map")]);
        assert_eq!(m["content"], json!([{"type": "text", "text": "[curator] the map"}]));
    }
    let mut m = json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a", "content": ""}]});
    add_blocks(&mut m, [text_block("x")]);
    assert_eq!(m["content"].as_array().map(Vec::len), Some(2));
}

#[test]
fn one_long_line_dense_in_tokens_is_cut_to_its_budget_without_a_panic() {
    // control bytes weigh more than a quarter token each: the budget holds fewer characters than four a token
    let dense = "\u{1}".repeat(100);
    let cut = head_and_tail(&dense, 100);
    assert!(cut.contains("characters cut") && cut.chars().filter(|c| *c == '\u{1}').count() < 100, "{cut:?}");
    // quotes weigh half a token: the head and tail never overlap
    let quotes = "\"".repeat(100);
    let cut = head_and_tail(&quotes, 30);
    assert!(cut.chars().filter(|c| *c == '"').count() < 100, "{cut:?}");
}

#[test]
fn cards_negative_limit_counts_hidden() {
    let mut msgs: Vec<Value> = Vec::new();
    for k in 0..6 {
        if k > 0 {
            msgs.push(
                json!({"role": "assistant", "content": [{"type": "text", "text": format!("It is {}.", word(k + 50))}]}),
            );
        }
        msgs.push(json!({"role": "user", "content": format!("What does {} stand for?", word(k))}));
    }
    let mut e = Engine::new(params("nudge=0;eoq=0;link=1;apart=0").unwrap());
    // the settings take no negative limit, a `Params` built in code can: all but the two oldest shown
    e.p.cards = -2;
    e.sync(&msgs);
    e.view();
    assert_eq!(e.goal.others().len(), 5);
    let text = e.cards_text();
    // five other topics, all but two shown: two are not
    assert!(text.ends_with("• … and 2 older topics."), "{text}");
    assert_eq!(text.lines().count(), 1 + 3 + 1, "{text}");
}

/// A stretch of the conversation on topic `t`: the request, `steps` reads of `lines` lines each, the reply.
/// Topic `t` always asks and reads in its own words, so a later stretch of the same `t` is a return to it.
fn stretch(t: usize, steps: usize, lines: usize, k0: usize) -> Vec<Value> {
    let w = |i: usize| word(t * 1000 + i);
    let mut msgs = vec![json!({"role": "user", "content": format!(
        "{} {} {} {} {} {} {} {} {} {} {} {}", w(1), w(2), w(3), w(4), w(5), w(6), w(21), w(22), w(23), w(24), w(25), w(26))})];
    for s in 0..steps {
        msgs.extend(step(k0 + s, &format!("cat {}.src", w(10 + s)), &source(t * 1000 + 10 * s, lines)));
    }
    msgs.push(
        json!({"role": "assistant", "content": [{"type": "text", "text": format!("{} {} {}", w(7), w(8), w(1))}]}),
    );
    msgs
}

/// The stretches of `plan` (topic, steps) one after the other, and the engine that took them in request by request.
fn conversation(spec: &str, plan: &[(usize, usize)], lines: usize) -> (Engine, Vec<Value>) {
    let mut e = Engine::new(params(spec).unwrap());
    e.tools(&["recall".to_string()]);
    let mut msgs = Vec::new();
    for &(t, steps) in plan {
        let k0 = msgs.len();
        msgs.extend(stretch(t, steps, lines, k0));
    }
    // the last stretch is open: no reply yet
    msgs.pop();
    super::recall::feed(&mut e, &msgs);
    (e, msgs)
}

const TOPICS: &str = "nudge=0;eoq=0;link=1;ahead=0.25;margin=20;ratio=0.115;cards=2";

/// The tokens the topics other than the active one hold.
fn others_hold(e: &Engine) -> i64 {
    e.topic_tokens().0.iter().map(|o| o.1).sum()
}

#[test]
fn split_horizon_follows_the_topic_length() {
    // the same tokens in the topics left behind: twelve short topics say the next one is short too, two long
    // ones say it lasts
    let short: Vec<(usize, usize)> = (0..12).map(|t| (t, 2)).chain([(50, 1)]).collect();
    let long = [(0, 12), (1, 12), (50, 1)];
    let (a, _) = conversation(&format!("{TOPICS};apart=100000000"), &short, 40);
    let (b, _) = conversation(&format!("{TOPICS};apart=100000000"), &long, 40);
    let apart = (others_hold(&a).min(others_hold(&b)) as f64 * 0.96) as i64;
    let (a, _) = conversation(&format!("{TOPICS};apart={apart}"), &short, 40);
    let (b, _) = conversation(&format!("{TOPICS};apart={apart}"), &long, 40);
    assert!(!a.split, "{:?} {:?}", a.stats, a.goal.others());
    assert!(b.split, "{:?} {:?}", b.stats, b.goal.others());
}

/// `msgs` fed to `e` request by request, `secs` seconds apart (the conversation of the stretches before the last
/// one is in `e` already).
fn feed_timed(e: &mut Engine, msgs: &[Value], secs: f64) {
    for t in 1..=msgs.len() {
        if msgs[t - 1]["role"] == "user" && t > e.tr.msgs.len() {
            e.now = secs * t as f64;
            e.sync(&msgs[..t]);
            e.view();
        }
    }
}

#[test]
fn a_likely_return_charges_the_cold_pocket() {
    // two topics the person goes back and forth between, then a third: the one just left is likely to be asked
    // for again. Split, coming back reads it without the cache unless the provider keeps its view that long
    let plan = [(0, 3), (1, 20), (2, 1)];
    let decide = |pockets: &str| {
        let mut msgs = Vec::new();
        for &(t, steps) in &plan {
            let k0 = msgs.len();
            msgs.extend(stretch(t, steps, 40, k0));
        }
        msgs.pop();
        let mut e = Engine::new(params(&format!("{TOPICS};apart=0;{pockets}")).unwrap());
        e.tools(&["recall".to_string()]);
        // the topics before the last are split (each is kept in a pocket of its own), then the split is priced
        let last = msgs.iter().rposition(|m| m["role"] == "user" && m["content"].is_string()).unwrap();
        feed_timed(&mut e, &msgs[..last], 3.0);
        // (the view the person was sent last was the whole one)
        e.p.apart = 1000;
        e.split = false;
        feed_timed(&mut e, &msgs, 3.0);
        e
    };
    let cold = decide("pockets=0");
    let warm = decide("pockets=1;life=3600");
    assert!(!cold.split, "{:?} {:?}", cold.stats, cold.goal.switches());
    assert!(warm.split, "{:?} {:?}", warm.stats, warm.goal.switches());
}

#[test]
fn message_zero_becomes_a_card_when_its_thread_leaves() {
    let plan = [(0, 3), (1, 3)];
    let (mut e, msgs) = conversation(&format!("{TOPICS};apart=0"), &plan, 40);
    assert!(e.split && e.goal.thread_at(0) != e.goal.active(), "{:?}", e.stats);
    let ask = msgs[0]["content"].as_str().unwrap().to_string();
    let view = e.view();
    let text = sent(&view);
    // the request of message 0 is said once, in the cards, with the way to bring the message back
    assert_eq!(text.matches(&ask).count(), 1, "{text}");
    assert!(text.contains("recall brings any of m00001"), "{text}");
    // and the view has shrunk by what the message held, were it sent as a message
    let (whole, now) = (e.sizes[0], e.sizes_now()[0]);
    assert!(whole > 20 && now < 10, "{whole} {now}");
    assert_eq!(view[0]["role"], "user");
}

#[test]
fn two_recalls_into_hidden_threads_unsplit_until_the_next_switch() {
    let plan = [(0, 3), (1, 3)];
    let (mut e, mut msgs) = conversation(&format!("{TOPICS};apart=0"), &plan, 40);
    assert!(e.split);
    // one lookup into the topic left out is only a lookup
    msgs.extend(super::recall::recall_step(1, 2, None, "the output"));
    super::recall::feed(&mut e, &msgs);
    assert!(e.split && e.stats.get("unsplit").is_none(), "{:?}", e.stats);
    // the second is a storm: the whole conversation is back
    msgs.extend(super::recall::recall_step(2, 4, None, "the output"));
    super::recall::feed(&mut e, &msgs);
    assert!(!e.split && e.stats.get("unsplit") == Some(&1), "{:?}", e.stats);
    let text = sent(&e.view());
    assert!(!text.contains("This conversation has other topics"), "{text}");
    assert!(text.contains(msgs[0]["content"].as_str().unwrap()));
    // it stays whole through the rest of the stretch
    msgs.extend(step(99, "ls", "a b c"));
    super::recall::feed(&mut e, &msgs);
    assert!(!e.split);
    // and the next change of topic decides again
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "ok"}]}));
    msgs.extend(stretch(7, 1, 40, 200).into_iter().take(2));
    super::recall::feed(&mut e, &msgs);
    assert!(e.split, "{:?}", e.stats);
    assert_eq!(e.stats.get("unsplit"), Some(&1));
}
