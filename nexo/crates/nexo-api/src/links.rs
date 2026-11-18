//! Where a request's values come from: a value that one response hands out and a later request to another endpoint
//! sends back (an id from a list used in the path of the detail call, a cursor, a `Location`), and a secret that one
//! response hands out and later requests send (a login's token). An agent can then chain calls without guessing.
//!
//! The map keeps paths and names only, never a value. A secret is followed by its fingerprint ([`Exchange::sent`] and
//! [`Exchange::got`]) and becomes a link of kind `auth`. A candidate that means nothing is dropped and counted, by
//! reason: a boolean, a small number whose field is not an id, an empty or tiny string, a value an endpoint only
//! echoes, a value seen in many unrelated places, a link from an endpoint to itself (but a cursor), a secret that a
//! page (not an endpoint) handed out.

use crate::call::path_matches;
use crate::exchange::{fingerprint, secret_name, secret_param, Exchange, Fp, Place as Spot, SECRET};
use crate::graphql;
use crate::noise;
use crate::spec::{standard, Api, Endpoint};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Why a candidate link is dropped; the last one is what a file with another reason reads as.
pub const REASONS: [&str; 9] = [
    "boolean",
    "empty or short",
    "small number",
    "same endpoint",
    "echo",
    "too common",
    "source is not an endpoint",
    "over the cap",
    "other",
];

/// How many scalar values of one body are looked at, and of the whole capture; the rest is counted as `over the cap`.
const MAX_LEAVES: usize = 4000;
const MAX_TOTAL: usize = 200_000;
/// How many earlier places that hold the same value are weighed for one request value (more is `too common` anyway),
/// and how many links a map keeps.
const MAX_SCAN: usize = 32;
const MAX_LINKS: usize = 5000;
/// A text longer than this is not a value an id or a cursor is made of.
const MAX_TEXT: usize = 512;
/// A number under this is not a link unless its field name says it is an id.
const SMALL: f64 = 1000.0;
/// A text shorter than this (in characters) is not a link.
const MIN_TEXT: usize = 3;
/// A value found in responses at more places than this, or sent to more parameters than [`MAX_TARGETS`], is too common.
const MAX_SOURCES: usize = 3;
const MAX_TARGETS: usize = 6;
/// How many steps `nexo api call` prints, how many links one summary line and one flow group list.
pub const MAX_NEXT: usize = 3;
const MAX_LINE: usize = 3;
const MAX_FLOW: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A plain value (an id, a cursor).
    Value,
    /// A secret; no value is kept anywhere.
    Auth,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Value => "value",
            Kind::Auth => "auth",
        }
    }

    pub fn of(s: &str) -> Option<Kind> {
        match s {
            "value" => Some(Kind::Value),
            "auth" => Some(Kind::Auth),
            _ => None,
        }
    }
}

/// Where the value goes in the later request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Place {
    Path,
    Query,
    Body,
    Header,
    Cookie,
}

impl Place {
    pub fn name(self) -> &'static str {
        match self {
            Place::Path => "path",
            Place::Query => "query",
            Place::Body => "body",
            Place::Header => "header",
            Place::Cookie => "cookie",
        }
    }

    pub fn of(s: &str) -> Option<Place> {
        [Place::Path, Place::Query, Place::Body, Place::Header, Place::Cookie].into_iter().find(|p| p.name() == s)
    }
}

/// A value that came out of one endpoint's response and went into another's request.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub kind: Kind,
    /// The id of the endpoint whose response holds it.
    pub from: String,
    /// Where in that response: a JSON path (`$.items[].id`, `[]` stands for any array item), `header:location` (with
    /// `#2` for the third path segment of it, `?name` for a query value) or `cookie:NAME` for a `set-cookie`.
    pub path: String,
    /// The status of the response it was first seen in.
    pub status: u16,
    /// The id of the endpoint whose request takes it.
    pub to: String,
    pub place: Place,
    /// The parameter: a path or query name, a header or cookie name, or the JSON path of a body field without `$.`.
    pub param: String,
    /// In how many requests it was seen.
    pub seen: usize,
    /// The number (in the capture) of the first of them.
    pub first: usize,
}

impl Link {
    /// The source as a short text: `items[].id`, `header:location`.
    pub fn from_short(&self) -> String {
        short(&self.path)
    }

    /// The parameter as the person says it to `nexo api call -p`: a body field is `body.NAME`.
    pub fn arg(&self) -> String {
        match self.place {
            Place::Body => format!("body.{}", self.param),
            _ => self.param.clone(),
        }
    }
}

/// A JSON path without its `$.`.
pub fn short(path: &str) -> String {
    path.strip_prefix("$.").or_else(|| path.strip_prefix('$')).unwrap_or(path).to_string()
}

// ---------------------------------------------------------------- reading a capture

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ty {
    Bool,
    Num,
    Str,
}

/// A scalar of a JSON body: its generalized path, the name of its field, its text.
struct Leaf {
    path: String,
    name: String,
    text: String,
    ty: Ty,
}

fn walk(v: &Value, path: &str, name: &str, out: &mut Vec<Leaf>, n: &mut usize, cut: &mut usize, max: usize) {
    let (text, ty) = match v {
        Value::Object(m) => {
            for (k, c) in m {
                walk(c, &format!("{path}.{k}"), k, out, n, cut, max);
            }
            return;
        }
        Value::Array(a) => {
            let inner = format!("{path}[]");
            for c in a {
                walk(c, &inner, name, out, n, cut, max);
            }
            return;
        }
        Value::Null => return,
        Value::Bool(b) => (b.to_string(), Ty::Bool),
        Value::Number(x) => (x.to_string(), Ty::Num),
        Value::String(s) => (s.clone(), Ty::Str),
    };
    if text == SECRET {
        return;
    }
    if *n >= max || text.chars().count() > MAX_TEXT {
        *cut += 1;
        return;
    }
    *n += 1;
    out.push(Leaf { path: path.to_string(), name: name.to_string(), text, ty });
}

/// The scalars of a body, `[3]` as `[]`; `budget` is what is left of the capture's total, and it shrinks.
fn leaves(v: &Value, cut: &mut usize, budget: &mut usize) -> Vec<Leaf> {
    let mut out = Vec::new();
    let mut n = 0;
    walk(v, "$", "", &mut out, &mut n, cut, (*budget).min(MAX_LEAVES));
    *budget -= out.len();
    out
}

/// `$.a[0].b` as `$.a[].b`.
fn general(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '[' {
            while chars.peek().is_some_and(|d| d.is_ascii_digit()) {
                chars.next();
            }
        }
    }
    out
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(byte) = hex {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether a field name says its value is an id: `id`, `user_id`, `userId`, `item-id`, not `paid`.
fn id_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower == "id" {
        return true;
    }
    if !lower.ends_with("id") || name.len() < 3 {
        return false;
    }
    let before = name[..name.len() - 2].chars().last().unwrap_or(' ');
    !before.is_alphanumeric() || name.ends_with("Id") || name.ends_with("ID")
}

fn small_number(text: &str, ty: Ty) -> bool {
    ty != Ty::Bool && text.parse::<f64>().is_ok_and(|f| f.is_finite() && f.abs() < SMALL)
}

/// A value of a request: where it goes and what it is.
struct Req {
    place: Place,
    param: String,
    /// The last name on the way (for an id check).
    name: String,
    text: String,
    ty: Ty,
}

/// A value of a response: the first one seen at a place (an endpoint and a path).
struct Src {
    seq: usize,
    ep: usize,
    path: String,
    ty: Ty,
    id: bool,
    status: u16,
}

/// One exchange as the matching needs it.
struct Ex {
    ep: Option<usize>,
    reqs: Vec<Req>,
}

/// The endpoint a call belongs to: same method, origin and a path that fits its template (the one with the fewest
/// parameters when several do). A GraphQL call belongs to none: its operations share one URL.
fn endpoint_of(api: &Api, x: &Exchange) -> Option<usize> {
    if !graphql::ops(x).is_empty() {
        return None;
    }
    let origin = x.origin();
    let path = x.url.path();
    api.endpoints
        .iter()
        .enumerate()
        .filter(|(_, e)| e.gql.is_none() && e.method == x.method && e.origin == origin && path_matches(&e.path, path))
        .min_by_key(|(_, e)| e.path.matches('{').count())
        .map(|(i, _)| i)
}

fn requests(e: &Endpoint, x: &Exchange, cut: &mut usize, budget: &mut usize) -> Vec<Req> {
    let mut out = Vec::new();
    let t: Vec<&str> = e.path.split('/').collect();
    let c: Vec<&str> = x.url.path().split('/').collect();
    for (t, c) in t.iter().zip(&c) {
        if t.len() > 2 && t.starts_with('{') && t.ends_with('}') {
            let name = t[1..t.len() - 1].to_string();
            out.push(Req { place: Place::Path, param: name.clone(), name, text: decode(c), ty: Ty::Str });
        }
    }
    for (k, v) in x.url.query_pairs() {
        if v != SECRET {
            out.push(Req {
                place: Place::Query,
                param: k.to_string(),
                name: k.to_string(),
                text: v.to_string(),
                ty: Ty::Str,
            });
        }
    }
    if let Some(v) = x.req_body.as_ref().and_then(|b| b.json.as_ref()) {
        for l in leaves(v, cut, budget) {
            out.push(Req { place: Place::Body, param: short(&l.path), name: l.name, text: l.text, ty: l.ty });
        }
    }
    for (n, v) in &x.req_headers {
        if !standard(n) && !secret_name(n) && !v.is_empty() && v != SECRET {
            out.push(Req { place: Place::Header, param: n.clone(), name: n.clone(), text: v.clone(), ty: Ty::Str });
        }
    }
    out
}

/// The path segments and query values of a `Location` as leaves (`header:location#2`, `header:location?name`), the
/// segments that are words of the path of the call itself left out.
fn location_leaves(x: &Exchange, own: &Endpoint) -> Vec<Leaf> {
    let Some(loc) = x.resp_header("location") else { return Vec::new() };
    let Ok(u) = url::Url::parse("http://relative.invalid/").and_then(|b| b.join(loc.trim())) else {
        return Vec::new();
    };
    let own_words: HashSet<&str> = own.path.split('/').filter(|s| !s.starts_with('{')).collect();
    let mut out = Vec::new();
    for (i, seg) in u.path().split('/').filter(|s| !s.is_empty()).enumerate() {
        let text = decode(seg);
        if !own_words.contains(text.as_str()) && text != SECRET {
            out.push(Leaf { path: format!("header:location#{i}"), name: String::new(), text, ty: Ty::Str });
        }
    }
    for (k, v) in u.query_pairs() {
        if !secret_param(&k) && v != SECRET && !v.is_empty() {
            out.push(Leaf {
                path: format!("header:location?{k}"),
                name: k.to_string(),
                text: v.to_string(),
                ty: Ty::Str,
            });
        }
    }
    out
}

/// What accumulates for one link while the capture is read.
struct Acc {
    seen: BTreeSet<usize>,
    status: u16,
    why: Option<&'static str>,
}

type Key = (Kind, usize, String, usize, Place, String);

fn note(accs: &mut IndexMap<Key, Acc>, key: Key, status: u16, i: usize, why: Option<&'static str>) {
    let acc = accs.entry(key).or_insert_with(|| Acc { seen: BTreeSet::new(), status, why: None });
    match why {
        None => {
            acc.seen.insert(i);
        }
        Some(w) => {
            if acc.why.is_none() {
                acc.why = Some(w);
            }
        }
    }
}

/// Whether the endpoint pages through this very parameter.
fn pages(api: &Api, ep: usize, r: &Req) -> bool {
    let want = match r.place {
        Place::Query => r.param.clone(),
        Place::Body => format!("body.{}", r.param),
        _ => return false,
    };
    api.endpoints[ep].paging.as_ref().and_then(|p| p.param.as_deref()) == Some(want.as_str())
}

/// Finds the links of a capture: `all` are its exchanges in order, `api` the endpoints worked out from them (the
/// same `hosts` filter). Returns the links and, by reason, how many candidate links were dropped.
pub fn find(all: &[Exchange], api: &Api, hosts: &[String]) -> (Vec<Link>, IndexMap<&'static str, usize>) {
    let mut cut = 0usize;
    let mut budget = MAX_TOTAL;
    let (kept, _) = noise::sift(all, hosts);
    let mut exs: Vec<Ex> = all.iter().map(|_| Ex { ep: None, reqs: Vec::new() }).collect();
    let mut index: HashMap<String, IndexMap<(usize, String), Src>> = HashMap::new();
    for &i in &kept {
        let x = &all[i];
        let Some(e) = endpoint_of(api, x) else { continue };
        exs[i].ep = Some(e);
        exs[i].reqs = requests(&api.endpoints[e], x, &mut cut, &mut budget);
        if !(200..400).contains(&x.status) {
            continue;
        }
        let mut found = x
            .resp_body
            .as_ref()
            .and_then(|b| b.json.as_ref())
            .map(|v| leaves(v, &mut cut, &mut budget))
            .unwrap_or_default();
        found.extend(location_leaves(x, &api.endpoints[e]));
        for l in found {
            let path = general(&l.path);
            let id = id_name(&l.name);
            index.entry(l.text).or_default().entry((e, path.clone())).or_insert(Src {
                seq: i,
                ep: e,
                path,
                ty: l.ty,
                id,
                status: x.status,
            });
        }
    }

    // a value that is a known secret (by fingerprint) is never a plain link, wherever it sits
    let fps: HashSet<Fp> = all.iter().flat_map(|x| x.sent.iter().chain(&x.got)).map(|s| s.fp).collect();
    let mut secret: HashMap<&str, bool> = HashMap::new();
    // where each value that a response holds is sent
    let mut targets: HashMap<&str, HashSet<(usize, Place, &str)>> = HashMap::new();
    for ex in &exs {
        if let Some(e) = ex.ep {
            for r in &ex.reqs {
                if index.contains_key(&r.text) {
                    let hidden = *secret.entry(r.text.as_str()).or_insert_with(|| fps.contains(&fingerprint(&r.text)));
                    if !hidden {
                        targets.entry(r.text.as_str()).or_default().insert((e, r.place, r.param.as_str()));
                    }
                }
            }
        }
    }
    // the texts each request sends, to tell an echo in O(1)
    let sent: Vec<HashSet<&str>> = exs.iter().map(|x| x.reqs.iter().map(|r| r.text.as_str()).collect()).collect();

    let mut accs: IndexMap<Key, Acc> = IndexMap::new();
    for &i in &kept {
        let Some(e) = exs[i].ep else { continue };
        for r in &exs[i].reqs {
            let Some(srcs) = index.get(&r.text) else { continue };
            if secret.get(r.text.as_str()).copied().unwrap_or(false) {
                continue;
            }
            let places = srcs.len();
            let sent_to = targets.get(r.text.as_str()).map(HashSet::len).unwrap_or(0);
            for s in srcs.values().filter(|s| s.seq < i).take(MAX_SCAN) {
                let why = if s.ty == Ty::Bool || r.ty == Ty::Bool || r.text == "true" || r.text == "false" {
                    Some("boolean")
                } else if r.text.trim().chars().count() < MIN_TEXT && !small_number(&r.text, s.ty) {
                    Some("empty or short")
                } else if small_number(&r.text, s.ty) && !(s.id || id_name(&r.name)) {
                    Some("small number")
                } else if s.ep == e && !pages(api, e, r) {
                    Some("same endpoint")
                } else if sent[s.seq].contains(r.text.as_str()) {
                    Some("echo")
                } else if places > MAX_SOURCES || sent_to > MAX_TARGETS {
                    Some("too common")
                } else {
                    None
                };
                note(&mut accs, (Kind::Value, s.ep, s.path.clone(), e, r.place, r.param.clone()), s.status, i, why);
            }
        }
    }

    // secrets: followed by fingerprint, never by value
    let mut handed: HashMap<Fp, (usize, Option<usize>, u16, String)> = HashMap::new();
    for (i, x) in all.iter().enumerate() {
        for s in &x.got {
            let path = match s.place {
                Spot::Body => general(&s.name),
                Spot::Header => format!("header:{}", s.name),
                Spot::Cookie => format!("cookie:{}", s.name),
                Spot::Query => continue,
            };
            handed.entry(s.fp).or_insert((i, exs[i].ep, x.status, path));
        }
    }
    let mut lost: HashMap<String, usize> = HashMap::new();
    for &i in &kept {
        let Some(e) = exs[i].ep else { continue };
        for s in &all[i].sent {
            let Some((seq, from, status, path)) = handed.get(&s.fp) else { continue };
            if *seq >= i {
                continue;
            }
            let (place, param) = match s.place {
                Spot::Body => (Place::Body, short(&general(&s.name))),
                Spot::Header => (Place::Header, s.name.clone()),
                Spot::Cookie => (Place::Cookie, s.name.clone()),
                Spot::Query => (Place::Query, s.name.clone()),
            };
            match from {
                None => {
                    *lost.entry(format!("{e}|{}|{param}", place.name())).or_insert(0) += 1;
                }
                Some(f) => {
                    let why = if *f == e { Some("same endpoint") } else { None };
                    note(&mut accs, (Kind::Auth, *f, path.clone(), e, place, param), *status, i, why);
                }
            }
        }
    }

    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    *counts.entry("source is not an endpoint").or_insert(0) += lost.len();
    if cut > 0 {
        *counts.entry("over the cap").or_insert(0) += cut;
    }
    let mut links = Vec::new();
    for ((kind, from, path, to, place, param), acc) in accs {
        match (acc.seen.first(), acc.why) {
            (Some(first), _) => links.push(Link {
                kind,
                from: api.endpoints[from].id.clone(),
                path,
                status: acc.status,
                to: api.endpoints[to].id.clone(),
                place,
                param,
                seen: acc.seen.len(),
                first: *first,
            }),
            (None, why) => *counts.entry(why.unwrap_or("other")).or_insert(0) += 1,
        }
    }
    links.sort_by_key(|l| l.first);
    if links.len() > MAX_LINKS {
        *counts.entry("over the cap").or_insert(0) += links.len() - MAX_LINKS;
        links.truncate(MAX_LINKS);
    }
    let mut ignored = IndexMap::new();
    for r in REASONS {
        if let Some(n) = counts.get(r).filter(|n| **n > 0) {
            ignored.insert(r, *n);
        }
    }
    (links, ignored)
}

// ---------------------------------------------------------------- showing them

fn find_ep<'a>(api: &'a Api, id: &str) -> Option<&'a Endpoint> {
    api.endpoints.iter().find(|e| e.id == id)
}

/// `GET /api/items`.
fn name_of(api: &Api, id: &str) -> String {
    find_ep(api, id).map(|e| format!("{} {}", e.method, e.path)).unwrap_or_else(|| id.to_string())
}

/// `item_id from GET /api/items items[].id; auth from POST /api/login access_token`, for the summary: one short
/// line for an endpoint, `None` when nothing feeds it.
pub fn endpoint_line(api: &Api, e: &Endpoint) -> Option<String> {
    let mut seen: Vec<String> = Vec::new();
    for l in api.links.iter().filter(|l| l.to == e.id) {
        let label = if l.kind == Kind::Auth { "auth".to_string() } else { l.arg() };
        let item = format!("{label} from {} {}", name_of(api, &l.from), l.from_short());
        if !seen.contains(&item) {
            seen.push(item);
        }
    }
    if seen.is_empty() {
        return None;
    }
    let more = seen.len().saturating_sub(MAX_LINE);
    seen.truncate(MAX_LINE);
    let tail = if more > 0 { format!("; …+{more}") } else { String::new() };
    Some(format!("  links: {}{}", seen.join("; "), tail))
}

/// How many candidate links were dropped, with the reasons: `3 ignored (2 small number, 1 echo)`.
pub fn ignored_text(api: &Api) -> String {
    let total: usize = api.links_ignored.values().sum();
    if total == 0 {
        return "0 ignored".to_string();
    }
    let by: Vec<String> = api.links_ignored.iter().map(|(r, n)| format!("{n} {r}")).collect();
    format!("{total} ignored ({})", by.join(", "))
}

/// The line of the summary header: `links: 4 found (1 auth), 3 ignored (2 small number, 1 echo)`; empty when the
/// capture gave neither.
pub fn header_line(api: &Api) -> String {
    if api.links.is_empty() && api.links_ignored.is_empty() {
        return String::new();
    }
    let auth = api.links.iter().filter(|l| l.kind == Kind::Auth).count();
    format!("links: {} found ({} auth), {}\n", api.links.len(), auth, ignored_text(api))
}

/// The chains of a map as text: how to log in and what the token opens, what a list hands to a detail call, how a
/// list pages, and how many candidates were dropped.
pub fn flows(api: &Api) -> String {
    let mut out = format!("flows: {} links, {}\n", api.links.len(), ignored_text(api));
    let mut groups: IndexMap<(Kind, String, String), Vec<&Link>> = IndexMap::new();
    for l in &api.links {
        groups.entry((l.kind, l.from.clone(), l.path.clone())).or_default().push(l);
    }
    let mut any = false;
    for kind in [Kind::Auth, Kind::Value] {
        for ((k, from, path), ls) in &groups {
            if *k != kind {
                continue;
            }
            any = true;
            let mut to: Vec<String> = Vec::new();
            for l in ls {
                let item = if kind == Kind::Auth {
                    format!("{} (x{})", find_ep(api, &l.to).map(|e| e.id.as_str()).unwrap_or(&l.to), l.seen)
                } else {
                    format!("{} {} ({} x{})", name_of(api, &l.to), l.place.name(), l.arg(), l.seen)
                };
                if !to.contains(&item) {
                    to.push(item);
                }
            }
            let more = to.len().saturating_sub(MAX_FLOW);
            to.truncate(MAX_FLOW);
            let tail = if more > 0 { format!(", …+{more}") } else { String::new() };
            let label = match kind {
                Kind::Auth => "login -> token -> calls",
                Kind::Value if path.starts_with("header:") => "created -> item",
                Kind::Value if path.contains("[]") => "list -> item",
                Kind::Value => "value -> call",
            };
            out.push_str(&format!("{label}: {} {} -> {}{}\n", name_of(api, from), short(path), to.join(", "), tail));
        }
    }
    for e in &api.endpoints {
        if let Some(p) = &e.paging {
            any = true;
            out.push_str(&format!("page -> next: {} {}: {}\n", e.method, e.path, p.describe()));
        }
    }
    if !any {
        out.push_str("no chains found\n");
    }
    out
}

// ---------------------------------------------------------------- OpenAPI

/// `$.items[].id` as the JSON pointer `/items/0/id` (the first item).
fn pointer(path: &str) -> String {
    let mut out = String::new();
    for part in short(path).split('.') {
        let (name, arrays) = match part.find('[') {
            Some(i) => (&part[..i], part[i..].matches("[]").count()),
            None => (part, 0),
        };
        if !name.is_empty() {
            out.push('/');
            out.push_str(&name.replace('~', "~0").replace('/', "~1"));
        }
        for _ in 0..arrays {
            out.push_str("/0");
        }
    }
    out
}

/// A runtime expression for where a link's value is in the response.
fn expression(l: &Link) -> String {
    if let Some(h) = l.path.strip_prefix("header:") {
        let name = h.split(['#', '?']).next().unwrap_or(h);
        return format!("$response.header.{name}");
    }
    if l.path.starts_with("cookie:") {
        return "$response.header.set-cookie".to_string();
    }
    format!("$response.body#{}", pointer(&l.path))
}

fn component_name(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect()
}

/// The OpenAPI Link Objects of one endpoint's responses: (status, name, object). They go on the response the value
/// was seen in.
pub fn openapi(api: &Api, e: &Endpoint) -> Vec<(u16, String, Value)> {
    let mut out: Vec<(u16, String, Value)> = Vec::new();
    for l in api.links.iter().filter(|l| l.from == e.id) {
        let base = if l.kind == Kind::Auth { format!("auth_{}", l.to) } else { l.to.clone() };
        let mut name = component_name(&base);
        if out.iter().any(|(s, n, _)| *s == l.status && *n == name) {
            name = component_name(&format!("{base}_{}", l.param));
        }
        let mut n = 2;
        let stem = name.clone();
        while out.iter().any(|(s, x, _)| *s == l.status && *x == name) {
            name = format!("{stem}_{n}");
            n += 1;
        }
        let expr = expression(l);
        let mut o = Map::new();
        o.insert("operationId".into(), json!(l.to));
        match l.place {
            Place::Body => {
                o.insert("x-nexo-body".into(), json!({ (l.param.clone()): expr }));
            }
            p => {
                o.insert("parameters".into(), json!({ (format!("{}.{}", p.name(), l.param)): expr }));
            }
        }
        let mut note = format!("seen {} time{}", l.seen, if l.seen == 1 { "" } else { "s" });
        if let Some((_, seg)) = l.path.split_once('#') {
            note.push_str(&format!("; path segment {seg} of the location"));
        } else if let Some((_, q)) = l.path.split_once('?') {
            note.push_str(&format!("; query value {q} of the location"));
        }
        o.insert("description".into(), json!(note));
        o.insert("x-nexo-kind".into(), json!(l.kind.name()));
        out.push((l.status, name, Value::Object(o)));
    }
    out
}

// ---------------------------------------------------------------- next steps of a call

/// A call that the answer just read makes possible.
#[derive(Debug, Clone, PartialEq)]
pub struct Next {
    pub kind: Kind,
    /// The endpoint ids it opens (one for a value, several for an auth link).
    pub to: Vec<String>,
    /// `-p` arguments with their values; empty for auth.
    pub params: Vec<(String, String)>,
    /// Where in this answer they come from (`items[].id`).
    pub from: String,
    /// The endpoint the answer came from.
    pub source: String,
    /// How often the link was seen in the capture.
    pub seen: usize,
}

/// What an answer makes possible: the steps (value steps first, the most seen first, then auth), and how many
/// candidate steps could not be made (a value that is not in this answer, a target with path parameters the answer
/// does not hold).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Steps {
    pub list: Vec<Next>,
    pub skipped: usize,
}

/// The first value at a generalized path of an answer, or in its `Location`.
fn value_at(body: Option<&Value>, location: Option<&str>, path: &str) -> Option<String> {
    if let Some(h) = path.strip_prefix("header:location") {
        let loc = location?;
        let u = url::Url::parse("http://relative.invalid/").and_then(|b| b.join(loc.trim())).ok()?;
        if let Some(n) = h.strip_prefix('#') {
            let n: usize = n.parse().ok()?;
            return u.path().split('/').filter(|s| !s.is_empty()).nth(n).map(decode);
        }
        let name = h.strip_prefix('?')?;
        if secret_param(name) || name.eq_ignore_ascii_case("code") {
            return None;
        }
        return u.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());
    }
    let mut rest = path.strip_prefix('$')?;
    let mut cur = body?;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("[]") {
            cur = cur.as_array()?.first()?;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('.') {
            let end = r.find(['.', '[']).unwrap_or(r.len());
            cur = cur.get(&r[..end])?;
            rest = &r[end..];
        } else {
            return None;
        }
    }
    match cur {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The calls the answer of endpoint `id` makes possible, with the values filled in from `body` (an answer already
/// scrubbed of secrets) and `location`. A secret is never a value here: an auth link says where the secret comes from.
pub fn next(api: &Api, id: &str, body: Option<&Value>, location: Option<&str>) -> Steps {
    let mut steps = Steps::default();
    let mut by: IndexMap<String, Next> = IndexMap::new();
    let mut auth: IndexMap<String, Next> = IndexMap::new();
    for l in api.links.iter().filter(|l| l.from == id) {
        if l.kind == Kind::Auth {
            let n = auth.entry(l.path.clone()).or_insert_with(|| Next {
                kind: Kind::Auth,
                to: Vec::new(),
                params: Vec::new(),
                from: short(&l.path),
                source: id.to_string(),
                seen: 0,
            });
            if !n.to.contains(&l.to) {
                n.to.push(l.to.clone());
            }
            n.seen += l.seen;
            continue;
        }
        let key_secret = l.path.rsplit('.').next().is_some_and(secret_name);
        let value = value_at(body, location, &l.path).filter(|v| v != SECRET && !key_secret);
        let arg = match l.place {
            Place::Path | Place::Query => Some(l.arg()),
            Place::Body if !l.param.contains("[]") => Some(l.arg()),
            _ => None,
        };
        let (Some(value), Some(arg)) = (value, arg) else {
            steps.skipped += 1;
            continue;
        };
        let n = by.entry(l.to.clone()).or_insert_with(|| Next {
            kind: Kind::Value,
            to: vec![l.to.clone()],
            params: Vec::new(),
            from: l.from_short(),
            source: id.to_string(),
            seen: 0,
        });
        if !n.params.iter().any(|(a, _)| *a == arg) {
            n.params.push((arg, value));
            n.seen += l.seen;
        }
    }
    let mut values: Vec<Next> = Vec::new();
    for (to, n) in by {
        let covered = find_ep(api, &to)
            .map(|e| e.path_params.iter().all(|p| n.params.iter().any(|(a, _)| *a == p.name)))
            .unwrap_or(false);
        if covered {
            values.push(n);
        } else {
            steps.skipped += 1;
        }
    }
    values.sort_by_key(|n| std::cmp::Reverse(n.seen));
    steps.list = values;
    steps.list.extend(auth.into_values());
    steps
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Options;

    const TOKEN: &str = "tkAAAA1111bbbbCCCC2222dddd";
    const PASSWORD: &str = "hunter2hunter2";

    /// A HAR of `(method, url, request headers, request body, status, response body)`.
    #[allow(clippy::type_complexity)]
    fn har(list: &[(&str, &str, &[(&str, &str)], Option<&str>, u16, &str)]) -> String {
        let entries: Vec<Value> = list
            .iter()
            .map(|(method, url, headers, body, status, resp)| {
                let hs: Vec<Value> = headers.iter().map(|(n, v)| json!({ "name": n, "value": v })).collect();
                let mut request = json!({ "method": method, "url": url, "headers": hs });
                if let Some(b) = body {
                    request["postData"] = json!({ "mimeType": "application/json", "text": b });
                }
                json!({
                    "_resourceType": "xhr",
                    "request": request,
                    "response": { "status": status, "headers": [],
                        "content": { "mimeType": "application/json", "text": resp } },
                })
            })
            .collect();
        json!({ "log": { "entries": entries } }).to_string()
    }

    fn analyze(text: &str) -> Api {
        crate::analyze_har(text, &Options::default()).unwrap()
    }

    fn list_detail() -> Api {
        analyze(&har(&[
            (
                "GET",
                "https://shop.test/api/items?limit=10",
                &[],
                None,
                200,
                r#"{"items":[{"id":4711,"name":"Lamp"},{"id":4712,"name":"Desk"}],"total":2}"#,
            ),
            ("GET", "https://shop.test/api/items/4711", &[], None, 200, r#"{"id":4711,"name":"Lamp","price":12}"#),
            ("GET", "https://shop.test/api/items/4712", &[], None, 200, r#"{"id":4712,"name":"Desk","price":80}"#),
        ]))
    }

    #[test]
    fn a_list_hands_its_ids_to_the_detail_call() {
        let api = list_detail();
        let l = api.links.iter().find(|l| l.to == "get_api_items_item_id").expect("a link to the detail");
        assert_eq!((l.kind, l.from.as_str(), l.path.as_str()), (Kind::Value, "get_api_items", "$.items[].id"));
        assert_eq!((l.place, l.param.as_str(), l.seen, l.first), (Place::Path, "item_id", 2, 1));
        // the detail call only echoes the id it was given, so it is not a source
        assert!(api.links.iter().all(|l| l.from != "get_api_items_item_id"), "{:?}", api.links);
        let line = endpoint_line(&api, find_ep(&api, "get_api_items_item_id").unwrap()).unwrap();
        assert_eq!(line, "  links: item_id from GET /api/items items[].id");
    }

    #[test]
    fn a_logins_token_is_an_auth_link_and_no_value_is_kept() {
        let bearer = format!("Bearer {TOKEN}");
        let login = format!(r#"{{"email":"ann@example.com","password":"{PASSWORD}"}}"#);
        let resp = format!(r#"{{"access_token":"{TOKEN}","user":{{"name":"Ann"}}}}"#);
        let h: &[(&str, &str)] = &[("authorization", bearer.as_str())];
        let api = analyze(&har(&[
            ("POST", "https://app.test/api/login", &[], Some(login.as_str()), 200, resp.as_str()),
            ("GET", "https://app.test/api/me", h, None, 200, r#"{"name":"Ann"}"#),
            ("GET", "https://app.test/api/orders", h, None, 200, r#"{"orders":[]}"#),
        ]));
        let auth: Vec<&Link> = api.links.iter().filter(|l| l.kind == Kind::Auth).collect();
        assert_eq!(auth.len(), 2, "{:?}", api.links);
        for l in &auth {
            assert_eq!((l.from.as_str(), l.path.as_str()), ("post_api_login", "$.access_token"));
            assert_eq!((l.place, l.param.as_str()), (Place::Header, "authorization"));
        }
        // nothing that prints holds the token or the password
        let steps = next(&api, "post_api_login", Some(&json!({"access_token": SECRET})), None);
        let outs = [
            format!("{:?}", api.links),
            crate::store::to_text(&api),
            crate::summary::render(&api, 6000),
            flows(&api),
            openapi_text(&api),
            format!("{steps:?}"),
        ];
        for out in &outs {
            for secret in [TOKEN, PASSWORD] {
                assert!(!out.contains(secret), "{secret} leaked in {out}");
            }
        }
        assert_eq!(steps.list.len(), 1);
        assert_eq!(steps.list[0].kind, Kind::Auth);
        assert!(steps.list[0].params.is_empty());
        assert!(flows(&api).contains("login -> token -> calls: POST /api/login access_token -> get_api_me (x1)"));
    }

    fn openapi_text(api: &Api) -> String {
        crate::openapi::to_openapi(api, "t").to_string()
    }

    #[test]
    fn a_cursor_goes_from_a_page_to_the_next_request_of_the_same_endpoint() {
        let api = analyze(&har(&[
            (
                "GET",
                "https://app.test/api/feed?limit=20",
                &[],
                None,
                200,
                r#"{"items":[{"id":5001},{"id":5002}],"next_cursor":"cur_page2_aB3x"}"#,
            ),
            (
                "GET",
                "https://app.test/api/feed?limit=20&cursor=cur_page2_aB3x",
                &[],
                None,
                200,
                r#"{"items":[{"id":5003}],"next_cursor":"cur_page3_zZ9q"}"#,
            ),
            (
                "GET",
                "https://app.test/api/feed?limit=20&cursor=cur_page3_zZ9q",
                &[],
                None,
                200,
                r#"{"items":[],"next_cursor":null}"#,
            ),
        ]));
        let l = api.links.iter().find(|l| l.param == "cursor").expect("a cursor link");
        assert_eq!(
            (l.from.as_str(), l.to.as_str(), l.path.as_str()),
            ("get_api_feed", "get_api_feed", "$.next_cursor")
        );
        assert_eq!((l.place, l.seen, l.first), (Place::Query, 2, 1));
        assert!(flows(&api).contains("page -> next: GET /api/feed"), "{}", flows(&api));
        let steps = next(&api, "get_api_feed", Some(&json!({"items": [], "next_cursor": "cur_page4_q"})), None);
        assert_eq!(steps.list.len(), 1);
        assert_eq!(steps.list[0].params, vec![("cursor".to_string(), "cur_page4_q".to_string())]);
    }

    #[test]
    fn a_false_link_is_ignored_and_counted() {
        let api = analyze(&har(&[
            ("GET", "https://app.test/api/stats", &[], None, 200, r#"{"count":1,"active":true}"#),
            ("GET", "https://app.test/api/search?page=1&flag=true", &[], None, 200, r#"{"results":[]}"#),
        ]));
        assert!(api.links.is_empty(), "{:?}", api.links);
        assert_eq!(api.links_ignored.get("small number"), Some(&1), "{:?}", api.links_ignored);
        assert_eq!(api.links_ignored.get("boolean"), Some(&1), "{:?}", api.links_ignored);
        assert_eq!(ignored_text(&api), "2 ignored (1 boolean, 1 small number)");
        assert!(crate::summary::render(&api, 3000).contains("links: 0 found (0 auth), 2 ignored"));
    }

    #[test]
    fn a_small_number_is_a_link_when_its_field_is_an_id() {
        let api = analyze(&har(&[
            ("GET", "https://app.test/api/teams", &[], None, 200, r#"{"teams":[{"id":7,"name":"red"}]}"#),
            ("GET", "https://app.test/api/teams/7", &[], None, 200, r#"{"id":7,"name":"red"}"#),
        ]));
        assert!(api.links.iter().any(|l| l.from == "get_api_teams" && l.param.ends_with("id")), "{:?}", api.links);
    }

    #[test]
    fn links_survive_a_save_and_an_old_map_without_them_still_loads() {
        let api = list_detail();
        let text = crate::store::to_text(&api);
        let back = crate::store::from_text(&text).unwrap();
        assert_eq!(back.links, api.links);
        assert_eq!(back.links_ignored, api.links_ignored);
        let mut v: Value = serde_json::from_str(&text).unwrap();
        let o = v.as_object_mut().unwrap();
        o.remove("links");
        o.remove("links_ignored");
        let old = crate::store::from_json(&v).unwrap();
        assert!(old.links.is_empty() && old.links_ignored.is_empty());
        assert!(!crate::summary::render(&old, 3000).contains("links:"));
    }

    #[test]
    fn openapi_has_link_objects_on_the_source_response() {
        let oa = crate::openapi::to_openapi(&list_detail(), "t");
        let link = &oa["paths"]["/api/items"]["get"]["responses"]["200"]["links"]["get_api_items_item_id"];
        assert_eq!(link["operationId"], "get_api_items_item_id");
        assert_eq!(link["parameters"]["path.item_id"], "$response.body#/items/0/id");
    }

    #[test]
    fn the_summary_says_where_the_values_of_an_endpoint_come_from() {
        let s = crate::summary::render(&list_detail(), 4000);
        assert!(s.contains("item_id from GET /api/items items[].id"), "{s}");
        assert!(s.contains("links: 1 found (0 auth), 0 ignored"), "{s}");
    }

    #[test]
    fn the_next_steps_fill_the_value_in_and_skip_what_the_answer_lacks() {
        let api = list_detail();
        let body = json!({"items": [{"id": 9001, "name": "x"}], "total": 1});
        let steps = next(&api, "get_api_items", Some(&body), None);
        assert_eq!(steps.list.len(), 1);
        assert_eq!(steps.list[0].to, vec!["get_api_items_item_id".to_string()]);
        assert_eq!(steps.list[0].params, vec![("item_id".to_string(), "9001".to_string())]);
        let empty = next(&api, "get_api_items", Some(&json!({"items": []})), None);
        assert!(empty.list.is_empty());
        assert_eq!(empty.skipped, 1);
    }

    #[test]
    fn a_created_resource_hands_its_location_to_the_next_call() {
        let (ok, resp) = (json!({}).to_string(), r#"{"ok":true}"#);
        let mut text: Value = serde_json::from_str(&har(&[
            ("POST", "https://app.test/api/notes", &[], Some(ok.as_str()), 201, resp),
            ("GET", "https://app.test/api/notes/note-48213", &[], None, 200, r#"{"title":"hi"}"#),
            ("GET", "https://app.test/api/notes/note-48214", &[], None, 200, r#"{"title":"yo"}"#),
        ]))
        .unwrap();
        text["log"]["entries"][0]["response"]["headers"] =
            json!([{ "name": "Location", "value": "/api/notes/note-48213" }]);
        let api = analyze(&text.to_string());
        let l = api.links.iter().find(|l| l.path.starts_with("header:location")).expect("a link from the location");
        assert_eq!((l.from.as_str(), l.place, l.param.as_str()), ("post_api_notes", Place::Path, "note_id"));
        assert_eq!(l.path, "header:location#2");
        let steps = next(&api, "post_api_notes", None, Some("/api/notes/note-48299"));
        assert_eq!(steps.list[0].params[0].1, "note-48299");
    }
}
