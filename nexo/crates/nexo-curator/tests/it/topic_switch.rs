//! A clear change of topic: after a long task (a binary taken apart with a disassembler and a debugger, tens of
//! thousands of tokens of dumps) the person asks for something that has nothing to do with it (a small game).
//! The request does not bring the task's lines into the view, nor up to the model's window, however many of
//! them hold a word of it. The session is synthetic (`synthetic-topic-switch-re.jsonl`).

use nexo_curator::{Curator, Reply};
use serde_json::Value;

const FIXTURE: &str = "synthetic-topic-switch-re.jsonl";

/// Replays the fixture through a curator told a model window of `window` tokens (0: none). Each request's reply,
/// the tokens of the messages it added to the history, and whether the last of them is a request of the person.
fn replay(window: u64) -> Vec<(Reply, i64, bool)> {
    let path = format!("{}/tests/fixtures/{FIXTURE}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(path).unwrap();
    let mut cur = Curator::new("").unwrap();
    cur.window(window);
    let mut msgs: Vec<Value> = Vec::new();
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let rec: Value = serde_json::from_str(line).unwrap();
        let req = &rec["req"];
        if let Some(tools) = req["tools"].as_array() {
            let names: Vec<String> = tools.iter().filter_map(|t| t.as_str().map(str::to_string)).collect();
            cur.tools(&names);
        }
        let new = req["new"].as_array().unwrap();
        msgs.truncate(req["base"].as_u64().unwrap() as usize);
        msgs.extend(new.iter().cloned());
        // (a quarter of the characters: the same side of the estimate for every request)
        let added = new.iter().map(|m| m.to_string().len() as i64 / 4).sum();
        let asks = new.last().is_some_and(|m| m["role"] == "user" && !m.to_string().contains("tool_result"));
        let now = rec["now"].as_f64().unwrap_or(0.0);
        out.push((cur.view(req["system_tokens"].as_u64().unwrap_or(0), &msgs, now).unwrap(), added, asks));
    }
    out
}

/// The request that changes the topic: the person's last one.
fn change(rs: &[(Reply, i64, bool)]) -> usize {
    let at = rs.iter().rposition(|r| r.2).unwrap();
    assert!(at > 40 && at + 1 < rs.len(), "the change of topic is request {} of {}", at + 1, rs.len());
    at
}

#[test]
fn a_change_of_topic_adds_no_more_to_the_view_than_the_request() {
    // the words of the new request are in hundreds of lines of the task's dumps (a symbol table that happens to
    // hold them): they are not what the request asks for
    for window in [0, 170_000] {
        let rs = replay(window);
        let at = change(&rs);
        // the request and the first call of the new task add what they add and no line of the task before
        for k in [at, at + 1] {
            let (before, new) = (rs[k - 1].0.tokens, rs[k].0.tokens);
            assert!(
                new <= before + rs[k].1,
                "window {window}: request {}: {before} tokens, then {new} with {} new",
                k + 1,
                rs[k].1
            );
        }
        for (k, (r, ..)) in rs.iter().enumerate().skip(at) {
            assert_eq!(r.stats.get("inflate_wide").copied().unwrap_or(0), 0, "window {window}: request {}", k + 1);
        }
    }
}
