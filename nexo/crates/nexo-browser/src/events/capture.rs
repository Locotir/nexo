//! API capture mode: records what a page's front end sends to its API (method, URL, headers, request body,
//! resource type, and the response body of API calls) and exports it as a HAR file.
//!
//! Off by default (`capture_start` turns it on). Nothing secret is kept: every value is turned into a
//! [`crate::marker`] (or left out) in the event handler, before it reaches a buffer, so the same token shows
//! up as the same marker in the login response and in the requests that send it later.
//!
//! What each source can do:
//! - The headless browser (`chromiumoxide` events) records the type, headers (also the ones the browser adds
//!   itself: `Cookie`, `Set-Cookie`), request body, and the response body of `XHR`, `Fetch` and `Other` requests
//!   whose mime type is JSON or text, read with `Network.getResponseBody` when the request finishes.
//! - The person's own Chrome (relay) forwards only address, method, type, status and mime type of each request:
//!   the extension keeps headers, bodies and cookies in the browser on purpose and sends no `loadingFinished`.
//!   Capture there gives a HAR with URLs, methods, types and statuses, and no headers or bodies.

use super::{lock, Buffers};

mod live;
use crate::redact;
use base64::Engine;
pub use live::{SseEvent, WsCapture, WsFrame};
use serde::Serialize;
use serde_json::{json, Value};

/// Most bytes kept of one body; a longer one is cut and noted.
pub const CAPTURE_BODY_CAP: usize = 1_000_000;
/// Most bytes of all bodies together; once it is reached, further bodies are skipped and counted.
pub const CAPTURE_TOTAL_CAP: usize = 64_000_000;
/// Bodies bigger than this are not even read through the redaction (parsing them costs too much): skipped.
const PARSE_CAP: usize = 16_000_000;
/// Requests kept in one capture.
const CAPTURE_REQUESTS_CAP: usize = 20_000;

/// What a capture has seen, kept and left out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CaptureStats {
    /// Requests seen while capture was on.
    pub requests: u64,
    /// Requests not kept because the capture already had the maximum.
    pub dropped_requests: u64,
    pub request_bodies: u64,
    pub response_bodies: u64,
    /// Bodies kept but cut at [`CAPTURE_BODY_CAP`].
    pub bodies_cut: u64,
    /// Bodies left out because [`CAPTURE_TOTAL_CAP`] was reached.
    pub skipped_total_cap: u64,
    /// Responses of API calls left out because their mime type is neither JSON nor text, or the body is not text.
    pub skipped_binary: u64,
    /// Bodies bigger than the size that is read at all.
    pub skipped_too_large: u64,
    /// Bodies the browser could not give (evicted, none, tab closed).
    pub fetch_failed: u64,
    /// Bytes of all bodies kept.
    pub body_bytes: u64,
    /// WebSockets kept (each is one entry in the HAR).
    pub ws_sockets: u64,
    /// WebSockets left out because the capture already had the maximum.
    pub ws_sockets_dropped: u64,
    /// Frames kept, both directions.
    pub ws_frames: u64,
    /// Frames left out: over the per-socket maximum, over the total size cap, or of a socket that was not kept.
    pub ws_frames_dropped: u64,
    /// Text frames kept but cut at the per-frame cap.
    pub ws_frames_cut: u64,
    /// Binary (and control) frames kept without their payload: only length and opcode.
    pub ws_frames_binary: u64,
    /// EventSource messages kept.
    pub sse_events: u64,
    /// EventSource messages left out (over the per-stream maximum or the total size cap).
    pub sse_events_dropped: u64,
}

/// One captured request with its response, already free of secrets (markers instead of values).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CapturedRequest {
    /// Number within the capture (1, 2, ...), in the order the requests were sent.
    pub seq: u64,
    /// Milliseconds since the Unix epoch.
    pub started_ms: f64,
    pub method: String,
    pub url: String,
    /// `XHR`, `Fetch`, `Document`, `Script`, `Other`, ...; empty when the browser did not say.
    pub resource_type: String,
    pub request_headers: Vec<(String, String)>,
    pub request_mime: String,
    pub request_body: Option<String>,
    /// Why the request body is missing or short ("cut: ...").
    pub request_body_note: Option<String>,
    pub status: Option<u16>,
    pub status_text: String,
    pub mime: String,
    pub response_headers: Vec<(String, String)>,
    pub response_body: Option<String>,
    pub response_body_note: Option<String>,
    pub duration_ms: Option<f64>,
    pub error: Option<String>,
    /// Set for a WebSocket: its frames (the request is the handshake, the status 101).
    pub ws: Option<WsCapture>,
    /// The messages an `EventSource` received; the HAR writes them as a `text/event-stream` body.
    pub sse: Vec<SseEvent>,
    /// EventSource messages of this request left out.
    pub sse_dropped: u64,
}

#[derive(Default)]
pub(crate) struct Capture {
    on: bool,
    next: u64,
    items: Vec<Item>,
    stats: CaptureStats,
    bytes: usize,
}

struct Item {
    id: String,
    req: CapturedRequest,
    /// The browser's monotonic clock at the request, in seconds.
    started: Option<f64>,
    body_done: bool,
    /// For a WebSocket: (monotonic seconds, wall clock milliseconds) at its handshake, to date its frames.
    clock: Option<(f64, f64)>,
}

/// One `Network.requestWillBeSent`, raw. It is turned into markers inside [`Buffers::cap_request`] and not kept.
pub(crate) struct RawRequest<'a> {
    pub id: &'a str,
    pub wall_ms: f64,
    pub now: Option<f64>,
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a Value,
    pub rtype: Option<String>,
    pub post: Option<String>,
    pub has_post: bool,
}

fn header_of(list: &[(String, String)], name: &str) -> String {
    list.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()).unwrap_or_default()
}

/// The request types whose responses are worth reading: the page talking to its API.
fn api_type(rtype: &str) -> bool {
    matches!(rtype.to_lowercase().as_str(), "xhr" | "fetch" | "other")
}

fn text_mime(mime: &str) -> bool {
    let m = mime.to_lowercase();
    m.contains("json") || m.starts_with("text/") || m.contains("xml") || m.contains("graphql")
}

/// Cuts at a character boundary, at most `max` bytes. Returns whether it cut.
fn cut(mut text: String, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

impl Capture {
    /// Redacts a raw body, cuts it and counts it. `None` when it is left out (too large, over the total).
    fn keep(&mut self, raw: &str, mime: &str) -> Option<(String, Option<String>)> {
        if raw.len() > PARSE_CAP {
            self.stats.skipped_too_large += 1;
            return None;
        }
        let marked = redact::body_marked(raw, mime);
        let (text, was_cut) = cut(marked, CAPTURE_BODY_CAP);
        if self.bytes + text.len() > CAPTURE_TOTAL_CAP {
            self.stats.skipped_total_cap += 1;
            return None;
        }
        self.bytes += text.len();
        self.stats.body_bytes = self.bytes as u64;
        let note = was_cut.then(|| format!("cut: {CAPTURE_BODY_CAP} bytes kept"));
        if was_cut {
            self.stats.bodies_cut += 1;
        }
        Some((text, note))
    }

    fn find(&mut self, id: &str) -> Option<&mut Item> {
        self.items.iter_mut().rev().find(|i| i.id == id)
    }
}

impl Buffers {
    /// Starts a fresh capture (the previous one is forgotten).
    pub(crate) fn capture_start(&self) {
        let mut c = lock(&self.capture);
        *c = Capture { on: true, ..Capture::default() };
    }

    /// Stops recording. Bodies that are still being read are waited for (a few seconds at most).
    pub(crate) async fn capture_stop(&self) -> CaptureStats {
        lock(&self.capture).on = false;
        for _ in 0..120 {
            if self.pending.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        lock(&self.capture).stats.clone()
    }

    pub(crate) fn capturing(&self) -> bool {
        lock(&self.capture).on
    }

    pub(crate) fn capture_stats(&self) -> CaptureStats {
        lock(&self.capture).stats.clone()
    }

    /// Everything captured so far, in request order.
    pub(crate) fn captured(&self) -> Vec<CapturedRequest> {
        lock(&self.capture).items.iter().map(|i| i.req.clone()).collect()
    }

    /// Records a request. Returns true when the request has a body the event did not carry (ask the browser).
    pub(crate) fn cap_request(&self, r: &RawRequest<'_>) -> bool {
        let mut c = lock(&self.capture);
        if !c.on {
            return false;
        }
        c.stats.requests += 1;
        if c.items.len() >= CAPTURE_REQUESTS_CAP {
            c.stats.dropped_requests += 1;
            return false;
        }
        c.next += 1;
        let headers = redact::header_list_marked(r.headers);
        let mut req = CapturedRequest {
            seq: c.next,
            started_ms: r.wall_ms,
            method: r.method.to_string(),
            url: redact::url_marked(r.url),
            resource_type: r.rtype.clone().unwrap_or_default(),
            request_mime: header_of(&headers, "content-type"),
            request_headers: headers,
            ..CapturedRequest::default()
        };
        if let Some(raw) = &r.post {
            let mime = req.request_mime.clone();
            if let Some((text, note)) = c.keep(raw, &mime) {
                req.request_body = Some(text);
                req.request_body_note = note;
                c.stats.request_bodies += 1;
            }
        }
        c.items.push(Item { id: r.id.to_string(), req, started: r.now, body_done: false, clock: None });
        r.has_post && r.post.is_none()
    }

    /// The body of a request, as `Network.getRequestPostData` gave it (raw: redacted here).
    pub(crate) fn cap_set_request_body(&self, id: &str, raw: Option<String>) {
        let mut c = lock(&self.capture);
        let Some(mime) = c.find(id).map(|i| i.req.request_mime.clone()) else { return };
        let kept = match raw {
            Some(raw) => c.keep(&raw, &mime),
            None => {
                c.stats.fetch_failed += 1;
                None
            }
        };
        if let Some((text, note)) = kept {
            c.stats.request_bodies += 1;
            if let Some(i) = c.find(id) {
                i.req.request_body = Some(text);
                i.req.request_body_note = note;
            }
        }
    }

    /// Headers the browser added to a request (`Cookie` and others; `requestWillBeSentExtraInfo`), merged in.
    pub(crate) fn cap_extra_request_headers(&self, id: &str, headers: &Value) {
        let mut c = lock(&self.capture);
        if !c.on {
            return;
        }
        if let Some(i) = c.find(id) {
            merge(&mut i.req.request_headers, redact::header_list_marked(headers));
            if i.req.request_mime.is_empty() {
                i.req.request_mime = header_of(&i.req.request_headers, "content-type");
            }
        }
    }

    /// Headers the browser saw on a response and the event did not carry (`Set-Cookie`), merged in.
    pub(crate) fn cap_extra_response_headers(&self, id: &str, headers: &Value) {
        let mut c = lock(&self.capture);
        if let Some(i) = c.find(id) {
            merge(&mut i.req.response_headers, redact::header_list_marked(headers));
        }
    }

    /// The response of the latest request with this id that has none yet.
    pub(crate) fn cap_answered(&self, id: &str, status: Option<u16>, text: &str, mime: &str, headers: &Value) {
        let mut c = lock(&self.capture);
        if let Some(i) = c.items.iter_mut().rev().find(|i| i.id == id && i.req.status.is_none()) {
            i.req.status = status;
            i.req.status_text = text.to_string();
            i.req.mime = mime.to_string();
            merge(&mut i.req.response_headers, redact::header_list_marked(headers));
        }
    }

    /// The end of the latest request with this id: duration (monotonic `now`, seconds) and why it failed.
    pub(crate) fn cap_ended(&self, id: &str, now: Option<f64>, error: Option<String>) {
        let mut c = lock(&self.capture);
        if let Some(i) = c.items.iter_mut().rev().find(|i| i.id == id && i.req.duration_ms.is_none()) {
            i.req.duration_ms = i.started.zip(now).map(|(s, n)| (n - s) * 1_000.0);
            if error.is_some() {
                i.req.error = error;
            }
        }
    }

    /// Whether the body of this finished request should be read: an API call (`XHR`, `Fetch`, `Other`) that was
    /// answered, with a JSON or text mime type. Counts the API calls it turns down for their type of content.
    pub(crate) fn cap_wants_body(&self, id: &str) -> bool {
        let mut c = lock(&self.capture);
        if !c.on {
            return false;
        }
        let Some(i) = c.find(id) else { return false };
        if i.body_done || i.req.status.is_none() || i.req.error.is_some() || !api_type(&i.req.resource_type) {
            return false;
        }
        if !text_mime(&i.req.mime) {
            c.stats.skipped_binary += 1;
            return false;
        }
        if let Some(i) = c.find(id) {
            i.body_done = true;
        }
        true
    }

    /// The response body as `Network.getResponseBody` gave it (raw: decoded, redacted and capped here).
    pub(crate) fn cap_set_response_body(&self, id: &str, raw: Option<(String, bool)>) {
        let mut c = lock(&self.capture);
        let Some(mime) = c.find(id).map(|i| i.req.mime.clone()) else { return };
        let text = match raw {
            None => {
                c.stats.fetch_failed += 1;
                return;
            }
            Some((body, true)) => match base64::engine::general_purpose::STANDARD
                .decode(body.as_bytes())
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
            {
                Some(t) => t,
                None => {
                    c.stats.skipped_binary += 1;
                    return;
                }
            },
            Some((body, false)) => body,
        };
        let kept = c.keep(&text, &mime);
        let ok = kept.is_some();
        if let Some(i) = c.find(id) {
            match kept {
                Some((t, note)) => {
                    i.req.response_body = Some(t);
                    i.req.response_body_note = note;
                }
                None => i.req.response_body_note = Some("left out: too large or over the total cap".to_string()),
            }
        }
        if ok {
            c.stats.response_bodies += 1;
        }
    }
}

/// Adds the headers whose name is not in `into` yet (any case).
fn merge(into: &mut Vec<(String, String)>, extra: Vec<(String, String)>) {
    for (k, v) in extra {
        if !into.iter().any(|(n, _)| n.eq_ignore_ascii_case(&k)) {
            into.push((k, v));
        }
    }
}

/// `2026-11-18T10:20:30.123Z` for milliseconds since the Unix epoch.
fn iso(ms: f64) -> String {
    let total = if ms.is_finite() && ms > 0.0 { ms as i64 } else { 0 };
    let (secs, millis) = (total.div_euclid(1_000), total.rem_euclid(1_000));
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z", rest / 3_600, rest % 3_600 / 60, rest % 60)
}

fn har_headers(list: &[(String, String)]) -> Vec<Value> {
    list.iter().map(|(k, v)| json!({"name": k, "value": v})).collect()
}

fn query_of(url: &str) -> Vec<Value> {
    let query = url.split('#').next().and_then(|u| u.split_once('?')).map_or("", |(_, q)| q);
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            json!({"name": k, "value": v})
        })
        .collect()
}

/// The captured requests as a HAR 1.2 document (JSON text), in request order. Headers, URLs and bodies hold
/// markers (`<secret:...>`) where the page had credentials; no raw secret is in it. Response bodies are there
/// only for the API calls whose body was read (see the module doc).
pub fn export_har(entries: &[CapturedRequest]) -> String {
    export_har_with(entries, &CaptureStats::default())
}

/// [`export_har`], and what the capture left out (see [`CaptureStats`]) in `log._nexoCapture`, a field readers of
/// HAR ignore. WebSocket entries have `_webSocketMessages` (the Chrome devtools extension `nexo api` reads);
/// EventSource entries have the messages as the `text/event-stream` body of the response.
pub fn export_har_with(entries: &[CapturedRequest], stats: &CaptureStats) -> String {
    let entries: Vec<Value> = entries
        .iter()
        .map(|e| {
            let time = e.duration_ms.unwrap_or(-1.0);
            let mut request = json!({
                "method": e.method,
                "url": e.url,
                "httpVersion": "HTTP/1.1",
                "cookies": [],
                "headers": har_headers(&e.request_headers),
                "queryString": query_of(&e.url),
                "headersSize": -1,
                "bodySize": e.request_body.as_ref().map_or(0, String::len),
            });
            if let Some(body) = &e.request_body {
                request["postData"] = json!({"mimeType": e.request_mime, "text": body});
            }
            let stream = (!e.sse.is_empty()).then(|| live::sse_text(&e.sse));
            let body = stream.as_ref().or(e.response_body.as_ref());
            let size = body.map_or(0, String::len);
            let mime = if stream.is_some() && e.mime.is_empty() { "text/event-stream" } else { e.mime.as_str() };
            let mut content = json!({"size": size, "mimeType": mime});
            if let Some(body) = body {
                content["text"] = json!(body);
            }
            let mut out = json!({
                "startedDateTime": iso(e.started_ms),
                "time": time,
                "request": request,
                "response": {
                    "status": e.status.map_or(0, u32::from),
                    "statusText": e.status_text,
                    "httpVersion": "HTTP/1.1",
                    "cookies": [],
                    "headers": har_headers(&e.response_headers),
                    "content": content,
                    "redirectURL": "",
                    "headersSize": -1,
                    "bodySize": size,
                },
                "cache": {},
                "timings": {"send": 0, "wait": time.max(0.0), "receive": 0},
                "_resourceType": e.resource_type,
            });
            if let Some(err) = &e.error {
                out["_error"] = json!(err);
            }
            if let Some(ws) = &e.ws {
                live::ws_fields(&mut out, ws);
            }
            if e.sse_dropped > 0 {
                out["_nexoEventSourceDropped"] = json!(e.sse_dropped);
            }
            out
        })
        .collect();
    let har = json!({"log": {
        "version": "1.2",
        "creator": {"name": "nexo", "version": env!("CARGO_PKG_VERSION")},
        "entries": entries,
        "_nexoCapture": json!(stats),
    }});
    serde_json::to_string_pretty(&har).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_formats_known_instants() {
        assert_eq!(iso(0.0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_700_000_000_123.0), "2023-11-14T22:13:20.123Z");
        assert_eq!(iso(951_782_400_000.0), "2000-02-29T00:00:00.000Z");
    }

    fn sample(seq: u64, url: &str) -> CapturedRequest {
        CapturedRequest {
            seq,
            started_ms: 1_700_000_000_000.0 + seq as f64,
            method: "POST".into(),
            url: redact::url_marked(url),
            resource_type: "XHR".into(),
            request_headers: redact::header_list_marked(&json!({
                "Authorization": "Bearer raw-token-777", "Cookie": "sid=raw-cookie-888", "Content-Type": "application/json"
            })),
            request_mime: "application/json".into(),
            request_body: Some(redact::body_marked(r#"{"user":"ann","password":"raw-pass-999"}"#, "application/json")),
            status: Some(200),
            status_text: "OK".into(),
            mime: "application/json".into(),
            response_headers: redact::header_list_marked(&json!({"Set-Cookie": "sid=raw-cookie-888; Path=/"})),
            response_body: Some(redact::body_marked(r#"{"access_token":"raw-token-777","n":1}"#, "application/json")),
            duration_ms: Some(12.5),
            ..CapturedRequest::default()
        }
    }

    #[test]
    fn har_has_the_fields_and_no_raw_secret() {
        let list = vec![sample(1, "https://a.b/login?token=raw-url-111&q=1"), sample(2, "https://a.b/me")];
        let har = export_har(&list);
        for secret in ["raw-token-777", "raw-cookie-888", "raw-pass-999", "raw-url-111"] {
            assert!(!har.contains(secret), "{secret}");
        }
        let v: Value = serde_json::from_str(&har).expect("valid json");
        assert_eq!(v["log"]["version"], "1.2");
        assert_eq!(v["log"]["creator"]["name"], "nexo");
        let e = v["log"]["entries"].as_array().expect("entries");
        assert_eq!(e.len(), 2);
        assert_eq!(e[0]["startedDateTime"], "2023-11-14T22:13:20.001Z");
        assert_eq!(e[0]["_resourceType"], "XHR");
        assert_eq!(e[0]["time"], 12.5);
        assert_eq!(e[0]["request"]["method"], "POST");
        assert_eq!(e[0]["request"]["postData"]["mimeType"], "application/json");
        assert!(e[0]["request"]["postData"]["text"]
            .as_str()
            .is_some_and(|t| t.contains(&redact::marker("raw-pass-999"))));
        assert_eq!(e[0]["request"]["queryString"][0]["name"], "token");
        assert_eq!(e[0]["response"]["status"], 200);
        assert_eq!(e[0]["response"]["content"]["mimeType"], "application/json");
        assert!(e[0]["response"]["content"]["text"]
            .as_str()
            .is_some_and(|t| t.contains(&redact::marker("raw-token-777"))));
        assert!(e[0]["response"]["headers"].as_array().is_some_and(|h| !h.is_empty()));
        // the token a login response hands out is the one the next request sends
        let auth = e[1]["request"]["headers"]
            .as_array()
            .and_then(|h| h.iter().find(|x| x["name"] == "Authorization"))
            .and_then(|x| x["value"].as_str().map(str::to_string))
            .unwrap_or_default();
        assert_eq!(auth, format!("Bearer {}", redact::marker("raw-token-777")));
        assert_eq!(e[1]["startedDateTime"], "2023-11-14T22:13:20.002Z");
    }

    #[test]
    fn capture_keeps_only_while_on_links_and_caps() {
        let b = Buffers::default();
        let headers = json!({"Content-Type": "application/json", "Authorization": "Bearer t-1"});
        let req = |id: &'static str, post: Option<&str>| RawRequest {
            id,
            wall_ms: 1_000.0,
            now: Some(1.0),
            method: "POST",
            url: "https://a.b/api?token=u-1",
            headers: &headers,
            rtype: Some("XHR".into()),
            post: post.map(str::to_string),
            has_post: post.is_some(),
        };
        assert!(!b.cap_request(&req("0", Some("{}"))), "off: nothing kept");
        assert!(b.captured().is_empty());
        b.capture_start();
        assert!(!b.cap_request(&req("1", Some(r#"{"password":"p-1","a":1}"#))));
        b.cap_answered("1", Some(200), "OK", "application/json", &json!({}));
        assert!(b.cap_wants_body("1"));
        b.cap_set_response_body("1", Some((r#"{"access_token":"t-1"}"#.to_string(), false)));
        b.cap_ended("1", Some(1.5), None);
        // a body the event did not carry
        assert!(b.cap_request(&RawRequest { post: None, has_post: true, ..req("2", None) }));
        b.cap_set_request_body("2", Some("password=p-2&x=1".to_string()));
        // a response that is not text is turned down and counted
        b.cap_request(&req("3", None));
        b.cap_answered("3", Some(200), "OK", "image/png", &json!({}));
        assert!(!b.cap_wants_body("3"));
        let all = b.captured();
        assert_eq!(all.len(), 3);
        let dump = format!("{all:?}");
        for secret in ["p-1", "p-2", "u-1", "t-1"] {
            assert!(!dump.contains(secret), "{secret}");
        }
        assert_eq!(all[0].request_body.as_deref().map(|t| t.contains(&redact::marker("p-1"))), Some(true));
        assert_eq!(all[0].duration_ms, Some(500.0));
        assert!(all[0].response_body.as_deref().is_some_and(|t| t.contains(&redact::marker("t-1"))));
        assert_eq!(all[1].request_body.as_deref(), Some(format!("password={}&x=1", redact::marker("p-2")).as_str()));
        let s = b.capture_stats();
        assert_eq!((s.requests, s.request_bodies, s.response_bodies, s.skipped_binary), (3, 2, 1, 1));
    }

    /// What a HAR written by the browser holds, from the raw events to the file text: no live token, cookie or
    /// password in it, whichever way the browser handed them over (request event, extra headers, response, bodies).
    #[test]
    fn a_har_made_from_raw_events_holds_no_live_secret() {
        let b = Buffers::default();
        b.capture_start();
        let headers = json!({"Authorization": "Bearer live-bearer-1", "X-Api-Key": "live-key-2", "Accept": "*/*"});
        b.cap_request(&RawRequest {
            id: "1",
            wall_ms: 1_000.0,
            now: Some(1.0),
            method: "POST",
            url: "https://a.b/login?access_token=live-url-3",
            headers: &headers,
            rtype: Some("XHR".into()),
            post: Some(r#"{"user":"ann","password":"live-pass-4"}"#.into()),
            has_post: true,
        });
        b.cap_extra_request_headers("1", &json!({"Cookie": "sid=live-cookie-5; theme=dark"}));
        b.cap_answered("1", Some(200), "OK", "application/json", &json!({"Set-Cookie": "sid=live-cookie-6; Path=/"}));
        b.cap_extra_response_headers("1", &json!({"Set-Cookie": "sid=live-cookie-6; Path=/"}));
        assert!(b.cap_wants_body("1"));
        b.cap_set_response_body("1", Some((r#"{"access_token":"live-token-7","n":1}"#.to_string(), false)));
        let har = export_har(&b.captured());
        for secret in [
            "live-bearer-1",
            "live-key-2",
            "live-url-3",
            "live-pass-4",
            "live-cookie-5",
            "live-cookie-6",
            "live-token-7",
        ] {
            assert!(!har.contains(secret), "{secret} reached the HAR: {har}");
        }
        assert!(har.contains("https://a.b/login") && har.contains("theme") && har.contains("Accept"), "{har}");
    }

    #[test]
    fn bodies_are_cut_and_the_total_is_capped() {
        let mut c = Capture::default();
        let big = format!("{{\"a\":\"{}\"}}", "x".repeat(CAPTURE_BODY_CAP + 10));
        let (text, note) = c.keep(&big, "application/json").expect("kept");
        assert_eq!(text.len(), CAPTURE_BODY_CAP);
        assert!(note.is_some());
        assert_eq!(c.stats.bodies_cut, 1);
        c.bytes = CAPTURE_TOTAL_CAP - 1;
        assert!(c.keep("{\"a\":1}", "application/json").is_none());
        assert_eq!(c.stats.skipped_total_cap, 1);
    }

    #[test]
    fn login_token_under_an_ordinary_key_links_to_the_bearer_header() {
        let tok = "eyJhbGciOiJIUzI1NiJ9.abc_DEF-1234567890";
        let mut login = sample(1, "https://a.b/login");
        login.response_body = Some(redact::body_marked(&format!(r#"{{"data":{{"t":"{tok}"}}}}"#), "application/json"));
        let mut next = sample(2, "https://a.b/me");
        next.request_headers = redact::header_list_marked(&json!({"Authorization": format!("Bearer {tok}")}));
        let har = export_har(&[login, next]);
        assert!(!har.contains(tok));
        let m = redact::marker(tok);
        let v: Value = serde_json::from_str(&har).expect("valid json");
        let e = &v["log"]["entries"];
        assert!(e[0]["response"]["content"]["text"].as_str().is_some_and(|t| t.contains(&m)));
        assert_eq!(e[1]["request"]["headers"][0]["value"], format!("Bearer {m}"));
    }
}
