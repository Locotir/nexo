use super::*;
use nexo_core::message::{Message, Part};
use nexo_core::stream::Usage;
use serde_json::json;
use serde_json::Value;
use std::time::Duration;

#[test]
fn decisions_are_read_from_the_first_lines() {
    let d = parse_decision("CHECKPOINT CONTINUE: tests still failing in parser.rs. NEXT: fix it").unwrap();
    assert!(d.go_on);
    assert_eq!(d.reason, "tests still failing in parser.rs");
    let d = parse_decision("**checkpoint stop:** all done\nThe parser works.").unwrap();
    assert!(!d.go_on);
    assert_eq!((d.reason.as_str(), d.rest.as_str()), ("all done", "The parser works."));
    let d = parse_decision("Let me think.\n\nCheckpoint Continue - more to do").unwrap();
    assert!(d.go_on && d.reason == "more to do");
    assert!(parse_decision("a\nb\nc\nd\ne\nf\ng\nh\nCHECKPOINT STOP: late").is_none(), "only the first eight lines");
    assert!(parse_decision("I will stop at the checkpoint").is_none());
    assert!(parse_decision("").is_none());
    assert!(parse_decision("CHECKPOINT").is_none());
    let d = parse_decision("Voy a decidir.\n1. **CHECKPOINT PARAR**: ya esta").unwrap();
    assert!(!d.go_on && d.reason == "ya esta");
    let d = parse_decision("[CHECKPOINT CONTINUAR]: sigo. NEXT: a").unwrap();
    assert!(d.go_on && d.reason == "sigo");
    assert!(parse_decision("checkpoint stopwatch").is_none(), "a word that only begins with the marker word");
    assert!(parse_decision("a\nb\nc\nd\ne\nf\ng\nCHECKPOINT STOP: in the eighth").is_some());
}

#[test]
fn progress_needs_one_signal() {
    let a = Mark { writes: 2, wrote: 2, fresh: 10, todo: 7 };
    assert!(!progress(&a, &a));
    assert!(!progress(&Mark { writes: 3, ..a.clone() }, &a), "a write that left nothing new is not progress");
    assert!(progress(&Mark { wrote: 3, ..a.clone() }, &a));
    assert!(progress(&Mark { fresh: 11, ..a.clone() }, &a));
    assert!(progress(&Mark { todo: 8, ..a.clone() }, &a));
}

#[test]
fn the_todo_list_is_read_from_the_last_call() {
    let call = |items: Value| {
        Message::new(nexo_core::message::Role::Assistant, vec![Part::tool_call("t", "todo", json!({"items": items}))])
    };
    let h = vec![
        call(json!([{"text": "a", "status": "done"}])),
        call(json!([{"text": "a", "status": "done"}, {"text": "fix parser", "status": "in_progress"}, {"text": "c"}])),
    ];
    let t = todo_of(&h).unwrap();
    assert_eq!((t.done, t.doing, t.pending), (1, 1, 1));
    assert_eq!(t.doing_text.as_deref(), Some("fix parser"));
    assert_ne!(t.hash, todo_of(&h[..1]).unwrap().hash);
    assert!(todo_of(&[]).is_none());
    assert_eq!(calls_in(&h), 2);
}

fn big() -> Report {
    Report {
        number: 2,
        cap: Some(5),
        cost: 0.31,
        calls: 400,
        rounds: 400,
        input: 1_200_000,
        output: 38_000,
        cached: 1_116_000,
        elapsed: Duration::from_secs(41 * 60),
        request: "x".repeat(900),
        goal: Some("g".repeat(500)),
        todo: Some(Todo { done: 3, doing: 1, pending: 2, doing_text: Some("fix parser tests".into()), hash: 1 }),
        prev_todo: Some(Todo { done: 2, hash: 2, ..Default::default() }),
        files: (0..30).map(|i| format!("src/file{i}.rs")).collect(),
        new_files: (0..30).map(|i| format!("src/file{i}.rs")).collect(),
        fresh_total: 300,
        fresh_new: 85,
        build: Some((391, "cargo test -> test result: FAILED. 2 failed".into())),
        errors: (0..20).map(|i| (format!("error number {i} {}", "e".repeat(100)), 4)).collect(),
        loop_hits: 0,
        loop_limit: 3,
        stale: 2,
        stale_limit: 6,
        spend: "$0.31 this request, $2.10 of $5.00 this session (max_cost)".into(),
        idle: 0,
    }
}

#[test]
fn the_message_is_compact_and_has_the_header() {
    let m = render(&big());
    assert!(m.chars().count() <= MAX_CHARS, "{}", m.chars().count());
    for keep in ["Since the last checkpoint", "Spend:", "Guards:", "Todo:"] {
        assert!(m.contains(keep), "{keep} survives a long message: {m}");
    }
    assert!(m.starts_with("[checkpoint 2/5] Session at this point: $0.31 · 400 tool calls · 400 steps · 1.2M in / 38k out, 93% cached · 41m | Consider, based on all the progress"), "{m}");
    assert!(m.contains("CHECKPOINT CONTINUE") && m.contains("CHECKPOINT STOP"));
    let mut small = big();
    small.cap = None;
    small.errors.clear();
    small.files = vec!["a.rs".into()];
    small.new_files = small.files.clone();
    small.request = "fix it".into();
    small.goal = None;
    let m = render(&small);
    assert!(m.starts_with("[checkpoint 2] Session"), "{m}");
    assert!(m.contains("Goal: none") && m.contains("(a.rs)") && !m.contains("Repeated errors"), "{m}");
}

#[test]
fn an_idle_checkpoint_says_so() {
    let mut r = big();
    r.idle = 1;
    assert!(render(&r).contains("no progress"));
}

#[test]
fn build_lines_and_repeated_errors_are_noted() {
    assert_eq!(build_line("ls", "error: x"), None);
    assert_eq!(
        build_line("cargo test -j 2", "running 3 tests\ntest result: FAILED. 1 passed; 2 failed\n\n").as_deref(),
        Some("test result: FAILED. 1 passed; 2 failed")
    );
    let mut t = Tracker::new(0.0, Usage::default(), Mark::default(), None);
    let err = vec![Part::ToolResult {
        call_id: "1".into(),
        name: Some("bash".into()),
        content: vec![Part::text("error: cannot borrow x\nmore")],
        is_error: true,
    }];
    for _ in 0..3 {
        t.round(&[("bash".into(), json!({"command": "cargo build"}).to_string())], &err);
    }
    assert_eq!(t.repeated(), vec![("error: cannot borrow x".to_string(), 3)]);
    assert_eq!(t.build().unwrap().0, 3);
    assert_eq!(tokens(1_200_000), "1.2M");
    assert_eq!(elapsed(Duration::from_secs(3 * 3600 + 5 * 60)), "3h05m");
}
