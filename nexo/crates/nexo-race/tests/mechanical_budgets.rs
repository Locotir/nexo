//! Cold, deterministic checks of Race's estimated output budget (not provider token counts).
use nexo_race::Race;
use serde_json::{json, Value};

const HEAD: u64 = 640;

fn msg(role: &str, content: impl Into<Value>) -> Value {
    json!({"role": role, "content": content.into()})
}
fn cold(window: u64, h: &[Value]) -> nexo_race::Reply {
    let mut c = Race::new("").unwrap();
    c.window(window);
    c.tools(&["read_file".into(), "recall".into()]);
    c.view(HEAD, h, 1.0).unwrap()
}
fn history(body: &str) -> Vec<Value> {
    vec![
        msg("user", "Review archive receipts and report a requested identifier."),
        msg("assistant", json!([{"type":"tool_use","id":"r1","name":"read_file","input":{"path":"archive.csv"}}])),
        msg("user", json!([{"type":"tool_result","tool_use_id":"r1","content":body}])),
        msg("assistant", "Archive review complete."),
        msg("user", "Find cerulean742 spindle916 and report its receipt."),
    ]
}

fn evidence(reply: &nexo_race::Reply) -> Vec<Value> {
    let Some(last) = reply.view.last() else { return vec![] };
    let content = &last["content"];
    let blocks = content.as_array().cloned().unwrap_or_else(|| vec![json!({"text":content})]);
    let mut rows = vec![];
    for block in blocks {
        let Some(text) = block["text"].as_str() else { continue };
        let mut lines = text.lines();
        if lines.any(|line| line.starts_with("Historical evidence")) {
            rows.extend(
                lines.filter(|line| !line.trim().is_empty()).map(|line| serde_json::from_str::<Value>(line).unwrap()),
            );
        }
    }
    rows
}

fn assert_budget(reply: &nexo_race::Reply, window: u64) {
    assert!(reply.tokens >= 0, "negative estimated tokens: {}", reply.tokens);
    assert!(
        reply.tokens + HEAD as i64 <= window as i64,
        "estimated Race budget {} + head {} exceeds window {}",
        reply.tokens,
        HEAD,
        window
    );
}

#[test]
fn cold_estimated_budget_respects_multiple_windows_and_headroom() {
    let body = format!(
        "cerulean742 spindle916: receipt = cobalt\n{}",
        (0..900).map(|i| format!("archive row {i}: {}\n", "filler".repeat(8))).collect::<String>()
    );
    let h = history(&body);
    for window in [1_600, 3_200, 8_000, 20_000] {
        let reply = cold(window, &h);
        assert_budget(&reply, window);
        // Recovery is not promised at every budget; only validate a row if emitted.
        for row in evidence(&reply) {
            let line = row["quote"].as_str().expect("quote is text");
            assert!(body.lines().any(|source| source == line), "citation must remain a literal source line: {row}");
        }
    }
}

#[test]
fn cold_estimator_handles_utf8_and_crlf_content_without_invalid_citations() {
    let quote = "cerulean742 spindle916: recepción_日本_é 🧭 = cobalt";
    let body = format!(
        "前置き\r\n{quote}\r\n{}",
        (0..240).map(|i| format!("archivo {i}: café 日本語 🪁\r\n")).collect::<String>()
    );
    let h = history(&body);
    for window in [1_600, 4_000, 12_000] {
        let reply = cold(window, &h);
        assert_budget(&reply, window);
        for row in evidence(&reply) {
            assert!(
                body.lines().any(|source| source == row["quote"].as_str().unwrap()),
                "UTF-8 citation is not literal: {row}"
            );
        }
    }
}

#[test]
fn cold_saturation_with_repeated_padding_stays_within_estimated_budget() {
    let target = "cerulean742 spindle916: receipt = cobalt";
    let repeated = "cerulean742 spindle916 repeated filler archive row ".repeat(3_000);
    let body = format!("{target}\n{repeated}");
    let h = history(&body);
    for window in [1_600, 2_400, 4_000] {
        let reply = cold(window, &h);
        assert_budget(&reply, window);
        let rows = evidence(&reply);
        assert!(rows.len() <= 48, "unexpectedly unbounded evidence output: {}", rows.len());
        for row in rows {
            assert!(
                body.lines().any(|source| source == row["quote"].as_str().unwrap()),
                "padding must not fabricate citations: {row}"
            );
        }
    }
}
