//! Has the live answer of an endpoint kept the shape the map recorded? [`compare`] sets a [`Shape`] recorded from a
//! capture against the shape of what the endpoint answers now and lists what is gone, what is new and what changed
//! its type; [`check`] adds the status and the content type and gives one [`Report`] per endpoint. Pure: it reads no
//! network and no file, and it names fields and types only, never a value, so nothing it says can hold a secret.
//!
//! A new field is only information (an API grows); a field the map saw in every answer that is gone, a type that
//! changed, a value that can be null now, a media type that changed are drift; an answer with a status the map never
//! saw (a 401 where it saw 200, a 404, a redirect) is broken.

use crate::channel::Channel;
use crate::shape::Shape;
use crate::spec::Endpoint;
use serde_json::Value;

/// How deep [`compare`] goes.
const MAX_DEPTH: usize = 32;

/// What differs at one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// The map saw this field in every object; the live answer has none (`was` is its type).
    Missing { was: String },
    /// A field the map never saw (`now` is its type).
    Added { now: String },
    /// A value of a type the map never saw there.
    Retyped { was: String, now: String },
    /// The map saw no null here; the live answer has one.
    Nullable,
}

/// One difference, at a path like `$.items[].price` (`[]` is an array item, `.*` any value of a dictionary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub path: String,
    pub kind: Kind,
}

impl Finding {
    /// Drift, as opposed to information: everything but a new field.
    pub fn is_drift(&self) -> bool {
        !matches!(self.kind, Kind::Added { .. })
    }

    /// `drift` or `info`.
    pub fn severity(&self) -> &'static str {
        if self.is_drift() {
            "drift"
        } else {
            "info"
        }
    }

    /// A short name of the kind: `missing`, `added`, `retyped`, `nullable`.
    pub fn name(&self) -> &'static str {
        match self.kind {
            Kind::Missing { .. } => "missing",
            Kind::Added { .. } => "added",
            Kind::Retyped { .. } => "retyped",
            Kind::Nullable => "nullable",
        }
    }

    /// What it says, such as `missing field $.user.email (was string)`.
    pub fn text(&self) -> String {
        match &self.kind {
            Kind::Missing { was } => format!("missing field {} (was {was})", self.path),
            Kind::Added { now } => format!("new field {} ({now})", self.path),
            Kind::Retyped { was, now } => format!("type changed at {}: {was} -> {now}", self.path),
            Kind::Nullable => format!("{} can be null now", self.path),
        }
    }
}

/// The JSON types a place holds, without null; whole numbers and fractions are both `number`.
fn classes(s: &Shape) -> Vec<&'static str> {
    let mut v = Vec::new();
    if s.bools > 0 {
        v.push("boolean");
    }
    if s.ints + s.nums > 0 {
        v.push("number");
    }
    if s.strs > 0 {
        v.push("string");
    }
    if s.arrays > 0 {
        v.push("array");
    }
    if s.objects > 0 {
        v.push("object");
    }
    v
}

/// The types of a place as text: `string`, `number|null`, `unknown` when nothing was seen.
pub fn type_text(s: &Shape) -> String {
    let mut v = classes(s);
    if s.nulls > 0 {
        v.push("null");
    }
    if v.is_empty() {
        "unknown".to_string()
    } else {
        v.join("|")
    }
}

fn child(path: &str, key: &str) -> String {
    let plain = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if plain {
        format!("{path}.{key}")
    } else {
        format!("{path}[{}]", Value::String(key.to_string()))
    }
}

/// Compares the shape the map recorded with the shape of the live answer (for example `Shape::of(&answer)`). A place
/// the live answer has no value at (an empty list, no object) is not compared. A field the map saw in only some of
/// its objects may be absent. The findings come in the order the fields were recorded.
pub fn compare(recorded: &Shape, live: &Shape) -> Vec<Finding> {
    let mut out = Vec::new();
    walk("$", recorded, live, 0, &mut out);
    out
}

fn walk(path: &str, rec: &Shape, live: &Shape, depth: usize, out: &mut Vec<Finding>) {
    if rec.seen == 0 || live.seen == 0 || depth > MAX_DEPTH {
        return;
    }
    let rc = classes(rec);
    let lc = classes(live);
    if !rc.is_empty() && lc.iter().any(|c| !rc.contains(c)) {
        out.push(Finding { path: path.to_string(), kind: Kind::Retyped { was: type_text(rec), now: type_text(live) } });
    } else if !rc.is_empty() && rec.nulls == 0 && live.nulls > 0 {
        out.push(Finding { path: path.to_string(), kind: Kind::Nullable });
    }
    if rec.arrays > 0 && live.arrays > 0 {
        if let (Some(ri), Some(li)) = (&rec.items, &live.items) {
            walk(&format!("{path}[]"), ri, li, depth + 1, out);
        }
    }
    if rec.objects > 0 && live.objects > 0 {
        if rec.dict.is_some() || live.dict.is_some() {
            if let (Some(rd), Some(ld)) = (&rec.dict, &live.dict) {
                walk(&format!("{path}.*"), rd, ld, depth + 1, out);
            }
            return;
        }
        for (key, rf) in &rec.fields {
            match live.fields.get(key) {
                Some(lf) => walk(&child(path, key), rf, lf, depth + 1, out),
                None if rec.required(key) => {
                    out.push(Finding { path: child(path, key), kind: Kind::Missing { was: type_text(rf) } })
                }
                None => {}
            }
        }
        for (key, lf) in &live.fields {
            if !rec.fields.contains_key(key) {
                out.push(Finding { path: child(path, key), kind: Kind::Added { now: type_text(lf) } });
            }
        }
    }
}

/// How one endpoint stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Drift,
    Broken,
}

impl Verdict {
    pub fn word(self) -> &'static str {
        match self {
            Verdict::Ok => "ok",
            Verdict::Drift => "drift",
            Verdict::Broken => "broken",
        }
    }
}

/// What [`check`] found for one endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub verdict: Verdict,
    /// The statuses the map recorded, ascending.
    pub recorded_status: Vec<u16>,
    /// Why it is broken: a status the map never saw, an auth failure.
    pub broken: Vec<String>,
    /// Drift that is not about a field: the media type changed, the answer is no longer JSON.
    pub changes: Vec<String>,
    /// The field-level differences, drift first.
    pub findings: Vec<Finding>,
}

impl Report {
    /// How many things drifted.
    pub fn drift_count(&self) -> usize {
        self.changes.len() + self.findings.iter().filter(|f| f.is_drift()).count()
    }

    /// How many differences are only information.
    pub fn info_count(&self) -> usize {
        self.findings.iter().filter(|f| !f.is_drift()).count()
    }
}

/// The media type of a `Content-Type`, lower case, without parameters.
fn media(ct: &str) -> String {
    ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase()
}

fn same_media(a: &str, b: &str) -> bool {
    a == b || (a.contains("json") && b.contains("json"))
}

/// The statuses as text: `200`, `200/404`.
pub fn status_text(list: &[u16]) -> String {
    if list.is_empty() {
        "none".to_string()
    } else {
        list.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("/")
    }
}

/// Sets the live answer of an endpoint (its status, its `Content-Type`, and its body when that parsed as JSON) against
/// what the map recorded for it.
pub fn check(ep: &Endpoint, status: u16, content_type: &str, body: Option<&Value>) -> Report {
    let mut recorded_status: Vec<u16> = ep.responses.keys().copied().collect();
    recorded_status.sort_unstable();
    let mut report =
        Report { verdict: Verdict::Ok, recorded_status, broken: Vec::new(), changes: Vec::new(), findings: Vec::new() };
    let payload = ep.responses.get(&status);
    let seen = status_text(&report.recorded_status);
    let recorded_ok = report.recorded_status.iter().any(|s| (200..300).contains(s));
    if !report.recorded_status.is_empty() && payload.is_none() {
        let msg = if matches!(status, 401 | 403) && recorded_ok {
            format!("auth failed: status {status}, the map recorded {seen}")
        } else {
            format!("status {status}, the map recorded {seen}")
        };
        report.broken.push(msg);
    } else if report.recorded_status.is_empty() && !(200..300).contains(&status) {
        report.broken.push(if matches!(status, 401 | 403) {
            format!("auth failed: status {status}")
        } else {
            format!("status {status}")
        });
    }
    if !report.broken.is_empty() {
        report.verdict = Verdict::Broken;
        return report;
    }
    if let Some(p) = payload {
        let (want, got) = (media(&p.mime), media(content_type));
        let mut media_changed = false;
        if !want.is_empty() && !got.is_empty() && !same_media(&want, &got) {
            report.changes.push(format!("content type changed: {want} -> {got}"));
            media_changed = true;
        }
        if p.shape.seen > 0 {
            match body {
                Some(v) => report.findings = compare(&p.shape, &Shape::of(v)),
                None if !media_changed => report.changes.push("the answer is no longer JSON".to_string()),
                None => {}
            }
        }
        // drift first, each group in the order found
        report.findings.sort_by_key(|f| !f.is_drift());
    }
    if report.drift_count() > 0 {
        report.verdict = Verdict::Drift;
    }
    report
}

/// One difference between a recorded channel and a live one, or between two maps' channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelFinding {
    /// Drift (a channel, a message type or a field is gone, a type changed), as opposed to information (something new).
    pub drift: bool,
    pub text: String,
}

impl ChannelFinding {
    fn new(drift: bool, text: String) -> ChannelFinding {
        ChannelFinding { drift, text }
    }
}

/// How many field-level findings of one message type are listed.
const MAX_TYPE_FINDINGS: usize = 5;

/// Sets the message types of a channel seen live (or in a newer capture) against the recorded ones: a type that is
/// gone (`gone` is the word: `vanished`, `not seen`) is drift, a new one is information, and for a type both have the
/// JSON shapes are compared like an endpoint's answer. `beat` also reports heartbeats that are gone.
fn channel_findings(rec: &Channel, live: &Channel, gone: &str, beat: bool) -> Vec<ChannelFinding> {
    let mut out = Vec::new();
    for rt in &rec.types {
        match live.type_named(&rt.name) {
            None => out
                .push(ChannelFinding::new(true, format!("message type {} {gone} (recorded x{})", rt.name, rt.total()))),
            Some(lt) => {
                if rt.shape.seen > 0 && lt.shape.seen > 0 {
                    for f in compare(&rt.shape, &lt.shape).into_iter().take(MAX_TYPE_FINDINGS) {
                        out.push(ChannelFinding::new(f.is_drift(), format!("{}: {}", rt.name, f.text())));
                    }
                }
            }
        }
    }
    for lt in &live.types {
        if rec.type_named(&lt.name).is_none() {
            out.push(ChannelFinding::new(false, format!("new message type {} (x{})", lt.name, lt.total())));
        }
    }
    if beat && rec.beat.is_some() && live.beat.is_none() {
        out.push(ChannelFinding::new(true, "heartbeat vanished".to_string()));
    }
    // drift first, each group in the order found
    out.sort_by_key(|f| !f.drift);
    out
}

/// A recorded channel against what the same channel does now (a live follow): the differences, drift first. A type
/// the window did not show is reported as `not seen`, not as gone for good.
pub fn channel(recorded: &Channel, live: &Channel) -> Vec<ChannelFinding> {
    channel_findings(recorded, live, "not seen", false)
}

/// The channels of two maps (or two captures) against each other, matched by kind, host and path like endpoints: a
/// channel that vanished is drift, a new one is information, and for both the message types are compared.
pub fn channels(old: &[Channel], new: &[Channel]) -> Vec<ChannelFinding> {
    let same = |a: &Channel, b: &Channel| a.kind == b.kind && a.origin == b.origin && a.path == b.path;
    let mut out = Vec::new();
    for o in old {
        match new.iter().find(|n| same(o, n)) {
            None => out.push(ChannelFinding::new(true, format!("channel {} vanished", o.id))),
            Some(n) => {
                for f in channel_findings(o, n, "vanished", true) {
                    out.push(ChannelFinding::new(f.drift, format!("{}: {}", o.id, f.text)));
                }
            }
        }
    }
    for n in new {
        if !old.iter().any(|o| same(o, n)) {
            out.push(ChannelFinding::new(false, format!("new channel {}", n.id)));
        }
    }
    out.sort_by_key(|f| !f.drift);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Payload;
    use indexmap::IndexMap;
    use serde_json::json;

    fn rec(samples: &[Value]) -> Shape {
        let mut s = Shape::default();
        for v in samples {
            s.add(v);
        }
        s
    }

    fn cmp(recorded: &[Value], live: Value) -> Vec<Finding> {
        compare(&rec(recorded), &Shape::of(&live))
    }

    fn kinds(f: &[Finding]) -> Vec<String> {
        f.iter().map(|f| format!("{} {}", f.name(), f.path)).collect()
    }

    #[test]
    fn an_equal_shape_has_no_finding() {
        let a = json!({"id": 1, "name": "a", "tags": ["x"], "owner": {"id": 2}});
        let b = json!({"id": 9, "name": "zz", "tags": [], "owner": {"id": 3}});
        assert!(cmp(&[a], b).is_empty());
    }

    #[test]
    fn a_field_in_every_object_that_is_gone_is_drift() {
        let f = cmp(&[json!({"id": 1, "email": "a@b.co"})], json!({"id": 1}));
        assert_eq!(kinds(&f), vec!["missing $.email"]);
        assert!(f[0].is_drift());
        assert_eq!(f[0].text(), "missing field $.email (was string)");
    }

    #[test]
    fn an_optional_field_may_be_absent() {
        let f = cmp(&[json!({"id": 1, "note": "x"}), json!({"id": 2})], json!({"id": 3}));
        assert!(f.is_empty(), "{f:?}");
    }

    #[test]
    fn a_new_field_is_information() {
        let f = cmp(&[json!({"id": 1})], json!({"id": 1, "extra": [1]}));
        assert_eq!(kinds(&f), vec!["added $.extra"]);
        assert!(!f[0].is_drift());
        assert_eq!(f[0].severity(), "info");
        assert_eq!(f[0].text(), "new field $.extra (array)");
    }

    #[test]
    fn a_changed_type_is_drift() {
        let f = cmp(&[json!({"price": "9.50", "u": {"a": 1}})], json!({"price": 9.5, "u": [1]}));
        assert_eq!(kinds(&f), vec!["retyped $.price", "retyped $.u"]);
        assert_eq!(f[0].text(), "type changed at $.price: string -> number");
        assert_eq!(f[1].text(), "type changed at $.u: object -> array");
        // a whole number and a fraction are one type
        assert!(cmp(&[json!({"n": 1})], json!({"n": 1.5})).is_empty());
        // a type the map saw among others is fine
        assert!(cmp(&[json!({"v": 1}), json!({"v": "a"})], json!({"v": "b"})).is_empty());
    }

    #[test]
    fn a_value_that_can_be_null_now_is_drift() {
        let f = cmp(&[json!({"a": "x", "b": null})], json!({"a": null, "b": null}));
        assert_eq!(kinds(&f), vec!["nullable $.a"]);
        assert!(f[0].is_drift());
        assert_eq!(f[0].text(), "$.a can be null now");
        // a changed type is one finding
        let g = cmp(&[json!({"a": 1})], json!({"a": "x"}));
        assert_eq!(g.len(), 1);
    }

    #[test]
    fn the_items_of_an_array_are_compared_at_any_depth() {
        let recorded = [json!({"items": [{"id": 1, "p": {"cur": "EUR"}}]})];
        let live = json!({"items": [{"id": "1", "p": {}, "new": true}]});
        let f = cmp(&recorded, live);
        assert_eq!(kinds(&f), vec!["retyped $.items[].id", "missing $.items[].p.cur", "added $.items[].new"]);
        // an empty list has nothing to compare
        assert!(cmp(&recorded, json!({"items": []})).is_empty());
        // a root array
        let g = cmp(&[json!([{"a": 1}])], json!([{"b": 1}]));
        assert_eq!(kinds(&g), vec!["missing $[].a", "added $[].b"]);
    }

    #[test]
    fn a_dictionary_is_compared_by_its_values() {
        let mut m = serde_json::Map::new();
        let mut n = serde_json::Map::new();
        for i in 0..10 {
            m.insert(format!("{:08}", 10_000_000 + i), json!({"a": 1}));
            n.insert(format!("{:08}", 20_000_000 + i), json!({"a": "x"}));
        }
        let f = cmp(&[Value::Object(m)], Value::Object(n));
        assert_eq!(kinds(&f), vec!["retyped $.*.a"]);
    }

    fn ep(responses: Vec<(u16, &str, Shape)>) -> Endpoint {
        let mut map = IndexMap::new();
        for (status, mime, shape) in responses {
            map.insert(status, Payload { mime: mime.to_string(), shape, non_json: 0 });
        }
        Endpoint {
            id: "get_x".to_string(),
            method: "GET".to_string(),
            origin: "https://app.test".to_string(),
            path: "/x".to_string(),
            gql: None,
            calls: 1,
            path_params: Vec::new(),
            query: Vec::new(),
            headers: Vec::new(),
            body: None,
            responses: map,
            paging: None,
            auth: Vec::new(),
            example: String::new(),
        }
    }

    #[test]
    fn a_status_the_map_never_saw_is_broken() {
        let e = ep(vec![(200, "application/json", Shape::of(&json!({"a": 1})))]);
        let r = check(&e, 404, "application/json", Some(&json!({})));
        assert_eq!(r.verdict, Verdict::Broken);
        assert_eq!(r.broken, vec!["status 404, the map recorded 200".to_string()]);
        let r = check(&e, 401, "application/json", None);
        assert_eq!(r.verdict, Verdict::Broken);
        assert!(r.broken[0].starts_with("auth failed"), "{:?}", r.broken);
        let r = check(&e, 204, "", None);
        assert_eq!(r.verdict, Verdict::Broken);
    }

    #[test]
    fn a_map_without_responses_expects_a_success() {
        let e = ep(vec![]);
        assert_eq!(check(&e, 200, "text/html", None).verdict, Verdict::Ok);
        assert_eq!(check(&e, 500, "", None).verdict, Verdict::Broken);
    }

    #[test]
    fn the_report_counts_drift_and_information() {
        let e = ep(vec![(200, "application/json", Shape::of(&json!({"a": 1, "b": "x"})))]);
        let ok = check(&e, 200, "application/json; charset=utf-8", Some(&json!({"a": 2, "b": "y"})));
        assert_eq!(ok.verdict, Verdict::Ok);
        let info = check(&e, 200, "application/json", Some(&json!({"a": 2, "b": "y", "c": 1})));
        assert_eq!((info.verdict, info.drift_count(), info.info_count()), (Verdict::Ok, 0, 1));
        let d = check(&e, 200, "application/json", Some(&json!({"a": "2", "c": 1})));
        assert_eq!((d.verdict, d.drift_count(), d.info_count()), (Verdict::Drift, 2, 1));
        assert!(d.findings[0].is_drift() && !d.findings[2].is_drift(), "drift comes first");
    }

    #[test]
    fn a_changed_media_type_or_a_non_json_answer_is_drift() {
        let e = ep(vec![(200, "application/json", Shape::of(&json!({"a": 1})))]);
        let r = check(&e, 200, "text/html", None);
        assert_eq!(r.verdict, Verdict::Drift);
        assert_eq!(r.changes, vec!["content type changed: application/json -> text/html".to_string()]);
        let r = check(&e, 200, "application/json", None);
        assert_eq!(r.changes, vec!["the answer is no longer JSON".to_string()]);
        // json and a +json type are the same family
        let r = check(&e, 200, "application/vnd.api+json", Some(&json!({"a": 1})));
        assert_eq!(r.verdict, Verdict::Ok);
    }

    #[test]
    fn a_vanished_message_type_or_channel_is_drift_and_a_new_one_is_information() {
        use crate::channel::{self, Kind as ChKind};
        let (all, live) = crate::har::read_all(&channel::tests::har()).unwrap();
        let old = channel::build(&all, &live, &[]);
        let mut new = old.clone();
        let ws = new.iter_mut().find(|c| c.kind == ChKind::Ws).unwrap();
        ws.types.retain(|t| t.name != "trade");
        let mut extra = ws.types[0].clone();
        extra.name = "orderbook".to_string();
        ws.types.push(extra);
        let found = channels(&old, &new);
        assert!(found[0].drift && found[0].text.contains("message type trade vanished"), "{found:?}");
        assert!(found.iter().any(|f| !f.drift && f.text.contains("new message type orderbook")), "{found:?}");
        assert!(found.iter().all(|f| !f.text.contains("ticker")), "{found:?}");
        // a whole channel gone
        new.retain(|c| c.kind == ChKind::Ws);
        let found = channels(&old, &new);
        assert!(found.iter().any(|f| f.drift && f.text == "channel sse_api_stream vanished"), "{found:?}");
        // the same difference seen in a live follow is only "not seen"
        let live_only = channel(&old[0], &new[0]);
        assert!(live_only.iter().any(|f| f.drift && f.text.contains("trade not seen")), "{live_only:?}");
    }
}
