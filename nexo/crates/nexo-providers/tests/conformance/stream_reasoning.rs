use super::*;

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
