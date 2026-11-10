//! The model's window: a view never passes what the host says the model takes, whatever the history
//! holds, and each view says how many messages it put in and took out.

use nexo_curator::Curator;
use serde_json::{json, Value};

fn step(k: usize, lines: usize) -> [Value; 2] {
    let id = format!("call_{k}");
    let out: String =
        (0..lines).map(|l| format!("data/part_{k}/row_{l}.csv:{l}: amount_{k}_{l} = {}\n", l * 7)).collect();
    [
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "bash", "input": {"command": format!("cat data/part_{k}")}}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out}]}),
    ]
}

fn est(v: &[Value]) -> i64 {
    (serde_json::to_string(v).unwrap().len() / 4) as i64
}

#[test]
fn a_long_history_stays_under_the_window() {
    let mut cur = Curator::new("").unwrap();
    cur.window(20_000);
    let mut msgs = vec![json!({"role": "user", "content": "Sum the amounts of every part."})];
    for k in 0..40 {
        msgs.extend(step(k, 60));
        let r = cur.view(1_000, &msgs, 0.0).unwrap();
        assert!(r.tokens <= 19_000, "call {k}: {} tokens", r.tokens);
        assert!(est(&r.view) <= 21_000, "call {k}: view of {} tokens", est(&r.view));
        // the request that started it all is never the one that leaves
        assert!(r.view[0]["content"].to_string().contains("Sum the amounts"));
    }
}

#[test]
fn one_output_past_the_window_is_paged_with_a_mark() {
    let mut cur = Curator::new("").unwrap();
    cur.window(8_000);
    let mut msgs = vec![json!({"role": "user", "content": "Read the export."})];
    msgs.extend(step(0, 2_000));
    let r = cur.view(500, &msgs, 0.0).unwrap();
    assert!(r.tokens <= 7_500, "{} tokens", r.tokens);
    let text = serde_json::to_string(&r.view).unwrap();
    assert!(text.contains("left out here: the whole text does not fit the model's window"));
    // both ends of the output stay
    assert!(text.contains("row_0.csv") && text.contains("row_1999.csv"));
}

#[test]
fn paging_never_cuts_an_image() {
    let mut cur = Curator::new("").unwrap();
    cur.window(8_000);
    let img = "A".repeat(900_000);
    let mut msgs = vec![
        json!({"role": "user", "content": [
            {"type": "text", "text": "Look at this."},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": img}}
        ]}),
        json!({"role": "assistant", "content": [{"type": "text", "text": "ok"}]}),
    ];
    msgs.extend(step(0, 2_000));
    let r = cur.view(500, &msgs, 0.0).unwrap();
    let text = serde_json::to_string(&r.view).unwrap();
    assert!(text.contains(&img), "image data was cut");
    assert!(text.contains("left out here: the whole text does not fit the model's window"));
}

#[test]
fn paging_keeps_crlf_line_ends() {
    let mut cur = Curator::new("").unwrap();
    cur.window(8_000);
    let out: String = (0..2_000).map(|l| format!("row_{l} = {}\r\n", l * 7)).collect();
    let msgs = vec![
        json!({"role": "user", "content": "Read."}),
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": "c0", "name": "bash", "input": {"command": "cat x"}}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "c0", "content": out}]}),
    ];
    let r = cur.view(500, &msgs, 0.0).unwrap();
    let text = serde_json::to_string(&r.view).unwrap();
    assert!(text.contains("left out here"));
    assert!(text.contains("\\r\\n") && !text.replace("\\r\\n", "").contains("\\n"), "a bare LF replaced a CRLF");
}

#[test]
fn an_edited_history_keeps_the_recall_tool_and_the_window() {
    let mut cur = Curator::new("").unwrap();
    cur.window(8_000);
    cur.tools(&["bash".to_string(), "recall".to_string()]);
    let out: String = (1..=300).map(|k| format!("job {k} finished in {} ms with status ok\n", k * 3)).collect();
    let mk = |ask: &str| {
        vec![
            json!({"role": "user", "content": ask}),
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": "c1", "name": "bash", "input": {"command": "./run-jobs"}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "c1", "content": format!("{ask}\n{out}")}]}),
        ]
    };
    cur.view(0, &mk("Which job was slowest?"), 0.0).unwrap();
    // the person edits the history: the history diverges and the engine starts over
    let r = cur.view(0, &mk("Which job was the fastest?"), 0.0).unwrap();
    let text = serde_json::to_string(&r.view).unwrap();
    assert!(text.contains("recall m00003 brings it back"), "{text}");
}

#[test]
fn no_window_changes_nothing() {
    let mut a = Curator::new("").unwrap();
    let mut b = Curator::new("").unwrap();
    b.window(0);
    let mut msgs = vec![json!({"role": "user", "content": "Read the export."})];
    msgs.extend(step(0, 2_000));
    assert_eq!(a.view(500, &msgs, 0.0).unwrap().view, b.view(500, &msgs, 0.0).unwrap().view);
}

#[test]
fn each_view_counts_what_came_in_and_went_out() {
    let mut cur = Curator::new("").unwrap();
    let mut msgs = vec![json!({"role": "user", "content": "Sum the amounts of every part."})];
    let r = cur.view(0, &msgs, 0.0).unwrap();
    assert_eq!((r.added, r.removed), (1, 0));
    msgs.extend(step(0, 5));
    let r = cur.view(0, &msgs, 0.0).unwrap();
    assert_eq!((r.added, r.removed), (2, 0));
    // the same history again: nothing moved
    let r = cur.view(0, &msgs, 0.0).unwrap();
    assert_eq!((r.added, r.removed), (0, 0));
    // a tight window makes old messages leave, and the count says so
    cur.window(3_000);
    for k in 1..30 {
        msgs.extend(step(k, 30));
    }
    let r = cur.view(0, &msgs, 0.0).unwrap();
    assert!(r.removed > 0, "{} removed", r.removed);
}

#[test]
fn only_a_message_the_view_had_left_out_counts_as_back() {
    let mut cur = Curator::new("").unwrap();
    let mut msgs = vec![json!({"role": "user", "content": "Sum the amounts of every part."})];
    let r = cur.view(0, &msgs, 0.0).unwrap();
    assert_eq!((r.added, r.back), (1, 0), "the first message is new, not back");
    msgs.extend(step(0, 5));
    let r = cur.view(0, &msgs, 0.0).unwrap();
    assert_eq!((r.added, r.back), (2, 0), "what the conversation appended is not the curator's move");
    cur.window(3_000);
    for k in 1..30 {
        msgs.extend(step(k, 30));
    }
    let r = cur.view(0, &msgs, 0.0).unwrap();
    assert!(r.removed > 0 && r.back == 0, "{} removed, {} back", r.removed, r.back);
    // the same history again: nothing moved
    let r = cur.view(0, &msgs, 0.0).unwrap();
    assert_eq!((r.added, r.back, r.removed), (0, 0, 0));
}

#[test]
fn what_leaves_says_how_to_bring_it_back_when_the_host_can() {
    let run = |tools: &[&str]| {
        let mut cur = Curator::new("").unwrap();
        cur.tools(&tools.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        cur.window(6_000);
        let mut msgs = vec![json!({"role": "user", "content": "Sum the amounts of every part."})];
        let mut text = String::new();
        for k in 0..30 {
            msgs.extend(step(k, 30));
            text = serde_json::to_string(&cur.view(0, &msgs, 0.0).unwrap().view).unwrap();
        }
        text
    };
    let with = run(&["bash", "recall"]);
    assert!(with.contains("recall m"), "{with}");
    let without = run(&["bash"]);
    assert!(!without.contains("recall m"), "{without}");
}

#[test]
fn an_output_shortened_as_it_arrives_says_how_to_have_it_whole() {
    let run = |tools: &[&str]| {
        let mut cur = Curator::new("").unwrap();
        cur.tools(&tools.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let out: String = (1..=300).map(|k| format!("job {k} finished in {} ms with status ok\n", k * 3)).collect();
        let msgs = vec![
            json!({"role": "user", "content": "Run the jobs and say which one was slowest."}),
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": "c1", "name": "bash", "input": {"command": "./run-jobs"}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "c1", "content": out}]}),
        ];
        serde_json::to_string(&cur.view(0, &msgs, 0.0).unwrap().view).unwrap()
    };
    let with = run(&["bash", "recall"]);
    assert!(with.contains("(300 lines in full); recall m00003 brings it back"), "{with}");
    let without = run(&["bash"]);
    assert!(!without.contains("recall m"), "{without}");
}

#[test]
fn what_recall_brings_back_is_never_shortened_again() {
    let mut cur = Curator::new("").unwrap();
    cur.tools(&["bash".to_string(), "recall".to_string()]);
    let out: String = (1..=300).map(|k| format!("job {k} finished in {} ms with status ok\n", k * 3)).collect();
    let msgs = vec![
        json!({"role": "user", "content": "Which job took 711 ms?"}),
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": "c1", "name": "recall", "input": {"message": "m00003"}}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "c1", "content": out}]}),
    ];
    let text = serde_json::to_string(&cur.view(0, &msgs, 0.0).unwrap().view).unwrap();
    assert!(text.contains("job 237 finished in 711 ms"), "{text}");
    assert!(!text.contains("shortened above"), "{text}");
}

#[test]
fn a_topic_left_out_names_its_messages_for_recall() {
    let run = |tools: &[&str]| {
        let mut cur = Curator::new("apart=0;epoch=0").unwrap();
        cur.tools(&tools.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let mut msgs: Vec<Value> = Vec::new();
        let mut text = String::new();
        for line in include_str!("fixtures/topics.jsonl").lines() {
            let rec: Value = serde_json::from_str(line).unwrap();
            let req = &rec["req"];
            msgs.truncate(req["base"].as_u64().unwrap() as usize);
            msgs.extend(req["new"].as_array().unwrap().iter().cloned());
            text = serde_json::to_string(&cur.view(0, &msgs, 0.0).unwrap().view).unwrap();
            if text.contains("other topics") {
                break;
            }
        }
        text
    };
    let with = run(&["Bash", "Read", "recall"]);
    assert!(with.contains("other topics") && with.contains("[recall brings any of m0"), "{with}");
    let without = run(&["Bash", "Read"]);
    assert!(without.contains("other topics") && !without.contains("[recall brings"), "{without}");
}

#[test]
fn a_small_topic_stays_in_the_view() {
    // leaving a topic out saves in proportion to its size and costs a step when the guess is wrong: the
    // topics of this short conversation hold too little to be worth that, so the view keeps them all
    let mut cur = Curator::new("").unwrap();
    cur.tools(&["Bash".to_string(), "Read".to_string(), "recall".to_string()]);
    let mut msgs: Vec<Value> = Vec::new();
    let mut seen = 0;
    for line in include_str!("fixtures/topics.jsonl").lines() {
        let rec: Value = serde_json::from_str(line).unwrap();
        let req = &rec["req"];
        msgs.truncate(req["base"].as_u64().unwrap() as usize);
        msgs.extend(req["new"].as_array().unwrap().iter().cloned());
        let r = cur.view(0, &msgs, 0.0).unwrap();
        let text = serde_json::to_string(&r.view).unwrap();
        assert!(!text.contains("other topics"), "{text}");
        seen = seen.max(r.memory.zones.len());
    }
    // the topics are still told apart, only none is left out
    assert!(seen > 2, "{seen} zones");
}

#[test]
fn a_request_naming_a_file_read_under_another_topic_gets_that_read_back() {
    // the second topic read `sulvulko_bordekul.py`; the request that names it comes while the first topic
    // is the one the view holds: its read comes back, though no other step of that topic does
    let mut cur = Curator::new("nudge=0").unwrap();
    let mut msgs: Vec<Value> = Vec::new();
    let mut text = String::new();
    for line in include_str!("fixtures/synthetic-g2-t40-s3.jsonl").lines() {
        let rec: Value = serde_json::from_str(line).unwrap();
        let req = &rec["req"];
        let Some(new) = req.get("new") else { continue };
        msgs.truncate(req["base"].as_u64().unwrap() as usize);
        msgs.extend(new.as_array().unwrap().iter().cloned());
        let r = cur.view(0, &msgs, 0.0).unwrap();
        if serde_json::to_string(new).unwrap().contains("Set SELDEKUL_1_PORT in sulvulko_bordekul.py") {
            text = serde_json::to_string(&r.view).unwrap();
            break;
        }
    }
    assert!(text.contains("nako_kuldolbor1"), "the read of the named file is missing from the view");
}

/// A session of ``steps`` steps on ``topics`` topics in turn (the person asks about another one every ``len`` steps),
/// viewed after every step: the views, in order.
fn session(spec: &str, window: u64, steps: usize, len: usize) -> Vec<Vec<Value>> {
    let mut cur = Curator::new(spec).unwrap();
    cur.window(window);
    let mut msgs = vec![json!({"role": "user", "content": "Sum the amounts of every part of topic 0."})];
    let mut views = Vec::new();
    for k in 0..steps {
        if k > 0 && k % len == 0 {
            msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Done with that topic."}]}));
            msgs.push(
                json!({"role": "user", "content": format!("Now the amounts of every part of topic {}.", k / len)}),
            );
        }
        msgs.extend(step(k, 60));
        views.push(cur.view(1_000, &msgs, 0.0).unwrap().view);
    }
    views
}

/// The share of the tokens of each view, but its last message (which carries the tail), that the next view starts
/// with unchanged: what the provider's cache reads.
fn kept(views: &[Vec<Value>]) -> f64 {
    let (mut same, mut all) = (0, 0);
    for w in views.windows(2) {
        let n = w[0].len() - 1;
        let size = |m: &Value| m.to_string().len();
        let shared: usize = w[0][..n].iter().zip(&w[1]).take_while(|(a, b)| a == b).map(|(a, _)| size(a)).sum();
        same += shared;
        all += w[0][..n].iter().map(size).sum::<usize>();
    }
    same as f64 / all as f64
}

#[test]
fn between_epochs_a_view_is_a_prefix_of_the_next() {
    // 200 steps of about 1.8k tokens in a window of 100k: the view is rewritten a few times, not on every call
    let views = session("", 100_000, 200, 9);
    let share = kept(&views);
    assert!(share >= 0.9, "{share:.3} of the tokens of the views kept as the prefix of the next");
    let before = kept(&session("epoch=0", 100_000, 200, 9));
    assert!(share > before, "{share:.3} with epochs, {before:.3} without");
}

#[test]
fn a_request_that_names_more_than_the_tail_holds_inflates_that_call_only() {
    let mut cur = Curator::new("").unwrap();
    cur.window(100_000);
    let mut msgs = vec![json!({"role": "user", "content": "Keep the settings of each topic in mind."})];
    // 24 topics, each with 12 settings, then enough work to push them out of the view
    for k in 0..24 {
        let id = format!("call_s{k}");
        let out: String = (0..12).map(|i| format!("[topic {k}] setting_{i} = value_{k}_{i}\n")).collect();
        msgs.push(json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "bash", "input": {"command": format!("cat topic_{k}.conf")}}]}));
        msgs.push(json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out}]}));
    }
    for k in 0..120 {
        msgs.extend(step(k, 60));
        cur.view(1_000, &msgs, 0.0).unwrap();
    }
    let named: Vec<String> = (0..16).map(|k| format!("topic {k}")).collect();
    msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Ready."}]}));
    msgs.push(json!({"role": "user", "content": format!("List every setting of {}.", named.join(", "))}));
    let wide = cur.view(1_000, &msgs, 0.0).unwrap();
    let lines_of =
        |r: &nexo_curator::Reply| serde_json::to_string(&r.view.last().unwrap()).unwrap().matches("] setting_").count();
    assert_eq!(wide.stats.get("inflate_wide"), Some(&1), "{:?}", wide.stats);
    assert!(lines_of(&wide) > 40, "{} lines in the tail", lines_of(&wide));
    assert!(wide.tokens < 100_000, "the inflated call stays under the window: {}", wide.tokens);
    // the next call (a step of the same request) is back to the normal view: no inflation, no lines
    msgs.extend(step(99, 60));
    let next = cur.view(1_000, &msgs, 0.0).unwrap();
    assert_eq!(next.stats.get("inflate_wide"), Some(&1), "no new inflation: {:?}", next.stats);
    assert_eq!(lines_of(&next), 0);
}
