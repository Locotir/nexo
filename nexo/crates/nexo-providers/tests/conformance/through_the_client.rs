use super::*;

async fn stream_through_client(c: &ProviderConfig, model: &str, with_tool: bool) -> Collector {
    let mut r = req(model);
    if with_tool {
        r.tools = vec![spec("read", json!({"type":"object","properties":{"path":{"type":"string"}}}))];
    }
    let mut s = Client::new(registry()).stream(c, &r, 0).await.expect("a stream");
    let mut col = Collector::new();
    while let Some(ev) = s.next().await {
        col.push(ev);
    }
    col
}

#[tokio::test]
async fn via_the_client_chat_text_and_a_split_tool_call() {
    let s = MockServer::start_async().await;
    let body = sse(&[
        d(json!({"role":"assistant","content":"on it"})),
        tcs(vec![tc(0, "c1", "read", "")]),
        tcs(vec![json!({"index":0,"function":{"arguments":"{\"path\":\"a\"}"}})]),
        fin("tool_calls"),
        usage_chunk(json!({"prompt_tokens":5,"completion_tokens":3})),
    ]);
    let m = s
        .mock_async(|w, t| {
            w.method(POST).path("/v1/chat/completions").json_body_includes(r#"{"stream": true}"#);
            t.status(200).header("content-type", "text/event-stream").body(body);
        })
        .await;
    let c = stream_through_client(&cfg("openai-chat", &s.base_url()), "m", true).await;
    assert!(c.error.is_none(), "{:?}", c.error);
    assert_eq!(c.message().text(), "on it");
    assert_eq!(
        c.message().tool_calls().map(|(_, n, a)| (n.to_string(), a.clone())).collect::<Vec<_>>(),
        [("read".to_string(), json!({"path":"a"}))]
    );
    assert_eq!(c.finish_reason(), FinishReason::ToolUse);
    assert_eq!(c.usage.output, 3);
    m.assert_async().await;
}

#[tokio::test]
async fn via_the_client_anthropic_gemini_and_responses_paths() {
    let a = MockServer::start_async().await;
    let ma = a
        .mock_async(|w, t| {
            w.method(POST).path("/v1/messages").header("anthropic-version", "2023-06-01");
            t.status(200).header("content-type", "text/event-stream").body(
                a_start(json!({"input_tokens":2}))
                    + &a_text(0, "hello")
                    + &a_end("end_turn", json!({"output_tokens":1})),
            );
        })
        .await;
    let c = stream_through_client(&cfg("anthropic", &a.base_url()), "claude-sonnet-4-5", false).await;
    assert_eq!((c.message().text().as_str(), c.error.is_none()), ("hello", true), "{:?}", c.error);
    ma.assert_async().await;

    let g = MockServer::start_async().await;
    let mg = g
        .mock_async(|w, t| {
            w.method(POST).path("/v1beta/models/gemini-2.5-flash:streamGenerateContent").query_param("alt", "sse");
            t.status(200).header("content-type", "text/event-stream").body(data(&[g_text("hello")]));
        })
        .await;
    let c = stream_through_client(&cfg("gemini", &g.base_url()), "gemini-2.5-flash", false).await;
    assert_eq!((c.message().text().as_str(), c.error.is_none()), ("hello", true), "{:?}", c.error);
    mg.assert_async().await;

    let r = MockServer::start_async().await;
    let mr = r
        .mock_async(|w, t| {
            w.method(POST).path("/v1/responses").json_body_includes(r#"{"store": false}"#);
            t.status(200).header("content-type", "text/event-stream").body(r_events(&[
                r_created(),
                json!({"type":"response.output_text.delta","output_index":0,"delta":"hello"}),
                r_done(json!({"input_tokens":2,"output_tokens":1})),
            ]));
        })
        .await;
    let c = stream_through_client(&cfg("openai-responses", &r.base_url()), "gpt-5", false).await;
    assert_eq!((c.message().text().as_str(), c.error.is_none()), ("hello", true), "{:?}", c.error);
    mr.assert_async().await;
}

// ================= the real local Ollama, $0 (run with --ignored) =================

fn ollama() -> ProviderConfig {
    ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None)
}

/// One text turn and one tool-call turn through nexo's own client against the local Ollama.
/// Ignored by default: it needs the server and its model on this machine.
#[tokio::test]
#[ignore = "needs the local Ollama at localhost:11434 with ornith:9b"]
async fn ollama_ornith_text_turn_and_tool_call_turn() {
    let model = "ornith:9b";
    let mut r = ModelRequest::new(model, "Answer in one short sentence.", vec![Message::user("Say hello.")]);
    r.max_tokens = 400;
    let mut s = Client::new(registry()).stream(&ollama(), &r, 0).await.expect("Ollama answers");
    let mut text = Collector::new();
    while let Some(ev) = s.next().await {
        text.push(ev);
    }
    eprintln!(
        "text turn: {:?} | reasoning {:?} | finish {:?} | usage {:?}",
        text.message().text(),
        text.message().reasoning().len(),
        text.finish_reason(),
        text.usage
    );
    assert!(text.error.is_none(), "{:?}", text.error);
    assert!(!text.message().text().trim().is_empty(), "no text in the reply");
    assert!(!text.message().text().contains("<think>"), "think tags leaked into the answer");

    let mut r = ModelRequest::new(
        model,
        "You must use the tool to read files. Never answer without calling it.",
        vec![Message::user("Read the file notes.txt.")],
    );
    r.max_tokens = 600;
    r.tools = vec![spec(
        "read_file",
        json!({"type":"object","properties":{"path":{"type":"string","description":"file to read"}},"required":["path"]}),
    )];
    let mut s = Client::new(registry()).stream(&ollama(), &r, 0).await.expect("Ollama answers");
    let mut call = Collector::new();
    while let Some(ev) = s.next().await {
        call.push(ev);
    }
    let calls: Vec<_> =
        call.message().tool_calls().map(|(i, n, a)| (i.to_string(), n.to_string(), a.clone())).collect();
    eprintln!("tool turn: {calls:?} | finish {:?} | usage {:?}", call.finish_reason(), call.usage);
    assert!(call.error.is_none(), "{:?}", call.error);
    assert_eq!(calls.len(), 1, "one call expected: {calls:?}");
    assert_eq!(calls[0].1, "read_file");
    assert!(calls[0].2["path"].is_string(), "{:?}", calls[0].2);
    assert_eq!(call.finish_reason(), FinishReason::ToolUse);
}

fn xai_mock_config(base: &str) -> ProviderConfig {
    let mut c = nexo_providers::presets()["xai"].clone();
    c.base_url = base.into();
    c.login = None;
    c.api_key_env = None;
    c.api_key_cmd = None;
    c.api_key_file = None;
    c.auth = None;
    c
}

/// A grok turn on the xai preset (`openai-responses`): each round the model reasons (an encrypted item), writes
/// one short line and calls a tool. At round 3 the request carries the encrypted reasoning of rounds 1 and 2,
/// unchanged and in order, each before its round's call; so the model does not have to rebuild its conclusion
/// from the one-line text it left behind.
#[tokio::test]
async fn the_xai_preset_sends_the_encrypted_reasoning_of_earlier_rounds_back_at_round_three() {
    let s = MockServer::start_async().await;
    let c = xai_mock_config(&s.base_url());
    assert_eq!(c.protocol, "openai-responses", "the xai preset is on the Responses API");
    let client = Client::new(registry());

    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let mut history = vec![Message::user("[background jobs ended]\n- job 1 `make` ended after 3s")];
    let rounds = [
        ("rs_1", "ENC-1", "The gap fix does not change a token. I remove the map counter.", "c1"),
        ("rs_2", "ENC-2", "The gap fix does not change a token. I measure it.", "c2"),
        ("rs_3", "ENC-3", "Measured.", "c3"),
    ];
    for (rid, enc, line, call) in rounds {
        let rec = sent.clone();
        let m = s
            .mock_async(move |w, t| {
                w.method(POST).path("/v1/responses").is_true(move |r| {
                    rec.lock().unwrap().push(r.body_string());
                    true
                });
                t.status(200).header("content-type", "text/event-stream").body(r_events(&[
                    json!({"type":"response.created","response":{"id":"resp_1","model":"grok-4.7"}}),
                    json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":rid}}),
                    json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":rid,"encrypted_content":enc}}),
                    json!({"type":"response.output_text.delta","output_index":1,"delta":line}),
                    r_call(2, call, "read"),
                    r_args_done(2, "{\"path\":\"a\"}"),
                    r_done(json!({"input_tokens":5,"output_tokens":3})),
                ]));
            })
            .await;
        let r = ModelRequest::new("grok-4.7", "sys", history.clone());
        let mut st = client.stream(&c, &r, 0).await.expect("a stream");
        let mut col = Collector::new();
        while let Some(ev) = st.next().await {
            col.push(ev);
        }
        assert!(col.error.is_none(), "{:?}", col.error);
        let reply = col.message();
        assert!(
            reply.content.iter().any(|p| matches!(p, Part::Reasoning { encrypted: Some(e), .. } if e == enc)),
            "the collector keeps the encrypted reasoning of {call}: {:?}",
            reply.content
        );
        history.push(reply);
        history.push(Message::tool_results(vec![Part::tool_result(call, "ok", false)]));
        m.delete_async().await;
    }

    let third: Value = serde_json::from_str(&sent.lock().unwrap()[2]).expect("a json body");
    let input = third["input"].as_array().expect("input items");
    let kinds: Vec<(String, String)> = input
        .iter()
        .filter(|i| matches!(i["type"].as_str(), Some("reasoning" | "function_call")))
        .map(|i| {
            let tag = i["encrypted_content"].as_str().or_else(|| i["call_id"].as_str()).unwrap_or("").to_string();
            (i["type"].as_str().unwrap().to_string(), tag)
        })
        .collect();
    let want: Vec<(String, String)> =
        [("reasoning", "ENC-1"), ("function_call", "c1"), ("reasoning", "ENC-2"), ("function_call", "c2")]
            .into_iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
    assert_eq!(kinds, want, "round 3 input: {}", serde_json::to_string_pretty(input).unwrap());
}

/// A person who sets `protocol = "openai-chat"` on the xai preset keeps a chat request.
#[tokio::test]
async fn the_xai_preset_with_openai_chat_set_by_the_person_still_sends_a_chat_request() {
    let s = MockServer::start_async().await;
    let mut c = xai_mock_config(&s.base_url());
    c.protocol = "openai-chat".into();
    let m = s
        .mock_async(|w, t| {
            w.method(POST).path("/v1/chat/completions");
            t.status(200).header("content-type", "text/event-stream").body(sse(&[
                d(json!({"role":"assistant","content":"hello"})),
                fin("stop"),
                usage_chunk(json!({"prompt_tokens":5,"completion_tokens":3})),
            ]));
        })
        .await;
    let col = stream_through_client(&c, "grok-4.7", false).await;
    assert!(col.error.is_none(), "{:?}", col.error);
    assert_eq!(col.message().text(), "hello");
    m.assert_async().await;
}
