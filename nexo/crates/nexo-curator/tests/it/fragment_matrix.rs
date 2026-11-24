//! Deterministic curator-vs-curator ablation, not an LLM quality/cost benchmark.
//! Exact labelled clauses measure availability, not whether a model uses or understands them.
use nexo_curator::Curator;
use serde_json::{json, Value};

const CURRENT: &str = "Correction: atlas metrics port is 9417, not 8080.";
const OLD: &str = "Atlas metrics port is 8080.";
const SHELL: &str = "Atlas shell config is dots/shell/rc.zsh.";
const OTHER: &str = "Boreal metrics port is 8080.";

fn user(text: &str) -> Value {
    json!({"role":"user", "content":text})
}
fn contains(view: &[Value], clause: &str) -> bool {
    view.iter().any(|m| nexo_curator::sim::input::text_of(m).contains(clause))
}

#[derive(Debug, PartialEq)]
struct Measurement {
    // Kept separately: seeing a current clause does not prove stale claims are absent.
    current: bool,
    stale: bool,
    shell: bool,
    other_scope: bool,
    estimated_tokens: i64,
    removed: usize,
}

fn replay(spec: &str, distance: usize, noise: usize, tool_facts: bool) -> Measurement {
    let mut curator = Curator::new(spec).unwrap();
    curator.window(4_000);
    let mut history = vec![
        user("Maintain atlas and boreal; preserve service-specific settings."),
        user(OLD),
        user(SHELL),
        user(OTHER),
        user(CURRENT),
    ];
    if tool_facts {
        history.truncate(1);
        for (i, text) in [format!("{OLD}\n{SHELL}\n{OTHER}"), CURRENT.into()].iter().enumerate() {
            let id = format!("facts_{i}");
            history.push(json!({"role":"assistant", "content":[{"type":"tool_use", "id":id, "name":"read", "input":{"path":"docs/settings.md"}}]}));
            history.push(json!({"role":"user", "content":[{"type":"tool_result", "tool_use_id":id, "content":text}]}));
            curator.view(200, &history, i as f64).unwrap();
        }
    }
    curator.view(200, &history, 2.0).unwrap();
    let mut removed = 0;
    for i in 0..distance {
        let id = format!("noise_{i}");
        let text: String = (0..noise)
            .map(|j| format!("logs/component_{i}/row_{j}.txt: unrelated diagnostic item_{i}_{j}\n"))
            .collect();
        history.push(json!({"role":"assistant", "content":[{"type":"tool_use", "id":id, "name":"read", "input":{"path":format!("logs/component_{i}")}}]}));
        history.push(json!({"role":"user", "content":[{"type":"tool_result", "tool_use_id":id, "content":text}]}));
        removed += curator.view(200, &history, (i + 3) as f64).unwrap().removed;
    }
    history.push(user("Return atlas metrics port and shell config, and boreal metrics port. Apply the correction."));
    let result = curator.view(200, &history, (distance + 3) as f64).unwrap();
    Measurement {
        current: contains(&result.view, CURRENT),
        stale: contains(&result.view, OLD),
        shell: contains(&result.view, SHELL),
        other_scope: contains(&result.view, OTHER),
        estimated_tokens: result.tokens,
        removed: removed + result.removed,
    }
}

#[test]
fn fragment_matrix_curator_variants_are_repeatable_and_expose_stale_claims() {
    // Same engine/fixtures/window; only cache price assumption changes. Neither arm is plain.
    // Repeating ensures determinism, not statistical confidence or independent samples.
    for spec in ["", "ratio=1"] {
        for distance in [0, 8, 32] {
            for noise in [1, 48] {
                let first = replay(spec, distance, noise, false);
                assert_eq!(first, replay(spec, distance, noise, false), "{spec}/{distance}/{noise}");
                assert!(
                    first.current && first.shell && first.other_scope,
                    "labelled fragment regression {spec}/{distance}/{noise}: {first:?}"
                );
                if distance == 32 && noise == 48 {
                    assert!(first.removed > 0, "fixture must exercise removal, not just an untouched history");
                }
                println!("curator spec={spec:?} distance={distance} noise={noise}: {first:?}");
            }
        }
    }
}

#[test]
fn fragment_matrix_tool_revisions_report_each_fragment_separately() {
    // Fixture-specific retention gate, not a universal guarantee for tool observations.
    // Exact clauses certify availability only, not conflict resolution or model use.
    for spec in ["", "ratio=1"] {
        for distance in [0, 8, 32] {
            let result = replay(spec, distance, 48, true);
            assert_eq!(result, replay(spec, distance, 48, true));
            assert!(result.current && result.shell && result.other_scope, "tool fragments lost: {result:?}");
            if distance == 32 {
                assert!(result.removed > 0, "must exercise compaction");
            }
            println!("curator tool-revision spec={spec:?} distance={distance}: {result:?}");
        }
    }
}

#[test]
fn fragment_oracle_does_not_conflate_presence_with_conflict_resolution() {
    let both = [user(CURRENT), user(OLD)];
    assert!(contains(&both, CURRENT) && contains(&both, OLD));
    assert!(!contains(&[user("Atlas metrics port is 9417.")], CURRENT), "paraphrases are not certified");
    assert!(!contains(&[user(OTHER)], OLD), "other scope is not a contradiction");
    assert!(!contains(&[user("Correction: atlas metrics port is 8080, not 9417.")], CURRENT));
    assert!(!contains(&[], CURRENT));
    // Ablate each labelled fragment independently; other labels must not mask its loss.
    let clauses = [CURRENT, OLD, SHELL, OTHER];
    for missing in 0..clauses.len() {
        let mutated: Vec<_> = clauses.iter().enumerate().filter(|(i, _)| *i != missing).map(|(_, c)| user(c)).collect();
        for (i, clause) in clauses.iter().enumerate() {
            assert_eq!(contains(&mutated, clause), i != missing, "ablation {missing}, label {i}");
        }
    }
}
