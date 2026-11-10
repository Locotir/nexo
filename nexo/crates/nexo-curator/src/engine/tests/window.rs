//! The working window: a target, a cut at `cap`, a stop at `hard`, and a sticky set the cut leaves alone.

use super::*;

/// A tool result of about `tokens` tokens, every line its own words.
pub(super) fn big(seed: usize, tokens: usize) -> String {
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
fn still_holds_the_newest_unwritten_reads_whatever_the_head_takes() {
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
    // the target less the head and the sticky set is no bound: `still` itself is, a request that alone takes the
    // window or not
    let small = build("nudge=0;fresh=0;eoq=16000;still=60000;target=20000", "Say what slot 50 holds.");
    assert_eq!(small.still_now(), 60_000);
    let long = format!("Say what slot 50 holds in this listing:\n{}", big(99, 25_000));
    let big_ask = build("nudge=0;fresh=0;eoq=16000;still=60000;target=20000", &long);
    assert!(big_ask.sticky_tokens() > 20_000, "{}", big_ask.sticky_tokens());
    assert_eq!(big_ask.still_now(), 60_000);
    // no target: the flat budget as before
    let flat = build("nudge=0;fresh=0;eoq=16000;still=32000;target=0;cap=0;hard=0", "Say what slot 50 holds.");
    assert_eq!(flat.still_now(), 32_000);
    let off = build("nudge=0;fresh=0;eoq=16000;still=0;target=20000", "Say what slot 50 holds.");
    assert_eq!(off.still_now(), 0);
}

#[test]
fn a_big_request_does_not_cut_the_reads_the_agent_is_working_from() {
    // a task of 11.6k tokens, a system prompt of 3k, 9 reads of 3.5k and nothing written, on the default
    // settings: every read is what the agent works from (`still=32000`), so none leaves although the view passes
    // twice the target
    let mut e = Engine::new(crate::params("").unwrap());
    e.head = 3_000;
    let task = format!("Find what is wrong in the parts.\n{}", big(99, 11_600));
    let mut msgs = vec![json!({"role": "user", "content": task})];
    for k in 1..=9 {
        msgs.extend(step(k, &format!("cat part{k}.txt"), &big(k, 3500)));
        e.sync(&msgs);
        let view = sent(&e.view());
        for j in 1..=k {
            let mark = big(j, 3500)[..200].replace('\n', "\\n");
            assert!(view.contains(&mark), "request {k}: read {j} left the view: {:?}", e.stats);
        }
    }
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

#[test]
fn a_chat_of_pasted_text_has_no_results_to_cut() {
    // the window drops tool results only: what the person pasted is theirs and stays, however far past `cap`
    let mut e = Engine::new(params("nudge=0;fresh=0;eoq=0;target=10000;cap=20000;hard=80000").unwrap());
    let mut msgs = vec![];
    for k in 1..=6 {
        msgs.push(json!({"role": "user", "content": format!("Topic {k}.\n{}\nWhat is the total?", big(k, 6000))}));
        msgs.push(json!({"role": "assistant", "content": "42.00"}));
        e.sync(&msgs);
        let _ = e.view();
    }
    assert!(e.tokens() > 25_000, "the view is far past the cap: {}", e.tokens());
    assert!(e.stats.get("window_cuts").is_none(), "{:?}", e.stats);
}

#[test]
fn a_recall_result_is_not_cut_on_the_request_after_it_came() {
    // the agent asked for a message back and got it; the next step brings a long output and the window cuts:
    // what it asked for stays, the older read goes
    let mut e = Engine::new(params("nudge=0;fresh=0;eoq=0;still=0;target=0;cap=20000;hard=80000").unwrap());
    let mut msgs = vec![json!({"role": "user", "content": "Read the parts and find what is wrong in them."})];
    msgs.extend(step(1, "cat part1.txt", &big(1, 3000)));
    let back = big(77, 3000);
    msgs.extend(super::recall::recall_step(2, 2, None, &back));
    let mark = |text: &str| text[..200].replace('\n', "\\n");
    e.sync(&msgs);
    assert!(sent(&e.view()).contains(&mark(&back)), "the request that carries it holds it");
    msgs.extend(step(3, "cat part3.txt", &big(3, 24_000)));
    e.sync(&msgs);
    let view = sent(&e.view());
    assert!(e.stats.get("window_cuts").is_some_and(|n| *n > 0), "the window cut: {:?}", e.stats);
    assert!(!view.contains(&mark(&big(1, 3000))), "the older read left");
    assert!(view.contains(&mark(&back)), "the recall result survived the cut on the next request");
}

/// A session of `n` files the agent edits and reads whole (each read about `tokens` tokens): the work set, which
/// only the ceiling may cut. Returns the engine after each step's request with the view's tokens (head included).
fn work_set(e: &mut Engine, n: usize, tokens: usize) -> Vec<i64> {
    let mut msgs = vec![json!({"role": "user", "content": "Fix every module under src until the tests pass."})];
    let mut seen = Vec::new();
    for k in 1..=n {
        let path = format!("src/f{k}.rs");
        msgs.extend(tool(
            2 * k,
            "edit",
            json!({"path": path, "old": "a", "new": "b"}),
            "applied and verified on disk",
            false,
        ));
        msgs.extend(tool(2 * k + 1, "read", json!({"path": path}), &big(k, tokens), false));
        e.sync(&msgs);
        let _ = e.view();
        seen.push(e.tokens() + e.head);
    }
    seen
}

#[test]
fn a_big_window_never_cuts_the_work_set_at_a_fixed_ceiling() {
    // the agent works from 50 files of 5k tokens (250k): with a 1M window nothing cuts them, where a ceiling of
    // 80k (the old `hard`) would have
    let mut e = Engine::new(params("eoq=100000000;still=0;target=10000").unwrap());
    e.head = 3_000;
    e.set_budget(1_000_000 - 3_000);
    let seen = work_set(&mut e, 50, 5_000);
    assert!(seen.last().copied().unwrap() > 200_000, "{seen:?}");
    assert!(seen.windows(2).all(|w| w[1] > w[0]), "the view only grew: {seen:?}");
    assert!(e.stats.get("window_cuts").is_none() && e.stats.get("window_over_hard").is_none(), "{:?}", e.stats);
    // the same session in a 128k window is held under 95 % of it, and only past it
    let mut small = Engine::new(params("eoq=100000000;still=0;target=10000").unwrap());
    small.head = 3_000;
    small.set_budget(128_000 - 3_000);
    let seen = work_set(&mut small, 50, 5_000);
    assert!(seen.iter().all(|&t| t <= 128_000 * 95 / 100), "{seen:?}");
    assert!(seen.iter().copied().max().unwrap() > 100_000, "the view used the window: {seen:?}");
    assert!(small.stats.get("window_cuts").is_some_and(|n| *n > 0), "{:?}", small.stats);
}

#[test]
fn an_unknown_window_falls_back_to_the_spec_hard_or_to_none() {
    // no window given: the spec's `hard` is the ceiling when it has one
    let mut fixed = Engine::new(params("eoq=100000000;still=0;target=10000;hard=80000").unwrap());
    fixed.head = 3_000;
    let seen = work_set(&mut fixed, 50, 5_000);
    assert!(seen.iter().all(|&t| t <= 80_000), "{seen:?}");
    assert!(fixed.stats.get("window_cuts").is_some_and(|n| *n > 0), "{:?}", fixed.stats);
    // and none when it has none: the default spec has no number of its own
    let mut open = Engine::new(params("eoq=100000000;still=0;target=10000").unwrap());
    open.head = 3_000;
    let seen = work_set(&mut open, 50, 5_000);
    assert!(seen.last().copied().unwrap() > 200_000, "{seen:?}");
    assert!(open.stats.get("window_cuts").is_none(), "{:?}", open.stats);
    // the default spec is that one
    assert_eq!(crate::params("").unwrap().hard, 0);
    assert_eq!(crate::params("").unwrap().cap, 0);
}

/// The rewrites a run made: window cuts and cuts of the priced cap.
fn cuts(stats: &IndexMap<&'static str, i64>) -> i64 {
    (stats.get("window_cuts").copied().unwrap_or(0) > 0) as i64 + stats.get("cap_rewrites").copied().unwrap_or(0)
}

/// Reads of 1500 tokens none of which the agent works from, `n` of them; `ratio`, `ahead` and `steps_before`
/// (steps of the session before them, a stand-in for what the session has still to run) set the price of keeping
/// them against rewriting. Returns the stats and the largest view.
fn stale_body(ratio: f64, ahead: f64, steps_before: usize, n: usize) -> (IndexMap<&'static str, i64>, i64) {
    let mut e = Engine::new(params(&format!("eoq=100000000;still=0;target=10000;ahead={ahead};margin=1")).unwrap());
    e.set_ratio(ratio);
    let mut msgs = vec![json!({"role": "user", "content": "Read the parts and say what slot 50 holds."})];
    for k in 1..=steps_before {
        msgs.extend(step(k, "true", "ok"));
    }
    let mut most = 0;
    for k in 1..=n {
        msgs.extend(step(1000 + k, &format!("cat part{k}.txt"), &big(k, 1_500)));
        e.sync(&msgs);
        let _ = e.view();
        most = most.max(e.tokens());
    }
    (e.stats.clone(), most)
}

#[test]
fn a_rewrite_waits_when_the_cache_is_cheap_and_few_calls_remain() {
    // 30 reads of 1.5k (45k) on a cache that costs 2 %, a session at its fourth call: keeping 35k extra tokens
    // for the 4 calls ahead costs less than reading the new 10k prefix without the cache
    let (stats, most) = stale_body(0.02, 0.25, 0, 30);
    assert_eq!(cuts(&stats), 0, "{stats:?}");
    assert!(most > 35_000, "the view grew past twice the target: {most}");
}

#[test]
fn a_rewrite_comes_when_many_calls_remain_and_the_body_is_stale() {
    // the same reads at the 120th call of a long session (30 calls ahead): the stale body costs more to keep than
    // the 10k prefix costs to send again, so it is cut, and cut once for a stretch of requests, not at every call
    let (stats, most) = stale_body(0.02, 0.25, 120, 30);
    assert!(cuts(&stats) > 0, "{stats:?}");
    assert!(most < 30_000, "the view stayed small: {most}");
    assert!(cuts(&stats) < 10, "{stats:?}");
    // the dearer the cache, the dearer keeping the extra tokens, and the sooner it pays to rewrite
    let (dear, most_dear) = stale_body(0.5, 0.25, 120, 30);
    assert!(most_dear < most, "{most_dear} against {most}: {dear:?}");
}

#[test]
fn a_very_long_view_pays_for_its_quality_even_with_few_calls_left() {
    // 100 reads of 1.5k, 4 calls ahead on a cheap cache: past the long-context knee the view is cut
    let (stats, most) = stale_body(0.02, 0.25, 0, 100);
    assert!(cuts(&stats) > 0, "{stats:?}");
    assert!(most < 130_000, "{most}");
}

/// The default price of a rewrite (`ahead`, `margin`), over the mechanism tests' base, by epochs.
const PRICED: &str = "nudge=0;fresh=0;eoq=100000000;target=10000;ahead=0.25;margin=2.25;epoch=1;still=32000";

#[test]
fn a_long_history_of_old_results_nobody_holds_is_still_cut() {
    let (stats, most) = stale_body(0.1, 0.25, 120, 40);
    assert!(stats.get("window_cuts").is_some_and(|n| *n > 0), "{stats:?}");
    assert!(most < 40_000, "{most}");
}

#[test]
fn an_epoch_that_changed_nothing_does_not_put_the_next_look_off_by_the_whole_span() {
    // everything is held (`still`), so the epochs along the way change nothing; then the old reads may go
    let mut e = Engine::new(params(&PRICED.replace("still=32000", "still=100000000")).unwrap());
    let mut msgs = vec![json!({"role": "user", "content": "Read the parts and say what slot 50 holds."})];
    for k in 1..=60 {
        msgs.extend(step(k, &format!("cat part{k}.txt"), &big(k, 2000)));
        e.sync(&msgs);
        let _ = e.view();
    }
    assert!(e.stats.get("window_cuts").is_none(), "{:?}", e.stats);
    e.p.still = 32000;
    let (start, land) = e.window_span();
    // the view is long past where a cut starts; it comes within a few requests, not after the whole span again
    for k in 61..=64 {
        msgs.extend(step(k, &format!("cat part{k}.txt"), &big(k, 2000)));
        e.sync(&msgs);
        let _ = e.view();
    }
    assert!(e.stats.get("window_cuts").is_some_and(|n| *n > 0), "{start} {land} {:?}", e.stats);
}
