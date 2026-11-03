//! Per-call latency of the curator at a conversation's length.
//!
//! `cargo run --release -p nexo-curator --example bench -- MESSAGES.jsonl` with one message per line
//! (in the messages-API JSON nexo records, e.g. a saved session's messages): the curator first takes in
//! all but the last 10 requests in one call, then the last 10 are timed one by one, each from the JSON
//! line in to the JSON line out (`mean10_ms`); `curate_ms` leaves out the parsing and printing, as nexo
//! calls it in process. The agent-level benchmark is `nexo bench` (crates/nexo-bench).

use std::time::Instant;

use nexo_curator::Curator;
use serde_json::{json, Value};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: bench MESSAGES.jsonl");
        std::process::exit(2);
    };
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });
    let msgs: Vec<Value> =
        text.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect();
    let ks: Vec<usize> =
        (1..=msgs.len()).filter(|&k| msgs[k - 1].get("role").and_then(Value::as_str) == Some("user")).collect();
    let calls = 10.min(ks.len());
    let (head, tail) = ks.split_at(ks.len() - calls);
    let mut cur = Curator::new("").expect("default spec");
    let t = Instant::now();
    if let Some(&h) = head.last() {
        cur.answer(&json!({"op": "view", "system_tokens": 9000, "messages": &msgs[..h]}), 0.0);
    }
    let ingest = t.elapsed().as_secs_f64();
    let mut times = Vec::new();
    let mut inner = Vec::new();
    let mut reply = Value::Null;
    for &k in tail {
        let line = json!({"op": "view", "system_tokens": 9000, "messages": &msgs[..k]}).to_string();
        let t = Instant::now();
        let req: Value = serde_json::from_str(&line).expect("own JSON");
        let t2 = Instant::now();
        reply = cur.answer(&req, 0.0);
        inner.push(t2.elapsed().as_secs_f64());
        let out = reply.to_string();
        times.push(t.elapsed().as_secs_f64());
        assert!(reply["ok"] == json!(true), "{out}");
    }
    let curate = inner.iter().sum::<f64>() / inner.len().max(1) as f64;
    let mean = times.iter().sum::<f64>() / times.len().max(1) as f64;
    println!(
        "{}",
        json!({"messages": msgs.len(), "ingest_s": ingest, "last_ms": times.last().copied().unwrap_or(0.0) * 1000.0,
               "mean10_ms": mean * 1000.0, "curate_ms": curate * 1000.0, "view": reply["view"].as_array().map_or(0, Vec::len), "tokens": reply["tokens"]})
    );
}
