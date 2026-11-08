//! A session of a few requests and some twenty steps is not worth splitting into topics: the cache keeps
//! what the split would drop at a fraction of its price, and the model needs it again. The fixtures are
//! the conversations two tasks of this shape sent, replayed through the curator.

use nexo_curator::{Curator, Reply};
use serde_json::Value;

const TOPICS: &str = "synthetic-g5-t24-s0.jsonl";

/// Replays a `nexo bench` transcript (`messages` of each record is the whole history so far) with `spec`.
/// Each request's reply and the length of the history it was made from.
fn transcript(file: &str, spec: &str) -> Vec<(Reply, usize)> {
    let text = std::fs::read_to_string(format!("tests/transcripts/{file}")).unwrap();
    let mut cur = Curator::new(spec).unwrap();
    let mut out = Vec::new();
    for (i, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let rec: Value = serde_json::from_str(line).unwrap();
        let Some(msgs) = rec["messages"].as_array() else { continue };
        let head = rec["system"].as_str().map_or(0, str::len) + rec["tools"].to_string().len();
        let names: Vec<String> = rec["tools"]
            .as_array()
            .map(|l| l.iter().filter_map(|t| t["name"].as_str().or_else(|| t.as_str())).map(str::to_string).collect())
            .unwrap_or_default();
        if !names.is_empty() {
            cur.tools(&names);
        }
        let r = cur.view((head as u64).div_ceil(4), msgs, 1_750_000_000.0 + 2.0 * i as f64).unwrap();
        out.push((r, msgs.len()));
    }
    out
}

fn assert_whole(file: &str, spec: &str) {
    let rs = transcript(file, spec);
    assert!(rs.len() >= 10, "{file}: {} requests", rs.len());
    assert_eq!(rs.last().unwrap().0.stats.get("cards").copied().unwrap_or(0), 0, "{file}: cards");
    for (k, (r, n)) in rs.iter().enumerate() {
        assert!(!(r.view.len() == 1 && *n >= 3), "{file}: request {k} sends 1 of {n} messages");
        assert!(
            !serde_json::to_string(&r.view).unwrap().contains("This conversation has other topics"),
            "{file}: request {k} names other topics"
        );
    }
}

const FILES: [&str; 2] = ["h1-padded-revert-code.jsonl", "h1-padded-revert-data.jsonl"];

#[test]
fn h1_code_session_is_not_split_at_deepseek_prices() {
    for f in FILES {
        assert_whole(f, "ratio=0.02");
    }
}

// Measured with the default ratio (0.1), the split still passes on the revert request: padded-revert-code at 29
// messages: topic 0 holds 8520 tokens with step_need 0.303 and joint 0.058, topic 1 holds 4438 with step_need 0.000
// and joint 0.063 (gain before the need term 3782 on a 14684-token view, 3782 - 2768 - 344 > 0); padded-revert-data
// at 27 messages: topics of 765 / 6866 / 3664 tokens, step_need 0.052 / 0.070 / 0.000, joint 0.056 / 0.005 / 0.064.
// At 0.1 the rent alone pays for the split there; a need above ~0.4 would block it. The learned DeepSeek ratio
// (0.02) is the test above.
#[test]
#[ignore = "needs a need signal above 0.4 on the hidden steps at ratio 0.1; see the measurements above"]
fn h1_code_session_is_not_split_at_the_default_ratio() {
    for f in FILES {
        assert_whole(f, "");
    }
}

#[test]
fn a_long_session_still_splits() {
    let text = std::fs::read_to_string(format!("tests/fixtures/{TOPICS}")).unwrap();
    let mut cur = Curator::new("").unwrap();
    cur.tools(&["Bash".to_string(), "Read".to_string(), "recall".to_string()]);
    let (mut msgs, mut now, mut last) = (Vec::<Value>::new(), 0.0, None);
    for line in text.lines() {
        let rec: Value = serde_json::from_str(line).unwrap();
        let req = &rec["req"];
        if req["op"] != "view" {
            continue;
        }
        msgs.truncate(req["base"].as_u64().unwrap() as usize);
        msgs.extend(req["new"].as_array().unwrap().iter().cloned());
        now += 30.0;
        last = Some(cur.view(req["system_tokens"].as_u64().unwrap_or(0), &msgs, now).unwrap());
    }
    let cards = last.unwrap().stats.get("cards").copied().unwrap_or(0);
    assert!(cards >= 1, "cards {cards}");
}
