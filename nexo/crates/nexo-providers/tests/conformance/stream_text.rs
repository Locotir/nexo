use super::*;

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
