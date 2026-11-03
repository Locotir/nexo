//! Golden replay of the briefing's selection: on the recorded conversations and
//! tasks of `tests/fixtures/brief` it gives the recorded spans, labels, texts and
//! asks; scores and relevances to rounding.

use std::path::Path;

use nexo_agent::brief::curated::select;
use serde_json::Value;

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn the_selection_matches_the_reference() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/brief");
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    let (mut cases, mut spans, mut asks) = (0, 0, 0);
    for f in files.iter().filter(|f| f.extension().is_some_and(|e| e == "json")) {
        let conv: Value = serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap();
        let msgs = conv["messages"].as_array().unwrap();
        for c in conv["cases"].as_array().unwrap() {
            let task = c["task"].as_str().unwrap();
            let upto = c.get("upto").and_then(Value::as_u64).map_or(msgs.len(), |k| k as usize);
            let got = select(task, &msgs[..upto], c["budget"].as_i64().unwrap());
            let want = &c["reply"];
            let at = format!("{} / {task:?} / budget {} / upto {upto}", f.display(), c["budget"]);
            let w_spans = want["spans"].as_array().unwrap();
            let key = |s: &Value| {
                (
                    s["msg"].as_u64().unwrap() as usize,
                    s["block"].as_u64().unwrap() as usize,
                    s["start"].as_u64().unwrap() as usize,
                    s["end"].as_u64().unwrap() as usize,
                    s["label"].as_str().unwrap().to_string(),
                    s["text"].as_str().unwrap().to_string(),
                    s["tokens"].as_u64().unwrap() as usize,
                )
            };
            let want_keys: Vec<_> = w_spans.iter().map(key).collect();
            let got_keys: Vec<_> = got
                .spans
                .iter()
                .map(|s| (s.msg, s.block, s.start, s.end, s.label.clone(), s.text.clone(), s.tokens))
                .collect();
            assert_eq!(got_keys, want_keys, "spans of {at}");
            for (g, w) in got.spans.iter().zip(w_spans) {
                let w = w["score"].as_f64().unwrap();
                assert!(close(g.score, w, 1.5e-4), "score {} != {w} in {at}", g.score);
            }
            let w_asks = want["asks"].as_array().unwrap();
            assert_eq!(got.asks.len(), w_asks.len(), "asks of {at}: {:?}", got.asks);
            for (g, w) in got.asks.iter().zip(w_asks) {
                assert_eq!(g.turn as u64, w["turn"].as_u64().unwrap(), "{at}");
                assert_eq!(g.ask, w["ask"].as_str().unwrap(), "{at}");
                let w = w["relevance"].as_f64().unwrap();
                assert!(close(g.relevance, w, 1.5e-3), "relevance {} != {w} in {at}", g.relevance);
            }
            cases += 1;
            spans += got.spans.len();
            asks += got.asks.len();
        }
    }
    assert!(cases >= 40 && spans >= 300 && asks > 0, "{cases} cases, {spans} spans, {asks} asks");
}
