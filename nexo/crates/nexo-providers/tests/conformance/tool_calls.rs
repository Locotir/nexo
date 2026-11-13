use super::*;

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
