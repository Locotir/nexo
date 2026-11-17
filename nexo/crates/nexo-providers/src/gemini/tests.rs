use super::*;
use crate::parse::Feed;
use nexo_core::ToolSpec;
use nexo_core::{ErrorKind, FinishReason, ImageSource, Message, Part, Role, StreamEvent, Usage};

#[test]
fn body_uses_contents_and_names_function_responses() {
    let mut req = ModelRequest::new(
        "gemini-2.5-pro",
        "sys",
        vec![
            Message::user("hi"),
            Message::new(
                Role::Assistant,
                vec![
                    Part::Reasoning { text: "t".into(), signature: Some("TS".into()), encrypted: None, model: None },
                    Part::tool_call("call_1", "read", json!({"path": "a"})),
                ],
            ),
            Message::tool_results(vec![Part::tool_result("call_1", "contents", false)]),
        ],
    );
    req.tools = vec![ToolSpec {
        name: "read".into(),
        description: "d".into(),
        parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}, "additionalProperties": false}),
    }];
    let b = Gemini::body(&ProviderConfig::new(ID, "https://generativelanguage.googleapis.com", None), &req);
    assert_eq!(b["systemInstruction"]["parts"][0]["text"], "sys");
    assert_eq!(b["contents"][0], json!({"role": "user", "parts": [{"text": "hi"}]}));
    assert_eq!(b["contents"][1]["role"], "model");
    assert_eq!(b["contents"][1]["parts"][0]["functionCall"]["name"], "read");
    assert_eq!(b["contents"][1]["parts"][0]["thoughtSignature"], "TS");
    assert_eq!(
        b["contents"][2]["parts"][0]["functionResponse"],
        json!({"name": "read", "response": {"result": "contents"}})
    );
    assert!(b["tools"][0]["functionDeclarations"][0]["parameters"].get("additionalProperties").is_none());
    assert_eq!(b["generationConfig"]["maxOutputTokens"], 8192);
    assert_eq!(
        Gemini::url(&ProviderConfig::new(ID, "https://generativelanguage.googleapis.com", None), "gemini-2.5-pro"),
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
    );
}

#[test]
fn parses_thoughts_text_whole_calls_and_usage() {
    let data = [
        r#"{"candidates":[{"content":{"parts":[{"text":"plan","thought":true}],"role":"model"}}],"responseId":"r1","modelVersion":"gemini-2.5-pro"}"#,
        r#"{"candidates":[{"content":{"parts":[{"text":"Hello "}],"role":"model"}}]}"#,
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{"path":"a"}},"thoughtSignature":"TS"}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":10,"cachedContentTokenCount":40,"thoughtsTokenCount":6}}"#,
    ];
    let mut p = GeminiParser::default();
    let evs: Vec<StreamEvent> = data.iter().flat_map(|d| p.feed(d)).collect();
    assert_eq!(evs[0], StreamEvent::Start { response_id: Some("r1".into()), model: Some("gemini-2.5-pro".into()) });
    assert_eq!(evs[1], StreamEvent::ReasoningDelta("plan".into()));
    assert_eq!(evs[2], StreamEvent::TextDelta("Hello ".into()));
    assert_eq!(evs[3], StreamEvent::ReasoningMeta { signature: Some("TS".into()), encrypted: None });
    let id = match &evs[4] {
        StreamEvent::ToolCallStart { id, name } if name == "read" => id.clone(),
        other => panic!("{other:?}"),
    };
    assert!(id.starts_with("call_"));
    assert_eq!(evs[5], StreamEvent::ToolCallEnd { id: id.clone(), name: "read".into(), args: json!({"path": "a"}) });
    assert_eq!(
        evs[6],
        StreamEvent::Usage(Usage {
            input: 60,
            output: 16,
            cache_read: 40,
            cache_write: 0,
            cache_write_1h: 0,
            transformations: 0,
            reasoning: 6
        })
    );
    assert_eq!(evs[7], StreamEvent::Finish(FinishReason::ToolUse));
    // a second response never reuses an id
    let mut p2 = GeminiParser::default();
    let evs2: Vec<StreamEvent> = data.iter().flat_map(|d| p2.feed(d)).collect();
    assert!(matches!(&evs2[4], StreamEvent::ToolCallStart { id: id2, .. } if id2 != &id));
}

#[test]
fn a_garbled_line_is_skipped_and_the_next_event_still_parses() {
    let mut p = GeminiParser::default();
    assert!(p.feed("<<not json>>").is_empty());
    let evs = p.feed(r#"{"candidates":[{"content":{"parts":[{"text":"Hello "}],"role":"model"}}]}"#);
    assert!(evs.contains(&StreamEvent::TextDelta("Hello ".into())), "{evs:?}");
}

#[test]
fn errors_in_the_stream_are_classified_and_a_cut_prompt_is_not_a_reply() {
    let mut p = GeminiParser::default();
    let evs = p.feed(r#"{"error":{"code":429,"message":"Quota exceeded","status":"RESOURCE_EXHAUSTED"}}"#);
    assert!(matches!(&evs[0], StreamEvent::Error(e) if e.kind == ErrorKind::RateLimit && e.is_retryable()));
    let mut p = GeminiParser::default();
    p.feed(r#"{"candidates":[{"content":{"parts":[],"role":"model"}}]}"#);
    assert!(matches!(&p.finish()[0], StreamEvent::Error(e) if e.kind == ErrorKind::Network));
}

#[test]
fn a_malformed_call_or_an_empty_finish_is_a_retryable_error() {
    for r in ["MALFORMED_FUNCTION_CALL", "OTHER"] {
        let mut p = GeminiParser::default();
        let mut evs = p.feed(&format!(r#"{{"candidates":[{{"finishReason":"{r}"}}]}}"#));
        evs.extend(p.finish());
        assert!(!evs.iter().any(|e| matches!(e, StreamEvent::Finish(_))), "{evs:?}");
        assert!(evs.iter().any(|e| matches!(e, StreamEvent::Error(e) if e.is_retryable() && e.message.contains(r))));
    }
    let mut p = GeminiParser::default();
    let evs = p.feed(r#"{"candidates":[{"finishReason":"SAFETY"}]}"#);
    assert!(evs.contains(&StreamEvent::Finish(FinishReason::ContentFilter)), "a block stays a block");
}

#[test]
fn clean_schema_keeps_properties_named_like_keywords() {
    let s = json!({"type": "object", "additionalProperties": false,
        "properties": {"default": {"type": "string", "default": "x"}, "const": {"type": "integer"}},
        "required": ["default"]});
    let c = clean_schema(&s);
    assert_eq!(c["properties"]["default"], json!({"type": "string"}));
    assert_eq!(c["properties"]["const"], json!({"type": "integer"}));
    assert!(c.get("additionalProperties").is_none());
}

#[test]
fn usage_after_the_finish_reason_is_still_reported_once() {
    let late = [
        r#"{"candidates":[{"content":{"parts":[{"text":"Hi"}],"role":"model"},"finishReason":"STOP"}]}"#,
        r#"{"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":2}}"#,
    ];
    let mut p = GeminiParser::default();
    let mut evs: Vec<StreamEvent> = late.iter().flat_map(|d| p.feed(d)).collect();
    evs.extend(p.finish());
    let usages: Vec<_> = evs.iter().filter(|e| matches!(e, StreamEvent::Usage(_))).collect();
    assert_eq!(usages, vec![&StreamEvent::Usage(Usage { input: 10, output: 2, ..Default::default() })]);
    // usage sent with the finish is not sent again when a later chunk repeats it
    let mut p = GeminiParser::default();
    let mut evs = p.feed(r#"{"candidates":[{"content":{"parts":[{"text":"Hi"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":2}}"#);
    evs.extend(p.feed(late[1]));
    evs.extend(p.finish());
    assert_eq!(evs.iter().filter(|e| matches!(e, StreamEvent::Usage(_))).count(), 1);
}

#[test]
fn thought_signature_of_another_model_is_not_sent_back() {
    let history = |made_by: &str| {
        vec![
            Message::user("hi"),
            Message::new(
                Role::Assistant,
                vec![
                    Part::Reasoning {
                        text: "t".into(),
                        signature: Some("TS".into()),
                        encrypted: None,
                        model: Some(made_by.into()),
                    },
                    Part::tool_call("call_1", "read", json!({})),
                ],
            ),
        ]
    };
    let cfg = ProviderConfig::new(ID, "https://generativelanguage.googleapis.com", None);
    let same = Gemini::body(&cfg, &ModelRequest::new("gem-a", "", history("gem-a")));
    assert_eq!(same["contents"][1]["parts"][0]["thoughtSignature"], "TS");
    let other = Gemini::body(&cfg, &ModelRequest::new("gem-b", "", history("gem-a")));
    assert!(!other.to_string().contains("thoughtSignature"));
    assert_eq!(other["contents"][1]["parts"][0]["functionCall"]["name"], "read");
}

#[test]
fn a_picture_in_a_tool_result_rides_as_inline_data_after_the_response() {
    let msgs = vec![
        Message::new(Role::Assistant, vec![Part::tool_call("c1", "read", json!({"path": "n.png"}))]),
        Message::tool_results(vec![Part::ToolResult {
            call_id: "c1".into(),
            name: Some("read".into()),
            content: vec![
                Part::text("n.png: PNG image"),
                Part::Image { source: ImageSource::Base64 { media_type: "image/png".into(), data: "AAAA".into() } },
            ],
            is_error: false,
        }]),
    ];
    let c = contents(&msgs, "gemini-x");
    assert_eq!(c[1]["parts"][0]["functionResponse"]["response"]["result"], "n.png: PNG image");
    assert_eq!(c[1]["parts"][1], json!({"inlineData": {"mimeType": "image/png", "data": "AAAA"}}));
}

#[test]
fn pin_a_function_call_is_started_and_ended_in_one_chunk_and_empty_args_are_an_object() {
    let mut p = GeminiParser::default();
    let evs =
        p.feed(r#"{"candidates":[{"content":{"parts":[{"functionCall":{"id":"g1","name":"ls","args":null}}]}}]}"#);
    assert_eq!(
        &evs[1..],
        &[
            StreamEvent::ToolCallStart { id: "g1".into(), name: "ls".into() },
            StreamEvent::ToolCallEnd { id: "g1".into(), name: "ls".into(), args: json!({}) },
        ]
    );
    let mut p = GeminiParser::default();
    let evs =
        p.feed(r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{"path":"a"}}}]}}]}"#);
    assert!(matches!(&evs[1], StreamEvent::ToolCallStart { id, .. } if id.starts_with("call_")), "{evs:?}");
    assert!(
        matches!(&evs[2], StreamEvent::ToolCallEnd { id, args, .. } if id.starts_with("call_") && args["path"] == "a"),
        "{evs:?}"
    );
}

#[test]
fn pin_a_transport_failure_reports_the_cut_once_and_a_late_one_says_nothing() {
    let mut p = GeminiParser::default();
    p.feed(r#"{"candidates":[{"content":{"parts":[{"text":"hi"}]}}]}"#);
    let evs = p.fail("reset".into());
    assert_eq!(evs.len(), 1, "{evs:?}");
    assert!(
        matches!(&evs[0], StreamEvent::Error(e) if e.kind == ErrorKind::Network && e.message == "stream cut: reset")
    );
    assert!(p.fail("again".into()).is_empty(), "once");
    assert!(p.finish().is_empty());
    let mut p = GeminiParser::default();
    p.feed(r#"{"candidates":[{"content":{"parts":[{"text":"hi"}]},"finishReason":"STOP"}]}"#);
    assert!(p.fail("late".into()).is_empty());
}

#[test]
fn pin_an_error_body_uses_its_message_and_status() {
    let mut p = GeminiParser::default();
    let evs = p.feed(r#"{"error":{"code":503,"message":"busy","status":"UNAVAILABLE"}}"#);
    assert!(
        matches!(&evs[..], [StreamEvent::Error(e)] if e.kind == ErrorKind::Overloaded && e.message == "busy"),
        "{evs:?}"
    );
}

#[test]
fn a_schema_with_local_definitions_is_inlined_and_a_loop_is_cut_out() {
    // the shape a filesystem MCP server sends: the arguments point into `$defs`
    let s = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "edits": {"type": "array", "items": {"$ref": "#/$defs/Edit"}},
            "dry": {"$ref": "#/definitions/Flag", "description": "only show it"},
            "tree": {"$ref": "#/$defs/Node"},
            "gone": {"$ref": "#/$defs/Missing"},
            "web": {"$ref": "https://example.com/other.json"}
        },
        "$defs": {
            "Edit": {"type": "object", "properties": {"old": {"type": "string"}}, "required": ["old"]},
            "Node": {"type": "object", "properties": {"label": {"type": "string"}, "child": {"$ref": "#/$defs/Node"}}}
        },
        "definitions": {"Flag": {"type": "boolean"}}
    });
    let c = clean_schema(&s);
    assert_eq!(c["properties"]["edits"]["items"]["properties"]["old"], json!({"type": "string"}));
    assert_eq!(c["properties"]["edits"]["items"]["required"], json!(["old"]));
    assert_eq!(c["properties"]["dry"], json!({"type": "boolean", "description": "only show it"}));
    // an object with no properties is a 400 anywhere in the schema, so the stand-ins are cut out
    // and the parent says what is missing
    let tree = &c["properties"]["tree"];
    assert!(tree["properties"].get("child").is_none());
    assert!(tree["description"].as_str().unwrap().contains("child"), "{tree}");
    for name in ["gone", "web"] {
        assert!(c["properties"].get(name).is_none(), "{name}");
    }
    assert!(c["description"].as_str().unwrap().contains("gone, web"), "{c}");
    for gone in ["$defs", "definitions", "$schema"] {
        assert!(c.get(gone).is_none(), "{gone}");
    }
}

#[test]
fn an_optional_field_typed_with_null_becomes_nullable() {
    let s = json!({"type": "object", "properties": {
        "a": {"type": ["string", "null"]},
        "b": {"anyOf": [{"type": "integer"}, {"type": "null"}], "description": "count"},
        "c": {"type": ["string", "integer"]},
        "d": {"type": ["null"]}
    }});
    let cleaned = clean_schema(&s);
    let p = &cleaned["properties"];
    assert_eq!(p["a"], json!({"type": "string", "nullable": true}));
    assert_eq!(p["b"], json!({"type": "integer", "description": "count", "nullable": true}));
    assert_eq!(p["c"], json!({"anyOf": [{"type": "string"}, {"type": "integer"}]}));
    assert_eq!(p["d"], json!({"nullable": true}));
}

#[test]
fn one_of_is_any_of_all_of_objects_are_merged_and_not_is_dropped() {
    let s = json!({"type": "object", "properties": {
        "pick": {"oneOf": [
            {"type": "object", "properties": {"a": {"type": "string"}}},
            {"type": "object", "properties": {"b": {"type": "string"}}}
        ]},
        "both": {"allOf": [
            {"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x"]},
            {"type": "object", "properties": {"y": {"type": "string"}}, "required": ["y"]}
        ]},
        "other": {"type": "string", "not": {"enum": ["no"]}}
    }});
    let cleaned = clean_schema(&s);
    let p = &cleaned["properties"];
    assert_eq!(p["pick"]["anyOf"].as_array().map(Vec::len), Some(2));
    assert!(p["pick"].get("oneOf").is_none());
    assert_eq!(p["both"]["type"], "object");
    assert_eq!(p["both"]["properties"], json!({"x": {"type": "string"}, "y": {"type": "string"}}));
    assert_eq!(p["both"]["required"], json!(["x", "y"]));
    assert_eq!(p["other"], json!({"type": "string"}));
}

#[test]
fn bounds_formats_and_enums_the_api_does_not_know_are_dropped_and_the_rest_kept() {
    let s = json!({"type": "object", "properties": {
        "n": {"type": "integer", "exclusiveMinimum": 0, "minimum": 1, "multipleOf": 2, "format": "int32"},
        "u": {"type": "string", "format": "uri", "minLength": 1, "pattern": "^h"},
        "t": {"type": "string", "format": "date-time"},
        "l": {"type": "array", "uniqueItems": true, "items": {"type": "string"}, "minItems": 1},
        "tuple": {"type": "array", "items": [{"type": "string"}, {"type": "integer"}]},
        "bare": {"type": "array"},
        "level": {"type": "integer", "enum": [1, 2, 3], "description": "How loud."},
        "mode": {"enum": ["a", "b", null]},
        "gate": {"if": {"type": "string"}, "then": {"minLength": 2}, "else": {"type": "integer"}}
    }});
    let cleaned = clean_schema(&s);
    let p = &cleaned["properties"];
    assert_eq!(p["n"], json!({"type": "integer", "minimum": 1, "format": "int32"}));
    assert_eq!(p["u"], json!({"type": "string", "minLength": 1, "pattern": "^h"}));
    assert_eq!(p["t"]["format"], "date-time");
    assert_eq!(p["l"], json!({"type": "array", "items": {"type": "string"}, "minItems": 1}));
    assert_eq!(
        p["tuple"],
        json!({
            "type": "array",
            "items": {"anyOf": [{"type": "string"}, {"type": "integer"}]},
            "description": "Items by position: string, integer."
        })
    );
    assert_eq!(
        p["bare"],
        json!({"type": "array", "items": {"anyOf": [{"type": "string"}, {"type": "number"}, {"type": "boolean"}]}})
    );
    // numbers cannot be an enum here, so the model still gets them in the description
    assert_eq!(p["level"], json!({"type": "integer", "description": "How loud. Allowed values: 1, 2, 3."}));
    assert_eq!(p["mode"], json!({"type": "string", "enum": ["a", "b"], "nullable": true}));
    assert_eq!(p["gate"], json!({}));
}

#[test]
fn the_request_body_carries_none_of_the_keywords_the_api_rejects() {
    let mut req = ModelRequest::new("gemini-2.5-pro", "", vec![Message::user("hi")]);
    req.tools = vec![ToolSpec {
        name: "edit".into(),
        description: "d".into(),
        parameters: json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {
                "target": {"$ref": "#/$defs/Target"},
                "count": {"type": ["integer", "null"], "exclusiveMinimum": 0},
                "link": {"type": "string", "format": "uri"},
                "how": {"oneOf": [{"type": "string"}, {"type": "integer"}]}
            },
            "$defs": {"Target": {"allOf": [{"type": "object", "properties": {"path": {"type": "string"}}}]}}
        }),
    }];
    let b = Gemini::body(&ProviderConfig::new(ID, "https://generativelanguage.googleapis.com", None), &req);
    let sent = b["tools"][0]["functionDeclarations"][0]["parameters"].to_string();
    for gone in ["$ref", "$defs", "$schema", "allOf", "oneOf", "exclusiveMinimum", "uri", "\"null\""] {
        assert!(!sent.contains(gone), "{gone} in {sent}");
    }
    assert!(sent.contains("\"path\""), "{sent}");
}

fn foreign_calls_history() -> Vec<Message> {
    vec![
        Message::user("hi"),
        Message::new(
            Role::Assistant,
            vec![
                Part::tool_call("c1", "read", json!({"path": "a"})),
                Part::tool_call("c2", "read", json!({"path": "b"})),
            ],
        ),
        Message::tool_results(vec![Part::tool_result("c1", "x", false), Part::tool_result("c2", "y", false)]),
        Message::new(Role::Assistant, vec![Part::tool_call("c3", "read", json!({"path": "c"}))]),
    ]
}

#[test]
fn a_call_with_no_signature_gets_the_documented_placeholder_on_gemini_3_first_call_only() {
    let c = contents(&foreign_calls_history(), "gemini-3-pro-preview");
    assert_eq!(c[1]["parts"][0]["thoughtSignature"], "skip_thought_signature_validator");
    assert!(c[1]["parts"][1].get("thoughtSignature").is_none(), "parallel calls after the first carry none");
    assert_eq!(c[3]["parts"][0]["thoughtSignature"], "skip_thought_signature_validator");
    // an alias without a number follows the newest model of its line
    for alias in ["gemini-flash-latest", "gemini-pro-latest"] {
        let c = contents(&foreign_calls_history(), alias);
        assert_eq!(c[1]["parts"][0]["thoughtSignature"], "skip_thought_signature_validator", "{alias}");
    }
    // a version with a dot and a proxy prefix are read the same way
    let c = contents(&foreign_calls_history(), "models/gemini-3.1-pro");
    assert_eq!(c[1]["parts"][0]["thoughtSignature"], "skip_thought_signature_validator");
}

#[test]
fn gemini_2_models_get_no_placeholder_and_a_real_signature_is_never_replaced() {
    for model in ["gemini-2.5-pro", "gem-b"] {
        let c = contents(&foreign_calls_history(), model);
        assert!(!serde_json::to_string(&c).unwrap().contains("thoughtSignature"), "{model}");
    }
    let signed = vec![
        Message::user("hi"),
        Message::new(
            Role::Assistant,
            vec![
                Part::Reasoning { text: "t".into(), signature: Some("TS".into()), encrypted: None, model: None },
                Part::tool_call("c1", "read", json!({})),
            ],
        ),
    ];
    let c = contents(&signed, "gemini-3-pro-preview");
    assert_eq!(c[1]["parts"][0]["thoughtSignature"], "TS");
}

#[test]
fn a_nested_object_without_properties_is_cut_out_with_its_required_entry_and_arrays_of_it() {
    let s = json!({"type": "object", "properties": {
        "keep": {"type": "string"},
        "meta": {"type": "object", "additionalProperties": true},
        "list": {"type": "array", "items": {"type": "object"}},
        "deep": {"type": "object", "properties": {"inner": {"type": "object"}}},
        "pick": {"anyOf": [{"type": "object"}, {"type": "string"}]}
    }, "required": ["keep", "meta"]});
    let c = clean_schema(&s);
    assert_eq!(c["required"], json!(["keep"]));
    let p = c["properties"].as_object().unwrap();
    assert_eq!(p.keys().collect::<Vec<_>>(), ["keep", "pick"]);
    assert_eq!(p["pick"]["anyOf"], json!([{"type": "string"}]));
    assert!(c["description"].as_str().unwrap().contains("meta, list, deep"), "{c}");
    // the top level stays as it is, for the caller to send as no parameters
    assert_eq!(clean_schema(&json!({"type": "object"}))["type"], "object");
}

#[test]
fn definitions_that_refer_to_each_other_many_times_are_cut_off_instead_of_growing_without_end() {
    let mut defs = serde_json::Map::new();
    for i in 0..40 {
        let next = json!({"$ref": format!("#/$defs/D{}", i + 1)});
        defs.insert(
            format!("D{i}"),
            json!({"type": "object", "properties": {"a": next.clone(), "b": next, "n": {"type": "string"}}}),
        );
    }
    defs.insert("D40".into(), json!({"type": "object", "properties": {"n": {"type": "string"}}}));
    let s = json!({"type": "object", "properties": {"start": {"$ref": "#/$defs/D0"}}, "$defs": defs});
    let started = std::time::Instant::now();
    let c = clean_schema(&s);
    assert!(started.elapsed().as_secs() < 5);
    assert!(c.to_string().len() < 1_000_000);
    assert!(c["properties"]["start"]["properties"]["n"].is_object());
}

#[test]
fn the_levels_above_high_take_the_high_thinking_budget() {
    for e in [nexo_core::Effort::High, nexo_core::Effort::Xhigh, nexo_core::Effort::Max] {
        let mut req = ModelRequest::new("gemini-2.5-pro", "", vec![Message::user("hi")]);
        req.reasoning = Some(nexo_core::ReasoningConfig::Effort(e));
        let cfg = ProviderConfig::new(ID, "https://generativelanguage.googleapis.com", None);
        let b = Gemini::body(&cfg, &req);
        assert_eq!(b["generationConfig"]["thinkingConfig"]["thinkingBudget"], 24_576, "{e:?}");
    }
}
