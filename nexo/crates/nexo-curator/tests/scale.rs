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

use scale_gen::{cold_facts, fixed_fact, holds, run_cold, Cold, ColdParams, STAND_REPS};
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

#[test]
fn cold_tokens_sizes_the_session_and_reports_hops_apart() {
    let p = ColdParams::for_tokens(2_000_000, 0, 1);
    let mut small = ColdParams::new(300, 0, 1);
    small.questions = 9;
    small.stride = 3;
    let row = run_cold(&small);
    // questions take 1, 2, 3 facts in turn: three of one fact, six of two or three
    assert_eq!((row.by_hops[1], row.by_hops[3]), (3, 6));
    assert!(row.tokens > 50_000 && row.stopped.is_none());
    assert!(p.topics > 200 && ColdParams::for_tokens(4_000_000, 0, 1).topics > p.topics);
    let line = row.line_tokens();
    assert!(line.contains(" tokens | 300 topics | "), "{line}");
    // a limit stops the session and says why
    let mut cut = ColdParams::new(3000, 0, 1);
    cut.max_secs = 0.001;
    let row = run_cold(&cut);
    assert!(row.stopped.as_deref().is_some_and(|s| s.starts_with("tiempo")), "{:?}", row.stopped);
    assert!(row.line_tokens().contains("PARADO"));
}

// Standing facts: facts that override a default (a shell rc that is not ~/.bashrc, a database on another port), said
// once early; the tasks that come later suppose the default and never name them.

#[test]
fn standing_generator_plants_each_fact_once_and_asks_after_it() {
    let mut p = ColdParams::new(120, 1, 3);
    p.standing = 8;
    p.questions = 4;
    let mut g = Cold::new(p.clone());
    let (mut history, mut tasks, mut seen) = (String::new(), 0, Vec::new());
    while let Some(b) = g.next_block() {
        if let Some(lines) = &b.stand {
            tasks += 1;
            assert_eq!(lines.len(), 1);
            assert!(history.contains(lines[0].as_str()), "the fact comes before the task that supposes it: {lines:?}");
            let task = serde_json::Value::Array(b.msgs.clone()).to_string();
            assert!(!task.contains(lines[0].as_str()), "the task does not name the fact");
            seen.push(lines[0].clone());
        }
        history += &serde_json::Value::Array(b.msgs).to_string().replace("\\n", "\n");
    }
    assert_eq!(tasks, 8 * STAND_REPS);
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 8, "eight different facts");
    // the three ways a fact comes into the history are all used
    assert!(history.contains("Finding: the database (postgres) listens on port"));
    assert!(history.contains("shell_rc = /etc/skel-"));
    assert!(history.contains("Don't touch /srv/legacy"));
    // 1 opening, the topics, the tasks, four per question
    assert_eq!(session(&p).len(), 1 + 120 + tasks + 4 * 4);
    assert_eq!(session(&p), session(&p));
    assert_ne!(fixed_fact(1, 0).line, fixed_fact(2, 0).line);
}

#[test]
fn standing_facts_reach_the_view_at_200_topics() {
    let mut p = ColdParams::new(200, 1, 1);
    p.standing = 8;
    p.questions = 4;
    let row = run_cold(&p);
    assert_eq!(row.stand[1], 8 * STAND_REPS);
    assert_eq!(row.stand[0], row.stand[1], "{}", row.line());
    assert!(row.line().contains(" | fijos 100.0 | "), "{}", row.line());
}
