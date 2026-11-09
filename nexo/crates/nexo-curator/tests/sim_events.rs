#![cfg(feature = "sim")]
//! The event engine of the session simulator (`nexo_curator::sim`).

use nexo_curator::sim::events::{run, Kind, Tool};
use nexo_curator::sim::input::load;
use nexo_curator::sim::price::Prices;

fn events(file: &str, spec: &str) -> nexo_curator::sim::events::Run {
    let path = format!("{}/tests/fixtures/sim/{file}", env!("CARGO_MANIFEST_DIR"));
    let s = load(&path).expect("the fixture loads");
    run(&s.requests, spec, Prices::LEGACY, 0.0, 0).expect("a run")
}

#[test]
fn cut_then_cited_is_one_event() {
    let r = events("cut-and-cite.jsonl", "protect=1;still=0");
    let cited: Vec<_> = r.events.iter().filter(|e| e.kind == Kind::Cited).collect();
    assert_eq!(cited.len(), 1, "events: {:?}", r.events);
    let e = cited[0];
    assert_eq!(e.tool, Tool::Read);
    // (1200 with 4 characters a token: the result is a table of values, which the accurate count prices higher)
    assert!((e.s_e - 1900).abs() <= 60, "s_e {}", e.s_e);
    // the reply that cites comes after request 5 (the fifth request holds the idle steps)
    assert_eq!(e.t, 5);
    assert_eq!(e.words, r.forgotten);
    assert!(e.words >= 2, "two identifiers: {e:?}");
}

#[test]
fn denied_result_is_one_event() {
    let r = events("denied.jsonl", "protect=1;still=0;fresh=0");
    let denied: Vec<_> = r.events.iter().filter(|e| e.kind == Kind::Denied).collect();
    assert_eq!(denied.len(), 1, "events: {:?}", r.events);
    assert_eq!(denied[0].tool, Tool::Bash);
}

#[test]
fn slots_keep_more_prefixes_than_one() {
    let path = format!("{}/tests/fixtures/sim/cut-and-cite.jsonl", env!("CARGO_MANIFEST_DIR"));
    let s = load(&path).unwrap();
    let one = run(&s.requests, "protect=1;still=0", Prices::LEGACY, 0.0, 1).unwrap();
    let many = run(&s.requests, "protect=1;still=0", Prices::LEGACY, 0.0, 4).unwrap();
    assert_eq!(one.miss + one.hit, many.miss + many.hit);
    assert!(many.hit >= one.hit);
}

/// A bench transcript with no cut in it: `n` steps, each adding a model reply and a tool result.
fn uncut_session(n: usize) -> Vec<nexo_curator::sim::input::Request> {
    use nexo_curator::sim::input::Request;
    use serde_json::json;
    let mut msgs = vec![json!({"role": "user", "content": "Fix the shipping cost in src/lib.rs."})];
    let mut out = Vec::new();
    for i in 0..n {
        out.push(Request {
            now: 1_750_000_000.0 + 2.0 * i as f64,
            system_tokens: 1600,
            system: String::new(),
            messages: msgs.clone(),
            tools: None,
            out_tokens: Some(150),
        });
        let id = format!("t{i}");
        let word = format!("step{i}");
        msgs.push(json!({"role": "assistant", "content": [
            {"type": "text", "text": format!("Reading part {word} of the file. {}", "plain words ".repeat(40))},
            {"type": "tool_use", "id": id, "name": "read", "input": {"path": format!("src/part{i}.rs")}}]}));
        msgs.push(json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": format!("{word} {}", "fn body() {} ".repeat(60))}]}));
    }
    out
}

#[test]
fn the_reply_the_model_wrote_is_read_from_the_cache_next_step() {
    use nexo_curator::sim::price::tokens_of;
    let reqs = uncut_session(6);
    let r = run(&reqs, "directives=0", Prices::LIVE, 0.0, 0).unwrap();
    let size =
        |q: &nexo_curator::sim::input::Request| q.messages.iter().map(tokens_of).sum::<i64>() + q.system_tokens as i64;
    // nothing is cut, so every view is the request as sent
    assert_eq!(r.miss + r.hit, reqs.iter().map(size).sum::<i64>());
    // a provider caches the reply with the prompt: all that each next request adds is the tool result
    let new: i64 = reqs[1..].iter().map(|q| tokens_of(q.messages.last().unwrap())).sum();
    assert_eq!(r.miss, size(&reqs[0]) + new);
}

#[test]
fn a_transcript_sends_its_system_prompt_at_every_step() {
    use nexo_curator::sim::input::transcript_requests;
    // a bench transcript records the system prompt on its first step only
    let first = r#"{"system":"You are a coding agent. Work in the repo.","tools":["bash","read"],"messages":[{"role":"user","content":"hi"}],"usage":{"output":3}}"#;
    let second = r#"{"tools":["bash","read"],"messages":[{"role":"user","content":"hi"}],"usage":{"output":3}}"#;
    let reqs = transcript_requests(&format!("{first}\n{second}\n"));
    assert!(reqs[0].system_tokens > 10);
    assert_eq!(reqs[1].system_tokens, reqs[0].system_tokens);
    assert_eq!(reqs[1].system, reqs[0].system);
}
