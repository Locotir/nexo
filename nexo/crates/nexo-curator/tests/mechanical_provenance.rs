//! Cold black-box provenance contracts. Every positive quote is resolved against raw input.
use nexo_curator::{Curator, Reply};
use serde_json::{json, Value};

const HEAD: u64 = 640;
const WINDOW: u64 = 20_000;

fn msg(role: &str, content: impl Into<Value>) -> Value {
    json!({"role":role,"content":content.into()})
}
fn call(id: &str) -> Value {
    json!({"type":"tool_use","id":id,"name":"read_file","input":{"path":format!("{id}.txt")}})
}
fn result(id: &str, content: &str) -> Value {
    json!({"type":"tool_result","tool_use_id":id,"content":content})
}
fn page(n: usize) -> String {
    (0..n).map(|i| format!("archive/{i:03}.csv,entry_{i},count_{}\n", i * 17)).collect()
}
fn history(calls: Vec<Value>, results: Vec<Value>) -> Vec<Value> {
    vec![msg("user", "Audit the museum inventory ledger."), msg("assistant", calls), msg("user", results)]
}
fn records(reply: &Reply) -> Vec<Value> {
    assert!(reply.tokens >= 0 && reply.tokens + HEAD as i64 <= WINDOW as i64);
    let last = reply.view.last().expect("final user request");
    assert_eq!(last["role"], "user");
    // Only the appended section in the final request is evidence; raw visible tool
    // results elsewhere in the view cannot satisfy a historical-quote assertion.
    let blocks = last["content"].as_array().expect("final request with evidence blocks");
    for text in blocks.iter().filter_map(|b| b["text"].as_str()) {
        let mut lines = text.lines();
        if lines.any(|l| l.starts_with("Historical evidence")) {
            let rows = lines
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str::<Value>(l).expect("JSON evidence record"))
                .collect::<Vec<_>>();
            assert!(!rows.is_empty(), "empty evidence section");
            assert_eq!(reply.relation_quotes, Some(rows.len()));
            return rows;
        }
    }
    panic!("missing Historical evidence section: {last}");
}
fn cold(h: &[Value], question: &str) -> Vec<Value> {
    let mut h = h.to_vec();
    // Distinct, substantial pages prevent deduplication from defeating window pressure.
    for n in 0..16 {
        let id = format!("age-{n}");
        h.extend([
            msg("assistant", vec![call(&id)]),
            msg(
                "user",
                vec![result(
                    &id,
                    &(0..150)
                        .map(|i| format!("table/{id}/{i:03}.csv,article_{i},bin_{},quantity_{}\n", i % 17, i * 53))
                        .collect::<String>(),
                )],
            ),
        ]);
    }
    let mut c = Curator::new("relations=1").unwrap();
    c.tools(&["read_file".into(), "bash".into(), "edit_file".into(), "recall".into()]);
    c.window(WINDOW);
    // Commit eviction before posing the lookup, as in relations.rs::query_pair.
    c.view(HEAD, &h, 1.0).unwrap();
    h.extend([
        msg("assistant", "Finished that inventory pass."),
        msg("user", format!("Continue the museum inventory ledger audit. {question}")),
    ]);
    let reply = c.view(HEAD, &h, 2.0).unwrap();
    let rows = records(&reply);
    for r in &rows {
        let mi = r["message"].as_str().unwrap().strip_prefix('m').unwrap().parse::<usize>().unwrap() - 1;
        assert!(mi < h.len() - 1, "nonhistorical source: {r}");
        let bi = r["block"].as_u64().unwrap() as usize - 1;
        let m = &h[mi];
        let b = &m["content"][bi];
        let (raw, kind) = if let Some(s) = m["content"].as_str() {
            (s, if m["role"] == "user" { "person" } else { "assistant" })
        } else if b["type"] == "tool_result" {
            (b["content"].as_str().unwrap(), "tool")
        } else {
            (b["text"].as_str().unwrap_or(""), if m["role"] == "user" { "person" } else { "assistant" })
        };
        let line = r["line"].as_u64().unwrap() as usize - 1;
        assert_eq!(raw.lines().nth(line), r["quote"].as_str(), "nonliteral or shifted quote: {r}");
        assert_eq!(r["kind"], kind, "wrong source kind: {r}");
        if r.get("call_id").is_some() {
            assert_eq!(kind, "tool");
            let ci = r["call_message"].as_str().unwrap().strip_prefix('m').unwrap().parse::<usize>().unwrap() - 1;
            let cb = r["call_block"].as_u64().unwrap() as usize - 1;
            assert_eq!(ci + 1, mi, "call not immediately preceding result: {r}");
            assert_eq!(h[ci]["content"][cb]["type"], "tool_use");
            assert_eq!(h[ci]["content"][cb]["id"], b["tool_use_id"], "misattributed call: {r}");
            assert_eq!(r["call_id"], b["tool_use_id"], "wrong advertised ID: {r}");
            let id = r["call_id"].as_str().unwrap();
            assert_eq!(
                h[ci]["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|b| b["type"] == "tool_use" && b["id"] == id)
                    .count(),
                1
            );
            assert_eq!(
                m["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|b| b["type"] == "tool_result" && b["tool_use_id"] == id)
                    .count(),
                1
            );
        }
    }
    rows
}

#[test]
fn crlf_utf8_duplicate_lines_and_shifted_coordinates_are_literal() {
    let quote = "receipt café_日本 = 🧭";
    let body = format!("{quote}\r\n{quote}\r\n{}tail Ω", page(150));
    let rows = cold(&history(vec![call("receipt")], vec![result("receipt", &body)]), "Locate receipt café_日本.");
    let hits = rows.iter().filter(|r| r["quote"] == quote).collect::<Vec<_>>();
    assert!(!hits.is_empty(), "target receipt not recovered");
    for hit in hits {
        assert_eq!(hit["message"], "m00003");
        assert!([1, 2].contains(&hit["line"].as_u64().unwrap()));
        assert_eq!(hit["call_id"], "receipt");
    }
}

#[test]
fn permuted_parallel_results_retain_independent_call_attribution() {
    let quote = "parallel receipt = cobalt";
    let body = format!("{quote}\n{}", page(150));
    let h = history(vec![call("west"), call("east")], vec![result("east", &body), result("west", &body)]);
    let rows = cold(&h, "Compare parallel receipt sources.");
    for (id, block) in [("east", 2), ("west", 1)] {
        let r = rows
            .iter()
            .find(|r| r["quote"] == quote && r["call_id"] == id)
            .unwrap_or_else(|| panic!("missing {id}: {rows:?}"));
        assert_eq!(r["call_block"], block);
        assert_eq!(r["line"], 1);
    }
}

#[test]
fn duplicate_missing_and_orphan_ids_never_borrow_attribution() {
    let quote = "safely sourced receipt";
    let body = format!("{quote}\n{}", page(150));
    let mut noid = call("removed");
    noid.as_object_mut().unwrap().remove("id");
    let mut missing = result("absent", &body);
    missing.as_object_mut().unwrap().remove("tool_use_id");
    let mut orphan = result("orphan", &body);
    let h = history(
        vec![call("dup"), call("dup"), call("good"), noid],
        vec![result("dup", &body), result("good", &body), missing.take(), orphan.take()],
    );
    let rows = cold(&h, "Find safely sourced receipt.");
    let good = rows
        .iter()
        .find(|r| r["quote"] == quote && r["call_id"] == "good")
        .unwrap_or_else(|| panic!("valid sibling lost: {rows:?}"));
    assert_eq!(good["call_block"], 3);
    for r in rows.iter().filter(|r| r["quote"] == quote && r["message"] == "m00003" && r["call_id"] != "good") {
        assert!(
            r.get("call_id").is_none() && r.get("call_block").is_none() && r.get("call_message").is_none(),
            "unsafe ambiguous attribution: {r}"
        );
    }
}
