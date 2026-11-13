use super::*;

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
