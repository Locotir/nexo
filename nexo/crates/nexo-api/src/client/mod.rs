//! `nexo api client`: writes a small client for a mapped API, in TypeScript, Python or POSIX shell, that a person can
//! run from cron or CI without nexo. This file holds what the three share: the model (one [`Op`] per endpoint, built
//! the same way [`crate::call`] resolves an endpoint, so `nexo api call MAP X` and the generated `X(...)` are the same
//! thing), the names, the header comment and the text helpers. Each language has its own file.
//!
//! A secret never reaches the text. The map holds none, and what a map does hold that looks like one (a `<secret>` or
//! `<opaque>` marker, an opaque example) is dropped on the way. A credential is read from an environment variable when
//! a call runs; the variable names are the ones `nexo api call` reads (`NEXO_API_BEARER`, ...).

mod py;
mod sh;
mod ts;

#[cfg(test)]
mod tests;

use crate::auth::Scheme;
use crate::call;
use crate::exchange::{opaque, secret_param};
use crate::shape::Shape;
use crate::spec::{Api, Endpoint};
use serde_json::Value;
use std::collections::HashSet;

/// The language of a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ts,
    Py,
    Sh,
}

impl Lang {
    /// `ts`, `py` or `sh` (and the long names).
    pub fn parse(s: &str) -> Result<Lang, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ts" | "typescript" => Ok(Lang::Ts),
            "py" | "python" => Ok(Lang::Py),
            "sh" | "shell" => Ok(Lang::Sh),
            _ => Err(format!("--lang {s:?}: use ts, py or sh")),
        }
    }

    /// The file extension.
    pub fn ext(self) -> &'static str {
        match self {
            Lang::Ts => "ts",
            Lang::Py => "py",
            Lang::Sh => "sh",
        }
    }
}

/// Writes the client of a map. `only` names the endpoints to keep (an id, or `METHOD /path`, as `call` takes them);
/// empty keeps all. `date` is written in the header as given: the clock is never read, so the text is stable.
pub fn render(api: &Api, lang: Lang, only: &[String], date: &str) -> Result<String, String> {
    let m = model(api, only, date)?;
    Ok(match lang {
        Lang::Ts => ts::render(&m),
        Lang::Py => py::render(&m),
        Lang::Sh => sh::render(&m),
    })
}

// ---------------------------------------------------------------- the model

/// How an auth scheme is sent.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Bearer(String),
    Basic(String),
    Header(String),
    Query(String),
    /// The session cookie: sent only when its variable is set.
    Cookie,
    /// A header that echoes a cookie: `cookie` names it, `jar` is the variable that holds the cookie string.
    Csrf {
        header: String,
        cookie: Option<String>,
        jar: Option<String>,
    },
}

/// One auth scheme and the variable it is read from.
#[derive(Debug, Clone)]
pub struct Slot {
    /// The scheme's label as `nexo api describe` shows it (`bearer`, `header:x-api-key`).
    pub label: String,
    /// The environment variable.
    pub env: String,
    pub kind: Kind,
    /// Where the capture says the secret came from (`set by POST /api/login ($.access_token)`).
    pub source: Option<String>,
}

impl Slot {
    /// The kind as the generated runtimes spell it.
    pub fn kind_word(&self) -> &'static str {
        match self.kind {
            Kind::Bearer(_) => "bearer",
            Kind::Basic(_) => "basic",
            Kind::Header(_) => "header",
            Kind::Query(_) => "query",
            Kind::Cookie => "cookie",
            Kind::Csrf { .. } => "csrf",
        }
    }

    /// The header or query name the value goes to.
    pub fn target(&self) -> String {
        match &self.kind {
            Kind::Bearer(n) | Kind::Basic(n) | Kind::Header(n) | Kind::Query(n) => n.clone(),
            Kind::Cookie => "cookie".to_string(),
            Kind::Csrf { header, .. } => header.clone(),
        }
    }

    /// One short phrase for a person: what the variable holds.
    pub fn what(&self) -> String {
        let text = match &self.kind {
            Kind::Bearer(h) => format!("bearer token, sent in the {h} header"),
            Kind::Basic(h) => format!("basic credentials as user:password, sent in the {h} header"),
            Kind::Header(n) => format!("the value of the {n} header"),
            Kind::Query(n) => format!("the value of the {n} query parameter"),
            Kind::Cookie => {
                "opt in: the Cookie header of a signed-in session (name=value; name=value), not sent unless set"
                    .to_string()
            }
            Kind::Csrf { header, cookie: Some(c), jar: Some(j) } => {
                format!("the value of the {header} header; when unset it is read from the {c} cookie inside {j}")
            }
            Kind::Csrf { header, cookie: Some(c), jar: None } => {
                format!("the value of the {header} header, copied from the {c} cookie")
            }
            Kind::Csrf { header, .. } => format!("the value of the {header} header"),
        };
        clean(&text)
    }
}

/// A parameter, for the comments and the types.
#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub required: bool,
    /// A short type and a safe example (`int, e.g. 1`).
    pub note: String,
}

/// The body of an operation (for GraphQL, its variables).
#[derive(Debug, Clone)]
pub struct Body {
    pub mime: String,
    /// JSON Schema of what the capture sent, when it was JSON.
    pub schema: Option<Value>,
    pub sketch: String,
    pub form: bool,
}

/// What marks an operation as GraphQL.
#[derive(Debug, Clone)]
pub struct Gql {
    pub kind: String,
    /// `None` for an anonymous or persisted operation.
    pub operation: Option<String>,
    pub persisted: bool,
}

/// Where a paging parameter lives.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Query(String),
    Body(Vec<String>),
    Var(Vec<String>),
}

impl Target {
    /// `query`, `body` or `var`.
    pub fn place(&self) -> &'static str {
        match self {
            Target::Query(_) => "query",
            Target::Body(_) => "body",
            Target::Var(_) => "var",
        }
    }

    /// The path: one name for a query parameter, the dotted path for the others.
    pub fn path(&self) -> Vec<String> {
        match self {
            Target::Query(n) => vec![n.clone()],
            Target::Body(p) | Target::Var(p) => p.clone(),
        }
    }
}

/// How a list pages, in a form a generator can follow.
#[derive(Debug, Clone)]
pub struct Pager {
    /// `page`, `offset`, `cursor`, `next url` or `link header`.
    pub style: &'static str,
    pub target: Option<Target>,
    /// A query parameter that sets the page size.
    pub size: Option<String>,
    /// Where the response holds what the next request needs.
    pub next: Option<Vec<String>>,
    /// Where the response holds the list; empty when the response is the list.
    pub items: Option<Vec<String>>,
    /// The map's own words (`cursor via ?after, next at $.pageInfo.endCursor`).
    pub text: String,
}

/// One endpoint, ready to be written.
#[derive(Debug, Clone)]
pub struct Op {
    /// The endpoint id: what `nexo api call MAP ID` takes.
    pub id: String,
    pub method: String,
    pub origin: String,
    /// The templated path (`/items/{item_id}`).
    pub path: String,
    /// The names in the template, in order, each once.
    pub path_args: Vec<String>,
    pub query: Vec<Field>,
    pub body: Option<Body>,
    /// For a body: its fields.
    pub body_fields: Vec<Field>,
    pub gql: Option<Gql>,
    /// Labels of the schemes it uses.
    pub auth: Vec<String>,
    pub pager: Option<Pager>,
    /// JSON Schema of the first successful response that was JSON.
    pub response: Option<Value>,
    pub response_sketch: String,
}

/// Everything a language file needs.
#[derive(Debug, Clone)]
pub struct Model {
    pub date: String,
    pub origins: Vec<String>,
    pub ops: Vec<Op>,
    /// The schemes the operations use, each once.
    pub slots: Vec<Slot>,
    /// `id (kind)` of each channel the map holds: a client does not follow them.
    pub channels: Vec<String>,
}

impl Model {
    pub fn slot(&self, label: &str) -> Option<&Slot> {
        self.slots.iter().find(|s| s.label == label)
    }
}

/// The endpoints to write, in the map's order.
fn chosen(api: &Api, only: &[String]) -> Result<Vec<usize>, String> {
    let mut out: Vec<usize> = Vec::new();
    if only.is_empty() {
        out.extend(0..api.endpoints.len());
    } else {
        for key in only {
            let ep = call::find(api, key)?;
            if let Some(i) = api.endpoints.iter().position(|e| e.id == ep.id) {
                if !out.contains(&i) {
                    out.push(i);
                }
            }
        }
        out.sort_unstable();
    }
    if out.is_empty() {
        return Err("the map has no endpoints to write a client for".to_string());
    }
    Ok(out)
}

/// The model of a map: the chosen endpoints, the auth they use, the channels that are left out.
pub fn model(api: &Api, only: &[String], date: &str) -> Result<Model, String> {
    let picked = chosen(api, only)?;
    let mut slots: Vec<Slot> = Vec::new();
    let mut ops: Vec<Op> = Vec::new();
    let mut origins: Vec<String> = Vec::new();
    for &i in &picked {
        let ep = &api.endpoints[i];
        let mut labels = Vec::new();
        for need in call::needs(api, ep) {
            if !slots.iter().any(|s| s.label == need.label) {
                slots.push(slot_of(api, need.index, &need.label, &need.env));
            }
            labels.push(need.label);
        }
        if !origins.contains(&ep.origin) {
            origins.push(ep.origin.clone());
        }
        ops.push(op_of(api, ep, labels, &slots));
    }
    link_csrf(&mut slots, &ops);
    let channels = api.channels.iter().map(|c| format!("{} ({})", clean(&c.id), c.kind.name())).collect();
    Ok(Model { date: clean(date), origins, ops, slots, channels })
}

fn slot_of(api: &Api, index: usize, label: &str, env: &str) -> Slot {
    let a = &api.auth[index];
    let kind = match &a.scheme {
        Scheme::Bearer { header } => Kind::Bearer(header.clone()),
        Scheme::Basic { header } => Kind::Basic(header.clone()),
        Scheme::ApiKeyHeader { name } => Kind::Header(name.clone()),
        Scheme::ApiKeyQuery { name } => Kind::Query(name.clone()),
        Scheme::Cookie { .. } => Kind::Cookie,
        Scheme::Csrf { header, from } => {
            let cookie = from.strip_prefix("cookie ").map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
            Kind::Csrf { header: header.clone(), cookie, jar: None }
        }
    };
    Slot { label: label.to_string(), env: env.to_string(), kind, source: a.source.as_deref().map(clean) }
}

/// A CSRF header that echoes a cookie reads it from the cookie slot an operation that uses it also uses.
fn link_csrf(slots: &mut [Slot], ops: &[Op]) {
    let cookies: Vec<(String, String)> =
        slots.iter().filter(|s| s.kind == Kind::Cookie).map(|s| (s.label.clone(), s.env.clone())).collect();
    for s in slots.iter_mut() {
        let label = s.label.clone();
        if let Kind::Csrf { jar, cookie: Some(_), .. } = &mut s.kind {
            let found = ops
                .iter()
                .filter(|o| o.auth.contains(&label))
                .find_map(|o| cookies.iter().find(|(l, _)| o.auth.contains(l)).map(|(_, env)| env.clone()));
            *jar = found;
        }
    }
}

/// The names between braces in a path template, in order, each once.
pub fn template_names(path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = path;
    while let Some(a) = rest.find('{') {
        let Some(len) = rest[a..].find('}') else { break };
        let name = &rest[a + 1..a + len];
        if !name.is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
        rest = &rest[a + len + 1..];
    }
    out
}

fn op_of(api: &Api, ep: &Endpoint, auth: Vec<String>, slots: &[Slot]) -> Op {
    let gql = ep.gql.as_ref().map(|g| {
        let (kind, name) = g.split_once(' ').unwrap_or(("query", g.as_str()));
        let persisted = name.starts_with("persisted:");
        let operation = if name.is_empty() || name == "anonymous" || persisted { None } else { Some(name.to_string()) };
        Gql { kind: kind.to_string(), operation, persisted }
    });
    let skip: Vec<&str> = ep
        .auth
        .iter()
        .filter_map(|&i| match &api.auth.get(i)?.scheme {
            Scheme::ApiKeyQuery { name } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    let query: Vec<Field> = if gql.is_some() {
        Vec::new()
    } else {
        ep.query
            .iter()
            .filter(|p| !skip.contains(&p.name.as_str()))
            .map(|p| Field {
                name: p.name.clone(),
                required: ep.calls > 0 && p.seen >= ep.calls && !secret_param(&p.name),
                note: note_of(&p.shape),
            })
            .collect()
    };
    let mut body_fields = Vec::new();
    let body = ep.body.as_ref().map(|b| {
        let json = b.shape.seen > 0;
        if b.shape.objects > 0 {
            for (name, f) in &b.shape.fields {
                body_fields.push(Field { name: name.clone(), required: b.shape.required(name), note: note_of(f) });
            }
        }
        let mime = if gql.is_some() || b.mime.is_empty() { "application/json".to_string() } else { b.mime.clone() };
        Body {
            form: mime.contains("x-www-form-urlencoded"),
            mime: clean(&mime),
            schema: if json { Some(b.shape.schema()) } else { None },
            sketch: if json { clean(&b.shape.sketch(240)) } else { String::new() },
        }
    });
    let mut ok: Vec<u16> = ep.responses.keys().copied().filter(|s| (200..300).contains(s)).collect();
    ok.sort_unstable();
    let found = ok.iter().find_map(|s| ep.responses.get(s).filter(|p| p.shape.seen > 0));
    let (response, response_sketch) = match found {
        Some(p) => (Some(p.shape.schema()), clean(&p.shape.sketch(240))),
        None => (None, String::new()),
    };
    let path_args = if gql.is_some() { Vec::new() } else { template_names(&ep.path) };
    let has_body = body.is_some();
    let pager = pager_of(ep, gql.is_some(), has_body).filter(|_| gql.is_none() || ep.method == "POST");
    // a label the slots list does not hold cannot happen; keep only the ones that exist
    let auth = auth.into_iter().filter(|l| slots.iter().any(|s| s.label == *l)).collect();
    Op {
        id: ep.id.clone(),
        method: ep.method.clone(),
        origin: ep.origin.clone(),
        path: ep.path.clone(),
        path_args,
        query,
        body,
        body_fields,
        gql,
        auth,
        pager,
        response,
        response_sketch,
    }
}

/// A short type and a safe example of a parameter.
fn note_of(s: &Shape) -> String {
    let mut out = clean(&s.sketch(40));
    if let Some(v) = &s.example {
        let text = match v {
            Value::String(t) if safe(t) => Some(t.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        };
        if let Some(t) = text {
            let t = clean(&t);
            if !t.is_empty() && t.chars().count() <= 40 {
                out.push_str(&format!(", e.g. {t}"));
            }
        }
    }
    out
}

/// `$.a.b` as `["a", "b"]`, `$` as an empty path; `None` for anything with an index or an empty step.
pub fn json_path(p: &str) -> Option<Vec<String>> {
    let rest = p.strip_prefix('$')?;
    if rest.is_empty() {
        return Some(Vec::new());
    }
    let rest = rest.strip_prefix('.')?;
    let mut out = Vec::new();
    for seg in rest.split('.') {
        if seg.is_empty() || seg.contains('[') || seg.contains(']') {
            return None;
        }
        out.push(seg.to_string());
    }
    Some(out)
}

fn target_of(p: &str) -> Target {
    if let Some(rest) = p.strip_prefix("variables.") {
        return Target::Var(rest.split('.').map(str::to_string).collect());
    }
    if let Some(rest) = p.strip_prefix("body.") {
        return Target::Body(rest.split('.').map(str::to_string).collect());
    }
    Target::Query(p.to_string())
}

/// The paging a generator can follow; `None` when the map's paging lacks what it needs (a parameter, a path).
fn pager_of(ep: &Endpoint, gql: bool, has_body: bool) -> Option<Pager> {
    let p = ep.paging.as_ref()?;
    let style: &'static str = match p.style {
        "page" => "page",
        "offset" => "offset",
        "cursor" => "cursor",
        "next url" => "next url",
        "link header" => "link header",
        _ => return None,
    };
    let target = p.param.as_deref().map(target_of);
    let target_ok = match &target {
        Some(Target::Var(_)) => gql,
        Some(Target::Body(_)) => !gql && has_body,
        Some(Target::Query(_)) => !gql,
        None => false,
    };
    let items = p.items.as_deref().and_then(json_path);
    let next = p.next.as_deref().and_then(json_path).filter(|n| !n.is_empty());
    let usable = match style {
        "page" => target_ok,
        "offset" => target_ok && items.is_some(),
        "cursor" => target_ok && next.is_some(),
        "next url" => !gql && next.is_some(),
        _ => !gql,
    };
    if !usable {
        return None;
    }
    let size = p.size.clone().filter(|z| !z.contains('.'));
    let target = if matches!(style, "next url" | "link header") { None } else { target };
    Some(Pager { style, target, size, next, items, text: clean(&p.describe()) })
}

// ---------------------------------------------------------------- names

/// The names an operation gets in one language.
#[derive(Debug, Clone)]
pub struct Names {
    pub func: String,
    /// The paging generator, in the languages that have one.
    pub pages: Option<String>,
}

/// An identifier from any text: other characters become `_`, a leading digit gets a `_` before it, and a word in
/// `reserved` gets one after it.
pub fn ident(name: &str, reserved: &[&str]) -> String {
    let mut out: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if out.is_empty() {
        out.push('_');
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    let low = out.to_ascii_lowercase();
    if low.starts_with("nx") || low.starts_with("_nx") {
        out.insert_str(0, "op_");
    }
    if reserved.contains(&out.as_str()) {
        out.push('_');
    }
    out
}

/// `ident`, then a number after it while the name is taken.
pub fn unique(name: String, taken: &mut HashSet<String>) -> String {
    let mut out = name.clone();
    let mut n = 1;
    while !taken.insert(out.clone()) {
        n += 1;
        out = format!("{name}_{n}");
    }
    out
}

/// The function names of every operation in one language, all different.
pub fn names(m: &Model, reserved: &[&str], pages: bool) -> Vec<Names> {
    let mut taken: HashSet<String> = HashSet::new();
    m.ops
        .iter()
        .map(|o| {
            let func = unique(ident(&o.id, reserved), &mut taken);
            let pages = if pages && o.pager.is_some() {
                Some(unique(ident(&format!("{}_all", o.id), reserved), &mut taken))
            } else {
                None
            };
            Names { func, pages }
        })
        .collect()
}

/// The names of an operation's path arguments in one language, all different from each other.
pub fn arg_names(o: &Op, reserved: &[&str]) -> Vec<String> {
    let mut taken: HashSet<String> = HashSet::new();
    o.path_args.iter().map(|n| unique(ident(n, reserved), &mut taken)).collect()
}

// ---------------------------------------------------------------- text

/// Whether a text from the map may be written out: not a marker and not token-like.
pub fn safe(s: &str) -> bool {
    !s.contains("<secret>") && !s.contains("<opaque>") && !opaque(s)
}

/// One line of plain text for a comment: markers become `?`, line breaks and control characters a space.
pub fn clean(s: &str) -> String {
    let s = s.replace("<secret>", "?").replace("<opaque>", "?").replace("*/", "* /");
    let mut out = String::new();
    for c in s.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if c == ' ' && out.ends_with(' ') {
            continue;
        }
        out.push(c);
    }
    out.trim().to_string()
}

/// A string literal of JavaScript or Python (JSON is both).
pub fn lit(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// A POSIX shell word in single quotes.
pub fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The header comment all three languages open with. `c` is the comment prefix; `how` says what the file is.
pub fn header(m: &Model, c: &str, names: &[Names], how: &str) -> String {
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(c);
        if !s.is_empty() {
            out.push(' ');
            out.push_str(&s);
        }
        out.push('\n');
    };
    line(format!("nexo api client: {how}"));
    line(format!("Written by `nexo api client` on {}. It needs nothing from nexo to run.", m.date));
    line(format!("Origin: {}", m.origins.iter().map(|o| clean(o)).collect::<Vec<_>>().join(", ")));
    line("Operations (the names `nexo api call MAP NAME` takes):".to_string());
    for (o, n) in m.ops.iter().zip(names) {
        let mut s = format!("  {}: {} {}", n.func, o.method, clean(&o.path));
        if let Some(g) = &o.gql {
            s.push_str(&format!(" (GraphQL {})", clean(&g.kind)));
        }
        if let Some(p) = &n.pages {
            s.push_str(&format!("; all pages: {p}"));
        }
        line(s);
    }
    line(String::new());
    if m.slots.is_empty() {
        line("No credentials were seen on these operations.".to_string());
    } else {
        line("Secrets come only from environment variables, read when a call runs; none is written here:".to_string());
        for s in &m.slots {
            let mut t = format!("  {}: {}", s.env, s.what());
            if let Some(src) = &s.source {
                t.push_str(&format!(" (in the capture: {src})"));
            }
            line(t);
        }
    }
    for ch in &m.channels {
        line(format!("Channel {ch} is not in this client; follow it with `nexo api watch MAP ID --for 60s`."));
    }
    line("Every call times out after 30 s, sends a User-Agent naming nexo, and retries once on 429 (Retry-After, at most 30 s).".to_string());
    out
}

/// The comment lines that describe one operation (without the comment prefix).
pub fn notes(o: &Op, env: &dyn Fn(&str) -> String) -> Vec<String> {
    let mut out = Vec::new();
    let mut first = format!("{} {}", o.method, clean(&o.path));
    if let Some(g) = &o.gql {
        first.push_str(&format!(" (GraphQL {}", clean(&g.kind)));
        if let Some(n) = &g.operation {
            first.push_str(&format!(" {}", clean(n)));
        }
        first.push(')');
    }
    out.push(first);
    if !o.path_args.is_empty() {
        out.push(format!("path: {}", o.path_args.iter().map(|n| clean(n)).collect::<Vec<_>>().join(", ")));
    }
    if !o.query.is_empty() {
        let l: Vec<String> = o.query.iter().map(field_note).collect();
        out.push(format!("query: {}  (* required)", l.join(", ")));
    }
    if let Some(b) = &o.body {
        let what = if o.gql.is_some() { "variables" } else { "body" };
        if b.sketch.is_empty() {
            out.push(format!("{what} ({})", b.mime));
        } else {
            out.push(format!("{what} ({}): {}", b.mime, b.sketch));
        }
    }
    if !o.response_sketch.is_empty() {
        out.push(format!("returns: {}", o.response_sketch));
    }
    if let Some(p) = &o.pager {
        out.push(format!("paging: {}", p.text));
    }
    if !o.auth.is_empty() {
        let l: Vec<String> = o.auth.iter().map(|a| env(a)).collect();
        out.push(format!("reads: {}", l.join(", ")));
    }
    out
}

fn field_note(f: &Field) -> String {
    let star = if f.required { "*" } else { "" };
    if f.note.is_empty() {
        format!("{}{star}", clean(&f.name))
    } else {
        format!("{}{star} ({})", clean(&f.name), f.note)
    }
}

/// Where a paging parameter goes in the generated runtimes: `query` or `body`; a GraphQL variable is a field of the
/// body's `variables`.
pub fn place_path(t: &Target) -> (&'static str, Vec<String>) {
    match t {
        Target::Query(n) => ("query", vec![n.clone()]),
        Target::Body(p) => ("body", p.clone()),
        Target::Var(p) => {
            let mut v = vec!["variables".to_string()];
            v.extend(p.iter().cloned());
            ("body", v)
        }
    }
}

/// A JSON array of strings (valid in JavaScript and Python).
pub fn json_arr(v: &[String]) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string())
}
