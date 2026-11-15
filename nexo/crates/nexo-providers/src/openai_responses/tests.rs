use super::*;
use crate::parse::Feed;
use nexo_core::{ErrorKind, FinishReason, Message, Part, ReasoningConfig, Role, StreamEvent, Usage};

const CALL_OPEN: [&str; 3] = [
    r#"{"type":"response.created","response":{"id":"r"}}"#,
    r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"read"}}"#,
    r#"{"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"pa"}"#,
];

#[test]
fn an_error_drops_open_tool_calls_instead_of_closing_them() {
    for err in [
        r#"{"type":"error","code":"server_error","message":"boom"}"#,
        r#"{"type":"response.failed","response":{"id":"r","error":{"code":"server_error","message":"boom"}}}"#,
    ] {
        let mut p = ResponsesParser::default();
        let mut evs: Vec<StreamEvent> = CALL_OPEN.iter().flat_map(|d| p.feed(d)).collect();
        evs.extend(p.feed(err));
        evs.extend(p.finish());
        assert!(!evs.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { .. })), "{evs:?}");
        assert!(matches!(evs.last(), Some(StreamEvent::Error(_))), "{evs:?}");
    }
}

#[test]
fn a_clean_end_inside_a_tool_call_is_a_truncation_error() {
    let mut p = ResponsesParser::default();
    let mut evs: Vec<StreamEvent> = CALL_OPEN.iter().flat_map(|d| p.feed(d)).collect();
    evs.extend(p.finish());
    assert!(!evs.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { .. } | StreamEvent::Finish(_))), "{evs:?}");
    assert!(matches!(evs.last(), Some(StreamEvent::Error(e)) if e.message.contains("truncated")), "{evs:?}");
}

#[test]
fn a_mid_stream_context_error_without_a_number_is_the_context_kind() {
    let mut p = ResponsesParser::default();
    let evs = p.feed(r#"{"type":"response.failed","response":{"id":"r","error":{"code":"context_length_exceeded","message":"Your input exceeds the context window of this model."}}}"#);
    assert!(matches!(&evs[0], StreamEvent::Error(e) if e.kind == ErrorKind::ContextLength), "{evs:?}");
}

#[test]
fn a_failed_response_and_a_cut_stream_are_errors() {
    let mut p = ResponsesParser::default();
    let evs = p.feed(r#"{"type":"response.failed","response":{"id":"r","status":"failed","error":{"code":"rate_limit_exceeded","message":"slow down"}}}"#);
    assert!(matches!(&evs[0], StreamEvent::Error(e) if e.kind == ErrorKind::RateLimit), "{evs:?}");
    let mut p = ResponsesParser::default();
    p.feed(r#"{"type":"response.created","response":{"id":"r"}}"#);
    assert!(matches!(&p.finish()[0], StreamEvent::Error(e) if e.kind == ErrorKind::Network));
    let mut p = ResponsesParser::default();
    p.feed(r#"{"type":"response.created","response":{"id":"r"}}"#);
    p.feed(r#"{"type":"response.output_text.delta","delta":"hi"}"#);
    assert_eq!(p.finish(), vec![StreamEvent::Finish(FinishReason::Stop)]);
}

#[test]
fn body_uses_items_and_echoes_only_opaque_reasoning() {
    let mut req = ModelRequest::new(
        "gpt-5",
        "sys",
        vec![
            Message::user("hi"),
            Message::new(
                Role::Assistant,
                vec![
                    Part::Reasoning {
                        text: "visible".into(),
                        signature: Some("rs_1".into()),
                        encrypted: Some("ENC".into()),
                        model: None,
                    },
                    Part::tool_call("call_1", "read", json!({"path": "a"})),
                ],
            ),
            Message::tool_results(vec![Part::tool_result("call_1", "text", false)]),
        ],
    );
    req.reasoning = Some(ReasoningConfig::Effort(nexo_core::Effort::High));
    let b = OpenAiResponses::body(&ProviderConfig::new(ID, "https://api.openai.com/v1", None), &req);
    assert_eq!(b["instructions"], "sys");
    assert_eq!(b["store"], false);
    assert_eq!(b["input"][0]["type"], "message");
    assert_eq!(b["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(b["input"][1], json!({"type": "reasoning", "id": "rs_1", "encrypted_content": "ENC", "summary": []}));
    assert_eq!(b["input"][2]["type"], "function_call");
    assert_eq!(b["input"][2]["call_id"], "call_1");
    assert_eq!(b["input"][3], json!({"type": "function_call_output", "call_id": "call_1", "output": "text"}));
    assert_eq!(b["reasoning"]["effort"], "high");
    assert_eq!(b["include"][0], "reasoning.encrypted_content");
    assert_eq!(b["max_output_tokens"], 8192);
}

fn with_session(id: &str) -> ModelRequest {
    let mut req = ModelRequest::new("gpt-5", "sys", vec![Message::user("hi")]);
    req.session_id = Some(id.into());
    req
}

#[test]
fn the_cache_key_is_the_session_id_stable_within_a_session_and_apart_between_sessions() {
    let cfg = ProviderConfig::new(ID, "https://api.openai.com/v1", None);
    let mut a = with_session("20261107-101500-aaaa");
    let first = OpenAiResponses::body(&cfg, &a);
    a.messages.push(Message::user("and then"));
    let second = OpenAiResponses::body(&cfg, &a);
    assert_eq!(first["prompt_cache_key"], "20261107-101500-aaaa");
    assert_eq!(first["prompt_cache_key"], second["prompt_cache_key"]);
    let other = OpenAiResponses::body(&cfg, &with_session("20261107-101500-bbbb"));
    assert_ne!(first["prompt_cache_key"], other["prompt_cache_key"]);
    let none = OpenAiResponses::body(&cfg, &ModelRequest::new("gpt-5", "sys", vec![]));
    assert!(none.get("prompt_cache_key").is_none());
    let mut off = cfg.clone();
    off.extra.insert("prompt_cache_key".into(), toml::Value::Boolean(false));
    assert!(OpenAiResponses::body(&off, &a).get("prompt_cache_key").is_none());
    // a third-party server on this protocol gets the field only when its config asks for it
    let mut router = ProviderConfig::new(ID, "https://router.example.com/v1", None);
    assert!(OpenAiResponses::body(&router, &a).get("prompt_cache_key").is_none());
    router.extra.insert("prompt_cache_key".into(), toml::Value::Boolean(true));
    assert_eq!(OpenAiResponses::body(&router, &a)["prompt_cache_key"], "20261107-101500-aaaa");
}

#[test]
fn a_chatgpt_login_sends_the_session_headers_codex_sends() {
    let mut cfg = ProviderConfig::new(ID, "https://chatgpt.com/backend-api/codex", None);
    cfg.login = Some("codex".into());
    let h = OpenAiResponses::request_headers(&cfg, Some("tok"), &with_session("s-1"));
    for name in ["session-id", "thread-id", "x-client-request-id"] {
        assert_eq!(h[name], "s-1", "{name}");
    }
    let h2 = OpenAiResponses::request_headers(&cfg, Some("tok"), &with_session("s-2"));
    assert_ne!(h["session-id"], h2["session-id"]);
    // a config header wins, and a provider that is not a ChatGPT login gets none
    cfg.headers.insert("session-id".into(), "mine".into());
    assert_eq!(OpenAiResponses::request_headers(&cfg, None, &with_session("s-1"))["session-id"], "mine");
    let plain = ProviderConfig::new(ID, "https://api.openai.com/v1", None);
    assert!(OpenAiResponses::request_headers(&plain, None, &with_session("s-1")).get("session-id").is_none());
}

#[test]
fn parses_typed_events() {
    let data = [
        r#"{"type":"response.created","response":{"id":"resp_1","model":"gpt-5"}}"#,
        r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_1"}}"#,
        r#"{"type":"response.reasoning_summary_text.delta","delta":"hm"}"#,
        r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs_1","encrypted_content":"ENC"}}"#,
        r#"{"type":"response.output_text.delta","delta":"Hello"}"#,
        r#"{"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","call_id":"call_9","name":"bash"}}"#,
        r#"{"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"command\":"}"#,
        r#"{"type":"response.function_call_arguments.delta","output_index":2,"delta":"\"ls\"}"}"#,
        r#"{"type":"response.function_call_arguments.done","output_index":2,"arguments":"{\"command\":\"ls\"}"}"#,
        r#"{"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":50,"output_tokens":9,"input_tokens_details":{"cached_tokens":20},"output_tokens_details":{"reasoning_tokens":4}}}}"#,
    ];
    let mut p = ResponsesParser::default();
    let evs: Vec<StreamEvent> = data.iter().flat_map(|d| p.feed(d)).collect();
    assert_eq!(evs[0], StreamEvent::Start { response_id: Some("resp_1".into()), model: Some("gpt-5".into()) });
    assert_eq!(evs[1], StreamEvent::ReasoningDelta("hm".into()));
    assert_eq!(evs[2], StreamEvent::ReasoningMeta { signature: Some("rs_1".into()), encrypted: Some("ENC".into()) });
    assert_eq!(evs[3], StreamEvent::TextDelta("Hello".into()));
    assert_eq!(evs[4], StreamEvent::ToolCallStart { id: "call_9".into(), name: "bash".into() });
    assert_eq!(
        evs[7],
        StreamEvent::ToolCallEnd { id: "call_9".into(), name: "bash".into(), args: json!({"command": "ls"}) }
    );
    assert_eq!(
        evs[8],
        StreamEvent::Usage(Usage {
            input: 30,
            output: 9,
            cache_read: 20,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 4
        })
    );
    assert_eq!(evs[9], StreamEvent::Finish(FinishReason::ToolUse));
}

#[test]
fn encrypted_reasoning_of_another_model_is_not_echoed() {
    let history = |made_by: &str| {
        vec![
            Message::user("hi"),
            Message::new(
                Role::Assistant,
                vec![
                    Part::Reasoning {
                        text: "v".into(),
                        signature: Some("rs_1".into()),
                        encrypted: Some("ENC".into()),
                        model: Some(made_by.into()),
                    },
                    Part::text("answer"),
                ],
            ),
        ]
    };
    let cfg = ProviderConfig::new(ID, "https://api.openai.com/v1", None);
    let same = OpenAiResponses::body(&cfg, &ModelRequest::new("gpt-a", "", history("gpt-a")));
    assert!(same["input"].to_string().contains("encrypted_content"));
    let other = OpenAiResponses::body(&cfg, &ModelRequest::new("gpt-b", "", history("gpt-a")));
    let text = other["input"].to_string();
    assert!(!text.contains("encrypted_content") && !text.contains("rs_1"), "{text}");
    assert!(text.contains("answer"));
}

fn think(id: &str) -> Part {
    Part::Reasoning {
        text: "visible".into(),
        signature: Some(id.into()),
        encrypted: Some(format!("ENC-{id}")),
        model: None,
    }
}

fn two_person_requests() -> Vec<Message> {
    vec![
        Message::user("first"),
        Message::new(Role::Assistant, vec![think("rs_old"), Part::tool_call("c1", "read", json!({"path": "a"}))]),
        Message::tool_results(vec![Part::tool_result("c1", "one", false)]),
        Message::new(Role::Assistant, vec![think("rs_old2"), Part::text("done")]),
        Message::user("second"),
        Message::new(Role::Assistant, vec![think("rs_new"), Part::tool_call("c2", "read", json!({"path": "b"}))]),
        Message::tool_results(vec![Part::tool_result("c2", "two", false)]),
        Message::new(Role::Assistant, vec![think("rs_new2"), Part::tool_call("c3", "read", json!({"path": "c"}))]),
        Message::tool_results(vec![Part::tool_result("c3", "three", false)]),
    ]
}

fn kinds(b: &Value) -> Vec<String> {
    b["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| match v["type"].as_str().unwrap() {
            "reasoning" => format!("reasoning:{}", v["id"].as_str().unwrap_or("")),
            "function_call" => format!("call:{}", v["call_id"].as_str().unwrap()),
            t => t.to_string(),
        })
        .collect()
}

#[test]
fn reasoning_before_the_latest_person_message_is_not_sent_and_the_current_turns_stays_in_order() {
    let cfg = ProviderConfig::new(ID, "https://api.openai.com/v1", None);
    let b = OpenAiResponses::body(&cfg, &ModelRequest::new("gpt-5", "", two_person_requests()));
    assert_eq!(
        kinds(&b),
        [
            "message",
            "call:c1",
            "function_call_output",
            "message",
            "message",
            "reasoning:rs_new",
            "call:c2",
            "function_call_output",
            "reasoning:rs_new2",
            "call:c3",
            "function_call_output"
        ]
    );
    let text = b["input"].to_string();
    assert!(!text.contains("rs_old") && !text.contains("ENC-rs_old"), "{text}");
    // a first request has only its own turn: everything stays
    let first = OpenAiResponses::body(&cfg, &ModelRequest::new("gpt-5", "", two_person_requests()[..3].to_vec()));
    assert_eq!(kinds(&first), ["message", "reasoning:rs_old", "call:c1", "function_call_output"]);
}

#[test]
fn a_reasoning_item_is_never_sent_without_the_item_it_led_to() {
    let cfg = ProviderConfig::new(ID, "https://api.openai.com/v1", None);
    // the call its reasoning led to was cut from the view: the reasoning goes with it
    let cut = vec![
        Message::user("go"),
        Message::new(Role::Assistant, vec![think("rs_a"), Part::text("")]),
        Message::new(Role::Assistant, vec![think("rs_b"), Part::text("then this")]),
        Message::new(Role::Assistant, vec![think("rs_c")]),
    ];
    let b = OpenAiResponses::body(&cfg, &ModelRequest::new("gpt-5", "", cut));
    assert_eq!(kinds(&b), ["message", "reasoning:rs_b", "message"]);
    // the reasoning of a call is left out by the curator: the call goes alone, with no id naming it
    let alone = vec![
        Message::user("go"),
        Message::new(Role::Assistant, vec![Part::tool_call("c1", "read", json!({}))]),
        Message::tool_results(vec![Part::tool_result("c1", "ok", false)]),
    ];
    let b = OpenAiResponses::body(&cfg, &ModelRequest::new("gpt-5", "", alone));
    assert_eq!(kinds(&b), ["message", "call:c1", "function_call_output"]);
    assert!(b["input"][1].get("id").is_none());
    // a reasoning item of another model is dropped as before, and the pairing sees it gone
    let foreign = vec![
        Message::user("go"),
        Message::new(
            Role::Assistant,
            vec![
                Part::Reasoning {
                    text: "v".into(),
                    signature: Some("rs_f".into()),
                    encrypted: Some("E".into()),
                    model: Some("other".into()),
                },
                Part::text("answer"),
            ],
        ),
    ];
    let b = OpenAiResponses::body(&cfg, &ModelRequest::new("gpt-5", "", foreign));
    assert_eq!(kinds(&b), ["message", "message"]);
}

#[test]
fn a_picture_in_a_tool_result_is_an_input_image_in_its_output() {
    let req = ModelRequest::new(
        "gpt-5",
        "sys",
        vec![
            Message::new(Role::Assistant, vec![Part::tool_call("c1", "read", json!({"path": "n.png"}))]),
            Message::tool_results(vec![Part::ToolResult {
                call_id: "c1".into(),
                name: Some("read".into()),
                content: vec![
                    Part::text("n.png: PNG image"),
                    Part::Image {
                        source: nexo_core::message::ImageSource::Base64 {
                            media_type: "image/png".into(),
                            data: "AAAA".into(),
                        },
                    },
                ],
                is_error: false,
            }]),
        ],
    );
    let b = OpenAiResponses::body(&ProviderConfig::new(ID, "https://api.openai.com/v1", None), &req);
    assert_eq!(
        b["input"][1],
        json!({"type": "function_call_output", "call_id": "c1", "output": [
            {"type": "input_text", "text": "n.png: PNG image"},
            {"type": "input_image", "image_url": "data:image/png;base64,AAAA"}
        ]})
    );
}

#[test]
fn pin_a_tool_call_split_over_chunks_ends_with_the_joined_arguments() {
    let mut p = ResponsesParser::default();
    let mut evs: Vec<StreamEvent> = CALL_OPEN.iter().flat_map(|d| p.feed(d)).collect();
    evs.extend(p.feed(r#"{"type":"response.function_call_arguments.delta","output_index":0,"delta":"th\":\"a\"}"}"#));
    evs.extend(p.feed(r#"{"type":"response.completed","response":{"id":"r"}}"#));
    assert_eq!(
        &evs[1..],
        &[
            StreamEvent::ToolCallStart { id: "c1".into(), name: "read".into() },
            StreamEvent::ToolCallDelta { id: "c1".into(), partial_json: "{\"pa".into() },
            StreamEvent::ToolCallDelta { id: "c1".into(), partial_json: "th\":\"a\"}".into() },
            StreamEvent::ToolCallEnd { id: "c1".into(), name: "read".into(), args: json!({"path": "a"}) },
            StreamEvent::Finish(FinishReason::ToolUse),
        ]
    );
}

#[test]
fn pin_a_call_without_arguments_or_id_ends_with_empty_object_args() {
    let mut p = ResponsesParser::default();
    let mut evs =
        p.feed(r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","name":"ls"}}"#);
    evs.extend(p.feed(r#"{"type":"response.completed","response":{"id":"r"}}"#));
    assert_eq!(
        &evs[..2],
        &[
            StreamEvent::ToolCallStart { id: "".into(), name: "ls".into() },
            StreamEvent::ToolCallEnd { id: "".into(), name: "ls".into(), args: json!({}) },
        ]
    );
}

#[test]
fn pin_a_transport_failure_drops_open_calls_and_reports_the_cut_once() {
    let mut p = ResponsesParser::default();
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
    let mut p = ResponsesParser::default();
    p.feed(r#"{"type":"response.created","response":{"id":"r"}}"#);
    p.feed(r#"{"type":"response.completed","response":{"id":"r"}}"#);
    assert!(p.fail("late".into()).is_empty());
}

#[test]
fn pin_an_error_body_is_shown_as_type_and_message() {
    let mut p = ResponsesParser::default();
    let evs = p.feed(r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#);
    assert!(
        matches!(&evs[..], [StreamEvent::Error(e)] if e.kind == ErrorKind::RateLimit && e.message == "rate_limit_error: slow down"),
        "{evs:?}"
    );
    let mut p = ResponsesParser::default();
    let evs = p.feed(r#"{"error":{"code":"server_error","message":"boom"}}"#);
    assert!(
        matches!(&evs[..], [StreamEvent::Error(e)] if e.kind == ErrorKind::Server && e.message == "server_error: boom"),
        "{evs:?}"
    );
}
