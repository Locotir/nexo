//! Cold, deterministic contracts for saved curator states. Every acceptance check uses public headers,
//! restore errors, or literal evidence sourced from the input history; no model is called.
use nexo_race::snapshot::{history_hash, message_hash, Header};
use nexo_race::{Race, Reply};
use serde_json::{json, Value};

const HEAD: u64 = 640;

fn msg(role: &str, content: impl Into<Value>) -> Value {
    json!({"role": role, "content": content.into()})
}

fn fixture(seed: usize) -> Vec<Value> {
    let quote = format!("amethyst{seed} vault-key = {seed}cobalt");
    let rows = (0..240).map(|n| format!("archive/{seed}/record-{n},code_{}\n", n * 31)).collect::<String>();
    let mut h = vec![
        msg("user", "Audit the archive receipts and retain the vault key."),
        msg(
            "assistant",
            vec![json!({"type":"tool_use","id":"read","name":"read_file","input":{"path":"archive/receipt.csv"}})],
        ),
        msg("user", vec![json!({"type":"tool_result","tool_use_id":"read","content":format!("{quote}\n{rows}")})]),
    ];
    // Historical evidence excludes intact visible sources. Evict the target with
    // distinct pages before saving, so continuation really tests retrieval.
    for n in 0..16 {
        let id = format!("shelf-{n}");
        let rows = (0..150)
            .map(|i| format!("table/{id}/{i:03}.csv,article_{i},bin_{},quantity_{}\n", i % 17, i * 53))
            .collect::<String>();
        h.extend([
            msg("assistant", vec![json!({"type":"tool_use", "id":id, "name":"read_file", "input":{"path":format!("shelves/{id}.csv")}})]),
            msg("user", vec![json!({"type":"tool_result", "tool_use_id":id, "content":rows})]),
        ]);
    }
    h
}

fn curator(window: u64) -> Race {
    let mut c = Race::new("").unwrap();
    c.window(window);
    c.tools(&["read_file".into(), "edit_file".into(), "edit".into(), "recall".into()]);
    c
}

fn hashes(history: &[Value]) -> Vec<u64> {
    history.iter().map(message_hash).collect()
}

fn save(c: &Race, h: &[Value]) -> Vec<u8> {
    let hs = hashes(h);
    c.save(1, history_hash(&hs)).encode()
}

fn query(reply: &Reply) -> Vec<Value> {
    let content = &reply.view.last().unwrap()["content"];
    let blocks = content.as_array().cloned().unwrap_or_else(|| vec![json!({"type":"text","text":content})]);
    let mut rows = Vec::new();
    for block in blocks {
        let Some(text) = block["text"].as_str() else { continue };
        let mut lines = text.lines();
        if lines.any(|l| l.starts_with("Historical evidence")) {
            rows.extend(lines.filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str::<Value>(l).unwrap()));
        }
    }
    assert!(!rows.is_empty(), "missing Historical evidence section");
    assert_eq!(reply.relation_quotes, Some(rows.len()));
    rows
}

fn validate(rows: &[Value], h: &[Value]) {
    for row in rows {
        let mi = row["message"].as_str().unwrap().strip_prefix('m').unwrap().parse::<usize>().unwrap() - 1;
        assert!(mi < h.len() - 1, "evidence is not historical: {row}");
        assert_eq!(row["message"], format!("m{:05}", mi + 1));
        assert_eq!(row["kind"], "tool");
        let bi = row["block"].as_u64().unwrap() as usize - 1;
        let b = &h[mi]["content"][bi];
        assert_eq!(b["type"], "tool_result");
        assert_eq!(row["call_id"], b["tool_use_id"]);
        assert_eq!(row["call_message"], format!("m{:05}", mi));
        assert_eq!(row["call_id"], h[mi - 1]["content"][row["call_block"].as_u64().unwrap() as usize - 1]["id"]);
        let raw = b["content"].as_str().unwrap();
        assert_eq!(raw.lines().nth(row["line"].as_u64().unwrap() as usize - 1), row["quote"].as_str());
    }
}

fn resume(h: &[Value], c: &Race, bytes: &[u8], seed: usize) {
    let hs = hashes(h);
    let (mut restored, header) = Race::restore("", bytes, h, &hs).unwrap();
    assert_eq!(header.covered as usize, h.len());
    let mut continued = h.to_vec();
    continued.extend([
        msg("assistant", "The archive scan is complete."),
        msg("user", format!("Continue the archive audit. Find amethyst{seed} vault-key.")),
    ]);
    let restored_reply = restored.view(HEAD, &continued, 2.0).unwrap();
    let live_reply = c.clone().view(HEAD, &continued, 2.0).unwrap();
    let out = query(&restored_reply);
    let live_out = query(&live_reply);
    validate(&out, &continued);
    validate(&live_out, &continued);
    assert_eq!(out, live_out, "restored/live continuation diverged for seed {seed}");
    let expected = format!("amethyst{seed} vault-key = {seed}cobalt");
    assert!(
        out.iter().any(|r| r["quote"] == expected),
        "restored continuation lost literal target {expected:?}; live={live_out:?}, restored={out:?}"
    );
}

#[test]
fn restored_snapshot_continues_and_recovers_source_grounded_receipt() {
    for seed in [11, 37, 103] {
        let h = fixture(seed);
        let mut c = curator(20_000);
        c.view(HEAD, &h, 1.0).unwrap();
        let bytes = save(&c, &h);
        resume(&h, &c, &bytes, seed);
    }
}

#[test]
fn truncated_snapshot_is_rejected() {
    for seed in [11, 37, 103] {
        let h = fixture(seed);
        let mut c = curator(20_000);
        c.view(HEAD, &h, 1.0).unwrap();
        let mut bytes = save(&c, &h);
        bytes.pop();
        assert!(Race::restore("", &bytes, &h, &hashes(&h)).is_err(), "accepted truncated snapshot, seed {seed}");
    }
}

#[test]
fn changed_same_length_message_is_rejected_by_history_oracle() {
    for seed in [11, 37, 103] {
        let h = fixture(seed);
        let mut c = curator(20_000);
        c.view(HEAD, &h, 1.0).unwrap();
        let bytes = save(&c, &h);
        let mut changed = h.clone();
        let old = changed[2]["content"][0]["content"].as_str().unwrap().to_owned();
        let mut replacement = old.clone();
        replacement.replace_range(0..1, if &old[0..1] == "a" { "b" } else { "a" });
        assert_eq!(old.len(), replacement.len());
        changed[2]["content"][0]["content"] = json!(replacement);
        assert_eq!(changed.len(), h.len());
        assert!(
            Race::restore("", &bytes, &changed, &hashes(&changed)).is_err(),
            "accepted same-size content mutation, seed {seed}"
        );
    }
}

#[test]
fn incompatible_configuration_and_damaged_payload_are_rejected() {
    for seed in [11, 37, 103] {
        let h = fixture(seed);
        let mut c = curator(20_000);
        c.view(HEAD, &h, 1.0).unwrap();
        let bytes = save(&c, &h);
        assert!(
            Race::restore("different explicit curator settings", &bytes, &h, &hashes(&h)).is_err(),
            "accepted incompatible config, seed {seed}"
        );

        let mut corrupt = bytes.clone();
        *corrupt.last_mut().unwrap() ^= 0x40;
        assert!(Race::restore("", &corrupt, &h, &hashes(&h)).is_err(), "accepted corrupt payload, seed {seed}");
        let header = Header::read(&bytes).unwrap();
        assert_eq!(header.covered as usize, h.len());
    }
}
