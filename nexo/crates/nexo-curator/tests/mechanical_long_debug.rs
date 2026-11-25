//! Cold, deterministic check (idea L15): a long debugging session. Many attempts at one bug, four failed
//! combinations (each a tool call + error output), then a resolution. The last request asks for the next
//! step. The curated view must still carry which attempts already failed and the final resolution (short
//! form or literal quote), so the model does not retry them. No model, no network.
//! Negative control: an unrelated old tool output with a unique marker must not come back in full.
use nexo_curator::Curator;
use serde_json::{json, Value};

const HEAD: u64 = 640;
const WINDOW: u64 = 20_000;
const TURNS: usize = 150;
const SEEDS: [usize; 3] = [5, 23, 61];
/// first turn of the debugging session (about 30% from the start)
const START: usize = 40;

const REQUEST: &str = "The flaky websocket reconnect bug: what should we try next?";
const UNRELATED_MARKER: &str = "ZQX-UNRELATED-OLD-OUTPUT";

/// (attempt description, error text) for the failed combinations
const FAILED: [(&str, &str); 4] = [
    (
        "Attempt 1 failed: raising the reconnect timeout to 30s did not stop the websocket drops.",
        "error: reconnect_timeout=30s still dropped frame 812 (ERR_WS_CLOSE_1006)",
    ),
    (
        "Attempt 2 failed: disabling permessage-deflate left the same close code 1006.",
        "error: deflate=off, handshake ok, then ERR_WS_CLOSE_1006 after 41 messages",
    ),
    (
        "Attempt 3 failed: pinning the client to HTTP/1.1 together with a longer timeout made no difference.",
        "error: http1_only+timeout=30s, ERR_WS_CLOSE_1006 on message 77",
    ),
    (
        "Attempt 4 failed: the heartbeat every 5s plus disabled deflate still lost the socket.",
        "error: heartbeat=5s deflate=off, ERR_WS_CLOSE_1006 on message 130",
    ),
];
const RESOLUTION: &str =
    "Resolved: the websocket drops came from the proxy idle timeout of 60s; setting proxy_read_timeout to 300s in nginx.conf fixed it.";

fn msg(role: &str, content: impl Into<Value>) -> Value {
    json!({"role": role, "content": content.into()})
}

fn rng(seed: usize, k: usize) -> usize {
    let mut x = (seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (k as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9)
        ^ 0x1234_5678;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    (x >> 11) as usize
}

const TOPICS: [&str; 8] = [
    "css grid layout",
    "unit test fixtures",
    "log rotation",
    "image resizing",
    "csv export",
    "cache warmup",
    "locale files",
    "retry backoff",
];

fn turn(h: &mut Vec<Value>, n: usize, ask: String, call: Value, output: String, done: String) {
    h.push(msg("user", ask));
    h.push(msg("assistant", json!([{"type":"tool_use","id":format!("t{n}"),"name":"read_file","input":call}])));
    h.push(msg("user", json!([{"type":"tool_result","tool_use_id":format!("t{n}"),"content":output}])));
    h.push(msg("assistant", done));
}

/// `debug`: plant the failed attempts + resolution; `unrelated`: plant the unrelated marker output.
fn history(seed: usize, debug: bool, unrelated: Option<usize>) -> Vec<Value> {
    let mut h = vec![];
    for n in 0..TURNS {
        let topic = TOPICS[rng(seed, n) % TOPICS.len()];
        let rows = (0..30 + rng(seed, n + 7) % 15)
            .map(|i| {
                format!(
                    "src/{topic}/part{n}_{i:03}.txt: line {} touched, checksum {:08x}\n",
                    i * 31,
                    rng(seed, n * 100 + i) & 0xffff_ffff
                )
            })
            .collect::<String>();
        let mut ask = format!("Please look at the {topic} work, step {n}.");
        let mut call = json!({"path": format!("s{n}.txt")});
        let mut out = rows;
        let mut done = format!("Step {n} on {topic} is done.");
        if debug && (START..START + 5).contains(&n) {
            let k = n - START;
            ask = format!("Still debugging the websocket reconnect bug, try number {}.", k + 1);
            if k < FAILED.len() {
                call = json!({"path": format!("ws_try{}.log", k + 1)});
                let noise = (0..25)
                    .map(|i| format!("ws[{i:02}] frame {} ok len={}\n", i * 17 + k, 100 + rng(seed, n * 7 + i) % 900))
                    .collect::<String>();
                out = format!("{noise}{}\n", FAILED[k].1);
                done = FAILED[k].0.to_string();
            } else {
                call = json!({"path": "nginx.conf"});
                out = "proxy_read_timeout 300s;\nws reconnect test: 500 messages, 0 drops\n".into();
                done = RESOLUTION.to_string();
            }
        }
        if unrelated == Some(n) {
            out = format!("{UNRELATED_MARKER}\n{}", out.repeat(2));
        }
        turn(&mut h, n, ask, call, out, done);
    }
    h.push(msg("user", REQUEST));
    h
}

fn view_text(h: &[Value]) -> (String, i64) {
    let mut c = Curator::new("").unwrap();
    c.window(WINDOW);
    c.tools(&["read_file".into(), "recall".into()]);
    let r = c.view(HEAD, h, 1.0).unwrap();
    assert!(r.tokens + HEAD as i64 <= WINDOW as i64, "over budget: {}", r.tokens);
    (serde_json::to_string(&r.view).unwrap(), r.tokens)
}

/// the sentence appears literally in the view; JSON escaping is applied to the needle
fn has(view: &str, needle: &str) -> bool {
    let esc = serde_json::to_string(needle).unwrap();
    view.contains(&esc[1..esc.len() - 1])
}

#[test]
fn long_debug_keeps_each_failed_attempt() {
    for seed in SEEDS {
        let (view, tokens) = view_text(&history(seed, true, None));
        let kept: Vec<bool> = FAILED.iter().map(|(s, _)| has(&view, s)).collect();
        println!("seed {seed}: failed attempts kept {kept:?} ({tokens} tokens)");
        for (i, k) in kept.iter().enumerate() {
            assert!(k, "attempt {} (already failed) lost from the view (seed {seed})", i + 1);
        }
    }
}

#[test]
fn long_debug_keeps_the_resolution() {
    for seed in SEEDS {
        let (view, tokens) = view_text(&history(seed, true, None));
        let kept = has(&view, RESOLUTION);
        println!("seed {seed}: resolution kept {kept} ({tokens} tokens)");
        assert!(kept, "final resolution lost from the view (seed {seed})");
    }
}

#[test]
fn long_debug_view_is_shorter_than_the_history() {
    for seed in SEEDS {
        let h = history(seed, true, None);
        let raw = serde_json::to_string(&h).unwrap().len();
        let (view, _) = view_text(&h);
        println!("seed {seed}: raw {raw} bytes, view {} bytes", view.len());
        assert!(view.len() * 2 < raw, "view not clearly smaller than history (seed {seed})");
        let full = FAILED.iter().filter(|(_, e)| has(&view, e)).count();
        println!("seed {seed}: raw error lines kept literally: {full}/{}", FAILED.len());
    }
}

#[test]
fn negative_control_unrelated_old_output_is_not_pulled_back() {
    for seed in SEEDS {
        let h = history(seed, true, Some(START - 15));
        let (view, tokens) = view_text(&h);
        let full = view.matches(UNRELATED_MARKER).count();
        println!("negative seed {seed}: unrelated marker occurrences {full} ({tokens} tokens)");
        assert_eq!(full, 0, "unrelated old output came back (seed {seed})");
        // the debugging facts are still there
        assert!(has(&view, RESOLUTION), "resolution lost (seed {seed})");
    }
}
