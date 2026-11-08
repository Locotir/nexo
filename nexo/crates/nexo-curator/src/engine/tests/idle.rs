//! Outputs that go idle and leave (engine::idle).

use super::*;

#[test]
fn what_the_agent_has_not_written_about_stays_through_a_new_request() {
    let view_of = |spec: &str| {
        let mut e = reads(spec, &[(1, 40), (41, 80), (81, 120)]);
        let mut msgs = e.tr.msgs.clone();
        msgs.push(json!({"role": "user", "content": "And what does slot 200 hold?"}));
        for k in 4..9 {
            msgs.extend(step(
                k,
                &format!("sed -n '{},{}p' other.txt", k * 50, k * 50 + 39),
                &listing(k * 50, k * 50 + 39),
            ));
        }
        e.sync(&msgs);
        e.view()
    };
    let spec = "nudge=0;eoq=0;protect=0;threshold=1;margin=0;every=1;check=0";
    // no text and no note since the first reads: the agent is still working from them
    assert_eq!(output(&view_of(&format!("{spec};still=100000")), 2), listing(1, 40));
    // a guess that they are not needed takes them otherwise
    assert_ne!(output(&view_of(&format!("{spec};still=0")), 2), listing(1, 40));
}

#[test]
fn live_drop_keeps_part_carrying_a_copy() {
    let log =
        (0..200).map(|k| format!("event {} at {}", word(k + 2000), word(k + 3000))).collect::<Vec<_>>().join("\n");
    let mut msgs = vec![json!({"role": "user", "content": "What does the log hold?"})];
    msgs.extend(step(1, "cat run.log", &log));
    msgs.extend(step(2, "cat run.log", &log));
    for k in 3..8 {
        msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
    }
    let seen = |view: &[Value]| -> (usize, bool) {
        let text: String = view.iter().map(|m| crate::msg::flat(m.get("content")) + "\n").collect();
        let lines: FxHashSet<&str> = text.lines().filter(|l| l.starts_with("event ")).collect();
        (lines.len(), text.contains("a later output below sends"))
    };
    let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
    whole.sync(&msgs);
    let total = whole.tokens();
    let cap = total - 1;
    let floor = (total - est_str(&log) / 2) as f64 / cap as f64;
    let given = format!(
        "threshold=1;still=0;protect=2;nudge=0;sup=1;cut=copies;cap={cap};floor={floor};live=1;close=2;margin=0"
    );
    let mut e = Engine::new(params(&given).unwrap());
    e.sync(&msgs);
    let (lines, stub) = seen(&e.view());
    assert!(stub && lines == 200 && !e.carrying.is_empty(), "{:?}", e.stats);
    // more steps the agent never writes about: the idle outputs of the open request may leave, never the
    // one that sends the lines of the copy left out
    for k in 9..14 {
        msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
        e.sync(&msgs);
        let (lines, stub) = seen(&e.view());
        assert!(!stub || lines == 200, "request {k}: {lines} lines, {:?}", e.stats);
    }
}

#[test]
fn a_call_leaves_only_when_it_pays_for_the_view_read_again_from_the_request_it_was_made_for() {
    // the cut is priced from the request the call was made for: all read after it, the large read the
    // next request still works from too, is read again without the cache, not only what follows the call
    let mut msgs =
        vec![json!({"role": "user", "content": "Which client has the largest invoice total in report.txt?"})];
    msgs.extend(step(1, "cat report.txt", &report()));
    msgs.extend(step(2, "date", "Fri Oct 31 10:00:00 UTC 2026"));
    let reply = format!("The largest is client {}.", word(529));
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": reply}]}));
    msgs.push(json!({"role": "user", "content": "And the smallest invoice total in report.txt?"}));
    let spec = |margin: &str| format!("nudge=0;eoq=0;link=0;close=2;named=16000;ahead=1;margin={margin}");
    let mut e = Engine::new(params(&spec("0.5")).unwrap());
    e.sync(&msgs);
    let view = sent(&e.view());
    assert!(view.contains(&word(0)[..]) && !view.contains("left out of this context"), "{view}");
    // when the cut costs nothing, the call leaves and the read the request names stays
    let mut e = Engine::new(params(&spec("0")).unwrap());
    e.sync(&msgs);
    let view = sent(&e.view());
    assert!(view.contains(&word(0)[..]) && view.contains("left out of this context: bash date"), "{view}");
}

#[test]
fn the_line_for_calls_that_left_goes_just_before_them_and_all_ahead_stays_as_the_cache_holds_it() {
    // calls that leave from the middle of a long request are named on the output just before them, not
    // on the request: the view only changes from there on. Only the line of the calls up to the reply
    // says the reply follows.
    let mut msgs =
        vec![json!({"role": "user", "content": "Which client has the largest invoice total in report.txt?"})];
    msgs.extend(step(1, "cat report.txt", &report()));
    msgs.extend(step(2, "date", "Fri Oct 31 10:00:00 UTC 2026"));
    msgs.extend(step(3, "uptime", "10:00 up 3 days"));
    msgs.extend(step(4, "wc -l report.txt", "530 report.txt"));
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "The largest is client X."}]}));
    msgs.push(json!({"role": "user", "content": "Thanks."}));
    let mut e = Engine::new(params("nudge=0;eoq=0;close=1;margin=1000").unwrap());
    e.sync(&msgs);
    e.view();
    assert_eq!((e.steps.len(), e.steps.iter().filter(|s| s.out).count()), (4, 0));
    let line = |e: &Engine, mi: usize| e.shut.get(&mi).map(|l| l.join(" ")).unwrap_or_default();
    (e.steps[1].out, e.steps[2].out) = (true, true);
    assert_eq!(e.shut_lines(), Some(2));
    assert!(!e.shut.contains_key(&0) && line(&e, 2).contains("2 tool calls"), "{:?}", e.shut);
    assert!(line(&e, 2).contains("bash date | bash uptime.") && !line(&e, 2).contains("follows"));
    // the run grows to the last call: its line, the same place, now says the reply follows
    e.steps[3].out = true;
    assert_eq!(e.shut_lines(), Some(2));
    assert!(line(&e, 2).contains("3 tool calls") && line(&e, 2).contains("The reply that closed the request follows."));
    // the first call leaves too: the run starts after the request, and the request takes the line
    e.steps[0].out = true;
    assert_eq!(e.shut_lines(), Some(0));
    assert!(e.shut.keys().eq([&0]) && line(&e, 0).contains("4 tool calls"), "{:?}", e.shut);
    let view = sent(&e.view());
    assert!(!view.contains(&word(0)[..]) && view.contains("4 tool calls made for this request"), "{view}");
}

#[test]
fn live_an_output_no_later_call_or_text_drew_on_leaves_in_its_place() {
    let spec = "nudge=0;eoq=0;link=0;threshold=0;close=2;margin=0;ahead=1;live=1";
    // two more reads of other lines follow the first, and neither names anything it printed
    let e = &mut reads(spec, &[(1, 40), (41, 80), (81, 120)]);
    let view = e.view();
    assert!(output(&view, 2).contains("left out here"), "{view:?}");
    assert_eq!(output(&view, 6), listing(81, 120));
    // the agent's next call names an address only the first output printed: it is in use and stays
    let mut e = Engine::new(params(spec).unwrap());
    let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
    msgs.extend(step(1, "sed -n '1,40p' listing.txt", &listing(1, 40)));
    msgs.extend(step(2, "grep -n 1052 listing.txt", &listing(7, 7)));
    msgs.extend(step(3, "sed -n '81,120p' listing.txt", &listing(81, 120)));
    e.sync(&msgs);
    assert_eq!(output(&e.view(), 2), listing(1, 40));
}

#[test]
fn live_prices_an_output_at_what_it_sends_now() {
    let mut e = reads("nudge=0;eoq=0;threshold=0", &[(1, 200)]);
    let i = result_in(&e, 2);
    let n = e.fz.chunks[i].len();
    assert!(n > 2, "{n} chunks");
    let whole: i64 = e.fz.chunks[i].iter().map(|c| c.tokens).sum();
    assert_eq!(e.sends_now(i, None), whole);
    // all but the last chunk already left: only that one is still paid for on every call
    e.drop_as(i, 0..n - 1, "evict");
    assert_eq!(e.sends_now(i, None), e.fz.chunks[i][n - 1].tokens);
    // one line in a hundred is sent by a copy that stays: leaving would not save it
    let one: Vec<bool> = (0..100).map(|k| k == 0).collect();
    let (last, tokens) = (e.fz.chunks[i][n - 1].tokens, e.tr.units[i].tokens);
    assert!(last > tokens / 100, "{last} {tokens}");
    assert_eq!(e.sends_now(i, Some(&one)), last - tokens / 100);
}

#[test]
fn live_with_named_an_output_of_the_file_the_request_names_stays() {
    let spec = "nudge=0;eoq=0;link=0;threshold=0;close=2;margin=0;ahead=1;live=1;named=16000";
    // the request names listing.txt: its reads are the material the answer needs, used or not yet
    let e = &mut reads(spec, &[(1, 40), (41, 80), (81, 120)]);
    assert_eq!(output(&e.view(), 2), listing(1, 40));
    // a request that names another file lets the unused read leave, as without `named`
    let mut e = Engine::new(params(spec).unwrap());
    let mut msgs = vec![json!({"role": "user", "content": "Read notes.md and say what slot 50 holds."})];
    for (k, (a, b)) in [(1, 40), (41, 80), (81, 120)].into_iter().enumerate() {
        msgs.extend(step(k + 1, &format!("sed -n '{a},{b}p' listing.txt"), &listing(a, b)));
    }
    e.sync(&msgs);
    assert!(output(&e.view(), 2).contains("left out here"));
}

#[test]
fn lead_an_output_that_leaves_keeps_the_line_naming_what_nothing_followed_yet() {
    // the agent has looked up one handler by name; the read that leaves points at another
    let read = format!("{}\n 1200:  jmp handler_77", listing(1, 40));
    let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
    msgs.extend(step(1, "grep -rn handler_12 src", "src/a.rs:3: handler_12()"));
    msgs.extend(step(2, "sed -n '1,41p' listing.txt", &read));
    msgs.extend(step(3, "sed -n '41,80p' listing.txt", &listing(41, 80)));
    msgs.extend(step(4, "sed -n '81,120p' listing.txt", &listing(81, 120)));
    let spec = "nudge=0;eoq=0;link=0;threshold=0;close=2;margin=0;ahead=1;live=1";
    for (lead, kept) in [("lead=0", false), ("lead=1", true)] {
        let mut e = Engine::new(params(&format!("{spec};{lead}")).unwrap());
        e.sync(&msgs);
        let out = output(&e.view(), 4);
        assert!(out.contains("left out here") && !out.contains("slot 1 "), "{out}");
        assert_eq!(out.ends_with("\n 1200:  jmp handler_77"), kept, "{lead}: {out}");
    }
}

#[test]
fn brief_the_note_for_an_output_left_out_only_says_how_to_bring_it_back() {
    let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
    msgs.extend(step(1, "sed -n '1,40p' listing.txt", &listing(1, 40)));
    msgs.extend(step(2, "grep -n 1052 listing.txt", &listing(7, 7)));
    msgs.extend(step(3, "sed -n '81,120p' listing.txt", &listing(81, 120)));
    let spec = "nudge=0;eoq=0;link=0;threshold=0;close=2;margin=0;ahead=1;live=1;follow=1";
    let mut e = Engine::new(params(&format!("{spec};brief=1")).unwrap());
    e.sync(&msgs);
    assert_eq!(
        output(&e.view(), 2),
        "[curator] bash output left out here, 40 lines you read already; run it again to see it again"
    );
}

#[test]
fn digest_what_the_agent_read_stays_until_it_writes_about_it() {
    // the agent reads two ranges and looks further without a word on what it read so far
    let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
    msgs.extend(step(1, "sed -n '1,40p' listing.txt", &listing(1, 40)));
    msgs.extend(step(2, "sed -n '41,80p' listing.txt", &listing(41, 80)));
    msgs.extend(step(3, "sed -n '81,120p' listing.txt", &listing(81, 120)));
    let spec = "nudge=0;eoq=0;link=0;threshold=0;close=2;margin=0;ahead=1;live=1";
    for (digest, kept) in [("digest=0", false), ("digest=1", true)] {
        let mut e = Engine::new(params(&format!("{spec};{digest}")).unwrap());
        e.sync(&msgs);
        let view = e.view();
        assert_eq!(output(&view, 2) == listing(1, 40), kept, "{digest}: {view:?}");
    }
    // once it has written what it found, the read it did not draw on goes
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Finding: the slots run in order."}]}));
    msgs.extend(step(4, "sed -n '121,160p' listing.txt", &listing(121, 160)));
    let mut e = Engine::new(params(&format!("{spec};digest=1")).unwrap());
    e.sync(&msgs);
    let view = e.view();
    assert_ne!(output(&view, 2), listing(1, 40), "{view:?}");
}

#[test]
fn follow_a_call_that_printed_what_it_went_for_does_not_keep_the_output_that_named_it() {
    // the agent looks up an address the first read printed, and the lookup prints that line again
    let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
    msgs.extend(step(1, "sed -n '1,40p' listing.txt", &listing(1, 40)));
    msgs.extend(step(2, "grep -n 1052 listing.txt", &listing(7, 7)));
    msgs.extend(step(3, "sed -n '81,120p' listing.txt", &listing(81, 120)));
    let spec = "nudge=0;eoq=0;link=0;threshold=0;close=2;margin=0;ahead=1;live=1";
    for (follow, kept) in [("follow=0", true), ("follow=1", false)] {
        let mut e = Engine::new(params(&format!("{spec};{follow}")).unwrap());
        e.sync(&msgs);
        let view = e.view();
        assert_eq!(output(&view, 2) == listing(1, 40), kept, "{follow}: {view:?}");
        assert_eq!(output(&view, 4), listing(7, 7));
    }
}
