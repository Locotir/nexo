use super::*;

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
