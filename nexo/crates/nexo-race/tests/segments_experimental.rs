use nexo_race::snapshot::{history_hash, message_hash};
use nexo_race::Race;
use serde_json::{json, Value};

fn run(spec: &str, duplicate: bool) -> Vec<Value> {
    let call = json!({"type":"tool_use","id":"a","name":"read_file","input":{"path":"west/ledger.csv"}});
    let mut calls = vec![call.clone()];
    if duplicate {
        calls.push(call);
    }
    let mut h = vec![
        json!({"role":"user","content":"Audit the museum inventory ledger."}),
        json!({"role":"assistant","content":calls}),
        json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"a","content":format!("anchor spindle916\nassociated cobalt\n\nother independent\n{}", (0..150).map(|i| format!("museum/west/{i}.csv,article_{i},quantity_{}\n", i*53)).collect::<String>())}]}),
    ];
    for n in 0..16 {
        let id = format!("shelf-{n}");
        let text: String =
            (0..150).map(|i| format!("table/{n}/{i:03}.csv,article_{i},quantity_{}\n", i * 53)).collect();
        h.push(json!({"role":"assistant","content":[{"type":"tool_use","id":id,"name":"read_file","input":{"path":format!("shelf/{n}.csv")}}]}));
        h.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":text}]}));
    }
    let mut c = Race::new(spec).unwrap();
    c.window(20_000);
    c.tools(&["read_file".into(), "recall".into()]);
    c.view(640, &h, 1.0).unwrap();
    let hashes: Vec<_> = h.iter().map(message_hash).collect();
    let saved = c.save(1, history_hash(&hashes)).encode();
    let (mut restored, _) = Race::restore(spec, &saved, &h, &hashes).unwrap();
    h.push(json!({"role":"assistant","content":"Finished that inventory pass."}));
    h.push(json!({"role":"user","content":"Continue the museum inventory ledger audit. What is spindle916?"}));
    let live = c.view(640, &h, 2.0).unwrap();
    let resumed = restored.view(640, &h, 2.0).unwrap();
    assert_eq!(live.view, resumed.view);
    let last = live.view.last().unwrap();
    let content = &last["content"];
    let texts: Vec<&str> = content
        .as_str()
        .into_iter()
        .chain(content.as_array().into_iter().flatten().filter_map(|b| b["text"].as_str()))
        .collect();
    texts.into_iter().flat_map(str::lines).filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect()
}

#[test]
fn segments_are_opt_in_and_preserve_pair_and_snapshot() {
    let plain = run("relations=1", false);
    let segment = run("segments=1", false);
    assert!(!plain.iter().any(|r| r["quote"] == "associated cobalt"));
    let row = segment.iter().find(|r| r["quote"] == "associated cobalt").unwrap();
    assert_eq!(row["message"], "m00003");
    assert_eq!(row["line"], 2);
    assert_eq!(row["call_id"], "a");
    assert_eq!(row["path"], "west/ledger.csv");
    assert!(!segment.iter().any(|r| r["quote"] == "other independent"));
    assert!(!run("segments=1", true).iter().any(|r| r["quote"] == "associated cobalt"));
}

#[test]
fn segment_flag_is_validated() {
    assert!(Race::new("segments=2").is_err());
    assert!(Race::new("segments=1;relations=0").is_err());
}
