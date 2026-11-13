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

mod finish_reasons;
mod model_list;
mod request_shape;
mod stream_oddities;
mod stream_reasoning;
mod stream_text;
mod through_the_client;
mod tool_calls;
mod usage;
