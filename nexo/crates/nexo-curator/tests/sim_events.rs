#![cfg(feature = "sim")]
//! The event engine of the session simulator (`nexo_curator::sim`).

use nexo_curator::sim::events::{run, Kind, Tool};
use nexo_curator::sim::input::load;
use nexo_curator::sim::price::Prices;

fn events(file: &str, spec: &str) -> nexo_curator::sim::events::Run {
    let path = format!("{}/tests/fixtures/sim/{file}", env!("CARGO_MANIFEST_DIR"));
    let s = load(&path).expect("the fixture loads");
    run(&s.requests, spec, Prices::LEGACY, 0.0, 0).expect("a run")
}

#[test]
fn cut_then_cited_is_one_event() {
    let r = events("cut-and-cite.jsonl", "protect=1;still=0");
    let cited: Vec<_> = r.events.iter().filter(|e| e.kind == Kind::Cited).collect();
    assert_eq!(cited.len(), 1, "events: {:?}", r.events);
    let e = cited[0];
    assert_eq!(e.tool, Tool::Read);
    assert!((e.s_e - 1200).abs() <= 60, "s_e {}", e.s_e);
    // the reply that cites comes after request 5 (the fifth request holds the idle steps)
    assert_eq!(e.t, 5);
    assert_eq!(e.words, r.forgotten);
    assert!(e.words >= 2, "two identifiers: {e:?}");
}

#[test]
fn denied_result_is_one_event() {
    let r = events("denied.jsonl", "protect=1;still=0;fresh=0");
    let denied: Vec<_> = r.events.iter().filter(|e| e.kind == Kind::Denied).collect();
    assert_eq!(denied.len(), 1, "events: {:?}", r.events);
    assert_eq!(denied[0].tool, Tool::Bash);
}

#[test]
fn slots_keep_more_prefixes_than_one() {
    let path = format!("{}/tests/fixtures/sim/cut-and-cite.jsonl", env!("CARGO_MANIFEST_DIR"));
    let s = load(&path).unwrap();
    let one = run(&s.requests, "protect=1;still=0", Prices::LEGACY, 0.0, 1).unwrap();
    let many = run(&s.requests, "protect=1;still=0", Prices::LEGACY, 0.0, 4).unwrap();
    assert_eq!(one.miss + one.hit, many.miss + many.hit);
    assert!(many.hit >= one.hit);
}
