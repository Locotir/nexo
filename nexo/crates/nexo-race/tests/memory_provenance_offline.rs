//! Offline comparative replay. The oracle never treats a textual memory as proof of source.
//! A message number, nearby path or ID is not a validated association to one result block.
use nexo_race::Race;
use serde_json::{json, Value};
use std::collections::HashMap;

const A: &str = "Zephyr pump inspection pressure is 43 kilopascals.";
const B: &str = "Zephyr pump inspection temperature is 71 degrees.";

#[derive(Debug, PartialEq, Eq)]
enum Evidence {
    Absent,
    TextOnlyUnverified,
    StructuredVerified,
}

fn contains(v: &Value, needle: &str) -> bool {
    match v {
        Value::String(s) => s.contains(needle),
        Value::Array(a) => a.iter().any(|x| contains(x, needle)),
        Value::Object(o) => o.values().any(|x| contains(x, needle)),
        _ => false,
    }
}

// Verification is intentionally limited to a unique, successful structured call/result pair.
// Duplicate results also invalidate the pair; ordering is by message, never by block position.
fn evidence(view: &[Value], path: &str, needle: &str) -> Evidence {
    let mut calls: HashMap<&str, Vec<(usize, &Value)>> = HashMap::new();
    let mut results: HashMap<&str, Vec<(usize, &Value)>> = HashMap::new();
    for (i, m) in view.iter().enumerate() {
        if let Some(blocks) = m["content"].as_array() {
            for b in blocks {
                let (id, map) = if m["role"] == "assistant" && b["type"] == "tool_use" {
                    (b["id"].as_str(), &mut calls)
                } else if m["role"] == "user" && b["type"] == "tool_result" {
                    (b["tool_use_id"].as_str(), &mut results)
                } else {
                    continue;
                };
                if let Some(id) = id.filter(|s| !s.is_empty()) {
                    map.entry(id).or_default().push((i, b));
                }
            }
        }
    }
    for (id, cs) in &calls {
        if cs.len() != 1 {
            continue;
        }
        let Some(rs) = results.get(id).filter(|r| r.len() == 1) else { continue };
        let (ci, c) = cs[0];
        let (ri, r) = rs[0];
        if ci < ri
            && c["name"] == "read"
            && c["input"]["path"] == path
            && !r["is_error"].as_bool().unwrap_or(false)
            && contains(&r["content"], needle)
        {
            return Evidence::StructuredVerified;
        }
    }
    if view.iter().any(|v| contains(v, needle)) {
        Evidence::TextOnlyUnverified
    } else {
        Evidence::Absent
    }
}

fn pair() -> Vec<Value> {
    vec![
        json!({"role":"assistant","content":[
        {"type":"tool_use","id":"a","name":"read","input":{"path":"audit/a"}},
        {"type":"tool_use","id":"b","name":"read","input":{"path":"audit/b"}}]}),
        json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"b","content":B},
            {"type":"tool_result","tool_use_id":"a","content":A}]}),
    ]
}

#[test]
fn oracle_controls() {
    use Evidence::*;
    let p = pair();
    assert_eq!(evidence(&p, "audit/a", A), StructuredVerified);
    assert_eq!(evidence(&p, "audit/b", B), StructuredVerified);
    assert_eq!(evidence(&p, "audit/b", A), TextOnlyUnverified);
    let mut duplicate_call = p.clone();
    duplicate_call[0]["content"][1]["id"] = json!("a");
    assert_eq!(evidence(&duplicate_call, "audit/a", A), TextOnlyUnverified);
    let mut duplicate_result = p.clone();
    duplicate_result[1]["content"].as_array_mut().unwrap().push(p[1]["content"][1].clone());
    assert_eq!(evidence(&duplicate_result, "audit/a", A), TextOnlyUnverified);
    let mut missing = p.clone();
    missing[1]["content"][1].as_object_mut().unwrap().remove("tool_use_id");
    assert_eq!(evidence(&missing, "audit/a", A), TextOnlyUnverified);
    let mut error = p.clone();
    error[1]["content"][1]["is_error"] = json!(true);
    assert_eq!(evidence(&error, "audit/a", A), TextOnlyUnverified);
    let reordered = vec![p[1].clone(), p[0].clone()];
    assert_eq!(evidence(&reordered, "audit/a", A), TextOnlyUnverified);
    for text in [
        format!("Recall (lines of earlier messages):\nm00003 (tool):\n{A}"),
        format!("m00003 (tool) audit/a id=a: {A}"),
        format!("audit/a\n{B}\naudit/b\n{A}"),
    ] {
        assert_eq!(evidence(&[json!({"role":"user","content":text})], "audit/a", A), TextOnlyUnverified);
    }
    assert_eq!(evidence(&p, "audit/a", "not present"), Absent);
}

#[test]
fn replay_variants() {
    for variant in ["normal", "duplicate_call", "duplicate_result", "missing_id", "error"] {
        for window in [10_000, 30_000] {
            let mut curator = Race::new("").unwrap();
            curator.window(window);
            let mut p = pair();
            // Long results make compaction and text memories observable.
            for (idx, fact) in [(0, B), (1, A)] {
                let body = (0..160)
                    .map(|j| {
                        if j == 80 {
                            format!("{fact}\n")
                        } else {
                            format!("Routine archive entry {j}: stock and warehouse inventory.\n")
                        }
                    })
                    .collect::<String>();
                p[1]["content"][idx]["content"] = json!(body);
            }
            match variant {
                "duplicate_call" => p[0]["content"][1]["id"] = json!("a"),
                "duplicate_result" => {
                    let result = p[1]["content"][1].clone();
                    p[1]["content"].as_array_mut().unwrap().push(result);
                }
                "missing_id" => {
                    p[1]["content"][1].as_object_mut().unwrap().remove("tool_use_id");
                }
                "error" => p[1]["content"][1]["is_error"] = json!(true),
                _ => (),
            }
            let mut h = vec![json!({"role":"user","content":"Review Zephyr pump records."})];
            h.extend(p);
            curator.view(200, &h, 1.0).unwrap();
            let mut removed = 0;
            for i in 0..65 {
                let noise = (0..100)
                    .map(|j| format!("Batch {i} item {j}: ordinary inventory and warehouse supplies.\n"))
                    .collect::<String>();
                h.push(json!({"role":"assistant","content":[{"type":"tool_use","id":format!("n{i}"),"name":"read","input":{"path":format!("noise/{i}")}}]}));
                h.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":format!("n{i}"),"content":noise}]}));
                removed += curator.view(200, &h, (i + 2) as f64).unwrap().removed;
            }
            h.push(json!({"role":"assistant","content":"Archive review complete."}));
            h.push(json!({"role":"user","content":"What did the Zephyr pump inspection records say about pressure and temperature? Cite the sources."}));
            let r = curator.view(200, &h, 69.0).unwrap();
            removed += r.removed;
            assert!(removed > 0, "replay must compact");
            println!("MEM variant={variant} window={window} pressure={:?} temperature={:?} removals={removed} estimated_tokens={}", evidence(&r.view,"audit/a",A), evidence(&r.view,"audit/b",B), r.tokens);
        }
    }
}
