//! The OpenAI-compatible chat path against the quirks of real servers
//! (Ollama, vLLM, LM Studio, llama.cpp, OpenRouter, LiteLLM): each stream or
//! reply shape goes through the real parsing path, from bytes to events,
//! and the person must get text, a call, or a message that says what
//! happened, never a raw parse error and never a silent empty reply.

use futures::StreamExt;
use httpmock::prelude::*;
use nexo_core::{Collector, ErrorKind, FinishReason, Message, ModelRequest, ProviderConfig, StreamEvent, Usage};
use nexo_providers::testing::replay;
use nexo_providers::{registry, Client};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn run(text: &str, chunk: usize) -> (Collector, Vec<StreamEvent>) {
    let mut s = replay("openai-chat", text, chunk).unwrap();
    let mut c = Collector::new();
    let mut evs = Vec::new();
    while let Some(ev) = s.next().await {
        c.push(ev.clone());
        evs.push(ev);
    }
    (c, evs)
}

/// Chunks as `data:` events.
fn sse(chunks: &[Value]) -> String {
    chunks.iter().map(|c| format!("data: {c}\n\n")).collect()
}

fn delta(d: Value) -> Value {
    json!({"id":"x","choices":[{"index":0,"delta":d}]})
}

fn stop() -> Value {
    json!({"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
}

fn calls(c: &Collector) -> Vec<(String, String, Value)> {
    c.message().tool_calls().map(|(i, n, a)| (i.to_string(), n.to_string(), a.clone())).collect()
}

fn no_error(c: &Collector) {
    assert!(c.error.is_none(), "{:?}", c.error);
}

// ---- a: SSE framing ----

const HELLO: &str = r#"{"id":"x","choices":[{"index":0,"delta":{"content":"hola"}}]}"#;

#[tokio::test]
async fn a1_crlf_line_endings() {
    let text = format!("data: {HELLO}\r\n\r\ndata: {}\r\n\r\ndata: [DONE]\r\n\r\n", stop());
    for chunk in [0, 1, 5] {
        let (c, _) = run(&text, chunk).await;
        no_error(&c);
        assert_eq!(c.message().text(), "hola");
        assert_eq!(c.finish_reason(), FinishReason::Stop);
    }
}

#[tokio::test]
async fn a2_keep_alive_comment_lines_are_ignored() {
    let text =
        format!(": OPENROUTER PROCESSING\n\n: ping\n\ndata: {HELLO}\n\n: ping\n\ndata: {}\n\ndata: [DONE]\n\n", stop());
    let (c, evs) = run(&text, 0).await;
    no_error(&c);
    assert_eq!(c.message().text(), "hola");
    assert_eq!(evs.iter().filter(|e| matches!(e, StreamEvent::Finish(_))).count(), 1);
}

#[tokio::test]
async fn a3_a_stream_that_ends_without_done() {
    let text = format!("data: {HELLO}\n\ndata: {}\n\n", stop());
    let (c, evs) = run(&text, 0).await;
    no_error(&c);
    assert_eq!(c.message().text(), "hola");
    assert_eq!(c.finish_reason(), FinishReason::Stop);
    assert!(matches!(evs.last(), Some(StreamEvent::Done)));
}

#[tokio::test]
async fn a4_data_without_a_space_after_the_colon() {
    let text = format!("data:{HELLO}\n\ndata:{}\n\ndata:[DONE]\n\n", stop());
    let (c, _) = run(&text, 0).await;
    no_error(&c);
    assert_eq!(c.message().text(), "hola");
    assert_eq!(c.finish_reason(), FinishReason::Stop);
}

#[tokio::test]
async fn a5_an_event_split_in_the_middle_of_a_utf8_character() {
    let ev = r#"{"id":"x","choices":[{"index":0,"delta":{"content":"café ñandú 日本"}}]}"#;
    let ev = ev.replace("\\u00e9", "é");
    let text = format!("data: {ev}\n\ndata: {}\n\n", stop());
    let bytes = text.as_bytes();
    let at = text.find('ñ').unwrap() + 1; // inside the two bytes of ñ
    let chunks = vec![Ok(bytes::Bytes::copy_from_slice(&bytes[..at])), Ok(bytes::Bytes::copy_from_slice(&bytes[at..]))];
    let mut s = nexo_providers::openai_chat::OpenAiChat::parse(nexo_providers::sse::SseStream::new(
        futures::stream::iter(chunks),
    ));
    let mut c = Collector::new();
    while let Some(e) = s.next().await {
        c.push(e);
    }
    no_error(&c);
    assert_eq!(c.message().text(), "café ñandú 日本");
    // and one byte at a time
    let (c, _) = run(&text, 1).await;
    assert_eq!(c.message().text(), "café ñandú 日本");
}

#[tokio::test]
async fn a6_two_events_in_one_chunk() {
    let text = format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\n",
        delta(json!({"content":"a"})),
        delta(json!({"content":"b"})),
        stop()
    );
    let (c, _) = run(&text, 0).await;
    assert_eq!(c.message().text(), "ab");
    assert_eq!(c.finish_reason(), FinishReason::Stop);
}

// ---- b: tool calls ----

fn tc(v: Value) -> Value {
    delta(json!({"tool_calls": v}))
}

fn finish(reason: Value) -> Value {
    json!({"id":"x","choices":[{"index":0,"delta":{},"finish_reason":reason}]})
}

#[tokio::test]
async fn b1_arguments_sent_as_a_json_object() {
    let text = sse(&[
        tc(json!([{"index":0,"id":"c1","type":"function","function":{"name":"read","arguments":{"path":"a.rs"}}}])),
        finish(json!("tool_calls")),
    ]);
    let (c, _) = run(&text, 0).await;
    no_error(&c);
    assert_eq!(calls(&c), vec![("c1".into(), "read".into(), json!({"path":"a.rs"}))]);
}

#[tokio::test]
async fn b2_empty_or_missing_arguments_for_a_no_argument_tool() {
    for f in [json!({"name":"ls","arguments":""}), json!({"name":"ls"}), json!({"name":"ls","arguments":"{}"})] {
        let text =
            sse(&[tc(json!([{"index":0,"id":"c1","type":"function","function":f}])), finish(json!("tool_calls"))]);
        let (c, _) = run(&text, 0).await;
        no_error(&c);
        assert_eq!(calls(&c), vec![("c1".into(), "ls".into(), json!({}))], "{f}");
    }
}

#[tokio::test]
async fn b3_a_tool_call_without_an_id_gets_one() {
    let text = sse(&[
        tc(json!([{"index":0,"type":"function","function":{"name":"read","arguments":"{\"path\":"}}])),
        tc(json!([{"index":0,"function":{"arguments":"\"a\"}"}}])),
        finish(json!("tool_calls")),
    ]);
    let (c, _) = run(&text, 0).await;
    no_error(&c);
    let got = calls(&c);
    assert_eq!(got.len(), 1);
    assert!(!got[0].0.is_empty(), "an id was made up");
    assert_eq!(got[0].2, json!({"path":"a"}));
    // two calls without ids get different ones
    let text = sse(&[
        tc(
            json!([{"index":0,"function":{"name":"a","arguments":"{}"}},{"index":1,"function":{"name":"b","arguments":"{}"}}]),
        ),
        finish(json!("tool_calls")),
    ]);
    let (c, _) = run(&text, 0).await;
    let got = calls(&c);
    assert_eq!(got.len(), 2);
    assert_ne!(got[0].0, got[1].0);
}

#[tokio::test]
async fn b4_tool_call_deltas_without_an_index() {
    let text = sse(&[
        tc(json!([{"id":"c1","type":"function","function":{"name":"read","arguments":"{\"path\":"}}])),
        tc(json!([{"function":{"arguments":"\"a\"}"}}])),
        finish(json!("tool_calls")),
    ]);
    let (c, _) = run(&text, 0).await;
    no_error(&c);
    assert_eq!(calls(&c), vec![("c1".into(), "read".into(), json!({"path":"a"}))]);
}

#[tokio::test]
async fn b5_the_whole_tool_call_in_one_delta() {
    let text = sse(&[
        tc(json!([{"index":0,"id":"c1","type":"function","function":{"name":"read","arguments":"{\"path\":\"a\"}"}}])),
        finish(json!("tool_calls")),
    ]);
    let (c, evs) = run(&text, 0).await;
    no_error(&c);
    assert_eq!(calls(&c), vec![("c1".into(), "read".into(), json!({"path":"a"}))]);
    assert_eq!(evs.iter().filter(|e| matches!(e, StreamEvent::ToolCallEnd { .. })).count(), 1);
}

#[tokio::test]
async fn b6_two_tool_calls_interleaved_by_index() {
    let text = sse(&[
        tc(json!([{"index":0,"id":"a","type":"function","function":{"name":"read","arguments":""}}])),
        tc(json!([{"index":1,"id":"b","type":"function","function":{"name":"ls","arguments":""}}])),
        tc(json!([{"index":0,"function":{"arguments":"{\"path\":"}}])),
        tc(json!([{"index":1,"function":{"arguments":"{\"dir\":"}}])),
        tc(json!([{"index":0,"function":{"arguments":"\"x\"}"}}])),
        tc(json!([{"index":1,"function":{"arguments":"\"y\"}"}}])),
        finish(json!("tool_calls")),
    ]);
    for chunk in [0, 3] {
        let (c, _) = run(&text, chunk).await;
        no_error(&c);
        assert_eq!(
            calls(&c),
            vec![("a".into(), "read".into(), json!({"path":"x"})), ("b".into(), "ls".into(), json!({"dir":"y"}))]
        );
    }
}

#[tokio::test]
async fn b7_the_name_repeated_in_every_delta_is_not_concatenated() {
    let text = sse(&[
        tc(json!([{"index":0,"id":"c1","type":"function","function":{"name":"read","arguments":"{\"path\":"}}])),
        tc(json!([{"index":0,"id":"c1","type":"function","function":{"name":"read","arguments":"\"a\"}"}}])),
        finish(json!("tool_calls")),
    ]);
    let (c, _) = run(&text, 0).await;
    no_error(&c);
    assert_eq!(calls(&c), vec![("c1".into(), "read".into(), json!({"path":"a"}))]);
    // same without the id on the later deltas
    let text = sse(&[
        tc(json!([{"index":0,"id":"c1","function":{"name":"read","arguments":"{\"path\":"}}])),
        tc(json!([{"index":0,"function":{"name":"read","arguments":"\"a\"}"}}])),
        finish(json!("tool_calls")),
    ]);
    let (c, _) = run(&text, 0).await;
    assert_eq!(calls(&c), vec![("c1".into(), "read".into(), json!({"path":"a"}))]);
}

// ---- c: content ----

#[tokio::test]
async fn c1_null_content_beside_tool_calls() {
    let text = sse(&[
        delta(
            json!({"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"ls","arguments":"{}"}}]}),
        ),
        finish(json!("tool_calls")),
    ]);
    let (c, _) = run(&text, 0).await;
    no_error(&c);
    assert_eq!(c.message().text(), "");
    assert_eq!(calls(&c).len(), 1);
    assert_eq!(c.finish_reason(), FinishReason::ToolUse);
}

#[tokio::test]
async fn c2_an_empty_delta_is_harmless() {
    let text = sse(&[delta(json!({})), delta(json!({"content":"hi"})), delta(json!({})), stop()]);
    let (c, _) = run(&text, 0).await;
    no_error(&c);
    assert_eq!(c.message().text(), "hi");
}

#[tokio::test]
async fn c3_an_empty_choices_chunk_that_carries_only_usage() {
    let text = sse(&[
        delta(json!({"content":"hi"})),
        stop(),
        json!({"id":"x","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2}}),
    ]) + "data: [DONE]\n\n";
    let (c, evs) = run(&text, 0).await;
    no_error(&c);
    assert_eq!(c.usage, Usage { input: 10, output: 2, ..Default::default() });
    assert_eq!(evs.iter().filter(|e| matches!(e, StreamEvent::Finish(_))).count(), 1);
}

#[tokio::test]
async fn c4_finish_reason_missing_null_or_not_standard() {
    // missing and null: the end of the stream is the stop
    for fr in [None, Some(Value::Null)] {
        let mut last = json!({"id":"x","choices":[{"index":0,"delta":{}}]});
        if let Some(v) = fr {
            last["choices"][0]["finish_reason"] = v;
        }
        let text = sse(&[delta(json!({"content":"hi"})), last]) + "data: [DONE]\n\n";
        let (c, _) = run(&text, 0).await;
        no_error(&c);
        assert_eq!(c.message().text(), "hi");
        assert_eq!(c.finish_reason(), FinishReason::Stop);
    }
    // a word nobody defined
    let text = sse(&[delta(json!({"content":"hi"})), finish(json!("eos"))]);
    let (c, _) = run(&text, 0).await;
    no_error(&c);
    assert_eq!(c.finish_reason(), FinishReason::Stop);
    // "function_call" after a call is a tool use
    let text = sse(&[
        tc(json!([{"index":0,"id":"c1","function":{"name":"ls","arguments":"{}"}}])),
        finish(json!("function_call")),
    ]);
    let (c, _) = run(&text, 0).await;
    assert_eq!(c.finish_reason(), FinishReason::ToolUse);
}

#[tokio::test]
async fn c5_tool_calls_as_the_finish_with_no_tool_calls_is_a_plain_stop() {
    let text = sse(&[delta(json!({"content":"all done"})), finish(json!("tool_calls"))]);
    let (c, _) = run(&text, 0).await;
    no_error(&c);
    assert_eq!(c.message().text(), "all done");
    assert_eq!(c.finish_reason(), FinishReason::Stop, "there is no call to run");
}

#[tokio::test]
async fn c6_reasoning_under_reasoning_content_and_under_reasoning() {
    for key in ["reasoning_content", "reasoning"] {
        let text = sse(&[delta(json!({ key: "hmm" })), delta(json!({"content":"ok"})), stop()]);
        let (c, _) = run(&text, 0).await;
        no_error(&c);
        assert_eq!(c.message().reasoning(), "hmm", "{key}");
        assert_eq!(c.message().text(), "ok", "{key}");
    }
}

#[tokio::test]
async fn c7_think_tags_inside_content_stay_in_the_text_today() {
    // recorded behaviour: nexo does not parse <think> tags; they arrive as text
    let text = sse(&[delta(json!({"content":"<think>plan</think>answer"})), stop()]);
    let (c, _) = run(&text, 0).await;
    no_error(&c);
    assert_eq!(c.message().text(), "<think>plan</think>answer");
    assert_eq!(c.message().reasoning(), "");
}

// ---- d: usage ----

#[tokio::test]
async fn d1_no_usage_at_all_streaming() {
    let text = sse(&[delta(json!({"content":"hi"})), stop()]) + "data: [DONE]\n\n";
    let (c, evs) = run(&text, 0).await;
    no_error(&c);
    assert!(!evs.iter().any(|e| matches!(e, StreamEvent::Usage(_))));
    assert_eq!(c.usage, Usage::default());
}

#[tokio::test]
async fn d2_usage_only_in_the_final_chunk_and_without_cache_details() {
    let text = sse(&[
        delta(json!({"content":"hi"})),
        stop(),
        json!({"id":"x","choices":[],"usage":{"prompt_tokens":50,"completion_tokens":4,"total_tokens":54}}),
    ]);
    let (c, _) = run(&text, 0).await;
    assert_eq!(c.usage, Usage { input: 50, output: 4, ..Default::default() });
    // details present but with no cached_tokens, or null
    for u in [
        json!({"prompt_tokens":50,"completion_tokens":4,"prompt_tokens_details":{}}),
        json!({"prompt_tokens":50,"completion_tokens":4,"prompt_tokens_details":null,"completion_tokens_details":null}),
    ] {
        let text = sse(&[delta(json!({"content":"hi"})), stop(), json!({"id":"x","choices":[],"usage":u})]);
        let (c, _) = run(&text, 0).await;
        assert_eq!(c.usage, Usage { input: 50, output: 4, ..Default::default() });
    }
}

#[tokio::test]
async fn d3_usage_numbers_sent_as_floats() {
    let text = sse(&[
        delta(json!({"content":"hi"})),
        stop(),
        json!({"id":"x","choices":[],"usage":{"prompt_tokens":50.0,"completion_tokens":4.0,"prompt_tokens_details":{"cached_tokens":10.0}}}),
    ]);
    let (c, _) = run(&text, 0).await;
    assert_eq!(c.usage, Usage { input: 40, output: 4, cache_read: 10, ..Default::default() });
}

#[tokio::test]
async fn d4_no_usage_in_a_non_sse_reply() {
    // a server that ignores stream:true and answers with one JSON body: the
    // reply is not lost and the missing usage is not an error
    let body =
        r#"{"id":"x","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#;
    let (c, _) = run(&format!("{body}\n"), 0).await;
    assert!(c.error.is_none(), "{:?}", c.error);
    assert_eq!(c.message().text(), "hi");
}

// ---- e: errors ----

fn cfg(base: &str) -> ProviderConfig {
    let mut c = ProviderConfig::new("openai-chat", base, None);
    c.models = vec!["m".into()];
    c
}

async fn http_error(status: u16, headers: &[(&str, &str)], body: &str) -> nexo_core::ProviderError {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            let mut t = then.status(status);
            for (k, v) in headers {
                t = t.header(*k, *v);
            }
            t.body(body);
        })
        .await;
    let req = ModelRequest::new("m", "", vec![Message::user("hi")]);
    match Client::new(registry()).stream(&cfg(&server.base_url()), &req, 0).await {
        Err(e) => e,
        Ok(_) => panic!("an HTTP {status} is an error"),
    }
}

async fn stream_of(body: &str) -> Collector {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).header("content-type", "text/event-stream").body(body);
        })
        .await;
    let req = ModelRequest::new("m", "", vec![Message::user("hi")]);
    let mut s = Client::new(registry()).stream(&cfg(&server.base_url()), &req, 0).await.unwrap();
    let mut c = Collector::new();
    while let Some(e) = s.next().await {
        c.push(e);
    }
    c
}

#[tokio::test]
async fn e1_http_200_whose_first_event_is_an_error() {
    let body = format!(
        "data: {}\n\n",
        json!({"error":{"message":"Model is loading, try again","type":"server_error","code":503}})
    );
    let c = stream_of(&body).await;
    let e = c.error.clone().expect("the error reaches the person");
    assert!(e.human().contains("Model is loading"), "{}", e.human());
    assert_eq!(c.message().text(), "");
}

#[tokio::test]
async fn e2_an_error_body_that_is_plain_text_or_html() {
    let e = http_error(500, &[], "Internal Server Error").await;
    assert_eq!(e.kind, ErrorKind::Server);
    assert!(e.human().contains("Internal Server Error"), "{}", e.human());
    let html = "<html>\n<head><title>502 Bad Gateway</title></head>\n<body>\n<center><h1>502 Bad Gateway</h1></center>\n<hr><center>nginx</center>\n</body>\n</html>";
    let e = http_error(502, &[], html).await;
    let h = e.human();
    assert!(!h.contains('<'), "no markup shown to the person: {h}");
    assert!(h.contains("502"), "{h}");
    assert!(h.to_lowercase().contains("base url") || h.to_lowercase().contains("web page"), "says what it is: {h}");
}

#[tokio::test]
async fn e3_error_json_with_error_as_a_string() {
    let e = http_error(500, &[], r#"{"error":"CUDA out of memory"}"#).await;
    assert!(e.human().contains("CUDA out of memory"), "{}", e.human());
    assert!(!e.human().contains('{'), "{}", e.human());
    // the same inside a 200 stream
    let c = stream_of("data: {\"error\":\"model crashed\"}\n\n").await;
    let e = c.error.expect("error");
    assert!(e.human().contains("model crashed"), "{}", e.human());
}

#[tokio::test]
async fn e4_429_with_retry_after_in_seconds_and_as_a_date() {
    let e = http_error(429, &[("retry-after", "7")], r#"{"error":{"message":"slow down","type":"rate_limit_error"}}"#)
        .await;
    assert_eq!(e.kind, ErrorKind::RateLimit);
    assert_eq!(e.retry_after, Some(std::time::Duration::from_secs(7)));
    assert!(e.human().contains("slow down"), "{}", e.human());
    let e = http_error(429, &[("retry-after", "Wed, 21 Oct 2099 07:28:00 GMT")], "too many requests").await;
    assert_eq!(e.kind, ErrorKind::RateLimit);
    assert!(e.retry_after.unwrap() > std::time::Duration::from_secs(3600), "{:?}", e.retry_after);
}

#[tokio::test]
async fn e5_ollama_model_not_found() {
    let e = http_error(404, &[], r#"{"error":"model \"x\" not found, try pulling it first"}"#).await;
    let h = e.human();
    assert!(h.contains("model \"x\" not found, try pulling it first"), "{h}");
    assert!(!h.contains('{'), "{h}");
}

// ---- 2: a stream cut by the server mid-reply ----

/// A server that promises a long body, sends `part` and closes.
async fn cutting_server(part: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let mut got = Vec::new();
                while !String::from_utf8_lossy(&got).contains("\r\n\r\n") {
                    let k = sock.read(&mut buf).await.unwrap_or(0);
                    if k == 0 {
                        return;
                    }
                    got.extend_from_slice(&buf[..k]);
                }
                // the request body may still be in flight: read what comes, then answer
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n";
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(part.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn s2_a_connection_cut_mid_reply_keeps_the_text_and_tells_the_person() {
    let part: &'static str =
        "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"half of an ans\"}}]}\n\n";
    let base = cutting_server(part).await;
    let req = ModelRequest::new("m", "", vec![Message::user("hi")]);
    let mut s = Client::new(registry()).stream(&cfg(&base), &req, 0).await.unwrap();
    let mut c = Collector::new();
    while let Some(e) = s.next().await {
        c.push(e);
    }
    assert_eq!(c.message().text(), "half of an ans", "the partial text is kept");
    let e = c.error.as_ref().expect("the person is told the reply was cut");
    assert_ne!(c.finish_reason(), FinishReason::Stop);
    assert_eq!(e.kind, ErrorKind::Network);
    assert!(e.human().contains("cut") || e.human().contains("Could not reach"), "{}", e.human());
}
