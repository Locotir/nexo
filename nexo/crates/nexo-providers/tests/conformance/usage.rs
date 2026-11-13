use super::*;

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
