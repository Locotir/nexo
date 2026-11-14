//! The retry path under faults, against local servers: waits, which statuses
//! retry, dead connections, silence, and how long an error stays.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use nexo_core::{Collector, ErrorKind, Message, ModelRequest, ProviderConfig, ProviderError};
use nexo_providers::{registry, Client};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn cfg(base: &str) -> ProviderConfig {
    let mut c = ProviderConfig::new("openai-chat", base, Some("NEXO_TEST_KEY_THAT_IS_NOT_SET"));
    c.models = vec!["m".into()];
    c
}

fn req() -> ModelRequest {
    ModelRequest::new("m", "", vec![Message::user("hi")])
}

/// A client that does not sleep between attempts.
fn quick() -> Client {
    let mut c = Client::new(registry());
    c.max_backoff = Duration::ZERO;
    c
}

async fn first_error(client: &Client, base: &str, retries: u32) -> ProviderError {
    match client.stream(&cfg(base), &req(), retries).await {
        Err(e) => e,
        Ok(_) => panic!("expected an error"),
    }
}

/// Answers every request with `status`, the extra headers and `body`; counts them.
async fn answering(
    status: u16,
    headers: &'static [(&'static str, &'static str)],
    body: String,
) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            h.fetch_add(1, Ordering::SeqCst);
            let body = body.clone();
            tokio::spawn(async move {
                read_request(&mut sock).await;
                let mut head =
                    format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
                for (k, v) in headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                let _ = sock.write_all(format!("{head}\r\n{body}").as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}"), hits)
}

async fn read_request(sock: &mut tokio::net::TcpStream) {
    let mut buf = vec![0u8; 65536];
    let mut got = Vec::new();
    loop {
        let k = sock.read(&mut buf).await.unwrap_or(0);
        if k == 0 {
            return;
        }
        got.extend_from_slice(&buf[..k]);
        let text = String::from_utf8_lossy(&got);
        if let Some(i) = text.find("\r\n\r\n") {
            let len = text[..i]
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))
                })
                .unwrap_or(0usize);
            if got.len() >= i + 4 + len {
                return;
            }
        }
    }
}

// ---- 1 and 2: Retry-After ----

#[tokio::test]
async fn a_retry_after_of_two_seconds_is_waited_not_the_exponential_value() {
    // attempt 1 would wait 1 s on its own; the server asked for 2
    let (base, hits) = answering(429, &[("Retry-After", "2")], "{\"error\":\"slow\"}".into()).await;
    let started = std::time::Instant::now();
    let e = first_error(&Client::new(registry()), &base, 1).await;
    assert!(started.elapsed() >= Duration::from_secs(2), "{:?}", started.elapsed());
    assert_eq!(e.kind, ErrorKind::RateLimit);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

async fn wait_for(value: &'static str) -> Option<Duration> {
    let (base, _) = answering(429, Box::leak(Box::new([("Retry-After", value)])), "slow".into()).await;
    first_error(&Client::new(registry()), &base, 0).await.retry_after
}

#[tokio::test]
async fn retry_after_in_every_shape_and_the_cap_on_the_wait() {
    assert_eq!(wait_for("0").await, Some(Duration::ZERO));
    assert_eq!(wait_for("-5").await, None, "a negative value is ignored: the usual backoff applies");
    assert_eq!(wait_for("soon").await, None);
    assert_eq!(wait_for("86400").await, Some(Duration::from_secs(86_400)));
    let past = wait_for("Mon, 26 Oct 2026 12:00:00 GMT").await;
    assert_eq!(past, Some(Duration::ZERO), "a date in the past");
    let future = wait_for("Wed, 21 Oct 2099 07:28:00 GMT").await.unwrap();
    assert!(future > Duration::from_secs(3600));
    // whatever the server asks, one wait is never longer than max_backoff
    let client = Client::new(registry());
    let mut e = ProviderError::new(ErrorKind::RateLimit, "slow");
    e.retry_after = Some(Duration::from_secs(86_400));
    assert_eq!(client.backoff(1, &e), Duration::from_secs(30));
    // without a hint: 1, 2, 4, 8, 16, 30 (capped), 30
    e.retry_after = None;
    let waits: Vec<u64> = (1..=7).map(|n| client.backoff(n, &e).as_secs()).collect();
    assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30]);
}

// ---- 3: which statuses retry ----

#[tokio::test]
async fn server_statuses_are_retried_and_client_statuses_are_not() {
    for status in [500u16, 502, 503, 504, 529] {
        let (base, hits) = answering(status, &[], "{\"error\":{\"message\":\"down\"}}".into()).await;
        let e = first_error(&quick(), &base, 2).await;
        assert_eq!(hits.load(Ordering::SeqCst), 3, "{status}: the first try and two retries");
        assert!(e.is_retryable(), "{status}");
        assert_eq!(e.status, Some(status));
        assert!(e.human().contains("down"), "{}", e.human());
    }
    for (status, hint) in
        [(400u16, "refused"), (401, "Key rejected"), (403, "Key rejected"), (404, "refused"), (422, "refused")]
    {
        let (base, hits) = answering(status, &[], "{\"error\":{\"message\":\"nope\"}}".into()).await;
        let e = first_error(&quick(), &base, 3).await;
        assert_eq!(hits.load(Ordering::SeqCst), 1, "{status}: a 4xx is never repeated");
        assert!(!e.is_retryable(), "{status}");
        assert!(e.human().contains(hint) && e.human().contains("nope"), "{status}: {}", e.human());
    }
    // the auth ones say what to do
    let (base, _) = answering(401, &[], "{\"error\":{\"message\":\"bad\"}}".into()).await;
    let e = first_error(&quick(), &base, 3).await;
    assert!(
        e.message.contains("NEXO_TEST_KEY_THAT_IS_NOT_SET") && e.message.contains("nexo auth login"),
        "{}",
        e.message
    );
}

// ---- 4: dead connections ----

#[tokio::test]
async fn connection_refused_is_retried_a_bounded_number_of_times() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let started = std::time::Instant::now();
    let e = first_error(&quick(), &format!("http://127.0.0.1:{port}"), 3).await;
    assert_eq!(e.kind, ErrorKind::Network);
    assert!(e.human().starts_with("Could not reach the provider"), "{}", e.human());
    assert!(started.elapsed() < Duration::from_secs(25));
}

#[tokio::test]
async fn a_connection_reset_before_any_byte_is_retried_then_reported() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            h.fetch_add(1, Ordering::SeqCst);
            let mut b = [0u8; 1024];
            let _ = sock.read(&mut b).await;
            drop(sock); // closed with no answer
        }
    });
    let e = first_error(&quick(), &format!("http://{addr}"), 2).await;
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    assert_eq!(e.kind, ErrorKind::Network);
    assert!(e.human().starts_with("Could not reach the provider"), "{}", e.human());
}

// ---- 5: a stream cut after text is never retried ----

#[tokio::test]
async fn a_stream_cut_after_text_is_not_re_issued() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            h.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                read_request(&mut sock).await;
                let part = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"half\"}}]}\n\n";
                let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n";
                let _ = sock.write_all(format!("{head}{part}").as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    let mut s = quick().stream(&cfg(&format!("http://{addr}")), &req(), 3).await.unwrap();
    let mut c = Collector::new();
    while let Some(ev) = s.next().await {
        c.push(ev);
    }
    assert_eq!(c.message().text(), "half", "shown once");
    assert!(c.error.is_some());
    assert_eq!(hits.load(Ordering::SeqCst), 1, "no second request after text was shown");
}

/// A cut after reasoning alone is not the client's to send again either (the reasoning is on the caller's
/// screen): it reaches the caller as a retryable error with nothing said, and the agent loop sends it again.
#[tokio::test]
async fn a_stream_cut_after_reasoning_alone_reaches_the_caller_as_a_retryable_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            h.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                read_request(&mut sock).await;
                let part = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"content\":null,\"reasoning_content\":\"hm\"}}]}\n\n";
                let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n";
                let _ = sock.write_all(format!("{head}{part}").as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    let mut s = quick().stream(&cfg(&format!("http://{addr}")), &req(), 3).await.unwrap();
    let mut c = Collector::new();
    while let Some(ev) = s.next().await {
        c.push(ev);
    }
    let m = c.message();
    assert_eq!((m.reasoning().as_str(), m.text().as_str(), m.tool_calls().count()), ("hm", "", 0));
    let e = c.error.expect("the cut is told");
    assert!(e.is_retryable() && e.message.starts_with("stream cut"), "{e:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 1, "one request: the reasoning was already shown");
}

// ---- 6: silence ----

/// Accepts, reads the request, then says nothing for ever (or after `head`).
async fn silent(head: Option<&'static str>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            read_request(&mut sock).await;
            if let Some(h) = head {
                let _ = sock.write_all(h.as_bytes()).await;
            }
            held.push(sock); // kept open, never written to again
        }
    });
    format!("http://{addr}")
}

#[tokio::test(start_paused = true)]
async fn a_server_that_never_answers_ends_in_an_error_not_a_hang() {
    let base = silent(None).await;
    // paused time: the idle limit passes at once
    let e =
        tokio::time::timeout(Duration::from_secs(3600), first_error(&quick(), &base, 0)).await.expect("the call ended");
    assert_eq!(e.kind, ErrorKind::Network);
    assert!(e.human().starts_with("Could not reach the provider"), "{}", e.human());
}

#[tokio::test(start_paused = true)]
async fn a_stream_that_goes_silent_ends_in_an_error_not_a_hang() {
    let base = silent(Some("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")).await;
    let run = async {
        let mut s = quick().stream(&cfg(&base), &req(), 0).await.unwrap();
        let mut c = Collector::new();
        while let Some(ev) = s.next().await {
            c.push(ev);
        }
        c
    };
    let c = tokio::time::timeout(Duration::from_secs(3600), run).await.expect("the stream ended");
    let e = c.error.expect("the person is told");
    assert_eq!(e.kind, ErrorKind::Network);
    assert!(e.human().starts_with("Could not reach the provider"), "{}", e.human());
}

// ---- 8: the error stays short ----

#[tokio::test]
async fn the_error_the_person_reads_stays_short() {
    let big = "x".repeat(100_000);
    let long_json = format!("{{\"error\":{{\"message\":\"{big}\"}}}}");
    let bodies = [
        ("{\"error\":{\"message\":\"model gone\",\"type\":\"not_found\"}}".to_string(), "model gone"),
        ("{\"status\":\"bad\"}".to_string(), "refused"),
        (String::new(), "refused"),
        (big.clone(), "xxx"),
        (long_json, "xxx"),
        (format!("<html><body>{big}</body></html>"), "web page"),
    ];
    for (body, expect) in bodies {
        let (base, _) = answering(400, &[], body.clone()).await;
        let e = first_error(&quick(), &base, 0).await;
        let h = e.human();
        assert!(h.contains(expect), "{h}");
        assert!(h.chars().count() < 400, "{} chars for a {} byte body", h.chars().count(), body.len());
        assert!(e.message.chars().count() < 700, "message {} chars", e.message.chars().count());
    }
}
