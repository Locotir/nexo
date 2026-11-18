//! The live channels of a capture: WebSocket frames and EventSource messages, from the CDP events of the Network
//! domain (`webSocketCreated`, `webSocketWillSendHandshakeRequest`, `webSocketHandshakeResponseReceived`,
//! `webSocketFrameSent`, `webSocketFrameReceived`, `webSocketFrameError`, `webSocketClosed`,
//! `eventSourceMessageReceived`). The handler gets the event's JSON, so a recorded event can be fed to it as is.
//!
//! A WebSocket is one entry of the HAR: URL, handshake headers, status 101, and `_webSocketMessages`, the Chrome
//! devtools extension `nexo api` reads (`{type: "send" | "receive", time, opcode, data}`). An EventSource is the
//! request that was already captured; its messages are written as the `text/event-stream` body of its response.
//! Frames and messages go through [`redact::frame_marked`] before they are kept, like any body.
//!
//! Bounds: [`WS_SOCKETS_CAP`] sockets, [`WS_FRAMES_CAP`] frames per socket, [`WS_FRAME_CAP`] bytes per frame (a
//! longer text frame is cut), and the total byte cap of the capture. Binary frames (and ping, pong, close) keep the
//! opcode and the length (`_length`) and no payload. What was left out is counted in [`CaptureStats`], on the
//! entry (`_nexoWebSocket`, `_nexoEventSourceDropped`) and in `log._nexoCapture`.

use super::super::{lock, Buffers};
use super::{cut, Capture, CapturedRequest, Item, CAPTURE_REQUESTS_CAP, CAPTURE_TOTAL_CAP, PARSE_CAP};
use crate::redact;
use serde::Serialize;
use serde_json::{json, Value};

/// WebSockets kept in one capture.
pub const WS_SOCKETS_CAP: usize = 100;
/// Frames kept per WebSocket (the same number `nexo api` reads).
pub const WS_FRAMES_CAP: usize = 5_000;
/// Bytes kept of one text frame or one EventSource message.
pub const WS_FRAME_CAP: usize = 65_536;
/// Messages kept per EventSource request.
pub const SSE_EVENTS_CAP: usize = 5_000;
/// Bytes kept of an EventSource event name or id, and of a WebSocket error message.
const SSE_FIELD_CAP: usize = 1_024;

/// One WebSocket frame, already free of secrets.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct WsFrame {
    /// Sent by the page.
    pub send: bool,
    /// Seconds since the Unix epoch (the browser's own clock when the handshake was not seen).
    pub time: f64,
    /// 1 text, 2 binary, 9 ping, 10 pong, 8 close.
    pub opcode: u8,
    /// The text; empty for anything but a text frame.
    pub data: String,
    /// Payload length in bytes of a frame whose payload is not kept.
    pub length: Option<usize>,
}

/// The frames of one WebSocket and what was left out.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct WsCapture {
    pub frames: Vec<WsFrame>,
    pub dropped_frames: u64,
    pub cut_frames: u64,
    pub binary_frames: u64,
    pub closed: bool,
    /// The last `webSocketFrameError` message.
    pub error: Option<String>,
}

/// One message of an EventSource.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SseEvent {
    pub event: String,
    pub id: String,
    pub data: String,
}

fn txt<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

/// Bytes a base64 text stands for.
fn decoded_len(b64: &str) -> usize {
    let t = b64.trim_end_matches('=');
    t.len() * 3 / 4
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64() * 1_000.0)
}

/// A field value on one line: a line break in it would start another field of the stream.
fn one_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

/// The messages as the body of a `text/event-stream` response.
pub(super) fn sse_text(events: &[SseEvent]) -> String {
    let mut out = String::new();
    for e in events {
        if !e.event.is_empty() {
            out.push_str(&format!("event: {}\n", one_line(&e.event)));
        }
        if !e.id.is_empty() {
            out.push_str(&format!("id: {}\n", one_line(&e.id)));
        }
        for line in e.data.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
            out.push_str(&format!("data: {line}\n"));
        }
        out.push('\n');
    }
    out
}

/// The WebSocket fields of a HAR entry: the devtools `_webSocketMessages`, and the counters of what was left out.
pub(super) fn ws_fields(out: &mut Value, ws: &WsCapture) {
    out["_resourceType"] = json!("websocket");
    let frames: Vec<Value> = ws
        .frames
        .iter()
        .map(|f| {
            let mut v = json!({
                "type": if f.send { "send" } else { "receive" },
                "time": f.time,
                "opcode": f.opcode,
                "data": f.data,
            });
            if let Some(n) = f.length {
                v["_length"] = json!(n);
            }
            v
        })
        .collect();
    out["_webSocketMessages"] = json!(frames);
    out["_nexoWebSocket"] = json!({
        "closed": ws.closed,
        "frames": ws.frames.len(),
        "dropped_frames": ws.dropped_frames,
        "cut_frames": ws.cut_frames,
        "binary_frames_without_payload": ws.binary_frames,
        "error": ws.error,
    });
}

impl Buffers {
    /// One CDP event of a WebSocket or EventSource (`method` is its name, `p` its parameters). Ignored while
    /// capture is off.
    pub(crate) fn cap_live_event(&self, method: &str, p: &Value) {
        let mut c = lock(&self.capture);
        if !c.on {
            return;
        }
        let id = txt(p, "requestId").to_string();
        if id.is_empty() {
            return;
        }
        match method {
            "Network.webSocketCreated" => c.ws_created(&id, txt(p, "url")),
            "Network.webSocketWillSendHandshakeRequest" => c.ws_handshake(&id, p),
            "Network.webSocketHandshakeResponseReceived" => c.ws_handshake_answer(&id, &p["response"]),
            "Network.webSocketFrameSent" => c.ws_frame(&id, true, p),
            "Network.webSocketFrameReceived" => c.ws_frame(&id, false, p),
            "Network.webSocketFrameError" => {
                if let Some(ws) = c.find(&id).and_then(|i| i.req.ws.as_mut()) {
                    ws.error = Some(cut(redact::frame_marked(txt(p, "errorMessage")), SSE_FIELD_CAP).0);
                }
            }
            "Network.webSocketClosed" => {
                if let Some(i) = c.find(&id) {
                    if let Some(ws) = i.req.ws.as_mut() {
                        ws.closed = true;
                        i.req.duration_ms = i.started.zip(num(p, "timestamp")).map(|(s, n)| (n - s) * 1_000.0);
                    }
                }
            }
            "Network.eventSourceMessageReceived" => c.sse_message(&id, p),
            _ => {}
        }
    }
}

impl Capture {
    fn ws_created(&mut self, id: &str, url: &str) {
        self.stats.requests += 1;
        let sockets = self.items.iter().filter(|i| i.req.ws.is_some()).count();
        if self.items.len() >= CAPTURE_REQUESTS_CAP || sockets >= WS_SOCKETS_CAP {
            self.stats.dropped_requests += 1;
            self.stats.ws_sockets_dropped += 1;
            return;
        }
        self.next += 1;
        let req = CapturedRequest {
            seq: self.next,
            started_ms: now_ms(),
            method: "GET".to_string(),
            url: redact::url_marked(url),
            resource_type: "WebSocket".to_string(),
            ws: Some(WsCapture::default()),
            ..CapturedRequest::default()
        };
        self.items.push(Item { id: id.to_string(), req, started: None, body_done: true, clock: None });
        self.stats.ws_sockets += 1;
    }

    fn ws_handshake(&mut self, id: &str, p: &Value) {
        let Some(i) = self.find(id).filter(|i| i.req.ws.is_some()) else { return };
        let (mono, wall) = (num(p, "timestamp"), num(p, "wallTime"));
        if let (Some(m), Some(w)) = (mono, wall) {
            i.clock = Some((m, w * 1_000.0));
            i.req.started_ms = w * 1_000.0;
        }
        i.started = mono;
        i.req.request_headers = redact::header_list_marked(&p["request"]["headers"]);
    }

    fn ws_handshake_answer(&mut self, id: &str, r: &Value) {
        let Some(i) = self.find(id).filter(|i| i.req.ws.is_some()) else { return };
        i.req.status = r.get("status").and_then(Value::as_u64).and_then(|s| u16::try_from(s).ok());
        i.req.status_text = txt(r, "statusText").to_string();
        i.req.response_headers = redact::header_list_marked(&r["headers"]);
    }

    fn ws_frame(&mut self, id: &str, send: bool, p: &Value) {
        let Some(idx) = self.items.iter().rposition(|i| i.id == id && i.req.ws.is_some()) else {
            self.stats.ws_frames_dropped += 1;
            return;
        };
        // a full socket drops before the frame is masked: a flood costs a counter, not a regex pass
        if self.items[idx].req.ws.as_ref().map_or(0, |w| w.frames.len()) >= WS_FRAMES_CAP {
            if let Some(ws) = self.items[idx].req.ws.as_mut() {
                ws.dropped_frames += 1;
            }
            self.stats.ws_frames_dropped += 1;
            return;
        }
        let f = &p["response"];
        let opcode = num(f, "opcode").map_or(1, |o| o as u8);
        let payload = txt(f, "payloadData");
        // text is masked first and cut after, so a cut never leaves the start of a token visible
        let (data, length, was_cut) = if opcode == 1 {
            if payload.len() > PARSE_CAP {
                (String::new(), Some(payload.len()), true)
            } else {
                let (t, was_cut) = cut(redact::frame_marked(payload), WS_FRAME_CAP);
                (t, None, was_cut)
            }
        } else {
            (String::new(), Some(decoded_len(payload)), false)
        };
        let over_total = self.bytes + data.len() > CAPTURE_TOTAL_CAP;
        let item = &mut self.items[idx];
        let clock = item.clock;
        let Some(ws) = item.req.ws.as_mut() else { return };
        if over_total {
            ws.dropped_frames += 1;
            self.stats.ws_frames_dropped += 1;
            return;
        }
        let ts = num(p, "timestamp").unwrap_or(0.0);
        let time = clock.map_or(ts, |(m, w)| (w / 1_000.0) + (ts - m));
        if opcode != 1 {
            ws.binary_frames += 1;
            self.stats.ws_frames_binary += 1;
        }
        if was_cut {
            ws.cut_frames += 1;
            self.stats.ws_frames_cut += 1;
        }
        self.bytes += data.len();
        self.stats.body_bytes = self.bytes as u64;
        self.stats.ws_frames += 1;
        ws.frames.push(WsFrame { send, time, opcode, data, length });
    }

    fn sse_message(&mut self, id: &str, p: &Value) {
        let Some(idx) = self.items.iter().rposition(|i| i.id == id && i.req.ws.is_none()) else {
            self.stats.sse_events_dropped += 1;
            return;
        };
        if self.items[idx].req.sse.len() >= SSE_EVENTS_CAP {
            self.items[idx].req.sse_dropped += 1;
            self.stats.sse_events_dropped += 1;
            return;
        }
        let data = txt(p, "data");
        let marked = if data.len() > PARSE_CAP { String::new() } else { redact::frame_marked(data) };
        let (text, _) = cut(marked, WS_FRAME_CAP);
        // the name and the id are text from the server too
        let (event, _) = cut(redact::frame_marked(txt(p, "eventName")), SSE_FIELD_CAP);
        let (event_id, _) = cut(redact::frame_marked(txt(p, "eventId")), SSE_FIELD_CAP);
        let size = text.len() + event.len() + event_id.len();
        let item = &mut self.items[idx];
        if self.bytes + size > CAPTURE_TOTAL_CAP {
            item.req.sse_dropped += 1;
            self.stats.sse_events_dropped += 1;
            return;
        }
        self.bytes += size;
        self.stats.body_bytes = self.bytes as u64;
        self.stats.sse_events += 1;
        item.req.sse.push(SseEvent { event, id: event_id, data: text });
    }
}

#[cfg(test)]
mod tests {
    use super::super::export_har;
    use super::*;

    fn feed(b: &Buffers, method: &str, p: Value) {
        b.cap_live_event(&format!("Network.{method}"), &p);
    }

    /// CDP events as the browser sends them: one socket with an auth frame, a binary frame, a ticker, a ping, a
    /// close; and an EventSource stream.
    fn record(b: &Buffers) {
        feed(b, "webSocketCreated", json!({"requestId": "7.1", "url": "wss://app.test/socket?token=url-tok-12345678"}));
        feed(
            b,
            "webSocketWillSendHandshakeRequest",
            json!({"requestId": "7.1", "timestamp": 100.0, "wallTime": 1_700_000_000.0,
                "request": {"headers": {"Origin": "https://app.test", "Cookie": "sid=cookie-secret-1"}}}),
        );
        feed(
            b,
            "webSocketHandshakeResponseReceived",
            json!({"requestId": "7.1", "timestamp": 100.1,
                "response": {"status": 101, "statusText": "Switching Protocols", "headers": {"Upgrade": "websocket"}}}),
        );
        let frame = |op: f64, data: &str| json!({"opcode": op, "mask": true, "payloadData": data});
        feed(
            b,
            "webSocketFrameSent",
            json!({"requestId": "7.1", "timestamp": 100.2, "response": frame(1.0, r#"{"type":"auth","token":"ws-token-abc123"}"#)}),
        );
        feed(
            b,
            "webSocketFrameSent",
            json!({"requestId": "7.1", "timestamp": 100.3, "response": frame(1.0, "Bearer plain-bearer-xyz987")}),
        );
        feed(
            b,
            "webSocketFrameReceived",
            json!({"requestId": "7.1", "timestamp": 101.0, "response": frame(2.0, "AAECAwQ=")}),
        );
        feed(
            b,
            "webSocketFrameReceived",
            json!({"requestId": "7.1", "timestamp": 102.0, "response": frame(1.0, r#"{"type":"ticker","symbol":"BTC","price":1.5}"#)}),
        );
        feed(b, "webSocketFrameReceived", json!({"requestId": "7.1", "timestamp": 103.0, "response": frame(9.0, "")}));
        feed(b, "webSocketClosed", json!({"requestId": "7.1", "timestamp": 104.0}));
        // an EventSource: the request goes through the normal events, its messages through this one
        let headers = json!({"Accept": "text/event-stream"});
        b.cap_request(&super::super::RawRequest {
            id: "8.1",
            wall_ms: 1_700_000_001_000.0,
            now: Some(200.0),
            method: "GET",
            url: "https://app.test/stream",
            headers: &headers,
            rtype: Some("EventSource".into()),
            post: None,
            has_post: false,
        });
        b.cap_answered("8.1", Some(200), "OK", "text/event-stream", &json!({"Content-Type": "text/event-stream"}));
        feed(
            b,
            "eventSourceMessageReceived",
            json!({"requestId": "8.1", "timestamp": 201.0, "eventName": "tick", "eventId": "1", "data": r#"{"n":1,"access_token":"sse-token-999"}"#}),
        );
        feed(
            b,
            "eventSourceMessageReceived",
            json!({"requestId": "8.1", "timestamp": 202.0, "eventName": "", "eventId": "", "data": "a\nb"}),
        );
    }

    #[test]
    fn websocket_and_eventsource_events_become_har_entries_without_a_live_token() {
        let b = Buffers::default();
        feed(&b, "webSocketCreated", json!({"requestId": "x", "url": "wss://off.test/"}));
        assert!(b.captured().is_empty(), "off: nothing kept");
        b.capture_start();
        record(&b);
        let har = export_har_with_stats(&b);
        for secret in ["ws-token-abc123", "plain-bearer-xyz987", "sse-token-999", "url-tok-12345678", "cookie-secret-1"]
        {
            assert!(!har.contains(secret), "{secret} reached the HAR: {har}");
        }
        let v: Value = serde_json::from_str(&har).expect("json");
        let e = v["log"]["entries"].as_array().expect("entries");
        assert_eq!(e.len(), 2);
        // the socket
        assert_eq!(e[0]["_resourceType"], "websocket");
        assert_eq!(e[0]["response"]["status"], 101);
        assert!(e[0]["request"]["url"].as_str().is_some_and(|u| u.starts_with("wss://app.test/socket")));
        let frames = e[0]["_webSocketMessages"].as_array().expect("frames");
        assert_eq!(frames.len(), 5);
        let kinds: Vec<(&str, u64)> =
            frames.iter().map(|f| (f["type"].as_str().unwrap_or(""), f["opcode"].as_u64().unwrap_or(0))).collect();
        assert_eq!(kinds, [("send", 1), ("send", 1), ("receive", 2), ("receive", 1), ("receive", 9)]);
        assert!(frames[0]["data"].as_str().is_some_and(|d| d.contains(&redact::marker("ws-token-abc123"))));
        assert!(frames[0]["data"].as_str().is_some_and(|d| d.contains(r#""type":"auth""#)));
        assert_eq!(frames[1]["data"], format!("Bearer {}", redact::marker("plain-bearer-xyz987")));
        assert_eq!(frames[2]["data"], "");
        assert_eq!(frames[2]["_length"], 5);
        assert!((frames[0]["time"].as_f64().unwrap_or(0.0) - 1_700_000_000.2).abs() < 0.01);
        assert_eq!(e[0]["_nexoWebSocket"]["closed"], true);
        assert_eq!(e[0]["_nexoWebSocket"]["binary_frames_without_payload"], 2);
        // the stream: a text/event-stream body
        assert_eq!(e[1]["response"]["content"]["mimeType"], "text/event-stream");
        let body = e[1]["response"]["content"]["text"].as_str().unwrap_or("");
        assert!(body.starts_with("event: tick\nid: 1\ndata: {"), "{body}");
        assert!(body.ends_with("data: a\ndata: b\n\n"), "{body}");
        assert!(body.contains(&redact::marker("sse-token-999")));
        assert_eq!(v["log"]["_nexoCapture"]["ws_frames"], 5);
        assert_eq!(v["log"]["_nexoCapture"]["sse_events"], 2);
        // `nexo-browser` has no dependency on `nexo-api`, so the reader is not run here: the shape above is what
        // `nexo-api/src/har.rs` reads (`_resourceType`, `_webSocketMessages` {type, time, opcode, data},
        // text/event-stream content).
    }

    #[test]
    fn bare_jwt_and_header_tokens_are_masked_and_plain_words_stay() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTYifQ.c2lnbmF0dXJl";
        let b = Buffers::default();
        b.capture_start();
        let headers = json!({"Authorization": "Bearer hdr-token-abcdefgh12345"});
        b.cap_request(&super::super::RawRequest {
            id: "h1",
            wall_ms: 1.0,
            now: Some(1.0),
            method: "GET",
            url: "https://app.test/api",
            headers: &headers,
            rtype: Some("XHR".into()),
            post: None,
            has_post: false,
        });
        feed(&b, "webSocketCreated", json!({"requestId": "w1", "url": "wss://app.test/"}));
        let text = |d: &str| json!({"requestId": "w1", "timestamp": 1.0, "response": {"opcode": 1.0, "mask": true, "payloadData": d}});
        feed(&b, "webSocketFrameSent", text(&format!("login {jwt} ok")));
        feed(&b, "webSocketFrameSent", text("auth hdr-token-abcdefgh12345 done"));
        feed(&b, "webSocketFrameSent", text("basic settings and bearer of news"));
        let all = b.captured();
        let frames = &all[1].ws.as_ref().expect("ws").frames;
        assert_eq!(frames[0].data, format!("login {} ok", redact::marker(jwt)));
        assert_eq!(frames[1].data, format!("auth {} done", redact::marker("hdr-token-abcdefgh12345")));
        assert_eq!(frames[2].data, "basic settings and bearer of news");
        let har = export_har(&all);
        assert!(!har.contains(jwt) && !har.contains("hdr-token-abcdefgh12345"), "{har}");
    }

    fn export_har_with_stats(b: &Buffers) -> String {
        super::super::export_har_with(&b.captured(), &b.capture_stats())
    }

    #[test]
    fn sockets_frames_and_bytes_are_capped_and_counted() {
        let b = Buffers::default();
        b.capture_start();
        for n in 0..WS_SOCKETS_CAP + 3 {
            feed(&b, "webSocketCreated", json!({"requestId": format!("s{n}"), "url": "wss://a.test/"}));
        }
        let frame = |d: &str| json!({"requestId": "s0", "timestamp": 1.0, "response": {"opcode": 1.0, "mask": false, "payloadData": d}});
        for _ in 0..WS_FRAMES_CAP + 2 {
            feed(&b, "webSocketFrameReceived", frame("x"));
        }
        feed(&b, "webSocketFrameReceived", frame(&"y".repeat(WS_FRAME_CAP + 10)));
        feed(
            &b,
            "webSocketFrameReceived",
            json!({"requestId": "nope", "timestamp": 1.0, "response": {"opcode": 1.0, "mask": false, "payloadData": "z"}}),
        );
        let s = b.capture_stats();
        assert_eq!((s.ws_sockets, s.ws_sockets_dropped), (WS_SOCKETS_CAP as u64, 3));
        assert_eq!(s.ws_frames, WS_FRAMES_CAP as u64);
        assert_eq!(s.ws_frames_dropped, 4, "2 over the cap, the long one (cap reached), the unknown socket");
        let all = b.captured();
        let ws = all[0].ws.as_ref().expect("ws");
        assert_eq!(ws.frames.len(), WS_FRAMES_CAP);
        assert_eq!(ws.dropped_frames, 3);
        // a long text frame is cut when there is room
        let b = Buffers::default();
        b.capture_start();
        feed(&b, "webSocketCreated", json!({"requestId": "s0", "url": "wss://a.test/"}));
        feed(&b, "webSocketFrameReceived", frame(&"y".repeat(WS_FRAME_CAP + 10)));
        let all = b.captured();
        let ws = all[0].ws.as_ref().expect("ws");
        assert_eq!((ws.frames[0].data.len(), ws.cut_frames), (WS_FRAME_CAP, 1));
        let har = export_har(&all);
        assert!(har.contains("cut_frames"));
    }
}
