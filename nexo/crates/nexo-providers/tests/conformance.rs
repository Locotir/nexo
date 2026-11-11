//! Conformance: every wire format against the stream shapes its providers send.
//! The checks are numbered as in the report (`t1` plain text, `t2` reasoning, `t3` tool calls,
//! `t4` usage, `t5` finish reasons, `t6` stream oddities, `t7` request shape, `t8` model list and
//! probe). Streams go through the real parsers from bytes to events, at several chunk sizes, and
//! the `via_the_client_*` tests go through the real HTTP client against a local fixture server.

use futures::StreamExt;
use httpmock::prelude::*;
use nexo_core::{
    Collector, Effort, ErrorKind, FinishReason, Message, ModelRequest, Part, ProviderConfig, ReasoningConfig, Role,
    StreamEvent, ToolSpec, Usage,
};
use nexo_providers::anthropic::Anthropic;
use nexo_providers::gemini::Gemini;
use nexo_providers::openai_chat::OpenAiChat;
use nexo_providers::openai_responses::OpenAiResponses;
use nexo_providers::testing::replay;
use nexo_providers::{registry, Client};
use serde_json::{json, Value};

// ---- helpers ----

#[derive(Debug, PartialEq)]
struct Seen {
    text: String,
    reasoning: String,
    /// `(name, arguments)` of each call, in order; the ids are checked apart.
    calls: Vec<(String, Value)>,
    finish: FinishReason,
    usage: Usage,
    error: Option<String>,
}

async fn run(protocol: &str, text: &str, chunk: usize) -> (Collector, Vec<StreamEvent>) {
    let mut s = replay(protocol, text, chunk).expect("a known protocol");
    let mut c = Collector::new();
    let mut evs = Vec::new();
    while let Some(ev) = s.next().await {
        c.push(ev.clone());
        evs.push(ev);
    }
    (c, evs)
}

fn seen(c: &Collector) -> Seen {
    let m = c.message();
    Seen {
        text: m.text(),
        reasoning: m.reasoning(),
        calls: m.tool_calls().map(|(_, n, a)| (n.to_string(), a.clone())).collect(),
        finish: c.finish_reason(),
        usage: c.usage,
        error: c.error.as_ref().map(|e| e.message.clone()),
    }
}

/// The stream at four chunk sizes (whole, one byte, odd sizes): all must agree.
async fn play(protocol: &str, text: &str) -> Seen {
    let (first, _) = run(protocol, text, 0).await;
    let want = seen(&first);
    for chunk in [1, 3, 17] {
        let (c, _) = run(protocol, text, chunk).await;
        assert_eq!(seen(&c), want, "chunks of {chunk} bytes");
    }
    want
}

fn ids(c: &Collector) -> Vec<String> {
    c.message().tool_calls().map(|(i, _, _)| i.to_string()).collect()
}

// chat completions

fn sse(chunks: &[Value]) -> String {
    let mut s: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    s.push_str("data: [DONE]\n\n");
    s
}

fn d(delta: Value) -> Value {
    json!({"id":"c1","model":"m","choices":[{"index":0,"delta":delta}]})
}

fn fin(reason: &str) -> Value {
    json!({"id":"c1","model":"m","choices":[{"index":0,"delta":{},"finish_reason":reason}]})
}

fn tc(index: u64, id: &str, name: &str, args: &str) -> Value {
    json!({"index":index,"id":id,"type":"function","function":{"name":name,"arguments":args}})
}

fn tcs(calls: Vec<Value>) -> Value {
    d(json!({ "tool_calls": calls }))
}

fn usage_chunk(u: Value) -> Value {
    json!({"id":"c1","choices":[],"usage":u})
}

// anthropic

fn ev(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn a_start(usage: Value) -> String {
    ev("message_start", json!({"type":"message_start","message":{"id":"msg_1","model":"claude-x","usage":usage}}))
}

fn a_text(i: u64, t: &str) -> String {
    ev("content_block_start", json!({"type":"content_block_start","index":i,"content_block":{"type":"text","text":""}}))
        + &ev(
            "content_block_delta",
            json!({"type":"content_block_delta","index":i,"delta":{"type":"text_delta","text":t}}),
        )
        + &ev("content_block_stop", json!({"type":"content_block_stop","index":i}))
}

fn a_tool(i: u64, id: &str, name: &str, parts: &[&str]) -> String {
    let mut s = ev(
        "content_block_start",
        json!({"type":"content_block_start","index":i,"content_block":{"type":"tool_use","id":id,"name":name,"input":{}}}),
    );
    for p in parts {
        s += &ev(
            "content_block_delta",
            json!({"type":"content_block_delta","index":i,"delta":{"type":"input_json_delta","partial_json":p}}),
        );
    }
    s + &ev("content_block_stop", json!({"type":"content_block_stop","index":i}))
}

fn a_end(reason: &str, usage: Value) -> String {
    ev("message_delta", json!({"type":"message_delta","delta":{"stop_reason":reason},"usage":usage}))
        + &ev("message_stop", json!({"type":"message_stop"}))
}

// gemini and responses: `data:` lines

fn data(chunks: &[Value]) -> String {
    chunks.iter().map(|c| format!("data: {c}\r\n\r\n")).collect()
}

fn g_text(t: &str) -> Value {
    json!({"candidates":[{"content":{"role":"model","parts":[{"text":t}]},"index":0}]})
}

fn r_events(items: &[Value]) -> String {
    items.iter().map(|c| format!("event: {}\ndata: {c}\n\n", c["type"].as_str().unwrap_or(""))).collect()
}

fn r_created() -> Value {
    json!({"type":"response.created","response":{"id":"resp_1","model":"gpt-5"}})
}

fn r_done(usage: Value) -> Value {
    json!({"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":usage}})
}

fn r_call(idx: u64, id: &str, name: &str) -> Value {
    json!({"type":"response.output_item.added","output_index":idx,"item":{"type":"function_call","call_id":id,"name":name,"arguments":""}})
}

fn r_args(idx: u64, d: &str) -> Value {
    json!({"type":"response.function_call_arguments.delta","output_index":idx,"delta":d})
}

fn r_args_done(idx: u64, a: &str) -> Value {
    json!({"type":"response.function_call_arguments.done","output_index":idx,"arguments":a})
}

fn cfg(protocol: &str, base: &str) -> ProviderConfig {
    ProviderConfig::new(protocol, base, None)
}

fn req(model: &str) -> ModelRequest {
    ModelRequest::new(model, "sys", vec![Message::user("hi")])
}

fn spec(name: &str, params: Value) -> ToolSpec {
    ToolSpec { name: name.into(), description: "d".into(), parameters: params }
}

// ================= t1: plain text =================

#[tokio::test]
async fn t1_chat_text_at_every_chunking_with_a_role_only_first_chunk_and_split_utf8() {
    let text = sse(&[
        d(json!({"role":"assistant","content":""})),
        d(json!({"content":"Hel"})),
        d(json!({"content":"lo é日本"})),
        fin("stop"),
        usage_chunk(json!({"prompt_tokens":9,"completion_tokens":3,"total_tokens":12})),
    ]);
    let s = play("openai-chat", &text).await;
    assert_eq!(s.text, "Hello é日本");
    assert_eq!(s.finish, FinishReason::Stop);
    assert_eq!(s.usage, Usage { input: 9, output: 3, ..Usage::default() });
    assert_eq!(s.error, None);
}

#[tokio::test]
async fn t1_chat_server_that_ignores_stream_answers_with_one_json_body() {
    // llama.cpp, LM Studio and Ollama can answer a whole `chat.completion` to a streaming request
    let body = json!({"id":"c1","choices":[{"index":0,"message":{"role":"assistant","content":"whole"},"finish_reason":"stop"}],"usage":{"prompt_tokens":4,"completion_tokens":1}});
    let s = play("openai-chat", &body.to_string()).await;
    assert_eq!((s.text.as_str(), s.finish, s.usage.output), ("whole", FinishReason::Stop, 1));
}

#[tokio::test]
async fn t1_anthropic_text() {
    let text = a_start(json!({"input_tokens":12,"output_tokens":1}))
        + &a_text(0, "Hello")
        + &ev("ping", json!({"type":"ping"}))
        + &a_end("end_turn", json!({"output_tokens":5}));
    let s = play("anthropic", &text).await;
    assert_eq!(s.text, "Hello");
    assert_eq!(s.finish, FinishReason::Stop);
    assert_eq!(s.usage, Usage { input: 12, output: 5, ..Usage::default() });
}

#[tokio::test]
async fn t1_gemini_text_with_crlf_framing() {
    let text = data(&[
        g_text("Hel"),
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"lo"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":2,"totalTokenCount":9}}),
    ]);
    let s = play("gemini", &text).await;
    assert_eq!(s.text, "Hello");
    assert_eq!(s.finish, FinishReason::Stop);
    assert_eq!(s.usage, Usage { input: 7, output: 2, ..Usage::default() });
}

#[tokio::test]
async fn t1_responses_text() {
    let text = r_events(&[
        r_created(),
        json!({"type":"response.output_text.delta","output_index":0,"delta":"Hel"}),
        json!({"type":"response.output_text.delta","output_index":0,"delta":"lo"}),
        r_done(json!({"input_tokens":10,"output_tokens":2})),
    ]);
    let s = play("openai-responses", &text).await;
    assert_eq!(s.text, "Hello");
    assert_eq!(s.finish, FinishReason::Stop);
}

// ================= t2: reasoning, kept apart from the answer =================

#[tokio::test]
async fn t2_chat_reasoning_content_deepseek_moonshot_vllm_llamacpp() {
    // DeepSeek sends `reasoning_content: ""` first and `reasoning_content: null` once it answers
    let text = sse(&[
        d(json!({"role":"assistant","content":null,"reasoning_content":""})),
        d(json!({"content":null,"reasoning_content":"Think "})),
        d(json!({"content":null,"reasoning_content":"hard."})),
        d(json!({"content":"Answer","reasoning_content":null})),
        fin("stop"),
    ]);
    let s = play("openai-chat", &text).await;
    assert_eq!((s.reasoning.as_str(), s.text.as_str()), ("Think hard.", "Answer"));
}

#[tokio::test]
async fn t2_chat_reasoning_field_ollama_and_openrouter() {
    let text = sse(&[
        d(json!({"role":"assistant","content":"","reasoning":"Let me see. "})),
        d(json!({"reasoning":"Done.","reasoning_details":[{"type":"reasoning.text","text":"Done."}]})),
        d(json!({"content":"42"})),
        fin("stop"),
    ]);
    let s = play("openai-chat", &text).await;
    assert_eq!((s.reasoning.as_str(), s.text.as_str()), ("Let me see. Done.", "42"));
}

#[tokio::test]
async fn t2_chat_mistral_typed_thinking_parts() {
    let text = sse(&[
        d(json!({"content":[{"type":"thinking","thinking":[{"type":"text","text":"Hmm. "}]}]})),
        d(
            json!({"content":[{"type":"thinking","thinking":[{"type":"text","text":"Ok."}]},{"type":"text","text":"Hi"}]}),
        ),
        d(json!({"content":[{"type":"text","text":" there"}]})),
        fin("stop"),
    ]);
    let s = play("openai-chat", &text).await;
    assert_eq!((s.reasoning.as_str(), s.text.as_str()), ("Hmm. Ok.", "Hi there"));
}

#[tokio::test]
async fn t2_chat_think_tags_in_content_lm_studio_llamacpp_vllm_become_reasoning() {
    // a tag cut across events, and the blank lines after the close
    let text = sse(&[
        d(json!({"content":"<thi"})),
        d(json!({"content":"nk>\nplan it</th"})),
        d(json!({"content":"ink>\n\nThe "})),
        d(json!({"content":"answer"})),
        fin("stop"),
    ]);
    let s = play("openai-chat", &text).await;
    assert_eq!((s.reasoning.as_str(), s.text.as_str()), ("\nplan it", "The answer"));
}

#[tokio::test]
async fn t2_chat_think_tags_only_count_at_the_start_of_the_reply() {
    for body in ["use <think> in prompts", "<b>bold</b> text", "a<think>x</think>b"] {
        let s = play("openai-chat", &sse(&[d(json!({ "content": body })), fin("stop")])).await;
        assert_eq!((s.text.as_str(), s.reasoning.as_str()), (body, ""), "{body}");
    }
}

#[tokio::test]
async fn t2_chat_unclosed_think_block_is_reasoning_not_lost() {
    let s = play("openai-chat", &sse(&[d(json!({"content":"<think>never closed"})), fin("length")])).await;
    assert_eq!((s.reasoning.as_str(), s.text.as_str(), s.finish), ("never closed", "", FinishReason::Length));
}

#[tokio::test]
async fn t2_chat_reasoning_then_tool_call_without_any_text() {
    let text = sse(&[
        d(json!({"reasoning_content":"I should read."})),
        tcs(vec![tc(0, "c1", "read", "{\"path\":\"a\"}")]),
        fin("tool_calls"),
    ]);
    let s = play("openai-chat", &text).await;
    assert_eq!(s.reasoning, "I should read.");
    assert_eq!(s.text, "");
    assert_eq!(s.calls, vec![("read".to_string(), json!({"path":"a"}))]);
    assert_eq!(s.finish, FinishReason::ToolUse);
}

#[tokio::test]
async fn t2_anthropic_thinking_signature_and_redacted_block() {
    let text = a_start(json!({"input_tokens":3}))
        + &ev(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        )
        + &ev(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Plan."}}),
        )
        + &ev(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SIG"}}),
        )
        + &ev("content_block_stop", json!({"type":"content_block_stop","index":0}))
        + &ev(
            "content_block_start",
            json!({"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"RED"}}),
        )
        + &ev("content_block_stop", json!({"type":"content_block_stop","index":1}))
        + &a_text(2, "Done")
        + &a_end("end_turn", json!({"output_tokens":4}));
    let s = play("anthropic", &text).await;
    assert_eq!((s.reasoning.as_str(), s.text.as_str()), ("Plan.", "Done"));
    let (c, _) = run("anthropic", &text, 0).await;
    let parts = c.message().content;
    assert!(parts.iter().any(|p| matches!(p, Part::Reasoning { signature: Some(s), .. } if s == "SIG")), "{parts:?}");
    assert!(parts.iter().any(|p| matches!(p, Part::RedactedReasoning { data, .. } if data == "RED")), "{parts:?}");
}

#[tokio::test]
async fn t2_gemini_thought_parts_are_reasoning_and_do_not_reach_the_text() {
    let text = data(&[
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"Think.","thought":true}]}}]}),
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"Answer","thoughtSignature":"TS"}]},"finishReason":"STOP"}]}),
    ]);
    let s = play("gemini", &text).await;
    assert_eq!((s.reasoning.as_str(), s.text.as_str()), ("Think.", "Answer"));
}

#[tokio::test]
async fn t2_responses_reasoning_summary_with_its_id_and_encrypted_content() {
    let text = r_events(&[
        r_created(),
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[]}}),
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":0,"delta":"Plan"}),
        json!({"type":"response.reasoning_summary_text.done","output_index":0,"summary_index":0,"text":"Plan"}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_1","encrypted_content":"ENC","summary":[]}}),
        json!({"type":"response.output_text.delta","output_index":1,"delta":"Done"}),
        r_done(json!({"input_tokens":3,"output_tokens":9,"output_tokens_details":{"reasoning_tokens":5}})),
    ]);
    let s = play("openai-responses", &text).await;
    assert_eq!((s.reasoning.trim(), s.text.as_str()), ("Plan", "Done"));
    assert_eq!(s.usage.reasoning, 5);
    let (c, _) = run("openai-responses", &text, 0).await;
    let parts = c.message().content;
    assert!(
        parts.iter().any(
            |p| matches!(p, Part::Reasoning { signature: Some(i), encrypted: Some(e), .. } if i == "rs_1" && e == "ENC")
        ),
        "{parts:?}"
    );
}

// ================= t3: tool calls =================

#[tokio::test]
async fn t3_chat_one_call_with_arguments_split_in_deltas() {
    let text = sse(&[
        tcs(vec![tc(0, "call_1", "read", "")]),
        tcs(vec![json!({"index":0,"function":{"arguments":"{\"pa"}})]),
        tcs(vec![json!({"index":0,"function":{"arguments":"th\":\"a.rs\"}"}})]),
        fin("tool_calls"),
    ]);
    let s = play("openai-chat", &text).await;
    assert_eq!(s.calls, vec![("read".to_string(), json!({"path":"a.rs"}))]);
    let (c, _) = run("openai-chat", &text, 0).await;
    assert_eq!(ids(&c), ["call_1"]);
}

#[tokio::test]
async fn t3_chat_two_parallel_calls_by_index_with_interleaved_deltas() {
    let text = sse(&[
        tcs(vec![tc(0, "c0", "read", ""), tc(1, "c1", "grep", "")]),
        tcs(vec![json!({"index":1,"function":{"arguments":"{\"q\":"}})]),
        tcs(vec![json!({"index":0,"function":{"arguments":"{\"path\":\"a\"}"}})]),
        tcs(vec![json!({"index":1,"function":{"arguments":"\"x\"}"}})]),
        fin("tool_calls"),
    ]);
    let s = play("openai-chat", &text).await;
    assert_eq!(s.calls, vec![("read".to_string(), json!({"path":"a"})), ("grep".to_string(), json!({"q":"x"}))]);
    let (c, _) = run("openai-chat", &text, 0).await;
    assert_eq!(ids(&c), ["c0", "c1"]);
}

#[tokio::test]
async fn t3_chat_ollama_parallel_calls_all_numbered_zero_with_different_ids() {
    let text = sse(&[
        tcs(vec![tc(0, "call_a", "read", "{\"path\":\"a\"}")]),
        tcs(vec![tc(0, "call_b", "read", "{\"path\":\"b\"}")]),
        fin("stop"),
    ]);
    let s = play("openai-chat", &text).await;
    assert_eq!(s.calls.len(), 2, "{s:?}");
    assert_eq!(s.finish, FinishReason::ToolUse, "`stop` with calls is a tool round");
    let (c, _) = run("openai-chat", &text, 0).await;
    assert_eq!(ids(&c), ["call_a", "call_b"]);
}

#[tokio::test]
async fn t3_chat_a_reused_id_on_a_later_index_keeps_each_call_whole() {
    // before: the second call got a fresh id, and its next chunk (still "A") never matched it again
    let chunk = |i: u64, args: &str, first: bool| {
        let name = if first { "read" } else { "" };
        tcs(vec![tc(i, "A", name, args)])
    };
    let text = sse(&[
        chunk(0, "{\"path\"", true),
        chunk(0, ":\"a", false),
        chunk(0, ".rs\"}", false),
        chunk(1, "{\"path\"", true),
        chunk(1, ":\"b", false),
        chunk(1, ".rs\"}", false),
        fin("tool_calls"),
    ]);
    let s = play("openai-chat", &text).await;
    assert_eq!(
        s.calls,
        vec![("read".to_string(), json!({"path":"a.rs"})), ("read".to_string(), json!({"path":"b.rs"}))]
    );
    let (c, evs) = run("openai-chat", &text, 0).await;
    let got = ids(&c);
    assert_eq!(got.len(), 2);
    assert_ne!(got[0], got[1]);
    let starts = evs.iter().filter(|e| matches!(e, StreamEvent::ToolCallStart { .. })).count();
    let ends = evs.iter().filter(|e| matches!(e, StreamEvent::ToolCallEnd { .. })).count();
    assert_eq!((starts, ends), (2, 2));
}

#[tokio::test]
async fn t3_chat_empty_null_and_missing_arguments_all_give_an_empty_object() {
    // `null` was kept as null and sent back as `"arguments":"null"`, which strict servers refuse
    for args in [json!(""), json!("{}"), json!("null"), json!(" null "), Value::Null] {
        let call = json!({"index":0,"id":"c1","type":"function","function":{"name":"ping","arguments":args}});
        let s = play("openai-chat", &sse(&[tcs(vec![call]), fin("tool_calls")])).await;
        assert_eq!(s.calls, vec![("ping".to_string(), json!({}))], "arguments {args}");
    }
    let no_key = json!({"index":0,"id":"c1","type":"function","function":{"name":"ping"}});
    let s = play("openai-chat", &sse(&[tcs(vec![no_key]), fin("tool_calls")])).await;
    assert_eq!(s.calls, vec![("ping".to_string(), json!({}))]);
}

#[tokio::test]
async fn t3_chat_arguments_sent_as_a_json_object_by_local_servers() {
    let call = json!({"index":0,"id":"c1","type":"function","function":{"name":"read","arguments":{"path":"a"}}});
    let s = play("openai-chat", &sse(&[tcs(vec![call]), fin("tool_calls")])).await;
    assert_eq!(s.calls, vec![("read".to_string(), json!({"path":"a"}))]);
}

#[tokio::test]
async fn t3_chat_later_deltas_with_an_empty_id_continue_the_call_qwen_fireworks_cerebras() {
    let text = sse(&[
        tcs(vec![tc(0, "call_x", "read", "")]),
        tcs(vec![json!({"index":0,"id":"","type":"function","function":{"arguments":"{\"path\":"}})]),
        tcs(vec![json!({"index":0,"id":"","type":"function","function":{"arguments":"\"a\"}"}})]),
        fin("tool_calls"),
    ]);
    let s = play("openai-chat", &text).await;
    assert_eq!(s.calls, vec![("read".to_string(), json!({"path":"a"}))]);
}

#[tokio::test]
async fn t3_chat_a_call_without_id_gets_an_id_no_other_reply_will_have() {
    // `call_0` was made up for every reply, so the next turn's result could land on an older call
    let text = sse(&[tcs(vec![json!({"index":0,"function":{"name":"read","arguments":"{}"}})]), fin("tool_calls")]);
    let (a, _) = run("openai-chat", &text, 0).await;
    let (b, _) = run("openai-chat", &text, 0).await;
    assert_eq!(ids(&a).len(), 1);
    assert!(!ids(&a)[0].is_empty());
    assert_ne!(ids(&a), ids(&b));
}

#[tokio::test]
async fn t3_chat_two_calls_that_repeat_one_id_get_distinct_ids() {
    // two results answering to one id are refused by the next request
    let text = sse(&[tcs(vec![tc(0, "same", "read", "{}"), tc(1, "same", "grep", "{}")]), fin("tool_calls")]);
    let (c, _) = run("openai-chat", &text, 0).await;
    let got = ids(&c);
    assert_eq!(got.len(), 2);
    assert_ne!(got[0], got[1]);
    assert_eq!(got[0], "same");
}

#[tokio::test]
async fn t3_chat_a_prefixed_or_odd_name_is_passed_on_unchanged() {
    // nexo does not rewrite names a server adds or the tools carry; pinned so a change is seen
    for name in ["functions.read", "mcp__srv__read-file", "read:0"] {
        let s = play("openai-chat", &sse(&[tcs(vec![tc(0, "c1", name, "{}")]), fin("tool_calls")])).await;
        assert_eq!(s.calls, vec![(name.to_string(), json!({}))]);
    }
}

#[tokio::test]
async fn t3_chat_mistral_sends_the_whole_call_in_one_chunk_and_text_before_it() {
    let text = sse(&[
        d(json!({"content":"Let me check."})),
        tcs(vec![json!({"id":"D681PevKs","function":{"name":"read","arguments":"{\"path\": \"a\"}"},"index":0})]),
        json!({"id":"c1","choices":[{"index":0,"delta":{"content":""},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":5,"completion_tokens":6,"total_tokens":11}}),
    ]);
    let s = play("openai-chat", &text).await;
    assert_eq!((s.text.as_str(), s.finish), ("Let me check.", FinishReason::ToolUse));
    assert_eq!(s.calls, vec![("read".to_string(), json!({"path":"a"}))]);
    assert_eq!(s.usage.output, 6);
}

#[tokio::test]
async fn t3_anthropic_two_parallel_calls_split_deltas_and_empty_arguments() {
    let text = a_start(json!({"input_tokens":3}))
        + &a_tool(0, "toolu_1", "read", &["", "{\"pa", "th\":\"a\"}"])
        + &a_tool(1, "toolu_2", "ping", &[])
        + &a_tool(2, "toolu_3", "ping", &[""])
        + &a_tool(3, "toolu_4", "ping", &["null"])
        + &a_end("tool_use", json!({"output_tokens":9}));
    let s = play("anthropic", &text).await;
    assert_eq!(
        s.calls,
        vec![
            ("read".to_string(), json!({"path":"a"})),
            ("ping".to_string(), json!({})),
            ("ping".to_string(), json!({})),
            ("ping".to_string(), json!({})),
        ]
    );
    assert_eq!(s.finish, FinishReason::ToolUse);
    let (c, _) = run("anthropic", &text, 0).await;
    assert_eq!(ids(&c), ["toolu_1", "toolu_2", "toolu_3", "toolu_4"]);
}

#[tokio::test]
async fn t3_gemini_parallel_calls_get_distinct_ids_and_missing_args_are_an_empty_object() {
    let text = data(&[json!({"candidates":[{"content":{"role":"model","parts":[
        {"functionCall":{"name":"read","args":{"path":"a"}},"thoughtSignature":"TS"},
        {"functionCall":{"name":"ping"}},
        {"functionCall":{"name":"ping","args":null}},
        {"functionCall":{"name":"read","args":{"path":"b"}}}
    ]},"finishReason":"STOP"}]})]);
    let s = play("gemini", &text).await;
    assert_eq!(
        s.calls,
        vec![
            ("read".to_string(), json!({"path":"a"})),
            ("ping".to_string(), json!({})),
            ("ping".to_string(), json!({})),
            ("read".to_string(), json!({"path":"b"})),
        ]
    );
    assert_eq!(s.finish, FinishReason::ToolUse);
    let (c, _) = run("gemini", &text, 0).await;
    let got = ids(&c);
    let mut unique = got.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 4, "{got:?}");
}

#[tokio::test]
async fn t3_responses_two_calls_split_deltas_and_empty_or_null_arguments() {
    let text = r_events(&[
        r_created(),
        r_call(0, "call_1", "read"),
        r_args(0, "{\"pa"),
        r_args(0, "th\":\"a\"}"),
        r_args_done(0, "{\"path\":\"a\"}"),
        r_call(1, "call_2", "ping"),
        r_args_done(1, ""),
        r_call(2, "call_3", "ping"),
        r_args_done(2, "null"),
        r_done(json!({"input_tokens":3,"output_tokens":9})),
    ]);
    let s = play("openai-responses", &text).await;
    assert_eq!(
        s.calls,
        vec![
            ("read".to_string(), json!({"path":"a"})),
            ("ping".to_string(), json!({})),
            ("ping".to_string(), json!({})),
        ]
    );
    assert_eq!(s.finish, FinishReason::ToolUse);
}

// ================= t4: usage =================

#[tokio::test]
async fn t4_chat_usage_in_the_last_chunk_after_finish_with_openai_cache_and_reasoning_fields() {
    let text = sse(&[
        d(json!({"content":"x"})),
        fin("stop"),
        usage_chunk(json!({"prompt_tokens":100,"completion_tokens":30,"total_tokens":130,
            "prompt_tokens_details":{"cached_tokens":64},"completion_tokens_details":{"reasoning_tokens":20}})),
    ]);
    let s = play("openai-chat", &text).await;
    assert_eq!(s.usage, Usage { input: 36, output: 30, cache_read: 64, cache_write: 0, reasoning: 20 });
}

#[tokio::test]
async fn t4_chat_usage_in_the_finish_chunk_and_running_counts_end_with_the_final_ones() {
    let mut last = fin("stop");
    last["usage"] = json!({"prompt_tokens":10,"completion_tokens":7});
    let mut early = d(json!({"content":"x"}));
    early["usage"] = json!({"prompt_tokens":10,"completion_tokens":1});
    let s = play("openai-chat", &sse(&[early, last])).await;
    assert_eq!(s.usage, Usage { input: 10, output: 7, ..Usage::default() });
}

#[tokio::test]
async fn t4_chat_no_usage_anywhere_is_zero_and_not_an_error() {
    let s = play("openai-chat", &sse(&[d(json!({"content":"x"})), fin("stop")])).await;
    assert_eq!((s.usage, s.error), (Usage::default(), None));
}

#[tokio::test]
async fn t4_chat_cached_token_field_names_by_vendor() {
    let cases = [
        // OpenAI, Azure, xAI, Groq, Cerebras, Together, Fireworks, OpenRouter
        (json!({"prompt_tokens":100,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":40}}), 40),
        // DeepSeek
        (
            json!({"prompt_tokens":100,"completion_tokens":5,"prompt_cache_hit_tokens":40,"prompt_cache_miss_tokens":60}),
            40,
        ),
        // Moonshot: a top-level `cached_tokens`
        (json!({"prompt_tokens":100,"completion_tokens":5,"cached_tokens":40}), 40),
        // no cache field
        (json!({"prompt_tokens":100,"completion_tokens":5}), 0),
        // a server that counts in floats
        (json!({"prompt_tokens":100.0,"completion_tokens":5.0,"prompt_tokens_details":{"cached_tokens":40.0}}), 40),
    ];
    for (u, cached) in cases {
        let s = play("openai-chat", &sse(&[d(json!({"content":"x"})), fin("stop"), usage_chunk(u.clone())])).await;
        assert_eq!(s.usage, Usage { input: 100 - cached, output: 5, cache_read: cached, ..Usage::default() }, "{u}");
    }
}

#[tokio::test]
async fn t4_chat_groq_usage_lives_in_x_groq() {
    // before: the counts were dropped and the session showed 0 tokens
    let mut last = fin("stop");
    last["x_groq"] =
        json!({"id":"req_1","usage":{"queue_time":0.01,"prompt_tokens":20,"completion_tokens":5,"total_tokens":25}});
    let s = play("openai-chat", &sse(&[d(json!({"content":"x"})), last])).await;
    assert_eq!(s.usage, Usage { input: 20, output: 5, ..Usage::default() });
}

#[tokio::test]
async fn t4_chat_moonshot_usage_lives_inside_the_choice() {
    let mut last = fin("stop");
    last["choices"][0]["usage"] = json!({"prompt_tokens":10,"completion_tokens":2,"total_tokens":12,"cached_tokens":6});
    let s = play("openai-chat", &sse(&[d(json!({"content":"x"})), last])).await;
    assert_eq!(s.usage, Usage { input: 4, output: 2, cache_read: 6, ..Usage::default() });
}

#[tokio::test]
async fn t4_chat_llamacpp_extra_timings_and_empty_choices_chunk_are_ignored() {
    let mut u = usage_chunk(json!({"prompt_tokens":8,"completion_tokens":2,"total_tokens":10}));
    u["timings"] = json!({"prompt_n":8,"predicted_n":2,"predicted_per_second":41.5});
    let s = play("openai-chat", &sse(&[d(json!({"content":"x"})), fin("stop"), u])).await;
    assert_eq!(s.usage, Usage { input: 8, output: 2, ..Usage::default() });
}

#[tokio::test]
async fn t4_anthropic_usage_start_and_delta_with_cache_fields() {
    let text = a_start(
        json!({"input_tokens":12,"output_tokens":1,"cache_read_input_tokens":4,"cache_creation_input_tokens":2}),
    ) + &a_text(0, "x")
        + &a_end("end_turn", json!({"output_tokens":30}));
    let s = play("anthropic", &text).await;
    assert_eq!(s.usage, Usage { input: 12, output: 30, cache_read: 4, cache_write: 2, reasoning: 0 });
}

#[tokio::test]
async fn t4_gemini_usage_cached_and_thought_tokens_in_a_chunk_of_their_own() {
    let text = data(&[
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"x"}]},"finishReason":"STOP"}]}),
        json!({"usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":40,"candidatesTokenCount":5,"thoughtsTokenCount":20,"totalTokenCount":125}}),
    ]);
    let s = play("gemini", &text).await;
    assert_eq!(s.usage, Usage { input: 60, output: 25, cache_read: 40, cache_write: 0, reasoning: 20 });
    assert_eq!(s.finish, FinishReason::Stop);
}

#[tokio::test]
async fn t4_responses_usage_with_cached_reasoning_and_float_counts() {
    let text = r_events(&[
        r_created(),
        json!({"type":"response.output_text.delta","output_index":0,"delta":"x"}),
        r_done(json!({"input_tokens":100.0,"input_tokens_details":{"cached_tokens":60.0},"output_tokens":9.0,
            "output_tokens_details":{"reasoning_tokens":4.0}})),
    ]);
    let s = play("openai-responses", &text).await;
    assert_eq!(s.usage, Usage { input: 40, output: 9, cache_read: 60, cache_write: 0, reasoning: 4 });
}

// ================= t5: finish reasons =================

#[tokio::test]
async fn t5_chat_finish_reasons_by_vendor() {
    let table = [
        ("stop", FinishReason::Stop),
        ("length", FinishReason::Length),
        ("content_filter", FinishReason::ContentFilter),
        // Mistral
        ("model_length", FinishReason::Length),
        // Z.ai
        ("model_context_window_exceeded", FinishReason::Length),
        ("sensitive", FinishReason::ContentFilter),
        // a word nobody documents: the reply is whole, so it is a stop
        ("end_turn", FinishReason::Stop),
        ("eos", FinishReason::Stop),
    ];
    for (word, want) in table {
        let s = play("openai-chat", &sse(&[d(json!({"content":"x"})), fin(word)])).await;
        assert_eq!(s.finish, want, "{word}");
        assert_eq!(s.error, None, "{word}");
    }
}

#[tokio::test]
async fn t5_chat_tool_calls_and_function_call_are_a_tool_round_only_when_a_call_came() {
    for word in ["tool_calls", "function_call"] {
        let s = play("openai-chat", &sse(&[tcs(vec![tc(0, "c1", "read", "{}")]), fin(word)])).await;
        assert_eq!(s.finish, FinishReason::ToolUse, "{word}");
        let s = play("openai-chat", &sse(&[d(json!({"content":"x"})), fin(word)])).await;
        assert_eq!(s.finish, FinishReason::Stop, "{word} without a call");
    }
}

#[tokio::test]
async fn t5_chat_length_with_a_cut_call_keeps_the_finish_reason_for_the_agent() {
    // the agent decides what to do with a call cut by `length` (nexo-agent turn/round.rs)
    let text = sse(&[tcs(vec![tc(0, "c1", "write", "{\"path\":\"a\",\"content\":\"abc")]), fin("length")]);
    let s = play("openai-chat", &text).await;
    assert_eq!(s.finish, FinishReason::Length);
}

#[tokio::test]
async fn t5_chat_zai_network_error_and_openrouter_error_are_failed_attempts_not_empty_replies() {
    // before: `Finish(Stop)` with nothing said, and the person got a blank turn
    for word in ["network_error", "error"] {
        let (c, _) = run("openai-chat", &sse(&[d(json!({"content":"par"})), fin(word)]), 0).await;
        let e = c.error.as_ref().unwrap_or_else(|| panic!("{word}: no error"));
        assert_eq!(e.kind, ErrorKind::Network, "{word}");
        assert!(e.message.contains(word), "{}", e.message);
    }
}

#[tokio::test]
async fn t5_anthropic_stop_reasons() {
    let table = [
        ("end_turn", FinishReason::Stop),
        ("stop_sequence", FinishReason::Stop),
        ("max_tokens", FinishReason::Length),
        ("model_context_window_exceeded", FinishReason::Length),
        ("refusal", FinishReason::ContentFilter),
        // no server tools are sent, so a pause is not expected; treated as the end of the reply
        ("pause_turn", FinishReason::Stop),
    ];
    for (word, want) in table {
        let text = a_start(json!({"input_tokens":1})) + &a_text(0, "x") + &a_end(word, json!({"output_tokens":1}));
        assert_eq!(play("anthropic", &text).await.finish, want, "{word}");
    }
    let text = a_start(json!({"input_tokens":1}))
        + &a_tool(0, "t1", "read", &["{}"])
        + &a_end("tool_use", json!({"output_tokens":1}));
    assert_eq!(play("anthropic", &text).await.finish, FinishReason::ToolUse);
}

#[tokio::test]
async fn t5_gemini_finish_reasons_and_a_blocked_prompt() {
    for (word, want) in [
        ("STOP", FinishReason::Stop),
        ("MAX_TOKENS", FinishReason::Length),
        ("SAFETY", FinishReason::ContentFilter),
        ("RECITATION", FinishReason::ContentFilter),
        ("PROHIBITED_CONTENT", FinishReason::ContentFilter),
    ] {
        let text =
            data(&[json!({"candidates":[{"content":{"role":"model","parts":[{"text":"x"}]},"finishReason":word}]})]);
        assert_eq!(play("gemini", &text).await.finish, want, "{word}");
    }
    // a call the model wrote badly is a failed attempt the client sends again
    let text = data(&[json!({"candidates":[{"finishReason":"MALFORMED_FUNCTION_CALL"}]})]);
    let (c, _) = run("gemini", &text, 0).await;
    assert_eq!(c.error.as_ref().map(|e| e.kind), Some(ErrorKind::Server));
    // a blocked prompt arrives without candidates
    let text = data(&[json!({"promptFeedback":{"blockReason":"SAFETY"}})]);
    let (c, _) = run("gemini", &text, 0).await;
    assert!(c.error.as_ref().is_some_and(|e| e.message.contains("blocked: SAFETY")), "{:?}", c.error);
}

#[tokio::test]
async fn t5_responses_completed_incomplete_and_failed() {
    let done = |reason: Option<&str>| {
        let mut r = r_done(json!({"input_tokens":1,"output_tokens":1}));
        if let Some(reason) = reason {
            r["type"] = json!("response.incomplete");
            r["response"]["incomplete_details"] = json!({ "reason": reason });
        }
        r_events(&[r_created(), json!({"type":"response.output_text.delta","output_index":0,"delta":"x"}), r])
    };
    assert_eq!(play("openai-responses", &done(None)).await.finish, FinishReason::Stop);
    assert_eq!(play("openai-responses", &done(Some("max_output_tokens"))).await.finish, FinishReason::Length);
    assert_eq!(play("openai-responses", &done(Some("content_filter"))).await.finish, FinishReason::ContentFilter);
    let failed = r_events(&[
        r_created(),
        json!({"type":"response.failed","response":{"id":"r","error":{"code":"server_error","message":"boom"}}}),
    ]);
    let (c, _) = run("openai-responses", &failed, 0).await;
    assert!(c.error.as_ref().is_some_and(|e| e.message.contains("boom")), "{:?}", c.error);
}

#[tokio::test]
async fn t5_a_refusal_is_read_by_the_person_in_chat_and_in_responses() {
    // before: `delta.refusal` and `response.refusal.delta` were ignored, so a refusal was an empty reply
    let text = sse(&[d(json!({"content":null,"refusal":"I can't help with that."})), fin("stop")]);
    let s = play("openai-chat", &text).await;
    assert_eq!(s.text, "I can't help with that.");
    let text = r_events(&[
        r_created(),
        json!({"type":"response.refusal.delta","output_index":0,"delta":"I can't "}),
        json!({"type":"response.refusal.delta","output_index":0,"delta":"help."}),
        json!({"type":"response.refusal.done","output_index":0,"refusal":"I can't help."}),
        r_done(json!({"input_tokens":1,"output_tokens":1})),
    ]);
    assert_eq!(play("openai-responses", &text).await.text, "I can't help.");
}

// ================= t6: stream oddities =================

#[tokio::test]
async fn t6_chat_keep_alive_comments_crlf_no_space_after_data_and_junk_lines() {
    let hello = d(json!({"content":"hi"}));
    let text = format!(
        ": OPENROUTER PROCESSING\r\n\r\n: keep-alive\r\n\r\ndata:{hello}\r\n\r\ndata: this is not json\r\n\r\ndata: {}\r\n\r\ndata: [DONE]\r\n\r\n",
        fin("stop")
    );
    let s = play("openai-chat", &text).await;
    assert_eq!((s.text.as_str(), s.finish, s.error), ("hi", FinishReason::Stop, None));
}

#[tokio::test]
async fn t6_chat_stream_ends_without_done_and_without_finish_reason() {
    let text = format!("data: {}\n\n", d(json!({"content":"hi"})));
    let (c, evs) = run("openai-chat", &text, 0).await;
    assert_eq!(c.message().text(), "hi");
    assert!(c.error.is_none(), "{:?}", c.error);
    assert!(matches!(evs.last(), Some(StreamEvent::Done)));
}

#[tokio::test]
async fn t6_chat_openrouter_mid_stream_error_keeps_the_text_and_drops_the_open_call() {
    let err = json!({"id":"c1","error":{"code":"server_error","message":"Provider disconnected"},
        "choices":[{"index":0,"delta":{"content":""},"finish_reason":"error"}]});
    let text = sse(&[d(json!({"content":"part"})), tcs(vec![tc(0, "c1", "write", "{\"pa")]), err]);
    let (c, evs) = run("openai-chat", &text, 0).await;
    assert_eq!(c.message().text(), "part");
    assert!(!evs.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { .. })), "a call cut by the error is not closed");
    let e = c.error.expect("an error");
    assert!(e.message.contains("Provider disconnected"), "{}", e.message);
}

#[tokio::test]
async fn t6_chat_azure_style_first_chunk_with_no_choices_and_filter_results() {
    let first = json!({"id":"","object":"","created":0,"model":"","prompt_filter_results":[{"prompt_index":0,"content_filter_results":{}}],"choices":[]});
    let mut mid = d(json!({"content":"ok"}));
    mid["choices"][0]["content_filter_results"] = json!({"hate":{"filtered":false,"severity":"safe"}});
    let s = play("openai-chat", &sse(&[first, mid, fin("stop")])).await;
    assert_eq!((s.text.as_str(), s.finish), ("ok", FinishReason::Stop));
}

#[tokio::test]
async fn t6_chat_a_second_choice_or_a_null_delta_does_not_break_the_reply() {
    let odd = json!({"id":"c1","choices":[{"index":0,"delta":null}]});
    let mut two = d(json!({"content":"hi"}));
    two["choices"].as_array_mut().unwrap().push(json!({"index":1,"delta":{"content":"x"}}));
    // a gateway that lists choice 1 before choice 0
    let mut flipped = d(json!({"content":"!"}));
    flipped["choices"].as_array_mut().unwrap().insert(0, json!({"index":1,"delta":{"content":"x"}}));
    let text = sse(&[odd, two, flipped, fin("stop")]);
    let s = play("openai-chat", &text).await;
    assert_eq!((s.text.as_str(), s.error), ("hi!", None));
}

#[tokio::test]
async fn t6_anthropic_ping_unknown_events_and_an_overloaded_error_event() {
    let text = a_start(json!({"input_tokens":1}))
        + &ev("ping", json!({"type":"ping"}))
        + &ev("something_new", json!({"type":"something_new","x":1}))
        + &a_text(0, "par")
        + &ev("error", json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}));
    let (c, _) = run("anthropic", &text, 0).await;
    assert_eq!(c.message().text(), "par");
    let e = c.error.expect("an error");
    assert_eq!(e.kind, ErrorKind::Overloaded);
}

#[tokio::test]
async fn t6_gemini_empty_candidate_chunks_and_unknown_fields_are_skipped() {
    let text = data(&[
        json!({"candidates":[{"index":0}],"modelVersion":"g","responseId":"r"}),
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"hi"}]},"safetyRatings":[{"category":"X","probability":"NEGLIGIBLE"}]}]}),
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"!"}]},"finishReason":"STOP"}]}),
    ]);
    let s = play("gemini", &text).await;
    assert_eq!((s.text.as_str(), s.finish, s.error), ("hi!", FinishReason::Stop, None));
}

#[tokio::test]
async fn t6_responses_unknown_events_and_a_plain_error_event() {
    let text = r_events(&[
        r_created(),
        json!({"type":"response.in_progress","response":{"id":"resp_1"}}),
        json!({"type":"response.content_part.added","output_index":0}),
        json!({"type":"response.output_text.delta","output_index":0,"delta":"par"}),
        json!({"type":"error","code":"rate_limit_exceeded","message":"slow down"}),
    ]);
    let (c, _) = run("openai-responses", &text, 0).await;
    assert_eq!(c.message().text(), "par");
    assert_eq!(c.error.map(|e| e.kind), Some(ErrorKind::RateLimit));
}

// ================= t7: request shape =================

fn api(model: &str) -> (ProviderConfig, ModelRequest) {
    (cfg("openai-chat", "https://api.openai.com/v1"), req(model))
}

#[test]
fn t7_chat_url_of_every_preset_is_the_one_the_vendor_documents() {
    let want = [
        ("openai", "https://api.openai.com/v1/chat/completions"),
        ("deepseek", "https://api.deepseek.com/v1/chat/completions"),
        ("xai", "https://api.x.ai/v1/chat/completions"),
        ("groq", "https://api.groq.com/openai/v1/chat/completions"),
        ("mistral", "https://api.mistral.ai/v1/chat/completions"),
        ("openrouter", "https://openrouter.ai/api/v1/chat/completions"),
        ("together", "https://api.together.xyz/v1/chat/completions"),
        ("fireworks", "https://api.fireworks.ai/inference/v1/chat/completions"),
        ("cerebras", "https://api.cerebras.ai/v1/chat/completions"),
        ("moonshot", "https://api.moonshot.ai/v1/chat/completions"),
        ("qwen", "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions"),
        ("zhipu", "https://api.z.ai/api/paas/v4/chat/completions"),
        ("ollama", "http://localhost:11434/v1/chat/completions"),
        ("lmstudio", "http://localhost:1234/v1/chat/completions"),
        ("vllm", "http://localhost:8000/v1/chat/completions"),
        ("llamacpp", "http://localhost:8080/v1/chat/completions"),
    ];
    let all = nexo_providers::presets();
    for (name, url) in want {
        let c = all.get(name).unwrap_or_else(|| panic!("no preset {name}"));
        assert_eq!(c.protocol, "openai-chat", "{name}");
        assert_eq!(OpenAiChat::url(c), url, "{name}");
    }
    assert_eq!(Anthropic::url(&all["anthropic"]), "https://api.anthropic.com/v1/messages");
    assert_eq!(
        Gemini::url(&all["gemini"], "gemini-2.5-pro"),
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
    );
    assert_eq!(
        OpenAiResponses::url(&all["codex"]),
        format!("{}/responses", all["codex"].base_url.trim_end_matches('/'))
    );
}

#[test]
fn t7_every_preset_and_directory_entry_has_a_registered_protocol_and_a_well_formed_url() {
    let reg = registry();
    let mut configs: Vec<(String, ProviderConfig)> = nexo_providers::presets().into_iter().collect();
    configs.extend(nexo_providers::directory::all().iter().map(|e| (e.id.to_string(), e.to_config())));
    assert!(configs.len() > 50, "the directory is loaded: {}", configs.len());
    for (name, c) in configs {
        assert!(reg.get(&c.protocol).is_some(), "{name}: protocol {:?} is not registered", c.protocol);
        let u = reqwest::Url::parse(&c.base_url).unwrap_or_else(|e| panic!("{name}: {} ({e})", c.base_url));
        assert!(matches!(u.scheme(), "http" | "https"), "{name}: {}", c.base_url);
        assert_eq!(c.base_url.trim(), c.base_url, "{name}");
        if c.protocol == "openai-chat" {
            let url = OpenAiChat::url(&c);
            let path = url.split_once("://").map_or(url.as_str(), |(_, rest)| rest);
            assert!(url.ends_with("/chat/completions") && !path.contains("//"), "{name}: {url}");
        }
    }
}

#[test]
fn t7_chat_openai_uses_max_completion_tokens_for_o_series_and_gpt5_and_max_tokens_elsewhere() {
    for (model, key) in [
        ("gpt-5", "max_completion_tokens"),
        ("gpt-5-mini", "max_completion_tokens"),
        ("o3", "max_completion_tokens"),
        ("o4-mini", "max_completion_tokens"),
        ("gpt-4.1", "max_tokens"),
        ("orca-mini", "max_tokens"),
        ("deepseek-chat", "max_tokens"),
        ("qwen3:8b", "max_tokens"),
    ] {
        let (c, r) = api(model);
        let b = OpenAiChat::body(&c, &r);
        assert_eq!(b[key], 8192, "{model}");
        let other = if key == "max_tokens" { "max_completion_tokens" } else { "max_tokens" };
        assert!(b.get(other).is_none(), "{model}");
    }
    let (mut c, r) = api("gpt-5");
    c.extra.insert("max_tokens_key".into(), toml::Value::String("max_tokens".into()));
    assert_eq!(OpenAiChat::body(&c, &r)["max_tokens"], 8192);
}

#[test]
fn t7_chat_no_temperature_to_openai_reasoning_models_but_to_everyone_else() {
    // before: review.rs asks for 0.0 and the retry changes it; `gpt-5` and `o3` answer 400 to any value
    for (base, model, sent) in [
        ("https://api.openai.com/v1", "gpt-5", false),
        ("https://api.openai.com/v1", "o3", false),
        ("https://api.openai.com/v1", "o4-mini", false),
        ("https://api.openai.com/v1", "gpt-5-chat-latest", true),
        ("https://api.openai.com/v1", "gpt-4.1", true),
        ("https://openrouter.ai/api/v1", "openai/gpt-5", true),
        ("https://api.deepseek.com", "deepseek-reasoner", true),
        ("http://localhost:11434/v1", "gpt-oss:20b", true),
    ] {
        let mut r = req(model);
        r.temperature = Some(0.0);
        let b = OpenAiChat::body(&cfg("openai-chat", base), &r);
        assert_eq!(b.get("temperature").is_some(), sent, "{base} {model}");
        let b = OpenAiResponses::body(&cfg("openai-responses", base), &r);
        assert_eq!(b.get("temperature").is_some(), sent, "responses {base} {model}");
    }
}

#[test]
fn t7_chat_stream_usage_system_role_and_stop_fields() {
    let (c, mut r) = api("gpt-4.1");
    r.stop = vec!["END".into()];
    let b = OpenAiChat::body(&c, &r);
    assert_eq!(b["stream"], true);
    assert_eq!(b["stream_options"], json!({"include_usage": true}));
    assert_eq!(b["messages"][0], json!({"role":"system","content":"sys"}));
    assert_eq!(b["stop"], json!(["END"]));
    let mut dev = c.clone();
    dev.extra.insert("system_role".into(), toml::Value::String("developer".into()));
    assert_eq!(OpenAiChat::body(&dev, &r)["messages"][0]["role"], "developer");
}

#[test]
fn t7_chat_prompt_cache_key_goes_to_openai_only() {
    for (base, sent) in [
        ("https://api.openai.com/v1", true),
        ("https://api.mistral.ai/v1", false),
        ("https://api.groq.com/openai/v1", false),
        ("https://openrouter.ai/api/v1", false),
        ("http://localhost:11434/v1", false),
    ] {
        let mut r = req("m");
        r.session_id = Some("sess-1".into());
        assert_eq!(OpenAiChat::body(&cfg("openai-chat", base), &r).get("prompt_cache_key").is_some(), sent, "{base}");
    }
}

#[test]
fn t7_chat_reasoning_effort_is_sent_only_when_asked_and_reasoning_is_not_replayed_by_default() {
    let (c, mut r) = api("gpt-5");
    assert!(OpenAiChat::body(&c, &r).get("reasoning_effort").is_none());
    r.reasoning = Some(ReasoningConfig::Effort(Effort::High));
    assert_eq!(OpenAiChat::body(&c, &r)["reasoning_effort"], "high");
    r.reasoning = Some(ReasoningConfig::Off);
    assert!(OpenAiChat::body(&c, &r).get("reasoning_effort").is_none());
    // earlier reasoning stays out of the history: DeepSeek R1 answers 400 to `reasoning_content` in input
    r.messages.push(Message::new(
        Role::Assistant,
        vec![
            Part::Reasoning { text: "secret plan".into(), signature: None, encrypted: None, model: None },
            Part::text("answer"),
        ],
    ));
    let b = OpenAiChat::body(&c, &r);
    assert!(!b.to_string().contains("secret plan"), "{b}");
    let mut on = c.clone();
    on.extra.insert("send_reasoning".into(), toml::Value::Boolean(true));
    assert_eq!(OpenAiChat::body(&on, &r)["messages"][2]["reasoning_content"], "secret plan");
}

#[test]
fn t7_chat_tools_tool_results_and_assistant_calls_have_the_documented_shape() {
    let (c, mut r) = api("gpt-4.1");
    r.tools = vec![spec("read", json!({"type":"object","properties":{"path":{"type":"string"}}}))];
    r.messages.push(Message::new(Role::Assistant, vec![Part::tool_call("call_1", "read", json!({"path":"a"}))]));
    r.messages.push(Message::tool_results(vec![Part::tool_result("call_1", "boom", true)]));
    let b = OpenAiChat::body(&c, &r);
    assert_eq!(b["tools"][0]["type"], "function");
    assert_eq!(b["tools"][0]["function"]["name"], "read");
    assert!(b.get("tool_choice").is_none());
    let m = &b["messages"];
    assert_eq!(m[2]["role"], "assistant");
    assert!(m[2]["content"].is_null(), "null content next to calls: {}", m[2]);
    assert_eq!(m[2]["tool_calls"][0]["function"]["arguments"], "{\"path\":\"a\"}");
    assert_eq!(m[3], json!({"role":"tool","tool_call_id":"call_1","content":"Error: boom"}));
    r.no_tool_call = true;
    assert_eq!(OpenAiChat::body(&c, &r)["tool_choice"], "none");
}

#[test]
fn t7_chat_mistral_tool_call_ids_are_nine_letters_and_digits_and_the_pair_still_meets() {
    // before: `call_0` and ids from another provider went out as they were, and Mistral answers 400
    // ("Tool call id was call_0 but must be a-z, A-Z, 0-9, with a length of 9")
    let mut r = req("mistral-large-latest");
    r.messages.push(Message::new(
        Role::Assistant,
        vec![Part::tool_call("call_0", "read", json!({})), Part::tool_call("D681PevKs", "grep", json!({}))],
    ));
    r.messages.push(Message::tool_results(vec![
        Part::tool_result("call_0", "a", false),
        Part::tool_result("D681PevKs", "b", false),
    ]));
    let b = OpenAiChat::body(&cfg("openai-chat", "https://api.mistral.ai/v1"), &r);
    let call_ids: Vec<&str> =
        b["messages"][2]["tool_calls"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    let result_ids =
        [b["messages"][3]["tool_call_id"].as_str().unwrap(), b["messages"][4]["tool_call_id"].as_str().unwrap()];
    for id in call_ids.iter().chain(result_ids.iter()) {
        assert!(id.len() == 9 && id.bytes().all(|c| c.is_ascii_alphanumeric()), "{id}");
    }
    assert_eq!(call_ids, result_ids, "each result keeps the id of its call");
    assert_ne!(call_ids[0], call_ids[1]);
    assert_eq!(call_ids[1], "D681PevKs", "an id that already fits is not touched");
    // every other vendor gets the ids as they are
    let b = OpenAiChat::body(&cfg("openai-chat", "https://api.openai.com/v1"), &r);
    assert_eq!(b["messages"][2]["tool_calls"][0]["id"], "call_0");
}

#[test]
fn t7_chat_mistral_leaves_messages_without_calls_untouched_no_null_tool_calls() {
    // before: the id mapping indexed `tool_calls` mutably, which writes `"tool_calls": null` into every message
    let mut r = req("mistral-large-latest");
    r.messages.push(Message::assistant("plain"));
    r.messages.push(Message::user("again"));
    let mistral = cfg("openai-chat", "https://api.mistral.ai/v1");
    let generic = cfg("openai-chat", "https://example.com/v1");
    let want = serde_json::to_string(&OpenAiChat::body(&generic, &r)["messages"]).unwrap();
    let got = serde_json::to_string(&OpenAiChat::body(&mistral, &r)["messages"]).unwrap();
    assert_eq!(got, want, "no tool calls anywhere: the messages are the same bytes");
    r.messages.push(Message::new(Role::Assistant, vec![Part::tool_call("call_0", "read", json!({}))]));
    r.messages.push(Message::tool_results(vec![Part::tool_result("call_0", "a", false)]));
    let b = OpenAiChat::body(&mistral, &r);
    let m = b["messages"].as_array().unwrap();
    assert_eq!(m.len(), 6);
    for (i, msg) in m.iter().enumerate() {
        assert_eq!(msg.get("tool_calls").is_some(), i == 4, "message {i}: {msg}");
    }
}
#[test]
fn t7_responses_body_store_off_strict_off_encrypted_reasoning_and_input_items() {
    let c = cfg("openai-responses", "https://api.openai.com/v1");
    let mut r = req("gpt-5");
    r.tools = vec![spec("read", json!({"type":"object","properties":{}}))];
    r.reasoning = Some(ReasoningConfig::Effort(Effort::Medium));
    r.session_id = Some("s1".into());
    r.messages.push(Message::new(Role::Assistant, vec![Part::tool_call("call_1", "read", json!({"p":1}))]));
    r.messages.push(Message::tool_results(vec![Part::tool_result("call_1", "ok", false)]));
    let b = OpenAiResponses::body(&c, &r);
    assert_eq!(b["store"], false);
    assert_eq!(b["tools"][0]["strict"], false);
    assert_eq!(b["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(b["prompt_cache_key"], "s1");
    assert_eq!(b["max_output_tokens"], 8192);
    let input = b["input"].as_array().unwrap();
    assert!(input.iter().any(|i| i["type"] == "function_call" && i["call_id"] == "call_1"), "{input:?}");
    assert!(input.iter().any(|i| i["type"] == "function_call_output" && i["call_id"] == "call_1"), "{input:?}");
    // a server that is not OpenAI gets no cache key
    let b = OpenAiResponses::body(&cfg("openai-responses", "https://example.org/v1"), &r);
    assert!(b.get("prompt_cache_key").is_none());
}

#[test]
fn t7_responses_a_codex_login_omits_max_tokens_and_temperature_and_keeps_instructions() {
    let c = nexo_providers::presets()["codex"].clone();
    let mut r = ModelRequest::new("gpt-5.5", "", vec![Message::user("hi")]);
    r.temperature = Some(0.2);
    let b = OpenAiResponses::body(&c, &r);
    assert!(b.get("max_output_tokens").is_none() && b.get("temperature").is_none(), "{b}");
    assert_eq!(b["instructions"], "");
}

#[test]
fn t7_anthropic_body_tools_cache_thinking_and_blank_text_blocks() {
    let c = cfg("anthropic", "https://api.anthropic.com");
    let mut r = req("claude-sonnet-4-5");
    r.tools = vec![spec("read", json!({"type":"object","properties":{}}))];
    r.temperature = Some(0.3);
    r.reasoning = Some(ReasoningConfig::Effort(Effort::Low));
    // before: a blank text block next to a call went out as it was and the API answered 400
    r.messages
        .push(Message::new(Role::Assistant, vec![Part::text("\n\n"), Part::tool_call("toolu_1", "read", json!({}))]));
    r.messages.push(Message::tool_results(vec![Part::tool_result("toolu_1", "ok", false)]));
    let b = Anthropic::body(&c, &r);
    assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
    assert_eq!(b["thinking"], json!({"type":"enabled","budget_tokens":2048}));
    assert!(b.get("temperature").is_none(), "not allowed with thinking");
    assert_eq!(b["messages"][1]["content"], json!([{"type":"tool_use","id":"toolu_1","name":"read","input":{}}]));
    assert_eq!(b["messages"][2]["content"][0]["type"], "tool_result");
    assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
    let modern = Anthropic::body(&c, &{
        let mut m = r.clone();
        m.model = "claude-sonnet-5-5".into();
        m
    });
    assert_eq!(modern["thinking"]["type"], "adaptive");
    assert!(modern.get("temperature").is_none());
}

#[test]
fn t7_gemini_body_a_tool_without_arguments_has_no_parameters_and_schemas_are_cleaned() {
    // before: `parameters: {"type":"object","properties":{}}` made the API answer 400
    // ("properties: should be non-empty for OBJECT type"); the setup ping tool has this shape
    let c = cfg("gemini", "https://generativelanguage.googleapis.com");
    let mut r = req("gemini-2.5-flash");
    r.tools = vec![
        spec("ping", json!({"type":"object","properties":{}})),
        spec("bare", json!({"type":"object"})),
        spec(
            "read",
            json!({"type":"object","additionalProperties":false,"properties":{"path":{"type":"string"}},"required":["path"]}),
        ),
    ];
    let b = Gemini::body(&c, &r);
    let decls = &b["tools"][0]["functionDeclarations"];
    assert!(decls[0].get("parameters").is_none(), "{}", decls[0]);
    assert!(decls[1].get("parameters").is_none(), "{}", decls[1]);
    assert_eq!(decls[2]["parameters"]["properties"]["path"]["type"], "string");
    assert!(!decls[2].to_string().contains("additionalProperties"), "{}", decls[2]);
    assert_eq!(b["systemInstruction"]["parts"][0]["text"], "sys");
    assert_eq!(b["generationConfig"]["maxOutputTokens"], 8192);
}

#[test]
fn t7_gemini_results_are_named_after_their_calls() {
    let c = cfg("gemini", "https://generativelanguage.googleapis.com");
    let mut r = req("gemini-2.5-flash");
    r.messages.push(Message::new(Role::Assistant, vec![Part::tool_call("id1", "read", json!({"p":1}))]));
    r.messages.push(Message::tool_results(vec![Part::tool_result("id1", "ok", false)]));
    let b = Gemini::body(&c, &r);
    assert_eq!(b["contents"][1]["parts"][0]["functionCall"]["name"], "read");
    assert_eq!(b["contents"][2]["parts"][0]["functionResponse"]["name"], "read");
}

// ================= t8: model list and probe =================

async fn list(server: &MockServer, protocol: &str, path: &str, body: Value) -> Vec<String> {
    server.mock(|w, t| {
        w.method(GET).path(path);
        t.status(200).json_body(body);
    });
    Client::new(registry())
        .list_models(&cfg(protocol, &server.base_url()))
        .await
        .expect("a list")
        .into_iter()
        .map(|m| m.id)
        .collect()
}

#[tokio::test]
async fn t8_openai_style_list_in_data() {
    let s = MockServer::start();
    let got =
        list(&s, "openai-chat", "/v1/models", json!({"object":"list","data":[{"id":"a"},{"id":"b","object":"model"}]}))
            .await;
    assert_eq!(got, ["a", "b"]);
}

#[tokio::test]
async fn t8_together_answers_a_bare_array() {
    // before: an empty list, so `/model` offered nothing for Together
    let s = MockServer::start();
    let got = list(
        &s,
        "openai-chat",
        "/v1/models",
        json!([{"id":"meta-llama/Llama-3.3-70B","type":"chat"},{"id":"x/embed","type":"embedding"}]),
    )
    .await;
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got.contains(&"meta-llama/Llama-3.3-70B".to_string()));
}

#[tokio::test]
async fn t8_anthropic_and_gemini_lists() {
    let s = MockServer::start();
    let got = list(
        &s,
        "anthropic",
        "/v1/models",
        json!({"data":[{"id":"claude-x","display_name":"Claude X"}],"has_more":false}),
    )
    .await;
    assert_eq!(got, ["claude-x"]);
    let g = MockServer::start();
    let got = list(
        &g,
        "gemini",
        "/v1beta/models",
        json!({"models":[
            {"name":"models/gemini-2.5-pro","supportedGenerationMethods":["generateContent","countTokens"],"inputTokenLimit":1048576,"outputTokenLimit":65536},
            {"name":"models/text-embedding-004","supportedGenerationMethods":["embedContent"]}
        ]}),
    )
    .await;
    assert_eq!(got, ["gemini-2.5-pro"], "an embedding model is not a chat model");
}

#[tokio::test]
async fn t8_a_server_without_a_list_and_a_refused_key_are_errors_with_words_not_panics() {
    let s = MockServer::start();
    s.mock(|w, t| {
        w.method(GET).path("/v1/models");
        t.status(404).body("not found");
    });
    let e = Client::new(registry()).list_models(&cfg("openai-chat", &s.base_url())).await.unwrap_err();
    assert!(!e.message.is_empty());
    let k = MockServer::start();
    k.mock(|w, t| {
        w.method(GET).path("/v1/models");
        t.status(401).json_body(json!({"error":{"message":"invalid api key"}}));
    });
    let e = Client::new(registry()).list_models(&cfg("openai-chat", &k.base_url())).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Auth, "{e:?}");
}

#[tokio::test]
async fn t8_probe_finds_lm_studio_by_its_own_route_and_skips_embedding_models() {
    let s = MockServer::start();
    s.mock(|w, t| {
        w.method(GET).path("/api/v0/models");
        t.status(200).json_body(json!({"object":"list","data":[
            {"id":"qwen3-8b","type":"llm","state":"loaded","max_context_length":40960,"loaded_context_length":8192},
            {"id":"nomic-embed","type":"embeddings"}
        ]}));
    });
    let p = nexo_providers::probe::server(&cfg("openai-chat", &s.base_url()), None).await.unwrap();
    assert_eq!(p.kind, "lmstudio");
    assert_eq!(p.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["qwen3-8b"]);
    assert_eq!(p.base_url, format!("{}/v1", s.base_url()));
}

#[tokio::test]
async fn t8_probe_reads_a_bare_array_list_and_says_so_when_a_server_has_no_list() {
    let s = MockServer::start();
    s.mock(|w, t| {
        w.method(GET).path("/v1/models");
        t.status(200).json_body(json!([{"id":"m1"},{"id":"m2"}]));
    });
    let p = nexo_providers::probe::server(&cfg("openai-chat", &format!("{}/v1", s.base_url())), None).await.unwrap();
    assert_eq!(p.models.len(), 2, "{p:?}");
    let none = MockServer::start();
    let p = nexo_providers::probe::server(&cfg("openai-chat", &format!("{}/v1", none.base_url())), None).await.unwrap();
    assert!(p.models.is_empty() && !p.needs_key);
    assert!(p.trail.iter().any(|l| l.contains("404")), "{:?}", p.trail);
}

#[tokio::test]
async fn t8_probe_anthropic_and_gemini_need_a_key_on_401_and_400() {
    for (protocol, path, status) in [("anthropic", "/v1/models", 401), ("gemini", "/v1beta/models", 400)] {
        let s = MockServer::start();
        s.mock(|w, t| {
            w.method(GET).path(path);
            t.status(status).json_body(json!({"error":{"message":"API key not valid"}}));
        });
        let p = nexo_providers::probe::server(&cfg(protocol, &s.base_url()), None).await.unwrap();
        assert!(p.needs_key && p.models.is_empty(), "{protocol}: {p:?}");
    }
}

// ================= through the real client =================

async fn stream_through_client(c: &ProviderConfig, model: &str, with_tool: bool) -> Collector {
    let mut r = req(model);
    if with_tool {
        r.tools = vec![spec("read", json!({"type":"object","properties":{"path":{"type":"string"}}}))];
    }
    let mut s = Client::new(registry()).stream(c, &r, 0).await.expect("a stream");
    let mut col = Collector::new();
    while let Some(ev) = s.next().await {
        col.push(ev);
    }
    col
}

#[tokio::test]
async fn via_the_client_chat_text_and_a_split_tool_call() {
    let s = MockServer::start_async().await;
    let body = sse(&[
        d(json!({"role":"assistant","content":"on it"})),
        tcs(vec![tc(0, "c1", "read", "")]),
        tcs(vec![json!({"index":0,"function":{"arguments":"{\"path\":\"a\"}"}})]),
        fin("tool_calls"),
        usage_chunk(json!({"prompt_tokens":5,"completion_tokens":3})),
    ]);
    let m = s
        .mock_async(|w, t| {
            w.method(POST).path("/v1/chat/completions").json_body_includes(r#"{"stream": true}"#);
            t.status(200).header("content-type", "text/event-stream").body(body);
        })
        .await;
    let c = stream_through_client(&cfg("openai-chat", &s.base_url()), "m", true).await;
    assert!(c.error.is_none(), "{:?}", c.error);
    assert_eq!(c.message().text(), "on it");
    assert_eq!(
        c.message().tool_calls().map(|(_, n, a)| (n.to_string(), a.clone())).collect::<Vec<_>>(),
        [("read".to_string(), json!({"path":"a"}))]
    );
    assert_eq!(c.finish_reason(), FinishReason::ToolUse);
    assert_eq!(c.usage.output, 3);
    m.assert_async().await;
}

#[tokio::test]
async fn via_the_client_anthropic_gemini_and_responses_paths() {
    let a = MockServer::start_async().await;
    let ma = a
        .mock_async(|w, t| {
            w.method(POST).path("/v1/messages").header("anthropic-version", "2023-06-01");
            t.status(200).header("content-type", "text/event-stream").body(
                a_start(json!({"input_tokens":2}))
                    + &a_text(0, "hello")
                    + &a_end("end_turn", json!({"output_tokens":1})),
            );
        })
        .await;
    let c = stream_through_client(&cfg("anthropic", &a.base_url()), "claude-sonnet-4-5", false).await;
    assert_eq!((c.message().text().as_str(), c.error.is_none()), ("hello", true), "{:?}", c.error);
    ma.assert_async().await;

    let g = MockServer::start_async().await;
    let mg = g
        .mock_async(|w, t| {
            w.method(POST).path("/v1beta/models/gemini-2.5-flash:streamGenerateContent").query_param("alt", "sse");
            t.status(200).header("content-type", "text/event-stream").body(data(&[g_text("hello")]));
        })
        .await;
    let c = stream_through_client(&cfg("gemini", &g.base_url()), "gemini-2.5-flash", false).await;
    assert_eq!((c.message().text().as_str(), c.error.is_none()), ("hello", true), "{:?}", c.error);
    mg.assert_async().await;

    let r = MockServer::start_async().await;
    let mr = r
        .mock_async(|w, t| {
            w.method(POST).path("/v1/responses").json_body_includes(r#"{"store": false}"#);
            t.status(200).header("content-type", "text/event-stream").body(r_events(&[
                r_created(),
                json!({"type":"response.output_text.delta","output_index":0,"delta":"hello"}),
                r_done(json!({"input_tokens":2,"output_tokens":1})),
            ]));
        })
        .await;
    let c = stream_through_client(&cfg("openai-responses", &r.base_url()), "gpt-5", false).await;
    assert_eq!((c.message().text().as_str(), c.error.is_none()), ("hello", true), "{:?}", c.error);
    mr.assert_async().await;
}

// ================= the real local Ollama, $0 (run with --ignored) =================

fn ollama() -> ProviderConfig {
    ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None)
}

/// One text turn and one tool-call turn through nexo's own client against the local Ollama.
/// Ignored by default: it needs the server and its model on this machine.
#[tokio::test]
#[ignore = "needs the local Ollama at localhost:11434 with ornith:9b"]
async fn ollama_ornith_text_turn_and_tool_call_turn() {
    let model = "ornith:9b";
    let mut r = ModelRequest::new(model, "Answer in one short sentence.", vec![Message::user("Say hello.")]);
    r.max_tokens = 400;
    let mut s = Client::new(registry()).stream(&ollama(), &r, 0).await.expect("Ollama answers");
    let mut text = Collector::new();
    while let Some(ev) = s.next().await {
        text.push(ev);
    }
    eprintln!(
        "text turn: {:?} | reasoning {:?} | finish {:?} | usage {:?}",
        text.message().text(),
        text.message().reasoning().len(),
        text.finish_reason(),
        text.usage
    );
    assert!(text.error.is_none(), "{:?}", text.error);
    assert!(!text.message().text().trim().is_empty(), "no text in the reply");
    assert!(!text.message().text().contains("<think>"), "think tags leaked into the answer");

    let mut r = ModelRequest::new(
        model,
        "You must use the tool to read files. Never answer without calling it.",
        vec![Message::user("Read the file notes.txt.")],
    );
    r.max_tokens = 600;
    r.tools = vec![spec(
        "read_file",
        json!({"type":"object","properties":{"path":{"type":"string","description":"file to read"}},"required":["path"]}),
    )];
    let mut s = Client::new(registry()).stream(&ollama(), &r, 0).await.expect("Ollama answers");
    let mut call = Collector::new();
    while let Some(ev) = s.next().await {
        call.push(ev);
    }
    let calls: Vec<_> =
        call.message().tool_calls().map(|(i, n, a)| (i.to_string(), n.to_string(), a.clone())).collect();
    eprintln!("tool turn: {calls:?} | finish {:?} | usage {:?}", call.finish_reason(), call.usage);
    assert!(call.error.is_none(), "{:?}", call.error);
    assert_eq!(calls.len(), 1, "one call expected: {calls:?}");
    assert_eq!(calls[0].1, "read_file");
    assert!(calls[0].2["path"].is_string(), "{:?}", calls[0].2);
    assert_eq!(call.finish_reason(), FinishReason::ToolUse);
}
