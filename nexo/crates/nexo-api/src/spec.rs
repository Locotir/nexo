//! The API behind a capture: the calls that are worth reading are grouped into endpoints (a method, a templated path,
//! or a GraphQL operation), and each endpoint gets the parameters it was called with, the shape of its body and of
//! each response, how it pages and which auth schemes its calls use. Only names, types and shapes are kept; secrets
//! were taken out when the capture was read, and opaque strings are never shown as examples.

use crate::auth::{self, Auth};
use crate::exchange::{opaque, secret_name, Exchange};
use crate::graphql::{self, Op};
use crate::noise;
use crate::paging::{self, Paging};
use crate::shape::Shape;
use crate::template;
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};

/// What to read.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Keep only these hosts (and the hosts under them); empty keeps all.
    pub hosts: Vec<String>,
}

/// A body (or a response) seen over the calls to one endpoint.
#[derive(Debug, Clone)]
pub struct Payload {
    /// The media type of the first one that had a type; empty when none had.
    pub mime: String,
    /// Everything seen in the JSON ones, merged.
    pub shape: Shape,
    /// How many were not JSON.
    pub non_json: usize,
}

impl Payload {
    fn new(mime: &str) -> Payload {
        Payload { mime: mime.to_string(), shape: Shape::default(), non_json: 0 }
    }
}

/// A path parameter or a query parameter.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    /// In how many calls it was present.
    pub seen: usize,
    /// Everything seen as its value.
    pub shape: Shape,
}

/// One endpoint.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// A short name that is also the OpenAPI operationId (`get_users_user_id`, `gql_GetUser`).
    pub id: String,
    pub method: String,
    /// `https://host`.
    pub origin: String,
    /// `/users/{user_id}`.
    pub path: String,
    /// `query GetUser` for a GraphQL operation.
    pub gql: Option<String>,
    pub calls: usize,
    pub path_params: Vec<Param>,
    pub query: Vec<Param>,
    /// Request headers that are not standard ones and not secret (`x-requested-with`).
    pub headers: Vec<String>,
    /// For a GraphQL operation: its variables.
    pub body: Option<Payload>,
    pub responses: IndexMap<u16, Payload>,
    pub paging: Option<Paging>,
    /// Indexes into [`Api::auth`].
    pub auth: Vec<usize>,
    /// One real URL of the group, opaque values hidden.
    pub example: String,
}

/// The whole description.
#[derive(Debug, Clone)]
pub struct Api {
    pub endpoints: Vec<Endpoint>,
    pub auth: Vec<Auth>,
    /// The origins of the endpoints, in order.
    pub hosts: Vec<String>,
    /// How many exchanges the capture held.
    pub total: usize,
    /// How many were dropped, by reason.
    pub dropped: IndexMap<&'static str, usize>,
}

/// The GraphQL calls by method, origin, path and operation: each call with its operation and how many its request had.
type GqlGroups<'a> = IndexMap<(String, String, String, String), Vec<(&'a Exchange, Op, usize)>>;

/// One call of an endpoint; for GraphQL, the operation it holds and how many operations its request had.
struct Call<'a> {
    x: &'a Exchange,
    op: Option<(&'a Op, usize)>,
}

/// An endpoint before the order and the ids are fixed.
struct Draft {
    first: usize,
    ep: Endpoint,
}

/// Request headers that every client sends; they say nothing about the API.
fn standard(n: &str) -> bool {
    n.starts_with("accept")
        || n.starts_with("content-")
        || n.starts_with("sec-")
        || n.starts_with("if-")
        || matches!(
            n,
            "user-agent"
                | "referer"
                | "origin"
                | "cookie"
                | "authorization"
                | "host"
                | "connection"
                | "cache-control"
                | "pragma"
                | "priority"
                | "dnt"
                | "upgrade-insecure-requests"
                | "te"
        )
}

/// A GraphQL response of a batch is an array with one answer per operation.
fn pick<'v>(v: &'v Value, op: Option<(&Op, usize)>) -> &'v Value {
    match (v, op) {
        (Value::Array(a), Some((op, n))) if n > 1 => a.get(op.index).unwrap_or(v),
        _ => v,
    }
}

/// One real URL, with opaque query values hidden.
fn example(x: &Exchange, with_query: bool) -> String {
    let mut s = format!("{}{}", x.origin(), auth::shown_path(x));
    if with_query {
        let pairs: Vec<String> = x
            .url
            .query_pairs()
            .map(|(k, v)| if opaque(&v) { format!("{k}=<opaque>") } else { format!("{k}={v}") })
            .collect();
        if !pairs.is_empty() {
            s.push('?');
            s.push_str(&pairs.join("&"));
        }
    }
    s
}

/// What the calls to one endpoint have in common.
fn fill(calls: &[Call], auths: &[Auth], ep: &mut Endpoint) {
    ep.calls = calls.len();
    let gql = ep.gql.is_some();

    let mut query: IndexMap<String, (usize, Shape)> = IndexMap::new();
    let mut headers: Vec<String> = Vec::new();
    let mut body: Option<Payload> = None;
    let mut responses: IndexMap<u16, Payload> = IndexMap::new();
    let mut ok = Shape::default();
    let mut used: BTreeSet<usize> = BTreeSet::new();

    for c in calls {
        let mut seen: HashSet<String> = HashSet::new();
        for (k, v) in c.x.url.query_pairs() {
            if gql && matches!(&*k, "query" | "operationName" | "variables" | "extensions") {
                continue;
            }
            let e = query.entry(k.to_string()).or_default();
            if seen.insert(k.to_string()) {
                e.0 += 1;
            }
            e.1.add(&Value::String(v.to_string()));
        }
        for (n, _) in &c.x.req_headers {
            if !standard(n) && !secret_name(n) && !headers.contains(n) {
                headers.push(n.clone());
            }
        }
        match c.op {
            Some((op, _)) => {
                if let Some(v) = &op.variables {
                    let p = body.get_or_insert_with(|| Payload::new("application/json"));
                    p.shape.add(v);
                }
            }
            None => {
                if let Some(b) = &c.x.req_body {
                    let p = body.get_or_insert_with(|| Payload::new(&b.mime));
                    if p.mime.is_empty() {
                        p.mime = b.mime.clone();
                    }
                    match &b.json {
                        Some(v) => p.shape.add(v),
                        None => p.non_json += 1,
                    }
                }
            }
        }
        let p = responses.entry(c.x.status).or_insert_with(|| Payload::new(""));
        if let Some(b) = &c.x.resp_body {
            if p.mime.is_empty() {
                p.mime = b.mime.clone();
            }
            match &b.json {
                Some(v) => {
                    let v = pick(v, c.op);
                    p.shape.add(v);
                    if (200..300).contains(&c.x.status) {
                        ok.add(v);
                    }
                }
                None => p.non_json += 1,
            }
        }
        used.extend(auth::of_call(c.x, auths));
    }

    let xs: Vec<&Exchange> = calls.iter().map(|c| c.x).collect();
    ep.query = query.into_iter().map(|(name, (seen, shape))| Param { name, seen, shape }).collect();
    ep.headers = headers;
    ep.body = body;
    ep.responses = responses;
    ep.paging = paging::detect(&xs, &ok);
    ep.auth = used.into_iter().collect();
    ep.example = xs.first().map(|x| example(x, !gql)).unwrap_or_default();
}

fn blank(method: &str, origin: &str, path: &str) -> Endpoint {
    Endpoint {
        id: String::new(),
        method: method.to_string(),
        origin: origin.to_string(),
        path: path.to_string(),
        gql: None,
        calls: 0,
        path_params: Vec::new(),
        query: Vec::new(),
        headers: Vec::new(),
        body: None,
        responses: IndexMap::new(),
        paging: None,
        auth: Vec::new(),
        example: String::new(),
    }
}

/// Letters and digits stay, every run of anything else becomes one `_`.
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

/// `get_users_user_id` for `GET /users/{user_id}`, `gql_GetUser` for a GraphQL operation.
fn base_id(e: &Endpoint) -> String {
    if let Some(g) = &e.gql {
        let name = g.split_once(' ').map(|(_, n)| n).unwrap_or(g);
        return format!("gql_{}", slug(name));
    }
    let words: Vec<String> = e.path.split('/').map(slug).filter(|w| !w.is_empty()).collect();
    let tail = if words.is_empty() { "root".to_string() } else { words.join("_") };
    format!("{}_{}", e.method.to_ascii_lowercase(), tail)
}

/// Reads what a capture holds: drops the noise, groups the calls, and describes each endpoint.
pub fn build(all: &[Exchange], o: &Options) -> Api {
    let (kept, dropped) = noise::sift(all, &o.hosts);
    let auths = auth::detect(all);

    let mut plain: Vec<&Exchange> = Vec::new();
    let mut gql: GqlGroups = IndexMap::new();
    for &i in &kept {
        let x = &all[i];
        let ops = graphql::ops(x);
        if ops.is_empty() {
            plain.push(x);
            continue;
        }
        let n = ops.len();
        for op in ops {
            let key = (x.method.clone(), x.origin(), x.url.path().to_string(), format!("{} {}", op.kind, op.name));
            gql.entry(key).or_default().push((x, op, n));
        }
    }

    let mut drafts: Vec<Draft> = Vec::new();
    for g in template::group(&plain) {
        let calls: Vec<Call> = g.members.iter().map(|(i, _)| Call { x: plain[*i], op: None }).collect();
        let mut ep = blank(&g.method, &g.origin, &g.path);
        ep.path_params = g
            .params
            .iter()
            .enumerate()
            .map(|(j, name)| {
                let mut shape = Shape::default();
                for (_, values) in &g.members {
                    if let Some(v) = values.get(j) {
                        shape.add(&Value::String(v.clone()));
                    }
                }
                Param { name: name.clone(), seen: g.members.len(), shape }
            })
            .collect();
        fill(&calls, &auths, &mut ep);
        let first = calls.iter().map(|c| c.x.seq).min().unwrap_or(0);
        drafts.push(Draft { first, ep });
    }
    for ((method, origin, path, name), members) in &gql {
        let calls: Vec<Call> = members.iter().map(|(x, op, n)| Call { x: *x, op: Some((op, *n)) }).collect();
        let mut ep = blank(method, origin, path);
        ep.gql = Some(name.clone());
        fill(&calls, &auths, &mut ep);
        let first = calls.iter().map(|c| c.x.seq).min().unwrap_or(0);
        drafts.push(Draft { first, ep });
    }

    drafts.sort_by(|a, b| b.ep.calls.cmp(&a.ep.calls).then(a.first.cmp(&b.first)));
    let mut taken: HashMap<String, usize> = HashMap::new();
    let mut hosts: Vec<String> = Vec::new();
    let mut endpoints = Vec::with_capacity(drafts.len());
    for d in drafts {
        let mut ep = d.ep;
        let base = base_id(&ep);
        let n = taken.entry(base.clone()).or_insert(0);
        *n += 1;
        ep.id = if *n == 1 { base } else { format!("{base}_{n}") };
        if !hosts.contains(&ep.origin) {
            hosts.push(ep.origin.clone());
        }
        endpoints.push(ep);
    }
    Api { endpoints, auth: auths, hosts, total: all.len(), dropped }
}
