//! The working window: a target, a cut at `cap`, a stop at `hard`, and a sticky set the cut leaves alone.

use super::*;

/// A tool result of about `tokens` tokens, every line its own words.
fn big(seed: usize, tokens: usize) -> String {
    listing(seed * 1000, seed * 1000 + tokens * 4 / 60)
}

const WINDOW: &str = "nudge=0;fresh=0;eoq=0;target=0;cap=40000;hard=80000";

#[test]
fn the_view_never_passes_the_deterministic_cap() {
    let mut e = Engine::new(params(WINDOW).unwrap());
    let mut msgs = vec![json!({"role": "user", "content": "Read the thirty parts and say what slot 50 holds."})];
    let mut longest = 0;
    for k in 1..=30 {
        msgs.extend(step(k, &format!("cat part{k}.txt"), &big(k, 3500)));
        e.sync(&msgs);
        let _ = e.view();
        let tokens = e.tokens();
        longest = longest.max(tokens);
        assert!(tokens <= 40_000, "request {k}: the view holds {tokens} tokens");
    }
    let raw: i64 = msgs.iter().map(crate::json::est).sum();
    assert!(raw > 80_000 && longest > 30_000, "the history is long ({raw}) and the view used its window ({longest})");
    assert!(e.stats.get("window_cuts").is_some_and(|n| *n > 0), "{:?}", e.stats);
}

#[test]
fn sticky_blocks_survive_the_cut() {
    let mut e = Engine::new(params(WINDOW).unwrap());
    let ask = "Fix src/a.rs until the tests pass.";
    let mut msgs = vec![json!({"role": "user", "content": ask})];
    let now = "applied and verified on disk\nfile now:\n     1│ pub fn adds(a: i32, b: i32) -> i32 { a + b }\n";
    msgs.extend(tool(1, "edit", json!({"path": "src/a.rs", "old": "a - b", "new": "a + b"}), now, false));
    let check = "test result: ok. 3 passed\n\ncheck: cargo test | exit 0 | 1.2 s\ntest result: ok. 3 passed; 0 failed";
    msgs.extend(step(2, "cargo test", check));
    // 100k tokens of reads, then the latest step
    for k in 3..=36 {
        msgs.extend(step(k, &format!("cat part{k}.txt"), &big(k, 3000)));
        e.sync(&msgs);
        let _ = e.view();
        let view = serde_json::to_string(&e.view()).unwrap();
        assert!(view.contains(ask), "the request stays at {k}");
        assert!(view.contains("applied and verified on disk"), "the latest `file now` stays at {k}");
        assert!(view.contains("check: cargo test | exit 0"), "the last check stays at {k}");
    }
    assert!(e.stats.get("window_cuts").is_some_and(|n| *n > 0), "{:?}", e.stats);
    let first_read = big(3, 3000);
    let view = serde_json::to_string(&e.view()).unwrap();
    assert!(!view.contains(&first_read[..200].replace('\n', "\\n")), "the oldest read was cut");
}

#[test]
fn still_is_bounded_by_the_target() {
    let build = |spec: &str, ask: &str| {
        let mut e = Engine::new(params(spec).unwrap());
        let mut msgs = vec![json!({"role": "user", "content": ask})];
        for k in 1..=12 {
            msgs.extend(step(k, &format!("cat part{k}.txt"), &big(k, 2000)));
        }
        e.sync(&msgs);
        e.decide();
        e
    };
    let small = build("nudge=0;fresh=0;eoq=16000;still=60000;target=20000", "Say what slot 50 holds.");
    let sticky = small.sticky_tokens();
    assert!(sticky > 0 && sticky < 4000, "{sticky}");
    assert_eq!(small.still_now(), 40_000 - small.head - sticky, "the window leaves what the sticky set does not take");
    // a request that alone takes the window leaves the floor, and `still` itself caps the rest
    let long = format!("Say what slot 50 holds in this listing:\n{}", big(99, 45_000));
    let big_ask = build("nudge=0;fresh=0;eoq=16000;still=60000;target=20000", &long);
    assert!(big_ask.sticky_tokens() > 40_000, "{}", big_ask.sticky_tokens());
    assert_eq!(big_ask.still_now(), 4000);
    // no target: the flat budget as before
    let flat = build("nudge=0;fresh=0;eoq=16000;still=32000;target=0;cap=0;hard=0", "Say what slot 50 holds.");
    assert_eq!(flat.still_now(), 32_000);
}
