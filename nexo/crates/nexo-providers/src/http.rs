//! The HTTP plumbing the adapters share: one client, a streaming POST that
//! classifies failures, URL joining, and the stream type the parsers take.

use std::sync::OnceLock;
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

pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(IDLE)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

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
    let resp = client().post(url).headers(headers).json(body).send().await.map_err(net_error)?;
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
    let resp = client().get(url).headers(headers).send().await.map_err(net_error)?;
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
    ProviderError::new(kind, e.to_string())
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

/// The message inside `{"error": {"message": ...}}` when the body is that shape.
pub fn pretty_error(text: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        let e = &v["error"];
        if let Some(m) = e["message"].as_str().or_else(|| e.as_str()).or_else(|| v["message"].as_str()) {
            let t = e["type"].as_str().or_else(|| e["code"].as_str()).unwrap_or("");
            return if t.is_empty() { m.to_string() } else { format!("{t}: {m}") };
        }
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
