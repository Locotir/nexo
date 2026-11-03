//! Every protocol's recorded response, replayed whole and in tiny chunks,
//! reduces to the same message: reasoning, text, one `read` call, usage.

use futures::StreamExt;
use nexo_core::{Collector, FinishReason, Part, StreamEvent, Usage};
use nexo_providers::testing::replay;

async fn collect(protocol: &str, text: &str, chunk: usize) -> (Collector, Vec<StreamEvent>) {
    let mut s = replay(protocol, text, chunk).expect("known protocol");
    let mut c = Collector::new();
    let mut evs = Vec::new();
    while let Some(ev) = s.next().await {
        c.push(ev.clone());
        evs.push(ev);
    }
    (c, evs)
}

fn check(c: &Collector, evs: &[StreamEvent], signature: bool) {
    let m = c.message();
    assert_eq!(m.text(), "I'll read it.");
    assert!(m.reasoning().contains("the file"), "reasoning: {:?}", m.reasoning());
    let calls: Vec<_> = m.tool_calls().collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, "read");
    assert_eq!(calls[0].2["path"], "src/main.rs");
    assert_eq!(c.finish_reason(), FinishReason::ToolUse);
    assert_eq!(c.usage.cache_read, 1024);
    assert_eq!(c.usage.input, 176);
    assert!(c.usage.output > 0);
    assert!(matches!(evs.last(), Some(StreamEvent::Done)));
    assert_eq!(evs.iter().filter(|e| matches!(e, StreamEvent::Finish(_))).count(), 1);
    assert_eq!(evs.iter().filter(|e| matches!(e, StreamEvent::Usage(_))).count(), 1);
    assert!(evs.iter().any(|e| matches!(e, StreamEvent::Start { .. })));
    if signature {
        assert!(
            m.content.iter().any(|p| matches!(
                p,
                Part::Reasoning { signature: Some(_), .. } | Part::Reasoning { encrypted: Some(_), .. }
            )),
            "the opaque reasoning data is kept"
        );
    }
}

#[tokio::test]
async fn openai_chat_fixture() {
    let text = include_str!("fixtures/openai_chat.sse");
    for chunk in [0, 7, 1000] {
        let (c, evs) = collect("openai-chat", text, chunk).await;
        check(&c, &evs, false);
        assert_eq!(c.usage, Usage { input: 176, output: 31, cache_read: 1024, cache_write: 0, reasoning: 9 });
    }
}

#[tokio::test]
async fn openai_responses_fixture() {
    let text = include_str!("fixtures/openai_responses.sse");
    for chunk in [0, 7, 1000] {
        let (c, evs) = collect("openai-responses", text, chunk).await;
        check(&c, &evs, true);
        assert_eq!(c.usage, Usage { input: 176, output: 40, cache_read: 1024, cache_write: 0, reasoning: 12 });
        let enc = c.message().content.iter().find_map(|p| match p {
            Part::Reasoning { encrypted, signature, .. } => Some((encrypted.clone(), signature.clone())),
            _ => None,
        });
        assert_eq!(enc, Some((Some("gAAAAABo_ENCRYPTED".into()), Some("rs_01".into()))));
    }
}

#[tokio::test]
async fn anthropic_fixture() {
    let text = include_str!("fixtures/anthropic.sse");
    for chunk in [0, 7, 1000] {
        let (c, evs) = collect("anthropic", text, chunk).await;
        check(&c, &evs, true);
        assert_eq!(c.usage, Usage { input: 176, output: 31, cache_read: 1024, cache_write: 0, reasoning: 0 });
    }
}

#[tokio::test]
async fn gemini_fixture() {
    let text = include_str!("fixtures/gemini.sse");
    for chunk in [0, 7, 1000] {
        let (c, evs) = collect("gemini", text, chunk).await;
        check(&c, &evs, true);
        assert_eq!(c.usage, Usage { input: 176, output: 31, cache_read: 1024, cache_write: 0, reasoning: 12 });
        assert!(c.message().tool_calls().next().unwrap().0.starts_with("call_"), "ids are synthesised");
    }
}

#[tokio::test]
async fn a_cut_stream_reports_the_cut_and_still_ends_with_done() {
    use bytes::Bytes;
    use nexo_core::ErrorKind;
    let head = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n\n";
    let body: nexo_providers::sse::SseStream<_> = nexo_providers::sse::SseStream::new(futures::stream::iter(vec![
        Ok(Bytes::from(head)),
        Err("reset".to_string()),
    ]));
    let mut s = nexo_providers::openai_chat::OpenAiChat::parse(body);
    let mut evs = Vec::new();
    while let Some(ev) = s.next().await {
        evs.push(ev);
    }
    assert!(evs.iter().any(|e| matches!(e, StreamEvent::TextDelta(t) if t == "Hel")));
    assert!(evs.iter().any(|e| matches!(e, StreamEvent::Error(e) if e.kind == ErrorKind::Network)));
    assert!(matches!(evs.last(), Some(StreamEvent::Done)));
}

/// A 200 whose body is not the protocol (a page, nothing, a JSON error
/// without SSE framing) is an error the client can act on, never an empty
/// reply that passes for an answer.
#[tokio::test]
async fn a_body_that_is_not_the_protocol_is_an_error() {
    use nexo_core::ErrorKind;
    for protocol in ["openai-chat", "openai-responses", "anthropic", "gemini"] {
        for (body, kind) in [
            ("<html><body><h1>502 Bad Gateway</h1></body></html>\n", ErrorKind::Parse),
            ("", ErrorKind::Parse),
            ("{\"error\":{\"message\":\"Rate limit reached for requests\",\"type\":\"rate_limit_error\",\"code\":\"rate_limit_exceeded\"}}", ErrorKind::RateLimit),
        ] {
            let (c, evs) = collect(protocol, body, 5).await;
            let err = c.error.clone().unwrap_or_else(|| panic!("{protocol}: no error for {body:?}: {evs:?}"));
            assert_eq!(err.kind, kind, "{protocol}: {body:?}: {}", err.message);
            assert!(matches!(evs.last(), Some(StreamEvent::Done)));
            assert_eq!(c.finish_reason(), FinishReason::Error);
            assert!(c.message().content.is_empty());
        }
    }
}
