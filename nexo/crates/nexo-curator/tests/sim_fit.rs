#![cfg(feature = "sim")]
//! Fitting the behaviour parameters (`nexo_curator::sim::fit`) on runs built from a golden fixture.

use nexo_curator::sim::fit::{fit_dirs, FitConfig, Holdout};
use nexo_curator::sim::input::load;
use serde_json::{json, Value};

const SPEC: &str = "protect=1;still=0;epoch=0";

fn lines(msgs: &[Value], lens: &[usize]) -> String {
    let mut s = String::new();
    for &n in lens {
        s += &json!({"messages": &msgs[..n], "system": "sys", "usage": {"output": 40}}).to_string();
        s.push('\n');
    }
    s
}

/// 40 pairs of the cut-and-cite session; in `planted` of them the agent reads the cut file again.
fn build(dir: &std::path::Path, pairs: usize, planted: impl Fn(usize) -> bool) {
    let path = format!("{}/tests/fixtures/sim/cut-and-cite.jsonl", env!("CARGO_MANIFEST_DIR"));
    let all = load(&path).expect("the fixture loads").requests;
    let plain: Vec<Value> = all.last().unwrap().messages.clone();
    assert_eq!(plain.len(), 12, "the fixture session has 12 messages");
    let lens: Vec<usize> = all.iter().map(|q| q.messages.len()).collect();
    let lens_re: Vec<usize> = lens.iter().map(|&n| if n > 10 { n + 2 } else { n }).collect();
    let mut cut = plain.clone();
    cut[2]["content"][0]["content"] = json!("[curator] 40 lines of lib/qbuf.c are left out here");
    let reread = [
        json!({"role":"assistant","content":[{"type":"tool_use","id":"r2","name":"read","input":{"path":"lib/qbuf.c"}}]}),
        json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"r2","content":"static int ring_slot_001_step(void);"}]}),
    ];
    let mut again = cut.clone();
    again.splice(10..10, reread);
    let mut rows = String::new();
    for i in 0..pairs {
        let hit = planted(i);
        std::fs::write(dir.join(format!("p{i}.jsonl")), lines(&plain, &lens)).unwrap();
        std::fs::write(dir.join(format!("c{i}.jsonl")), if hit { lines(&again, &lens_re) } else { lines(&cut, &lens) })
            .unwrap();
        let row = |arm: &str, spec: &str, steps: usize, tr: String| {
            json!({"task":"marathon-fake","arm":arm,"policy":arm,"spec":spec,"rep":i,"model":"deepseek-flash",
                "commit":"abc123","max_steps":40,"score":1.0,"stop":null,"steps":steps,"input":1000,
                "cache_read":9000,"output":400,"cost":0.001,"rewrites":1,"transcript":tr,
                "counters":{"recall_calls":0}})
            .to_string()
        };
        rows += &row("plain", "", 6, format!("p{i}.jsonl"));
        rows.push('\n');
        rows += &row("curator", SPEC, if hit { 7 } else { 6 }, format!("c{i}.jsonl"));
        rows.push('\n');
    }
    std::fs::write(dir.join("results.jsonl"), rows).unwrap();
}

#[test]
fn fit_recovers_planted_rate() {
    let tmp = tempfile::tempdir().unwrap();
    build(tmp.path(), 40, |i| i % 4 == 0);
    let cfg = FitConfig { holdout: Holdout::None, ..FitConfig::default() };
    let fit = fit_dirs(&[tmp.path().to_string_lossy().to_string()], &cfg).expect("a fit");
    let g = &fit.global;
    assert_eq!(g.events, 40, "one cited event per plain twin: {g:?}");
    assert!((g.r.value - 0.25).abs() <= 0.05, "r {:?}", g.r);
    assert!(g.r.lo <= 0.25 && 0.25 <= g.r.hi, "ci {:?}", g.r);
    let logs = &fit.families["logs"];
    assert!((logs.r.value - 0.25).abs() <= 0.05, "family r {:?}", logs.r);
}

#[test]
fn zero_fetches_get_the_rule_of_three() {
    let tmp = tempfile::tempdir().unwrap();
    build(tmp.path(), 40, |_| false);
    let fit = fit_dirs(&[tmp.path().to_string_lossy().to_string()], &FitConfig::default()).expect("a fit");
    let r = fit.global.r;
    assert!(r.rule3 && r.value == 0.0 && (r.hi - 3.0 / 40.0).abs() < 1e-9, "{r:?}");
}

#[test]
fn holdout_splits_by_rep_parity() {
    let tmp = tempfile::tempdir().unwrap();
    build(tmp.path(), 40, |i| i % 4 == 0);
    let d = [tmp.path().to_string_lossy().to_string()];
    let even = fit_dirs(&d, &FitConfig { holdout: Holdout::Even, ..FitConfig::default() }).unwrap();
    assert_eq!(even.obs.len(), 20);
    assert!(even.obs.iter().all(|o| o.eff_rep % 2 == 1));
    assert_eq!(even.held_out, 20);
}

/// A bench transcript with no cut in it, shaped like the `selective-revert` task: a long system prompt and
/// `n` steps, each adding a reply the model wrote and a tool result.
fn uncut_session(n: usize) -> Vec<nexo_curator::sim::input::Request> {
    use nexo_curator::sim::input::Request;
    let mut msgs = vec![json!({"role": "user", "content": "Change shipping() in src/lib.rs."})];
    let mut out = Vec::new();
    for i in 0..n {
        out.push(Request {
            now: 1_750_000_000.0 + 2.0 * i as f64,
            system_tokens: 1600,
            system: String::new(),
            messages: msgs.clone(),
            tools: None,
            out_tokens: Some(150),
        });
        let id = format!("t{i}");
        msgs.push(json!({"role": "assistant", "content": [
            {"type": "text", "text": format!("Reading part {i}. {}", "plain words ".repeat(40))},
            {"type": "tool_use", "id": id, "name": "read", "input": {"path": format!("src/part{i}.rs")}}]}));
        msgs.push(json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": format!("part{i} {}", "fn body() {} ".repeat(60))}]}));
    }
    out
}

/// A session the curator leaves alone (the `selective-revert` case: no events) costs what the plain arm
/// costs, whatever the cache keeps: the price of a view must not depend on which arm it is asked for.
#[test]
fn an_uncut_curator_arm_costs_what_the_plain_arm_costs() {
    use nexo_curator::sim::events::run;
    use nexo_curator::sim::fit::plain_tokens;
    use nexo_curator::sim::price::Prices;
    let reqs = uncut_session(15);
    for (life, slots) in [(0.0, 0), (1.0, 0), (3600.0, 0), (0.0, 3)] {
        let (miss, hit, out) = plain_tokens(&reqs, life, slots);
        let r = run(&reqs, "directives=0", Prices::LIVE, life, slots).unwrap();
        assert_eq!((r.miss, r.hit, r.out), (miss, hit, out), "life {life} slots {slots}");
        assert_eq!(r.events.len(), 0);
        let plain = Prices::LIVE.dollars(miss, hit) + Prices::LIVE.output(out);
        assert!((r.cost / plain - 1.0).abs() < 0.03, "dCost {:+.1} %", 100.0 * (r.cost / plain - 1.0));
    }
}
