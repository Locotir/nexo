#![cfg(feature = "sim")]
//! The family tagger (`sim::family`) against the task-name labels, and the synthetic generator.

use nexo_curator::sim::family::tag;
use nexo_curator::sim::fit::family_of_task;
use nexo_curator::sim::input::load;
use nexo_curator::sim::synth::{generate, Synth};

const RUNS: &str = "/home/lctr/curator-lab/runs";

const DBG: &[&str] = &[
    "dbg4-1105/dbg-core-plain-0.jsonl",
    "dbg4-1105/dbg-packed-plain-0.jsonl",
    "dbg4-1105/dbg-uaf-plain-0.jsonl",
    "dbg4-1105/dbg-vm-plain-0.jsonl",
    "dbgconfirm-1105/dbg-crash-cur-0.jsonl",
    "dbgconfirm-1105/dbg-crash-cur-1.jsonl",
    "dbgconfirm2-1105/dbg-corrupt-cur-0.jsonl",
    "dbgconfirm2-1105/dbg-corrupt-cur-1.jsonl",
    "dbgcorrupt-1105/dbg-corrupt-cur-0.jsonl",
    "dbgcorrupt-1105/dbg-corrupt-plain-0.jsonl",
    "dbgcrash-1105/dbg-crash-cur-0.jsonl",
    "dbgcrash-1105/dbg-crash-plain-0.jsonl",
    "dbgfix-1105/dbg-corrupt-cur-0.jsonl",
    "dbgfix-1105/dbg-corrupt-cur-1.jsonl",
    "dbgfix-1105/dbg-crash-cur-0.jsonl",
    "dbgfix-1105/dbg-crash-cur-1.jsonl",
    "dbgfix-1105/dbg-heap-cur-0.jsonl",
    "dbgfix-1105/dbg-heap-cur-1.jsonl",
    "dbgheap-1105/dbg-heap-cur-0.jsonl",
    "dbgheap-1105/dbg-heap-plain-0.jsonl",
    "dbgheap2-1105/dbg-heap-cur-0.jsonl",
    "dbgheap2-1105/dbg-heap-plain-0.jsonl",
    "dbgplain-1105/dbg-crash-plain-0.jsonl",
];

const ASM: &[&str] = &[
    "base-nodir/canonstep_asm-humaneval-composite-v1-nodir-0.jsonl",
    "base2/canonstep_asm-humaneval-composite-v1-plain-2.jsonl",
    "c56/canonstep_asm-humaneval-composite-v1-c56-2.jsonl",
    "c70/canonstep_asm-humaneval-composite-v1-plain-0.jsonl",
    "c78/canonstep_asm-humaneval-composite-v1-lector-1.jsonl",
    "h2/canonstep_asm-humaneval-composite-v1-set-0.jsonl",
    "h42/canonstep_asm-humaneval-composite-v1-h42-0.jsonl",
    "h5/canonstep_asm-humaneval-composite-v1-lag-2.jsonl",
    "k40a/canonstep_asm-humaneval-composite-v1-c40-2.jsonl",
    "t0c/canonstep_asm-humaneval-composite-v1-plain-0.jsonl",
];

const LOGS: &[&str] = &[
    "base-153ab70-20261108/long-haul-curator-0.jsonl",
    "base-153ab70-20261108/long-haul-plain-1.jsonl",
    "base-153ab70-20261108/marathon-plain-0.jsonl",
    "compact-long-1104b/marathon-compact-0.jsonl",
    "compact-long-1104c/marathon-nexo-0.jsonl",
    "p1/long-haul-sin-0.jsonl",
    "p1/marathon-sin-0.jsonl",
    "p2b/long-haul-sin-0.jsonl",
    "p3/long-haul-curador-1.jsonl",
    "pockets-1104/marathon-heavy-curator-0.jsonl",
];

const CODE: &[&str] = &[
    "p1/selective-revert-compact-0.jsonl",
    "p1/selective-revert-sin-0.jsonl",
    "p2a/selective-revert-curador-1.jsonl",
    "p2a/selective-revert-sin-0.jsonl",
    "p2a/selective-revert-sin-2.jsonl",
    "vulnhunt-560206d-20261108/vuln-hunt-curator-1.jsonl",
    "vulnhunt-560206d-20261108/vuln-hunt-plain-1.jsonl",
    "vulnhunt-k4-4b4f255-20261108/vuln-hunt-curator-1.jsonl",
    "vulnhunt-k4-4b4f255-20261108/vuln-hunt-curator-3.jsonl",
    "vulnhunt-k4-4b4f255-20261108/vuln-hunt-plain-1.jsonl",
];

fn label(path: &str) -> &'static str {
    let stem = path.rsplit('/').next().unwrap().trim_end_matches(".jsonl");
    family_of_task(stem).unwrap_or_else(|| panic!("no label for {stem}"))
}

#[test]
fn tagger_matches_task_labels() {
    let mut agree = 0;
    let mut all = 0;
    let mut wrong = Vec::new();
    for set in [DBG, ASM, LOGS, CODE] {
        for p in set {
            let s = load(&format!("{RUNS}/{p}")).unwrap_or_else(|e| panic!("{e}"));
            let (got, want) = (tag(&s.requests), label(p));
            all += 1;
            if got == want {
                agree += 1;
            } else {
                wrong.push(format!("{p}: tagged {got}, label {want}"));
            }
        }
    }
    assert_eq!(all, 23 + 10 + 10 + 10);
    assert!(agree * 100 >= all * 95, "{agree}/{all} agree; wrong: {wrong:#?}");

    // a generated session loads, with the requests and dependencies asked for
    let mut p = Synth::new(1, 60, "binary");
    p.recite = 0.5;
    let g = generate(&p);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.synth.jsonl");
    std::fs::write(&path, &g.jsonl).unwrap();
    let s = load(path.to_str().unwrap()).unwrap();
    assert_eq!(s.requests.len(), 60);
    assert!(g.deps.len() >= 10, "{} dependencies", g.deps.len());
    assert_eq!(tag(&s.requests), "binary");
}

#[test]
fn every_family_is_generated_and_tagged() {
    for f in ["binary", "asm", "http", "logs", "code", "chat"] {
        let mut p = Synth::new(3, 40, f);
        p.hex_share = 0.6;
        let g = generate(&p);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.synth.jsonl");
        std::fs::write(&path, &g.jsonl).unwrap();
        let s = load(path.to_str().unwrap()).unwrap();
        assert_eq!(s.requests.len(), 40, "{f}");
        assert_eq!(tag(&s.requests), f, "{f}");
    }
}

#[test]
fn fit_and_report_refuse_synthetic_files() {
    use nexo_curator::sim::fit::load_rows;
    assert!(load_rows(&["x/a.synth.jsonl".to_string()]).is_err());
    assert!(nexo_curator::sim::synth::refuse("x/a.synth.jsonl").is_err());
    assert!(nexo_curator::sim::synth::refuse("x/results.jsonl").is_ok());
}

/// 400 steps, 40 files, 60 % hex (about 8 MB): no panic, under 60 s, and as many cited events as planted
/// dependencies whose chunk the curator cut.
#[test]
fn stress_cited_events_are_the_planted_cut_dependencies() {
    use nexo_curator::sim::events::{run, Kind};
    use nexo_curator::sim::price::Prices;
    use nexo_curator::sim::synth::write;
    let mut p = Synth::new(7, 400, "binary");
    p.files = 40;
    p.hex_share = 0.6;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("big.synth.jsonl");
    let g = write(&p, out.to_str().unwrap()).unwrap();
    assert!(dir.path().join("big.synth.deps.json").is_file());
    assert!(g.jsonl.len() > 5_000_000, "{} bytes", g.jsonl.len());
    let s = load(out.to_str().unwrap()).unwrap();
    assert_eq!(s.requests.len(), 400);
    let spec = "directives=0;nudge=0";
    let t0 = std::time::Instant::now();
    let r = run(&s.requests, spec, Prices::LEGACY, 0.0, 0).unwrap();
    let secs = t0.elapsed().as_secs_f64();
    assert!(secs < 60.0, "{secs} s");
    // which planted chunks the curator cut, from the views themselves
    let mut cur = nexo_curator::Curator::new(spec).unwrap();
    cur.window(0);
    let mut cut = 0;
    for (i, q) in s.requests.iter().enumerate() {
        let shown = cur.view(q.system_tokens, &q.messages, q.now).unwrap();
        let text: String = shown.view.iter().map(nexo_curator::sim::input::text_of).collect::<Vec<_>>().join("\n");
        cut += g.deps.iter().filter(|d| d.cite_request == i + 1 && !text.contains(&d.word)).count();
        cur.cached(0, 1);
    }
    let cited = r.events.iter().filter(|e| e.kind == Kind::Cited).count();
    eprintln!("{secs:.1} s, {} deps, {cut} cut, {cited} cited events", g.deps.len());
    assert_eq!(cited, cut, "cited events vs planted dependencies cut");
}
