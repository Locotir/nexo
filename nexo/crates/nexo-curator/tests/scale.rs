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

// The cold scale test (many topics, big pastes, cross-topic questions): small N only, the large ones run
// from `examples/scale.rs --topics N`.

use scale_gen::{cold_facts, holds, run_cold, Cold, ColdParams};
use serde_json::json;

fn session(p: &ColdParams) -> Vec<String> {
    let mut g = Cold::new(p.clone());
    let mut out = Vec::new();
    while let Some(b) = g.next_block() {
        out.push(format!("{:?} {}", b.ask, serde_json::Value::Array(b.msgs)));
    }
    out
}

#[test]
fn cold_generator_is_deterministic() {
    let p = ColdParams::new(60, 2, 7);
    assert_eq!(session(&p), session(&p));
    assert_ne!(session(&p), session(&ColdParams::new(60, 2, 8)));
    assert_eq!(cold_facts(7, 3), cold_facts(7, 3));
    assert_ne!(cold_facts(7, 3), cold_facts(7, 4));
    // one block for the opening, one per topic, four per question
    assert_eq!(session(&p).len(), 1 + 60 + 30 * 4);
}

#[test]
fn cold_questions_name_topics_whose_lines_are_in_the_history() {
    for hops in 1..=3 {
        let mut p = ColdParams::new(50, hops, 3);
        p.questions = 8;
        let mut g = Cold::new(p);
        let (mut history, mut asked) = (String::new(), 0);
        while let Some(b) = g.next_block() {
            if let Some(lines) = &b.ask {
                asked += 1;
                assert_eq!(lines.len(), hops);
                assert!(lines.iter().all(|l| history.contains(l.as_str())), "a source line is not in the history");
            }
            history += &serde_json::Value::Array(b.msgs).to_string().replace("\\n", "\n");
        }
        assert_eq!(asked, 8);
    }
}

#[test]
fn oracle_marks_a_view_without_the_source_lines_as_a_miss() {
    let lines = cold_facts(1, 5)[..2].to_vec();
    let without = vec![json!({"role": "user", "content": format!("topic 5\n{}", lines[0])})];
    let with = vec![
        json!({"role": "user", "content": format!("a\n{}\nb", lines[0])}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "x", "content": format!("{}\n", lines[1])}]}),
    ];
    assert!(!holds(&[], &lines));
    assert!(!holds(&without, &lines), "one of two lines is a miss");
    assert!(holds(&with, &lines));
}

#[test]
fn cold_scale_runs_at_200_topics() {
    for hops in 1..=3 {
        let mut p = ColdParams::new(200, hops, 1);
        p.questions = 10;
        let row = run_cold(&p);
        assert_eq!(row.questions, 10);
        assert!(row.calls > 200 * 2 && row.view_max > 0 && row.sent > 0 && row.usd > 0.0);
        assert!(row.plain_max >= row.view_max, "plain sends the whole history");
        assert!(row.recall() >= 0.0 && row.recall() <= 100.0);
        let line = row.line();
        assert!(line.starts_with(&format!("200 | {hops} | ")), "{line}");
        assert_eq!(line.matches(" | ").count(), 8, "{line}");
    }
}
