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
    let mut e = Engine::new(params(&format!("threshold=1;still=0;nudge=0;protect=0;cap={cap};floor=0.5")).unwrap());
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
    let mut e = Engine::new(params(&format!("threshold=1;still=0;nudge=0;cap={cap};floor={floor}")).unwrap());
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
    let mut e = Engine::new(params(&format!("threshold=1;still=0;nudge=0;cap={cap};floor={floor}")).unwrap());
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
    let mut e = Engine::new(params(&format!("threshold=1;still=0;nudge=0;cap={cap};floor={floor}")).unwrap());
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
