//! Calling a saved map without a browser: finds the endpoint, fills its path, query and body from `name=value` pairs,
//! and puts each auth scheme the endpoint uses in the place the traffic had it. Nothing here touches the network or
//! the disk. A secret value (read from an environment variable by the caller, or taken from a login response) lives
//! in a [`Val`], which prints as `<secret>` in every `Debug` and in [`Request::render`]; only [`Val::reveal`] gives
//! it back, and only the code that sends the request calls that.

use crate::auth::{self, Login, Scheme};
use crate::exchange::{secret_name, secret_param, SECRET};
use crate::shape::Shape;
use crate::spec::{Api, Endpoint};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::time::Duration;

/// The shortest wait between two calls of `nexo api watch`.
pub const MIN_EVERY: Duration = Duration::from_secs(10);

/// How many bytes of a text answer are shown.
pub const SHOW_BYTES: usize = 4096;

// ---------------------------------------------------------------- finding an endpoint

/// Whether a concrete path fits a templated one: `/items/{item_id}` takes `/items/42`. A segment shown as `<opaque>`
/// (how an analysis writes an id it hid) fits any template segment.
pub fn path_matches(template: &str, concrete: &str) -> bool {
    let t: Vec<&str> = template.split('/').collect();
    let c: Vec<&str> = concrete.split('/').collect();
    t.len() == c.len()
        && t.iter().zip(&c).all(|(t, c)| t == c || (t.starts_with('{') && t.ends_with('}')) || *c == "<opaque>")
}

/// The endpoint a person means: its id (`get_api_items`), or `METHOD /path` (the templated path, or a real one).
pub fn find<'a>(api: &'a Api, key: &str) -> Result<&'a Endpoint, String> {
    let key = key.trim();
    if let Some(e) = api.endpoints.iter().find(|e| e.id == key) {
        return Ok(e);
    }
    if let Some((method, path)) = key.split_once(char::is_whitespace) {
        let method = method.to_ascii_uppercase();
        let path = path.trim();
        let exact: Vec<&Endpoint> = api.endpoints.iter().filter(|e| e.method == method && e.path == path).collect();
        let loose: Vec<&Endpoint> =
            api.endpoints.iter().filter(|e| e.method == method && path_matches(&e.path, path)).collect();
        for hits in [exact, loose] {
            match hits.len() {
                0 => {}
                1 => return Ok(hits[0]),
                _ => {
                    let ids: Vec<&str> = hits.iter().map(|e| e.id.as_str()).collect();
                    return Err(format!("{key:?} is more than one endpoint, name one by id: {}", ids.join(", ")));
                }
            }
        }
    }
    let ids: Vec<&str> = api.endpoints.iter().take(8).map(|e| e.id.as_str()).collect();
    let more = if api.endpoints.len() > 8 { format!(", and {} more", api.endpoints.len() - 8) } else { String::new() };
    Err(format!("no endpoint {key:?} in this map; ids: {}{more} (`nexo api describe MAP` lists all)", ids.join(", ")))
}

// ---------------------------------------------------------------- secrets and where they are read

/// A value that may be a secret: `reveal` gives it, everything that prints shows `shown`.
#[derive(Clone)]
pub struct Val {
    text: String,
    shown: String,
}

impl Val {
    pub fn plain(text: &str) -> Val {
        Val { text: text.to_string(), shown: text.to_string() }
    }

    /// A secret; `prefix` stays visible (`Bearer`): it shows as `Bearer <secret>`.
    pub fn secret(text: String, prefix: &str) -> Val {
        let shown = if prefix.is_empty() { SECRET.to_string() } else { format!("{prefix} {SECRET}") };
        Val { text, shown }
    }

    /// The real value. Only the code that sends the request may call this.
    pub fn reveal(&self) -> &str {
        &self.text
    }

    pub fn shown(&self) -> &str {
        &self.shown
    }
}

impl fmt::Debug for Val {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.shown)
    }
}

fn base_label(s: &Scheme) -> String {
    match s {
        Scheme::Bearer { header } if header == "authorization" => "bearer".to_string(),
        Scheme::Bearer { header } => format!("bearer:{header}"),
        Scheme::Basic { header } if header == "authorization" => "basic".to_string(),
        Scheme::Basic { header } => format!("basic:{header}"),
        Scheme::ApiKeyHeader { name } => format!("header:{name}"),
        Scheme::ApiKeyQuery { name } => format!("query:{name}"),
        Scheme::Cookie { .. } => "cookie".to_string(),
        Scheme::Csrf { header, .. } => format!("csrf:{header}"),
    }
}

/// The name of each scheme of the map (`bearer`, `header:x-api-key`, `cookie`), as `describe` shows it and
/// `--auth NAME=ENVVAR` takes it. Two schemes with the same name get `#N` (their place in the map, from 1).
pub fn labels(api: &Api) -> Vec<String> {
    let base: Vec<String> = api.auth.iter().map(|a| base_label(&a.scheme)).collect();
    base.iter()
        .enumerate()
        .map(|(i, l)| if base.iter().filter(|b| *b == l).count() > 1 { format!("{l}#{}", i + 1) } else { l.clone() })
        .collect()
}

/// The environment variable `call` reads for a scheme when `--auth` does not name one: `NEXO_API_BEARER`,
/// `NEXO_API_HEADER_X_API_KEY`.
pub fn env_name(label: &str) -> String {
    let mut out = String::from("NEXO_API_");
    for c in label.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_end_matches('_').to_string()
}

/// Reads `--auth NAME=ENVVAR` pairs into (scheme index, variable name). NAME is a label, or what follows the `:` of
/// one (`x-api-key` for `header:x-api-key`) when that is not ambiguous.
pub fn auth_map(api: &Api, pairs: &[String]) -> Result<Vec<(usize, String)>, String> {
    let names = labels(api);
    let mut out = Vec::new();
    for p in pairs {
        let (name, var) = p.split_once('=').ok_or_else(|| format!("--auth {p:?}: write NAME=ENVVAR"))?;
        let (name, var) = (name.trim(), var.trim());
        if var.is_empty() {
            return Err(format!("--auth {p:?}: the variable name is empty"));
        }
        let by_label: Vec<usize> = (0..names.len()).filter(|i| names[*i].eq_ignore_ascii_case(name)).collect();
        let hits = if by_label.is_empty() {
            (0..names.len())
                .filter(|i| names[*i].split_once(':').is_some_and(|(_, rest)| rest.eq_ignore_ascii_case(name)))
                .collect()
        } else {
            by_label
        };
        match hits.as_slice() {
            [i] => out.push((*i, var.to_string())),
            [] => return Err(format!("--auth {name}: no such scheme; this map has: {}", names.join(", "))),
            _ => return Err(format!("--auth {name} is ambiguous; use one of: {}", names.join(", "))),
        }
    }
    Ok(out)
}

/// One auth scheme an endpoint uses.
#[derive(Debug, Clone)]
pub struct Need {
    /// Index into [`Api::auth`].
    pub index: usize,
    pub label: String,
    /// The variable read when `--auth` names none.
    pub env: String,
    /// The call that hands the secret out, when the traffic showed one.
    pub login: Option<Login>,
}

/// The schemes an endpoint uses.
pub fn needs(api: &Api, ep: &Endpoint) -> Vec<Need> {
    let names = labels(api);
    ep.auth
        .iter()
        .filter_map(|&i| {
            let a = api.auth.get(i)?;
            Some(Need {
                index: i,
                label: names[i].clone(),
                env: env_name(&names[i]),
                login: a.source.as_deref().and_then(auth::login_of),
            })
        })
        .collect()
}

/// The login endpoint of a [`Login`], if the map has it.
pub fn login_endpoint<'a>(api: &'a Api, l: &Login) -> Option<&'a Endpoint> {
    let all: Vec<&Endpoint> = api.endpoints.iter().filter(|e| e.gql.is_none() && e.method == l.method).collect();
    all.iter()
        .copied()
        .find(|e| e.path == l.path)
        .or_else(|| all.iter().copied().find(|e| path_matches(&e.path, &l.path)))
}

/// Where each scheme of an endpoint gets its value, in this order: the variable `--auth` names, a token the login
/// call returned (`logged`, by scheme index), the default variable. Returns the values found and the names of the
/// variables that were asked for and are not set. `lookup` reads an environment variable.
pub fn auth_values(
    api: &Api,
    ep: &Endpoint,
    explicit: &[(usize, String)],
    logged: &BTreeMap<usize, String>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> (Vec<(usize, String)>, Vec<String>) {
    let mut found = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for n in needs(api, ep) {
        let named = explicit.iter().rev().find(|(i, _)| *i == n.index).map(|(_, v)| v.as_str());
        let value = match named {
            Some(var) => lookup(var).filter(|v| !v.is_empty()).ok_or_else(|| var.to_string()),
            None => match logged.get(&n.index) {
                Some(v) => Ok(v.clone()),
                None => lookup(&n.env).filter(|v| !v.is_empty()).ok_or_else(|| n.env.clone()),
            },
        };
        match value {
            Ok(v) => found.push((n.index, v)),
            Err(var) => {
                if !missing.contains(&var) {
                    missing.push(var);
                }
            }
        }
    }
    (found, missing)
}

// ---------------------------------------------------------------- parameters

/// What the person gives: `-p name=value` pairs and a whole `--body`.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub params: Vec<(String, String)>,
    pub body: Option<Value>,
}

/// Reads one `-p name=value`. A value written `@env:NAME` is read from that environment variable, so a password does
/// not have to be typed on the command line.
pub fn parse_param(arg: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<(String, String), String> {
    let (name, value) = arg.split_once('=').ok_or_else(|| format!("-p {arg:?}: write name=value"))?;
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("-p {arg:?}: the name is empty"));
    }
    match value.strip_prefix("@env:") {
        Some(var) => match lookup(var) {
            Some(v) => Ok((name.to_string(), v)),
            None => Err(format!("-p {name}: the variable {var} is not set")),
        },
        None => Ok((name.to_string(), value.to_string())),
    }
}

enum Slot {
    Path(String),
    Query(String),
    Body(String),
}

/// The shape of the body, which for a GraphQL operation is the shape of its variables.
fn body_shape(ep: &Endpoint) -> Option<&Shape> {
    ep.body.as_ref().map(|b| &b.shape).filter(|s| s.objects > 0)
}

fn shape_at<'a>(s: &'a Shape, path: &str) -> Option<&'a Shape> {
    let mut cur = s;
    for part in path.split('.') {
        cur = cur.fields.get(part)?;
    }
    Some(cur)
}

/// The query names an endpoint's auth fills (`?api_key=`).
fn auth_query_names<'a>(api: &'a Api, ep: &Endpoint) -> Vec<&'a str> {
    ep.auth
        .iter()
        .filter_map(|&i| match &api.auth.get(i)?.scheme {
            Scheme::ApiKeyQuery { name } => Some(name.as_str()),
            _ => None,
        })
        .collect()
}

fn known_names(api: &Api, ep: &Endpoint) -> String {
    let skip = auth_query_names(api, ep);
    let mut names: Vec<String> = ep.path_params.iter().map(|p| format!("{} (path)", p.name)).collect();
    names.extend(ep.query.iter().filter(|p| !skip.contains(&p.name.as_str())).map(|p| format!("{} (query)", p.name)));
    if let Some(s) = body_shape(ep) {
        names.extend(s.fields.keys().map(|k| format!("{k} (body)")));
    }
    if names.is_empty() {
        "this endpoint takes no parameters (use query.NAME or body.NAME to add one)".to_string()
    } else {
        format!("this endpoint takes: {}", names.join(", "))
    }
}

fn slot(api: &Api, ep: &Endpoint, name: &str) -> Result<Slot, String> {
    if let Some(r) = name.strip_prefix("query.") {
        return Ok(Slot::Query(r.to_string()));
    }
    if let Some(r) = name.strip_prefix("body.") {
        return Ok(Slot::Body(r.to_string()));
    }
    if ep.path_params.iter().any(|p| p.name == name) {
        return Ok(Slot::Path(name.to_string()));
    }
    if ep.query.iter().any(|p| p.name == name) {
        return Ok(Slot::Query(name.to_string()));
    }
    if body_shape(ep).is_some_and(|s| shape_at(s, name).is_some()) {
        return Ok(Slot::Body(name.to_string()));
    }
    Err(format!("unknown parameter {name:?}: {}", known_names(api, ep)))
}

/// The value of a body field, typed by what the map saw there: a number where only numbers were seen, a boolean, JSON
/// for an array or an object, a string otherwise.
fn typed(shape: Option<&Shape>, raw: &str) -> Value {
    let text = || Value::String(raw.to_string());
    let Some(s) = shape else { return text() };
    if s.strs > 0 {
        return text();
    }
    if s.ints + s.nums > 0 && s.bools + s.arrays + s.objects == 0 {
        if let Ok(n) = raw.trim().parse::<i64>() {
            return Value::from(n);
        }
        if let Some(n) = raw.trim().parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
            return Value::Number(n);
        }
    }
    if s.bools > 0 && s.ints + s.nums + s.arrays + s.objects == 0 {
        match raw.trim() {
            "true" => return Value::Bool(true),
            "false" => return Value::Bool(false),
            _ => {}
        }
    }
    if s.arrays + s.objects > 0 && s.ints + s.nums + s.bools == 0 {
        if let Ok(v) = serde_json::from_str::<Value>(raw) {
            return v;
        }
    }
    text()
}

fn set_path(doc: &mut Value, path: &str, v: Value) -> Result<(), String> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut cur = doc;
    for (i, part) in parts.iter().enumerate() {
        let Value::Object(m) = cur else {
            return Err(format!("cannot set {path}: the body is not a JSON object"));
        };
        if i + 1 == parts.len() {
            m.insert(part.to_string(), v);
            return Ok(());
        }
        cur = m.entry(part.to_string()).or_insert_with(|| Value::Object(Map::new()));
    }
    Ok(())
}

/// One example value of a shape for a message; none when it is a placeholder.
fn example_of(s: &Shape) -> Option<String> {
    match s.example.as_ref()? {
        Value::String(t) if t == SECRET || t == "<opaque>" => None,
        Value::String(t) => Some(t.clone()),
        other => Some(other.to_string()),
    }
}

fn describe_missing(name: &str, place: &str, s: &Shape) -> String {
    match example_of(s) {
        Some(e) => format!("{name} ({place}, e.g. {e})"),
        None => format!("{name} ({place})"),
    }
}

fn enc_segment(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------- the request

/// A request body: its media type and the JSON it was built from (form bodies are encoded when sent).
#[derive(Clone)]
pub struct Body {
    pub mime: String,
    value: Value,
}

impl Body {
    /// The text that goes on the wire.
    pub fn wire(&self) -> String {
        encode(&self.mime, &self.value)
    }

    /// The same, with every secret-named field hidden.
    pub fn shown(&self) -> String {
        encode(&self.mime, &mask(&self.value))
    }
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.shown())
    }
}

fn encode(mime: &str, v: &Value) -> String {
    match v {
        Value::Object(m) if mime.contains("x-www-form-urlencoded") => {
            let mut s = url::form_urlencoded::Serializer::new(String::new());
            for (k, v) in m {
                let text = match v {
                    Value::String(t) => t.clone(),
                    other => other.to_string(),
                };
                s.append_pair(k, &text);
            }
            s.finish()
        }
        _ => v.to_string(),
    }
}

/// A copy of a JSON value with every string under a secret-named field replaced by [`SECRET`].
fn mask(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| match v {
                    Value::String(s) if secret_name(k) && !s.is_empty() => {
                        (k.clone(), Value::String(SECRET.to_string()))
                    }
                    other => (k.clone(), mask(other)),
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(mask).collect()),
        other => other.clone(),
    }
}

/// A request ready to send. Its `Debug` and [`Request::render`] never hold a secret.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// Origin and path, no query.
    pub url: String,
    pub query: Vec<(String, Val)>,
    /// Lower-case names.
    pub headers: Vec<(String, Val)>,
    pub body: Option<Body>,
}

impl Request {
    /// The request as text for a person: the line, the headers, the body; each secret as `<secret>`.
    pub fn render(&self) -> String {
        let mut url = self.url.clone();
        for (i, (k, v)) in self.query.iter().enumerate() {
            url.push(if i == 0 { '?' } else { '&' });
            url.push_str(&format!("{k}={}", v.shown()));
        }
        let mut out = format!("{} {}\n", self.method, url);
        for (k, v) in &self.headers {
            out.push_str(&format!("{k}: {}\n", v.shown()));
        }
        if let Some(b) = &self.body {
            out.push('\n');
            out.push_str(&b.shown());
            out.push('\n');
        }
        out
    }
}

fn set_header(headers: &mut Vec<(String, Val)>, name: &str, v: Val) {
    headers.retain(|(n, _)| n != name);
    headers.push((name.to_string(), v));
}

/// Builds the request of an endpoint. `auth` holds a value for each scheme the endpoint uses (see [`auth_values`]);
/// a scheme without one is left out. A required parameter that is missing is an error that lists them all.
pub fn build(api: &Api, ep: &Endpoint, inp: &Inputs, auth: &[(usize, String)]) -> Result<Request, String> {
    let gql = ep.gql.is_some();
    let auth_names = auth_query_names(api, ep);

    // where each -p goes
    let mut path_vals: BTreeMap<String, String> = BTreeMap::new();
    let mut query_vals: Vec<(String, String)> = Vec::new();
    let mut body_vals: Vec<(String, String)> = Vec::new();
    for (name, value) in &inp.params {
        match slot(api, ep, name)? {
            Slot::Path(n) => {
                path_vals.insert(n, value.clone());
            }
            Slot::Query(n) => {
                query_vals.retain(|(k, _)| *k != n);
                query_vals.push((n, value.clone()));
            }
            Slot::Body(n) => {
                body_vals.retain(|(k, _)| *k != n);
                body_vals.push((n, value.clone()));
            }
        }
    }

    // the body
    let mime = match &ep.body {
        Some(b) if !gql && !b.mime.is_empty() => b.mime.clone(),
        _ => "application/json".to_string(),
    };
    let wants_body = inp.body.is_some() || !body_vals.is_empty() || gql;
    if matches!(ep.method.as_str(), "GET" | "HEAD") && (inp.body.is_some() || !body_vals.is_empty()) {
        return Err(format!("{} takes no body; remove --body and the body parameters", ep.method));
    }
    let mut doc = inp.body.clone().unwrap_or_else(|| Value::Object(Map::new()));
    if gql {
        if let (Some(name), Value::Object(m)) =
            (ep.gql.as_deref().and_then(|g| g.split_once(' ')).map(|(_, n)| n), &mut doc)
        {
            if !name.starts_with("anonymous") && !name.starts_with("persisted:") && !m.contains_key("operationName") {
                m.insert("operationName".to_string(), Value::String(name.to_string()));
            }
        }
    }
    for (name, raw) in &body_vals {
        let v = typed(body_shape(ep).and_then(|s| shape_at(s, name)), raw);
        let at = if gql { format!("variables.{name}") } else { name.clone() };
        set_path(&mut doc, &at, v)?;
    }

    // what is missing
    let mut missing: Vec<String> = Vec::new();
    for p in &ep.path_params {
        if !path_vals.contains_key(&p.name) {
            missing.push(describe_missing(&p.name, "path", &p.shape));
        }
    }
    for p in &ep.query {
        let given = query_vals.iter().any(|(k, _)| *k == p.name);
        let filled_by_auth = auth_names.contains(&p.name.as_str()) || secret_param(&p.name);
        if p.seen >= ep.calls && ep.calls > 0 && !given && !filled_by_auth {
            missing.push(describe_missing(&p.name, "query", &p.shape));
        }
    }
    if let Some(s) = body_shape(ep) {
        let holder = if gql { doc.get("variables") } else { Some(&doc) };
        let have: HashSet<&str> =
            holder.and_then(Value::as_object).map(|m| m.keys().map(String::as_str).collect()).unwrap_or_default();
        for (name, f) in &s.fields {
            if s.required(name) && !have.contains(name.as_str()) {
                missing.push(describe_missing(name, "body", f));
            }
        }
    }
    if !missing.is_empty() {
        return Err(format!("missing required parameters: {}; give each with -p name=value", missing.join(", ")));
    }

    // the url
    let mut parts: Vec<String> = Vec::new();
    for seg in ep.path.split('/') {
        match seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            Some(name) => parts.push(enc_segment(path_vals.get(name).map(String::as_str).unwrap_or(""))),
            None => parts.push(seg.to_string()),
        }
    }
    let url = format!("{}{}", ep.origin, parts.join("/"));

    let mut query: Vec<(String, Val)> = Vec::new();
    for (k, v) in query_vals {
        let val = if secret_param(&k) { Val::secret(v, "") } else { Val::plain(&v) };
        query.push((k, val));
    }
    let mut headers: Vec<(String, Val)> = vec![("accept".to_string(), Val::plain("application/json"))];
    let body = if wants_body && !matches!(ep.method.as_str(), "GET" | "HEAD") {
        set_header(&mut headers, "content-type", Val::plain(&mime));
        Some(Body { mime, value: doc })
    } else {
        None
    };

    // the auth
    let mut cookies: Vec<&str> = Vec::new();
    for &i in &ep.auth {
        let Some(value) = auth.iter().rev().find(|(j, _)| *j == i).map(|(_, v)| v.as_str()) else { continue };
        let Some(a) = api.auth.get(i) else { continue };
        let word = |w: &str| -> Val {
            if value.to_ascii_lowercase().starts_with(&format!("{} ", w.to_ascii_lowercase())) {
                Val::secret(value.to_string(), w)
            } else {
                Val::secret(format!("{w} {value}"), w)
            }
        };
        match &a.scheme {
            Scheme::Bearer { header } => set_header(&mut headers, header, word("Bearer")),
            Scheme::Basic { header } => {
                let v = if value.contains(':') && !value.to_ascii_lowercase().starts_with("basic ") {
                    use base64::Engine;
                    let enc = base64::engine::general_purpose::STANDARD.encode(value.as_bytes());
                    Val::secret(format!("Basic {enc}"), "Basic")
                } else {
                    word("Basic")
                };
                set_header(&mut headers, header, v);
            }
            Scheme::ApiKeyHeader { name } => set_header(&mut headers, name, Val::secret(value.to_string(), "")),
            Scheme::Csrf { header, .. } => set_header(&mut headers, header, Val::secret(value.to_string(), "")),
            Scheme::ApiKeyQuery { name } => {
                query.retain(|(k, _)| k != name);
                query.push((name.clone(), Val::secret(value.to_string(), "")));
            }
            Scheme::Cookie { .. } => cookies.push(value),
        }
    }
    if !cookies.is_empty() {
        set_header(&mut headers, "cookie", Val::secret(cookies.join("; "), ""));
    }
    Ok(Request { method: ep.method.clone(), url, query, headers, body })
}

// ---------------------------------------------------------------- answers

/// The value at a JSON path of the form `$.a.b[0]` (what a login source records); `None` when it is not there.
pub fn json_get<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut rest = path.strip_prefix('$')?;
    let mut cur = v;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('.') {
            let end = r.find(['.', '[']).unwrap_or(r.len());
            cur = cur.get(&r[..end])?;
            rest = &r[end..];
        } else {
            let r = rest.strip_prefix('[')?;
            let end = r.find(']')?;
            cur = cur.get(r[..end].parse::<usize>().ok()?)?;
            rest = &r[end + 1..];
        }
    }
    Some(cur)
}

/// A text answer for a person: at most [`SHOW_BYTES`] of it, cut at a character, and a count of what was left out.
pub fn show_text(text: &str) -> String {
    if text.len() <= SHOW_BYTES {
        return text.to_string();
    }
    let mut end = SHOW_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… {} more bytes cut\n", &text[..end], text.len() - end)
}

/// `30s`, `5m`, `2h`, `1d`, or a bare number of seconds.
pub fn duration(s: &str) -> Result<Duration, String> {
    let t = s.trim();
    let (num, unit) = match t.find(|c: char| !c.is_ascii_digit()) {
        Some(i) => t.split_at(i),
        None => (t, "s"),
    };
    let n: u64 = num.parse().map_err(|_| format!("{s:?} is not a duration (try 30s, 5m, 2h, 1d)"))?;
    let mult = match unit.trim() {
        "s" | "sec" => 1,
        "m" | "min" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return Err(format!("{s:?} is not a duration (try 30s, 5m, 2h, 1d)")),
    };
    Ok(Duration::from_secs(n.saturating_mul(mult)))
}

// ---------------------------------------------------------------- describe

/// What one endpoint takes and gives, for a person or an agent: parameters, body, auth and the variables read.
pub fn describe(api: &Api, ep: &Endpoint) -> String {
    let mut out = format!("[{}] {} {}{}  ({} calls seen)\n", ep.id, ep.method, ep.origin, ep.path, ep.calls);
    if let Some(g) = &ep.gql {
        out.push_str(&format!(
            "graphql: {g}; the map keeps no query text, so send it with --body '{{\"query\": \"...\"}}'; -p fills variables\n"
        ));
    }
    let one = |p: &crate::spec::Param, required: bool| {
        let star = if required { "*" } else { "" };
        match example_of(&p.shape) {
            Some(e) => format!("{}{star} (e.g. {e})", p.name),
            None => format!("{}{star}", p.name),
        }
    };
    if !ep.path_params.is_empty() {
        let l: Vec<String> = ep.path_params.iter().map(|p| one(p, true)).collect();
        out.push_str(&format!("path params: {}\n", l.join(", ")));
    }
    let skip = auth_query_names(api, ep);
    let q: Vec<String> = ep
        .query
        .iter()
        .filter(|p| !skip.contains(&p.name.as_str()))
        .map(|p| one(p, p.seen >= ep.calls && ep.calls > 0 && !secret_param(&p.name)))
        .collect();
    if !q.is_empty() {
        out.push_str(&format!("query: {}  (* required)\n", q.join(", ")));
    }
    if let Some(b) = &ep.body {
        if b.shape.seen > 0 {
            out.push_str(&format!(
                "body ({}): {}\n",
                if b.mime.is_empty() { "json" } else { &b.mime },
                b.shape.sketch(400)
            ));
        }
    }
    let mut statuses: Vec<(&u16, &crate::spec::Payload)> = ep.responses.iter().collect();
    statuses.sort_by_key(|(s, _)| **s);
    for (status, p) in statuses.into_iter().take(3) {
        if p.shape.seen > 0 {
            out.push_str(&format!("-> {status} json {}\n", p.shape.sketch(300)));
        } else {
            out.push_str(&format!("-> {status} {}\n", if p.mime.is_empty() { "empty" } else { &p.mime }));
        }
    }
    if let Some(p) = &ep.paging {
        out.push_str(&format!("paging: {}\n", p.describe()));
    }
    let n = needs(api, ep);
    if n.is_empty() {
        out.push_str("auth: none seen\n");
    } else {
        out.push_str("auth:\n");
        for need in n {
            out.push_str(&format!("  {}: reads ${} (or --auth {}=ENVVAR)", need.label, need.env, need.label));
            if let Some(l) = &need.login {
                out.push_str(&format!("; --login runs {} {} and reads {}", l.method, l.path, l.json_path));
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::api;
    use serde_json::json;

    fn inputs(pairs: &[(&str, &str)]) -> Inputs {
        Inputs { params: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), body: None }
    }

    #[test]
    fn an_endpoint_is_found_by_id_by_method_and_path_or_by_a_real_path() {
        let a = api();
        assert_eq!(find(&a, "get_api_items").unwrap().path, "/api/items");
        assert_eq!(find(&a, "GET /api/items").unwrap().id, "get_api_items");
        assert_eq!(find(&a, "get /api/items/{item_id}").unwrap().id, "get_api_items_item_id");
        assert_eq!(find(&a, "GET /api/items/99").unwrap().id, "get_api_items_item_id");
        assert_eq!(find(&a, "POST /api/login").unwrap().id, "post_api_login");
        let e = find(&a, "nope").unwrap_err();
        assert!(e.contains("get_api_items") && e.contains("no endpoint"), "{e}");
        assert!(find(&a, "DELETE /api/items").is_err());
    }

    #[test]
    fn path_and_query_are_filled_from_params() {
        let a = api();
        let ep = find(&a, "get_api_items_item_id").unwrap();
        let r = build(&a, ep, &inputs(&[("item_id", "a b/1")]), &[]).unwrap();
        assert_eq!(r.url, "https://app.test/api/items/a%20b%2F1");
        let ep = find(&a, "get_api_items").unwrap();
        let r = build(&a, ep, &inputs(&[("page", "3")]), &[]).unwrap();
        assert_eq!(r.render().lines().next(), Some("GET https://app.test/api/items?page=3"));
        assert!(r.body.is_none());
    }

    #[test]
    fn missing_required_params_are_listed_with_their_examples() {
        let a = api();
        let e = build(&a, find(&a, "get_api_items_item_id").unwrap(), &inputs(&[]), &[]).unwrap_err();
        assert!(e.contains("missing required parameters: item_id (path, e.g. 42)"), "{e}");
        let e = build(&a, find(&a, "get_api_items").unwrap(), &inputs(&[]), &[]).unwrap_err();
        assert!(e.contains("page (query, e.g. 1)"), "{e}");
        let e = build(&a, find(&a, "post_api_login").unwrap(), &inputs(&[("email", "a@b.co")]), &[]).unwrap_err();
        assert!(e.contains("password (body") && !e.contains("email (body"), "{e}");
        let e = build(&a, find(&a, "get_api_items").unwrap(), &inputs(&[("nope", "1")]), &[]).unwrap_err();
        assert!(e.contains("unknown parameter") && e.contains("page (query)"), "{e}");
    }

    #[test]
    fn body_fields_come_from_params_or_a_whole_body() {
        let a = api();
        let ep = find(&a, "post_api_login").unwrap();
        let r = build(&a, ep, &inputs(&[("email", "a@b.co"), ("password", "pw-1")]), &[]).unwrap();
        let b = r.body.as_ref().unwrap();
        assert_eq!(b.wire(), r#"{"email":"a@b.co","password":"pw-1"}"#);
        let whole = Inputs { params: vec![], body: Some(json!({"email": "x@y.zz", "password": "p"})) };
        assert!(build(&a, ep, &whole, &[]).is_ok());
        let partial = Inputs { params: vec![], body: Some(json!({"email": "x@y.zz"})) };
        assert!(build(&a, ep, &partial, &[]).unwrap_err().contains("password"));
    }

    #[test]
    fn a_dry_run_shows_secret_and_never_the_value() {
        let a = api();
        let ep = find(&a, "get_api_items").unwrap();
        let var = "NEXO_API_TEST_TOKEN_9F3A7C1B";
        let value = "tok-9f3a7c1b-real-value";
        std::env::set_var(var, value);
        let lookup = |n: &str| std::env::var(n).ok();
        let explicit = auth_map(&a, &[format!("bearer={var}")]).unwrap();
        let (found, missing) = auth_values(&a, ep, &explicit, &BTreeMap::new(), &lookup);
        assert!(missing.is_empty() || !missing.contains(&var.to_string()), "{missing:?}");
        assert_eq!(found.len(), 1);
        let r = build(&a, ep, &inputs(&[("page", "1")]), &found).unwrap();
        let text = r.render();
        assert!(text.contains("authorization: Bearer <secret>"), "{text}");
        assert!(!text.contains(value) && !format!("{r:?}").contains(value), "{text}");
        assert_eq!(r.headers.iter().find(|(n, _)| n == "authorization").unwrap().1.reveal(), format!("Bearer {value}"));
        std::env::remove_var(var);
    }

    #[test]
    fn a_secret_named_body_field_is_hidden_in_the_render() {
        let a = api();
        let ep = find(&a, "post_api_login").unwrap();
        let r = build(&a, ep, &inputs(&[("email", "a@b.co"), ("password", "pw-unique-7731")]), &[]).unwrap();
        let text = r.render();
        assert!(text.contains(r#""password":"<secret>""#) && !text.contains("pw-unique-7731"), "{text}");
        assert!(!format!("{r:?}").contains("pw-unique-7731"));
    }

    #[test]
    fn auth_names_env_names_and_logins() {
        let a = api();
        assert_eq!(labels(&a), vec!["bearer".to_string()]);
        assert_eq!(env_name("bearer"), "NEXO_API_BEARER");
        assert_eq!(env_name("header:x-api-key"), "NEXO_API_HEADER_X_API_KEY");
        assert!(auth_map(&a, &["nope=X".to_string()]).is_err());
        assert!(auth_map(&a, &["bearer".to_string()]).is_err());
        let ep = find(&a, "get_api_items").unwrap();
        let n = needs(&a, ep);
        assert_eq!(n.len(), 1);
        let login = n[0].login.clone().unwrap();
        assert_eq!(login_endpoint(&a, &login).unwrap().id, "post_api_login");
        let none = |_: &str| None;
        let (found, missing) = auth_values(&a, ep, &[], &BTreeMap::new(), &none);
        assert!(found.is_empty() && missing == vec!["NEXO_API_BEARER".to_string()]);
        let logged = BTreeMap::from([(n[0].index, "from-login".to_string())]);
        let (found, missing) = auth_values(&a, ep, &[], &logged, &none);
        assert!(missing.is_empty() && found == vec![(n[0].index, "from-login".to_string())]);
    }

    #[test]
    fn params_read_a_variable_when_asked() {
        let lookup = |n: &str| (n == "PW_VAR").then(|| "s3cret".to_string());
        assert_eq!(parse_param("a=1", &lookup).unwrap(), ("a".to_string(), "1".to_string()));
        assert_eq!(parse_param("password=@env:PW_VAR", &lookup).unwrap().1, "s3cret");
        assert!(parse_param("password=@env:NOPE", &lookup).is_err());
        assert!(parse_param("nope", &lookup).is_err());
    }

    #[test]
    fn json_paths_durations_and_long_text() {
        let v = json!({"a": {"b": [10, {"c": "x"}]}});
        assert_eq!(json_get(&v, "$.a.b[1].c"), Some(&json!("x")));
        assert_eq!(json_get(&v, "$.a.b[0]"), Some(&json!(10)));
        assert_eq!(json_get(&v, "$"), Some(&v));
        assert_eq!(json_get(&v, "$.a.z"), None);
        assert_eq!(duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(duration("45").unwrap(), Duration::from_secs(45));
        assert!(duration("soon").is_err() && duration("5x").is_err());
        let long = "é".repeat(3000);
        let s = show_text(&long);
        assert!(s.contains("more bytes cut") && s.len() < 4200, "{}", s.len());
        assert_eq!(show_text("short"), "short");
    }

    #[test]
    fn a_form_body_is_encoded_and_a_basic_login_is_built() {
        let b = Body { mime: "application/x-www-form-urlencoded".to_string(), value: json!({"a": "x y", "n": 1}) };
        assert_eq!(b.wire(), "a=x+y&n=1");
        let a = api();
        let text = describe(&a, find(&a, "get_api_items").unwrap());
        assert!(text.contains("NEXO_API_BEARER") && text.contains("--login runs POST /api/login"), "{text}");
        assert!(text.contains("page* (e.g. 1)"), "{text}");
    }
}
