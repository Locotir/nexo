//! `nexo api watch MAP CHANNEL --for 60s` for a WebSocket channel (the event-stream half lives in `api_call`).
//!
//! It connects to the channel's `ws://` or `wss://` URL (TLS through rustls and the system's root certificates) with
//! the headers the map's auth schemes give, plus the `Origin` of the page, sends the subscribe messages the map
//! recorded, answers pings, reads for the window, counts the messages by type with `nexo_api::channel::observe` (the
//! same path the SSE follow uses) and prints the same report. The `<secret>` placeholders of a recorded subscribe
//! message are filled from the same auth values that fill the headers; a placeholder that cannot be filled stops the run
//! before anything is opened. A filled message is a `call::Val`: it prints as `<secret>`, it is never stored, and the
//! only place that reads it back is the send.

use crate::api_call::{broken, follow_report};
use crate::headless::EXIT_USAGE;
use async_tungstenite::tungstenite::{self, client::IntoClientRequest, protocol::WebSocketConfig, Message};
use futures::{SinkExt, StreamExt};
use nexo_api::channel::{self, Channel, Kind, Msg};
use nexo_api::exchange::{self, Fp};
use nexo_api::{call, spec::Api};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// An exit code and what to say.
type Fail = (i32, String);

/// How long one send may take (never past the follow's own window).
const TIMEOUT: Duration = Duration::from_secs(30);
/// The most the closing handshake may take, and never past the end of the window.
const CLOSE_WAIT: Duration = Duration::from_millis(500);
/// How many bytes of frames are read in one follow; binary frames count by length only.
const FOLLOW_CAP: usize = 8 << 20;
/// The largest message or frame the connection accepts.
const MAX_MESSAGE: usize = 1 << 20;

fn usage(e: String) -> Fail {
    (EXIT_USAGE, e)
}

// ---------------------------------------------------------------- subscribe messages

/// One auth value that may stand for a `<secret>` of a subscribe message.
#[derive(Clone)]
pub struct Cand {
    /// The scheme's label as `describe` shows it (`bearer`, `header:x-api-key`, `csrf:x-csrf-token`).
    pub label: String,
    value: String,
}

impl Cand {
    pub fn new(label: &str, value: &str) -> Cand {
        Cand { label: label.to_string(), value: value.to_string() }
    }
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

/// The auth value for a field of a subscribe message: the only one there is, else the one whose scheme name is the
/// field's, or a bearer for a token field, or a csrf scheme for a csrf field. A cookie is never put in a message.
fn pick<'a>(cands: &'a [Cand], field: &str) -> Option<&'a Cand> {
    let usable: Vec<&Cand> = cands.iter().filter(|c| !c.label.starts_with("cookie")).collect();
    if usable.len() == 1 {
        return Some(usable[0]);
    }
    let f = norm(field);
    usable.into_iter().find(|c| {
        let base = c.label.split('#').next().unwrap_or("");
        let (kind, name) = base.split_once(':').unwrap_or((base, ""));
        (!name.is_empty() && norm(name) == f)
            || (kind == "bearer"
                && ["token", "accesstoken", "idtoken", "jwt", "bearer", "authorization", "auth"].contains(&f.as_str()))
            || (kind == "csrf" && (f.contains("csrf") || f.contains("xsrf")))
    })
}

/// A bearer value without its word: a message field holds the token itself.
fn bare(c: &Cand) -> &str {
    if c.label.starts_with("bearer") {
        let v = c.value.as_str();
        match v.get(..7) {
            Some(w) if w.eq_ignore_ascii_case("bearer ") => &v[7..],
            _ => v,
        }
    } else {
        c.value.as_str()
    }
}

fn fill_value(v: &mut Value, at: &str, field: &str, cands: &[Cand], n: usize) -> Result<(), String> {
    match v {
        Value::String(s) => {
            if s.contains("<opaque>") {
                return Err(format!(
                    "subscribe message {n}: {at} was a value the map does not keep (<opaque>); record the site again with the value in a field named like a secret"
                ));
            }
            if s.contains(exchange::SECRET) {
                let c = pick(cands, field).ok_or_else(|| no_value(n, at, cands))?;
                *s = s.replace(exchange::SECRET, bare(c));
            }
            Ok(())
        }
        Value::Array(items) => {
            for (i, x) in items.iter_mut().enumerate() {
                fill_value(x, &format!("{at}[{i}]"), field, cands, n)?;
            }
            Ok(())
        }
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                let here = if at.is_empty() { k.clone() } else { format!("{at}.{k}") };
                fill_value(x, &here, k, cands, n)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn no_value(n: usize, at: &str, cands: &[Cand]) -> String {
    let usable: Vec<&str> = cands.iter().filter(|c| !c.label.starts_with("cookie")).map(|c| c.label.as_str()).collect();
    if usable.is_empty() {
        format!("subscribe message {n}: {at} holds a secret and the channel has no auth value that can fill it")
    } else {
        format!(
            "subscribe message {n}: {at} holds a secret and none of the auth values ({}) can be told to be it",
            usable.join(", ")
        )
    }
}

/// The subscribe messages with every `<secret>` filled from the auth values. Fails, naming the field, when a
/// placeholder has no value, when the recorded text was cut, or when it held a value the map does not keep.
pub fn fill_subscribe(recorded: &[String], cands: &[Cand]) -> Result<Vec<call::Val>, String> {
    let mut out = Vec::new();
    for (i, text) in recorded.iter().enumerate() {
        let n = i + 1;
        let t = text.trim();
        let json = t.starts_with('{') || t.starts_with('[');
        match serde_json::from_str::<Value>(t).ok().filter(|_| json) {
            Some(mut v) => {
                if t.contains(exchange::SECRET) || t.contains("<opaque>") {
                    fill_value(&mut v, "", "", cands, n)?;
                    out.push(call::Val::secret(v.to_string(), ""));
                } else {
                    out.push(call::Val::plain(t));
                }
            }
            None => {
                if t.ends_with('…') {
                    return Err(format!(
                        "subscribe message {n} was cut when the map was written; record the site again to get it whole"
                    ));
                }
                if t.contains("<opaque>") {
                    return Err(format!(
                        "subscribe message {n} (text): holds a value the map does not keep (<opaque>)"
                    ));
                }
                if t.contains(exchange::SECRET) {
                    let c = pick(cands, "token").ok_or_else(|| no_value(n, "the text", cands))?;
                    out.push(call::Val::secret(t.replace(exchange::SECRET, bare(c)), ""));
                } else {
                    out.push(call::Val::plain(t));
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- pings

/// `ping` as `pong`, in the case it was written in.
fn swap_word(ping: &str) -> Option<String> {
    let lower = ping.to_ascii_lowercase();
    if lower != "ping" {
        return None;
    }
    Some(if ping.chars().all(|c| c.is_ascii_uppercase()) {
        "PONG".to_string()
    } else if ping.starts_with(|c: char| c.is_ascii_uppercase()) {
        "Pong".to_string()
    } else {
        "pong".to_string()
    })
}

/// What to answer to an application-level ping, when the map knows heartbeats of that shape: a tiny text frame
/// recorded as `"ping"`, or a JSON object whose discriminator field says `ping`. The pong is the one the map recorded
/// (`"pong"`, `{"type":"pong"}`), else the ping word turned into `pong`. A frame the map has no heartbeat for gets no
/// answer.
pub fn pong_for(c: &Channel, text: &str) -> Option<String> {
    let names: Vec<String> = c.beat.as_ref()?.name.split('/').map(str::to_string).collect();
    let t = text.trim();
    let quoted =
        |n: &str| -> Option<String> { n.strip_prefix('"').and_then(|s| s.strip_suffix('"')).map(str::to_string) };
    if let Some(word) = names.iter().filter_map(|n| quoted(n)).find(|w| w == t) {
        if !word.to_ascii_lowercase().contains("ping") {
            return None;
        }
        let recorded =
            names.iter().filter_map(|n| quoted(n)).find(|w| w != &word && w.to_ascii_lowercase().contains("pong"));
        return recorded.or_else(|| swap_word(&word));
    }
    let field = c.field.as_deref()?;
    let v: Value = serde_json::from_str(t).ok().filter(Value::is_object)?;
    let tag = v.get(field)?.as_str()?;
    if !tag.eq_ignore_ascii_case("ping") || !names.iter().any(|n| n == tag) {
        return None;
    }
    let pong = names.iter().find(|n| n.eq_ignore_ascii_case("pong")).cloned().or_else(|| swap_word(tag))?;
    Some(json!({ field: pong }).to_string())
}

// ---------------------------------------------------------------- frames

/// A frame read from the server as a message of the capture's kind, redacted; the number of payload bytes comes with
/// it. A binary frame keeps only its length. A close frame is not a message.
fn incoming(m: &Message, known: &HashSet<Fp>) -> Option<(Msg, usize)> {
    let msg = |opcode: u8, text: String| Msg { send: false, time: None, opcode, text, event: None };
    match m {
        Message::Text(t) => Some((msg(1, channel::redact_text(t.as_str(), known)), t.as_str().len())),
        Message::Binary(b) => Some((msg(2, String::new()), b.len())),
        Message::Ping(b) => Some((msg(9, String::new()), b.len())),
        Message::Pong(b) => Some((msg(10, String::new()), b.len())),
        Message::Close(_) | Message::Frame(_) => None,
    }
}

fn sent(text: String) -> Msg {
    Msg { send: true, time: None, opcode: 1, text, event: None }
}

// ---------------------------------------------------------------- the connection

/// The TLS connector for `wss://`: the system's root certificates, every one that loads and parses is kept; only
/// when none is usable does it fail, saying how many were not. It uses the crypto provider reqwest uses (aws-lc-rs),
/// named here so that no process-wide default has to exist.
fn tls_connector() -> Result<tokio_rustls::TlsConnector, String> {
    let loaded = rustls_native_certs::load_native_certs();
    let mut roots = rustls::RootCertStore::empty();
    let (added, ignored) = roots.add_parsable_certificates(loaded.certs);
    if added == 0 {
        return Err(format!(
            "no system root certificate could be used for TLS ({} failed to load, {ignored} could not be read)",
            loaded.errors.len()
        ));
    }
    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS setup failed: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(tokio_rustls::TlsConnector::from(std::sync::Arc::new(config)))
}

fn ws_error(e: tungstenite::Error) -> String {
    use tungstenite::Error;
    match e {
        Error::Http(r) => format!("the server refused the WebSocket handshake with status {}", r.status().as_u16()),
        Error::Io(_) => "could not connect".to_string(),
        Error::Tls(t) => format!("TLS failed: {t}"),
        Error::Url(u) => format!("bad url: {u}"),
        Error::ConnectionClosed | Error::AlreadyClosed => "the server closed the connection".to_string(),
        other => other.to_string(),
    }
}

/// The handshake request: the URL with the query, the map's headers with their secrets, and the page's `Origin`.
/// This is, with the subscribe sends, where a secret is read back out of a `Val`.
fn handshake(req: &call::Request) -> Result<tungstenite::handshake::client::Request, Fail> {
    use tungstenite::http::{HeaderName, HeaderValue};
    let mut url = url::Url::parse(&req.url).map_err(|e| usage(format!("bad url: {e}")))?;
    let scheme = match url.scheme() {
        "wss" | "https" => "wss",
        "ws" | "http" => "ws",
        other => return Err(usage(format!("{other}:// is not a WebSocket address"))),
    };
    url.set_scheme(scheme).map_err(|_| usage("bad url scheme".to_string()))?;
    if !req.query.is_empty() {
        let mut q = url.query_pairs_mut();
        for (k, v) in &req.query {
            q.append_pair(k, v.reveal());
        }
    }
    let page = {
        let mut o = url.clone();
        let _ = o.set_scheme(if scheme == "wss" { "https" } else { "http" });
        o.origin().ascii_serialization()
    };
    let mut r = url.as_str().into_client_request().map_err(|e| usage(ws_error(e)))?;
    let h = r.headers_mut();
    let mut put = |k: &str, v: &str, secret: bool| -> Result<(), Fail> {
        let name = HeaderName::from_bytes(k.as_bytes()).map_err(|_| usage(format!("bad header name {k}")))?;
        let mut value = HeaderValue::from_str(v).map_err(|_| usage(format!("the value of header {k} is not valid")))?;
        value.set_sensitive(secret);
        h.insert(name, value);
        Ok(())
    };
    put("user-agent", concat!("nexo/", env!("CARGO_PKG_VERSION")), false)?;
    put("origin", &page, false)?;
    for (k, v) in &req.headers {
        if k.eq_ignore_ascii_case("accept") {
            continue;
        }
        put(k, v.reveal(), v.shown() != v.reveal())?;
    }
    Ok(r)
}

fn candidates(api: &Api, vals: &[(usize, String)]) -> Vec<Cand> {
    let labels = call::labels(api);
    vals.iter().filter_map(|(i, v)| labels.get(*i).map(|l| Cand::new(l, v))).collect()
}

/// Follows one WebSocket channel for `span`; prints the report and returns the exit code (1 on drift, 3 when the
/// connection or a read failed, as for an event stream).
pub async fn follow(
    api: &Api,
    c: &Channel,
    req: &call::Request,
    vals: &[(usize, String)],
    known: &HashSet<Fp>,
    span: Duration,
    sel: Option<&crate::api_watch::Sel>,
) -> Result<i32, Fail> {
    let (report, code) = watch(api, c, req, vals, known, span, sel).await?;
    print!("{report}");
    use std::io::Write;
    let _ = std::io::stdout().flush();
    Ok(code)
}

/// The follow itself: what to print, and the exit code.
async fn watch(
    api: &Api,
    c: &Channel,
    req: &call::Request,
    vals: &[(usize, String)],
    known: &HashSet<Fp>,
    span: Duration,
    sel: Option<&crate::api_watch::Sel>,
) -> Result<(String, i32), Fail> {
    // everything that can be refused is refused before anything is opened
    let subscribe = fill_subscribe(&c.subscribe, &candidates(api, vals)).map_err(usage)?;
    let request = handshake(req)?;
    let config = WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE)).max_frame_size(Some(MAX_MESSAGE));
    let connector =
        if request.uri().scheme_str() == Some("wss") { Some(tls_connector().map_err(broken)?) } else { None };
    // a secret that fills a message is hidden from what the follow keeps, even when the map did not know it
    let mut known = known.clone();
    for cand in candidates(api, vals) {
        known.insert(exchange::fingerprint(bare(&cand)));
    }
    let known = &known;
    let mut out = format!("following {} (WebSocket {}{}) for {}s\n", c.id, c.origin, c.path, span.as_secs());
    // the window starts before the connection: connect, TLS and handshake are inside it, as for the event stream
    let started = Instant::now();
    let (mut ws, _) = match tokio::time::timeout(
        span,
        async_tungstenite::tokio::connect_async_with_tls_connector_and_config(request, connector, Some(config)),
    )
    .await
    {
        Ok(r) => r.map_err(|e| broken(ws_error(e)))?,
        Err(_) => return Err(broken("the server did not answer within the --for window".to_string())),
    };
    let mut msgs: Vec<Msg> = Vec::new();
    let mut dropped = 0usize;
    let mut keep = |m: Msg, msgs: &mut Vec<Msg>| {
        if msgs.len() < channel::MAX_FRAMES {
            msgs.push(m);
        } else {
            dropped += 1;
        }
    };
    for s in &subscribe {
        let text = s.reveal().to_string();
        let room = span.saturating_sub(started.elapsed()).min(TIMEOUT);
        tokio::time::timeout(room, ws.send(Message::text(text.clone())))
            .await
            .map_err(|_| broken("sending a subscribe message timed out".to_string()))?
            .map_err(|e| broken(ws_error(e)))?;
        keep(sent(channel::redact_text(&text, known)), &mut msgs);
    }

    let mut read = 0usize;
    let mut ended = false;
    loop {
        let left = span.saturating_sub(started.elapsed());
        if left.is_zero() || read >= FOLLOW_CAP {
            break;
        }
        let frame = match tokio::time::timeout(left, ws.next()).await {
            Err(_) => break,
            Ok(None) => {
                ended = true;
                break;
            }
            Ok(Some(Err(tungstenite::Error::ConnectionClosed))) | Ok(Some(Err(tungstenite::Error::AlreadyClosed))) => {
                ended = true;
                break;
            }
            Ok(Some(Err(e))) => return Err(broken(ws_error(e))),
            Ok(Some(Ok(f))) => f,
        };
        if matches!(frame, Message::Close(_)) {
            ended = true;
            break;
        }
        let Some((m, n)) = incoming(&frame, known) else { continue };
        read += n;
        // a protocol ping is answered by the library; make sure the pong leaves now
        if m.opcode == 9 {
            let _ = tokio::time::timeout(left, SinkExt::<Message>::flush(&mut ws)).await;
        }
        let reply = if m.opcode == 1 { pong_for(c, &m.text) } else { None };
        keep(m, &mut msgs);
        if let Some(p) = reply {
            let room = span.saturating_sub(started.elapsed()).min(TIMEOUT);
            let sent_ok = tokio::time::timeout(room, ws.send(Message::text(p.clone()))).await;
            if matches!(sent_ok, Ok(Ok(()))) {
                keep(sent(p), &mut msgs);
            }
        }
    }
    let secs = started.elapsed().as_secs_f64();
    // a close the server does not answer in time is not waited for: the stream is dropped
    let room = span.saturating_sub(started.elapsed()).min(CLOSE_WAIT);
    let _ = tokio::time::timeout(room, ws.close(None)).await;
    drop(ws);

    if let Some(sel) = sel {
        // --select / --when: the picked values of each message, not the report on message types
        let (mut text, code) = crate::api_watch::channel_report(sel, &msgs);
        if dropped > 0 {
            text.push_str(&format!(
                "note: {dropped} more messages came after the first {} and were not read\n",
                channel::MAX_FRAMES
            ));
        }
        return Ok((text, code));
    }
    let live = channel::observe(Kind::Ws, &c.origin, &c.path, &msgs, Some(secs));
    out.push_str(&follow_report(c, &live, secs, ended));
    if dropped > 0 {
        out.push_str(&format!(
            "note: {dropped} more messages came after the first {} and were not analysed\n",
            channel::MAX_FRAMES
        ));
    }
    let drift = nexo_api::drift::channel(c, &live).iter().any(|f| f.drift);
    Ok((out, if drift { 1 } else { 0 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_tungstenite::tokio::accept_async;

    fn cands() -> Vec<Cand> {
        vec![Cand::new("bearer", "Bearer abc123def456")]
    }

    #[test]
    fn a_placeholder_is_filled_wherever_it_is() {
        let rec = vec![
            r#"{"op":"auth","data":{"token":"<secret>"},"list":["<secret>"]}"#.to_string(),
            r#"{"op":"sub","channel":"prices"}"#.to_string(),
        ];
        let out = fill_subscribe(&rec, &cands()).unwrap();
        assert_eq!(out[0].reveal(), r#"{"op":"auth","data":{"token":"abc123def456"},"list":["abc123def456"]}"#);
        assert_eq!(out[1].reveal(), rec[1]);
        assert!(!format!("{:?}", out[0]).contains("abc123"));
    }

    #[test]
    fn text_and_a_bearer_word_are_filled_too() {
        let out = fill_subscribe(&["AUTH <secret>".to_string()], &cands()).unwrap();
        assert_eq!(out[0].reveal(), "AUTH abc123def456");
        let v = fill_subscribe(&[r#"{"authorization":"Bearer <secret>"}"#.to_string()], &cands()).unwrap();
        assert_eq!(v[0].reveal(), r#"{"authorization":"Bearer abc123def456"}"#);
    }

    #[test]
    fn a_placeholder_without_a_value_is_refused_by_field() {
        let rec = vec![r#"{"op":"auth","data":{"token":"<secret>"}}"#.to_string()];
        let e = fill_subscribe(&rec, &[]).unwrap_err();
        assert!(e.contains("data.token") && e.contains("message 1"), "{e}");
        let two = vec![Cand::new("bearer", "aaaa"), Cand::new("header:x-api-key", "bbbb")];
        let e = fill_subscribe(&[r#"{"session":"<secret>"}"#.to_string()], &two).unwrap_err();
        assert!(e.contains("session"), "{e}");
        assert!(!e.contains("aaaa") && !e.contains("bbbb"));
        // a field named like a scheme is told apart
        let v = fill_subscribe(&[r#"{"x-api-key":"<secret>","token":"<secret>"}"#.to_string()], &two).unwrap();
        assert_eq!(v[0].reveal(), r#"{"x-api-key":"bbbb","token":"aaaa"}"#);
        // a cookie is not put in a message
        let e = fill_subscribe(&[r#"{"t":"<secret>"}"#.to_string()], &[Cand::new("cookie", "sid=1")]).unwrap_err();
        assert!(e.contains("t"), "{e}");
    }

    #[test]
    fn a_cut_or_unkept_message_is_refused() {
        let e = fill_subscribe(&["{\"op\":\"subscribe\",\"chan…".to_string()], &[]).unwrap_err();
        assert!(e.contains("cut"), "{e}");
        let e = fill_subscribe(&[r#"{"room":"<opaque>"}"#.to_string()], &cands()).unwrap_err();
        assert!(e.contains("room"), "{e}");
    }

    fn live(msgs: &[(bool, &str)]) -> Channel {
        let m: Vec<Msg> = msgs
            .iter()
            .map(|(send, t)| Msg { send: *send, time: None, opcode: 1, text: t.to_string(), event: None })
            .collect();
        channel::observe(Kind::Ws, "wss://x.test", "/ws", &m, Some(10.0))
    }

    #[test]
    fn a_text_ping_gets_the_recorded_pong() {
        let c = live(&[
            (false, "ping"),
            (true, "pong"),
            (false, "ping"),
            (true, "pong"),
            (false, "ping"),
            (true, "pong"),
            (false, r#"{"type":"tick"}"#),
        ]);
        assert_eq!(pong_for(&c, "ping").as_deref(), Some("pong"));
        assert_eq!(pong_for(&c, "pong"), None);
        assert_eq!(pong_for(&c, r#"{"type":"tick"}"#), None);
    }

    #[test]
    fn a_json_ping_gets_a_json_pong() {
        let c = live(&[
            (false, r#"{"type":"ping"}"#),
            (true, r#"{"type":"pong"}"#),
            (false, r#"{"type":"tick","n":1}"#),
            (false, r#"{"type":"tick","n":2}"#),
            (false, r#"{"type":"tick","n":3}"#),
        ]);
        let p = pong_for(&c, r#"{"type":"ping","t":5}"#).expect("a pong");
        assert_eq!(serde_json::from_str::<Value>(&p).unwrap(), json!({"type":"pong"}));
        assert_eq!(pong_for(&c, r#"{"type":"tick","n":4}"#), None);
        // a channel without heartbeats answers nothing
        let none = live(&[(false, r#"{"type":"tick"}"#), (false, r#"{"type":"tock"}"#)]);
        assert_eq!(pong_for(&none, "ping"), None);
    }

    #[test]
    fn frames_become_messages_of_a_type() {
        let known = HashSet::new();
        let frames = [
            Message::text(r#"{"type":"tick","n":1}"#),
            Message::text(r#"{"type":"tick","n":2}"#),
            Message::text(r#"{"type":"book","bids":[]}"#),
            Message::binary(vec![1u8, 2, 3, 4]),
            Message::Ping(vec![0u8; 2].into()),
            Message::Close(None),
        ];
        let mut bytes = 0;
        let msgs: Vec<Msg> = frames
            .iter()
            .filter_map(|f| incoming(f, &known))
            .map(|(m, n)| {
                bytes += n;
                m
            })
            .collect();
        assert_eq!(msgs.len(), 5);
        assert_eq!(bytes, 21 + 21 + 25 + 4 + 2);
        assert!(msgs[3].text.is_empty() && msgs[3].opcode == 2);
        let c = channel::observe(Kind::Ws, "wss://x.test", "/ws", &msgs, Some(2.0));
        assert_eq!(c.type_named("tick").map(|t| t.got), Some(2));
        assert_eq!(c.type_named("book").map(|t| t.got), Some(1));
        assert_eq!(c.type_named("binary").map(|t| t.got), Some(1));
        assert!(c.beat.is_some());
    }

    #[test]
    fn a_known_secret_in_a_frame_is_hidden() {
        let mut known = HashSet::new();
        known.insert(exchange::fingerprint("abc123def456"));
        let (m, _) = incoming(&Message::text(r#"{"type":"hello","who":"abc123def456"}"#), &known).unwrap();
        assert!(!m.text.contains("abc123def456"), "{}", m.text);
    }

    /// A local WebSocket server (plain ws://, 127.0.0.1) that checks the subscribe message and the headers, then pushes
    /// three messages and a ping and goes quiet: the follow must stop at its deadline, not wait for the server.
    #[tokio::test]
    async fn a_local_server_is_followed_until_the_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(sock).await.unwrap();
            if let Some(Ok(Message::Text(t))) = ws.next().await {
                let _ = tx.send(t.as_str().to_string());
            }
            for m in [r#"{"type":"hello"}"#, r#"{"type":"tick","n":1}"#, r#"{"type":"tick","n":2}"#] {
                ws.send(Message::text(m)).await.unwrap();
            }
            ws.send(Message::Ping(Vec::<u8>::new().into())).await.unwrap();
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        let mut rec = live(&[
            (true, r#"{"op":"sub","token":"<secret>"}"#),
            (false, r#"{"type":"hello"}"#),
            (false, r#"{"type":"tick","n":1}"#),
            (false, r#"{"type":"tick","n":2}"#),
        ]);
        rec.subscribe = vec![r#"{"op":"sub","token":"<secret>"}"#.to_string()];
        let api = Api {
            endpoints: Vec::new(),
            auth: Vec::new(),
            hosts: Vec::new(),
            total: 0,
            dropped: Default::default(),
            channels: Vec::new(),
            links: Vec::new(),
            links_ignored: Default::default(),
        };
        let req = call::Request {
            method: "GET".to_string(),
            url: format!("ws://{addr}/ws"),
            query: Vec::new(),
            headers: Vec::new(),
            body: None,
        };
        let started = Instant::now();
        let vals: Vec<(usize, String)> = Vec::new();
        let cands = [Cand::new("bearer", "tok-1234567890")];
        // without a value for the placeholder nothing is opened
        let refused = watch(&api, &rec, &req, &vals, &HashSet::new(), Duration::from_secs(1), None).await;
        assert!(refused.is_err());
        assert!(fill_subscribe(&rec.subscribe, &cands).is_ok());
        assert!(started.elapsed() < Duration::from_secs(1));
        // the same channel with no placeholder: follows
        rec.subscribe = vec![r#"{"op":"sub"}"#.to_string()];
        let (report, _code) =
            watch(&api, &rec, &req, &vals, &HashSet::new(), Duration::from_secs(1), None).await.unwrap();
        assert_eq!(rx.await.unwrap(), r#"{"op":"sub"}"#);
        assert!(report.contains("tick x2"), "{report}");
        assert!(started.elapsed() < Duration::from_secs(8));
    }

    /// A local WebSocket server pushing prices: with --select and --when the report holds the picked values only and
    /// stops at the first alert.
    #[tokio::test]
    async fn a_local_server_is_followed_with_select_and_when() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(sock).await.unwrap();
            let _ = ws.next().await;
            for m in [
                r#"{"type":"hello"}"#,
                r#"{"type":"tick","price":10,"volume":1}"#,
                r#"{"type":"tick","price":10,"volume":2}"#,
                r#"{"type":"tick","price":7,"volume":3}"#,
            ] {
                ws.send(Message::text(m)).await.unwrap();
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        let mut rec = live(&[(false, r#"{"type":"tick","price":1}"#)]);
        rec.subscribe = vec![r#"{"op":"sub"}"#.to_string()];
        let api = Api {
            endpoints: Vec::new(),
            auth: Vec::new(),
            hosts: Vec::new(),
            total: 0,
            dropped: Default::default(),
            channels: Vec::new(),
        };
        let req = call::Request {
            method: "GET".to_string(),
            url: format!("ws://{addr}/ws"),
            query: Vec::new(),
            headers: Vec::new(),
            body: None,
        };
        let sel = crate::api_watch::Sel::build(&["$.price".to_string()], Some("$.price < prev"), None, None)
            .unwrap()
            .unwrap();
        let vals: Vec<(usize, String)> = Vec::new();
        let (report, code) =
            watch(&api, &rec, &req, &vals, &HashSet::new(), Duration::from_secs(1), Some(&sel)).await.unwrap();
        assert_eq!(code, crate::api_watch::EXIT_ALERT, "{report}");
        assert_eq!(report, "$.price\t10\n~ $.price: 10 -> 7\nalert: $.price=7 (was 10) | when $.price < prev\n");
    }
}
