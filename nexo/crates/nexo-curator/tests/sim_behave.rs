#![cfg(feature = "sim")]
//! The behaviour layer of the session simulator (`nexo_curator::sim::behave`).

use nexo_curator::sim::behave::{analytic, monte_carlo, probe, Context, Params};
use nexo_curator::sim::events::{run, Run};
use nexo_curator::sim::input::{load, Session};
use nexo_curator::sim::price::Prices;
use nexo_curator::D_SPEC;

const SPEC: &str = "protect=1;still=0;target=0;cap=0;hard=0";

/// The default spec without the working window these fixtures were measured with: the simulator's events and
/// their cost shape depend on how much a cut leaves, and the tests here probe the simulator, not the window.
fn dspec() -> String {
    format!("{D_SPEC};target=0;cap=0;hard=0")
}

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

fn real(file: &str) -> Session {
    load(&format!("{}/tests/fixtures/{file}", env!("CARGO_MANIFEST_DIR"))).expect("the fixture loads")
}

fn probed(s: &Session) -> (Run, Context) {
    let mut open = run(&s.requests, &dspec(), Prices::LIVE, 0.0, 0).expect("a run");
    probe(&mut open, &s.requests, &dspec(), 0.0, 0).expect("a probe");
    let c = Context::measure(&s.requests, &dspec(), Prices::LIVE, 40);
    (open, c)
}

fn within(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol * b.abs().max(1e-12)
}

/// The cost of a fetch is more than the tokens of the event: the curator cuts the new result and drops
/// copies, so the estimate from the event alone is off by tens of percent, and the replayed footprint
/// (the fetch request, the requests after it, the re-cuts) agrees with the Monte Carlo.
#[test]
fn the_probed_footprint_agrees_with_the_monte_carlo_where_the_estimate_does_not() {
    let p = params(1.0, 0.0, 0.0);
    let mut worst_estimate: f64 = 0.0;
    for file in ["bench-long-D-0.jsonl", "bench-marathon-D.jsonl", "bench-dir-Ddir-4.jsonl"] {
        let s = real(file);
        let (open, c) = probed(&s);
        let sites = open.events.iter().filter(|e| matches!(e.kind.name(), "cited" | "denied")).count();
        assert!(sites >= 1 && open.feet.len() == sites, "{file}: {sites} events, {} feet", open.feet.len());
        let mut estimate = open.clone();
        estimate.feet.clear();
        let (a, e) = (analytic(&open, p, 1.0, &c), analytic(&estimate, p, 1.0, &c));
        let m = monte_carlo(&s.requests, &dspec(), &c, 0.0, 0, p, 1.0, 4, 1).expect("mc");
        assert!(within(a.extra_cost, m.extra_cost, 0.05), "{file}: probed {} vs mc {}", a.extra_cost, m.extra_cost);
        assert!(within(a.extra_steps, m.extra_steps, 0.05), "{file}: {} vs {}", a.extra_steps, m.extra_steps);
        worst_estimate = worst_estimate.max((e.extra_cost - m.extra_cost).abs() / m.extra_cost);
        // what is fetched stays input: the fetches cost all of it, the output of the calls included
        assert!(within(a.fetch_cost, a.extra_cost, 1e-9), "{a:?}");
    }
    assert!(worst_estimate > 0.15, "the estimate from the event alone was off by {worst_estimate}");
}

/// A session where the agent asks again for the same result twice: together the fetches cost less than the
/// sum of each alone, and the replay says by how much.
#[test]
fn fetches_that_share_content_cost_less_together_than_alone() {
    let s = real("synthetic-g3-t8-s1.jsonl");
    let (open, _) = probed(&s);
    let tokens = |a: (i64, i64)| a.0 + a.1;
    let alone: i64 = open.feet.iter().map(|f| tokens(f.alone)).sum();
    let joint: i64 = open.feet.iter().map(|f| tokens(f.joint)).sum();
    assert!(joint < alone, "alone {alone}, joint {joint}");
}

/// At a fraction of the fetches the analytic cost is still the Monte Carlo's where events do not feed back
/// on each other (one fetch site), and the weights of alone and joint are the two ends.
#[test]
fn a_fraction_of_the_fetches_costs_that_fraction() {
    for file in ["bench-long-D-0.jsonl", "bench-dir-Ddir-4.jsonl"] {
        let s = real(file);
        let (open, c) = probed(&s);
        let p = params(0.3, 0.0, 0.0);
        let a = analytic(&open, p, 0.3, &c);
        let m = monte_carlo(&s.requests, &dspec(), &c, 0.0, 0, p, 0.3, 100, 1).expect("mc");
        assert!(within(a.extra_cost, m.extra_cost, 0.05), "{file}: {} vs {}", a.extra_cost, m.extra_cost);
        assert!(within(a.extra_steps, m.extra_steps, 0.05), "{file}: {} vs {}", a.extra_steps, m.extra_steps);
    }
}

/// Two events where fetching the first puts back what the second would have cut: the second adds no step
/// when the first is fetched, and all of its weight when nothing is.
#[test]
fn an_event_that_a_fetch_removes_adds_no_step_when_that_fetch_is_made() {
    use nexo_curator::sim::events::Foot;
    let s = session("cut-and-cite.jsonl");
    let mut open = open_loop(&s);
    let e = open.events.iter().find(|e| e.kind.name() == "cited").cloned().expect("one cited event");
    open.events = vec![e.clone(), e];
    let foot = Foot { alone: (1000, 500), joint: (1000, 500), gone: false };
    open.feet = vec![foot, Foot { gone: true, ..foot }];
    let c = cx(40);
    // everything fetched: the second event is not met
    let all = analytic(&open, params(1.0, 0.0, 0.0), 1.0, &c);
    assert!((all.extra_steps - 1.0).abs() < 1e-12, "{all:?}");
    // nothing fetched: the open loop, both events wander
    let none = analytic(&open, params(0.0, 0.5, 0.0), 0.0, &c);
    assert!((none.extra_steps - 1.0).abs() < 1e-12, "{none:?}");
    // half fetched: the second is met half of the time
    let half = analytic(&open, params(0.5, 0.0, 0.0), 0.5, &c);
    assert!((half.extra_steps - (0.5 + 0.5 * 0.5)).abs() < 1e-12, "{half:?}");
    // the failure chance sees it too
    let blind = analytic(&open, params(0.0, 0.0, 0.2), 0.0, &c);
    assert!((blind.p_fail - (1.0 - 0.8 * 0.8)).abs() < 1e-12, "{blind:?}");
}

/// In a real session some later events never happen once the earlier ones are fetched (the closed loop
/// meets fewer events than the open loop), and the analytic steps are the Monte Carlo's: `probe` marks them.
#[test]
fn probe_marks_the_events_a_fetch_removes_and_the_steps_agree_with_the_monte_carlo() {
    let s = real("synthetic-g3-t8-s1.jsonl");
    let (open, c) = probed(&s);
    let p = params(1.0, 0.0, 0.0);
    let m = monte_carlo(&s.requests, &dspec(), &c, 0.0, 0, p, 1.0, 4, 1).expect("mc");
    let gone = open.feet.iter().filter(|f| f.gone).count();
    assert!(gone > 0, "no event was marked as removed: {} events", open.feet.len());
    assert!(!open.feet[0].gone, "the first event always happens");
    assert_eq!(open.feet.len() - gone, m.events as usize, "events met: probe vs the Monte Carlo");
    let a = analytic(&open, p, 1.0, &c);
    assert!((a.extra_steps - m.extra_steps).abs() < 1e-9, "{} vs {}", a.extra_steps, m.extra_steps);
    // without any fetch the open loop is what happens, removed events included
    let none = analytic(&open, params(0.0, 1.0, 0.0), 0.0, &c);
    assert!((none.extra_steps - open.feet.len() as f64 * params(0.0, 1.0, 0.0).beta_eff()).abs() < 1e-9, "{none:?}");
}
