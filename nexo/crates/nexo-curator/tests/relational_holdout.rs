//! Independent frozen synthetic evidence replay; no answer model is invoked.
use nexo_curator::Curator;
use serde_json::{json, Value};
use std::collections::HashMap;

fn has(view: &[Value], evidence: &str) -> bool {
    fn walk(v: &Value, e: &str) -> bool {
        match v {
            Value::String(s) => s.contains(e),
            Value::Array(a) => a.iter().any(|v| walk(v, e)),
            Value::Object(o) => o.values().any(|v| walk(v, e)),
            _ => false,
        }
    }
    view.iter().any(|m| walk(m, evidence))
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

const CASES: &[(&str, &str, &str, &str, &str, &str)] = &[
    (
        "revision",
        "audit/harbor-v1",
        "Harbor valve limit revision 1: 41 units.",
        "audit/harbor-v2",
        "Harbor valve limit revision 2 corrects revision 1: 26 units, not 41 units.",
        "What is the current Harbor valve limit and its correction?",
    ),
    (
        "conflict",
        "audit/quartz-a",
        "Quartz gate clearance report A: 8 meters.",
        "audit/quartz-b",
        "Quartz gate clearance report B: 13 meters; conflict unresolved.",
        "What is the confirmed Quartz gate clearance? Abstain if the evidence conflicts.",
    ),
    (
        "missing",
        "audit/ivory-a",
        "Ivory turbine inspection records contain no rotor weight.",
        "audit/ivory-b",
        "Ivory turbine maintenance is scheduled for Tuesday.",
        "What is the Ivory turbine rotor weight? Abstain if unavailable.",
    ),
];
#[test]
fn relational_replay() {
    for &(name, p1, a, p2, b, q) in CASES {
        for reverse in [false, true] {
            for window in [10000, 30000] {
                let mut c = Curator::new("").unwrap();
                c.window(window);
                let mut h = vec![json!({"role":"user","content":"Review archived engineering records."})];
                let mut docs = [(p1, a), (p2, b)];
                if reverse {
                    docs.reverse();
                }
                // Both reads are requested in one assistant message; results arrive in reverse order.
                let mut calls = Vec::new();
                let mut results = Vec::new();
                for (i, (path, text)) in docs.iter().enumerate() {
                    let mut body = String::new();
                    for j in 0..160 {
                        if j == 80 {
                            body.push_str(text);
                            body.push('\n');
                        }
                        body.push_str(&format!("Routine archive item {j}: standard shipping and inventory notes.\n"));
                    }
                    calls.push(json!({"type":"tool_use","id":format!("doc{i}"),"name":"read","input":{"path":path}}));
                    results.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":format!("doc{i}"),"content":body}]}));
                }
                h.push(json!({"role":"assistant","content":calls}));
                h.extend(results.into_iter().rev());
                c.view(200, &h, 1.0).unwrap();
                let mut removed = 0;
                for i in 0..65 {
                    let noise = (0..100)
                        .map(|j| format!("Batch {i} item {j}: ordinary inventory and warehouse supplies.\n"))
                        .collect::<String>();
                    read(&mut h, &format!("noise{i}"), &format!("noise/{i}"), &noise);
                    removed += c.view(200, &h, (i + 2) as f64).unwrap().removed;
                }
                let pre = c.view(200, &h, 68.0).unwrap();
                removed += pre.removed;
                h.push(json!({"role":"assistant","content":"The archive review is complete."}));
                h.push(json!({"role":"user","content":q}));
                let r = c.view(200, &h, 69.0).unwrap();
                removed += r.removed;
                assert!(removed > 0);
                if std::env::var_os("REL_DUMP").is_some() && name == "revision" && !reverse && window == 10000 {
                    for m in &r.view {
                        if has(std::slice::from_ref(m), a) || has(std::slice::from_ref(m), b) {
                            println!("EVIDENCE {}", m);
                        }
                    }
                }
                println!("REL case={name} reverse={reverse} window={window} pre_a={} pre_b={} a={} b={} attr_a={} attr_b={} removals={removed} tokens={}",has(&pre.view,a),has(&pre.view,b),has(&r.view,a),has(&r.view,b),attributed(&r.view,p1,a),attributed(&r.view,p2,b),r.tokens);
            }
        }
    }
}
#[test]
fn attribution_mutations() {
    let (_, p, a, p2, b, _) = CASES[0];
    let mut h = Vec::new();
    read(&mut h, "one", p, a);
    read(&mut h, "two", p2, b);
    assert!(attributed(&h, p, a));
    assert!(attributed(&h, p2, b));
    assert!(!attributed(&h, p, b));
    assert!(!attributed(&h, p2, a));
    let mut swap = h.clone();
    swap[0]["content"][0]["input"]["path"] = json!(p2);
    swap[2]["content"][0]["input"]["path"] = json!(p);
    assert!(!attributed(&swap, p, a));
    assert!(!attributed(&swap, p2, b));
    let mut err = h.clone();
    err[1]["content"][0]["is_error"] = json!(true);
    assert!(!attributed(&err, p, a));
    let mut dup = h.clone();
    dup.push(h[0].clone());
    assert!(!attributed(&dup, p, a));
    assert!(!attributed(&[h[1].clone(), h[0].clone()], p, a));
    // Parallel calls and permuted results must be joined by ID, never by position.
    let parallel = vec![
        json!({"role":"assistant","content":[h[0]["content"][0].clone(), h[2]["content"][0].clone()]}),
        h[3].clone(),
        h[1].clone(),
    ];
    assert!(attributed(&parallel, p, a));
    assert!(attributed(&parallel, p2, b));
    assert!(!attributed(&parallel, p, b));
    let mut missing_id = parallel.clone();
    missing_id[2]["content"][0]["tool_use_id"] = json!("");
    assert!(!attributed(&missing_id, p, a));
    let recall = vec![
        json!({"role":"user","content":[{"type":"text","text":format!("Recall (lines of earlier messages that this request names):\nm00003 (tool):\n{a}")}]}),
    ];
    assert!(has(&recall, a));
    assert!(!attributed(&recall, p, a), "message number without a visible source cannot prove attribution");
}
