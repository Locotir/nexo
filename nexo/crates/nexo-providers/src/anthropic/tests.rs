use super::messages::convert;
use super::*;
use crate::parse::Feed;
use nexo_core::{
    Effort, ErrorKind, FinishReason, ImageSource, Message, Part, ReasoningConfig, Role, StreamEvent, ToolSpec, Usage,
};
use serde_json::json;

#[test]
fn a_stream_cut_after_message_start_is_an_error_not_an_empty_reply() {
    let mut p = MessagesParser::default();
    p.feed(r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":5}}}"#);
    let end = p.finish();
    assert!(matches!(&end[0], StreamEvent::Error(e) if e.kind == ErrorKind::Network), "{end:?}");
    let mut p = MessagesParser::default();
    let evs = p.feed(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#);
    assert!(matches!(&evs[0], StreamEvent::Error(e) if e.kind == ErrorKind::Overloaded));
    assert!(p.finish().is_empty(), "nothing more after an error");
}

#[test]
fn a_stream_that_closes_after_text_without_a_stop_is_a_truncation_error() {
    let mut p = MessagesParser::default();
    p.feed(r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":5}}}"#);
    p.feed(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#);
    p.feed(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#);
    let end = p.finish();
    assert!(
        end.iter().any(
            |e| matches!(e, StreamEvent::Error(e) if e.kind == ErrorKind::Network && e.message.contains("truncated"))
        ),
        "{end:?}"
    );
    assert!(!end.iter().any(|e| matches!(e, StreamEvent::Finish(_))), "{end:?}");
    // the stop reason of message_delta is a finish: nothing is missing when it came
    let mut p = MessagesParser::default();
    p.feed(r#"{"type":"message_start","message":{"id":"m"}}"#);
    p.feed(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#);
    p.feed(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#);
    assert!(p.finish().is_empty());
}

#[test]
fn a_garbled_line_is_skipped_and_the_next_event_still_parses() {
    let mut p = MessagesParser::default();
    assert!(p.feed("<<not json>>").is_empty());
    assert!(p.feed("").is_empty());
    let evs = p.feed(r#"{"type":"message_start","message":{"id":"m"}}"#);
    assert!(matches!(&evs[0], StreamEvent::Start { .. }), "{evs:?}");
}

const CALL_OPEN: [&str; 3] = [
    r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":5}}}"#,
    r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_1","name":"read","input":{}}}"#,
    r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"pa"}}"#,
];

#[test]
fn an_error_drops_open_tool_calls_instead_of_closing_them() {
    let mut p = MessagesParser::default();
    let mut evs: Vec<StreamEvent> = CALL_OPEN.iter().flat_map(|d| p.feed(d)).collect();
    evs.extend(p.feed(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#));
    evs.extend(p.finish());
    assert!(!evs.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { .. })), "{evs:?}");
    assert!(matches!(evs.last(), Some(StreamEvent::Error(e)) if e.kind == ErrorKind::Overloaded));
}

#[test]
fn a_clean_end_inside_a_tool_call_is_a_truncation_error() {
    let mut p = MessagesParser::default();
    let mut evs: Vec<StreamEvent> = CALL_OPEN.iter().flat_map(|d| p.feed(d)).collect();
    evs.extend(p.finish());
    assert!(!evs.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { .. } | StreamEvent::Finish(_))), "{evs:?}");
    assert!(matches!(evs.last(), Some(StreamEvent::Error(e)) if e.message.contains("truncated")), "{evs:?}");
}

#[test]
fn a_context_window_stop_is_a_length_finish() {
    let mut p = MessagesParser::default();
    let mut evs: Vec<StreamEvent> = CALL_OPEN.iter().flat_map(|d| p.feed(d)).collect();
    evs.extend(p.feed(r#"{"type":"message_delta","delta":{"stop_reason":"model_context_window_exceeded"},"usage":{"output_tokens":3}}"#));
    assert!(evs.contains(&StreamEvent::Finish(FinishReason::Length)), "{evs:?}");
}

#[test]
fn the_thinking_budget_stays_below_max_tokens() {
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    let mut req = ModelRequest::new("claude-x", "", vec![Message::user("hi")]);
    req.reasoning = Some(ReasoningConfig::Budget(4000));
    req.max_tokens = 1000;
    assert!(Anthropic::body(&cfg, &req).get("thinking").is_none(), "no room for the 1024 minimum");
    req.max_tokens = 1024;
    assert!(Anthropic::body(&cfg, &req).get("thinking").is_none());
    req.max_tokens = 2000;
    assert_eq!(Anthropic::body(&cfg, &req)["thinking"]["budget_tokens"], 1999);
    req.max_tokens = 1025;
    assert_eq!(Anthropic::body(&cfg, &req)["thinking"]["budget_tokens"], 1024);
    req.max_tokens = 8192;
    assert_eq!(Anthropic::body(&cfg, &req)["thinking"]["budget_tokens"], 4000);
}

#[test]
fn body_has_system_field_blocks_and_cache_breakpoints() {
    let mut req = ModelRequest::new(
        "claude-x",
        "sys",
        vec![
            Message::user("hi"),
            Message::new(
                Role::Assistant,
                vec![
                    Part::Reasoning { text: "t".into(), signature: Some("S".into()), encrypted: None, model: None },
                    Part::Reasoning { text: "unsigned".into(), signature: None, encrypted: None, model: None },
                    Part::tool_call("tu_1", "read", json!({"path": "a"})),
                ],
            ),
            Message::tool_results(vec![Part::tool_result("tu_1", "boom", true)]),
        ],
    );
    req.tools = vec![ToolSpec { name: "read".into(), description: "d".into(), parameters: json!({"type": "object"}) }];
    req.reasoning = Some(ReasoningConfig::Budget(4000));
    let b = Anthropic::body(&ProviderConfig::new(ID, "https://api.anthropic.com", None), &req);
    assert_eq!(b["system"][0]["text"], "sys");
    assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
    assert_eq!(b["tools"][0]["cache_control"]["type"], "ephemeral");
    assert!(b.get("tool_choice").is_none());
    req.no_tool_call = true;
    let b2 = Anthropic::body(&ProviderConfig::new(ID, "https://api.anthropic.com", None), &req);
    assert_eq!(b2["tool_choice"], json!({"type": "none"}));
    assert_eq!(b2["tools"], b["tools"], "the tools stay declared");
    let msgs = b["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[1]["content"][0], json!({"type": "thinking", "thinking": "t", "signature": "S"}));
    assert_eq!(msgs[1]["content"][1]["type"], "tool_use", "an unsigned thinking block is not sent");
    assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
    assert_eq!(msgs[2]["content"][0]["is_error"], true);
    assert_eq!(msgs[2]["content"][0]["cache_control"]["type"], "ephemeral", "the last block is a breakpoint");
    assert_eq!(b["thinking"]["budget_tokens"], 4000);
    assert_eq!(b["max_tokens"], 8192);
    assert_eq!(
        Anthropic::url(&ProviderConfig::new(ID, "https://api.deepseek.com/anthropic", None)),
        "https://api.deepseek.com/anthropic/v1/messages"
    );
}

/// The time line rides on one request only, as the last text of the last message: the breakpoint goes on the block
/// before it, so the next request finds this one's prefix in the cache.
#[test]
fn a_trailing_request_only_text_stays_after_the_cache_breakpoint() {
    let mut person = Message::user("[22:36] hola");
    person.content.push(Part::text("[now 22:36 · turn running]"));
    let req = ModelRequest::new("claude-x", "sys", vec![person]);
    let b = Anthropic::body(&ProviderConfig::new(ID, "https://api.anthropic.com", None), &req);
    let c = b["messages"][0]["content"].as_array().unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[0]["cache_control"]["type"], "ephemeral");
    assert!(c[1].get("cache_control").is_none());
}

#[test]
fn parses_named_events_with_cache_usage() {
    let data = [
        r#"{"type":"message_start","message":{"id":"msg_1","model":"m","usage":{"input_tokens":10,"cache_read_input_tokens":90,"cache_creation_input_tokens":5,"output_tokens":1}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SIG"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hi"}}"#,
        r#"{"type":"content_block_stop","index":1}"#,
        r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"tu_1","name":"read","input":{}}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\""}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":": \"a\"}"}}"#,
        r#"{"type":"content_block_stop","index":2}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":12}}"#,
        r#"{"type":"message_stop"}"#,
    ];
    let mut p = MessagesParser::default();
    let evs: Vec<StreamEvent> = data.iter().flat_map(|d| p.feed(d)).collect();
    assert!(matches!(evs[0], StreamEvent::Start { .. }));
    assert_eq!(evs[1], StreamEvent::ReasoningDelta("hmm".into()));
    assert_eq!(evs[2], StreamEvent::ReasoningMeta { signature: Some("SIG".into()), encrypted: None });
    assert_eq!(evs[3], StreamEvent::TextDelta("Hi".into()));
    assert_eq!(evs[4], StreamEvent::ToolCallStart { id: "tu_1".into(), name: "read".into() });
    assert_eq!(evs[7], StreamEvent::ToolCallEnd { id: "tu_1".into(), name: "read".into(), args: json!({"path": "a"}) });
    assert_eq!(evs[8], StreamEvent::Finish(FinishReason::ToolUse));
    assert_eq!(
        evs[9],
        StreamEvent::Usage(Usage {
            input: 10,
            output: 12,
            cache_read: 90,
            cache_write: 5,
            cache_write_1h: 0,
            transformations: 0,
            reasoning: 0
        })
    );
    assert_eq!(evs.len(), 10);
}

fn reasoning_history(made_by: &str) -> Vec<Message> {
    vec![
        Message::user("hi"),
        Message::new(
            Role::Assistant,
            vec![
                Part::Reasoning {
                    text: "t".into(),
                    signature: Some("S".into()),
                    encrypted: None,
                    model: Some(made_by.into()),
                },
                Part::RedactedReasoning { data: "R".into(), model: Some(made_by.into()) },
                Part::tool_call("tu_1", "read", json!({})),
            ],
        ),
        Message::tool_results(vec![Part::tool_result("tu_1", "ok", false)]),
    ]
}

#[test]
fn thinking_of_another_model_is_not_sent_back() {
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    let mut req = ModelRequest::new("model-a", "sys", reasoning_history("model-a"));
    req.reasoning = Some(ReasoningConfig::Budget(4000));
    let same = Anthropic::body(&cfg, &req);
    assert_eq!(same["messages"][1]["content"][0]["signature"], "S");
    assert_eq!(same["messages"][1]["content"][1]["type"], "redacted_thinking");
    assert_eq!(same["thinking"]["budget_tokens"], 4000);

    req.model = "model-b".into();
    let other = Anthropic::body(&cfg, &req);
    let text = other["messages"].to_string();
    assert!(!text.contains("\"signature\"") && !text.contains("redacted_thinking"), "{text}");
    assert_eq!(other["messages"][1]["content"][0]["type"], "tool_use");
    assert!(other.get("thinking").is_none(), "a tool round without its thinking block goes without thinking");

    // sessions from before the model was recorded keep today's behaviour
    let mut old = reasoning_history("x");
    for p in old[1].content.iter_mut() {
        if let Part::Reasoning { model, .. } | Part::RedactedReasoning { model, .. } = p {
            *model = None;
        }
    }
    let mut req = ModelRequest::new("model-b", "sys", old);
    req.reasoning = Some(ReasoningConfig::Budget(4000));
    let b = Anthropic::body(&cfg, &req);
    assert_eq!(b["messages"][1]["content"][0]["signature"], "S");
    assert_eq!(b["thinking"]["budget_tokens"], 4000);
}

#[test]
fn a_picture_in_a_tool_result_is_an_image_block_inside_it() {
    let (m, _) = convert(
        &Message::tool_results(vec![Part::ToolResult {
            call_id: "c1".into(),
            name: Some("read".into()),
            content: vec![
                Part::text("n.png: PNG image"),
                Part::Image { source: ImageSource::Base64 { media_type: "image/png".into(), data: "AAAA".into() } },
            ],
            is_error: false,
        }]),
        "claude-x",
    );
    assert_eq!(
        m["content"][0]["content"],
        json!([
            {"type": "text", "text": "n.png: PNG image"},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}
        ])
    );
}

#[test]
fn a_blank_text_next_to_a_picture_in_a_tool_result_is_left_out() {
    let (m, _) = convert(
        &Message::tool_results(vec![Part::ToolResult {
            call_id: "c1".into(),
            name: Some("read".into()),
            content: vec![
                Part::text("  \n"),
                Part::Image { source: ImageSource::Base64 { media_type: "image/png".into(), data: "AAAA".into() } },
            ],
            is_error: false,
        }]),
        "claude-x",
    );
    let blocks = m["content"][0]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["type"], "image");
}

#[test]
fn a_stop_sequence_is_sent_to_the_5_5_line_as_well() {
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    for model in ["claude-sonnet-5-5", "claude-sonnet-4-5"] {
        let mut req = modern_req(model);
        req.stop = vec!["END".into()];
        assert_eq!(Anthropic::body(&cfg, &req)["stop_sequences"], json!(["END"]), "{model}");
        req.stop.clear();
        assert!(Anthropic::body(&cfg, &req).get("stop_sequences").is_none(), "{model}");
    }
}

fn modern_req(model: &str) -> ModelRequest {
    let mut req = ModelRequest::new(model, "sys", vec![Message::user("hi")]);
    req.tools = vec![ToolSpec { name: "read".into(), description: "d".into(), parameters: json!({"type": "object"}) }];
    req.max_tokens = 16_384;
    req.temperature = Some(0.0);
    req.extra.insert("top_p".into(), json!(0.9));
    req
}

#[test]
fn the_5_5_models_are_told_apart_from_the_older_ones() {
    for m in [
        "claude-sonnet-5-5",
        "claude-opus-5-5",
        "claude-haiku-5-5",
        "anthropic.claude-sonnet-5-5-v1:0",
        "claude-haiku-5-5@20260101",
        "claude-sonnet-5.5",
        "claude-opus-6",
    ] {
        assert!(modern(m), "{m}");
    }
    for m in [
        "claude-sonnet-5",
        "claude-sonnet-5-20260101",
        "claude-opus-4-8",
        "claude-fable-5-2",
        "claude-sonnet-4-5",
        "claude-haiku-4-5",
        "claude-3-5-sonnet-latest",
        "gpt-5-5",
        "claude-x",
    ] {
        assert!(!modern(m), "{m}");
    }
}

#[test]
fn sonnet_5_5_gets_adaptive_thinking_effort_and_no_rejected_field() {
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    for model in ["claude-sonnet-5-5", "claude-haiku-5-5", "claude-opus-5-5"] {
        let mut req = modern_req(model);
        let b = Anthropic::body(&cfg, &req);
        assert_eq!(b["thinking"]["type"], "adaptive", "{model}");
        assert_eq!(b["thinking"]["block_binding"]["prefix_mismatch_behavior"], "drop_block");
        assert_eq!(b["output_config"], json!({"effort": "medium"}), "default is medium");
        for k in ["temperature", "top_p", "top_k"] {
            assert!(b.get(k).is_none(), "{model} sends {k}");
        }
        assert!(!b.to_string().contains("budget_tokens") && !b.to_string().contains("\"enabled\""));
        assert_eq!(b["max_tokens"], 64_000, "a default-sized cap is raised so a big file write is not cut");
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["tools"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["messages"][0]["content"][0]["cache_control"]["type"], "ephemeral");
        for (e, want) in
            [(Effort::Minimal, "low"), (Effort::Low, "low"), (Effort::Medium, "medium"), (Effort::High, "high")]
        {
            req.reasoning = Some(ReasoningConfig::Effort(e));
            assert_eq!(Anthropic::body(&cfg, &req)["output_config"]["effort"], want);
        }
        req.reasoning = Some(ReasoningConfig::Budget(30_000));
        assert_eq!(Anthropic::body(&cfg, &req)["output_config"]["effort"], "high");
    }
}

fn adaptive_req(model: &str, reasoning: Option<ReasoningConfig>) -> ModelRequest {
    let mut req = ModelRequest::new(model, "", vec![Message::user("hi")]);
    req.max_tokens = 16_384;
    req.temperature = Some(0.5);
    req.reasoning = reasoning;
    req
}

#[test]
fn adaptive_models_get_effort_never_a_budget_and_no_rejected_temperature() {
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    for model in [
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-haiku-5-5",
        "claude-fable-5-2",
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-sonnet-5",
        "claude-opus-4-6",
        "claude-sonnet-4-6",
        "us.anthropic.claude-opus-5-5-v1:0",
        "anthropic/claude-sonnet-5-5",
        "claude-opus-5-5[1m]",
    ] {
        for (r, want) in [
            (ReasoningConfig::Effort(Effort::Minimal), "low"),
            (ReasoningConfig::Effort(Effort::Low), "low"),
            (ReasoningConfig::Effort(Effort::Medium), "medium"),
            (ReasoningConfig::Effort(Effort::High), "high"),
            (ReasoningConfig::Budget(2048), "low"),
            (ReasoningConfig::Budget(8192), "medium"),
            (ReasoningConfig::Budget(20_000), "high"),
        ] {
            let b = Anthropic::body(&cfg, &adaptive_req(model, Some(r)));
            assert_eq!(b["thinking"]["type"], "adaptive", "{model} {r:?}");
            assert_eq!(b["output_config"]["effort"], want, "{model} {r:?}");
            assert!(!b.to_string().contains("budget_tokens"), "{model}");
            if model != "claude-opus-4-6" && model != "claude-sonnet-4-6" {
                assert!(b.get("temperature").is_none(), "{model} sends temperature");
            }
        }
        let b = Anthropic::body(&cfg, &adaptive_req(model, None));
        assert!(!b.to_string().contains("budget_tokens"), "{model}");
    }
}

#[test]
fn an_adaptive_model_with_no_reasoning_set_gets_no_thinking_fields_and_a_small_cap_is_kept() {
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    for model in ["claude-opus-4-8", "claude-fable-5-2", "claude-sonnet-4-6"] {
        let b = Anthropic::body(&cfg, &adaptive_req(model, None));
        assert!(b.get("thinking").is_none() && b.get("output_config").is_none(), "{model}");
        let mut req = adaptive_req(model, Some(ReasoningConfig::Effort(Effort::High)));
        req.max_tokens = 1000;
        let b = Anthropic::body(&cfg, &req);
        assert_eq!(b["thinking"]["type"], "adaptive", "no budget math, so no max_tokens floor");
        assert_eq!(b["max_tokens"], 1000);
    }
}

#[test]
fn temperature_is_kept_where_it_is_taken_and_a_user_extra_cannot_bring_it_back_where_it_is_not() {
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    for model in ["claude-sonnet-4-5", "claude-opus-4-6", "claude-sonnet-4-6", "glm-4.6", "claude-opus-4-1-20250805"] {
        let b = Anthropic::body(&cfg, &adaptive_req(model, None));
        assert_eq!(b["temperature"], 0.5, "{model}");
    }
    let mut req = adaptive_req("claude-opus-4-8", None);
    req.extra.insert("temperature".into(), json!(0.7));
    assert!(Anthropic::body(&cfg, &req).get("temperature").is_none());
    req.extra.insert("output_config".into(), json!({"effort": "max"}));
    assert_eq!(Anthropic::body(&cfg, &req)["output_config"]["effort"], "max", "extra still wins");
}

#[test]
fn older_claude_models_and_other_vendors_keep_the_budget() {
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    for model in [
        "claude-haiku-4-5",
        "claude-haiku-4-5-20251001",
        "claude-sonnet-4-5-20250929",
        "claude-opus-4-5",
        "claude-opus-4-1",
        "claude-3-5-sonnet-latest",
        "glm-4.6",
    ] {
        let b = Anthropic::body(&cfg, &adaptive_req(model, Some(ReasoningConfig::Effort(Effort::Medium))));
        assert_eq!(b["thinking"], json!({"type": "enabled", "budget_tokens": 8192}), "{model}");
        assert!(b.get("output_config").is_none() && b.get("temperature").is_none(), "{model}");
    }
}

#[test]
fn a_broken_round_sends_no_thinking_before_5_5_and_keeps_it_on_5_5() {
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    for model in ["claude-sonnet-5-5", "claude-opus-5-5"] {
        let mut req = adaptive_req(model, Some(ReasoningConfig::Effort(Effort::High)));
        req.messages = reasoning_history("another-model");
        let b = Anthropic::body(&cfg, &req);
        assert_eq!(b["thinking"]["type"], "adaptive", "{model}: {b}");
        assert_eq!(b["output_config"]["effort"], "high", "{model}");
    }
    for model in ["claude-opus-4-8", "claude-fable-5-2"] {
        let mut req = adaptive_req(model, Some(ReasoningConfig::Effort(Effort::High)));
        req.messages = reasoning_history("another-model");
        let b = Anthropic::body(&cfg, &req);
        assert!(b.get("thinking").is_none() && b.get("output_config").is_none(), "{model}: {b}");
    }
}

#[test]
fn a_5_5_reply_cap_is_raised_only_when_the_person_left_it_at_the_default_and_never_past_the_largest() {
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    let mut req = modern_req("claude-sonnet-5-5");
    req.max_tokens = 8192;
    assert_eq!(Anthropic::body(&cfg, &req)["max_tokens"], 8192);
    req.max_tokens = 1024;
    assert_eq!(Anthropic::body(&cfg, &req)["max_tokens"], 4096, "thinking needs room");
    req.max_tokens = 500_000;
    assert_eq!(Anthropic::body(&cfg, &req)["max_tokens"], 128_000);
}

#[test]
fn turning_reasoning_off_on_a_5_5_model_uses_what_it_accepts() {
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    let mut req = modern_req("claude-sonnet-5-5");
    req.reasoning = Some(ReasoningConfig::Off);
    req.messages = vec![
        Message::user("hi"),
        Message::new(
            Role::Assistant,
            vec![
                Part::Reasoning { text: "t".into(), signature: Some("sig".into()), encrypted: None, model: None },
                Part::text("ok"),
            ],
        ),
        Message::user("again"),
    ];
    let b = Anthropic::body(&cfg, &req);
    assert_eq!(b["thinking"], json!({"type": "between_tools"}), "no other field is allowed with it");
    assert!(b.get("output_config").is_none());
    assert!(!b["messages"].to_string().contains("signature"), "no block replayed after an edit risk");
    req.model = "claude-opus-5-5".into();
    let b = Anthropic::body(&cfg, &req);
    assert!(b.get("thinking").is_none(), "Opus 5.5 always thinks and takes neither disabled nor between_tools");
}

#[test]
fn thinking_blocks_of_a_5_5_conversation_go_back_with_their_signature() {
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    let mut req = modern_req("claude-sonnet-5-5");
    req.messages = vec![
        Message::user("hi"),
        Message::new(
            Role::Assistant,
            vec![
                Part::Reasoning { text: String::new(), signature: Some("sig".into()), encrypted: None, model: None },
                Part::text("ok"),
            ],
        ),
        Message::user("again"),
    ];
    let b = Anthropic::body(&cfg, &req);
    assert_eq!(b["messages"][1]["content"][0], json!({"type": "thinking", "thinking": "", "signature": "sig"}));
}

#[test]
fn the_binding_control_can_be_turned_off_and_older_models_keep_their_fields() {
    let mut cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    cfg.extra.insert("block_binding".into(), toml::Value::Boolean(false));
    let b = Anthropic::body(&cfg, &modern_req("claude-sonnet-5-5"));
    assert_eq!(b["thinking"], json!({"type": "adaptive"}));
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    let mut req = modern_req("claude-sonnet-4-5");
    let b = Anthropic::body(&cfg, &req);
    assert_eq!(b["temperature"], 0.0);
    assert_eq!(b["max_tokens"], 16_384);
    assert!(b.get("thinking").is_none() && b.get("output_config").is_none());
    req.reasoning = Some(ReasoningConfig::Effort(Effort::Medium));
    let b = Anthropic::body(&cfg, &req);
    assert_eq!(b["thinking"], json!({"type": "enabled", "budget_tokens": 8192}));
    assert!(b.get("temperature").is_none());
}

#[test]
fn usage_counts_cache_writes_and_reads_and_prices_them() {
    let mut p = MessagesParser::default();
    let mut evs = p.feed(r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":10,"cache_creation_input_tokens":2000,"cache_read_input_tokens":500}}}"#);
    evs.extend(p.feed(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":40}}"#));
    let u = evs.iter().find_map(|e| if let StreamEvent::Usage(u) = e { Some(*u) } else { None }).unwrap();
    assert_eq!((u.input, u.output, u.cache_read, u.cache_write), (10, 40, 500, 2000));
    let price = nexo_core::Pricing {
        input: 2.0,
        output: 10.0,
        cache_read: 0.10,
        cache_write: 2.50,
        cache_write_1h: 0.0,
        long: None,
    };
    let want = (10.0 * 2.0 + 40.0 * 10.0 + 500.0 * 0.10 + 2000.0 * 2.50) / 1e6;
    assert!((price.cost(&u) - want).abs() < 1e-12);
}

#[test]
fn pin_a_tool_call_split_over_chunks_ends_with_the_joined_arguments() {
    let mut p = MessagesParser::default();
    let mut evs: Vec<StreamEvent> = CALL_OPEN.iter().flat_map(|d| p.feed(d)).collect();
    evs.extend(p.feed(
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"th\":\"a\"}"}}"#,
    ));
    evs.extend(p.feed(r#"{"type":"content_block_stop","index":0}"#));
    assert_eq!(
        &evs[1..],
        &[
            StreamEvent::ToolCallStart { id: "tu_1".into(), name: "read".into() },
            StreamEvent::ToolCallDelta { id: "tu_1".into(), partial_json: "{\"pa".into() },
            StreamEvent::ToolCallDelta { id: "tu_1".into(), partial_json: "th\":\"a\"}".into() },
            StreamEvent::ToolCallEnd { id: "tu_1".into(), name: "read".into(), args: json!({"path": "a"}) },
        ]
    );
}

#[test]
fn pin_a_call_without_arguments_or_id_ends_with_empty_object_args() {
    let mut p = MessagesParser::default();
    let mut evs = p.feed(r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","name":"ls"}}"#);
    evs.extend(p.feed(r#"{"type":"content_block_stop","index":0}"#));
    assert_eq!(
        evs,
        vec![
            StreamEvent::ToolCallStart { id: "".into(), name: "ls".into() },
            StreamEvent::ToolCallEnd { id: "".into(), name: "ls".into(), args: json!({}) },
        ]
    );
}

#[test]
fn pin_a_transport_failure_drops_open_calls_and_reports_the_cut_once() {
    let mut p = MessagesParser::default();
    let _: Vec<StreamEvent> = CALL_OPEN.iter().flat_map(|d| p.feed(d)).collect();
    let evs = p.fail("reset".into());
    assert_eq!(evs.len(), 1, "{evs:?}");
    assert!(
        matches!(&evs[0], StreamEvent::Error(e) if e.kind == ErrorKind::Network && e.message == "stream cut: reset")
    );
    assert!(p.fail("again".into()).is_empty(), "once");
    assert!(p.finish().is_empty(), "the open call was dropped, nothing more at the end");
}

#[test]
fn pin_a_transport_failure_after_the_finish_says_nothing() {
    let mut p = MessagesParser::default();
    p.feed(r#"{"type":"message_start","message":{"id":"m"}}"#);
    p.feed(r#"{"type":"message_stop"}"#);
    assert!(p.fail("late".into()).is_empty());
}

#[test]
fn pin_an_error_body_is_shown_as_type_and_message() {
    let mut p = MessagesParser::default();
    let evs = p.feed(r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#);
    assert!(
        matches!(&evs[..], [StreamEvent::Error(e)] if e.kind == ErrorKind::RateLimit && e.message == "rate_limit_error: slow down"),
        "{evs:?}"
    );
    let mut p = MessagesParser::default();
    let evs = p.feed(r#"{"error":{"type":"overloaded_error","message":"busy"}}"#);
    assert!(
        matches!(&evs[..], [StreamEvent::Error(e)] if e.kind == ErrorKind::Overloaded && e.message == "overloaded_error: busy"),
        "{evs:?}"
    );
}

fn cached_req() -> ModelRequest {
    let mut person = Message::user("[22:36] hi");
    person.content.push(Part::text("[now 22:36 · turn running]"));
    let mut req = ModelRequest::new("claude-x", "sys", vec![person]);
    req.tools = vec![ToolSpec { name: "read".into(), description: "d".into(), parameters: json!({"type": "object"}) }];
    req
}

/// What a context policy adds after the opening request changes from one request to the next: a breakpoint on the
/// opening request keeps it read from the cache when only that addition changed.
#[test]
fn the_opening_request_is_a_breakpoint_of_its_own() {
    let mut first = Message::user("the task");
    first.content.push(Part::text("map of what was left out"));
    let req = ModelRequest::new("claude-x", "sys", vec![first, Message::assistant("ok"), Message::user("go on")]);
    let b = Anthropic::body(&ProviderConfig::new(ID, "https://api.anthropic.com", None), &req);
    let msgs = b["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["content"][0]["cache_control"]["type"], "ephemeral");
    assert!(msgs[0]["content"][1].get("cache_control").is_none(), "the addition stays after the breakpoint");
    assert_eq!(msgs[2]["content"][0]["cache_control"]["type"], "ephemeral", "the last block is still one");
    let marks = b.to_string().matches("cache_control").count();
    assert!(marks <= 4, "the API takes at most four breakpoints, found {marks}");
}

#[test]
fn the_long_cache_puts_one_hour_on_every_breakpoint() {
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    let mut req = cached_req();
    req.cache_long = true;
    let b = Anthropic::body(&cfg, &req);
    let hour = json!({"type": "ephemeral", "ttl": "1h"});
    assert_eq!(b["system"][0]["cache_control"], hour);
    assert_eq!(b["tools"][0]["cache_control"], hour);
    assert_eq!(b["messages"][0]["content"][0]["cache_control"], hour);
    assert!(b["messages"][0]["content"][1].get("cache_control").is_none());
    // nothing else changes: only the lifetime of the breakpoints
    let short = Anthropic::body(&cfg, &cached_req());
    let strip = |mut v: serde_json::Value| {
        for p in ["/system/0", "/tools/0", "/messages/0/content/0"] {
            v.pointer_mut(p).unwrap().as_object_mut().unwrap().remove("cache_control");
        }
        v
    };
    assert_eq!(strip(b), strip(short));
}

#[test]
fn without_the_long_cache_the_breakpoints_are_the_short_ones() {
    let b = Anthropic::body(&ProviderConfig::new(ID, "https://api.anthropic.com", None), &cached_req());
    let short = json!({"type": "ephemeral"});
    assert_eq!(b["system"][0]["cache_control"], short);
    assert_eq!(b["tools"][0]["cache_control"], short);
    assert_eq!(b["messages"][0]["content"][0]["cache_control"], short);
}

#[test]
fn the_long_cache_is_off_where_the_provider_config_says_so_or_the_cache_is_off() {
    let mut cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    let mut req = cached_req();
    req.cache_long = true;
    let short = Anthropic::body(&cfg, &cached_req());
    cfg.extra.insert("cache_ttl_1h".into(), toml::Value::Boolean(false));
    assert_eq!(Anthropic::body(&cfg, &req), short);
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    req.cache = false;
    assert!(Anthropic::body(&cfg, &req)["system"][0].get("cache_control").is_none());
}

#[test]
fn usage_reads_the_one_hour_part_of_the_writes_and_prices_it() {
    let mut p = MessagesParser::default();
    let mut evs = p.feed(r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":10,"cache_creation_input_tokens":2000,"cache_read_input_tokens":500,"cache_creation":{"ephemeral_5m_input_tokens":500,"ephemeral_1h_input_tokens":1500}}}}"#);
    evs.extend(p.feed(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":40}}"#));
    let u = evs.iter().find_map(|e| if let StreamEvent::Usage(u) = e { Some(*u) } else { None }).unwrap();
    assert_eq!((u.cache_write, u.cache_write_1h), (2000, 1500));
    let price = nexo_core::Pricing {
        input: 2.0,
        output: 10.0,
        cache_read: 0.10,
        cache_write: 2.50,
        cache_write_1h: 0.0,
        long: None,
    };
    let want = (10.0 * 2.0 + 40.0 * 10.0 + 500.0 * 0.10 + 500.0 * 2.50 + 1500.0 * 4.0) / 1e6;
    assert!((price.cost(&u) - want).abs() < 1e-12);
}

fn usage_of(events: &[&str]) -> nexo_core::Usage {
    let mut p = MessagesParser::default();
    let mut evs = Vec::new();
    for e in events {
        evs.extend(p.feed(e));
    }
    evs.iter().find_map(|e| if let StreamEvent::Usage(u) = e { Some(*u) } else { None }).unwrap()
}

#[test]
fn usage_counts_the_entries_the_server_lists_as_changes_to_the_history() {
    let done = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}"#;
    let start = r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":10},"input_transformations":[{"a":1},{"b":2}]}}"#;
    assert_eq!(usage_of(&[start, done]).transformations, 2);
    // the count may come with the closing delta instead of the opening message
    let late = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4},"input_transformations":[{"a":1}]}"#;
    let open =
        r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":10},"input_transformations":[]}}"#;
    assert_eq!(usage_of(&[open, late]).transformations, 1);
    // an empty array after a listed one does not wipe it
    let list = r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":10},"input_transformations":[{"a":1}]}}"#;
    let none = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4},"input_transformations":[]}"#;
    assert_eq!(usage_of(&[list, none]).transformations, 1);
}

#[test]
fn usage_counts_no_transformations_when_the_array_is_empty_or_missing() {
    let done = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}"#;
    let empty =
        r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":10},"input_transformations":[]}}"#;
    let none = r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":10}}}"#;
    assert_eq!(usage_of(&[empty, done]).transformations, 0);
    assert_eq!(usage_of(&[none, done]).transformations, 0);
}

#[test]
fn the_levels_above_high_reach_the_request_as_the_effort_or_the_larger_budget() {
    let cfg = ProviderConfig::new(ID, "https://api.anthropic.com", None);
    for (e, want) in [(Effort::Xhigh, "xhigh"), (Effort::Max, "max")] {
        let b = Anthropic::body(&cfg, &adaptive_req("claude-opus-5-5", Some(ReasoningConfig::Effort(e))));
        assert_eq!(b["output_config"]["effort"], want);
    }
    for (e, want) in [(Effort::High, 16_384), (Effort::Xhigh, 24_576), (Effort::Max, 32_768)] {
        let mut req = adaptive_req("claude-3-5-sonnet-latest", Some(ReasoningConfig::Effort(e)));
        req.max_tokens = 64_000;
        let b = Anthropic::body(&cfg, &req);
        assert_eq!(b["thinking"], json!({"type": "enabled", "budget_tokens": want}), "{e:?}");
    }
}
