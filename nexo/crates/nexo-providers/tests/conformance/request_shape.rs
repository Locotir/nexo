use super::*;

fn api(model: &str) -> (ProviderConfig, ModelRequest) {
    (cfg("openai-chat", "https://api.openai.com/v1"), req(model))
}

#[test]
fn t7_chat_url_of_every_preset_is_the_one_the_vendor_documents() {
    let want = [
        ("openai", "https://api.openai.com/v1/chat/completions"),
        ("deepseek", "https://api.deepseek.com/v1/chat/completions"),
        ("xai", "https://api.x.ai/v1/chat/completions"),
        ("groq", "https://api.groq.com/openai/v1/chat/completions"),
        ("mistral", "https://api.mistral.ai/v1/chat/completions"),
        ("openrouter", "https://openrouter.ai/api/v1/chat/completions"),
        ("together", "https://api.together.xyz/v1/chat/completions"),
        ("fireworks", "https://api.fireworks.ai/inference/v1/chat/completions"),
        ("cerebras", "https://api.cerebras.ai/v1/chat/completions"),
        ("moonshot", "https://api.moonshot.ai/v1/chat/completions"),
        ("qwen", "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions"),
        ("zhipu", "https://api.z.ai/api/paas/v4/chat/completions"),
        ("ollama", "http://localhost:11434/v1/chat/completions"),
        ("lmstudio", "http://localhost:1234/v1/chat/completions"),
        ("vllm", "http://localhost:8000/v1/chat/completions"),
        ("llamacpp", "http://localhost:8080/v1/chat/completions"),
    ];
    let all = nexo_providers::presets();
    for (name, url) in want {
        let c = all.get(name).unwrap_or_else(|| panic!("no preset {name}"));
        assert_eq!(c.protocol, "openai-chat", "{name}");
        assert_eq!(OpenAiChat::url(c), url, "{name}");
    }
    assert_eq!(Anthropic::url(&all["anthropic"]), "https://api.anthropic.com/v1/messages");
    assert_eq!(
        Gemini::url(&all["gemini"], "gemini-2.5-pro"),
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
    );
    assert_eq!(
        OpenAiResponses::url(&all["codex"]),
        format!("{}/responses", all["codex"].base_url.trim_end_matches('/'))
    );
}

#[test]
fn t7_every_preset_and_directory_entry_has_a_registered_protocol_and_a_well_formed_url() {
    let reg = registry();
    let mut configs: Vec<(String, ProviderConfig)> = nexo_providers::presets().into_iter().collect();
    configs.extend(nexo_providers::directory::all().iter().map(|e| (e.id.to_string(), e.to_config())));
    assert!(configs.len() > 50, "the directory is loaded: {}", configs.len());
    for (name, c) in configs {
        assert!(reg.get(&c.protocol).is_some(), "{name}: protocol {:?} is not registered", c.protocol);
        let u = reqwest::Url::parse(&c.base_url).unwrap_or_else(|e| panic!("{name}: {} ({e})", c.base_url));
        assert!(matches!(u.scheme(), "http" | "https"), "{name}: {}", c.base_url);
        assert_eq!(c.base_url.trim(), c.base_url, "{name}");
        if c.protocol == "openai-chat" {
            let url = OpenAiChat::url(&c);
            let path = url.split_once("://").map_or(url.as_str(), |(_, rest)| rest);
            assert!(url.ends_with("/chat/completions") && !path.contains("//"), "{name}: {url}");
        }
    }
}

#[test]
fn t7_chat_openai_uses_max_completion_tokens_for_o_series_and_gpt5_and_max_tokens_elsewhere() {
    for (model, key) in [
        ("gpt-5", "max_completion_tokens"),
        ("gpt-5-mini", "max_completion_tokens"),
        ("o3", "max_completion_tokens"),
        ("o4-mini", "max_completion_tokens"),
        ("gpt-4.1", "max_tokens"),
        ("orca-mini", "max_tokens"),
        ("deepseek-chat", "max_tokens"),
        ("qwen3:8b", "max_tokens"),
    ] {
        let (c, r) = api(model);
        let b = OpenAiChat::body(&c, &r);
        assert_eq!(b[key], 8192, "{model}");
        let other = if key == "max_tokens" { "max_completion_tokens" } else { "max_tokens" };
        assert!(b.get(other).is_none(), "{model}");
    }
    let (mut c, r) = api("gpt-5");
    c.extra.insert("max_tokens_key".into(), toml::Value::String("max_tokens".into()));
    assert_eq!(OpenAiChat::body(&c, &r)["max_tokens"], 8192);
}

#[test]
fn t7_chat_no_temperature_to_openai_reasoning_models_but_to_everyone_else() {
    // before: review.rs asks for 0.0 and the retry changes it; `gpt-5` and `o3` answer 400 to any value
    for (base, model, sent) in [
        ("https://api.openai.com/v1", "gpt-5", false),
        ("https://api.openai.com/v1", "o3", false),
        ("https://api.openai.com/v1", "o4-mini", false),
        ("https://api.openai.com/v1", "gpt-5-chat-latest", true),
        ("https://api.openai.com/v1", "gpt-4.1", true),
        ("https://openrouter.ai/api/v1", "openai/gpt-5", true),
        ("https://api.deepseek.com", "deepseek-reasoner", true),
        ("http://localhost:11434/v1", "gpt-oss:20b", true),
    ] {
        let mut r = req(model);
        r.temperature = Some(0.0);
        let b = OpenAiChat::body(&cfg("openai-chat", base), &r);
        assert_eq!(b.get("temperature").is_some(), sent, "{base} {model}");
        let b = OpenAiResponses::body(&cfg("openai-responses", base), &r);
        assert_eq!(b.get("temperature").is_some(), sent, "responses {base} {model}");
    }
}

#[test]
fn t7_chat_stream_usage_system_role_and_stop_fields() {
    let (c, mut r) = api("gpt-4.1");
    r.stop = vec!["END".into()];
    let b = OpenAiChat::body(&c, &r);
    assert_eq!(b["stream"], true);
    assert_eq!(b["stream_options"], json!({"include_usage": true}));
    assert_eq!(b["messages"][0], json!({"role":"system","content":"sys"}));
    assert_eq!(b["stop"], json!(["END"]));
    let mut dev = c.clone();
    dev.extra.insert("system_role".into(), toml::Value::String("developer".into()));
    assert_eq!(OpenAiChat::body(&dev, &r)["messages"][0]["role"], "developer");
}

#[test]
fn t7_chat_prompt_cache_key_goes_to_openai_only() {
    for (base, sent) in [
        ("https://api.openai.com/v1", true),
        ("https://api.mistral.ai/v1", false),
        ("https://api.groq.com/openai/v1", false),
        ("https://openrouter.ai/api/v1", false),
        ("http://localhost:11434/v1", false),
    ] {
        let mut r = req("m");
        r.session_id = Some("sess-1".into());
        assert_eq!(OpenAiChat::body(&cfg("openai-chat", base), &r).get("prompt_cache_key").is_some(), sent, "{base}");
    }
}

#[test]
fn t7_chat_reasoning_effort_is_sent_only_when_asked_and_reasoning_is_replayed_only_to_deepseek_or_on_request() {
    let (c, mut r) = api("gpt-5");
    assert!(OpenAiChat::body(&c, &r).get("reasoning_effort").is_none());
    r.reasoning = Some(ReasoningConfig::Effort(Effort::High));
    assert_eq!(OpenAiChat::body(&c, &r)["reasoning_effort"], "high");
    r.reasoning = Some(ReasoningConfig::Off);
    assert!(OpenAiChat::body(&c, &r).get("reasoning_effort").is_none());
    // earlier reasoning stays out of the history of strict servers; DeepSeek thinking mode needs it back
    r.messages.push(Message::new(
        Role::Assistant,
        vec![
            Part::Reasoning { text: "secret plan".into(), signature: None, encrypted: None, model: None },
            Part::text("answer"),
        ],
    ));
    let b = OpenAiChat::body(&c, &r);
    assert!(!b.to_string().contains("secret plan"), "{b}");
    let mut on = c.clone();
    on.extra.insert("send_reasoning".into(), toml::Value::Boolean(true));
    assert_eq!(OpenAiChat::body(&on, &r)["messages"][2]["reasoning_content"], "secret plan");
    let ds = cfg("openai-chat", "https://api.deepseek.com/v1");
    assert_eq!(OpenAiChat::body(&ds, &r)["messages"][2]["reasoning_content"], "secret plan");
    let mut off = ds.clone();
    off.extra.insert("send_reasoning".into(), toml::Value::Boolean(false));
    assert!(!OpenAiChat::body(&off, &r).to_string().contains("secret plan"), "the key overrides the host default");
}

#[test]
fn t7_chat_tools_tool_results_and_assistant_calls_have_the_documented_shape() {
    let (c, mut r) = api("gpt-4.1");
    r.tools = vec![spec("read", json!({"type":"object","properties":{"path":{"type":"string"}}}))];
    r.messages.push(Message::new(Role::Assistant, vec![Part::tool_call("call_1", "read", json!({"path":"a"}))]));
    r.messages.push(Message::tool_results(vec![Part::tool_result("call_1", "boom", true)]));
    let b = OpenAiChat::body(&c, &r);
    assert_eq!(b["tools"][0]["type"], "function");
    assert_eq!(b["tools"][0]["function"]["name"], "read");
    assert!(b.get("tool_choice").is_none());
    let m = &b["messages"];
    assert_eq!(m[2]["role"], "assistant");
    assert!(m[2]["content"].is_null(), "null content next to calls: {}", m[2]);
    assert_eq!(m[2]["tool_calls"][0]["function"]["arguments"], "{\"path\":\"a\"}");
    assert_eq!(m[3], json!({"role":"tool","tool_call_id":"call_1","content":"Error: boom"}));
    r.no_tool_call = true;
    assert_eq!(OpenAiChat::body(&c, &r)["tool_choice"], "none");
}

#[test]
fn t7_chat_mistral_tool_call_ids_are_nine_letters_and_digits_and_the_pair_still_meets() {
    // before: `call_0` and ids from another provider went out as they were, and Mistral answers 400
    // ("Tool call id was call_0 but must be a-z, A-Z, 0-9, with a length of 9")
    let mut r = req("mistral-large-latest");
    r.messages.push(Message::new(
        Role::Assistant,
        vec![Part::tool_call("call_0", "read", json!({})), Part::tool_call("D681PevKs", "grep", json!({}))],
    ));
    r.messages.push(Message::tool_results(vec![
        Part::tool_result("call_0", "a", false),
        Part::tool_result("D681PevKs", "b", false),
    ]));
    let b = OpenAiChat::body(&cfg("openai-chat", "https://api.mistral.ai/v1"), &r);
    let call_ids: Vec<&str> =
        b["messages"][2]["tool_calls"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    let result_ids =
        [b["messages"][3]["tool_call_id"].as_str().unwrap(), b["messages"][4]["tool_call_id"].as_str().unwrap()];
    for id in call_ids.iter().chain(result_ids.iter()) {
        assert!(id.len() == 9 && id.bytes().all(|c| c.is_ascii_alphanumeric()), "{id}");
    }
    assert_eq!(call_ids, result_ids, "each result keeps the id of its call");
    assert_ne!(call_ids[0], call_ids[1]);
    assert_eq!(call_ids[1], "D681PevKs", "an id that already fits is not touched");
    // every other vendor gets the ids as they are
    let b = OpenAiChat::body(&cfg("openai-chat", "https://api.openai.com/v1"), &r);
    assert_eq!(b["messages"][2]["tool_calls"][0]["id"], "call_0");
}

#[test]
fn t7_chat_mistral_leaves_messages_without_calls_untouched_no_null_tool_calls() {
    // before: the id mapping indexed `tool_calls` mutably, which writes `"tool_calls": null` into every message
    let mut r = req("mistral-large-latest");
    r.messages.push(Message::assistant("plain"));
    r.messages.push(Message::user("again"));
    let mistral = cfg("openai-chat", "https://api.mistral.ai/v1");
    let generic = cfg("openai-chat", "https://example.com/v1");
    let want = serde_json::to_string(&OpenAiChat::body(&generic, &r)["messages"]).unwrap();
    let got = serde_json::to_string(&OpenAiChat::body(&mistral, &r)["messages"]).unwrap();
    assert_eq!(got, want, "no tool calls anywhere: the messages are the same bytes");
    r.messages.push(Message::new(Role::Assistant, vec![Part::tool_call("call_0", "read", json!({}))]));
    r.messages.push(Message::tool_results(vec![Part::tool_result("call_0", "a", false)]));
    let b = OpenAiChat::body(&mistral, &r);
    let m = b["messages"].as_array().unwrap();
    assert_eq!(m.len(), 6);
    for (i, msg) in m.iter().enumerate() {
        assert_eq!(msg.get("tool_calls").is_some(), i == 4, "message {i}: {msg}");
    }
}
#[test]
fn t7_responses_body_store_off_strict_off_encrypted_reasoning_and_input_items() {
    let c = cfg("openai-responses", "https://api.openai.com/v1");
    let mut r = req("gpt-5");
    r.tools = vec![spec("read", json!({"type":"object","properties":{}}))];
    r.reasoning = Some(ReasoningConfig::Effort(Effort::Medium));
    r.session_id = Some("s1".into());
    r.messages.push(Message::new(Role::Assistant, vec![Part::tool_call("call_1", "read", json!({"p":1}))]));
    r.messages.push(Message::tool_results(vec![Part::tool_result("call_1", "ok", false)]));
    let b = OpenAiResponses::body(&c, &r);
    assert_eq!(b["store"], false);
    assert_eq!(b["tools"][0]["strict"], false);
    assert_eq!(b["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(b["prompt_cache_key"], "s1");
    assert_eq!(b["max_output_tokens"], 8192);
    let input = b["input"].as_array().unwrap();
    assert!(input.iter().any(|i| i["type"] == "function_call" && i["call_id"] == "call_1"), "{input:?}");
    assert!(input.iter().any(|i| i["type"] == "function_call_output" && i["call_id"] == "call_1"), "{input:?}");
    // a server that is not OpenAI gets no cache key
    let b = OpenAiResponses::body(&cfg("openai-responses", "https://example.org/v1"), &r);
    assert!(b.get("prompt_cache_key").is_none());
}

#[test]
fn t7_responses_a_codex_login_omits_max_tokens_and_temperature_and_keeps_instructions() {
    let c = nexo_providers::presets()["codex"].clone();
    let mut r = ModelRequest::new("gpt-5.5", "", vec![Message::user("hi")]);
    r.temperature = Some(0.2);
    let b = OpenAiResponses::body(&c, &r);
    assert!(b.get("max_output_tokens").is_none() && b.get("temperature").is_none(), "{b}");
    assert_eq!(b["instructions"], "");
}

#[test]
fn t7_anthropic_body_tools_cache_thinking_and_blank_text_blocks() {
    let c = cfg("anthropic", "https://api.anthropic.com");
    let mut r = req("claude-sonnet-4-5");
    r.tools = vec![spec("read", json!({"type":"object","properties":{}}))];
    r.temperature = Some(0.3);
    r.reasoning = Some(ReasoningConfig::Effort(Effort::Low));
    // before: a blank text block next to a call went out as it was and the API answered 400
    r.messages
        .push(Message::new(Role::Assistant, vec![Part::text("\n\n"), Part::tool_call("toolu_1", "read", json!({}))]));
    r.messages.push(Message::tool_results(vec![Part::tool_result("toolu_1", "ok", false)]));
    let b = Anthropic::body(&c, &r);
    assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
    assert_eq!(b["thinking"], json!({"type":"enabled","budget_tokens":2048}));
    assert!(b.get("temperature").is_none(), "not allowed with thinking");
    assert_eq!(b["messages"][1]["content"], json!([{"type":"tool_use","id":"toolu_1","name":"read","input":{}}]));
    assert_eq!(b["messages"][2]["content"][0]["type"], "tool_result");
    assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
    let modern = Anthropic::body(&c, &{
        let mut m = r.clone();
        m.model = "claude-sonnet-5-5".into();
        m
    });
    assert_eq!(modern["thinking"]["type"], "adaptive");
    assert!(modern.get("temperature").is_none());
}

#[test]
fn t7_gemini_body_a_tool_without_arguments_has_no_parameters_and_schemas_are_cleaned() {
    // before: `parameters: {"type":"object","properties":{}}` made the API answer 400
    // ("properties: should be non-empty for OBJECT type"); the setup ping tool has this shape
    let c = cfg("gemini", "https://generativelanguage.googleapis.com");
    let mut r = req("gemini-2.5-flash");
    r.tools = vec![
        spec("ping", json!({"type":"object","properties":{}})),
        spec("bare", json!({"type":"object"})),
        spec(
            "read",
            json!({"type":"object","additionalProperties":false,"properties":{"path":{"type":"string"}},"required":["path"]}),
        ),
    ];
    let b = Gemini::body(&c, &r);
    let decls = &b["tools"][0]["functionDeclarations"];
    assert!(decls[0].get("parameters").is_none(), "{}", decls[0]);
    assert!(decls[1].get("parameters").is_none(), "{}", decls[1]);
    assert_eq!(decls[2]["parameters"]["properties"]["path"]["type"], "string");
    assert!(!decls[2].to_string().contains("additionalProperties"), "{}", decls[2]);
    assert_eq!(b["systemInstruction"]["parts"][0]["text"], "sys");
    assert_eq!(b["generationConfig"]["maxOutputTokens"], 8192);
}

#[test]
fn t7_gemini_results_are_named_after_their_calls() {
    let c = cfg("gemini", "https://generativelanguage.googleapis.com");
    let mut r = req("gemini-2.5-flash");
    r.messages.push(Message::new(Role::Assistant, vec![Part::tool_call("id1", "read", json!({"p":1}))]));
    r.messages.push(Message::tool_results(vec![Part::tool_result("id1", "ok", false)]));
    let b = Gemini::body(&c, &r);
    assert_eq!(b["contents"][1]["parts"][0]["functionCall"]["name"], "read");
    assert_eq!(b["contents"][2]["parts"][0]["functionResponse"]["name"], "read");
}

#[test]
fn t7_chat_deepseek_tool_loop_sends_reasoning_content_back_on_the_second_request() {
    // before: the second request of a thinking-mode tool loop carried no `reasoning_content` and DeepSeek answered 400
    let mut r = req("deepseek-reasoner");
    r.tools = vec![spec("read", json!({"type":"object"}))];
    r.messages.push(Message::new(
        Role::Assistant,
        vec![
            Part::Reasoning { text: "need the file".into(), signature: None, encrypted: None, model: None },
            Part::tool_call("call_1", "read", json!({"path":"a"})),
        ],
    ));
    r.messages.push(Message::tool_results(vec![Part::tool_result("call_1", "fn main() {}", false)]));
    let b = OpenAiChat::body(&cfg("openai-chat", "https://api.deepseek.com/v1"), &r);
    assert_eq!(b["messages"][2]["reasoning_content"], "need the file", "{b}");
    assert_eq!(b["messages"][2]["tool_calls"][0]["id"], "call_1");
}

// ================= t8: model list and probe =================

/// provider x level -> the field that carries it (a JSON pointer into the body); `None` is no such field.
#[test]
fn effort_levels_reach_the_wire_in_the_shape_each_vendor_documents() {
    use ReasoningConfig::{Budget, Effort as E, Off};
    let x = Effort::Xhigh;
    type Case = (&'static str, &'static str, &'static str, ReasoningConfig, &'static str, Option<Value>);
    let cases: Vec<Case> = vec![
        // openai-chat: the level, up to xhigh and max, as `reasoning_effort`; a budget is the nearest level
        ("openai-chat", "https://api.openai.com/v1", "gpt-5.2", E(x), "/reasoning_effort", Some(json!("xhigh"))),
        (
            "openai-chat",
            "https://api.openai.com/v1",
            "gpt-5.2",
            E(Effort::Max),
            "/reasoning_effort",
            Some(json!("max")),
        ),
        ("openai-chat", "https://api.openai.com/v1", "gpt-5", Budget(30_000), "/reasoning_effort", Some(json!("high"))),
        ("openai-chat", "https://api.openai.com/v1", "gpt-5", Off, "/reasoning_effort", None),
        // off has a form on DeepSeek, Mistral, Groq and Ollama; OpenRouter takes the object
        ("openai-chat", "https://api.deepseek.com/v1", "deepseek-v4", Off, "/thinking/type", Some(json!("disabled"))),
        (
            "openai-chat",
            "https://api.mistral.ai/v1",
            "mistral-small-latest",
            Off,
            "/reasoning_effort",
            Some(json!("none")),
        ),
        ("openai-chat", "https://api.groq.com/openai/v1", "qwen3-32b", Off, "/reasoning_effort", Some(json!("none"))),
        ("openai-chat", "http://localhost:11434/v1", "qwen3:8b", Off, "/reasoning_effort", Some(json!("none"))),
        (
            "openai-chat",
            "https://openrouter.ai/api/v1",
            "x/y",
            E(Effort::High),
            "/reasoning/effort",
            Some(json!("high")),
        ),
        ("openai-chat", "https://openrouter.ai/api/v1", "x/y", E(Effort::High), "/reasoning_effort", None),
        // responses
        ("openai-responses", "https://api.openai.com/v1", "gpt-5.2", E(x), "/reasoning/effort", Some(json!("xhigh"))),
        (
            "openai-responses",
            "https://api.openai.com/v1",
            "gpt-5",
            Budget(100),
            "/reasoning/effort",
            Some(json!("low")),
        ),
        // anthropic: xhigh only where the model has it, max everywhere adaptive; off by model
        (
            "anthropic",
            "https://api.anthropic.com",
            "claude-opus-5-5",
            E(x),
            "/output_config/effort",
            Some(json!("xhigh")),
        ),
        (
            "anthropic",
            "https://api.anthropic.com",
            "claude-opus-5-5",
            E(Effort::Max),
            "/output_config/effort",
            Some(json!("max")),
        ),
        (
            "anthropic",
            "https://api.anthropic.com",
            "claude-opus-4-6",
            E(x),
            "/output_config/effort",
            Some(json!("high")),
        ),
        (
            "anthropic",
            "https://api.anthropic.com",
            "claude-opus-5-5",
            E(Effort::Minimal),
            "/output_config/effort",
            Some(json!("low")),
        ),
        ("anthropic", "https://api.anthropic.com", "claude-opus-5-1", Off, "/thinking/type", Some(json!("disabled"))),
        ("anthropic", "https://api.anthropic.com", "claude-sonnet-5-1", Off, "/thinking/type", Some(json!("disabled"))),
        ("anthropic", "https://api.anthropic.com", "claude-haiku-5-5", Off, "/thinking/type", Some(json!("disabled"))),
        (
            "anthropic",
            "https://api.anthropic.com",
            "claude-sonnet-5-5",
            Off,
            "/thinking/type",
            Some(json!("between_tools")),
        ),
        ("anthropic", "https://api.anthropic.com", "claude-opus-5-5", Off, "/thinking", None),
        ("anthropic", "https://api.anthropic.com", "claude-fable-5-2", Off, "/thinking", None),
        ("anthropic", "https://api.anthropic.com", "claude-sonnet-4-5", Off, "/thinking", None),
        (
            "anthropic",
            "https://api.anthropic.com",
            "claude-sonnet-4-5",
            E(Effort::Max),
            "/thinking/budget_tokens",
            Some(json!(8191)),
        ),
        // gemini: a budget on 2.5 (Pro cannot go below 128), a level on 3
        (
            "gemini",
            "https://generativelanguage.googleapis.com",
            "gemini-2.5-flash",
            Off,
            "/generationConfig/thinkingConfig/thinkingBudget",
            Some(json!(0)),
        ),
        (
            "gemini",
            "https://generativelanguage.googleapis.com",
            "gemini-2.5-pro",
            Off,
            "/generationConfig/thinkingConfig/thinkingBudget",
            Some(json!(128)),
        ),
        (
            "gemini",
            "https://generativelanguage.googleapis.com",
            "gemini-2.5-pro",
            E(x),
            "/generationConfig/thinkingConfig/thinkingBudget",
            Some(json!(24576)),
        ),
        (
            "gemini",
            "https://generativelanguage.googleapis.com",
            "gemini-3.1-pro-preview",
            E(Effort::Low),
            "/generationConfig/thinkingConfig/thinkingLevel",
            Some(json!("low")),
        ),
        (
            "gemini",
            "https://generativelanguage.googleapis.com",
            "gemini-3.1-pro-preview",
            E(Effort::Max),
            "/generationConfig/thinkingConfig/thinkingLevel",
            Some(json!("high")),
        ),
        (
            "gemini",
            "https://generativelanguage.googleapis.com",
            "gemini-3-pro-preview",
            E(Effort::Medium),
            "/generationConfig/thinkingConfig/thinkingLevel",
            Some(json!("high")),
        ),
        (
            "gemini",
            "https://generativelanguage.googleapis.com",
            "gemini-3-flash-preview",
            E(Effort::Medium),
            "/generationConfig/thinkingConfig/thinkingLevel",
            Some(json!("medium")),
        ),
        (
            "gemini",
            "https://generativelanguage.googleapis.com",
            "gemini-3-flash-preview",
            E(Effort::Medium),
            "/generationConfig/thinkingConfig/thinkingBudget",
            None,
        ),
    ];
    for (proto, base, model, level, ptr, want) in cases {
        let c = cfg(proto, base);
        let mut r = req(model);
        r.reasoning = Some(level);
        let body = match proto {
            "openai-chat" => OpenAiChat::body(&c, &r),
            "openai-responses" => OpenAiResponses::body(&c, &r),
            "anthropic" => Anthropic::body(&c, &r),
            _ => Gemini::body(&c, &r),
        };
        assert_eq!(body.pointer(ptr).cloned(), want, "{proto} {model} {level:?}: {body}");
    }
}

/// The levels each model is offered are the ones it takes: the picker cannot offer a 400.
#[test]
fn offered_levels_per_model_follow_the_vendor_docs() {
    let l = |p: &str, k: &str, b: &str, m: &str| nexo_providers::probe::effort_levels(p, k, b, m, true);
    let a = "https://api.anthropic.com";
    // off thinks no less on Opus 5.5 and Fable: it is not offered, `low` is the least
    assert_eq!(l("anthropic", "", a, "claude-opus-5-5"), ["low", "medium", "high", "xhigh", "max"]);
    assert!(!l("anthropic", "", a, "claude-fable-5-2").contains(&"off".to_string()));
    for m in ["claude-opus-5", "claude-sonnet-5-5", "claude-haiku-5-5"] {
        assert_eq!(l("anthropic", "", a, m)[0], "off", "{m}");
    }
    assert_eq!(l("anthropic", "", a, "claude-sonnet-4-6"), ["off", "low", "medium", "high", "max"]);
    assert_eq!(l("anthropic", "", a, "claude-sonnet-4-5"), ["off", "low", "medium", "high"]);
    assert_eq!(l("openai-chat", "", "https://api.openai.com/v1", "gpt-5.2"), ["low", "medium", "high", "xhigh"]);
    assert_eq!(l("openai-chat", "", "https://api.openai.com/v1", "gpt-5.1"), ["low", "medium", "high"]);
    assert_eq!(l("openai-chat", "", "https://api.x.ai/v1", "grok-4.7"), ["low", "medium", "high", "xhigh"]);
    assert_eq!(l("openai-chat", "", "https://api.x.ai/v1", "grok-4.5"), ["low", "medium", "high"]);
    assert_eq!(l("openai-chat", "", "https://api.deepseek.com/v1", "deepseek-v4"), ["off", "low", "high", "max"]);
    assert!(l("openai-chat", "", "https://api.deepseek.com/v1", "deepseek-reasoner").is_empty());
    assert_eq!(l("openai-chat", "", "https://api.mistral.ai/v1", "mistral-small-latest"), ["off", "high"]);
    assert_eq!(l("gemini", "", "https://generativelanguage.googleapis.com", "gemini-3-pro-preview"), ["low", "high"]);
    assert_eq!(
        l("gemini", "", "https://generativelanguage.googleapis.com", "gemini-2.5-flash"),
        ["off", "low", "medium", "high"]
    );
}

/// A level the model does not list becomes the nearest one it does, the lower on a tie; never `off` for a level.
#[test]
fn nearest_level_clamps_to_what_the_model_lists() {
    use nexo_core::provider::nearest_level;
    let s = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(nearest_level(&s(&["low", "medium", "high"]), "xhigh").as_deref(), Some("high"));
    assert_eq!(nearest_level(&s(&["low", "medium", "high"]), "minimal").as_deref(), Some("low"));
    assert_eq!(nearest_level(&s(&["off", "high"]), "low").as_deref(), Some("high"));
    assert_eq!(nearest_level(&s(&["low", "high"]), "medium").as_deref(), Some("low"));
    assert_eq!(nearest_level(&s(&["low", "medium"]), "medium"), None, "listed: no clamp");
    assert_eq!(nearest_level(&s(&["off", "on"]), "high"), None, "not a ladder: no clamp");
}
