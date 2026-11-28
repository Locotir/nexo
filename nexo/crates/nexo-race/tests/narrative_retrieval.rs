//! Offline evidence-availability replay, not a model answering or legal-accuracy test.
use nexo_race::Race;
use serde_json::{json, Value};
use std::collections::HashMap;

// The documents and answer key are deliberately separate: neither is inferred from curator output.
const V1: &str = "Article 7 (2023): The tenant must give 60 days notice. Article 8: Pets are allowed.";
const V2: &str = "Article 7 (2024 amendment): The tenant must give 30 days notice. Article 8 remains unchanged.";
const STORY: &str =
    "In the first chapter Mara hid the silver key under the blue bench. Later she moved it to the red box.";
const ANSWER_KEY: [(&str, &str); 4] = [
    ("old notice", "The tenant must give 60 days notice."),
    ("current notice", "The tenant must give 30 days notice."),
    ("unchanged clause", "Pets are allowed."),
    ("key history", STORY),
];

fn has(view: &[Value], evidence: &str) -> bool {
    view.iter().any(|m| nexo_race::sim::input::text_of(m).contains(evidence))
}

// Conservative: only a unique visible earlier read and its successful result establish attribution.
// Other representations (including curator memories) are not verifiable by this oracle.
fn attributed(view: &[Value], path: &str, evidence: &str) -> bool {
    let mut calls: HashMap<&str, Vec<(usize, &Value)>> = HashMap::new();
    for (i, message) in view.iter().enumerate() {
        if message["role"] != "assistant" {
            continue;
        }
        if let Some(blocks) = message["content"].as_array() {
            for block in blocks {
                if block["type"] == "tool_use" {
                    if let Some(id) = block["id"].as_str().filter(|s| !s.is_empty()) {
                        calls.entry(id).or_default().push((i, block));
                    }
                }
            }
        }
    }
    view.iter().enumerate().any(|(i, message)| {
        message["role"] == "user"
            && message["content"].as_array().is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    if block["type"] != "tool_result" || block["is_error"].as_bool().unwrap_or(false) {
                        return false;
                    }
                    let Some(id) = block["tool_use_id"].as_str().filter(|s| !s.is_empty()) else {
                        return false;
                    };
                    let Some(call) = calls.get(id).filter(|c| c.len() == 1).and_then(|c| c.first()) else {
                        return false;
                    };
                    if call.0 >= i || call.1["name"] != "read" || call.1["input"]["path"] != path {
                        return false;
                    }
                    match &block["content"] {
                        Value::String(text) => text.contains(evidence),
                        Value::Array(parts) => parts.iter().any(|part| {
                            part["type"] == "text" && part["text"].as_str().is_some_and(|text| text.contains(evidence))
                        }),
                        _ => false,
                    }
                })
            })
    })
}

fn read(history: &mut Vec<Value>, id: &str, path: &str, text: &str) {
    history
        .push(json!({"role":"assistant","content":[{"type":"tool_use","id":id,"name":"read","input":{"path":path}}]}));
    history.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":text}]}));
}

fn replay(window: u64, reverse_documents: bool) -> (Vec<bool>, usize, i64, Vec<bool>) {
    let mut curator = Race::new("").unwrap();
    curator.window(window);
    let mut history =
        vec![json!({"role":"user","content":"Review the lease and the story. Keep the versions distinct."})];
    let mut documents = [("lease/2023.txt", V1), ("lease/2024.txt", V2), ("story/chapter1.txt", STORY)];
    if reverse_documents {
        documents.reverse();
    }
    for (i, (path, text)) in documents.iter().enumerate() {
        read(&mut history, &format!("doc{i}"), path, text);
        curator.view(200, &history, i as f64).unwrap();
    }
    let mut removed = 0;
    for i in 0..80 {
        let noise: String = (0..100)
            .map(|j| {
                format!("archive/record_{i}_{j}: unrelated inventory with routine entries and ordinary stock levels.\n")
            })
            .collect();
        read(&mut history, &format!("noise{i}"), &format!("archive/{i}.txt"), &noise);
        removed += curator.view(200, &history, (i + 3) as f64).unwrap().removed;
    }
    history.push(json!({"role":"user","content":"What notice was required in 2023 and after the 2024 amendment, what happened to the pets clause, and where was Mara's key first and later? Cite the evidence."}));
    let result = curator.view(200, &history, 84.0).unwrap();
    (
        ANSWER_KEY.iter().map(|(_, evidence)| has(&result.view, evidence)).collect(),
        removed + result.removed,
        result.tokens,
        ANSWER_KEY
            .iter()
            .zip(["lease/2023.txt", "lease/2024.txt", "lease/2023.txt", "story/chapter1.txt"])
            .map(|((_, evidence), path)| attributed(&result.view, path, evidence))
            .collect(),
    )
}

#[test]
fn legal_and_narrative_evidence_replay() {
    for window in [10_000, 30_000] {
        for reverse_documents in [false, true] {
            let first = replay(window, reverse_documents);
            assert_eq!(first, replay(window, reverse_documents), "fixed replay must be deterministic");
            assert!(first.1 > 0, "noise must exercise compaction at {window}");
            for ((label, _), present) in ANSWER_KEY.iter().zip(&first.0) {
                assert!(present, "missing {label} after compaction: window={window}, reverse={reverse_documents}");
            }
            for ((label, _), verified) in ANSWER_KEY.iter().zip(&first.3) {
                assert!(
                    verified,
                    "{label} not verifiable by the visible-pair oracle: window={window}, reverse={reverse_documents}"
                );
            }
            println!("source-attributed (false = not verifiable by this oracle): {:?}", first.3);
            println!(
                "estimated window={window}, reverse={reverse_documents}, evidence {:?}, removals={}, final estimated tokens={}",
                ANSWER_KEY.iter().map(|x| x.0).zip(&first.0).collect::<Vec<_>>(),
                first.1,
                first.2
            );
        }
    }
}

#[test]
fn answer_key_rejects_wrong_version_and_partial_story() {
    let old = [json!({"role":"user","content":V1})];
    assert!(has(&old, ANSWER_KEY[0].1));
    assert!(!has(&old, ANSWER_KEY[1].1));
    assert!(!has(&[json!({"role":"user","content":"Mara hid the silver key under the blue bench."})], ANSWER_KEY[3].1));
    for (missing, (_, evidence)) in ANSWER_KEY.iter().enumerate() {
        let rest: Vec<_> = ANSWER_KEY
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != missing)
            .map(|(_, (_, s))| json!({"role":"user","content":s}))
            .collect();
        assert!(!has(&rest, evidence), "missing evidence {missing} must be detected");
    }
}

#[test]
fn source_attribution_oracle_adversarial_controls() {
    let mut correct = Vec::new();
    read(&mut correct, "doc", "lease/2023.txt", V1);
    assert!(attributed(&correct, "lease/2023.txt", ANSWER_KEY[0].1));
    let mut swapped = correct.clone();
    swapped[0]["content"][0]["input"]["path"] = json!("lease/2024.txt");
    assert!(!attributed(&swapped, "lease/2023.txt", ANSWER_KEY[0].1));
    let mut missing_id = correct.clone();
    missing_id[1]["content"][0].as_object_mut().unwrap().remove("tool_use_id");
    assert!(!attributed(&missing_id, "lease/2023.txt", ANSWER_KEY[0].1));
    let mut error = correct.clone();
    error[1]["content"][0]["is_error"] = json!(true);
    assert!(!attributed(&error, "lease/2023.txt", ANSWER_KEY[0].1));
    let mut duplicate = correct.clone();
    duplicate.push(correct[0].clone());
    assert!(!attributed(&duplicate, "lease/2023.txt", ANSWER_KEY[0].1));
    assert!(!attributed(&correct[1..], "lease/2023.txt", ANSWER_KEY[0].1));
    assert!(!attributed(&correct, "lease/2023.txt", ANSWER_KEY[1].1));
}

// A second, fixed corpus for comparing curator revisions: the key is hand-labelled independently
// of the replay. Scores are observations, not assertions that either implementation must win.
const COMPARATIVE_DOCS: [(&str, &str); 6] = [
    ("lease/alder-2022.txt", "Alder lease, article 7: notice is 60 days."),
    ("lease/alder-correction.txt", "Alder lease correction: notice is 45 days, not 60 days."),
    ("lease/birch.txt", "Birch lease, article 7: notice is 30 days, not 45 days."),
    ("story/mara-north.txt", "Mara of North placed the brass key in the green drawer."),
    (
        "story/mara-north-correction.txt",
        "Correction: Mara of North moved the brass key to the black chest, not the white chest.",
    ),
    ("story/mara-south.txt", "Mara of South moved the brass key to the white chest, not the black chest."),
];
// Question, source, exact evidence. The queries below do not contain the answers.
const COMPARATIVE_KEY: [(&str, &str, &str); 4] = [
    ("alder original", "lease/alder-2022.txt", "Alder lease, article 7: notice is 60 days."),
    ("alder corrected", "lease/alder-correction.txt", "Alder lease correction: notice is 45 days, not 60 days."),
    ("north initial", "story/mara-north.txt", "Mara of North placed the brass key in the green drawer."),
    (
        "north corrected",
        "story/mara-north-correction.txt",
        "Correction: Mara of North moved the brass key to the black chest, not the white chest.",
    ),
];

#[test]
fn comparative_corpus_reports_evidence_and_source_without_assuming_a_winner() {
    for window in [10_000, 30_000] {
        for reverse in [false, true] {
            let mut curator = Race::new("").unwrap();
            curator.window(window);
            let mut history =
                vec![json!({"role":"user","content":"Review both leases and both stories, including corrections."})];
            let mut documents = COMPARATIVE_DOCS;
            if reverse {
                documents.reverse();
            }
            for (i, (path, text)) in documents.iter().enumerate() {
                read(&mut history, &format!("case{i}"), path, text);
                curator.view(200, &history, i as f64).unwrap();
            }
            let mut removed = 0;
            for i in 0..80 {
                let noise: String = (0..100)
                    .map(|j| {
                        format!(
                            "archive/lease_{i}_{j}: tenant notice and brass key inventory for unrelated premises.\n"
                        )
                    })
                    .collect();
                read(&mut history, &format!("filler{i}"), &format!("archive/{i}.txt"), &noise);
                removed += curator.view(200, &history, (i + 6) as f64).unwrap().removed;
            }
            history.push(json!({"role":"user","content":"For the Alder lease, what was the original requirement and its correction? Where did Mara of North initially place the key and where did she later move it? Cite sources."}));
            let result = curator.view(200, &history, 87.0).unwrap();
            removed += result.removed;
            assert!(removed > 0, "fixture must exercise compaction");
            let scores: Vec<_> = COMPARATIVE_KEY
                .iter()
                .map(|(label, path, evidence)| {
                    (*label, has(&result.view, evidence), attributed(&result.view, path, evidence))
                })
                .collect();
            println!("comparative window={window} reverse={reverse} removals={removed} estimated_tokens={} scores(label,present,attributed)={scores:?}", result.tokens);
        }
    }
}

#[test]
fn comparative_key_distinguishes_distractors_and_corrections() {
    for (label, path, evidence) in COMPARATIVE_KEY {
        let mut actual = Vec::new();
        let (_, text) = COMPARATIVE_DOCS.iter().find(|(p, _)| *p == path).unwrap();
        read(&mut actual, "actual", path, text);
        assert!(attributed(&actual, path, evidence), "{label}: source positive");
        for (other_path, other_text) in COMPARATIVE_DOCS.iter().filter(|(p, _)| *p != path) {
            let mut other = Vec::new();
            read(&mut other, "other", other_path, other_text);
            assert!(!has(&other, evidence), "{label}: decoy {other_path}");
            assert!(!attributed(&other, path, evidence), "{label}: wrong source {other_path}");
        }
    }
}
