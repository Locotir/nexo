//! The curator saved at a point of a conversation and read back (`nexo_race::snapshot`) answers every later
//! request as the curator that ran through the whole conversation does, and a saved state that does not fit
//! the history it is read for is refused.

use nexo_race::snapshot::{history_hash, message_hash};
use nexo_race::Race;
use serde_json::Value;

/// One request of a fixture: what is sent, when, and the messages of the conversation after it.
struct Req {
    send: Value,
    now: f64,
    msgs: Vec<Value>,
}

fn requests(name: &str) -> Vec<Req> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap();
    let mut msgs: Vec<Value> = Vec::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let rec: Value = serde_json::from_str(line).unwrap();
        let req = &rec["req"];
        let send = if let Some(new) = req.get("new") {
            msgs.truncate(req["base"].as_u64().unwrap() as usize);
            msgs.extend(new.as_array().unwrap().iter().cloned());
            serde_json::json!({"op": "view", "system_tokens": req.get("system_tokens").cloned().unwrap_or(Value::Null), "messages": msgs})
        } else {
            req.clone()
        };
        out.push(Req { send, now: rec["now"].as_f64().unwrap_or(0.0), msgs: msgs.clone() });
    }
    out
}

fn hashes(msgs: &[Value]) -> Vec<u64> {
    msgs.iter().map(message_hash).collect()
}

/// The saved state of `cur`, whose history is `msgs`.
fn save(cur: &Race, msgs: &[Value]) -> Vec<u8> {
    let covered = cur.covered();
    cur.save(1, history_hash(&hashes(&msgs[..covered]))).encode()
}

/// Runs fixture `name` whole, saves the curator after the requests at `points`, reads each saved state back
/// and runs the rest of the fixture on it: the first request whose reply differs from the whole run's, if any.
fn restored_differs(name: &str, points: &[usize]) -> Option<String> {
    let reqs = requests(name);
    let mut cur = Race::new("").unwrap();
    let mut replies = Vec::new();
    let mut saved = Vec::new();
    for (k, r) in reqs.iter().enumerate() {
        replies.push(cur.answer(&r.send, r.now));
        if points.contains(&k) && r.send["op"] == "view" {
            saved.push((k, save(&cur, &r.msgs)));
        }
    }
    assert!(!saved.is_empty(), "{name}: no view at the points");
    for (k, bytes) in saved {
        let msgs = &reqs[k].msgs;
        let (mut back, head) = Race::restore("", &bytes, msgs, &hashes(msgs)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(head.covered as usize, back.covered(), "{name}");
        for (j, r) in reqs.iter().enumerate().skip(k + 1) {
            let got = back.answer(&r.send, r.now);
            if got != replies[j] {
                let at = |v: &Value| v.to_string().chars().take(400).collect::<String>();
                return Some(format!(
                    "{name}: saved after request {k}, request {j} differs:\n{}\n{}",
                    at(&got),
                    at(&replies[j])
                ));
            }
        }
    }
    None
}

#[test]
fn a_curator_read_back_answers_every_later_request_as_the_one_that_ran_through() {
    let dir = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".jsonl"))
        .collect();
    names.sort();
    let only = std::env::var("FIXTURE").unwrap_or_default();
    names.retain(|n| only.is_empty() || n.contains(&only));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(names.len().max(1));
    let bad: Vec<String> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    while let Some(n) = names.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
                        let len = requests(n).len();
                        let points: Vec<usize> = (1..4).map(|q| q * len / 4).collect();
                        out.extend(restored_differs(n, &points));
                    }
                    out
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    });
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}

/// A curator that ran through the first requests of a fixture, its history then, and the saved state.
fn saved_at_half(name: &str) -> (Race, Vec<Value>, Vec<u8>) {
    let reqs = requests(name);
    let mut cur = Race::new("").unwrap();
    let half = reqs.len() / 2;
    for r in &reqs[..=half] {
        cur.answer(&r.send, r.now);
    }
    let msgs = reqs[half].msgs.clone();
    let bytes = save(&cur, &msgs);
    (cur, msgs, bytes)
}

#[test]
fn a_state_read_back_over_a_history_that_grew_takes_only_the_new_messages() {
    let (cur, msgs, bytes) = saved_at_half("real-session-vault-audit.jsonl");
    let mut longer = msgs.clone();
    longer.push(serde_json::json!({"role": "user", "content": [{"type": "text", "text": "one more thing"}]}));
    let (back, head) = Race::restore("", &bytes, &longer, &hashes(&longer)).unwrap();
    assert_eq!(head.covered as usize, cur.covered());
    assert_eq!(back.covered(), msgs.len());
}

#[test]
fn a_state_saved_by_another_build_is_refused() {
    let (_, msgs, mut bytes) = saved_at_half("real-session-vault-audit.jsonl");
    bytes[8] ^= 1;
    let e = Race::restore("", &bytes, &msgs, &hashes(&msgs)).err().unwrap();
    assert!(e.contains("another build"), "{e}");
}

#[test]
fn a_state_saved_with_other_settings_is_refused() {
    let (_, msgs, bytes) = saved_at_half("real-session-vault-audit.jsonl");
    let e = Race::restore("pockets=1", &bytes, &msgs, &hashes(&msgs)).err().unwrap();
    assert!(e.contains("settings"), "{e}");
}

#[test]
fn a_state_over_a_history_edited_since_is_refused() {
    let (_, mut msgs, bytes) = saved_at_half("real-session-vault-audit.jsonl");
    msgs[1]["content"] = serde_json::json!([{"type": "text", "text": "an edited message"}]);
    let e = Race::restore("", &bytes, &msgs, &hashes(&msgs)).err().unwrap();
    assert!(e.contains("history differs"), "{e}");
    let short = &msgs[..msgs.len() / 2];
    let e = Race::restore("", &bytes, short, &hashes(short)).err().unwrap();
    assert!(e.contains("covers"), "{e}");
}

#[test]
fn a_cut_or_damaged_state_is_refused() {
    let (_, msgs, bytes) = saved_at_half("real-session-vault-audit.jsonl");
    for cut in [0, 10, 64, bytes.len() / 2, bytes.len() - 1] {
        assert!(Race::restore("", &bytes[..cut], &msgs, &hashes(&msgs)).is_err(), "cut at {cut}");
    }
    let mut bad = bytes.clone();
    let at = bad.len() - 7;
    bad[at] ^= 0x40;
    let e = Race::restore("", &bad, &msgs, &hashes(&msgs)).err().unwrap();
    assert!(e.contains("damaged"), "{e}");
}
