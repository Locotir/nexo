//! The map on disk: an [`Api`] as JSON (`nexo api analyze HAR --save FILE`) and back (`nexo api call|describe|watch`).
//! It holds what the [`Api`] holds and nothing more: names, paths, shapes, fingerprint-free auth schemes, and
//! `<secret>` where a capture had one. The format is written by hand over `serde_json::Value`: the model has
//! `&'static str` fields that a derive could not read back, and the crate stays free of `serde`.

use crate::auth::{Auth, Scheme};
use crate::paging::Paging;
use crate::shape::{Fmt, Shape};
use crate::spec::{Api, Endpoint, Param, Payload};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};

/// The version of the file format; a newer file is refused.
pub const VERSION: u64 = 1;

/// How deep a saved shape may nest when it is read back.
const MAX_DEPTH: usize = 64;

/// The reasons a call can be dropped (see `noise.rs`); a file with another one reads as `other`.
const REASONS: [&str; 5] = ["preflight", "tracker", "static file", "page", "other"];
/// The paging styles (see `paging.rs`).
const STYLES: [&str; 5] = ["cursor", "next url", "page", "offset", "link header"];

fn put(m: &mut Map<String, Value>, key: &str, n: usize) {
    if n > 0 {
        m.insert(key.to_string(), json!(n));
    }
}

fn fmt_name(f: Fmt) -> Option<&'static str> {
    match f {
        Fmt::Unset => None,
        Fmt::Mixed => Some("mixed"),
        Fmt::Uuid => Some("uuid"),
        Fmt::DateTime => Some("date-time"),
        Fmt::Date => Some("date"),
        Fmt::Email => Some("email"),
        Fmt::Uri => Some("uri"),
        Fmt::Digits => Some("digits"),
    }
}

fn fmt_of(name: &str) -> Fmt {
    match name {
        "mixed" => Fmt::Mixed,
        "uuid" => Fmt::Uuid,
        "date-time" => Fmt::DateTime,
        "date" => Fmt::Date,
        "email" => Fmt::Email,
        "uri" => Fmt::Uri,
        "digits" => Fmt::Digits,
        _ => Fmt::Unset,
    }
}

fn shape_json(s: &Shape) -> Value {
    let mut m = Map::new();
    put(&mut m, "seen", s.seen);
    put(&mut m, "nulls", s.nulls);
    put(&mut m, "bools", s.bools);
    put(&mut m, "ints", s.ints);
    put(&mut m, "nums", s.nums);
    put(&mut m, "strs", s.strs);
    if let Some(f) = fmt_name(s.fmt) {
        m.insert("fmt".to_string(), json!(f));
    }
    if !s.enums.is_empty() {
        m.insert("enums".to_string(), json!(s.enums));
    }
    if s.long {
        m.insert("long".to_string(), json!(true));
    }
    if let Some(e) = &s.example {
        m.insert("example".to_string(), e.clone());
    }
    put(&mut m, "arrays", s.arrays);
    if let Some(i) = &s.items {
        m.insert("items".to_string(), shape_json(i));
    }
    put(&mut m, "max_items", s.max_items);
    put(&mut m, "objects", s.objects);
    if !s.fields.is_empty() {
        let f: Map<String, Value> = s.fields.iter().map(|(k, v)| (k.clone(), shape_json(v))).collect();
        m.insert("fields".to_string(), Value::Object(f));
    }
    if let Some(d) = &s.dict {
        m.insert("dict".to_string(), shape_json(d));
    }
    put(&mut m, "cut", s.cut);
    Value::Object(m)
}

fn count(m: &Map<String, Value>, key: &str) -> usize {
    m.get(key).and_then(Value::as_u64).map(|n| n as usize).unwrap_or(0)
}

fn text(m: &Map<String, Value>, key: &str) -> Result<String, String> {
    m.get(key).and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("missing text field \"{key}\""))
}

fn opt_text(m: &Map<String, Value>, key: &str) -> Option<String> {
    m.get(key).and_then(Value::as_str).map(str::to_string)
}

fn texts(m: &Map<String, Value>, key: &str) -> Vec<String> {
    m.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn object<'a>(v: &'a Value, what: &str) -> Result<&'a Map<String, Value>, String> {
    v.as_object().ok_or_else(|| format!("{what} is not an object"))
}

fn shape_of(v: &Value, depth: usize) -> Result<Shape, String> {
    if depth > MAX_DEPTH {
        return Err("a shape nests too deep".to_string());
    }
    let m = object(v, "a shape")?;
    let mut s = Shape {
        seen: count(m, "seen"),
        nulls: count(m, "nulls"),
        bools: count(m, "bools"),
        ints: count(m, "ints"),
        nums: count(m, "nums"),
        strs: count(m, "strs"),
        fmt: m.get("fmt").and_then(Value::as_str).map(fmt_of).unwrap_or_default(),
        enums: texts(m, "enums"),
        long: m.get("long").and_then(Value::as_bool).unwrap_or(false),
        example: m.get("example").cloned(),
        arrays: count(m, "arrays"),
        max_items: count(m, "max_items"),
        objects: count(m, "objects"),
        cut: count(m, "cut"),
        ..Shape::default()
    };
    if let Some(i) = m.get("items") {
        s.items = Some(Box::new(shape_of(i, depth + 1)?));
    }
    if let Some(d) = m.get("dict") {
        s.dict = Some(Box::new(shape_of(d, depth + 1)?));
    }
    if let Some(f) = m.get("fields") {
        for (k, v) in object(f, "fields")? {
            s.fields.insert(k.clone(), shape_of(v, depth + 1)?);
        }
    }
    Ok(s)
}

fn scheme_json(s: &Scheme) -> Value {
    match s {
        Scheme::Bearer { header } => json!({ "type": "bearer", "header": header }),
        Scheme::Basic { header } => json!({ "type": "basic", "header": header }),
        Scheme::ApiKeyHeader { name } => json!({ "type": "api_key_header", "name": name }),
        Scheme::ApiKeyQuery { name } => json!({ "type": "api_key_query", "name": name }),
        Scheme::Cookie { names } => json!({ "type": "cookie", "names": names }),
        Scheme::Csrf { header, from } => json!({ "type": "csrf", "header": header, "from": from }),
    }
}

fn scheme_of(m: &Map<String, Value>) -> Result<Scheme, String> {
    Ok(match text(m, "type")?.as_str() {
        "bearer" => Scheme::Bearer { header: text(m, "header")? },
        "basic" => Scheme::Basic { header: text(m, "header")? },
        "api_key_header" => Scheme::ApiKeyHeader { name: text(m, "name")? },
        "api_key_query" => Scheme::ApiKeyQuery { name: text(m, "name")? },
        "cookie" => Scheme::Cookie { names: texts(m, "names") },
        "csrf" => Scheme::Csrf { header: text(m, "header")?, from: text(m, "from")? },
        other => return Err(format!("unknown auth scheme \"{other}\"")),
    })
}

fn paging_json(p: &Paging) -> Value {
    let mut m = Map::new();
    m.insert("style".to_string(), json!(p.style));
    for (k, v) in [("param", &p.param), ("size", &p.size), ("next", &p.next), ("items", &p.items)] {
        if let Some(v) = v {
            m.insert(k.to_string(), json!(v));
        }
    }
    Value::Object(m)
}

fn paging_of(m: &Map<String, Value>) -> Result<Paging, String> {
    let style = text(m, "style")?;
    let style =
        STYLES.iter().copied().find(|s| *s == style).ok_or_else(|| format!("unknown paging style \"{style}\""))?;
    Ok(Paging {
        style,
        param: opt_text(m, "param"),
        size: opt_text(m, "size"),
        next: opt_text(m, "next"),
        items: opt_text(m, "items"),
    })
}

fn payload_json(p: &Payload) -> Value {
    json!({ "mime": p.mime, "shape": shape_json(&p.shape), "non_json": p.non_json })
}

fn payload_of(v: &Value) -> Result<Payload, String> {
    let m = object(v, "a payload")?;
    let shape = match m.get("shape") {
        Some(s) => shape_of(s, 0)?,
        None => Shape::default(),
    };
    Ok(Payload { mime: opt_text(m, "mime").unwrap_or_default(), shape, non_json: count(m, "non_json") })
}

fn param_json(p: &Param) -> Value {
    json!({ "name": p.name, "seen": p.seen, "shape": shape_json(&p.shape) })
}

fn param_of(v: &Value) -> Result<Param, String> {
    let m = object(v, "a param")?;
    let shape = match m.get("shape") {
        Some(s) => shape_of(s, 0)?,
        None => Shape::default(),
    };
    Ok(Param { name: text(m, "name")?, seen: count(m, "seen"), shape })
}

fn endpoint_json(e: &Endpoint) -> Value {
    let mut m = Map::new();
    m.insert("id".to_string(), json!(e.id));
    m.insert("method".to_string(), json!(e.method));
    m.insert("origin".to_string(), json!(e.origin));
    m.insert("path".to_string(), json!(e.path));
    if let Some(g) = &e.gql {
        m.insert("gql".to_string(), json!(g));
    }
    m.insert("calls".to_string(), json!(e.calls));
    m.insert("path_params".to_string(), Value::Array(e.path_params.iter().map(param_json).collect()));
    m.insert("query".to_string(), Value::Array(e.query.iter().map(param_json).collect()));
    m.insert("headers".to_string(), json!(e.headers));
    if let Some(b) = &e.body {
        m.insert("body".to_string(), payload_json(b));
    }
    let responses: Map<String, Value> = e.responses.iter().map(|(s, p)| (s.to_string(), payload_json(p))).collect();
    m.insert("responses".to_string(), Value::Object(responses));
    if let Some(p) = &e.paging {
        m.insert("paging".to_string(), paging_json(p));
    }
    m.insert("auth".to_string(), json!(e.auth));
    m.insert("example".to_string(), json!(e.example));
    Value::Object(m)
}

fn endpoint_of(v: &Value, auths: usize) -> Result<Endpoint, String> {
    let m = object(v, "an endpoint")?;
    let id = text(m, "id")?;
    let ctx = |e: String| format!("endpoint {id}: {e}");
    let list = |key: &str| -> Result<Vec<Param>, String> {
        match m.get(key).and_then(Value::as_array) {
            Some(a) => a.iter().map(param_of).collect(),
            None => Ok(Vec::new()),
        }
    };
    let mut responses = IndexMap::new();
    if let Some(r) = m.get("responses") {
        for (k, p) in object(r, "responses").map_err(ctx)? {
            let status: u16 = k.parse().map_err(|_| ctx(format!("\"{k}\" is not a status")))?;
            responses.insert(status, payload_of(p).map_err(ctx)?);
        }
    }
    let mut auth = Vec::new();
    if let Some(a) = m.get("auth").and_then(Value::as_array) {
        for i in a {
            let i = i.as_u64().map(|n| n as usize).filter(|n| *n < auths);
            auth.push(i.ok_or_else(|| ctx("an auth index is out of range".to_string()))?);
        }
    }
    Ok(Endpoint {
        method: text(m, "method").map_err(ctx)?,
        origin: text(m, "origin").map_err(ctx)?,
        path: text(m, "path").map_err(ctx)?,
        gql: opt_text(m, "gql"),
        calls: count(m, "calls"),
        path_params: list("path_params").map_err(ctx)?,
        query: list("query").map_err(ctx)?,
        headers: texts(m, "headers"),
        body: match m.get("body") {
            Some(b) => Some(payload_of(b).map_err(ctx)?),
            None => None,
        },
        responses,
        paging: match m.get("paging") {
            Some(p) => Some(paging_of(object(p, "paging").map_err(ctx)?).map_err(ctx)?),
            None => None,
        },
        auth,
        example: opt_text(m, "example").unwrap_or_default(),
        id,
    })
}

/// The map as JSON.
pub fn to_json(api: &Api) -> Value {
    let auth: Vec<Value> = api
        .auth
        .iter()
        .map(|a| {
            let mut m = Map::new();
            m.insert("scheme".to_string(), scheme_json(&a.scheme));
            m.insert("uses".to_string(), json!(a.uses));
            if let Some(s) = &a.source {
                m.insert("source".to_string(), json!(s));
            }
            Value::Object(m)
        })
        .collect();
    let dropped: Map<String, Value> = api.dropped.iter().map(|(k, n)| (k.to_string(), json!(n))).collect();
    json!({
        "nexo_api_map": VERSION,
        "hosts": api.hosts,
        "total": api.total,
        "dropped": dropped,
        "auth": auth,
        "endpoints": api.endpoints.iter().map(endpoint_json).collect::<Vec<_>>(),
    })
}

/// Reads a map back. Every error says what was wrong; a file that is not a map is refused.
pub fn from_json(v: &Value) -> Result<Api, String> {
    let m = v.as_object().ok_or("not a nexo api map (not a JSON object)")?;
    let version =
        m.get("nexo_api_map").and_then(Value::as_u64).ok_or("not a nexo api map (no \"nexo_api_map\" field)")?;
    if version > VERSION {
        return Err(format!("map format {version} is newer than this nexo reads ({VERSION})"));
    }
    let mut auth = Vec::new();
    for a in m.get("auth").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]) {
        let a = object(a, "an auth entry")?;
        let scheme = match a.get("scheme") {
            Some(s) => scheme_of(object(s, "a scheme")?)?,
            None => return Err("an auth entry has no scheme".to_string()),
        };
        auth.push(Auth { scheme, uses: count(a, "uses"), source: opt_text(a, "source") });
    }
    let mut endpoints = Vec::new();
    for e in m.get("endpoints").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]) {
        endpoints.push(endpoint_of(e, auth.len())?);
    }
    let mut dropped: IndexMap<&'static str, usize> = IndexMap::new();
    if let Some(d) = m.get("dropped").and_then(Value::as_object) {
        for (k, n) in d {
            let reason = REASONS.iter().copied().find(|r| r == k).unwrap_or("other");
            *dropped.entry(reason).or_insert(0) += n.as_u64().map(|n| n as usize).unwrap_or(0);
        }
    }
    Ok(Api { endpoints, auth, hosts: texts(m, "hosts"), total: count(m, "total"), dropped })
}

/// The map as text, ready to write.
pub fn to_text(api: &Api) -> String {
    let mut s = serde_json::to_string_pretty(&to_json(api)).unwrap_or_else(|_| "{}".to_string());
    s.push('\n');
    s
}

/// Reads the text of a saved map.
pub fn from_text(text: &str) -> Result<Api, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    from_json(&v)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::spec::Options;

    fn har() -> String {
        let h = |n: &str, v: &str| json!({ "name": n, "value": v });
        let get = |url: &str, body: &str| {
            json!({
                "_resourceType": "xhr",
                "request": { "method": "GET", "url": url, "headers": [h("authorization", "Bearer tkAAAA1111bbbbCCCC2222dddd")] },
                "response": { "status": 200, "headers": [], "content": { "mimeType": "application/json", "text": body } },
            })
        };
        let login = json!({
            "_resourceType": "xhr",
            "request": { "method": "POST", "url": "https://app.test/api/login", "headers": [],
                "postData": { "mimeType": "application/json", "text": "{\"email\":\"a@b.co\",\"password\":\"hunter2hunter2\"}" } },
            "response": { "status": 200, "headers": [], "content": { "mimeType": "application/json",
                "text": "{\"access_token\":\"tkAAAA1111bbbbCCCC2222dddd\"}" } },
        });
        json!({ "log": { "entries": [
            login,
            get("https://app.test/api/items?page=1", "{\"items\":[{\"id\":1,\"name\":\"a\"}],\"total\":3}"),
            get("https://app.test/api/items?page=2", "{\"items\":[{\"id\":2,\"name\":\"b\"}],\"total\":3}"),
            get("https://app.test/api/items/42", "{\"id\":42,\"name\":\"a\"}"),
            get("https://app.test/api/items/43", "{\"id\":43,\"name\":\"b\"}"),
        ] } })
        .to_string()
    }

    pub(crate) fn api() -> Api {
        crate::analyze_har(&har(), &Options::default()).unwrap()
    }

    #[test]
    fn a_map_survives_a_save_and_a_load() {
        let a = api();
        assert!(!a.endpoints.is_empty() && !a.auth.is_empty());
        let back = from_text(&to_text(&a)).unwrap();
        assert_eq!(back.hosts, a.hosts);
        assert_eq!(back.total, a.total);
        assert_eq!(back.dropped, a.dropped);
        assert_eq!(back.auth, a.auth);
        assert_eq!(back.endpoints.len(), a.endpoints.len());
        for (x, y) in a.endpoints.iter().zip(&back.endpoints) {
            assert_eq!((&x.id, &x.method, &x.origin, &x.path, &x.gql), (&y.id, &y.method, &y.origin, &y.path, &y.gql));
            assert_eq!(x.calls, y.calls);
            assert_eq!(x.paging, y.paging);
            assert_eq!(x.auth, y.auth);
            assert_eq!(x.example, y.example);
            assert_eq!(x.headers, y.headers);
            assert_eq!(x.path_params.len(), y.path_params.len());
            for (p, q) in x.query.iter().chain(&x.path_params).zip(y.query.iter().chain(&y.path_params)) {
                assert_eq!((&p.name, p.seen, &p.shape), (&q.name, q.seen, &q.shape));
            }
            assert_eq!(
                x.body.as_ref().map(|b| (&b.mime, &b.shape, b.non_json)),
                y.body.as_ref().map(|b| (&b.mime, &b.shape, b.non_json))
            );
            assert_eq!(x.responses.len(), y.responses.len());
            for (s, p) in &x.responses {
                let q = &y.responses[s];
                assert_eq!((&p.mime, &p.shape, p.non_json), (&q.mime, &q.shape, q.non_json));
            }
        }
    }

    #[test]
    fn a_saved_map_holds_no_secret() {
        let t = to_text(&api());
        for secret in ["tkAAAA1111bbbbCCCC2222dddd", "hunter2hunter2"] {
            assert!(!t.contains(secret), "{secret} in {t}");
        }
    }

    #[test]
    fn a_file_that_is_not_a_map_is_refused() {
        assert!(from_text("nope").is_err());
        assert!(from_text("{}").unwrap_err().contains("not a nexo api map"));
        assert!(from_text("{\"nexo_api_map\": 99}").unwrap_err().contains("newer"));
        let bad = json!({ "nexo_api_map": 1, "endpoints": [{ "id": "x", "method": "GET", "origin": "o", "path": "/", "auth": [3] }] });
        assert!(from_json(&bad).unwrap_err().contains("out of range"));
    }
}
