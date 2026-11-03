//! Slope guard: the curator's time per request must not grow with the history. Ignored by default (slow, wants
//! a quiet machine): `cargo test --release -p nexo-curator --test scale -- --ignored --nocapture`.
//! `SCALE_RUNS` sets the number of runs per shape (median taken; default 5).

#[path = "support/scale_gen.rs"]
mod scale_gen;

use scale_gen::{median, run, Plan};

fn slope(kind: &str) {
    let runs: usize = std::env::var("SCALE_RUNS").ok().and_then(|s| s.parse().ok()).unwrap_or(5);
    let plan = Plan { checkpoints: vec![1000, 5000], tail: 40, stride: 5, dense_until: 0, ingest: false };
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for _ in 0..runs {
        let rows = run(kind, &plan);
        a.push(rows[0].median_ms);
        b.push(rows[1].median_ms);
    }
    let (m1, m5) = (median(&a), median(&b));
    eprintln!(
        "scale {kind}: 1k {m1:.3} ms (runs {a:.2?}), 5k {m5:.3} ms (runs {b:.2?}), limit {:.3} ms",
        1.5 * m1 + 1.0
    );
    assert!(m5 <= 1.5 * m1 + 1.0, "{kind}: {m5:.3} ms at 5k against {m1:.3} ms at 1k");
}

#[test]
#[ignore]
fn flat_time_e8() {
    slope("e8");
}

#[test]
#[ignore]
fn flat_time_text_replies() {
    slope("a");
}

#[test]
#[ignore]
fn flat_time_returning_topics() {
    slope("b");
}

#[test]
#[ignore]
fn flat_time_chat_without_tools() {
    slope("c");
}
