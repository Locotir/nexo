//! The HTTP plumbing the adapters share: one client, a streaming POST that
//! classifies failures, URL joining, and the stream type the parsers take.

use std::sync::Mutex;
use std::time::Duration;

use futures::{Stream, TryStreamExt};
use nexo_core::{ErrorKind, ProviderConfig, ProviderError};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, RETRY_AFTER};
use serde_json::Value;

use crate::sse::{ByteResult, SseStream};

pub const USER_AGENT: &str = concat!("nexo/", env!("CARGO_PKG_VERSION"));

/// Read timeout between two chunks of a stream; a model that says nothing
/// for this long has gone.
const IDLE: Duration = Duration::from_secs(300);

/// The client of the running tokio runtime, or why TLS could not be set up (see [`crate::tls`]).
///
/// A pooled connection is driven by a task of the runtime that opened it, so once that runtime ends
/// the connection is dead and a request handed to it fails with "error sending request" and no
/// answer. Each runtime gets a client of its own; a process with one runtime has one client.
pub fn client() -> Result<reqwest::Client, ProviderError> {
    let id = tokio::runtime::Handle::try_current().ok().map(|h| h.id());
    let mut all = CLIENTS.lock().unwrap_or_else(|p| p.into_inner());
    let built = kept_or_new(&mut all, id, || {
        crate::tls::build_client(|| {
            reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .connect_timeout(Duration::from_secs(30))
                .read_timeout(IDLE)
        })
    });
    built.map_err(|e| ProviderError::new(ErrorKind::Other, e))
}

type Built = Result<reqwest::Client, String>;
type Kept = Vec<(Option<tokio::runtime::Id>, Built)>;
static CLIENTS: Mutex<Kept> = Mutex::new(Vec::new());

/// The client kept for this runtime, or a new one; runtimes that ended are not told apart from live
/// ones, so the oldest goes when the list is full.
fn kept_or_new(all: &mut Kept, id: Option<tokio::runtime::Id>, build: impl FnOnce() -> Built) -> Built {
    if let Some((_, b)) = all.iter().find(|(k, _)| *k == id) {
        return b.clone();
    }
    let b = build();
    if all.len() >= RUNTIMES_KEPT {
        let _ = all.remove(0);
    }
    all.push((id, b.clone()));
    b
}

/// How many runtimes keep a client; the oldest goes first.
const RUNTIMES_KEPT: usize = 8;

/// `base_url/version/path`, unless the base already ends with a version
/// segment (`/v1`, `/v1beta`): then `base_url/path`. For protocols whose
/// vendors document a base that may carry a sub-path (`…/anthropic`).
pub fn join_versioned(base_url: &str, version: &str, path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    let tail = base.rsplit('/').next().unwrap_or("");
    if tail == version || (tail.starts_with('v') && tail[1..].starts_with(|c: char| c.is_ascii_digit())) {
        format!("{base}/{path}")
    } else {
        format!("{base}/{version}/{path}")
    }
}

/// `base_url/path` when the base has any path (`…/v1`, `…/openai/v1`,
/// `…/v1beta/openai`), else `base_url/v1/path`: the OpenAI convention.
pub fn join_openai(base_url: &str, path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    let pathless = reqwest::Url::parse(base).map(|u| u.path().trim_matches('/').is_empty()).unwrap_or(false);
    if pathless {
        format!("{base}/v1/{path}")
    } else {
        format!("{base}/{path}")
    }
}

/// An OpenAI-style endpoint of `cfg`: [`join_openai`], or `base_url/path`
/// as is when the provider serves no version segment (`extra.versionless`).
pub fn openai_url(cfg: &ProviderConfig, path: &str) -> String {
    if cfg.extra.get("versionless").and_then(|v| v.as_bool()).unwrap_or(false) {
        format!("{}/{}", cfg.base_url.trim_end_matches('/'), path.trim_start_matches('/'))
    } else {
        join_openai(&cfg.base_url, path)
    }
}

/// Headers from the config's extra table plus the protocol's own.
pub fn headers(cfg: &ProviderConfig, own: &[(&str, String)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in own
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .chain(cfg.headers.iter().map(|(k, v)| (k.clone(), v.clone())))
    {
        if let (Ok(name), Ok(value)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(&v)) {
            h.insert(name, value);
        }
    }
    h
}

/// A byte stream of a response body, errors as text.
pub type BodyStream = std::pin::Pin<Box<dyn Stream<Item = ByteResult> + Send>>;

/// POSTs `body` and returns the response's byte stream, or the classified error.
pub async fn post_stream(url: &str, headers: HeaderMap, body: &Value) -> Result<BodyStream, ProviderError> {
    let resp = client()?.post(url).headers(headers).json(body).send().await.map_err(net_error)?;
    // the plan figures ride on every response, an error one (the limit reached) included
    crate::usage::record_headers(resp.headers());
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let retry_after = retry_after(resp.headers());
        let text = resp.text().await.unwrap_or_default();
        let mut e = ProviderError::from_http(status, &pretty_error(&text));
        e.retry_after = retry_after;
        return Err(e);
    }
    Ok(Box::pin(resp.bytes_stream().map_err(|e| e.to_string())))
}

/// GETs `url` and parses the JSON.
pub async fn get_json(url: &str, headers: HeaderMap) -> Result<Value, ProviderError> {
    let resp = client()?.get(url).headers(headers).send().await.map_err(net_error)?;
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(net_error)?;
    if !(200..300).contains(&status) {
        return Err(ProviderError::from_http(status, &pretty_error(&text)));
    }
    serde_json::from_str(&text).map_err(|e| ProviderError::new(ErrorKind::Parse, format!("not JSON: {e}")))
}

pub fn sse(body: BodyStream) -> SseStream<BodyStream> {
    SseStream::new(body)
}

pub fn net_error(e: reqwest::Error) -> ProviderError {
    let kind = if e.is_timeout() || e.is_connect() || e.is_request() || e.is_body() {
        ErrorKind::Network
    } else {
        ErrorKind::Other
    };
    ProviderError::new(kind, with_causes(&e))
}

/// An error with the causes under it (`a: b: c`). reqwest says only "error sending request" and keeps
/// the reason (no such host, connection refused, a certificate) in its source chain.
fn with_causes(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut next = e.source();
    while let Some(cause) = next {
        let said = cause.to_string();
        if !out.contains(&said) {
            out.push_str(": ");
            out.push_str(&said);
        }
        next = cause.source();
    }
    out
}

fn retry_after(h: &HeaderMap) -> Option<Duration> {
    parse_retry_after(h.get(RETRY_AFTER)?.to_str().ok()?, time::OffsetDateTime::now_utc())
}

/// `Retry-After` as seconds, or as an HTTP date (`Wed, 21 Oct 2015 07:28:00 GMT`)
/// taken relative to `now`; a date in the past is no wait at all.
pub fn parse_retry_after(v: &str, now: time::OffsetDateTime) -> Option<Duration> {
    let v = v.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(Duration::from_secs(secs.min(86_400)));
    }
    if let Ok(f) = v.parse::<f64>() {
        return (f >= 0.0).then(|| Duration::from_secs_f64(f.min(86_400.0)));
    }
    let parts: Vec<&str> = v.split_whitespace().collect();
    // RFC 7231: "Sun, 06 Nov 1994 08:49:37 GMT"
    let (day, mon, year, hms) = match parts.as_slice() {
        [_, d, m, y, t, ..] => (*d, *m, *y, *t),
        _ => return None,
    };
    let month = match &mon.to_ascii_lowercase()[..] {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    let mut t = hms.split(':').map(|x| x.parse::<u8>().ok());
    let (h, mi, s) = (t.next()??, t.next()??, t.next()??);
    let date =
        time::Date::from_calendar_date(year.parse().ok()?, time::Month::try_from(month).ok()?, day.parse().ok()?)
            .ok()?;
    let when = time::PrimitiveDateTime::new(date, time::Time::from_hms(h, mi, s).ok()?).assume_utc();
    let wait = when - now;
    Some(if wait.is_positive() { Duration::from_secs(wait.whole_seconds().min(86_400) as u64) } else { Duration::ZERO })
}

fn looks_like_html(t: &str) -> bool {
    let h = t.trim_start().get(..200.min(t.trim_start().len())).unwrap_or("").to_ascii_lowercase();
    h.starts_with("<!doctype html") || h.starts_with("<html") || h.contains("<html") || h.contains("<head>")
}

/// The message inside `{"error": {"message": ...}}` when the body is that shape.
pub fn pretty_error(text: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        let e = &v["error"];
        if let Some(m) = e["message"].as_str().or_else(|| e.as_str()).or_else(|| v["message"].as_str()) {
            let t = e["type"].as_str().or_else(|| e["code"].as_str()).unwrap_or("");
            return if t.is_empty() { m.to_string() } else { format!("{t}: {m}") };
        }
    }
    if looks_like_html(text) {
        return "the server answered with a web page, not an API reply (check the base URL, and that the server is up and serves the OpenAI-compatible API)".into();
    }
    text.to_string()
}

/// A body stream made of the given chunks (tests, fixtures).
pub fn body_from(chunks: Vec<ByteResult>) -> BodyStream {
    Box::pin(futures::stream::iter(chunks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Layer(&'static str, Option<Box<Layer>>);
    impl std::fmt::Display for Layer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }
    impl std::error::Error for Layer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1.as_deref().map(|l| l as &(dyn std::error::Error + 'static))
        }
    }

    #[test]
    fn the_causes_of_an_error_are_kept_once_each() {
        let e = Layer(
            "error sending request",
            Some(Box::new(Layer("connect error", Some(Box::new(Layer("Connection refused (os error 111)", None)))))),
        );
        assert_eq!(with_causes(&e), "error sending request: connect error: Connection refused (os error 111)");
        let e = Layer("outer: inner", Some(Box::new(Layer("inner", None))));
        assert_eq!(with_causes(&e), "outer: inner", "a cause the message already carries is not said twice");
    }

    #[tokio::test]
    async fn a_refused_connection_says_why_in_the_message() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let e = reqwest::Client::new().get(format!("http://127.0.0.1:{port}/")).send().await.unwrap_err();
        let p = net_error(e);
        assert_eq!(p.kind, ErrorKind::Network);
        let m = p.message.to_ascii_lowercase();
        assert!(m.contains("refused") || m.contains("os error 111") || m.contains("10061"), "{m}");
    }

    #[test]
    fn joins_with_or_without_the_version() {
        assert_eq!(
            join_openai("https://api.openai.com/v1", "chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            join_openai("https://api.deepseek.com", "chat/completions"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            join_openai("https://api.deepseek.com/", "/chat/completions"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(join_openai("https://api.groq.com/openai/v1", "models"), "https://api.groq.com/openai/v1/models");
        assert_eq!(
            join_openai("https://generativelanguage.googleapis.com/v1beta/openai", "chat/completions"),
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
        );
        assert_eq!(join_openai("http://localhost:11434", "models"), "http://localhost:11434/v1/models");
        assert_eq!(
            join_versioned("https://api.anthropic.com", "v1", "messages"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            join_versioned("https://api.anthropic.com/v1", "v1", "messages"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            join_versioned("https://api.deepseek.com/anthropic", "v1", "messages"),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
        assert_eq!(
            join_versioned("https://generativelanguage.googleapis.com", "v1beta", "models"),
            "https://generativelanguage.googleapis.com/v1beta/models"
        );
        assert_eq!(
            join_versioned("https://generativelanguage.googleapis.com/v1beta", "v1beta", "models"),
            "https://generativelanguage.googleapis.com/v1beta/models"
        );
    }

    #[test]
    fn reads_retry_after_as_seconds_or_as_a_date() {
        let now = time::macros::datetime!(2026-10-27 12:00:00 UTC);
        assert_eq!(parse_retry_after("7", now), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after("0.5", now), Some(Duration::from_millis(500)));
        assert_eq!(parse_retry_after("Tue, 27 Oct 2026 12:00:30 GMT", now), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after("Mon, 26 Oct 2026 12:00:00 GMT", now), Some(Duration::ZERO), "a date in the past");
        assert_eq!(parse_retry_after("soon", now), None);
        assert_eq!(parse_retry_after("999999999", now), Some(Duration::from_secs(86_400)), "capped at a day");
    }

    #[test]
    fn reads_the_error_message() {
        assert_eq!(
            pretty_error(r#"{"error":{"message":"bad key","type":"authentication_error"}}"#),
            "authentication_error: bad key"
        );
        assert_eq!(pretty_error("plain"), "plain");
    }
}

#[cfg(test)]
mod client_tests {
    use super::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().build().unwrap()
    }

    #[test]
    fn each_runtime_has_a_client_of_its_own_and_reuses_it() {
        let (a, b) = (rt(), rt());
        let mut all = Kept::new();
        let mut get = |r: &tokio::runtime::Runtime, tag: &str| {
            kept_or_new(&mut all, Some(r.handle().id()), || Err(tag.to_string())).unwrap_err()
        };
        assert_eq!(get(&a, "a"), "a");
        assert_eq!(get(&b, "b"), "b", "another runtime gets its own client");
        assert_eq!(get(&a, "new"), "a", "the same runtime keeps its client");
    }

    #[test]
    fn the_list_of_clients_stays_short_and_drops_the_oldest() {
        let mut all = Kept::new();
        let first = rt();
        kept_or_new(&mut all, Some(first.handle().id()), || Err("first".into())).unwrap_err();
        let rts: Vec<_> = (0..RUNTIMES_KEPT).map(|_| rt()).collect();
        for r in &rts {
            kept_or_new(&mut all, Some(r.handle().id()), || Err("x".into())).unwrap_err();
        }
        assert_eq!(all.len(), RUNTIMES_KEPT);
        assert!(!all.iter().any(|(k, _)| *k == Some(first.handle().id())));
    }

    #[test]
    fn outside_a_runtime_there_is_still_a_client() {
        assert!(client().is_ok());
    }
}
