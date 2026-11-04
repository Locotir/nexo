//! Golden replay: the curator against the replies recorded in `tests/fixtures/`, one request and its
//! reply per line.
//!
//! The recorded replies are the curator's own output, kept as the reference: a change that alters what
//! the curator does shows up here line by line. When the change is intended, run the tests with
//! `NEXO_UPDATE_GOLDEN=1` to record the replies again (the requests are left as they are), review the
//! diff of the fixtures and run the tests once more without it.

use nexo_curator::Curator;
use rustc_hash::FxHashMap;
use serde_json::Value;

/// Whether this run records the replies again instead of checking them.
fn updating() -> bool {
    std::env::var("NEXO_UPDATE_GOLDEN").is_ok_and(|v| v == "1")
}

fn same(a: &Value, b: &Value, path: &str, diffs: &mut Vec<String>) {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            let (x, y) = (x.as_f64().unwrap_or(0.0), y.as_f64().unwrap_or(0.0));
            if (x - y).abs() > 1e-9 * x.abs().max(y.abs()).max(1.0) {
                diffs.push(format!("{path}: {x} != {y}"));
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                diffs.push(format!("{path}: length {} != {}", x.len(), y.len()));
            }
            for (k, (p, q)) in x.iter().zip(y).enumerate() {
                same(p, q, &format!("{path}[{k}]"), diffs);
            }
        }
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys()) {
                match (x.get(k), y.get(k)) {
                    (Some(p), Some(q)) => same(p, q, &format!("{path}.{k}"), diffs),
                    _ => diffs.push(format!("{path}.{k}: missing on one side")),
                }
            }
        }
        _ => {
            if a != b {
                let s = |v: &Value| v.to_string().chars().take(300).collect::<String>();
                diffs.push(format!("{path}: {} != {}", s(a), s(b)));
            }
        }
    }
}

fn expand(v: &Value, msgs: &[Value], prev: &[Value]) -> Value {
    match v {
        Value::Object(o) if o.len() == 1 && o.contains_key("$m") => msgs[o["$m"].as_u64().unwrap() as usize].clone(),
        Value::Object(o) if o.len() == 1 && o.contains_key("$v") => prev[o["$v"].as_u64().unwrap() as usize].clone(),
        _ => v.clone(),
    }
}

/// A view as recorded: a message sent as it is in the conversation is `{"$m": k}`, one sent as in the
/// previous view is `{"$v": j}`.
fn compress(view: &[Value], msgs: &[Value], prev: &[Value]) -> Vec<Value> {
    let index = |list: &[Value]| {
        let mut at: FxHashMap<String, usize> = FxHashMap::default();
        for (k, m) in list.iter().enumerate() {
            at.entry(m.to_string()).or_insert(k);
        }
        at
    };
    let (in_msgs, in_prev) = (index(msgs), index(prev));
    view.iter()
        .map(|v| {
            let s = v.to_string();
            match (in_msgs.get(&s), in_prev.get(&s)) {
                (Some(k), _) => serde_json::json!({"$m": k}),
                (None, Some(j)) => serde_json::json!({"$v": j}),
                _ => v.clone(),
            }
        })
        .collect()
}

/// ``line`` with its reply replaced by ``reply``; everything before the reply is kept byte for byte.
fn with_reply(line: &str, reply: &Value) -> String {
    let body = line.strip_suffix('}').expect("a fixture line is an object");
    let at = body
        .match_indices(",\"reply\":")
        .map(|(i, _)| i)
        .find(|&i| {
            serde_json::from_str::<Value>(&format!("{}}}", &body[..i])).is_ok()
                && serde_json::from_str::<Value>(&body[i + 9..]).is_ok()
        })
        .expect("a fixture line ends with its reply");
    format!("{},\"reply\":{}}}", &body[..at], reply)
}

/// Replays fixture ``name``: the differences from its recorded replies, or none when they were recorded
/// again (`NEXO_UPDATE_GOLDEN=1`).
fn run(name: &str) -> Vec<String> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap();
    let mut cur = Curator::new("").unwrap();
    let (mut msgs, mut prev): (Vec<Value>, Vec<Value>) = (Vec::new(), Vec::new());
    let mut diffs = Vec::new();
    let mut out = String::new();
    for (n, line) in text.lines().enumerate() {
        let rec: Value = serde_json::from_str(line).unwrap();
        let req = &rec["req"];
        let send = if let Some(new) = req.get("new") {
            let base = req["base"].as_u64().unwrap() as usize;
            msgs.truncate(base);
            msgs.extend(new.as_array().unwrap().iter().cloned());
            serde_json::json!({"op": "view", "system_tokens": req.get("system_tokens").cloned().unwrap_or(Value::Null), "messages": msgs})
        } else {
            req.clone()
        };
        let got = cur.answer(&send, rec["now"].as_f64().unwrap_or(0.0));
        if updating() {
            let mut reply = got.clone();
            if let Some(Value::Array(view)) = got.get("view") {
                reply["view"] = Value::Array(compress(view, &msgs, &prev));
                prev = view.clone();
            }
            out += &with_reply(line, &reply);
            out.push('\n');
            continue;
        }
        let mut want = rec["reply"].clone();
        if let Some(Value::Array(view)) = want.get("view") {
            let view: Vec<Value> = view.iter().map(|v| expand(v, &msgs, &prev)).collect();
            prev = view.clone();
            want["view"] = Value::Array(view);
        }
        let mut d = Vec::new();
        same(&got, &want, "", &mut d);
        if !d.is_empty() {
            diffs.push(format!("{name} line {n}: {}", d.into_iter().take(6).collect::<Vec<_>>().join("\n    ")));
            if diffs.len() >= 3 {
                break;
            }
        }
    }
    if updating() {
        std::fs::write(&path, out).unwrap();
    }
    diffs
}

#[test]
fn golden_fixtures() {
    let dir = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".jsonl"))
        .collect();
    names.sort();
    assert_eq!(names.len(), 20);
    let only = std::env::var("FIXTURE").unwrap_or_default();
    names.retain(|n| only.is_empty() || n.contains(&only));
    // each fixture is its own conversation: they replay side by side, a thread each, longest first
    names.sort_by_key(|n| {
        std::cmp::Reverse(
            std::fs::metadata(format!("{}/tests/fixtures/{n}", env!("CARGO_MANIFEST_DIR"))).map_or(0, |m| m.len()),
        )
    });
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(names.len().max(1));
    let mut found: Vec<(String, Vec<String>)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    while let Some(n) = names.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
                        out.push((n.clone(), run(n)));
                    }
                    out
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    });
    found.sort();
    let mut bad = Vec::new();
    for (n, d) in found {
        eprintln!("{n}: {}", if d.is_empty() { "ok" } else { "DIFF" });
        bad.extend(d);
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn the_comparison_sees_differences() {
    let mut d = Vec::new();
    same(&serde_json::json!({"a": [1, "x"]}), &serde_json::json!({"a": [1, "y"]}), "", &mut d);
    same(&serde_json::json!({"a": 1.5}), &serde_json::json!({"b": 1.5}), "", &mut d);
    assert!(d.len() >= 3, "{d:?}");
}

#[test]
fn a_recorded_view_expands_to_the_view_sent() {
    let msgs =
        vec![serde_json::json!({"role": "user", "content": "a"}), serde_json::json!({"role": "user", "content": "b"})];
    let prev = vec![serde_json::json!({"role": "user", "content": "a, cut"})];
    let view = vec![msgs[1].clone(), prev[0].clone(), serde_json::json!({"role": "user", "content": "new"})];
    let small = compress(&view, &msgs, &prev);
    assert_eq!(small[0], serde_json::json!({"$m": 1}));
    assert_eq!(small[1], serde_json::json!({"$v": 0}));
    let back: Vec<Value> = small.iter().map(|v| expand(v, &msgs, &prev)).collect();
    assert_eq!(back, view);
}

#[test]
fn recording_a_reply_keeps_the_request_as_it_was() {
    let line =
        r#"{"now":1.0,"req":{"op":"view","new":[{"role":"user","content":"say ,\"reply\": x"}]},"reply":{"ok":false}}"#;
    let got = with_reply(line, &serde_json::json!({"ok": true}));
    assert_eq!(
        got,
        r#"{"now":1.0,"req":{"op":"view","new":[{"role":"user","content":"say ,\"reply\": x"}]},"reply":{"ok":true}}"#
    );
}

#[test]
fn the_long_session_ends_with_the_reference_stats() {
    let path = format!("{}/tests/fixtures/bench-marathon-D.jsonl", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(path).unwrap();
    let mut cur = Curator::new("").unwrap();
    let mut msgs: Vec<Value> = Vec::new();
    let mut last = Value::Null;
    for line in text.lines() {
        let rec: Value = serde_json::from_str(line).unwrap();
        let req = &rec["req"];
        msgs.truncate(req["base"].as_u64().unwrap() as usize);
        msgs.extend(req["new"].as_array().unwrap().iter().cloned());
        last = cur
            .answer(&serde_json::json!({"op": "view", "system_tokens": req["system_tokens"], "messages": msgs}), 0.0);
    }
    // The reference: what the engine counts when it replays this long session (118 requests). Fixed so that a change
    // to the engine that moves any of them shows up here and is looked at, not slipped in.
    // The pins of the files the agent wrote moved them first (with nudge=0: 319 dropped chunks, 180
    // messages left, 560 ledger lines, was 327, 176, 545); the 14 nudges, kept in their results, make
    // the view grow faster, which brings the cap one more rewrite. Folding the rows of one form that a
    // message holds (a table, a listing) to their first four, and dropping the strings pulled out of a
    // binary, took the ledger from 603 lines to 261 and freed the room for 17 more chunks to leave. Leaving out what
    // a binary says about itself (section names, labels, `nm` rows, hexdump words, pointers) and weighing the
    // body of a file read or written at a third took it to 258. Rows whose numbers wander (a table of
    // measurements, not a listing of one shape) are no longer folded at birth: they are the information
    // the model came for. That leaves 2806 saved at birth (was 6840). Outputs under 100 tokens are never
    // left out in place (their stub is a third of them): 259 chunks leave, 274 ledger lines. The default then
    // took the measured view (cycle 50: no directives, no nudges, outputs closed when their request ends, copies
    // sent once, what the agent has not written about held): 249 chunks leave in 3 rewrites, 59 closed steps,
    // 708 tokens of repeated lines sent once, and no ledger, no cap rewrite, no nudge. A cut made only when
    // it saves twice the rewrite (cycle 52, `margin=2`) leaves 115 chunks in 1 rewrite, 64 closed steps. An output the same call gave
    // before, line for line, goes as one line that says nothing changed (cycle 54, `asked=3`): 2 such, 3117 saved.
    // A shorter horizon for closing (cycle 69, `ahead=0.25;margin=2.25`) closes 53 steps, not 64; the view
    // grows to the cap once, which leaves 10 messages and 41 ledger lines, and 130 chunks leave. The `miss_*` counts
    // are the tokens the cache misses behind the first message each kind of rewrite changed, `miss` all of them.
    // The line that says which calls left goes just before them, and back on the request once its message
    // has left the view: 150 more missed here (20426 for closing, was 20276), fewer over all the fixtures.
    // A block of lines an output already sent further up goes as a pointer to its first copy: 2945 saved at
    // birth, 129 chunks leave. Without a way back (the host offers no `recall`) an output folds only into what
    // gives every line back: 1799 saved at birth, and the cuts of closing miss 20992.
    // A part that sends the lines of a copy left out stays while the copy is out, or the stub of the copy would
    // point to nothing: 125 chunks leave, 22 messages, and the cache misses 39390 (was 41316).
    // The ledger folds only forms that follow each other, so lines that differ stay apart (59, was 46), and the
    // closing line sits on a message the view still sends: the cache misses 39518. The view is the same when the
    // count takes in what it left out: the anchor that leaving writes again (78 more for the cap), the outputs
    // the open request let go (1068) and the topic switch (1375, the anchor block changed with it): 40664.
    let want = serde_json::json!({"birth_saved": 1799, "dedup_runs": 6, "dedup_saved": 3117, "dedup_asked": 24,
        "dropped_chunks": 125, "sup_chunks": 31, "rewrites": 1, "left_messages": 22, "ledger_lines": 59,
        "cap_rewrites": 1, "miss_cap": 22641, "miss": 40664, "closed_steps": 47, "miss_close": 15746, "recalled_steps": 5,
        "live_dropped": 3, "miss_live": 1068, "cards": 1, "switches": 1, "miss_anchor": 1375, "miss_switch": 1375});
    eprintln!("stats: {}", last["stats"]);
    assert_eq!(last["stats"], want);
}
