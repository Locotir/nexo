use super::*;
use nexo_core::message::{Message, Part, Role};
use serde_json::json;

fn call(id: &str, name: &str, path: &str) -> Message {
    Message::new(Role::Assistant, vec![Part::tool_call(id, name, json!({"path": path}))])
}

fn result(id: &str, text: &str) -> Message {
    Message::tool_results(vec![Part::tool_result(id, text, false)])
}

#[test]
fn the_tail_keeps_the_last_reply_with_its_results_or_the_new_request() {
    let h = vec![Message::user("task"), call("1", "read", "a"), result("1", "x"), call("2", "edit", "a")];
    assert_eq!(keep_recent(&h), Some(3));
    let mut h = vec![Message::user("task"), call("1", "read", "a"), result("1", "x")];
    assert_eq!(keep_recent(&h), Some(1), "the call stays with its result");
    h.push(Message::assistant("done"));
    h.push(Message::user("next"));
    assert_eq!(keep_recent(&h), Some(4), "the new request is kept whole");
    assert_eq!(keep_recent(&[Message::user("x")]), None, "nothing to summarise");
    assert_eq!(keep_recent(&[]), None);
}

#[test]
fn the_requests_and_files_are_kept_word_for_word_across_compactions() {
    let h = vec![
        Message::user("Fix the parser in src/p.rs; never touch tests/."),
        call("1", "read", "src/p.rs"),
        result("1", "fn p() {}"),
        call("2", "edit", "src/p.rs"),
        result("2", "ok"),
        call("3", "read", "README.md"),
        result("3", "readme"),
    ];
    let k = kept(&h);
    assert_eq!(k.requests, ["Fix the parser in src/p.rs; never touch tests/."]);
    assert_eq!(
        (k.changed.as_slice(), k.read.as_slice()),
        (&["src/p.rs".to_string()][..], &["README.md".to_string()][..])
    );
    // a second compaction carries the first one's parts over
    let mut again = vec![Message::new(Role::User, summary_parts(&k, "parser fixed"))];
    again.push(Message::user("Now add a test."));
    again.push(call("4", "write", "tests/t.rs"));
    again.push(result("4", "ok"));
    let k2 = kept(&again);
    assert_eq!(k2.requests, ["Fix the parser in src/p.rs; never touch tests/.", "Now add a test."]);
    assert_eq!(k2.changed, ["src/p.rs", "tests/t.rs"]);
    assert_eq!(k2.read, ["README.md"]);
}

#[test]
fn every_file_of_a_read_of_several_is_kept_as_read() {
    let h = vec![
        Message::user("Compare a and b."),
        Message::new(Role::Assistant, vec![Part::tool_call("1", "read", json!({"paths": ["a.rs", "b.rs", "a.rs"]}))]),
        result("1", "x"),
    ];
    assert_eq!(kept(&h).read, ["a.rs", "b.rs"]);
}

#[test]
fn transcript_shortens_results() {
    let m = vec![
        Message::user("hi"),
        Message::new(Role::Assistant, vec![Part::tool_call("c", "read", serde_json::json!({"path": "a"}))]),
        Message::tool_results(vec![Part::tool_result("c", "x".repeat(10_000), false)]),
    ];
    let t = transcript(&m, TRANSCRIPT_CHARS);
    assert!(t.starts_with("[person] hi\n[call read]"));
    assert!(t.len() < 3_000);
    assert!(t.contains("[...]"));
}

#[test]
fn the_transcript_fits_a_small_window_with_the_task_and_the_newest_work() {
    // a 32k window: the summary request must fit in it, its own reply included
    let max = transcript_chars(Some(32_000), 4_096);
    assert!(max / 3 + 4_096 < 32_000 && max > 20_000, "{max}");
    assert_eq!(transcript_chars(None, 4_096), TRANSCRIPT_CHARS);
    assert_eq!(transcript_chars(Some(1_000_000), 4_096), TRANSCRIPT_CHARS);
    let mut h = vec![Message::user("Port the CSV loader to streaming; keep the column order.")];
    for i in 0..200 {
        h.push(call(&format!("c{i}"), "read", &format!("data/part{i}.csv")));
        h.push(result(&format!("c{i}"), &"row,".repeat(400)));
    }
    h.push(Message::assistant("LATEST: the loader streams now."));
    let t = transcript(&h, max);
    assert!(t.chars().count() <= max + 100, "{} > {max}", t.chars().count());
    assert!(t.starts_with("[person] Port the CSV loader to streaming; keep the column order."));
    assert!(t.contains("left out") && t.ends_with("[assistant] LATEST: the loader streams now.\n"));
}

#[test]
fn the_requests_are_capped_in_tokens_keeping_the_whole_task_statement() {
    // the task's rules sit in its middle, past the 8000 characters any other message keeps
    let task = format!("Clean the sales table. {} RULE: never drop a row. {}", "a".repeat(9_000), "b".repeat(9_000));
    let mut h = vec![Message::user(task.clone())];
    for i in 0..30 {
        h.push(Message::assistant("ok"));
        h.push(Message::user(format!("step {i}: {}", "c".repeat(3_000))));
    }
    let mut k = kept(&h);
    assert_eq!(k.requests.len(), 31);
    k.cap(24_000);
    assert_eq!(k.requests[0], task, "the task statement is kept whole");
    let chars: usize = k.requests.iter().map(|r| r.chars().count()).sum();
    assert!(chars <= 24_000 * 4, "{chars}");
    assert!(k.requests.len() < 31 && k.requests.last().unwrap().starts_with("step 29:"), "the oldest go first");
    // a small window: a smaller budget, the task still first and its head and tail kept
    let mut k = kept(&h);
    k.cap(2_000);
    assert!(k.requests[0].starts_with("Clean the sales table.") && k.requests[0].chars().count() <= 4_100);
    assert!(k.requests.iter().map(|r| r.chars().count()).sum::<usize>() <= 8_000 || k.requests.len() == 1);
}

#[test]
fn what_the_harness_adds_to_a_request_is_not_kept_as_one() {
    let shared = "<memory from=\"s-1\" title=\"t\">\nShared from another session.\nExcerpts: x\n</memory>";
    let jobs = "[background jobs ended]\n- job 1 `make` ended after 3s, 2 lines of output\n\
                Read the output with the `jobs` tool (action \"output\", the job's id).";
    let attached = format!(
        "\n\n{}\n```csv\nid,total\n1,2\n```\n\n@shot.png: attached as an image",
        crate::reads::attached_header("data/sales.csv")
    );
    let h = vec![
        Message::user(format!("{shared}\n\n{jobs}\n\nSum the totals per month.{attached}")),
        Message::assistant("ok"),
        Message::user(jobs),
        Message::user(crate::turn::GOAL_CONTINUE),
        Message::new(
            Role::User,
            vec![Part::tool_result("1", "x", false), Part::text("Tool calls are off for the rest of this request.")],
        ),
    ];
    let k = kept(&h);
    assert_eq!(k.requests, ["Sum the totals per month."]);
    assert_eq!(k.read, ["data/sales.csv"], "a file attached whole was seen");
    // as nexo writes them now: all it adds first, the words last under the mark
    let start = "[project at the start of this request]\nattached: 1 files, 9 tokens";
    let now = format!(
        "{shared}\n\n{jobs}\n\n{start}{attached}\n\n{}\nSum the totals per month.",
        nexo_core::agent::PERSON_MARK
    );
    let k = kept(&[Message::user(now), Message::assistant("ok")]);
    assert_eq!(k.requests, ["Sum the totals per month."]);
    assert_eq!(k.read, ["data/sales.csv"]);
    // a project file nexo included by itself was seen too, and is no part of the request
    let own = format!("{}\n```md\nUpdate STATUS.md first.\n```", crate::reads::included_header("NOTES.md"));
    let now = format!(
        "[project at the start of this request]\nincluded by nexo, not attached by the person: 1 project file, 20 \
         tokens.\n\n{own}{attached}\n\n{}\nSum the totals per month.",
        nexo_core::agent::PERSON_MARK
    );
    let k = kept(&[Message::user(now), Message::assistant("ok")]);
    assert_eq!(k.requests, ["Sum the totals per month."]);
    assert_eq!(k.read, ["NOTES.md", "data/sales.csv"]);
    // after a message that is not the person's (mail, a goal step), nexo's files follow the text
    let k = kept(&[Message::user(format!("Go on with the goal.\n\n{own}")), Message::assistant("ok")]);
    assert_eq!(k.requests, ["Go on with the goal."]);
    assert_eq!(k.read, ["NOTES.md"]);
}

#[test]
fn a_failed_write_is_not_listed_as_changed() {
    let h = vec![
        Message::user("rename the field"),
        call("1", "edit", "src/a.ts"),
        Message::tool_results(vec![Part::tool_result("1", "old text not found", true)]),
        call("2", "read", "missing.ts"),
        Message::tool_results(vec![Part::tool_result("2", "no such file", true)]),
        call("3", "edit", "src/b.ts"),
        result("3", "ok"),
        call("4", "write", "src/c.ts"),
    ];
    let k = kept(&h);
    assert_eq!(k.changed, ["src/b.ts"], "failed and unanswered writes changed nothing");
    assert!(k.read.is_empty(), "{:?}", k.read);
}

#[test]
fn fresh_tokens_leave_out_an_earlier_summary() {
    let k = Kept { requests: vec!["x".repeat(4_000)], ..Default::default() };
    let summary = Message::new(Role::User, summary_parts(&k, &"y".repeat(4_000)));
    let small = vec![summary.clone(), call("1", "read", "a"), result("1", "z")];
    assert!(fresh_tokens(&small) < 100, "{}", fresh_tokens(&small));
    let big = vec![summary, call("1", "read", "a"), result("1", &"z".repeat(8_000))];
    assert!(fresh_tokens(&big) >= 2_000, "{}", fresh_tokens(&big));
}
