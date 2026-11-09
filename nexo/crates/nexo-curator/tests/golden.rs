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
    assert_eq!(names.len(), 21);
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

/// The tokens of the largest view of fixture ``name`` (system prompt included) and the sticky set at that request.
fn widest(name: &str) -> (i64, i64) {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap();
    let mut cur = Curator::new("").unwrap();
    let mut msgs: Vec<Value> = Vec::new();
    let (mut most, mut sticky) = (0, 0);
    for line in text.lines() {
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
        let seen = got["tokens"].as_i64().unwrap_or(0) + cur.head_tokens() as i64;
        if seen > most {
            (most, sticky) = (seen, cur.sticky_tokens());
        }
    }
    (most, sticky)
}

/// The window on the recorded sessions: a view passes the cap only when what the agent is working from alone
/// is larger (`edge` holds a 62k-token request), and a session whose agent keeps reading without writing
/// (realm100, marathon) is cut to the target in a batch, not left at the cap.
#[test]
fn the_window_holds_on_every_fixture() {
    let dir = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".jsonl"))
        .collect();
    names.sort();
    let mut bad = Vec::new();
    for n in &names {
        let (most, sticky) = widest(n);
        // (in accurate tokens: the default `cap` is 44 000)
        if most > 44_000 && most > sticky + 10_400 {
            bad.push(format!("{n}: the view reached {most} tokens with a sticky set of {sticky}"));
        }
        if (n.contains("realm100") || n.contains("marathon-D")) && most > 33_000 {
            bad.push(format!("{n}: the view reached {most} tokens, past one and a half times twice the target"));
        }
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
    // the open request let go (1068) and the topic switch (1375, the anchor block changed with it): 40664. It
    // also counts the outputs sent whole as the latest message that go as a pointer one request later
    // (`miss_fresh`, 17298): 57962. An output of the open request is priced at what it still sends, not its
    // full size: 1 leaves, not 3, and the cache misses 57478.
    // A result counts as used only on evidence (a quoted run, an exact line of an edit, a `path:line`, or a word the
    // result brought before the reply wrote it): 127 chunks leave and the cache misses 63747 (miss_close 22583).
    // Closing is priced by the need a step still has (five horizons learned over many sessions): it closes 43
    // steps, not 47, each a larger cut, and the cache misses 64020 (miss_close 22288).
    // A file read under another topic comes back when the request names it, and only that read: it closes 50 steps,
    // recalls 4. With the ledger keeping the rows of a table of values (117 lines, not 58; the anchor 1840, was 1364)
    // the cache misses 59524 (miss_close 17895).
    // The working window (`target=10000`, `cap=40000`, `hard=80000`; `cut=age`): the view is let grow to 20k tokens
    // and cut to 10k in one rewrite, and the latest unread results (`still`) are bounded by the target too, so
    // 13 rewrites and the cache misses 136746 (miss_cap 107970), 372 chunks leave by the window. Priced decisions
    // measure their room to where the cut fires (twice the target), not to the target it lands on, and the output
    // of a failing run is held below the cap: 10 rewrites, the cache misses 147781 (miss_cap 114860), 336 chunks
    // leave by the window.
    // Counted in accurate tokens (`nexo_core::tokens`, not 4 characters a token), with `target=11000`, `cap=44000`,
    // `eoq=40000` and `apart=14000`: 16 rewrites and the cache misses 181447 (miss_cap 145667), 341 chunks leave by
    // the window. The misses are in the new count, 1.3 times or more what they were; compare them on the old
    // count with the replay example, as the commit that moved the count did.
    let want = serde_json::json!({"birth_saved": 1705, "dedup_runs": 5, "dedup_saved": 4285, "miss_fresh": 15298,
        "miss": 181447, "dedup_asked": 19, "window_cuts": 341, "rewrites": 16, "miss_cap": 145667,
        "dropped_chunks": 20, "left_messages": 202, "ledger_lines": 426, "cap_rewrites": 2, "recalled_steps": 3,
        "closed_steps": 37, "miss_close": 15165, "live_dropped": 1, "miss_live": 705, "cards": 1, "switches": 1,
        "miss_switch": 8848, "sup_chunks": 8});
    eprintln!("stats: {}", last["stats"]);
    assert_eq!(last["stats"], want);
}

#[test]
fn the_real_audit_ends_with_the_reference_stats() {
    // A real session (a bench run of a security audit of a small Rust crate, 26 requests, prose and std code,
    // paths scrubbed), kept to tell words the model knows (std names) from words only the history holds.
    // Replay with `REPLAY_LOST=all` forgets 16 words in 5 replies: 7 std names, 9 private (ids, names of the
    // crate, file names). Fixed so that a change that moves what is cut shows up here.
    let path = format!("{}/tests/fixtures/real-session-vault-audit.jsonl", env!("CARGO_MANIFEST_DIR"));
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
    // the 11k target cuts when the view passes 22k, in accurate tokens (with 4 characters a token and a 10k target
    // it cut once, with 89 chunks)
    assert_eq!(
        last["stats"],
        serde_json::json!({"birth_saved": 0, "window_cuts": 125, "rewrites": 3, "miss_cap": 27465, "miss": 27961,
            "live_dropped": 1, "miss_live": 496})
    );
}
