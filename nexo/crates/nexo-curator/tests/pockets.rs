//! Pockets (`pockets=1`): the provider keeps the last view of each topic in its cache for a while, so a
//! topic the person comes back to is read from the cache, and a view whose pocket is gone is read whole.

use nexo_curator::{Curator, Reply};
use serde_json::Value;

/// A session that goes back and forth between topics.
const TOPICS: &str = "synthetic-g5-t24-s0.jsonl";

/// Replays the fixture `f` with `spec`, request `k` coming `gap(k)` seconds after the one before; the
/// provider then says it read `read(k, input)` of the `input` tokens of that request from its cache
/// (`None`: it says nothing). Each request's reply.
fn replay(f: &str, spec: &str, gap: impl Fn(usize) -> f64, read: impl Fn(usize, u64) -> Option<u64>) -> Vec<Reply> {
    let text = std::fs::read_to_string(format!("tests/fixtures/{f}")).unwrap();
    let mut cur = Curator::new(spec).unwrap();
    cur.tools(&["Bash".to_string(), "Read".to_string(), "recall".to_string()]);
    let (mut msgs, mut now, mut out) = (Vec::<Value>::new(), 0.0, Vec::new());
    for line in text.lines() {
        let rec: Value = serde_json::from_str(line).unwrap();
        let req = &rec["req"];
        if req["op"] != "view" {
            continue;
        }
        msgs.truncate(req["base"].as_u64().unwrap() as usize);
        msgs.extend(req["new"].as_array().unwrap().iter().cloned());
        let k = out.len();
        now += gap(k);
        let sys = req["system_tokens"].as_u64().unwrap_or(0);
        let r = cur.view(sys, &msgs, now).unwrap();
        let input = sys + r.tokens.max(0) as u64;
        if let Some(got) = read(k, input) {
            cur.cached(got, input);
        }
        out.push(r);
    }
    out
}

fn stat(r: &Reply, k: &str) -> i64 {
    r.stats.get(k).copied().unwrap_or(0)
}

fn last(rs: &[Reply], k: &str) -> i64 {
    stat(rs.last().unwrap(), k)
}

#[test]
fn a_topic_comes_back_to_its_pocket() {
    // a request every 30 s: each topic the session goes back to is still in the cache
    let one = replay(TOPICS, "apart=0", |_| 30.0, |_, _| None);
    let pockets = replay(TOPICS, "apart=0;pockets=1", |_| 30.0, |_, _| None);
    assert!(last(&pockets, "pocket_back") >= 1);
    assert!(last(&pockets, "pocket_hit") > 0);
    assert_eq!(last(&pockets, "pocket_dead"), 0);
    assert!(last(&pockets, "miss_switch") < last(&one, "miss_switch"));
    // off, the curator counts on the last view alone
    assert!(one.last().unwrap().stats.keys().all(|k| !k.starts_with("pocket")));
}

#[test]
fn a_pocket_dies_with_its_life() {
    // a request every 4000 s, past the hour a view is kept: no pocket lives to come back to
    let rs = replay(TOPICS, "apart=0;pockets=1", |_| 4000.0, |_, _| None);
    assert_eq!(last(&rs, "pocket_back"), 0);
    assert!(last(&rs, "pocket_dead") >= 1);
    assert!(last(&rs, "pocket_cold") >= 1);
    assert!(last(&rs, "miss_cold") > 0);
    // a provider that keeps views longer than the whole session
    let kept = replay(TOPICS, "apart=0;pockets=1;life=1000000", |_| 4000.0, |_, _| None);
    assert!(last(&kept, "pocket_back") >= 1);
    assert_eq!(last(&kept, "pocket_cold"), 0);
}

#[test]
fn only_2_cuts_a_view_whose_pocket_is_gone() {
    // a request every 4000 s: each view is read whole, and only `pockets=2` cuts it down then
    let tokens = |rs: &[Reply]| rs.iter().map(|r| r.tokens).sum::<i64>();
    let f = "synthetic-g8-t40-s7.jsonl";
    let off = replay(f, "", |_| 4000.0, |_, _| None);
    let one = replay(f, "pockets=1", |_| 4000.0, |_, _| None);
    let two = replay(f, "pockets=2", |_| 4000.0, |_, _| None);
    assert_eq!(last(&one, "pocket_squeeze"), 0);
    assert_eq!(tokens(&one), tokens(&off));
    assert!(last(&two, "pocket_squeeze") >= 1);
    assert!(tokens(&two) < tokens(&off));
}

#[test]
fn one_slot_keeps_one_pocket() {
    // `life=0`: the provider keeps the last view only, as a local server with one slot does
    let rs = replay(TOPICS, "apart=0;pockets=1;life=0", |_| 30.0, |_, _| None);
    assert_eq!(last(&rs, "pocket_back"), 0);
    assert_eq!(last(&rs, "pocket_hit"), 0);
}

#[test]
fn the_lifetime_is_learned_from_what_the_provider_read() {
    // the provider reads each view whole from its cache, but nothing of one 1000 s after the last request
    // (it keeps views less than that), and all of one 2000 s after (so that miss was its load, not its
    // lifetime)
    let gap = |k| match k {
        40 => 1000.0,
        80 => 2000.0,
        _ => 30.0,
    };
    let rs = replay(TOPICS, "pockets=1", gap, |k, input| Some(if k == 40 { 0 } else { input }));
    assert_eq!(stat(&rs[41], "life_learned"), 1);
    assert_eq!(stat(&rs[41], "life_reset"), 0);
    assert_eq!(last(&rs, "life_reset"), 1);
    // a provider that never says it read from its cache teaches nothing
    let blind = replay(TOPICS, "pockets=1", gap, |_, _| Some(0));
    assert_eq!(last(&blind, "life_learned"), 0);
    assert_eq!(last(&blind, "life_reset"), 0);
}
