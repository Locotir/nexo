use super::*;

async fn list(server: &MockServer, protocol: &str, path: &str, body: Value) -> Vec<String> {
    server.mock(|w, t| {
        w.method(GET).path(path);
        t.status(200).json_body(body);
    });
    Client::new(registry())
        .list_models(&cfg(protocol, &server.base_url()))
        .await
        .expect("a list")
        .into_iter()
        .map(|m| m.id)
        .collect()
}

#[tokio::test]
async fn t8_openai_style_list_in_data() {
    let s = MockServer::start();
    let got =
        list(&s, "openai-chat", "/v1/models", json!({"object":"list","data":[{"id":"a"},{"id":"b","object":"model"}]}))
            .await;
    assert_eq!(got, ["a", "b"]);
}

#[tokio::test]
async fn t8_together_answers_a_bare_array() {
    // before: an empty list, so `/model` offered nothing for Together
    let s = MockServer::start();
    let got = list(
        &s,
        "openai-chat",
        "/v1/models",
        json!([{"id":"meta-llama/Llama-3.3-70B","type":"chat"},{"id":"x/embed","type":"embedding"}]),
    )
    .await;
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got.contains(&"meta-llama/Llama-3.3-70B".to_string()));
}

#[tokio::test]
async fn t8_anthropic_and_gemini_lists() {
    let s = MockServer::start();
    let got = list(
        &s,
        "anthropic",
        "/v1/models",
        json!({"data":[{"id":"claude-x","display_name":"Claude X"}],"has_more":false}),
    )
    .await;
    assert_eq!(got, ["claude-x"]);
    let g = MockServer::start();
    let got = list(
        &g,
        "gemini",
        "/v1beta/models",
        json!({"models":[
            {"name":"models/gemini-2.5-pro","supportedGenerationMethods":["generateContent","countTokens"],"inputTokenLimit":1048576,"outputTokenLimit":65536},
            {"name":"models/text-embedding-004","supportedGenerationMethods":["embedContent"]}
        ]}),
    )
    .await;
    assert_eq!(got, ["gemini-2.5-pro"], "an embedding model is not a chat model");
}

#[tokio::test]
async fn t8_a_server_without_a_list_and_a_refused_key_are_errors_with_words_not_panics() {
    let s = MockServer::start();
    s.mock(|w, t| {
        w.method(GET).path("/v1/models");
        t.status(404).body("not found");
    });
    let e = Client::new(registry()).list_models(&cfg("openai-chat", &s.base_url())).await.unwrap_err();
    assert!(!e.message.is_empty());
    let k = MockServer::start();
    k.mock(|w, t| {
        w.method(GET).path("/v1/models");
        t.status(401).json_body(json!({"error":{"message":"invalid api key"}}));
    });
    let e = Client::new(registry()).list_models(&cfg("openai-chat", &k.base_url())).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Auth, "{e:?}");
}

#[tokio::test]
async fn t8_probe_finds_lm_studio_by_its_own_route_and_skips_embedding_models() {
    let s = MockServer::start();
    s.mock(|w, t| {
        w.method(GET).path("/api/v0/models");
        t.status(200).json_body(json!({"object":"list","data":[
            {"id":"qwen3-8b","type":"llm","state":"loaded","max_context_length":40960,"loaded_context_length":8192},
            {"id":"nomic-embed","type":"embeddings"}
        ]}));
    });
    let p = nexo_providers::probe::server(&cfg("openai-chat", &s.base_url()), None).await.unwrap();
    assert_eq!(p.kind, "lmstudio");
    assert_eq!(p.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["qwen3-8b"]);
    assert_eq!(p.base_url, format!("{}/v1", s.base_url()));
}

#[tokio::test]
async fn t8_probe_reads_a_bare_array_list_and_says_so_when_a_server_has_no_list() {
    let s = MockServer::start();
    s.mock(|w, t| {
        w.method(GET).path("/v1/models");
        t.status(200).json_body(json!([{"id":"m1"},{"id":"m2"}]));
    });
    let p = nexo_providers::probe::server(&cfg("openai-chat", &format!("{}/v1", s.base_url())), None).await.unwrap();
    assert_eq!(p.models.len(), 2, "{p:?}");
    let none = MockServer::start();
    let p = nexo_providers::probe::server(&cfg("openai-chat", &format!("{}/v1", none.base_url())), None).await.unwrap();
    assert!(p.models.is_empty() && !p.needs_key);
    assert!(p.trail.iter().any(|l| l.contains("404")), "{:?}", p.trail);
}

#[tokio::test]
async fn t8_probe_anthropic_and_gemini_need_a_key_on_401_and_400() {
    for (protocol, path, status) in [("anthropic", "/v1/models", 401), ("gemini", "/v1beta/models", 400)] {
        let s = MockServer::start();
        s.mock(|w, t| {
            w.method(GET).path(path);
            t.status(status).json_body(json!({"error":{"message":"API key not valid"}}));
        });
        let p = nexo_providers::probe::server(&cfg(protocol, &s.base_url()), None).await.unwrap();
        assert!(p.needs_key && p.models.is_empty(), "{protocol}: {p:?}");
    }
}

// ================= through the real client =================
