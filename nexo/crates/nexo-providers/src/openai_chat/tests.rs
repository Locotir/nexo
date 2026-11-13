use nexo_core::{
    ErrorKind, FinishReason, ImageSource, Message, ModelRequest, Part, ProviderConfig, Role, StreamEvent, ToolSpec,
    Usage,
};
use serde_json::json;

use super::{ChatParser, OpenAiChat, ID};
use crate::parse::Feed;

fn cfg() -> ProviderConfig {
    ProviderConfig::new(ID, "https://api.deepseek.com", Some("DEEPSEEK_API_KEY"))
}

#[test]
fn tool_calls_without_an_index_are_told_apart_by_id() {
    let chunks = [
        r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"id":"a","type":"function","function":{"name":"read","arguments":"{\"path\":\"a\"}"}},{"id":"b","type":"function","function":{"name":"ls","arguments":"{}"}}]}}]}"#,
        r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
    ];
    let mut p = ChatParser::default();
    let evs: Vec<StreamEvent> = chunks.iter().flat_map(|c| p.feed(c)).collect();
    let ends: Vec<_> = evs.iter().filter(|e| matches!(e, StreamEvent::ToolCallEnd { .. })).collect();
    assert_eq!(ends.len(), 2, "{evs:?}");
    assert!(matches!(ends[0], StreamEvent::ToolCallEnd { id, args, .. } if id == "a" && args["path"] == "a"));
    assert!(matches!(ends[1], StreamEvent::ToolCallEnd { id, name, .. } if id == "b" && name == "ls"));
}

#[test]
fn parallel_calls_that_all_say_index_zero_are_told_apart_by_id() {
    let chunks = [
        r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"a","type":"function","function":{"name":"read","arguments":"{\"path\":\"a\"}"}}]}}]}"#,
        r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"b","type":"function","function":{"name":"ls","arguments":"{}"}}]}}]}"#,
        r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
    ];
    let mut p = ChatParser::default();
    let evs: Vec<StreamEvent> = chunks.iter().flat_map(|c| p.feed(c)).collect();
    let ends: Vec<_> = evs.iter().filter(|e| matches!(e, StreamEvent::ToolCallEnd { .. })).collect();
    assert_eq!(ends.len(), 2, "{evs:?}");
    assert!(
        matches!(ends[0], StreamEvent::ToolCallEnd { id, name, args } if id == "a" && name == "read" && args["path"] == "a")
    );
    assert!(
        matches!(ends[1], StreamEvent::ToolCallEnd { id, name, args } if id == "b" && name == "ls" && args == &json!({}))
    );
}

#[test]
fn a_null_error_field_is_not_an_error_and_an_empty_assistant_sends_empty_content() {
    let mut p = ChatParser::default();
    let evs = p.feed(r#"{"id":"x","error":null,"choices":[{"index":0,"delta":{"content":"ok"}}]}"#);
    assert!(evs.iter().all(|e| !matches!(e, StreamEvent::Error(_))));
    assert!(evs.contains(&StreamEvent::TextDelta("ok".into())));
    let mut p = ChatParser::default();
    let evs = p.feed(r#"{"error":{"message":"Rate limit reached","type":"rate_limit_error"}}"#);
    assert!(matches!(&evs[0], StreamEvent::Error(e) if e.kind == ErrorKind::RateLimit));
    let req = ModelRequest::new(
        "m",
        "",
        vec![Message::user("hi"), Message::new(Role::Assistant, vec![]), Message::user("more")],
    );
    let b = OpenAiChat::body(&cfg(), &req);
    assert_eq!(b["messages"][1]["content"], "");
}

#[test]
fn the_cache_key_goes_only_to_openai_itself() {
    let mut req = ModelRequest::new("gpt-5", "", vec![Message::user("hi")]);
    req.session_id = Some("sess-1".into());
    let openai = OpenAiChat::body(&ProviderConfig::new(ID, "https://api.openai.com/v1", None), &req);
    assert_eq!(openai["prompt_cache_key"], "sess-1");
    for base in ["https://api.deepseek.com", "https://openrouter.ai/api/v1", "http://localhost:11434/v1"] {
        let b = OpenAiChat::body(&ProviderConfig::new(ID, base, None), &req);
        assert!(b.get("prompt_cache_key").is_none(), "{base}");
    }
    req.session_id = None;
    assert!(OpenAiChat::body(&ProviderConfig::new(ID, "https://api.openai.com/v1", None), &req)
        .get("prompt_cache_key")
        .is_none());
}

#[test]
fn the_end_of_the_stream_without_a_finish_is_read_from_what_came() {
    let mut p = ChatParser::default();
    assert!(matches!(&p.finish()[0], StreamEvent::Error(e) if e.kind == ErrorKind::Parse), "nothing read at all");
    let mut p = ChatParser::default();
    p.feed(r#"{"id":"x","choices":[{"index":0,"delta":{"content":"partial"}}]}"#);
    assert_eq!(p.finish(), vec![StreamEvent::Finish(FinishReason::Stop)], "content without a finish is complete");
    let mut p = ChatParser::default();
    p.feed(r#"{"id":"x","choices":[{"index":0,"delta":{"role":"assistant"}}]}"#);
    assert!(matches!(&p.finish()[0], StreamEvent::Error(e) if e.kind == ErrorKind::Network), "a start without content");
}

#[test]
fn body_places_system_tools_and_results_the_openai_way() {
    let mut req = ModelRequest::new(
        "deepseek-chat",
        "be brief",
        vec![
            Message::user("read a.rs"),
            Message::new(
                Role::Assistant,
                vec![
                    Part::Reasoning { text: "secret".into(), signature: None, encrypted: None, model: None },
                    Part::text("ok"),
                    Part::tool_call("c1", "read", json!({"path": "a.rs"})),
                ],
            ),
            Message::tool_results(vec![Part::tool_result("c1", "fn main() {}", false)]),
        ],
    );
    req.tools =
        vec![ToolSpec { name: "read".into(), description: "reads".into(), parameters: json!({"type": "object"}) }];
    req.temperature = Some(0.2);
    req.no_tool_call = true;
    assert_eq!(OpenAiChat::body(&cfg(), &req)["tool_choice"], "none");
    req.no_tool_call = false;
    let b = OpenAiChat::body(&cfg(), &req);
    assert!(b.get("tool_choice").is_none());
    assert_eq!(b["messages"][0], json!({"role": "system", "content": "be brief"}));
    assert_eq!(b["messages"][1], json!({"role": "user", "content": "read a.rs"}));
    assert_eq!(b["messages"][2]["role"], "assistant");
    assert!(b["messages"][2].get("reasoning_content").is_none(), "readable reasoning is not sent back");
    assert_eq!(b["messages"][2]["tool_calls"][0]["function"]["arguments"], "{\"path\":\"a.rs\"}");
    assert_eq!(b["messages"][3], json!({"role": "tool", "tool_call_id": "c1", "content": "fn main() {}"}));
    assert_eq!(b["tools"][0]["type"], "function");
    assert_eq!(b["tools"][0]["function"]["name"], "read");
    assert_eq!(b["stream_options"]["include_usage"], true);
    assert_eq!(b["max_tokens"], 8192);
    assert_eq!(b["temperature"], 0.2);
    assert_eq!(OpenAiChat::url(&cfg()), "https://api.deepseek.com/v1/chat/completions");
}

#[test]
fn newer_openai_models_take_max_completion_tokens() {
    let req = ModelRequest::new("o3-mini", "", vec![Message::user("hi")]);
    let b = OpenAiChat::body(&cfg(), &req);
    assert_eq!(b["max_completion_tokens"], 8192);
    assert!(b.get("max_tokens").is_none());
    assert!(b.get("messages").unwrap().as_array().unwrap().len() == 1, "no empty system message");
}

#[test]
fn parses_fragmented_tool_calls_reasoning_and_deepseek_usage() {
    let chunks = [
        r#"{"id":"x","model":"deepseek-reasoner","choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"think"}}]}"#,
        r#"{"id":"x","choices":[{"index":0,"delta":{"content":"Hi"}}]}"#,
        r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":""}}]}}]}"#,
        r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"pa"}}]}}]}"#,
        r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a\"}"}}]}}]}"#,
        r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        r#"{"id":"x","choices":[],"usage":{"prompt_tokens":100,"completion_tokens":7,"prompt_cache_hit_tokens":64,"prompt_cache_miss_tokens":36,"completion_tokens_details":{"reasoning_tokens":3}}}"#,
        "[DONE]",
    ];
    let mut p = ChatParser::default();
    let evs: Vec<StreamEvent> = chunks.iter().flat_map(|c| p.feed(c)).collect();
    assert!(matches!(evs[0], StreamEvent::Start { .. }));
    assert_eq!(evs[1], StreamEvent::ReasoningDelta("think".into()));
    assert_eq!(evs[2], StreamEvent::TextDelta("Hi".into()));
    assert_eq!(evs[3], StreamEvent::ToolCallStart { id: "call_1".into(), name: "read".into() });
    assert_eq!(
        evs[6],
        StreamEvent::ToolCallEnd { id: "call_1".into(), name: "read".into(), args: json!({"path": "a"}) }
    );
    assert_eq!(evs[7], StreamEvent::Finish(FinishReason::ToolUse));
    assert_eq!(
        evs[8],
        StreamEvent::Usage(Usage { input: 36, output: 7, cache_read: 64, cache_write: 0, reasoning: 3 })
    );
    assert_eq!(evs.len(), 9, "[DONE] adds nothing after a finish");
}

#[test]
fn an_error_chunk_becomes_an_error_event() {
    let mut p = ChatParser::default();
    let evs = p.feed(r#"{"error":{"message":"overloaded","type":"server_error"}}"#);
    assert!(matches!(&evs[0], StreamEvent::Error(e) if e.message.contains("overloaded")));
}

#[test]
fn running_usage_reports_the_last_figures_once() {
    let chunks = [
        r#"{"id":"x","choices":[{"index":0,"delta":{"content":"a"}}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
        r#"{"id":"x","choices":[{"index":0,"delta":{"content":"b"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#,
        r#"{"id":"x","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        "[DONE]",
    ];
    let mut p = ChatParser::default();
    let mut evs: Vec<StreamEvent> = chunks.iter().flat_map(|c| p.feed(c)).collect();
    evs.extend(p.finish());
    let usages: Vec<_> = evs.iter().filter(|e| matches!(e, StreamEvent::Usage(_))).collect();
    assert_eq!(usages, vec![&StreamEvent::Usage(Usage { input: 10, output: 5, ..Default::default() })]);
    // a stream without [DONE] reports it at its end
    let mut p = ChatParser::default();
    let mut evs: Vec<StreamEvent> = chunks[..3].iter().flat_map(|c| p.feed(c)).collect();
    evs.extend(p.finish());
    assert_eq!(evs.last(), Some(&StreamEvent::Usage(Usage { input: 10, output: 5, ..Default::default() })));
}

const CALL_OPEN: &str = r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"read","arguments":"{\"pa"}}]}}]}"#;

#[test]
fn an_error_drops_open_tool_calls_instead_of_closing_them() {
    let mut p = ChatParser::default();
    let mut evs = p.feed(CALL_OPEN);
    evs.extend(p.feed(r#"{"error":{"message":"overloaded","type":"server_error"}}"#));
    evs.extend(p.finish());
    assert!(!evs.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { .. })), "{evs:?}");
    assert!(matches!(evs.last(), Some(StreamEvent::Error(_))), "{evs:?}");
}

#[test]
fn a_clean_end_inside_a_tool_call_is_a_truncation_error() {
    let mut p = ChatParser::default();
    let mut evs = p.feed(CALL_OPEN);
    evs.extend(p.finish());
    assert!(!evs.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { .. } | StreamEvent::Finish(_))), "{evs:?}");
    assert!(matches!(evs.last(), Some(StreamEvent::Error(e)) if e.message.contains("truncated")), "{evs:?}");
}

#[test]
fn a_picture_in_a_tool_result_follows_the_tool_message_as_an_image_url() {
    let req = ModelRequest::new(
        "gpt-5",
        "",
        vec![
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
        ],
    );
    let b = OpenAiChat::body(&cfg(), &req);
    assert_eq!(b["messages"][0]["role"], "assistant");
    assert_eq!(b["messages"][1], json!({"role": "tool", "tool_call_id": "c1", "content": "n.png: PNG image"}));
    assert_eq!(
        b["messages"][2],
        json!({"role": "user", "content": [{"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}]})
    );
}
