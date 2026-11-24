//! Cold, deterministic public-API tests for opt-in local segment expansion.
//! These test the current experimental behavior; they do not claim H-Net implementation.
use nexo_curator::{Curator, Reply};
use serde_json::{json, Value};

const HEAD: u64 = 640;
const SEEDS: [usize; 3] = [7, 29, 101];

fn rows(reply: &Reply) -> Vec<Value> {
    let content = &reply.view.last().unwrap()["content"];
    let blocks = content.as_array().cloned().unwrap_or_else(|| vec![json!({"text":content})]);
    let mut out = Vec::new();
    for block in blocks {
        let Some(text) = block["text"].as_str() else { continue };
        let mut lines = text.lines();
        if lines.any(|l| l.starts_with("Historical evidence")) {
            out.extend(lines.filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str::<Value>(l).unwrap()));
        }
    }
    out
}

fn history(seed: usize, tool: &str, path: &str, body: String) -> Vec<Value> {
    let mut h = vec![
        json!({"role":"user","content":"Audit the museum inventory ledger."}),
        json!({"role":"assistant","content":[{"type":"tool_use","id":"receipt","name":tool,"input":{"path":path}}]}),
        json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"receipt","content":body}]}),
    ];
    for n in 0..16 {
        let id = format!("noise-{seed}-{n}");
        let text: String =
            (0..150).map(|i| format!("archive/{seed}/{n}/{i:03}.csv,item_{i},quantity_{}\n", i * 53)).collect();
        h.push(json!({"role":"assistant","content":[{"type":"tool_use","id":id,"name":"read_file","input":{"path":format!("archive/{seed}/{n}.csv")}}]}));
        h.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":text}]}));
    }
    h
}

fn cold(spec: &str, h: &[Value], question: &str) -> Vec<Value> {
    let mut h = h.to_vec();
    h.extend([
        json!({"role":"assistant","content":"Finished that inventory pass."}),
        json!({"role":"user","content":question}),
    ]);
    let mut c = Curator::new(spec).unwrap();
    c.window(20_000);
    c.tools(&["read_file".into(), "recall".into()]);
    rows(&c.view(HEAD, &h, 1.0).unwrap())
}

fn assert_literal_provenance(rows: &[Value], h: &[Value]) {
    for r in rows {
        let mi = r["message"].as_str().unwrap().trim_start_matches('m').parse::<usize>().unwrap() - 1;
        let bi = r["block"].as_u64().unwrap() as usize - 1;
        let text = h[mi]["content"][bi]["content"].as_str().or_else(|| h[mi]["content"][bi]["text"].as_str()).unwrap();
        let li = r["line"].as_u64().unwrap() as usize - 1;
        assert_eq!(text.lines().nth(li), r["quote"].as_str(), "nonliteral source: {r}");
        assert_eq!(r["kind"], "tool");
        assert_eq!(r["message"], "m00003");
        assert_eq!(r["call_message"], "m00002");
        assert_eq!(r["call_block"], 1);
        assert_eq!(r["call_id"], "receipt");
    }
}

#[test]
fn cold_segments_expand_only_within_unique_read_result_unit_and_stop_at_blank() {
    for seed in SEEDS {
        let anchor = format!("cerulean{seed} spindle{seed} = cobalt");
        let body = format!(
            "irrelevant lead\n{anchor}\nnearby literal {seed}\n\nfar distractor {seed}\n{}",
            (0..150).map(|i| format!("inventory/{i},item_{i}\n")).collect::<String>()
        );
        let h = history(seed, "read_file", "west/ledger.csv", body.clone());
        let plain = cold("relations=1", &h, &format!("Locate spindle{seed}."));
        let segmented = cold("segments=1", &h, &format!("Locate spindle{seed}."));
        assert!(!plain.iter().any(|r| r["quote"] == format!("nearby literal {seed}")));
        assert!(segmented.iter().any(|r| r["quote"] == format!("nearby literal {seed}")), "seed {seed}: {segmented:?}");
        assert!(!segmented.iter().any(|r| r["quote"] == format!("far distractor {seed}")));
        assert_literal_provenance(&segmented, &h);
        // Repeated cold reconstruction must yield byte-equivalent JSON rows.
        assert_eq!(segmented, cold("segments=1", &h, &format!("Locate spindle{seed}.")));
    }
}

#[test]
fn segments_do_not_expand_nonread_units_or_ambiguous_result_pairs() {
    for seed in SEEDS {
        let anchor = format!("cerulean{seed} spindle{seed} = cobalt");
        let body = format!(
            "{anchor}\nneighbor evidence {seed}\n{}",
            (0..150).map(|i| format!("row/{i},item_{i}\n")).collect::<String>()
        );
        let nonread = history(seed, "write_file", "west/ledger.csv", body.clone());
        let r = cold("segments=1", &nonread, &format!("Locate spindle{seed}."));
        assert!(!r.iter().any(|x| x["quote"] == format!("neighbor evidence {seed}")));

        let mut ambiguous = history(seed, "read_file", "west/ledger.csv", body);
        ambiguous[1]["content"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"tool_use","id":"receipt","name":"read_file","input":{"path":"east/ledger.csv"}}));
        let r = cold("segments=1", &ambiguous, &format!("Locate spindle{seed}."));
        assert!(!r.iter().any(|x| x["quote"] == format!("neighbor evidence {seed}")));
    }
}
