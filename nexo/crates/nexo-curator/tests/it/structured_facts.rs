//! Hand-labelled, offline checks. Exact clauses are a deliberately conservative oracle, not a semantic parser.
//! A clause must survive intact: words scattered across different statements are not sufficient evidence.

use nexo_curator::{sim::events::forgotten, Curator};
use serde_json::{json, Value};

fn user(text: &str) -> Value {
    json!({"role": "user", "content": text})
}

fn assistant(text: &str) -> Value {
    json!({"role": "assistant", "content": [{"type": "text", "text": text}]})
}

struct Case {
    name: &'static str,
    history: &'static str,
    required: &'static str,
    decoy: &'static str,
    task: &'static str,
}

// The old and corrected values are deliberately in the same sentence: preserving only the old value,
// or the same words with their relation inverted, must not count as preserving the correction.
const CASES: &[Case] = &[
    Case {
        name: "negation",
        history: "Never deploy the atlas service to production.",
        required: "Never deploy the atlas service to production.",
        decoy: "Deploy the atlas service to production.",
        task: "Prepare the atlas deployment.",
    },
    Case {
        name: "small number",
        history: "The atlas retry limit is 2, not 3.",
        required: "The atlas retry limit is 2, not 3.",
        decoy: "The atlas retry limit is 3, not 2.",
        task: "Configure atlas retries.",
    },
    Case {
        name: "correction",
        history: "Correction: atlas uses port 5433, not port 5432.",
        required: "Correction: atlas uses port 5433, not port 5432.",
        decoy: "Correction: atlas uses port 5432, not port 5433.",
        task: "Connect to atlas.",
    },
    Case {
        name: "constraint",
        history: "Run atlas tests offline; never use network access.",
        required: "Run atlas tests offline; never use network access.",
        decoy: "Run atlas tests with network access, never offline.",
        task: "Run the atlas tests.",
    },
];

fn has_clause(view: &[Value], clause: &str) -> bool {
    // text_of excludes JSON metadata and extracts only message content used by the simulator.
    view.iter().any(|m| nexo_curator::sim::input::text_of(m).contains(clause))
}

#[test]
fn labelled_oracle_rejects_missing_and_inverted_facts() {
    for c in CASES {
        assert!(has_clause(&[user(c.history)], c.required), "{} positive", c.name);
        assert!(!has_clause(&[user(c.decoy)], c.required), "{} inverted", c.name);
        assert!(!has_clause(&[], c.required), "{} omitted", c.name);
        // Conservative by design: a correct paraphrase is not certified by this lexical oracle.
    }
    assert!(!has_clause(&[user("Do not deploy atlas to production.")], CASES[0].required));
}

#[test]
fn structured_instructions_survive_a_curated_view_after_tool_noise() {
    for c in CASES {
        let mut cur = Curator::new("").unwrap();
        let mut messages = vec![user(c.history), assistant("Understood.")];
        for i in 0..24 {
            let id = format!("call_{i}");
            let output: String =
                (0..35).map(|n| format!("src/component_{i}/row_{n}.rs: diagnostic value_{i}_{n}\n")).collect();
            messages.push(json!({"role":"assistant", "content":[{"type":"tool_use","id":id,"name":"bash","input":{"command":format!("inspect component_{i}")}}]}));
            messages.push(json!({"role":"user", "content":[{"type":"tool_result","tool_use_id":id,"content":output}]}));
            cur.view(0, &messages, i as f64).unwrap();
        }
        messages.push(user(c.task));
        let view = cur.view(0, &messages, 25.0).unwrap().view;
        assert!(has_clause(&view, c.required), "{}: required clause lost from curated view", c.name);
    }
}

#[test]
fn replay_word_metric_can_miss_a_reversed_fact() {
    for c in CASES {
        let history = [user(c.history)];
        let reply = assistant(c.history);
        let inverted = [user(c.decoy)];
        let (lost, copied) = forgotten(&reply, &history, &inverted, "");
        // This is a measured limitation, not evidence that the reversed view is safe.
        if c.name == "small number" || c.name == "correction" {
            if c.name == "correction" {
                assert!(copied > 0, "{}: exercise the metric", c.name);
            } else {
                // Single-digit numbers and ordinary words are not distinctive here.
                assert_eq!(copied, 0, "{}", c.name);
            }
            assert!(lost.is_empty(), "{}: {lost:?}", c.name);
            assert!(!has_clause(&inverted, c.required));
        }
    }
}
