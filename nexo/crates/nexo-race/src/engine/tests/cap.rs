//! The size cap and what leaves the view past it (engine::cap).

use super::*;

#[test]
fn past_the_cap_an_old_copy_leaves_before_the_first_file_read() {
    // the file read first, then the same log read twice, then short steps
    let spec = (0..200).map(|k| format!("field_{k} = {} {}", word(k), word(k + 1000))).collect::<Vec<_>>().join("\n");
    let log =
        (0..200).map(|k| format!("event {} at {}", word(k + 2000), word(k + 3000))).collect::<Vec<_>>().join("\n");
    let mut msgs = vec![json!({"role": "user", "content": "What does the spec say the log should hold?"})];
    msgs.extend(step(1, "cat spec.txt", &spec));
    msgs.extend(step(2, "cat run.log", &log));
    msgs.extend(step(3, "cat run.log", &log));
    for k in 4..11 {
        msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
    }
    let results = |view: &[Value]| -> Vec<String> {
        view.iter()
            .flat_map(|m| blocks(m.get("content")).into_owned())
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .map(|b| text_of(b.get("content")).into_owned())
            .collect()
    };
    let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
    whole.sync(&msgs);
    let total = whole.tokens();
    // a cap one token short, and a floor that dropping one copy of the log is enough for
    let cap = total - 1;
    let floor = (total - est_str(&log) / 2) as f64 / cap as f64;
    let given = format!("threshold=0;nudge=0;eoq=0;target={cap};floor={floor};cut=age");
    let mut age = Engine::new(params(&given).unwrap());
    age.sync(&msgs);
    assert!(!results(&age.view()).contains(&spec), "the oldest messages leave first");
    // the rewrite counts what the cache misses from the first message it changed on
    let miss = |k: &str| age.stats.get(k).copied().unwrap_or(0);
    assert!(miss("miss_cap") > 0 && miss("miss") >= miss("miss_cap"), "{:?}", age.stats);
    let mut need = Engine::new(params(&format!("{given};cut=need")).unwrap());
    need.sync(&msgs);
    let kept = results(&need.view());
    assert!(kept.contains(&spec), "{kept:?}");
    assert_eq!(kept.iter().filter(|r| **r == log).count(), 1, "{kept:?}");
    // only parts a later output holds went
    assert!(need.stats.get("need_copies").is_some_and(|n| *n > 0) && !need.stats.contains_key("need_chunks"));
}

#[test]
fn what_leaves_for_the_window_counts_the_miss_from_the_anchor() {
    // no working window: with one, the ceiling cut (95 % of the budget) comes before `fit` and leaves it nothing
    let mut e =
        Engine::new(params("nudge=0;eoq=0;close=0;live=0;check=100000000;every=100000;target=0;cap=0").unwrap());
    // the request holds a long paste: the anchor is a large part of the view
    let ask = format!("Say what slot 50 holds in this listing:\n{}", listing(1000, 1150));
    let mut msgs = vec![json!({"role": "user", "content": ask})];
    for k in 0..6 {
        msgs.extend(step(k + 1, &format!("cat part{k}.txt"), &listing(60 * k + 1, 60 * k + 60)));
    }
    e.sync(&msgs);
    e.decide();
    let total = e.tokens();
    e.set_budget(total / 2);
    msgs.extend(step(7, "cat part7.txt", &listing(361, 380)));
    e.sync(&msgs);
    // `leave` writes the anchor again: the view as weighed before the window is fit misses whole
    let weighed: i64 = e.sizes_now().iter().sum();
    e.decide();
    assert!(e.stats.get("left_messages").is_some_and(|&n| n > 0), "{:?}", e.stats);
    assert_eq!(e.stats["miss_fit"], weighed, "{:?}", e.stats);
    assert!(e.stats["miss"] >= weighed, "{:?}", e.stats);
}

#[test]
fn a_view_fit_to_the_window_leaves_room_for_the_next_call() {
    // no working window: `fit` alone keeps the view inside the model's window
    let mut e =
        Engine::new(params("nudge=0;eoq=0;close=0;live=0;check=100000000;every=100000;target=0;cap=0").unwrap());
    let mut msgs = vec![json!({"role": "user", "content": "Say what each part holds."})];
    for k in 0..24 {
        msgs.extend(step(k + 1, &format!("cat part{k}.txt"), &listing(60 * k + 1, 60 * k + 60)));
    }
    e.sync(&msgs);
    e.decide();
    let budget = e.tokens() * 9 / 10;
    e.set_budget(budget);
    // each call from here on adds an output as long as the ones before it
    msgs.extend(step(25, "cat part24.txt", &listing(1441, 1500)));
    e.sync(&msgs);
    e.decide();
    assert_eq!(e.stats.get("window_fits"), Some(&1), "{:?}", e.stats);
    let landed: i64 = e.sizes_now().iter().sum();
    // the oldest messages left down to where a ceiling cut lands, not down to the edge of the window: every
    // fit reads the whole view again without the cache, so it must buy more than one call
    assert!(landed <= budget * 9 / 10, "landed on {landed} of {budget}");
    msgs.extend(step(26, "cat part25.txt", &listing(1501, 1560)));
    e.sync(&msgs);
    e.decide();
    assert_eq!(e.stats.get("window_fits"), Some(&1), "the next call was fit again: {:?}", e.stats);
    assert!(e.sizes_now().iter().sum::<i64>() <= budget);
}

#[test]
fn past_the_cap_only_copies_leave_until_twice_the_cap() {
    let spec = (0..200).map(|k| format!("field_{k} = {} {}", word(k), word(k + 1000))).collect::<Vec<_>>().join("\n");
    let log =
        (0..200).map(|k| format!("event {} at {}", word(k + 2000), word(k + 3000))).collect::<Vec<_>>().join("\n");
    let mut msgs = vec![json!({"role": "user", "content": "What does the spec say the log should hold?"})];
    msgs.extend(step(1, "cat spec.txt", &spec));
    msgs.extend(step(2, "cat run.log", &log));
    msgs.extend(step(3, "cat run.log", &log));
    for k in 4..11 {
        msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
    }
    let results = |view: &[Value]| -> Vec<String> {
        view.iter()
            .flat_map(|m| blocks(m.get("content")).into_owned())
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .map(|b| text_of(b.get("content")).into_owned())
            .collect()
    };
    let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
    whole.sync(&msgs);
    let total = whole.tokens();
    // a target far below the view: the copy goes, and nothing else while the view is under twice the cap
    let given = format!("threshold=0;nudge=0;eoq=0;target={};floor=0.2;cut=copies", total - 1);
    let mut e = Engine::new(params(&given).unwrap());
    e.sync(&msgs);
    let kept = results(&e.view());
    assert!(kept.contains(&spec), "{kept:?}");
    assert_eq!(kept.iter().filter(|r| **r == log).count(), 1, "{kept:?}");
    assert!(!e.stats.contains_key("left_messages") && !e.stats.contains_key("need_chunks"), "{:?}", e.stats);
    // past twice the cap, the cut that loses material runs as before
    let given = format!("threshold=0;nudge=0;eoq=0;target={};floor=0.6;cut=copies", total / 4);
    let mut e = Engine::new(params(&given).unwrap());
    e.sync(&msgs);
    assert!(!results(&e.view()).contains(&spec));
}

#[test]
fn past_the_cap_what_tells_the_most_stays_whatever_its_age() {
    let table = |base: usize| -> String {
        (0..200)
            .map(|k| format!("row {} holds {}", word(base + k), word(base + k + 500)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (spec, log, other) = (table(0), table(2000), table(4000));
    let asked = word(10);
    let results = |view: &[Value]| -> Vec<String> {
        view.iter()
            .flat_map(|m| blocks(m.get("content")).into_owned())
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .map(|b| text_of(b.get("content")).into_owned())
            .collect()
    };
    // the spec read first, the log read twice, a third table nobody asked about, then short steps
    let mut msgs = vec![json!({"role": "user", "content": format!("What does row {asked} of the spec hold?")})];
    msgs.extend(step(1, "cat spec.txt", &spec));
    msgs.extend(step(2, "cat run.log", &log));
    msgs.extend(step(3, "cat run.log", &log));
    msgs.extend(step(4, "cat other.txt", &other));
    for k in 5..12 {
        msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
    }
    let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
    whole.sync(&msgs);
    let total = whole.tokens();
    // room for all but about one table and a half
    let cap = total - 1;
    let floor = (total - 3 * est_str(&log) / 2) as f64 / cap as f64;
    let mut e =
        Engine::new(params(&format!("threshold=0;nudge=0;eoq=0;target={cap};floor={floor};cut=cover")).unwrap());
    e.sync(&msgs);
    let kept = results(&e.view());
    let all = kept.join("\n");
    let row = format!("row {asked} holds");
    assert!(all.contains(&row), "the oldest output holds the row the request asks about: {:?}", e.stats);
    // a second copy tells nothing: no line of the log is sent twice
    assert!(log.split('\n').all(|l| all.matches(l).count() <= 1), "{kept:?}");
    assert!(
        !e.stats.contains_key("left_messages") && e.stats.get("cover_chunks").is_some_and(|n| *n > 0),
        "{:?}",
        e.stats
    );
}

#[test]
fn the_size_cap_leaves_alone_what_the_agent_has_not_written_about_yet() {
    // reads one after another with no word between them: the agent is still reading, and what it read
    // stays; once it writes about it, the cap counts it again
    let mut msgs = vec![json!({"role": "user", "content": "Which slot holds the longest name in the listing?"})];
    for n in 0..8 {
        msgs.extend(step(
            n,
            &format!("sed -n {},{}p listing.txt", 40 * n + 1, 40 * n + 40),
            &listing(40 * n + 1, 40 * n + 40),
        ));
    }
    let all = |view: &str| (0..8).all(|n| view.contains(&word(40 * n + 120)[..]));
    for (hold, kept) in [("hold=0", false), ("hold=1", true)] {
        let mut e = Engine::new(params(&format!("nudge=0;protect=0;eoq=500;{hold}")).unwrap());
        for n in (1..=msgs.len()).step_by(2) {
            e.sync(&msgs[..n]);
            e.view();
        }
        assert_eq!(all(&sent(&e.view())), kept, "{hold}");
    }
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Slot 7 so far; reading on."}]}));
    msgs.push(json!({"role": "user", "content": "go on"}));
    let mut e = Engine::new(params("nudge=0;protect=0;eoq=500;hold=1").unwrap());
    e.sync(&msgs);
    assert!(!all(&sent(&e.view())));
}

#[test]
fn a_session_where_the_agent_never_writes_still_has_a_bounded_view() {
    // what the cap leaves out of its count is only the latest outputs that fit `still`: past that the older
    // ones count, or an agent that only reads would grow the view past any cap
    let mut msgs = vec![json!({"role": "user", "content": "Which slot holds the longest name in the listing?"})];
    for n in 0..40 {
        msgs.extend(step(
            n,
            &format!("sed -n {},{}p listing.txt", 40 * n + 1, 40 * n + 40),
            &listing(40 * n + 1, 40 * n + 40),
        ));
    }
    let most = |spec: &str| {
        let mut e = Engine::new(params(&format!("nudge=0;protect=0;eoq=500;hold=1;target=20000;{spec}")).unwrap());
        let mut most = 0;
        for n in (1..=msgs.len()).step_by(2) {
            e.sync(&msgs[..n]);
            e.view();
            most = most.max(e.tokens());
        }
        (most, e.raw_tokens)
    };
    let (unbounded, all) = most("still=0");
    let (bounded, _) = most("still=2000");
    assert!(unbounded * 10 > all * 9, "with no budget for it, all that is held stays: {unbounded} of {all}");
    assert!(bounded * 3 < all, "past the budget, the cap takes the older outputs: {bounded} of {all}");
}

#[test]
fn a_history_that_leaves_at_once_keeps_its_newest_lines_in_the_pool_in_order() {
    // 24 requests of 500 lines read each leave together: more lines than the pool holds
    let mut msgs = Vec::new();
    for k in 0..24 {
        msgs.push(json!({"role": "user", "content": format!("Read part {k} of listing.txt.")}));
        msgs.extend(step(
            k,
            &format!("sed -n '{},{}p' listing.txt", 500 * k, 500 * k + 499),
            &listing(500 * k, 500 * k + 499),
        ));
        msgs.push(json!({"role": "assistant", "content": listing(500 * k, 500 * k + 499)}));
    }
    msgs.push(json!({"role": "user", "content": "Say which slot holds the last word."}));
    let mut e = Engine::new(params("nudge=0;ledger=6000;protect=2").unwrap());
    e.sync(&msgs);
    let sizes = vec![1000; msgs.len()];
    assert!(e.leave(&sizes, 1.0));
    assert!(e.start > 4 * 24 - 8, "start {}", e.start);
    assert_eq!(e.pool.len(), POOL);
    assert!(e.pool.windows(2).all(|w| w[0].0 <= w[1].0), "the pool keeps the order of the messages");
    // the newest output that left is in the pool whole, the oldest is not
    let last = (0..e.start).rev().find(|&mi| output_of(&msgs, mi).is_some()).unwrap();
    let newest = output_of(&msgs, last).unwrap();
    let first_line = crate::textops::squash_ws(newest.lines().next().unwrap());
    assert!(e.pool.iter().any(|(mi, ln, _, _)| *mi == last && *ln == first_line), "{first_line}");
    assert!(e.pool.iter().all(|(mi, _, _, _)| *mi > 3), "the first read is out of the pool");
}

/// The text of the tool result of message `mi` of `msgs`, if it holds one.
fn output_of(msgs: &[Value], mi: usize) -> Option<String> {
    let b = blocks(msgs[mi].get("content")).into_owned();
    let r = b.iter().find(|x| get_str(x, "type") == "tool_result")?;
    Some(text_of(r.get("content")).into_owned())
}
