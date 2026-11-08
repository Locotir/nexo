#![cfg(feature = "sim")]
//! The behaviour layer of the session simulator (`nexo_curator::sim::behave`).

use nexo_curator::sim::behave::{analytic, monte_carlo, Context, Params};
use nexo_curator::sim::events::{run, Run};
use nexo_curator::sim::input::{load, Session};
use nexo_curator::sim::price::Prices;

const SPEC: &str = "protect=1;still=0";

fn session(file: &str) -> Session {
    load(&format!("{}/tests/fixtures/sim/{file}", env!("CARGO_MANIFEST_DIR"))).expect("the fixture loads")
}

fn open_loop(s: &Session) -> Run {
    run(&s.requests, SPEC, Prices::LIVE, 0.0, 0).expect("a run")
}

fn cx(max_steps: usize) -> Context {
    Context { prices: Prices::LIVE, protect: 1, c_out: 100.0, max_steps }
}

fn params(r: f64, beta: f64, phi: f64) -> Params {
    Params { r, beta, phi, pi: 0.0, mu: 2.0 }
}

#[test]
fn fetch_adds_one_step_and_its_tokens() {
    let s = session("cut-and-cite.jsonl");
    let open = open_loop(&s);
    let cited: Vec<_> = open.events.iter().filter(|e| e.kind.name() == "cited").collect();
    assert_eq!(cited.len(), 1, "events: {:?}", open.events);
    let e = cited[0];
    let p = Prices::LIVE;
    let want = (p.hit * open.views[e.t - 1] as f64
        + p.miss * (100.0 + e.s_e as f64)
        + p.out * 100.0
        + p.hit * e.s_e as f64 * (1usize).min(e.s_rest) as f64)
        / 1e6;

    let a = analytic(&open, params(1.0, 0.0, 0.0), 1.0, &cx(40));
    assert!((a.extra_steps - 1.0).abs() < 1e-12, "{a:?}");
    assert!((a.extra_cost - want).abs() < 1e-12, "{} vs {want}", a.extra_cost);
    assert_eq!(a.p_fail, 0.0);

    let z = analytic(&open, params(0.0, 0.0, 0.0), 0.0, &cx(40));
    assert_eq!((z.extra_steps, z.extra_cost, z.p_fail), (0.0, 0.0, 0.0));
    let plain = run(&s.requests, SPEC, Prices::LIVE, 0.0, 0).unwrap();
    assert_eq!(plain.cost, open.cost, "cost with r = 0 is the open-loop cost");
}

#[test]
fn cap_fails_a_run_that_fetches_past_the_limit() {
    let s = session("cap.jsonl");
    assert_eq!(s.requests.len(), 38);
    let open = open_loop(&s);
    let cited = open.events.iter().filter(|e| e.kind.name() == "cited").count();
    assert_eq!(cited, 3, "events: {:?}", open.events);
    let a = analytic(&open, params(1.0, 0.0, 0.0), 1.0, &cx(40));
    assert!((a.extra_steps - 3.0).abs() < 1e-12, "{a:?}");
    assert!(a.p_fail >= 0.95, "{a:?}");
    // with room to spare it does not fail
    let b = analytic(&open, params(1.0, 0.0, 0.0), 1.0, &cx(60));
    assert_eq!(b.p_fail, 0.0, "{b:?}");
}

#[test]
fn blind_failure_is_one_minus_r_times_phi() {
    let s = session("cut-and-cite.jsonl");
    let open = open_loop(&s);
    let a = analytic(&open, params(0.5, 0.0, 0.2), 0.5, &cx(40));
    assert!((a.p_fail - 0.1).abs() < 1e-12, "{a:?}");
}

#[test]
fn monte_carlo_with_r_one_inserts_the_pair_and_agrees() {
    let s = session("cut-and-cite.jsonl");
    let open = open_loop(&s);
    let c = cx(40);
    let p = params(1.0, 0.0, 0.0);
    let a = analytic(&open, p, 1.0, &c);
    let m = monte_carlo(&s.requests, SPEC, &c, 0.0, 0, p, 1.0, 8, 1).expect("mc");
    assert!((m.extra_steps - 1.0).abs() < 1e-12, "{m:?}");
    assert_eq!(m.p_fail, 0.0);
    // the same tokens, up to the pair the MC really inserts (the result block against the whole message)
    assert!((m.extra_cost - a.extra_cost).abs() <= 0.15 * a.extra_cost, "{} vs {}", m.extra_cost, a.extra_cost);
    let z = monte_carlo(&s.requests, SPEC, &c, 0.0, 0, params(0.0, 0.0, 0.0), 0.0, 4, 1).unwrap();
    assert_eq!((z.extra_steps, z.extra_cost), (0.0, 0.0));
}
