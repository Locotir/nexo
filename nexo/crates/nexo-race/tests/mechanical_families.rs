//! Deterministic public-API regressions. Oracles are raw messages and literal receipts, not scores.
//! Seeds vary source position, batch ordering and line endings; the families test different contracts.
use nexo_race::snapshot::{history_hash, message_hash};
use nexo_race::{Race, Reply};
use serde_json::{json, Value};

const HEAD: u64 = 640;
const SEEDS: [usize; 3] = [7, 29, 101];

fn msg(role: &str, content: impl Into<Value>) -> Value {
    json!({"role": role, "content": content.into()})
}

fn call(id: &str, tool: &str, input: Value) -> Value {
    json!({"type":"tool_use", "id":id, "name":tool, "input":input})
}

fn result(id: &str, content: impl Into<Value>) -> Value {
    json!({"type":"tool_result", "tool_use_id":id, "content":content.into()})
}

fn page(seed: usize) -> String {
    (0..150).map(|n| format!("inventory/{seed}/shelf-{n}.csv,article_{n},quantity_{}\n", n * 53)).collect()
}

fn opening(calls: Vec<Value>, results: Vec<Value>) -> Vec<Value> {
    vec![msg("user", "Audit the museum inventory ledger."), msg("assistant", calls), msg("user", results)]
}

fn read(id: &str, path: &str) -> Value {
    call(id, "read_file", json!({"path":path}))
}

fn age(h: &mut Vec<Value>, seed: usize) {
    for n in 0..16 {
        let id = format!("shelf-{seed}-{n}");
        h.extend([
            msg("assistant", vec![read(&id, &format!("shelves/{seed}/{n}.csv"))]),
            msg("user", vec![result(&id, page(seed + n))]),
        ]);
    }
}

fn curator(window: u64) -> Race {
    let mut c = Race::new("").unwrap();
    c.window(window);
    c.tools(&["read_file".into(), "edit_file".into(), "edit".into(), "recall".into()]);
    c
}

fn rows(reply: &Reply, window: u64) -> Vec<Value> {
    assert!(
        reply.tokens >= 0 && reply.tokens + HEAD as i64 <= window as i64,
        "estimated budget: {} + {HEAD} > {window}",
        reply.tokens
    );
    let mut out = Vec::new();
    let content = &reply.view.last().unwrap()["content"];
    let blocks = content.as_array().cloned().unwrap_or_else(|| vec![json!({"type":"text", "text":content})]);
    for block in &blocks {
        let Some(text) = block["text"].as_str() else { continue };
        let mut lines = text.lines();
        if lines.any(|l| l.starts_with("Historical evidence")) {
            out.extend(lines.filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str::<Value>(l).unwrap()));
        }
    }
    assert_eq!(reply.relation_quotes.unwrap_or(0), out.len());
    out
}

/// Independently resolve every quote, including non-target distractors and call/result joins.
fn validate(rows: &[Value], h: &[Value]) {
    for row in rows {
        let name = row["message"].as_str().unwrap();
        let mi = name.strip_prefix('m').unwrap().parse::<usize>().unwrap() - 1;
        assert!(mi < h.len() - 1, "quote must precede current request: {row}");
        assert_eq!(name, format!("m{:05}", mi + 1));
        let bi = row["block"].as_u64().unwrap() as usize - 1;
        let m = &h[mi];
        let b = &m["content"][bi];
        let raw = m["content"]
            .as_str()
            .or_else(|| b["text"].as_str())
            .or_else(|| b["content"].as_str())
            .or_else(|| b["content"][0]["text"].as_str())
            .unwrap();
        let li = row["line"].as_u64().unwrap() as usize - 1;
        assert_eq!(raw.lines().nth(li), row["quote"].as_str(), "not a literal source line: {row}");
        let kind = if b["type"] == "tool_result" {
            "tool"
        } else if m["role"] == "user" {
            "person"
        } else {
            "assistant"
        };
        assert_eq!(row["kind"], kind);
        if row.get("call_id").is_some() {
            assert_eq!(kind, "tool");
            let ci = row["call_message"].as_str().unwrap().strip_prefix('m').unwrap().parse::<usize>().unwrap() - 1;
            let cb = row["call_block"].as_u64().unwrap() as usize - 1;
            assert_eq!(ci + 1, mi);
            assert_eq!(h[ci]["role"], "assistant");
            assert_eq!(h[ci]["content"][cb]["type"], "tool_use");
            assert_eq!(h[ci]["content"][cb]["id"], b["tool_use_id"]);
            assert_eq!(row["call_id"], b["tool_use_id"]);
        }
    }
}

/// A real pre-query snapshot, an incremental live request, and a fresh cold reconstruction.
fn query(mut h: Vec<Value>, seed: usize, question: &str, window: u64, hidden: &str) -> Vec<Value> {
    age(&mut h, seed);
    let mut c = curator(window);
    let before = c.view(HEAD, &h, 1.0).unwrap();
    assert!(
        !serde_json::to_string(&before.view).unwrap().contains(hidden),
        "positive control not evicted, seed {seed}"
    );
    let hashes: Vec<_> = h.iter().map(message_hash).collect();
    let bytes = c.save(1, history_hash(&hashes)).encode();
    let (mut restored, _) = Race::restore("", &bytes, &h, &hashes).unwrap();
    h.extend([
        msg("assistant", "Finished that inventory pass."),
        msg("user", format!("Continue the museum inventory ledger audit. {question}")),
    ]);
    let live = rows(&c.view(HEAD, &h, 2.0).unwrap(), window);
    let resumed = rows(&restored.view(HEAD, &h, 2.0).unwrap(), window);
    let cold = rows(&curator(window).view(HEAD, &h, 2.0).unwrap(), window);
    validate(&live, &h);
    validate(&resumed, &h);
    validate(&cold, &h);
    assert_eq!(live, resumed, "snapshot divergence, seed {seed}");
    // Cold rankings need not equal incremental rankings, but both must satisfy the oracle.
    assert!(live.iter().any(|r| r["quote"] == hidden), "live target lost, seed {seed}: {live:?}");
    assert!(cold.iter().any(|r| r["quote"] == hidden), "cold target lost, seed {seed}: {cold:?}");
    live
}

#[test]
fn unicode_crlf_and_middle_line_recovery_has_literal_provenance() {
    for seed in SEEDS {
        let quote = format!("cerulean{seed} spindle{seed}: ubicación = almacén_日本_é");
        let prefix = (0..seed).map(|n| format!("intake row {n}\r\n")).collect::<String>();
        let body = format!("{prefix}{quote}\r\n{}", page(seed));
        let h = opening(vec![read("receipt", "museum/receipts.csv")], vec![result("receipt", body)]);
        let r = query(h, seed, &format!("Locate cerulean{seed} spindle{seed}."), 20_000, &quote);
        let hit = r.iter().find(|r| r["quote"] == quote).unwrap();
        assert_eq!(hit["message"], "m00003");
        assert_eq!(hit["line"], seed + 1);
        assert_eq!(hit["call_id"], "receipt");
    }
}

#[test]
fn nested_text_results_and_permuted_parallel_batches_keep_their_own_paths() {
    for seed in SEEDS {
        let quote = format!("saffron{seed} receipt = cobalt");
        let body = format!("{quote}\n{}", page(seed));
        let mut results = vec![result("west", json!([{"type":"text", "text":body}])), result("east", body.clone())];
        if seed % 3 != 0 {
            results.reverse();
        }
        let h = opening(vec![read("west", "west/box.csv"), read("east", "east/box.csv")], results);
        let r = query(h, seed, &format!("Compare all saffron{seed} receipts."), 20_000, &quote);
        for (id, call_block) in [("west", 1), ("east", 2)] {
            let hit = r.iter().find(|r| r["quote"] == quote && r["call_id"] == id).expect("parallel source lost");
            assert_eq!(hit["call_block"], call_block);
        }
    }
}

#[test]
fn unsuccessful_tool_output_is_not_promoted_to_historical_evidence() {
    for seed in SEEDS {
        let good = format!("topaz{seed} receipt = validated");
        let bad = format!("topaz{seed} receipt = untrusted_failure");
        let mut failed = result("failed", format!("{bad}\n{}", page(seed + 1)));
        failed["is_error"] = json!(true);
        let h = opening(
            vec![read("good", "good.csv"), read("failed", "failed.csv")],
            vec![result("good", format!("{good}\n{}", page(seed))), failed],
        );
        let r = query(h, seed, &format!("Locate topaz{seed} receipt."), 20_000, &good);
        assert!(!r.iter().any(|r| r["quote"] == bad || r["call_id"] == "failed"));
    }
}

#[test]
fn failed_edits_conservatively_age_only_reads_of_the_target_file() {
    for seed in SEEDS {
        for target in ["artifacts/box.csv", "other/box.csv"] {
            let quote = format!("jasper{seed} lockbox = amber");
            let mut h = opening(
                vec![read("before", "artifacts/box.csv")],
                vec![result("before", format!("{quote}\n{}", page(seed)))],
            );
            // An error is not a rollback receipt: a check may fail after writing, or an
            // external change may invalidate the prior read. Do not infer freshness from it.
            let mut failed = result("edit", "post-edit check failed");
            failed["is_error"] = json!(true);
            h.extend([
                msg(
                    "assistant",
                    vec![call("edit", "edit", json!({"path":target, "old_string":"amber", "new_string":"indigo"}))],
                ),
                msg("user", vec![failed]),
            ]);
            let r = query(h, seed, &format!("Locate jasper{seed} lockbox."), 20_000, &quote);
            let hits: Vec<_> = r.iter().filter(|r| r["quote"] == quote && r["call_id"] == "before").collect();
            assert!(!hits.is_empty(), "successful read lost after failed edit of {target}, seed {seed}");
            for hit in hits {
                if target == "artifacts/box.csv" {
                    assert_eq!(hit["status"], "possibly superseded by a later edit", "unqualified old read: {hit}");
                } else {
                    assert!(
                        !hit["status"].as_str().is_some_and(|s| s.contains("superseded")),
                        "failed edit of a different directory's file aged read: {hit}"
                    );
                }
            }
        }
    }
}

#[test]
fn successful_edits_do_not_age_a_same_basename_in_another_directory() {
    for seed in SEEDS {
        let quote = format!("malachite{seed} lockbox = amber");
        let mut h = opening(
            vec![read("west", "west/box.csv"), read("east", "east/box.csv")],
            vec![
                result("west", format!("{quote}\n{}", page(seed))),
                result("east", format!("{quote}\n{}", page(seed + 1))),
            ],
        );
        h.extend([
            msg(
                "assistant",
                vec![call("edit", "edit_file", json!({"path":"west/box.csv", "old_str":"amber", "new_str":"indigo"}))],
            ),
            msg("user", vec![result("edit", "ok")]),
        ]);
        let r = query(h, seed, &format!("Compare malachite{seed} lockbox readings."), 20_000, &quote);
        let east = r.iter().find(|r| r["quote"] == quote && r["call_id"] == "east").expect("unmodified sibling lost");
        assert!(!east["status"].as_str().is_some_and(|s| s.contains("superseded")));
        for west in r.iter().filter(|r| r["quote"] == quote && r["call_id"] == "west") {
            assert!(
                west["status"].as_str().is_some_and(|s| s.contains("superseded")),
                "stale source unqualified: {west}"
            );
        }
    }
}

#[test]
fn recovery_respects_multiple_estimated_context_budgets() {
    for window in [12_000, 20_000, 32_000] {
        let quote = "turquoise917 spindle418: receipt = violet";
        let h =
            opening(vec![read("receipt", "receipts.csv")], vec![result("receipt", format!("{quote}\n{}", page(7)))]);
        query(h, 7, "Locate turquoise917 spindle418.", window, quote);
    }
}

#[test]
fn reset_and_independent_sessions_never_recover_another_sessions_receipt() {
    for seed in SEEDS {
        let secret = format!("obsidian{seed} receipt = private_vault_{seed}");
        let mut h = opening(
            vec![read("private", "private/box.csv")],
            vec![result("private", format!("{secret}\n{}", page(seed)))],
        );
        age(&mut h, seed);
        let mut used = curator(20_000);
        used.view(HEAD, &h, 1.0).unwrap();
        used.reset();
        let clean = vec![msg("user", format!("Audit the museum inventory ledger. Locate obsidian{seed} receipt."))];
        for mut c in [used, curator(20_000)] {
            let reply = c.view(HEAD, &clean, 2.0).unwrap();
            assert!(rows(&reply, 20_000).is_empty());
            assert!(!serde_json::to_string(&reply.view).unwrap().contains(&format!("private_vault_{seed}")));
        }
    }
}

#[test]
fn snapshot_rejects_mutated_source_even_if_message_count_matches() {
    for seed in SEEDS {
        let mut h = opening(vec![read("receipt", "box.csv")], vec![result("receipt", page(seed))]);
        let mut c = curator(20_000);
        c.view(HEAD, &h, 1.0).unwrap();
        let hashes: Vec<_> = h.iter().map(message_hash).collect();
        let bytes = c.save(1, history_hash(&hashes)).encode();
        h[2]["content"][0]["content"] = json!(page(seed + 1));
        let changed: Vec<_> = h.iter().map(message_hash).collect();
        assert!(Race::restore("", &bytes, &h, &changed).is_err(), "changed source accepted, seed {seed}");
    }
}
