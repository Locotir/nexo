//! Black-box tests of the historical evidence actually sent to the model. The oracle uses only
//! the original conversation, never Curator's index, trace, compacted output or ranking scores.
use nexo_curator::snapshot::{history_hash, message_hash};
use nexo_curator::{Curator, Reply};
use serde_json::{json, Value};

const SPEC: &str = "relations=1";
const WINDOW: u64 = 20_000;
const HEAD: u64 = 640;

fn msg(role: &str, content: impl Into<Value>) -> Value {
    json!({"role":role, "content":content.into()})
}

fn call(id: &str, tool: &str, input: Value) -> Value {
    json!({"type":"tool_use", "id":id, "name":tool, "input":input})
}

fn read(id: &str, path: &str) -> Value {
    call(id, "read_file", json!({"path":path}))
}

fn result(id: &str, body: &str) -> Value {
    json!({"type":"tool_result", "tool_use_id":id, "content":body})
}

fn page(label: &str, n: usize) -> String {
    (0..n).map(|i| format!("table/{label}/{i:03}.csv,article_{i},bin_{},quantity_{}\n", i % 17, i * 53)).collect()
}

fn history(calls: Vec<Value>, results: Vec<Value>) -> Vec<Value> {
    vec![msg("user", "Audit the museum inventory ledger."), msg("assistant", calls), msg("user", results)]
}

/// Evict evidence before asking for it. No sleeps, model, token-price estimates or hidden state.
fn age(h: &mut Vec<Value>) {
    for n in 0..16 {
        let id = format!("shelf-{n}");
        let path = format!("shelves/shelf-{n}.csv");
        h.extend([msg("assistant", vec![read(&id, &path)]), msg("user", vec![result(&id, &page(&id, 150))])]);
    }
}

/// Only JSON records in the advertised section of the final user message are evidence. A match in
/// raw history, the question, a standing fact or a synthetic card cannot pass a positive assertion.
fn records(reply: &Reply) -> Vec<Value> {
    assert!(reply.tokens + HEAD as i64 <= WINDOW as i64, "view used {} plus head", reply.tokens);
    let last = reply.view.last().expect("final user request");
    assert_eq!(last["role"], "user");
    let texts: Vec<_> = last["content"].as_array().into_iter().flatten().filter_map(|b| b["text"].as_str()).collect();
    for text in &texts {
        let mut lines = text.lines();
        if lines.any(|l| l.starts_with("Historical evidence")) {
            let rows: Vec<Value> = lines
                .filter(|l| !l.trim().is_empty())
                .map(|l| {
                    let row: Value =
                        serde_json::from_str(l).unwrap_or_else(|e| panic!("not a JSON evidence record: {l}: {e}"));
                    assert!(row["kind"].is_string(), "missing record kind: {row}");
                    row
                })
                .collect();
            assert!(!rows.is_empty(), "empty evidence section");
            assert_eq!(reply.relation_quotes, Some(rows.len()));
            return rows;
        }
    }
    panic!(
        "missing Historical evidence section: {}",
        last["content"].to_string().chars().take(1000).collect::<String>()
    );
}

/// Query once live and from an actual encoded snapshot saved *before* the query. Restoring with
/// the original prefix is the public host API; neither branch is allowed to feed a view back.
fn query_pair(spec: &str, mut h: Vec<Value>, question: &str) -> (Vec<Value>, Reply, Reply) {
    let mut c = Curator::new(spec).unwrap();
    c.window(WINDOW);
    c.tools(&["read_file".into(), "bash".into(), "edit_file".into(), "recall".into()]);
    age(&mut h);
    c.view(HEAD, &h, 1.0).unwrap();
    let hashes = h.iter().map(message_hash).collect::<Vec<_>>();
    let saved = c.save(1, history_hash(&hashes)).encode();
    let (mut restored, _) = Curator::restore(spec, &saved, &h, &hashes).unwrap();
    h.extend([
        msg("assistant", "Finished that inventory pass."),
        msg("user", format!("Continue the museum inventory ledger audit. {question}")),
    ]);
    let live = c.view(HEAD, &h, 2.0).unwrap();
    let resumed = restored.view(HEAD, &h, 2.0).unwrap();
    (h, live, resumed)
}

fn query(h: Vec<Value>, question: &str) -> Vec<Value> {
    let (h, live, resumed) = query_pair(SPEC, h, question);
    let rows = records(&live);
    assert_eq!(rows, records(&resumed), "evidence changed across snapshot");
    // Resolve each quote independently against the supplied raw history, not just the target
    // phrases. Relabelling synthetic text with a different message/block cannot make it valid.
    for r in &rows {
        let name = r["message"].as_str().expect("source message");
        let n: usize = name.strip_prefix('m').unwrap().parse().unwrap();
        assert_eq!(name, format!("m{n:05}"));
        assert!((1..h.len()).contains(&n), "not a historical message: {r}");
        let m = &h[n - 1];
        let b = r["block"].as_u64().unwrap().checked_sub(1).expect("one-based source block") as usize;
        let (raw, kind) = match m["content"].as_str() {
            Some(s) if b == 0 => (s, if m["role"] == "user" { "person" } else { "assistant" }),
            _ => {
                let source = &m["content"][b];
                match source["type"].as_str() {
                    Some("tool_result") => (source["content"].as_str().unwrap(), "tool"),
                    Some("text") => {
                        (source["text"].as_str().unwrap(), if m["role"] == "user" { "person" } else { "assistant" })
                    }
                    _ => panic!("invalid source block for quote: {r}"),
                }
            }
        };
        assert_eq!(r["kind"], kind, "{r}");
        let line = r["line"].as_u64().unwrap().checked_sub(1).expect("one-based raw line") as usize;
        assert_eq!(raw.lines().nth(line), Some(r["quote"].as_str().expect("literal quote")), "not the cited line: {r}");
    }
    rows
}

fn at<'a>(rows: &'a [Value], message: &'a str, block: u64) -> impl Iterator<Item = &'a Value> {
    rows.iter().filter(move |r| r["message"] == message && r["block"] == block)
}

fn pair(r: &Value, block: u64, id: &str) {
    assert_eq!(r["call_message"], "m00002", "{r}");
    assert_eq!(r["call_block"], block, "{r}");
    assert_eq!(r["call_id"], id, "{r}");
}

#[test]
fn historical_quotes_are_on_by_default_and_can_be_switched_back_to_keyword_recall() {
    let quote = "cerulean742 spindle916: batch = cobalt";
    let h =
        history(vec![read("a-31", "west/box.csv")], vec![result("a-31", &format!("{quote}\n{}", page("west", 68)))]);
    let question = "Which catalogue reported cerulean742 spindle916?";
    for spec in ["", "relations=0"] {
        let (_, live, resumed) = query_pair(spec, h.clone(), question);
        for r in [&live, &resumed] {
            if spec.is_empty() {
                let rows = records(r);
                assert!(at(&rows, "m00003", 1).any(|row| row["quote"] == quote && row["call_id"] == "a-31"));
            } else {
                assert_eq!(r.relation_quotes, None);
                let tail =
                    r.view.last().unwrap()["content"].as_array().unwrap().last().unwrap()["text"].as_str().unwrap();
                assert!(
                    tail.contains("Recall (lines of earlier messages") && tail.lines().any(|l| l == quote),
                    "{tail}"
                );
                assert!(!tail.contains("Historical evidence ("));
            }
        }
    }
}

#[test]
fn same_text_from_permuted_parallel_results_keeps_both_sources() {
    let quote = "cerulean742 spindle916: batch = \"cobalt\"";
    let body = format!("tag column\n{quote}\n{}", page("common", 68));
    let calls = vec![
        json!({"type":"text", "text":"Reading both catalogues."}),
        read("a-31", "west/box.csv"),
        read("b-72", "east/box.csv"),
    ];
    let h = history(calls, vec![result("b-72", &body), result("a-31", &body)]);
    let rows = query(h, "Which catalogues reported cerulean742 spindle916?");
    for (block, call_block, id) in [(1, 3, "b-72"), (2, 2, "a-31")] {
        let hits: Vec<_> = at(&rows, "m00003", block).filter(|r| r["quote"] == quote).collect();
        assert!(!hits.is_empty(), "missing independent source block {block}: {rows:?}");
        for r in hits {
            assert_eq!(r["line"], 2, "{r}");
            pair(r, call_block, id);
        }
    }
}

#[test]
fn malformed_batches_do_not_borrow_a_call_id_even_when_a_sibling_is_valid() {
    // No-id calls/results really lack the property (not `null` or an empty string). Include a
    // one-to-many case as well as many-to-one: checking just uniqueness in the call list fails.
    let mut no_call_id = read("remove", "unlabelled.csv");
    no_call_id.as_object_mut().unwrap().remove("id");
    let calls =
        vec![read("x", "one.csv"), read("x", "two.csv"), read("y", "three.csv"), no_call_id, read("good", "four.csv")];
    let labels = ["smalt501", "smalt502", "smalt503", "smalt504", "smalt505", "smalt506"];
    let ids = ["good", "x", "y", "y", "missing", "orphan"];
    let mut results: Vec<_> = ids
        .iter()
        .zip(labels)
        .enumerate()
        .map(|(i, (id, label))| result(id, &format!("{label} receipt\n{}", page(&format!("lot-{i}"), 64))))
        .collect();
    results[4].as_object_mut().unwrap().remove("tool_use_id");
    let rows = query(history(calls, results), &format!("Trace these receipts: {}", labels.join(" ")));
    for (i, label) in labels.iter().enumerate() {
        let block = i as u64 + 1;
        let quote = format!("{label} receipt");
        if i == 0 {
            let good = at(&rows, "m00003", block).find(|r| r["quote"] == quote).expect("valid sibling not recovered");
            pair(good, 5, "good");
        } else {
            // Suppressing uncertain evidence is safe. If a quote is kept it must retain its real
            // coordinates; dropping the pair is not permission to relabel the quote itself.
            for r in rows.iter().filter(|r| r["quote"] == quote || (r["message"] == "m00003" && r["block"] == block)) {
                assert_eq!(r["message"], "m00003", "{r}");
                assert_eq!(r["block"], block, "{r}");
                for k in ["call_message", "call_block", "call_id"] {
                    assert!(r.get(k).is_none(), "unconfirmed {k}: {r}");
                }
            }
        }
    }
}

#[test]
fn folded_process_output_is_not_quoted_as_raw_read_evidence() {
    let anchor = "pottery811 beacon254 = attic";
    let body = format!("{}after876 finial409 = shelf C\n", "Catalog scanner retrying the limestone bin\n".repeat(480));
    let raw_anchor = format!("{anchor}\n{}", page("annex", 68));
    let mut h = history(vec![read("tile", "tiles.csv")], vec![result("tile", &raw_anchor)]);
    h.extend([
        msg("assistant", vec![call("process", "bash", json!({"command":"inventory-summary"}))]),
        msg("user", vec![result("process", &body)]),
    ]);
    let rows = query(h, "Locate pottery811 beacon254 and after876 finial409. Which identical lines were shortened? Give the recall reference.");
    assert!(at(&rows, "m00003", 1).any(|r| r["quote"] == anchor), "control read never recovered: {rows:?}");
    // The check in query() also covers the real line *after* the fold: its source is line
    // 481, not line 3 of the condensed display. Either omission or a correct raw quote is safe.
}

#[test]
fn old_read_after_successful_edit_is_not_an_unqualified_current_value() {
    let old = "topaz728 lockbox312: compartment amber";
    let current = "topaz728 lockbox312: compartment indigo";
    let path = "artifacts/lockbox.csv";
    let mut h = history(vec![read("before", path)], vec![result("before", &format!("{old}\n{}", page("before", 68)))]);
    h.extend([
        msg(
            "assistant",
            vec![call("change", "edit_file", json!({"path":path, "old_str":"amber", "new_str":"indigo"}))],
        ),
        msg("user", vec![result("change", "ok")]),
        msg("assistant", vec![read("after", path)]),
        msg("user", vec![result("after", &format!("{current}\n{}", page("after", 68)))]),
    ]);
    let rows = query(h, "Compare readings of receipt topaz728 lockbox312. Which is current?");
    let new = at(&rows, "m00007", 1).find(|r| r["quote"] == current).expect("new read not recovered");
    assert!(!new["status"].as_str().is_some_and(|s| s.contains("superseded")), "newer read predates no edit: {new}");
    for r in rows.iter().filter(|r| r["quote"] == old) {
        assert!(
            r["status"].as_str().is_some_and(|s| s.contains("superseded")),
            "old read presented without qualification: {r}"
        );
    }
}

#[test]
fn a_complete_tail_line_of_a_large_result_keeps_its_raw_coordinates() {
    let quote = "cerulean742 spindle916: batch = cobalt";
    let row = format!("{}\r\n", "received ".repeat(62));
    let body = format!("{}{quote}", row.repeat(512));
    assert!(body.len() > 256 * 1024);
    let h = history(vec![read("lot", "west/box.csv")], vec![result("lot", &body)]);
    let rows = query(h, "Which catalogue reported cerulean742 spindle916?");
    let found = at(&rows, "m00003", 1).find(|r| r["quote"] == quote).expect("missing tail receipt");
    assert_eq!(found["line"], 513);
    pair(found, 1, "lot");
}

#[test]
fn a_receipt_named_only_in_the_middle_of_the_request_is_found() {
    let quote = "cerulean742 spindle916: batch = cobalt";
    let h = history(vec![read("lot", "west/box.csv")], vec![result("lot", &format!("{quote}\n{}", page("west", 68)))]);
    let pad = "catalogue ".repeat(100);
    let rows = query(h, &format!("{pad}cerulean742 spindle916 {pad}"));
    assert!(at(&rows, "m00003", 1).any(|r| r["quote"] == quote && r["call_id"] == "lot"));
}

#[test]
fn visible_hits_do_not_exhaust_the_shortlist_before_a_hidden_direct_match() {
    let quote = "saffron742: original receipt = cobalt";
    let h = history(vec![read("lot", "west/box.csv")], vec![result("lot", &format!("{quote}\n{}", page("west", 68)))]);
    let mut h = h;
    age(&mut h);
    // Newer, equally scoring matches stay visible. They are not evidence to echo back, but
    // neither should 48 of them exhaust the direct shortlist and hide match 49.
    let visible: String = (0..48).map(|n| format!("saffron742 scratch label {n}\n")).collect();
    h.extend([msg("assistant", visible), msg("user", "Continue the museum inventory ledger audit. Trace saffron742.")]);
    let mut c = Curator::new(SPEC).unwrap();
    c.window(WINDOW);
    let reply = c.view(HEAD, &h, 1.0).unwrap();
    let rows = records(&reply);
    let found = at(&rows, "m00003", 1).find(|r| r["quote"] == quote).expect("hidden direct match 49");
    assert_eq!(found["line"], 1);
    pair(found, 1, "lot");
    assert!(rows.len() <= 48);
    assert!(!rows.iter().any(|r| r["quote"].as_str().unwrap().contains("scratch label")));
}
