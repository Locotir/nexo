//! The description of an API as OpenAPI 3.1: paths with their parameters, bodies and responses as JSON Schema, the
//! auth schemes found in the traffic, paging as an extension, and GraphQL operations under `x-nexo-graphql` (they all
//! share one URL, so they cannot be paths). Everything comes from shapes and names; no secret value is in it.

use crate::auth::{Auth, Scheme};
use crate::exchange::clean_url;
use crate::shape::Shape;
use crate::spec::{Api, Endpoint, Payload};
use serde_json::{json, Map, Value};

/// A URL inside an example or an enum is cleaned like any URL taken from a response.
fn scrub(v: &mut Value) {
    match v {
        Value::String(s) => {
            if s.contains("://") || (s.starts_with('/') && s.contains('?')) {
                *s = clean_url(s);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(scrub),
        Value::Object(m) => m.iter_mut().for_each(|(_, x)| scrub(x)),
        _ => {}
    }
}

fn schema(shape: &Shape) -> Value {
    let mut v = shape.schema();
    scrub(&mut v);
    v
}

/// The `content` of a body or a response: its media type and its schema (`{}` when it was not JSON).
fn content(p: &Payload, default_mime: &str) -> Value {
    let mime = if p.mime.is_empty() { default_mime } else { p.mime.as_str() };
    let s = if p.shape.seen > 0 { schema(&p.shape) } else { json!({}) };
    let mut m = Map::new();
    m.insert(mime.to_string(), json!({ "schema": s }));
    Value::Object(m)
}

fn security_scheme(a: &Auth) -> Value {
    let mut m = Map::new();
    let key = |place: &str, name: &str, m: &mut Map<String, Value>| {
        m.insert("type".into(), json!("apiKey"));
        m.insert("in".into(), json!(place));
        m.insert("name".into(), json!(name));
    };
    match &a.scheme {
        Scheme::Bearer { header } if header == "authorization" => {
            m.insert("type".into(), json!("http"));
            m.insert("scheme".into(), json!("bearer"));
        }
        Scheme::Basic { header } if header == "authorization" => {
            m.insert("type".into(), json!("http"));
            m.insert("scheme".into(), json!("basic"));
        }
        Scheme::Bearer { header } => {
            key("header", header, &mut m);
            m.insert("x-nexo-scheme".into(), json!("bearer"));
        }
        Scheme::Basic { header } => {
            key("header", header, &mut m);
            m.insert("x-nexo-scheme".into(), json!("basic"));
        }
        Scheme::ApiKeyHeader { name } => key("header", name, &mut m),
        Scheme::ApiKeyQuery { name } => key("query", name, &mut m),
        Scheme::Cookie { names } => {
            key("cookie", names.first().map(String::as_str).unwrap_or(""), &mut m);
            m.insert("x-nexo-cookies".into(), json!(names));
        }
        Scheme::Csrf { header, from } => {
            key("header", header, &mut m);
            m.insert("x-nexo-from".into(), json!(from));
        }
    }
    if let Some(s) = &a.source {
        m.insert("x-nexo-source".into(), json!(s));
    }
    Value::Object(m)
}

fn paging_json(e: &Endpoint) -> Option<Value> {
    let p = e.paging.as_ref()?;
    let mut m = Map::new();
    m.insert("style".into(), json!(p.style));
    for (k, v) in [("param", &p.param), ("size", &p.size), ("next", &p.next), ("items", &p.items)] {
        if let Some(v) = v {
            m.insert(k.into(), json!(v));
        }
    }
    Some(Value::Object(m))
}

fn operation(e: &Endpoint, multi: bool) -> Value {
    let mut op = Map::new();
    op.insert("operationId".into(), json!(e.id));
    let mut params: Vec<Value> = Vec::new();
    for p in &e.path_params {
        params.push(json!({ "name": p.name, "in": "path", "required": true, "schema": schema(&p.shape) }));
    }
    for p in &e.query {
        params
            .push(json!({ "name": p.name, "in": "query", "required": p.seen == e.calls, "schema": schema(&p.shape) }));
    }
    for h in &e.headers {
        params.push(json!({ "name": h, "in": "header", "required": false, "schema": { "type": "string" } }));
    }
    if !params.is_empty() {
        op.insert("parameters".into(), Value::Array(params));
    }
    if let Some(b) = &e.body {
        let default = if b.shape.seen > 0 { "application/json" } else { "application/octet-stream" };
        op.insert("requestBody".into(), json!({ "content": content(b, default) }));
    }
    let mut responses = Map::new();
    for (status, p) in &e.responses {
        let mut r = Map::new();
        r.insert("description".into(), json!(format!("HTTP {status}")));
        if p.shape.seen > 0 || !p.mime.is_empty() {
            r.insert("content".into(), content(p, "application/json"));
        }
        responses.insert(status.to_string(), Value::Object(r));
    }
    if responses.is_empty() {
        responses.insert("default".into(), json!({ "description": "no response was captured" }));
    }
    op.insert("responses".into(), Value::Object(responses));
    if !e.auth.is_empty() {
        let mut req = Map::new();
        for i in &e.auth {
            req.insert(format!("a{}", i + 1), json!([]));
        }
        op.insert("security".into(), json!([req]));
    }
    if let Some(p) = paging_json(e) {
        op.insert("x-nexo-paging".into(), p);
    }
    if multi {
        op.insert("servers".into(), json!([{ "url": e.origin }]));
    }
    op.insert("x-nexo-example".into(), json!(e.example));
    Value::Object(op)
}

/// OpenAPI 3.1 for what the traffic showed. `title` names the API.
pub fn to_openapi(api: &Api, title: &str) -> Value {
    let mut root = Map::new();
    root.insert("openapi".into(), json!("3.1.0"));
    root.insert(
        "info".into(),
        json!({
            "title": title,
            "version": "0.0.0",
            "description": "Worked out from captured traffic by nexo api. No secret value is kept.",
        }),
    );
    let servers: Vec<Value> = api.hosts.iter().map(|h| json!({ "url": h })).collect();
    root.insert("servers".into(), Value::Array(servers));
    let multi = api.hosts.len() > 1;

    let mut paths = Map::new();
    let mut more: Vec<Value> = Vec::new();
    let mut graphql: Vec<Value> = Vec::new();
    for e in &api.endpoints {
        if let Some(g) = &e.gql {
            let (kind, name) = g.split_once(' ').unwrap_or(("query", g.as_str()));
            let vars = e.body.as_ref().map(|b| schema(&b.shape)).unwrap_or_else(|| json!({}));
            graphql.push(json!({
                "operation": name,
                "kind": kind,
                "path": e.path,
                "origin": e.origin,
                "variables": vars,
            }));
            continue;
        }
        let method = e.method.to_ascii_lowercase();
        let op = operation(e, multi);
        let item = paths.entry(e.path.clone()).or_insert_with(|| json!({}));
        match item.as_object_mut() {
            Some(m) if !m.contains_key(&method) => {
                m.insert(method, op);
            }
            _ => more.push(json!({ "method": method, "path": e.path, "origin": e.origin, "operation": op })),
        }
    }
    root.insert("paths".into(), Value::Object(paths));

    if !api.auth.is_empty() {
        let mut schemes = Map::new();
        for (i, a) in api.auth.iter().enumerate() {
            schemes.insert(format!("a{}", i + 1), security_scheme(a));
        }
        root.insert("components".into(), json!({ "securitySchemes": schemes }));
    }
    if !graphql.is_empty() {
        root.insert("x-nexo-graphql".into(), Value::Array(graphql));
    }
    if !more.is_empty() {
        root.insert("x-nexo-more-operations".into(), Value::Array(more));
    }
    Value::Object(root)
}
