//! Steps kept because a request names their file (engine::named).

use super::*;

#[test]
fn named_a_read_stays_while_the_latest_request_names_its_file() {
    let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1;named=16000";
    // the next request is about the report: what was read of it stays
    let mut e = Engine::new(params(spec).unwrap());
    e.sync(&sessions("Done.", Some("Now sort the report by date, please.")));
    assert!(sent(&e.view()).contains(&word(0)[..]));
    // the next request is about something else: the read leaves, as without `named`
    let mut e = Engine::new(params(spec).unwrap());
    e.sync(&sessions("Done.", Some("Rename notes.md to todo.md, please.")));
    assert!(!sent(&e.view()).contains(&word(0)[..]));
}

/// A request that names a file again after the talk moved away: what comes back of it.
fn named_again(reads: &[(usize, &str, String)], edit_after: Option<usize>) -> String {
    let mut msgs =
        vec![json!({"role": "user", "content": "Which client has the largest invoice total in report.txt?"})];
    for (k, command, out) in reads {
        msgs.extend(step(*k, command, out));
        if edit_after == Some(*k) {
            msgs.extend(step(900, "sed -i 's/client/customer/' report.txt", ""));
        }
    }
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Done."}]}));
    msgs.push(json!({"role": "user", "content": "How many lines has notes.md?"}));
    msgs.extend(step(901, "wc -l notes.md", "12 notes.md"));
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "12"}]}));
    msgs.push(json!({"role": "user", "content": "Now sort report.txt by date, please."}));
    let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1;named=16000").unwrap());
    e.sync(&msgs);
    sent(&e.view())
}

#[test]
fn named_a_read_an_edit_made_out_of_date_does_not_come_back() {
    let old = (0..30).map(|k| format!("row {} client {}", word(k), word(k + 700))).collect::<Vec<_>>().join("\n");
    let new = (0..30).map(|k| format!("row {} customer {}", word(k), word(k + 900))).collect::<Vec<_>>().join("\n");
    let view = named_again(&[(1, "cat report.txt", old), (2, "cat report.txt", new)], Some(1));
    assert!(view.contains(&word(900)[..]), "the read after the edit comes back");
    assert!(!view.contains(&word(700)[..]), "the read before the edit stays out: {view}");
}

#[test]
fn named_what_comes_back_of_a_file_read_in_many_parts_is_bounded() {
    // 40 parts of 400 lines: far more than the request can use at once
    let reads: Vec<(usize, String, String)> = (0..40)
        .map(|k| {
            (k + 1, format!("sed -n '{},{}p' report.txt", 400 * k, 400 * k + 399), listing(400 * k, 400 * k + 399))
        })
        .collect();
    let reads: Vec<(usize, &str, String)> = reads.iter().map(|(k, c, o)| (*k, c.as_str(), o.clone())).collect();
    let view = named_again(&reads, None);
    let whole: usize = reads.iter().map(|r| r.2.len()).sum();
    assert!(view.len() < whole / 2, "brought back {} of {} bytes", view.len(), whole);
    // the newest part is among what came back
    assert!(view.contains(&listing(400 * 39, 400 * 39 + 2)[..20]), "the newest part comes back");
}

#[test]
fn mission_a_read_of_a_file_the_first_message_lists_stays_until_a_request_takes_it() {
    let ledger = "quokka 41\nwombat 17\nnumbat 23";
    let session = |last: &str| {
        let mut msgs = vec![json!({"role": "user", "content": "The material is report.txt and ledger.csv. \
                Which client has the largest invoice total in report.txt?"})];
        msgs.extend(step(1, "cat report.txt", &report()));
        msgs.extend(step(2, "cat ledger.csv", ledger));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Done."}]}));
        msgs.push(json!({"role": "user", "content": "Now count the lines of report.txt."}));
        msgs.extend(step(3, "wc -l report.txt", "12 report.txt"));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "12"}]}));
        msgs.push(json!({"role": "user", "content": last}));
        msgs
    };
    let base = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1;named=16000";
    let kept = |spec: &str, last: &str| {
        let mut e = Engine::new(params(spec).unwrap());
        e.sync(&session(last));
        sent(&e.view()).contains("quokka 41")
    };
    // the list is being worked through (the report had its request), and no request has been about the
    // ledger yet: what was read of it stays, it is still to come
    assert!(kept(&format!("{base};mission=1"), "Rename notes.md to todo.md, please."));
    assert!(!kept(base, "Rename notes.md to todo.md, please."));
    // the ledger had its request: after it, the read leaves as any other
    let mut msgs = session("Sum the second column of ledger.csv.");
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "81"}]}));
    msgs.push(json!({"role": "user", "content": "Rename notes.md to todo.md, please."}));
    let mut e = Engine::new(params(&format!("{base};mission=1")).unwrap());
    e.sync(&msgs);
    assert!(!sent(&e.view()).contains("quokka 41"));
}
