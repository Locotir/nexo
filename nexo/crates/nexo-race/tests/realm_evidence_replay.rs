//! Offline replay of recorded tool traffic. Presence is not semantic understanding, and neither
//! replay nor attribution demonstrates that the binary was solved. Run with --nocapture to
//! compare the same fixture and oracle across curator revisions. Tokens are curator estimates.
use nexo_race::Race;
use serde_json::{json, Value};
use std::collections::HashMap;

const FIXTURE: &str = include_str!("fixtures/bench-realm-D.jsonl");
// Hand-labelled from the original tool results, not generated from curator output. These are
// observations, not claims that the manual or a provisional model is the correct loot formula.
// label, original call id, identifying command, exact excerpt
const KEY: &[(&str, &str, &str, &str)] = &[
    ("available data", "call_01_NA5w3RLLm1pWxwyMTWah8553", "data", "data/enemies.dat\ndata/items.dat\ndata/zones.dat"),
    ("binary format", "call_00_nnt9Zb2yA9jaIUkwb7hx6913", "file realm", "realm: ELF 64-bit LSB pie executable"),
    (
        "enemy record",
        "call_00_y7NFHGDSwOHISDUxDkm02880",
        "cat data/enemies.dat",
        "draullor|Draullor Husk|beast|35|405|23|10|260",
    ),
    ("zone record", "call_00_erI7O1F3oftvpKzdR9x29132", "cat data/zones.dat", "zengal_vale|0|cave|6"),
    (
        "seed experiment",
        "call_00_VgV69zCPA6wf4PkSsCs09841",
        "for s in 1 2 3 4 5 6 7 8",
        "seed 1: The kaven is defeated after 22 rounds.\nLoot: 16 coins",
    ),
    ("model mismatch", "call_00_dyx24aDCQ3CL4oUCcv486271", "exp/model.py", "ornrhorho vexzen_crypt 50 14 107"),
];

fn present(view: &[Value], needle: &str) -> bool {
    view.iter().any(|m| nexo_race::sim::input::text_of(m).contains(needle))
}

// A unique earlier matching tool call + a successful result with the original ID and evidence.
// False includes unprovable attribution (e.g. a memory summary), not necessarily wrong attribution.
fn attributed(view: &[Value], id: &str, command: &str, needle: &str) -> bool {
    let mut calls: HashMap<&str, Vec<(usize, &Value)>> = HashMap::new();
    for (i, m) in view.iter().enumerate() {
        if m["role"] == "assistant" {
            if let Some(blocks) = m["content"].as_array() {
                for b in blocks.iter().filter(|b| b["type"] == "tool_use") {
                    if let Some(id) = b["id"].as_str().filter(|s| !s.is_empty()) {
                        calls.entry(id).or_default().push((i, b));
                    }
                }
            }
        }
    }
    view.iter().enumerate().any(|(i, m)| {
        m["role"] == "user"
            && m["content"].as_array().is_some_and(|blocks| {
                blocks.iter().any(|b| {
                    if b["type"] != "tool_result" || b["tool_use_id"] != id || b["is_error"].as_bool().unwrap_or(false)
                    {
                        return false;
                    }
                    let Some((j, call)) = calls.get(id).filter(|c| c.len() == 1).and_then(|c| c.first()) else {
                        return false;
                    };
                    let input = if call["name"] == "run_command" {
                        call["input"]["command"].as_str()
                    } else if call["name"] == "list_files" {
                        call["input"]["path"].as_str()
                    } else {
                        None
                    };
                    *j < i
                        && input.is_some_and(|s| s.contains(command))
                        && b["content"].as_str().is_some_and(|s| s.contains(needle))
                })
            })
    })
}

fn records() -> Vec<Value> {
    FIXTURE.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}
fn original() -> Vec<Value> {
    records().iter().flat_map(|r| r["req"]["new"].as_array().unwrap().iter().cloned()).collect()
}

#[test]
fn realm_replay_comparative_observations() {
    for window in [10_000, 30_000] {
        let mut curator = Race::new("").unwrap();
        curator.window(window);
        let mut history = Vec::new();
        let mut removed = 0;
        for r in records() {
            history.extend(r["req"]["new"].as_array().unwrap().iter().cloned());
            removed += curator.view(120, &history, r["now"].as_f64().unwrap()).unwrap().removed;
        }
        history.push(json!({"role":"user","content":"Review the recorded binary investigation: which data files and enemy and zone records were observed, what did the seed experiment show, and what was the model mismatch? Cite the tool evidence."}));
        let result = curator.view(120, &history, 1750001200.0).unwrap();
        removed += result.removed;
        assert!(removed > 0, "fixture must exercise compaction at {window}");
        println!("realm window_estimate={window} removals={removed} final_estimated_tokens={}", result.tokens);
        for (label, id, cmd, excerpt) in KEY {
            println!(
                "  {label}: present={} attributed={}",
                present(&result.view, excerpt),
                attributed(&result.view, id, cmd, excerpt)
            );
        }
    }
}

#[test]
fn realm_oracle_and_fixture_controls() {
    let history = original();
    for (label, id, cmd, excerpt) in KEY {
        assert!(present(&history, excerpt), "fixture missing {label}");
        assert!(attributed(&history, id, cmd, excerpt), "fixture source missing {label}");
        assert!(!attributed(&history, id, "not-the-command", excerpt));
        assert!(!attributed(&history, "missing-id", cmd, excerpt));
        assert!(!attributed(&history, id, cmd, "not-the-evidence"));
    }
    let (_, id, cmd, excerpt) = KEY[2];
    let call =
        history.iter().find(|m| m["content"].as_array().is_some_and(|b| b.iter().any(|v| v["id"] == id))).unwrap();
    let result = history
        .iter()
        .find(|m| m["content"].as_array().is_some_and(|b| b.iter().any(|v| v["tool_use_id"] == id)))
        .unwrap();
    let pair = vec![call.clone(), result.clone()];
    assert!(attributed(&pair, id, cmd, excerpt));
    assert!(!attributed(&[result.clone(), call.clone()], id, cmd, excerpt));
    assert!(!attributed(&[call.clone(), result.clone(), call.clone()], id, cmd, excerpt));
    let mut error = result.clone();
    let b = error["content"].as_array_mut().unwrap().iter_mut().find(|b| b["tool_use_id"] == id).unwrap();
    b["is_error"] = json!(true);
    assert!(!attributed(&[call.clone(), error], id, cmd, excerpt));
}
