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
fn still_is_bounded_by_the_target_less_head_and_sticky() {
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
    assert_eq!(small.still_now(), 20_000 - small.head - sticky, "the window leaves what the sticky set does not take");
    // a request that alone takes the window leaves the floor, and `still` itself caps the rest
    let long = format!("Say what slot 50 holds in this listing:\n{}", big(99, 25_000));
    let big_ask = build("nudge=0;fresh=0;eoq=16000;still=60000;target=20000", &long);
    assert!(big_ask.sticky_tokens() > 20_000, "{}", big_ask.sticky_tokens());
    assert_eq!(big_ask.still_now(), 4000);
    // no target: the flat budget as before
    let flat = build("nudge=0;fresh=0;eoq=16000;still=32000;target=0;cap=0;hard=0", "Say what slot 50 holds.");
    assert_eq!(flat.still_now(), 32_000);
}

/// The tokens of the view after each request of a long run of reads, and the requests that cut.
fn run_reads(spec: &str, steps: usize) -> (Vec<i64>, Vec<usize>) {
    let mut e = Engine::new(params(spec).unwrap());
    let mut msgs = vec![json!({"role": "user", "content": "Read the parts and say what slot 50 holds."})];
    let (mut tokens, mut cuts) = (Vec::new(), Vec::new());
    for k in 1..=steps {
        msgs.extend(step(k, &format!("cat part{k}.txt"), &big(k, 900)));
        let before =
            e.stats.get("window_cuts").copied().unwrap_or(0) + e.stats.get("cap_rewrites").copied().unwrap_or(0);
        e.sync(&msgs);
        let _ = e.view();
        tokens.push(e.tokens());
        if e.stats.get("window_cuts").copied().unwrap_or(0) + e.stats.get("cap_rewrites").copied().unwrap_or(0) > before
        {
            cuts.push(k);
        }
    }
    (tokens, cuts)
}

#[test]
fn the_cut_starts_at_twice_the_target_and_lands_on_it() {
    let (tokens, cuts) = run_reads("nudge=0;fresh=0;still=0;target=10000;cap=40000;hard=80000", 60);
    let longest = tokens.iter().copied().max().unwrap();
    // it never grows past twice the target (and the step that reaches it), and what is left lands near the target
    assert!(longest <= 20_000 + 1_500, "the view grew to {longest}: {tokens:?}");
    assert!(longest > 18_000, "the view used the room up to the cut: {tokens:?}");
    assert!(cuts.len() >= 2, "{cuts:?} {tokens:?}");
    for &k in &cuts {
        assert!(tokens[k - 1] <= 10_000 + 1_500, "request {k} cut to {}: {tokens:?}", tokens[k - 1]);
    }
    // one cut buys several requests: it is not a trim at every step
    assert!(cuts.len() * 4 < tokens.len(), "{cuts:?}");
}

#[test]
fn the_fixed_cut_lands_on_the_target_too() {
    // the cap comes before twice the target: the cut starts at the cap and still lands on the target
    let (tokens, cuts) = run_reads("nudge=0;fresh=0;still=0;target=10000;cap=15000;hard=80000", 60);
    assert!(cuts.len() >= 2, "{cuts:?} {tokens:?}");
    for &k in &cuts {
        assert!(tokens[k - 1] <= 10_000 + 1_500, "request {k} cut to {}: {tokens:?}", tokens[k - 1]);
    }
    assert!(tokens.iter().all(|&t| t <= 15_000 + 1_500), "{tokens:?}");
    assert!(tokens.iter().copied().max().unwrap() > 13_000, "the view used the room up to the cap: {tokens:?}");
    assert!(cuts.len() * 4 < tokens.len(), "{cuts:?}");
}

/// A session that ran a failing test, fixed it and read on; the failing run's output is `failure`. Returns
/// whether the last view still holds `mark` after the window cut the reads.
fn failure_survives(failure: &str, mark: &str) -> bool {
    let mut e = Engine::new(params(WINDOW).unwrap());
    let mut msgs = vec![json!({"role": "user", "content": "Fix the stock tests, then read the parts."})];
    msgs.extend(step(1, "cargo test 2>&1 | tail -40", failure));
    msgs.extend(tool(
        2,
        "edit",
        json!({"path": "src/stock.rs", "old": "a", "new": "b"}),
        "applied and verified on disk",
        false,
    ));
    msgs.extend(step(3, "cargo test 2>&1 | tail -40", "test result: ok. 5 passed; 0 failed; 0 ignored"));
    for k in 4..=40 {
        msgs.extend(step(k, &format!("cat part{k}.txt"), &big(k, 1500)));
        e.sync(&msgs);
        let _ = e.view();
    }
    assert!(e.stats.get("window_cuts").is_some_and(|n| *n > 0), "{:?}", e.stats);
    let view = serde_json::to_string(&e.view()).unwrap();
    assert!(!view.contains(&big(4, 1500)[..200].replace('\n', "\\n")), "the oldest read was cut");
    view.contains(mark)
}

#[test]
fn a_failing_run_outlives_the_reads_the_window_cuts() {
    // what the agent reports at the end is what it fixed: the lines of the failure it quotes stay
    let failure = format!(
        "{}\nfailures:\n\n---- remove_too_many stdout ----\nthread 'remove_too_many' panicked at tests/stock.rs:28:5\n\ntest result: FAILED. 4 passed; 1 failed",
        big(55, 400)
    );
    assert!(failure_survives(&failure, "remove_too_many stdout"));
}

#[test]
fn a_failing_run_past_the_budget_leaves_with_the_rest() {
    // the held failures are a few thousand tokens at most: a 6k-token log of one leaves like any old result
    let failure = format!("---- remove_too_many stdout ----\nFAILED\n{}", big(77, 6000));
    assert!(!failure_survives(&failure, "remove_too_many stdout"));
}
