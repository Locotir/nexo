//! What comes back to the view (engine::recall).

use super::*;

/// The agent's `recall` of message `mi` (lines `from`-`to` of it, when given) and the answer the host
/// gives it, `text`.
pub(super) fn recall_step(k: usize, mi: usize, part: Option<(usize, usize)>, text: &str) -> [Value; 2] {
    let id = format!("recall_{k}");
    let mut input = json!({"message": format!("m{:05}", mi + 1)});
    let lines = match part {
        Some((from, to)) => {
            input["from"] = json!(from);
            input["to"] = json!(to);
            format!(", lines {from}-{to} of {}", text.lines().count())
        }
        None => String::new(),
    };
    let body: String = match part {
        Some((from, to)) => text.lines().skip(from - 1).take(to + 1 - from).collect::<Vec<_>>().join("\n"),
        None => text.to_string(),
    };
    [
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "recall", "input": input}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id,
                "content": format!("m{:05} (the output of bash){lines}, as it was:\n{body}", mi + 1)}]}),
    ]
}

/// Feeds `msgs` to `e` as a host does: a view before each call of the model (after each message of the
/// person, a request or the results of a step).
pub(super) fn feed(e: &mut Engine, msgs: &[Value]) {
    for t in 1..=msgs.len() {
        if msgs[t - 1]["role"] == "user" && t > e.tr.msgs.len() {
            e.sync(&msgs[..t]);
            e.view();
        }
    }
}

#[test]
fn the_agent_recalling_an_output_the_view_left_out_counts_its_step_as_brought_back() {
    let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1";
    let reply = format!("The largest is client {}.", word(529));
    let mut e = Engine::new(params(spec).unwrap());
    let mut msgs = sessions(&reply, None);
    feed(&mut e, &msgs);
    assert!(sent(&e.view()).contains("are left out of this context: bash cat report.txt"));
    assert!(e.steps[0].out);
    // the agent asks for the read back by its name (message 3): the step was needed after it left
    msgs.extend(recall_step(1, 2, None, &report()));
    feed(&mut e, &msgs);
    assert_eq!(e.stats.get("agent_recalled_steps"), Some(&1));
    // a recall of a message the view sends (the reply, the open request) is the agent's own doing
    msgs.extend(recall_step(2, 3, None, &reply));
    feed(&mut e, &msgs);
    msgs.extend(recall_step(3, 4, None, "How many lines has notes.md?"));
    feed(&mut e, &msgs);
    assert_eq!(e.stats.get("agent_recalled_steps"), Some(&1));
    // the next request is on the report: the curator brings the read back; a recall of it then is the
    // agent's own doing too, since its text is in the view
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "3 lines came back."}]}));
    msgs.push(json!({"role": "user", "content": "And the smallest invoice total in the report?"}));
    feed(&mut e, &msgs);
    assert_eq!((e.steps[0].out, e.steps[0].at.is_some()), (false, true));
    msgs.extend(recall_step(4, 2, None, &report()));
    feed(&mut e, &msgs);
    assert_eq!(e.stats.get("agent_recalled_steps"), Some(&1));
}

#[test]
fn a_read_made_again_counts_as_brought_back_only_when_its_earlier_output_was_left_out() {
    let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1";
    let reply = format!("The largest is client {}.", word(529));
    // the read of the first request left: the agent runs it again on the second
    let mut e = Engine::new(params(spec).unwrap());
    let mut msgs = sessions(&reply, None);
    feed(&mut e, &msgs);
    msgs.extend(step(2, "cat  report.txt", &report()));
    feed(&mut e, &msgs);
    assert_eq!(e.stats.get("agent_recalled_steps"), Some(&1));
    // the read stays in the view (no closing): running it again is the agent's own doing
    let mut e = Engine::new(params("nudge=0;eoq=0;link=0").unwrap());
    let mut msgs = sessions(&reply, None);
    feed(&mut e, &msgs);
    msgs.extend(step(2, "cat report.txt", &report()));
    feed(&mut e, &msgs);
    assert_eq!(e.stats.get("agent_recalled_steps"), None);
    // the file was edited since the read left: the agent reads what it holds now, not what left
    let mut e = Engine::new(params(spec).unwrap());
    let mut msgs = sessions(&reply, None);
    feed(&mut e, &msgs);
    msgs.extend(step(2, "sed -i 's/invoice/bill/' report.txt", ""));
    feed(&mut e, &msgs);
    msgs.extend(step(3, "cat report.txt", &report().replace("invoice", "bill")));
    feed(&mut e, &msgs);
    assert_eq!(e.stats.get("agent_recalled_steps"), None);
    // a command that runs a process, not one that prints files, is no read of what left
    let mut e = Engine::new(params(spec).unwrap());
    let mut msgs = vec![json!({"role": "user", "content": "Do the tests pass?"})];
    msgs.extend(step(1, "cargo test", &report()));
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "They pass."}]}));
    msgs.push(json!({"role": "user", "content": "How many lines has notes.md?"}));
    feed(&mut e, &msgs);
    assert!(e.steps[0].out);
    msgs.extend(step(2, "cargo test", &report()));
    feed(&mut e, &msgs);
    assert_eq!(e.stats.get("agent_recalled_steps"), None);
}

/// A read of lines `a..=b` of `listing.txt` by a read tool, with its output.
fn read_step(k: usize, (a, b): (usize, usize)) -> [Value; 2] {
    let id = format!("call_{k}");
    [
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "read",
                "input": {"path": "listing.txt", "offset": a, "limit": b + 1 - a}}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": listing(a, b)}]}),
    ]
}

#[test]
fn a_read_over_lines_an_output_that_left_gave_counts_and_the_step_made_of_it_later_starts_with_the_count() {
    // the first two reads leave unused (`live`) while the request is open, the third stays
    let spec = "nudge=0;eoq=0;link=0;threshold=0;close=2;margin=0;ahead=1;live=1";
    let mut e = Engine::new(params(spec).unwrap());
    let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
    for (k, r) in [(1, 40), (41, 80), (81, 120)].into_iter().enumerate() {
        msgs.extend(read_step(k + 1, r));
    }
    feed(&mut e, &msgs);
    let view = e.view();
    assert!(output(&view, 2).contains("left out here") && output(&view, 4).contains("left out here"));
    assert_eq!(output(&view, 6), listing(81, 120));
    // lines the view still sends, and lines nothing read yet, are no asking back
    msgs.extend(read_step(4, (100, 110)));
    feed(&mut e, &msgs);
    msgs.extend(read_step(5, (121, 160)));
    feed(&mut e, &msgs);
    assert_eq!(e.stats.get("agent_recalled_steps"), None);
    // lines of the two outputs that left: the latest of them is asked back
    msgs.extend(read_step(6, (20, 50)));
    feed(&mut e, &msgs);
    assert_eq!(e.stats.get("agent_recalled_steps"), Some(&1));
}

#[test]
fn aim_a_request_brings_back_the_step_that_went_for_what_it_names_not_one_that_only_listed_it() {
    // the first read lists the folder and prints the sales file; the second goes for the stock file
    let mut msgs = vec![json!({"role": "user", "content": "Summarize the files in data/."})];
    msgs.extend(step(1, "ls data; cat data/sales.csv", &format!("sales.csv\nstock.csv\n{}", report())));
    msgs.extend(step(2, "head -3 data/stock.csv", "item,count\nbolt_7,40\nnut_9,12"));
    msgs.extend(step(4, "wc -l data/sales.csv", "31 data/sales.csv"));
    msgs.extend(step(5, "du -sh data", "8K data"));
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Sales and stock."}]}));
    msgs.push(json!({"role": "user", "content": "How many lines has notes.md?"}));
    msgs.extend(step(3, "wc -l notes.md", "12 notes.md"));
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "12"}]}));
    msgs.push(json!({"role": "user", "content": "What does stock.csv hold?"}));
    let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1";
    for (aim, sales) in [("aim=0", true), ("aim=1", false)] {
        let mut e = Engine::new(params(&format!("{spec};{aim}")).unwrap());
        // the reads leave once the next request is open, and the last one asks about them again
        e.sync(&msgs[..msgs.len() - 4]);
        assert!(!sent(&e.view()).contains("bolt_7"));
        e.sync(&msgs);
        let view = e.view();
        let last = sent(&view[view.len() - 1..]);
        assert!(last.contains("bolt_7"), "{aim}: {last}");
        assert_eq!(last.contains(&word(0)), sales, "{aim}: {last}");
    }
}

#[test]
fn a_request_on_a_topic_brings_back_the_step_that_says_it_most_not_the_largest_that_names_it() {
    // the catalog names the pricing once in its header; the notes are about it
    let catalog = format!("shop catalog, pricing in euros\n{}", report());
    let notes = "pricing changed in march\nnew pricing for spring\nold pricing ends in may";
    let mut msgs = vec![json!({"role": "user", "content": "Summarize the shop files."})];
    msgs.extend(step(1, "cat catalog.txt", &catalog));
    msgs.extend(step(2, "cat notes.txt", notes));
    msgs.extend(step(4, "du -sh .", "8K ."));
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "A catalog and notes."}]}));
    msgs.push(json!({"role": "user", "content": "How many lines has todo.md?"}));
    msgs.extend(step(3, "wc -l todo.md", "12 todo.md"));
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "12"}]}));
    msgs.push(json!({"role": "user", "content": "What about pricing?"}));
    let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1;aim=1").unwrap());
    e.sync(&msgs[..msgs.len() - 4]);
    assert!(!sent(&e.view()).contains("spring"));
    e.sync(&msgs);
    let view = e.view();
    let last = sent(&view[view.len() - 1..]);
    assert!(last.contains("spring"), "{last}");
    assert!(!last.contains(&word(0)), "{last}");
}
