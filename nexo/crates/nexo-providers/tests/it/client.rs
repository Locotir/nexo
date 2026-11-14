//! The client against local servers: a plain success, a key that is
//! missing, and a 429 followed by a 200.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures::StreamExt;
use httpmock::prelude::*;
use nexo_core::{Collector, ErrorKind, Message, ModelRequest, ProviderConfig};
use nexo_providers::{registry, Client};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SSE: &str = "data: {\"id\":\"x\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}]}\n\ndata: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: {\"id\":\"x\",\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n";

fn cfg(base: &str) -> ProviderConfig {
    let mut c = ProviderConfig::new("openai-chat", base, Some("NEXO_TEST_KEY_THAT_IS_NOT_SET"));
    c.models = vec!["m".into()];
    c
}

async fn text_of(client: &Client, cfg: &ProviderConfig) -> Result<(String, Collector), nexo_core::ProviderError> {
    let req = ModelRequest::new("m", "", vec![Message::user("hi")]);
    let mut s = client.stream(cfg, &req, 3).await?;
    let mut c = Collector::new();
    while let Some(ev) = s.next().await {
        c.push(ev);
    }
    Ok((c.message().text(), c))
}

#[tokio::test]
async fn streams_from_a_mock_server() {
    let server = MockServer::start_async().await;
    let m = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .header("accept", "text/event-stream")
                .json_body_includes(r#"{"model": "m", "stream": true}"#);
            then.status(200).header("content-type", "text/event-stream").body(SSE);
        })
        .await;
    let client = Client::new(registry());
    let (text, c) = text_of(&client, &cfg(&server.base_url())).await.unwrap();
    assert_eq!(text, "hello");
    assert_eq!(c.usage.output, 1);
    m.assert_async().await;
}

#[tokio::test]
async fn a_missing_key_is_named_on_auth_errors() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(401)
                .body(r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#);
        })
        .await;
    let client = Client::new(registry());
    let e = text_of(&client, &cfg(&server.base_url())).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Auth);
    assert!(e.message.contains("NEXO_TEST_KEY_THAT_IS_NOT_SET"), "{}", e.message);
    assert!(e.message.contains("Incorrect API key"), "{}", e.message);
}

#[tokio::test]
async fn an_unknown_protocol_is_refused() {
    let client = Client::new(registry());
    let e = text_of(&client, &ProviderConfig::new("telepathy", "http://127.0.0.1:9", None)).await.unwrap_err();
    assert!(e.message.contains("unknown protocol"));
    assert!(e.message.contains("openai-chat"));
}

/// A server that answers the first request with 429 (`Retry-After: 0`) and
/// closes, and the second with the SSE body: sequencing httpmock cannot do.
async fn flaky_server() -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let n = h.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let mut got = Vec::new();
                loop {
                    let k = sock.read(&mut buf).await.unwrap_or(0);
                    if k == 0 {
                        break;
                    }
                    got.extend_from_slice(&buf[..k]);
                    let text = String::from_utf8_lossy(&got);
                    if let Some(i) = text.find("\r\n\r\n") {
                        let len = text[..i]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if got.len() >= i + 4 + len {
                            break;
                        }
                    }
                }
                let resp = if n == 0 {
                    "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 18\r\nConnection: close\r\n\r\n{\"error\":\"slow\"}\n".to_string()
                } else {
                    format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", SSE.len(), SSE)
                };
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}"), hits)
}

#[tokio::test]
async fn retries_a_429_and_then_succeeds() {
    let (base, hits) = flaky_server().await;
    let client = Client::new(registry());
    let (text, _) = text_of(&client, &cfg(&base)).await.unwrap();
    assert_eq!(text, "hello");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn gives_up_when_retries_are_exhausted() {
    let server = MockServer::start_async().await;
    let m = server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(503).header("retry-after", "0").body("overloaded");
        })
        .await;
    let client = Client::new(registry());
    let req = ModelRequest::new("m", "", vec![Message::user("hi")]);
    let e = match client.stream(&cfg(&server.base_url()), &req, 2).await {
        Err(e) => e,
        Ok(_) => panic!("a 503 three times is a failure"),
    };
    assert_eq!(e.kind, ErrorKind::Overloaded);
    assert_eq!(m.calls_async().await, 3, "the first try and two retries");
}
