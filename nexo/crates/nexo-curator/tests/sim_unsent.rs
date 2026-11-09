#![cfg(feature = "sim")]
//! Reasoning is not sent back: the replay neither prices it nor counts it as history.

use nexo_curator::sim::events::forgotten;
use nexo_curator::sim::price::tokens_of;
use serde_json::{json, Value};

fn assistant(blocks: Vec<Value>) -> Value {
    json!({"role": "assistant", "content": blocks})
}

fn thinking(t: &str) -> Value {
    json!({"type": "thinking", "thinking": t, "signature": "sig", "model": "m"})
}

fn text(t: &str) -> Value {
    json!({"type": "text", "text": t})
}

fn result(t: &str) -> Value {
    json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a", "content": t}]})
}

#[test]
fn a_thinking_block_is_priced_as_the_engine_sizes_it() {
    let plain = assistant(vec![text("hello there, the answer is 42")]);
    let with = assistant(vec![text("hello there, the answer is 42"), thinking(&"reason ".repeat(600))]);
    let rise = tokens_of(&with) - tokens_of(&plain);
    assert!((1..=8).contains(&rise), "a few tokens, not the reasoning: {rise}");
}

#[test]
fn reasoning_newlines_make_no_pseudo_words() {
    let reply = assistant(vec![thinking("it is ok.\n\nBut stock_level_check differs"), text("done")]);
    let history = vec![assistant(vec![thinking("it is ok.\n\nBut stock_level_check differs")])];
    let (lost, n) = forgotten(&reply, &history, &[], "");
    assert!(!lost.iter().any(|w| w.contains("nBut")), "{lost:?}");
    assert_eq!(n, 0, "history holds no sent text");
}

#[test]
fn a_word_only_in_older_reasoning_is_not_history() {
    let history = vec![assistant(vec![thinking("maybe warehouse_ledger_total matters")])];
    let reply = assistant(vec![thinking("so warehouse_ledger_total it is")]);
    let (lost, n) = forgotten(&reply, &history, &[], "");
    assert_eq!(n, 0);
    assert!(lost.is_empty(), "{lost:?}");
}

#[test]
fn a_reasoned_word_held_in_a_result_the_view_lacks_is_forgotten() {
    let history = vec![result("row warehouse_ledger_total = 7")];
    let reply = assistant(vec![thinking("so warehouse_ledger_total is 7"), text("ok")]);
    let (lost, n) = forgotten(&reply, &history, &[], "");
    assert!(n >= 1);
    assert!(lost.iter().any(|w| w == "warehouse_ledger_total"), "{lost:?}");
}
