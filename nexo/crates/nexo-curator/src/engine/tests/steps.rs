//! Steps of answered requests and how a request closes (engine::steps).

use super::*;

#[test]
fn close_line_survives_start_inside_request() {
    // a first request answered, then one whose request message leaves the view while its calls stay
    let mut msgs =
        vec![json!({"role": "user", "content": "Which client has the largest invoice total in report.txt?"})];
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Client X."}]}));
    msgs.push(json!({"role": "user", "content": "How long has the host been up?"}));
    msgs.extend(step(1, "date", "Fri Oct 31 10:00:00 UTC 2026"));
    msgs.extend(step(2, "uptime", "10:00 up 3 days"));
    msgs.extend(step(3, "who", "root tty1"));
    msgs.extend(step(4, "wc -l report.txt", "530 report.txt"));
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Up 3 days."}]}));
    msgs.push(json!({"role": "user", "content": "Thanks."}));
    let mut e = Engine::new(params("nudge=0;eoq=0;close=1;margin=1000").unwrap());
    e.sync(&msgs);
    e.view();
    assert_eq!((e.steps.len(), e.steps.iter().filter(|s| s.out).count()), (4, 0));
    // the view starts at the second call: the request message is no longer sent
    (e.start, e.vis) = (5, None);
    (e.steps[1].out, e.steps[2].out) = (true, true);
    e.shut_lines();
    let view = sent(&e.view());
    assert!(view.contains("2 tool calls made for this request"), "{:?} {view}", e.shut);
    assert!(view.contains("bash uptime | bash who."), "{view}");
    assert!(e.shut.keys().all(|&mi| mi >= e.start), "{:?}", e.shut);
    // the run reaches the last call: its line is still sent, after the reply, and says no reply follows
    e.steps[3].out = true;
    e.shut_lines();
    let view = sent(&e.view());
    assert!(view.contains("3 tool calls made for this request") && !view.contains("follows"), "{view}");
    assert!(e.shut.keys().all(|&mi| mi >= e.start), "{:?}", e.shut);
}

#[test]
fn an_answered_request_lets_its_steps_go_and_a_request_about_them_brings_them_back() {
    let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1";
    let mut e = Engine::new(params(spec).unwrap());
    e.sync(&sessions(&format!("The largest is client {}.", word(529)), None));
    let view = sent(&e.view());
    let before = e.view();
    assert!(!view.contains(&word(0)[..]), "{view}");
    assert!(view.contains("are left out of this context: bash cat report.txt"), "{view}");
    // the person goes back to the invoices: the read comes back, at the end of the view
    let q = "And the smallest invoice total in the report?";
    e.sync(&sessions(&format!("The largest is client {}.", word(529)), Some(q)));
    let view = e.view();
    let last = sent(&view[view.len() - 1..]);
    assert!(last.contains("Brought back") && last.contains(&word(0)), "{last}");
    assert_eq!(e.stats.get("recalled_steps"), Some(&1));
    // the line that says the read left stays as it was, so the cache keeps all before the new messages
    assert_eq!(sent(&view[..1]), sent(&before[..1]));
}

#[test]
fn a_read_brought_back_in_a_message_that_leaves_is_out_again_and_a_request_that_names_it_brings_it_back() {
    let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1;protect=1";
    let reply = format!("The largest is client {}.", word(529));
    let mut e = Engine::new(params(spec).unwrap());
    e.sync(&sessions(&reply, None));
    e.view();
    assert!(e.steps[0].out, "the read left");
    let mut msgs = sessions(&reply, Some("And the smallest invoice total in the report?"));
    e.sync(&msgs);
    e.view();
    assert!(!e.steps[0].out && e.steps[0].at.is_some(), "the read came back as text");
    // more work follows, then a cut takes the message the read came back at (and every one before it)
    for k in 3..=6 {
        msgs.extend(step(k, &format!("wc -l part{k}.txt"), "12 lines"));
    }
    e.sync(&msgs);
    e.view();
    let at = e.steps[0].at.unwrap();
    let sizes = e.sizes_now();
    assert!(e.leave(&sizes, 0.0) && e.start > at, "the cut passed the message: {} {at}", e.start);
    // none of it is in the view: the step is out, not held "in view" by a message that is gone
    assert!(e.steps[0].out && e.steps[0].at.is_none() && e.steps[0].idle.is_none(), "{:?}", e.steps[0].at);
    // a request that names the file brings it back anew
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Done."}]}));
    msgs.push(json!({"role": "user", "content": "And the client of invoice total 1037 in report.txt?"}));
    e.sync(&msgs);
    let view = e.view();
    let last = sent(&view[view.len() - 1..]);
    assert!(last.contains("Brought back") && last.contains(&word(0)), "{last}");
    assert_eq!(e.stats.get("recalled_steps"), Some(&2));
}

#[test]
fn a_request_closed_after_a_read_came_back_to_it_keeps_what_came_back_first() {
    let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1";
    let reply = format!("The largest is client {}.", word(529));
    let report = report();
    let low = report.lines().next().unwrap().rsplit(' ').next().unwrap();
    let mut msgs = sessions(&reply, Some("And the smallest invoice total in the report?"));
    let mut e = Engine::new(params(spec).unwrap());
    e.sync(&sessions(&reply, None));
    e.view();
    e.sync(&msgs);
    let before = e.view();
    let mi = before.len() - 1;
    assert!(sent(&before[mi..]).contains("Brought back"));
    // that request runs a call of its own and is answered, and the next one is on the report again: the
    // read stays, the call leaves, and the line that says so comes after what came back
    msgs.extend(step(3, "date", "Fri Oct 31 10:00:00 UTC 2026"));
    let answer = format!("The smallest is client {low}.");
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": answer}]}));
    msgs.push(json!({"role": "user", "content": "And which client has the invoice total of 1037 in report.txt?"}));
    e.sync(&msgs);
    let view = e.view();
    let (was, now) = (blocks(before[mi].get("content")).into_owned(), blocks(view[mi].get("content")).into_owned());
    assert!(now.len() > was.len() && now[..was.len()] == was[..], "{was:?}\n{now:?}");
}

#[test]
fn a_request_about_something_else_leaves_the_read_out() {
    let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1";
    let mut e = Engine::new(params(spec).unwrap());
    e.sync(&sessions("Done.", Some("Rename notes.md to todo.md, please.")));
    let view = sent(&e.view());
    assert!(!view.contains(&word(0)[..]), "{view}");
    assert!(!view.contains("Brought back"), "{view}");
}

#[test]
fn by_use_a_bare_reply_lets_no_step_go() {
    // "[4]" shows nothing of where it came from, so nothing of what the request read may go
    let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=1;margin=0;ahead=1").unwrap());
    e.sync(&sessions("[4]", None));
    assert!(sent(&e.view()).contains(&word(0)[..]));
    // a request done from an output of its own shows the read was not needed
    let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=1;margin=0;ahead=1").unwrap());
    e.sync(&sessions("Done.", Some("Rename notes.md to todo.md, please.")));
    assert!(!sent(&e.view()).contains(&word(0)[..]));
}

#[test]
fn with_no_calls_to_come_counted_a_step_stays() {
    // a cut is credited only with the rent it saves from now on: with none counted, the read stays
    let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=2;ahead=0").unwrap());
    e.sync(&sessions("Done.", None));
    assert!(sent(&e.view()).contains(&word(0)[..]));
    assert!(e.steps.iter().all(|s| !s.out && s.idle.is_some()));
}

#[test]
fn a_name_every_request_says_still_points_at_the_one_step_that_holds_it() {
    let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=2").unwrap());
    let mut msgs = Vec::new();
    let reads = ["cat ledger.csv", "cat notes.txt", "cat todo.txt"];
    for (k, q) in
        ["In ledger.csv: the invoices?", "In ledger.csv: the clients?", "In ledger.csv: the totals?"].iter().enumerate()
    {
        msgs.push(json!({"role": "user", "content": q}));
        msgs.extend(step(k + 1, reads[k], &format!("line {k}\n")));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Done."}]}));
    }
    msgs.pop();
    e.sync(&msgs);
    e.view();
    let asked: Vec<String> = e.asks(2).iter().map(|&w| e.int.get(w).to_string()).collect();
    assert!(asked.iter().any(|w| w.starts_with("ledger")), "{asked:?}");
}

#[test]
fn what_every_request_says_points_at_no_step() {
    let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=2").unwrap());
    let mut msgs = Vec::new();
    for (k, q) in
        ["Project Zephyr: list the invoices.", "Project Zephyr: list the clients.", "Project Zephyr: the totals?"]
            .iter()
            .enumerate()
    {
        msgs.push(json!({"role": "user", "content": q}));
        msgs.extend(step(k + 1, "cat report.txt", &report()));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Done."}]}));
    }
    msgs.pop();
    e.sync(&msgs);
    e.view();
    let asked: Vec<String> = e.asks(2).iter().map(|&w| e.int.get(w).to_string()).collect();
    assert!(asked.contains(&"totals".to_string()), "{asked:?}");
    assert!(!asked.contains(&"zephyr".to_string()) && !asked.contains(&"project".to_string()), "{asked:?}");
}

#[test]
fn topics_keep_long_words_and_labels() {
    let mut int = Interner::default();
    let got: Vec<String> = {
        let mut v: Vec<String> = topics("Function B returns boost::any; see assembly-B.txt, 2024 a I", &mut int)
            .into_iter()
            .map(|w| int.get(w).to_string())
            .collect();
        v.sort();
        v
    };
    assert_eq!(got, ["B", "I", "any", "assembly", "boost", "function", "returns", "see", "txt"]);
}
