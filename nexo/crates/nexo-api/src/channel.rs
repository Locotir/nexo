//! Live channels: WebSocket connections and Server-Sent Events streams a site keeps open and pushes data through.
//! A HAR file holds their frames (`_webSocketMessages` in Chrome and Playwright) or their stream body
//! (`text/event-stream`); this module redacts them like any other body, sorts the messages into types (by a
//! discriminator field such as `type` or `event`, by the event name of a stream, else by the top-level shape), counts
//! each type with its direction and rate, tells heartbeats apart from data, and keeps the first client messages as
//! the subscribe example to replay. Pure: no network and no file. Only names, counts and shapes are kept.

use crate::auth::{self, Auth};
use crate::exchange::{fingerprint, opaque, scrub_json, secret_name, Exchange, Fp, SECRET};
use crate::shape::Shape;
use crate::template;
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Object fields that may tell the kind of a message, in the order they are tried.
const FIELDS: [&str; 5] = ["type", "event", "op", "channel", "action"];
/// Values of a discriminator (or event names) that are heartbeats, lower case.
const BEAT_WORDS: [&str; 7] = ["ping", "pong", "heartbeat", "heart_beat", "keepalive", "keep-alive", "hb"];
/// A text frame this short and repeated is a heartbeat.
const TINY: usize = 8;
/// How many message types a channel keeps; the rest are counted as `(other)`.
const MAX_TYPES: usize = 30;
/// How many distinct values a discriminator may have.
const MAX_VALUES: usize = 20;
/// How many frames of one connection are read.
pub const MAX_FRAMES: usize = 5000;
/// How many bytes of one frame are read.
const MAX_FRAME: usize = 65536;
/// How many characters of a subscribe example are kept.
const MAX_EXAMPLE: usize = 300;
/// A channel seen for less than this many seconds gets no rate.
const MIN_SPAN: f64 = 5.0;

/// What kind of channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ws,
    Sse,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Ws => "ws",
            Kind::Sse => "sse",
        }
    }

    pub fn of(name: &str) -> Option<Kind> {
        match name {
            "ws" => Some(Kind::Ws),
            "sse" => Some(Kind::Sse),
            _ => None,
        }
    }
}

/// One message of a channel, with its text already redacted.
#[derive(Debug, Clone, PartialEq)]
pub struct Msg {
    /// Sent by the client.
    pub send: bool,
    /// Seconds, on the clock of the capture, when it has one.
    pub time: Option<f64>,
    /// The WebSocket opcode: 1 text, 2 binary (its text is left empty), 9 ping, 10 pong. A stream comment line is 9.
    pub opcode: u8,
    pub text: String,
    /// The event name of a stream message (`message` when the stream gave none).
    pub event: Option<String>,
}

/// The messages one connection carried. `index` is the handshake in the list of exchanges.
#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    pub kind: Kind,
    pub index: usize,
    /// How many seconds the connection was observed, when the capture says.
    pub span: Option<f64>,
    pub msgs: Vec<Msg>,
}

/// One kind of message of a channel.
#[derive(Debug, Clone)]
pub struct MsgType {
    pub name: String,
    pub sent: usize,
    pub got: usize,
    /// Everything seen in the JSON messages of this type, merged; `seen == 0` when none was JSON.
    pub shape: Shape,
    /// The frames were binary.
    pub binary: bool,
}

impl MsgType {
    pub fn total(&self) -> usize {
        self.sent + self.got
    }

    /// `->` sent by the client, `<-` sent by the server, `<->` both.
    pub fn arrow(&self) -> &'static str {
        match (self.sent > 0, self.got > 0) {
            (true, true) => "<->",
            (true, false) => "->",
            _ => "<-",
        }
    }
}

/// The heartbeats of a channel: ping and pong frames, tiny repeated frames, stream comments, `{"type":"ping"}`.
#[derive(Debug, Clone, PartialEq)]
pub struct Beat {
    /// What they look like: `ping/pong`, `"2"`, `comment`.
    pub name: String,
    pub sent: usize,
    pub got: usize,
}

/// One live channel.
#[derive(Debug, Clone)]
pub struct Channel {
    /// A short name (`ws_socket`, `sse_events`).
    pub id: String,
    pub kind: Kind,
    /// `wss://host` or `https://host`.
    pub origin: String,
    /// `/socket/{id}`.
    pub path: String,
    /// How many connections the capture held.
    pub connections: usize,
    /// Indexes into [`crate::spec::Api::auth`] the handshake uses.
    pub auth: Vec<usize>,
    /// The field that tells the kind of a message, when most messages have one.
    pub field: Option<String>,
    /// Messages that are not heartbeats.
    pub messages: usize,
    /// Seconds the capture saw the channel open, when it can tell.
    pub span: Option<f64>,
    /// Busiest first.
    pub types: Vec<MsgType>,
    pub beat: Option<Beat>,
    /// The first client messages after the open, redacted: what to send to start the data.
    pub subscribe: Vec<String>,
}

impl Channel {
    /// Messages per minute for `n` messages, when the channel was seen long enough.
    pub fn per_min(&self, n: usize) -> Option<f64> {
        match self.span {
            Some(s) if s >= MIN_SPAN => Some(n as f64 / s * 60.0),
            _ => None,
        }
    }

    /// The type with this name.
    pub fn type_named(&self, name: &str) -> Option<&MsgType> {
        self.types.iter().find(|t| t.name == name)
    }
}

/// `12/min`, `3.5/min`, `0.4/min`.
pub fn rate_text(r: f64) -> String {
    if r >= 10.0 {
        format!("{:.0}/min", r)
    } else {
        format!("{:.1}/min", r)
    }
}

// ---------------------------------------------------------------- redaction

/// Replaces every secret in one frame: a JSON field with a secret name or a string equal to one (see
/// [`crate::exchange::scrub_json`]), a string whose fingerprint is in `known` (a secret the capture's headers, cookies
/// or bodies held), and in text that is not JSON every word that is a known secret or looks like a token, and the
/// value of a `name=value` word with a secret name.
pub fn redact_text(text: &str, known: &HashSet<Fp>) -> String {
    let mut end = text.len().min(MAX_FRAME);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let text = &text[..end];
    let t = text.trim();
    if t.starts_with('{') || t.starts_with('[') {
        if let Ok(mut v) = serde_json::from_str::<Value>(t) {
            scrub_json(&mut v);
            scrub_known(&mut v, known);
            return v.to_string();
        }
    }
    let mut out = String::with_capacity(text.len());
    for (i, word) in text.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if word.len() >= 8 && (known.contains(&fingerprint(word)) || opaque(word)) {
            out.push_str(SECRET);
        } else if let Some((k, v)) = word.split_once('=') {
            if secret_name(k) && !v.is_empty() {
                out.push_str(k);
                out.push('=');
                out.push_str(SECRET);
            } else {
                out.push_str(word);
            }
        } else {
            out.push_str(word);
        }
    }
    out
}

fn scrub_known(v: &mut Value, known: &HashSet<Fp>) {
    match v {
        Value::String(s) => {
            if s.len() >= 8 && known.contains(&fingerprint(s)) {
                *s = SECRET.to_string();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|x| scrub_known(x, known)),
        Value::Object(map) => map.iter_mut().for_each(|(_, x)| scrub_known(x, known)),
        _ => {}
    }
}

/// Strings that look like tokens become `<opaque>`, so an example never shows one.
fn hide_opaque(v: &mut Value) {
    match v {
        Value::String(s) => {
            if opaque(s) {
                *s = "<opaque>".to_string();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(hide_opaque),
        Value::Object(map) => map.iter_mut().for_each(|(_, x)| hide_opaque(x)),
        _ => {}
    }
}

// ---------------------------------------------------------------- server-sent events

/// Reads a stream a piece at a time: [`SseParser::feed`] returns the messages the piece completed.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: String,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn new() -> SseParser {
        SseParser::default()
    }

    fn line(&mut self, line: &str, out: &mut Vec<Msg>) {
        if line.is_empty() {
            if !self.data.is_empty() {
                let name = self.event.take().unwrap_or_else(|| "message".to_string());
                let name = if name.is_empty() { "message".to_string() } else { name };
                let text = std::mem::take(&mut self.data).join("\n");
                out.push(Msg { send: false, time: None, opcode: 1, text, event: Some(name) });
            } else {
                self.event = None;
            }
            return;
        }
        if let Some(c) = line.strip_prefix(':') {
            out.push(Msg {
                send: false,
                time: None,
                opcode: 9,
                text: c.trim().to_string(),
                event: Some("comment".to_string()),
            });
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.trim().to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
    }

    /// Adds a piece of the stream and returns the messages it completed.
    pub fn feed(&mut self, chunk: &str) -> Vec<Msg> {
        self.buf.push_str(chunk);
        let mut out = Vec::new();
        while let Some(i) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=i).collect();
            let line = line.trim_end_matches(['\n', '\r']).to_string();
            self.line(&line, &mut out);
            if out.len() >= MAX_FRAMES {
                self.buf.clear();
                break;
            }
        }
        out
    }

    /// The message a stream that ended without a blank line still holds.
    pub fn finish(&mut self) -> Vec<Msg> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            let rest = rest.trim_end_matches(['\n', '\r']).to_string();
            self.line(&rest, &mut out);
        }
        self.line("", &mut out);
        out
    }
}

/// The messages of a whole stream body.
pub fn parse_sse(text: &str) -> Vec<Msg> {
    let mut p = SseParser::new();
    let mut out = p.feed(text);
    out.extend(p.finish());
    out.truncate(MAX_FRAMES);
    out
}

// ---------------------------------------------------------------- analysis

/// One message with what the analysis worked out.
struct Item<'a> {
    conn: usize,
    m: &'a Msg,
    json: Option<Value>,
    beat: Option<String>,
}

fn is_tiny(m: &Msg) -> bool {
    if m.opcode != 1 || m.event.as_deref().is_some_and(|e| e != "message") {
        return false;
    }
    let t = m.text.trim();
    let n = t.chars().count();
    n > 0 && n <= TINY && (!(t.starts_with('{') || t.starts_with('[')) || t == "{}" || t == "[]")
}

fn shape_name(v: &Value) -> String {
    match v {
        Value::Object(m) if m.is_empty() => "{}".to_string(),
        Value::Object(m) => {
            let mut keys: Vec<&str> = m.keys().map(String::as_str).collect();
            keys.sort_unstable();
            let more = if keys.len() > 4 { ",…" } else { "" };
            format!("{{{}{}}}", keys.iter().take(4).copied().collect::<Vec<_>>().join(","), more)
        }
        Value::Array(_) => "[]".to_string(),
        Value::String(_) => "text".to_string(),
        Value::Number(_) => "number".to_string(),
        Value::Bool(_) => "boolean".to_string(),
        Value::Null => "null".to_string(),
    }
}

/// The value of a discriminator field when it can be one: a short string that is not a token.
fn tag<'v>(v: &'v Value, field: &str) -> Option<&'v str> {
    let s = v.get(field)?.as_str()?;
    if s.is_empty() || s.chars().count() > 40 || opaque(s) {
        None
    } else {
        Some(s)
    }
}

/// The field most of the objects carry as a short string with a small set of values.
fn discriminator(objs: &[&Value]) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for field in FIELDS {
        let mut values: HashSet<&str> = HashSet::new();
        let mut present = 0;
        for o in objs {
            if let Some(s) = tag(o, field) {
                present += 1;
                values.insert(s);
            }
        }
        if present * 2 > objs.len() && values.len() <= MAX_VALUES && best.map(|(n, _)| present > n).unwrap_or(true) {
            best = Some((present, field));
        }
    }
    best.map(|(_, f)| f.to_string())
}

struct Analysis {
    field: Option<String>,
    messages: usize,
    types: Vec<MsgType>,
    beat: Option<Beat>,
    subscribe: Vec<String>,
}

/// Cuts at a character.
fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A client message as an example: JSON compact with token-like strings hidden, else the text.
fn example_text(m: &Msg) -> String {
    let t = m.text.trim();
    if t.starts_with('{') || t.starts_with('[') {
        if let Ok(mut v) = serde_json::from_str::<Value>(t) {
            hide_opaque(&mut v);
            return cut(&v.to_string(), MAX_EXAMPLE);
        }
    }
    cut(t, MAX_EXAMPLE)
}

fn analyze(conns: &[&[Msg]]) -> Analysis {
    // parse, and find the tiny frames that repeat
    let mut items: Vec<Item> = Vec::new();
    let mut tiny: HashMap<(bool, String), usize> = HashMap::new();
    for (c, msgs) in conns.iter().enumerate() {
        for m in msgs.iter() {
            let t = m.text.trim();
            let json = if m.opcode == 1 && (t.starts_with('{') || t.starts_with('[')) {
                serde_json::from_str::<Value>(t).ok()
            } else {
                None
            };
            if is_tiny(m) {
                *tiny.entry((m.send, t.to_string())).or_insert(0) += 1;
            }
            items.push(Item { conn: c, m, json, beat: None });
        }
    }
    // heartbeats that need no discriminator
    for it in items.iter_mut() {
        let m = it.m;
        it.beat = match m.opcode {
            9 if m.event.is_some() => Some("comment".to_string()),
            9 => Some("ping".to_string()),
            10 => Some("pong".to_string()),
            _ => {
                let t = m.text.trim();
                if is_tiny(m) && tiny.get(&(m.send, t.to_string())).copied().unwrap_or(0) >= 3 {
                    Some(format!("\"{t}\""))
                } else if m.event.as_deref().is_some_and(|e| BEAT_WORDS.contains(&e.to_ascii_lowercase().as_str())) {
                    m.event.clone()
                } else {
                    None
                }
            }
        };
    }
    // the discriminator, over the JSON objects that are not heartbeats yet (not for streams: the event name is it)
    let objs: Vec<&Value> = items
        .iter()
        .filter(|i| i.beat.is_none() && i.m.event.is_none())
        .filter_map(|i| i.json.as_ref().filter(|v| v.is_object()))
        .collect();
    let field = discriminator(&objs);
    if let Some(f) = &field {
        for it in items.iter_mut().filter(|i| i.beat.is_none() && i.m.event.is_none()) {
            if let Some(s) = it.json.as_ref().and_then(|v| tag(v, f)) {
                if BEAT_WORDS.contains(&s.to_ascii_lowercase().as_str()) {
                    it.beat = Some(s.to_string());
                }
            }
        }
    }

    // heartbeats
    let mut beats: IndexMap<String, (usize, usize)> = IndexMap::new();
    for it in items.iter().filter(|i| i.beat.is_some()) {
        let e = beats.entry(it.beat.clone().unwrap_or_default()).or_insert((0, 0));
        if it.m.send {
            e.0 += 1;
        } else {
            e.1 += 1;
        }
    }
    let beat = if beats.is_empty() {
        None
    } else {
        let mut list: Vec<(&String, &(usize, usize))> = beats.iter().collect();
        list.sort_by(|a, b| (b.1 .0 + b.1 .1).cmp(&(a.1 .0 + a.1 .1)));
        let name = list.iter().take(2).map(|(n, _)| n.as_str()).collect::<Vec<_>>().join("/");
        Some(Beat { name, sent: list.iter().map(|(_, c)| c.0).sum(), got: list.iter().map(|(_, c)| c.1).sum() })
    };

    // types
    let mut types: IndexMap<String, MsgType> = IndexMap::new();
    let mut messages = 0;
    for it in items.iter().filter(|i| i.beat.is_none()) {
        messages += 1;
        let m = it.m;
        let mut name = if let Some(e) = &m.event {
            e.clone()
        } else if m.opcode == 2 {
            "binary".to_string()
        } else {
            match (&it.json, &field) {
                (Some(v), Some(f)) => tag(v, f).map(str::to_string).unwrap_or_else(|| shape_name(v)),
                (Some(v), None) => shape_name(v),
                _ => "text".to_string(),
            }
        };
        if !types.contains_key(&name) && types.len() >= MAX_TYPES {
            name = "(other)".to_string();
        }
        let t = types.entry(name.clone()).or_insert_with(|| MsgType {
            name,
            sent: 0,
            got: 0,
            shape: Shape::default(),
            binary: m.opcode == 2,
        });
        if m.send {
            t.sent += 1;
        } else {
            t.got += 1;
        }
        if let Some(v) = &it.json {
            t.shape.add(v);
        }
    }
    let mut types: Vec<MsgType> = types.into_iter().map(|(_, t)| t).collect();
    types.sort_by_key(|t| std::cmp::Reverse(t.total()));

    // what the client said right after the open
    let mut subscribe: Vec<String> = Vec::new();
    for c in 0..conns.len() {
        for it in items.iter().filter(|i| i.conn == c && i.beat.is_none()).take(5) {
            if it.m.send && subscribe.len() < 3 {
                let e = example_text(it.m);
                if !e.is_empty() && !subscribe.contains(&e) {
                    subscribe.push(e);
                }
            }
        }
    }
    Analysis { field, messages, types, beat, subscribe }
}

fn span_of(spans: &[Option<f64>]) -> Option<f64> {
    let known: Vec<f64> = spans.iter().flatten().copied().collect();
    if known.is_empty() {
        None
    } else {
        Some(known.iter().sum())
    }
}

/// A channel from messages seen on one open connection, for a live follow: the id is `live`, no auth.
pub fn observe(kind: Kind, origin: &str, path: &str, msgs: &[Msg], span: Option<f64>) -> Channel {
    let a = analyze(&[msgs]);
    Channel {
        id: "live".to_string(),
        kind,
        origin: origin.to_string(),
        path: path.to_string(),
        connections: 1,
        auth: Vec::new(),
        field: a.field,
        messages: a.messages,
        span,
        types: a.types,
        beat: a.beat,
        subscribe: a.subscribe,
    }
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

/// Groups the connections by templated path, analyses the messages of each group and names the channels.
/// `all` are the exchanges the connections' `index` points into.
pub fn build(all: &[Exchange], live: &[Live], auths: &[Auth]) -> Vec<Channel> {
    let mut out: Vec<Channel> = Vec::new();
    let mut taken: HashMap<String, usize> = HashMap::new();
    for kind in [Kind::Ws, Kind::Sse] {
        let lives: Vec<&Live> = live.iter().filter(|l| l.kind == kind && l.index < all.len()).collect();
        let xs: Vec<&Exchange> = lives.iter().map(|l| &all[l.index]).collect();
        for g in template::group(&xs) {
            let members: Vec<&Live> = g.members.iter().filter_map(|(i, _)| lives.get(*i).copied()).collect();
            let conns: Vec<&[Msg]> = members.iter().map(|l| l.msgs.as_slice()).collect();
            let a = analyze(&conns);
            let mut used: Vec<usize> = Vec::new();
            for l in &members {
                for i in auth::of_call(&all[l.index], auths) {
                    if !used.contains(&i) {
                        used.push(i);
                    }
                }
            }
            used.sort_unstable();
            let words: Vec<String> = g.path.split('/').map(slug).filter(|w| !w.is_empty()).collect();
            let base =
                format!("{}_{}", kind.name(), if words.is_empty() { "root".to_string() } else { words.join("_") });
            let n = taken.entry(base.clone()).or_insert(0);
            *n += 1;
            let id = if *n == 1 { base } else { format!("{base}_{n}") };
            let spans: Vec<Option<f64>> = members.iter().map(|l| l.span).collect();
            out.push(Channel {
                id,
                kind,
                origin: g.origin.clone(),
                path: g.path.clone(),
                connections: members.len(),
                auth: used,
                field: a.field,
                messages: a.messages,
                span: span_of(&spans),
                types: a.types,
                beat: a.beat,
                subscribe: a.subscribe,
            });
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) const WS_TOKEN: &str = "wsTOKEN1234abcdEFGH5678ijkl";

    fn frame(send: bool, time: f64, opcode: u8, data: &str) -> Value {
        json!({ "type": if send { "send" } else { "receive" }, "time": time, "opcode": opcode, "data": data })
    }

    /// A HAR with a page's REST call, a WebSocket (subscribe, two JSON types, pings, a token in a frame) and an event
    /// stream with two event names.
    pub(crate) fn har() -> String {
        let t0 = 1_700_000_000.0;
        let sub = format!(r#"{{"type":"subscribe","channel":"prices","token":"{WS_TOKEN}"}}"#);
        let mut frames =
            vec![frame(true, t0, 1, &sub), frame(false, t0 + 0.1, 1, r#"{"type":"subscribed","channel":"prices"}"#)];
        for i in 0..6 {
            let at = t0 + 2.0 + i as f64 * 10.0;
            frames.push(frame(false, at, 1, &format!(r#"{{"type":"ticker","symbol":"BTC","price":{}.5}}"#, 100 + i)));
            frames.push(frame(false, at + 1.0, 1, &format!(r#"{{"type":"trade","id":{i},"qty":2}}"#)));
            frames.push(frame(true, at + 2.0, 9, ""));
            frames.push(frame(false, at + 2.1, 10, ""));
        }
        let stream = "event: tick\ndata: {\"n\":1}\n\n:keepalive\n\nevent: tick\ndata: {\"n\":2}\n\nevent: status\ndata: {\"up\":true}\n\n";
        json!({"log": {"version": "1.2", "entries": [
            {
                "_resourceType": "xhr",
                "request": {"method": "GET", "url": "https://app.test/api/items", "headers": []},
                "response": {"status": 200, "headers": [],
                             "content": {"mimeType": "application/json", "text": "{\"items\":[1]}"}}
            },
            {
                "_resourceType": "websocket",
                "request": {"method": "GET", "url": "wss://app.test/socket?access_token=QUERYTOKEN1234567890",
                            "headers": [{"name": "Authorization", "value": format!("Bearer {WS_TOKEN}")}]},
                "response": {"status": 101, "headers": [], "content": {"mimeType": "x-unknown"}},
                "_webSocketMessages": frames
            },
            {
                "_resourceType": "eventsource",
                "time": 60000,
                "request": {"method": "GET", "url": "https://app.test/api/stream", "headers": []},
                "response": {"status": 200, "headers": [{"name": "Content-Type", "value": "text/event-stream"}],
                             "content": {"mimeType": "text/event-stream", "text": stream}}
            }
        ]}})
        .to_string()
    }

    #[test]
    fn a_websocket_is_read_with_its_types_heartbeats_and_subscribe_example() {
        let (all, live) = crate::har::read_all(&har()).unwrap();
        let chans = build(&all, &live, &[]);
        let ws = chans.iter().find(|c| c.kind == Kind::Ws).expect("a ws channel");
        assert_eq!(ws.id, "ws_socket");
        assert_eq!(ws.origin, "wss://app.test");
        assert_eq!(ws.field.as_deref(), Some("type"));
        let names: Vec<&str> = ws.types.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names.len(), 4, "{names:?}");
        for n in ["ticker", "trade", "subscribe", "subscribed"] {
            assert!(names.contains(&n), "{n} in {names:?}");
        }
        let ticker = ws.type_named("ticker").unwrap();
        assert_eq!((ticker.sent, ticker.got), (0, 6));
        assert_eq!(ticker.arrow(), "<-");
        assert!(ticker.shape.fields.contains_key("price"));
        let sub = ws.type_named("subscribe").unwrap();
        assert_eq!((sub.sent, sub.got), (1, 0));
        // pings and pongs are counted and named, never types
        let beat = ws.beat.as_ref().expect("heartbeats");
        assert_eq!((beat.sent, beat.got), (6, 6));
        assert_eq!(beat.name, "ping/pong");
        assert_eq!(ws.messages, 14);
        // the first client message is the subscribe example, redacted
        assert_eq!(ws.subscribe.len(), 1);
        assert!(ws.subscribe[0].contains("\"subscribe\"") && ws.subscribe[0].contains("prices"), "{:?}", ws.subscribe);
        // about 54 seconds between the first and the last frame
        let span = ws.span.expect("a span");
        assert!((span - 54.1).abs() < 0.5, "{span}");
        assert!(ws.per_min(6).unwrap() > 5.0);
    }

    #[test]
    fn a_stream_is_read_by_event_name_and_comments_are_heartbeats() {
        let (all, live) = crate::har::read_all(&har()).unwrap();
        let chans = build(&all, &live, &[]);
        let sse = chans.iter().find(|c| c.kind == Kind::Sse).expect("an sse channel");
        assert_eq!(sse.id, "sse_api_stream");
        let tick = sse.type_named("tick").unwrap();
        assert_eq!(tick.got, 2);
        assert!(tick.shape.fields.contains_key("n"));
        let status = sse.type_named("status").unwrap();
        assert_eq!(status.got, 1);
        assert_eq!(sse.types.len(), 2);
        let beat = sse.beat.as_ref().expect("a comment heartbeat");
        assert_eq!(beat.name, "comment");
        assert_eq!(sse.span, Some(60.0));
        assert_eq!(sse.field, None);
    }

    #[test]
    fn a_token_inside_a_frame_never_survives() {
        let (all, live) = crate::har::read_all(&har()).unwrap();
        let chans = build(&all, &live, &[]);
        let all_text = format!("{:?}", live) + &format!("{:?}", chans);
        assert!(!all_text.contains(WS_TOKEN), "token leaked");
        assert!(!all_text.contains("QUERYTOKEN"), "query token leaked");
        let ws = live.iter().find(|l| l.kind == Kind::Ws).unwrap();
        assert!(ws.msgs[0].text.contains(SECRET), "{}", ws.msgs[0].text);
    }

    #[test]
    fn frames_that_are_not_json_are_redacted_word_by_word() {
        let mut known = HashSet::new();
        known.insert(fingerprint("hunter2hunter2"));
        let out = redact_text("login ann hunter2hunter2 sid=abc12345 note=hi", &known);
        assert_eq!(out, "login ann <secret> sid=<secret> note=hi");
        let out = redact_text(r#"{"auth":"shortone","name":"x","echo":"hunter2hunter2"}"#, &known);
        assert!(!out.contains("shortone") && !out.contains("hunter2"), "{out}");
    }

    #[test]
    fn tiny_repeated_frames_are_heartbeats_and_the_shape_names_types_without_a_discriminator() {
        let m = |send: bool, text: &str| Msg { send, time: None, opcode: 1, text: text.to_string(), event: None };
        let msgs = vec![
            m(true, "2"),
            m(false, "3"),
            m(true, "2"),
            m(false, "3"),
            m(true, "2"),
            m(false, "3"),
            m(false, r#"{"bid":1,"ask":2}"#),
            m(false, r#"{"bid":2,"ask":3}"#),
            m(false, r#"[1,2]"#),
        ];
        let c = observe(Kind::Ws, "wss://x.test", "/s", &msgs, None);
        assert_eq!(c.field, None);
        let beat = c.beat.as_ref().unwrap();
        assert_eq!((beat.sent, beat.got), (3, 3));
        let names: Vec<&str> = c.types.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["{ask,bid}", "[]"]);
        assert_eq!(c.types[0].got, 2);
    }

    #[test]
    fn the_parser_joins_pieces_and_ignores_unknown_fields() {
        let mut p = SseParser::new();
        let mut got = p.feed("id: 4\nevent: a\nda");
        assert!(got.is_empty());
        got.extend(p.feed("ta: one\ndata: two\n\r\n\ndata: lone\n"));
        got.extend(p.finish());
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].event.as_deref(), Some("a"));
        assert_eq!(got[0].text, "one\ntwo");
        assert_eq!(got[1].event.as_deref(), Some("message"));
        assert_eq!(got[1].text, "lone");
    }
}
