//! Property: a parser never panics on a payload a provider or a proxy could send, and the
//! recorded streams give the same text and tool calls however the bytes were cut.

use std::panic::{catch_unwind, AssertUnwindSafe};

use nexo_core::{FinishReason, Part, StreamEvent};
use serde_json::{json, Value};

use crate::common::{collect, drive, fixture, parser, payloads, sse_events, Rng, PROTOCOLS};

fn expected(protocol: &str) -> (&'static str, &'static str) {
    // (text, reasoning) of each recorded stream; every stream ends in the same `read` call
    match protocol {
        "anthropic" | "openai-chat" => ("I'll read it.", "The user wants the file."),
        "openai-responses" => ("I'll read it.", "**Reading the file**"),
        "gemini" => ("I'll read it.", "**Reading the file**\n\nThe user wants the file."),
        _ => unreachable!(),
    }
}

fn assert_expected(label: &str, protocol: &str, data: &[String]) {
    let c = collect(drive(protocol, data));
    let m = c.message();
    let (text, reasoning) = expected(protocol);
    assert_eq!(m.text(), text, "{label}: text");
    // the reasoning that has text: Gemini puts its thought signature on the call part, which comes
    // back as a reasoning part with a signature and no text (opaque data for the next request)
    let shown: Vec<&str> = m
        .content
        .iter()
        .filter_map(|p| match p {
            Part::Reasoning { text, .. } if !text.is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(shown.join("\n"), reasoning, "{label}: reasoning");
    let calls: Vec<_> = m.tool_calls().map(|(_, n, a)| (n.to_string(), a.clone())).collect();
    assert_eq!(calls, vec![("read".to_string(), json!({"path": "src/main.rs"}))], "{label}: tool calls");
    assert!(c.error.is_none(), "{label}: no error: {:?}", c.error);
}

#[test]
fn recorded_streams_give_the_expected_text_reasoning_and_call_at_every_cut() {
    for protocol in PROTOCOLS {
        let bytes = fixture(protocol).as_bytes();
        let whole = payloads(protocol);
        assert_expected(&format!("{protocol} whole"), protocol, &whole);
        for cut in 1..bytes.len() {
            let data: Vec<String> = sse_events(bytes, &[cut]).into_iter().map(|e| e.data).collect();
            assert_eq!(data, whole, "{protocol}: events at cut {cut}");
        }
        let mut rng = Rng::new(31);
        for round in 0..30 {
            let mut cuts: Vec<usize> = (0..1 + rng.below(8)).map(|_| 1 + rng.below(bytes.len() - 1)).collect();
            cuts.sort_unstable();
            let data: Vec<String> = sse_events(bytes, &cuts).into_iter().map(|e| e.data).collect();
            assert_expected(&format!("{protocol} round {round} cuts {cuts:?}"), protocol, &data);
        }
    }
}

// ---- the mutator ----

#[derive(Clone, Debug)]
enum Seg {
    Key(String),
    Idx(usize),
}

fn paths(v: &Value, here: &mut Vec<Seg>, out: &mut Vec<Vec<Seg>>) {
    out.push(here.clone());
    match v {
        Value::Object(m) => {
            for (k, c) in m {
                here.push(Seg::Key(k.clone()));
                paths(c, here, out);
                here.pop();
            }
        }
        Value::Array(a) => {
            for (i, c) in a.iter().enumerate() {
                here.push(Seg::Idx(i));
                paths(c, here, out);
                here.pop();
            }
        }
        _ => {}
    }
}

fn at_mut<'a>(v: &'a mut Value, path: &[Seg]) -> Option<&'a mut Value> {
    let mut cur = v;
    for s in path {
        cur = match s {
            Seg::Key(k) => cur.get_mut(k.as_str())?,
            Seg::Idx(i) => cur.get_mut(*i)?,
        };
    }
    Some(cur)
}

const MARK: &str = "@@MARK@@";

enum Mutation {
    Drop,
    Set(Value),
    /// Raw JSON text put in place of the value (what `serde_json::Value` cannot hold).
    Text(String),
}

fn mutations(rng: &mut Rng) -> Vec<(&'static str, Mutation)> {
    let deep = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
    let deep_obj = |n: usize| format!("{}1{}", "{\"a\":".repeat(n), "}".repeat(n));
    let escapes = [
        r#""\ud800""#,
        r#""\udc00""#,
        r#""\u12""#,
        r#""\ud800A""#,
        r#""\uZZZZ""#,
        r#""\""#,
        r#""a\x""#,
        r#""\udc00\ud800""#,
    ];
    vec![
        ("drop", Mutation::Drop),
        ("null", Mutation::Set(Value::Null)),
        ("true", Mutation::Set(json!(true))),
        ("minus one", Mutation::Set(json!(-1))),
        ("zero", Mutation::Set(json!(0))),
        ("u64 max", Mutation::Text("18446744073709551615".into())),
        ("past u64", Mutation::Text("18446744073709551616123".into())),
        ("2^32", Mutation::Text("4294967296".into())),
        ("i64 min", Mutation::Text("-9223372036854775808".into())),
        ("f64 max", Mutation::Text("1.7976931348623157e308".into())),
        ("past f64", Mutation::Text("1e999".into())),
        ("fraction", Mutation::Set(json!(0.5))),
        ("empty string", Mutation::Set(json!(""))),
        ("string", Mutation::Set(json!("x"))),
        ("long string", Mutation::Set(json!("\u{65e5}".repeat(2000)))),
        ("empty array", Mutation::Set(json!([]))),
        ("empty object", Mutation::Set(json!({}))),
        ("array of nulls", Mutation::Set(json!([null, null]))),
        ("deep arrays 100", Mutation::Text(deep(100))),
        ("deep arrays 200", Mutation::Text(deep(200))),
        ("deep objects 100", Mutation::Text(deep_obj(100))),
        ("bad escape", Mutation::Text((*rng.pick(&escapes)).to_string())),
        ("json in string", Mutation::Set(json!("{\"a\":"))),
    ]
}

fn apply(base: &Value, path: &[Seg], m: &Mutation) -> Option<String> {
    let mut v = base.clone();
    if path.is_empty() {
        return match m {
            Mutation::Drop => None,
            Mutation::Set(x) => Some(x.to_string()),
            Mutation::Text(t) => Some(t.clone()),
        };
    }
    let (last, parent) = path.split_last()?;
    let p = at_mut(&mut v, parent)?;
    match m {
        Mutation::Drop => match (last, p) {
            (Seg::Key(k), Value::Object(o)) => {
                o.remove(k);
            }
            (Seg::Idx(i), Value::Array(a)) => {
                a.remove(*i);
            }
            _ => return None,
        },
        Mutation::Set(x) => *at_mut(p, std::slice::from_ref(last))? = x.clone(),
        Mutation::Text(_) => *at_mut(p, std::slice::from_ref(last))? = Value::String(MARK.into()),
    }
    let s = v.to_string();
    Some(match m {
        Mutation::Text(t) => s.replace(&format!("\"{MARK}\""), t),
        _ => s,
    })
}

fn junk() -> Vec<String> {
    [
        "",
        " ",
        "\n",
        "null",
        "[]",
        "{}",
        "0",
        "-1",
        "\"x\"",
        "true",
        "[DONE]",
        "{\"type\":null}",
        "{\"choices\":null}",
        "{\"choices\":[null]}",
        "{\"candidates\":null}",
        "{\"candidates\":[{}]}",
        "{\"candidates\":[null]}",
        "{\"error\":null}",
        "{\"error\":{}}",
        "{\"error\":\"x\"}",
        "\u{0}",
        "{",
        "}",
        "[[[[",
        "\u{feff}{}",
        "{\"type\":\"response.completed\"}",
        "{\"type\":\"response.completed\",\"response\":null}",
        "{\"type\":\"message_delta\"}",
        "{\"type\":\"content_block_delta\"}",
        "{\"type\":\"content_block_delta\",\"index\":null}",
        "{\"type\":\"content_block_stop\",\"index\":99999999999}",
        "{\"type\":\"response.output_text.delta\",\"delta\":null}",
        "{\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":4294967296}]}}]}",
        "{\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":18446744073709551615}]}}]}",
        "{\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":-1,\"function\":{\"arguments\":\"{\"}}]}}]}",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Cut points of `s`: all of them for small payloads, an even spread plus seeded ones for big ones.
fn cut_points(s: &str, rng: &mut Rng) -> Vec<usize> {
    let boundaries: Vec<usize> = (0..s.len()).filter(|&i| s.is_char_boundary(i)).collect();
    if boundaries.len() <= 128 {
        return boundaries;
    }
    let mut out: Vec<usize> = (0..64).map(|k| boundaries[k * boundaries.len() / 64]).collect();
    out.extend((0..24).map(|_| boundaries[rng.below(boundaries.len())]));
    out.sort_unstable();
    out.dedup();
    out
}

/// The payloads of `protocol` with payload `i` replaced by `mutated`, through a fresh parser.
/// `nth` alternates a transport failure before the end of the stream.
fn replay(protocol: &str, data: &[String], i: usize, mutated: &str, nth: usize) {
    let mut p = parser(protocol);
    let mut out = Vec::new();
    for (k, d) in data.iter().enumerate() {
        out.extend(p.feed(if k == i { mutated } else { d }));
    }
    if nth % 2 == 0 {
        out.extend(p.fail("transport failed".into()));
    }
    out.extend(p.finish());
    let _ = collect(out);
}

fn check(failures: &mut Vec<String>, label: impl FnOnce() -> String, run: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(run)).is_err() {
        failures.push(label());
    }
}

fn report(protocol: &str, what: &str, failures: Vec<String>) {
    assert!(
        failures.is_empty(),
        "{protocol} {what}: {} panics, first ones:\n{}",
        failures.len(),
        failures.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn no_parser_panics_on_a_payload_cut_anywhere() {
    for protocol in PROTOCOLS {
        let data = payloads(protocol);
        let mut rng = Rng::new(11);
        let mut failures = Vec::new();
        let mut nth = 0;
        for (i, d) in data.iter().enumerate() {
            for c in cut_points(d, &mut rng) {
                nth += 1;
                check(
                    &mut failures,
                    || format!("event {i} cut at {c}: {:?}", &d[..c]),
                    || replay(protocol, &data, i, &d[..c], nth),
                );
            }
        }
        report(protocol, "truncation", failures);
    }
}

#[test]
fn no_parser_panics_on_a_payload_with_a_field_dropped_retyped_or_made_extreme() {
    for protocol in PROTOCOLS {
        let data = payloads(protocol);
        let mut rng = Rng::new(23);
        let mut failures = Vec::new();
        let mut nth = 0;
        let mut cases = 0;
        for (i, d) in data.iter().enumerate() {
            let Ok(base) = serde_json::from_str::<Value>(d) else { continue };
            let mut ps = Vec::new();
            paths(&base, &mut Vec::new(), &mut ps);
            for path in &ps {
                for (name, m) in mutations(&mut rng) {
                    let Some(text) = apply(&base, path, &m) else { continue };
                    nth += 1;
                    cases += 1;
                    check(
                        &mut failures,
                        || format!("event {i} path {path:?} {name}: {}", text.chars().take(300).collect::<String>()),
                        || replay(protocol, &data, i, &text, nth),
                    );
                }
            }
        }
        assert!(cases > 1000, "{protocol}: {cases} cases");
        report(protocol, "mutation", failures);
    }
}

#[test]
fn no_parser_panics_on_junk_payloads_anywhere_in_a_stream() {
    for protocol in PROTOCOLS {
        let data = payloads(protocol);
        let mut failures = Vec::new();
        let mut nth = 0;
        for j in junk() {
            for i in 0..data.len() {
                nth += 1;
                check(&mut failures, || format!("event {i} replaced by {j:?}"), || replay(protocol, &data, i, &j, nth));
            }
        }
        report(protocol, "junk", failures);
    }
}

#[test]
fn no_parser_panics_on_a_stream_with_events_dropped_repeated_or_shuffled() {
    for protocol in PROTOCOLS {
        let data = payloads(protocol);
        let mut rng = Rng::new(47);
        let mut failures = Vec::new();
        for round in 0..300 {
            let n = rng.below(data.len() * 2 + 1);
            let picked: Vec<String> = (0..n).map(|_| data[rng.below(data.len())].clone()).collect();
            check(
                &mut failures,
                || format!("round {round}: events {picked:?}"),
                || {
                    let mut p = parser(protocol);
                    let mut out = Vec::new();
                    for d in &picked {
                        out.extend(p.feed(d));
                    }
                    if round % 3 == 0 {
                        out.extend(p.fail("x".into()));
                    }
                    out.extend(p.finish());
                    let _ = collect(out);
                },
            );
        }
        // the end of a stream that never started
        check(
            &mut failures,
            || "finish without events".into(),
            || {
                let mut p = parser(protocol);
                let _ = collect(p.finish());
            },
        );
        report(protocol, "shuffle", failures);
    }
}

/// Regression: a Gemini usage chunk whose candidate count is `u64::MAX` (any server can send it) made
/// `candidatesTokenCount + thoughtsTokenCount` overflow; the output count now saturates.
#[test]
fn regression_gemini_usage_with_a_count_at_u64_max_saturates_instead_of_overflowing() {
    let data = vec![concat!(
        r#"{"candidates":[{"content":{"parts":[{"text":"ok"}],"role":"model"},"finishReason":"STOP","index":0}],"#,
        r#""usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":18446744073709551615,"thoughtsTokenCount":12}}"#
    )
    .to_string()];
    let events = drive("gemini", &data);
    let output = events.iter().find_map(|e| match e {
        StreamEvent::Usage(u) => Some(u.output),
        _ => None,
    });
    assert_eq!(output, Some(u64::MAX), "{events:?}");
    assert!(events.contains(&StreamEvent::Finish(FinishReason::Stop)), "{events:?}");
}
