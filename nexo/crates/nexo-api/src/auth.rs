//! How the calls of a site prove who they are: a bearer token, basic auth, an API key in a header or in the query,
//! session cookies, and the CSRF header that echoes a cookie. It is read from the fingerprints of the secrets each call
//! sent ([`Exchange::sent`]), never from values, and each scheme says where its secret came from when an earlier
//! response handed it out (`set by POST /api/login ($.access_token)`). A name, a path and a JSON path are all that is
//! ever kept.

use crate::exchange::{opaque, Exchange, Fp, Place, Secret};

/// One way of proving who a call is.
#[derive(Debug, Clone, PartialEq)]
pub enum Scheme {
    /// `Authorization: Bearer ...` (the header is `authorization` or `proxy-authorization`).
    Bearer { header: String },
    /// `Authorization: Basic ...`.
    Basic { header: String },
    /// A secret in a header of its own (`x-api-key`, or `authorization` with another scheme word).
    ApiKeyHeader { name: String },
    /// A secret in a query parameter (`?api_key=...`).
    ApiKeyQuery { name: String },
    /// The session cookies one origin takes, analytics cookies left out.
    Cookie { names: Vec<String> },
    /// A header that echoes a cookie or an earlier response (`x-csrf-token`); `from` says which.
    Csrf { header: String, from: String },
}

/// A scheme the traffic uses.
#[derive(Debug, Clone, PartialEq)]
pub struct Auth {
    pub scheme: Scheme,
    /// How many calls use it.
    pub uses: usize,
    /// Where an earlier response handed the secret out: `set by POST /api/login ($.access_token)` or
    /// `cookie set by GET /`. Never a value.
    pub source: Option<String>,
    /// For a header token ([`Scheme::Bearer`], [`Scheme::ApiKeyHeader`]): the fingerprint of the token the capture
    /// last sent (8 bytes of its SHA-256, never the token), so a browser that holds it can be told from one that
    /// does not. `None` for the other schemes.
    pub fp: Option<Fp>,
}

/// Cookie names that start like this belong to analytics and ads, not to the login.
const ANALYTICS: &[&str] = &[
    "_ga",
    "_gid",
    "_gcl",
    "_fbp",
    "_fbc",
    "_hj",
    "__utm",
    "ajs_",
    "mp_",
    "_uet",
    "_clck",
    "_clsk",
    "amplitude",
    "optimizely",
    "_pk_",
    "__cf_bm",
];

fn analytics(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ANALYTICS.iter().any(|p| n.starts_with(p))
}

fn csrf_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("csrf") || n.contains("xsrf")
}

/// Whether the exchange is an http call or the handshake of a WebSocket (which is an http request that upgrades).
fn is_http(x: &Exchange) -> bool {
    matches!(x.url.scheme(), "http" | "https" | "ws" | "wss")
}

/// The origin of an exchange, with a WebSocket's `wss://host` read as `https://host`, so that its cookies and tokens
/// belong to the same site as the page's calls.
fn site_origin(x: &Exchange) -> String {
    let o = x.origin();
    match o.split_once("://") {
        Some(("wss", rest)) => format!("https://{rest}"),
        Some(("ws", rest)) => format!("http://{rest}"),
        _ => o,
    }
}

/// Whether a header name is one of the two that carry a scheme word.
fn authorization(name: &str) -> bool {
    name == "authorization" || name == "proxy-authorization"
}

/// The first word of a header value, lower case (`bearer` for `Bearer <secret>`).
fn scheme_word(x: &Exchange, header: &str) -> String {
    x.req_header(header).and_then(|v| v.split_whitespace().next()).map(str::to_ascii_lowercase).unwrap_or_default()
}

/// The path of a call with every opaque segment hidden.
pub(crate) fn shown_path(x: &Exchange) -> String {
    let parts: Vec<&str> = x.url.path().split('/').map(|s| if opaque(s) { "<opaque>" } else { s }).collect();
    parts.join("/")
}

/// A secret an earlier response handed out, without its value.
struct Issued {
    fp: Fp,
    how: String,
}

fn how_set(x: &Exchange, s: &Secret) -> String {
    let call = format!("{} {}", x.method, shown_path(x));
    match s.place {
        Place::Cookie => format!("cookie set by {call}"),
        Place::Body => format!("set by {call} ({})", s.name),
        _ => format!("set by {call} (header {})", s.name),
    }
}

fn find_source(issued: &[Issued], fp: &Fp) -> Option<String> {
    issued.iter().find(|i| i.fp == *fp).map(|i| i.how.clone())
}

/// A scheme being collected, with the origin it belongs to when it is a cookie one.
struct Found {
    auth: Auth,
    origin: Option<String>,
}

fn same(a: &Scheme, b: &Scheme) -> bool {
    match (a, b) {
        (Scheme::Csrf { header: x, .. }, Scheme::Csrf { header: y, .. }) => x == y,
        _ => a == b,
    }
}

fn add(found: &mut Vec<Found>, scheme: Scheme, source: Option<String>, fp: Option<Fp>) {
    match found.iter_mut().find(|f| f.origin.is_none() && same(&f.auth.scheme, &scheme)) {
        Some(f) => {
            if f.auth.source.is_none() {
                f.auth.source = source;
            }
            // the latest token seen is the one most likely still alive
            if fp.is_some() {
                f.auth.fp = fp;
            }
        }
        None => found.push(Found { auth: Auth { scheme, uses: 0, source, fp }, origin: None }),
    }
}

fn add_cookie(found: &mut Vec<Found>, origin: &str, name: &str, source: Option<String>) {
    match found.iter_mut().find(|f| f.origin.as_deref() == Some(origin)) {
        Some(f) => {
            if let Scheme::Cookie { names } = &mut f.auth.scheme {
                if !names.iter().any(|n| n == name) {
                    names.push(name.to_string());
                }
            }
            if f.auth.source.is_none() {
                f.auth.source = source;
            }
        }
        None => found.push(Found {
            auth: Auth { scheme: Scheme::Cookie { names: vec![name.to_string()] }, uses: 0, source, fp: None },
            origin: Some(origin.to_string()),
        }),
    }
}

/// What a CSRF header echoes: a cookie the same request sends, or else an earlier response.
fn csrf_from(x: &Exchange, s: &Secret, issued: &[Issued]) -> Option<String> {
    if let Some(c) = x.sent.iter().find(|c| c.place == Place::Cookie && c.fp == s.fp) {
        return Some(format!("cookie {}", c.name));
    }
    find_source(issued, &s.fp)
}

/// The schemes the calls use, in the order they first show, with how many calls use each. It reads every http
/// exchange, the dropped ones too: a login is often a form post that looks like a page.
pub fn detect(all: &[Exchange]) -> Vec<Auth> {
    let mut found: Vec<Found> = Vec::new();
    let mut issued: Vec<Issued> = Vec::new();
    for x in all.iter().filter(|x| is_http(x)) {
        let origin = site_origin(x);
        for s in &x.sent {
            match s.place {
                Place::Header => {
                    if s.name == "url-password" {
                        continue;
                    }
                    let source = find_source(&issued, &s.fp);
                    if authorization(&s.name) {
                        let scheme = match scheme_word(x, &s.name).as_str() {
                            "bearer" => Scheme::Bearer { header: s.name.clone() },
                            "basic" => Scheme::Basic { header: s.name.clone() },
                            _ => Scheme::ApiKeyHeader { name: s.name.clone() },
                        };
                        let fp = if matches!(scheme, Scheme::Basic { .. }) { None } else { Some(s.fp) };
                        add(&mut found, scheme, source, fp);
                    } else if csrf_name(&s.name) {
                        match csrf_from(x, s, &issued) {
                            Some(from) => add(&mut found, Scheme::Csrf { header: s.name.clone(), from }, source, None),
                            None => add(&mut found, Scheme::ApiKeyHeader { name: s.name.clone() }, source, Some(s.fp)),
                        }
                    } else {
                        add(&mut found, Scheme::ApiKeyHeader { name: s.name.clone() }, source, Some(s.fp));
                    }
                }
                Place::Query => {
                    let source = find_source(&issued, &s.fp);
                    add(&mut found, Scheme::ApiKeyQuery { name: s.name.clone() }, source, None);
                }
                Place::Cookie => {
                    if s.name.is_empty() || analytics(&s.name) {
                        continue;
                    }
                    let source = find_source(&issued, &s.fp);
                    add_cookie(&mut found, &origin, &s.name, source);
                }
                Place::Body => {}
            }
        }
        for s in &x.got {
            issued.push(Issued { fp: s.fp, how: how_set(x, s) });
        }
    }
    let mut auths: Vec<Auth> = found.into_iter().map(|f| f.auth).collect();
    for x in all.iter().filter(|x| is_http(x)) {
        for i in of_call(x, &auths) {
            auths[i].uses += 1;
        }
    }
    auths
}

/// The indexes of the schemes in `auths` that one call uses.
pub fn of_call(x: &Exchange, auths: &[Auth]) -> Vec<usize> {
    let sends = |place: Place, name: &str| x.sent.iter().any(|s| s.place == place && s.name == name);
    let sent_cookies: Vec<&str> = x.sent.iter().filter(|s| s.place == Place::Cookie).map(|s| s.name.as_str()).collect();
    let mut out = Vec::new();
    // the cookie scheme that shares most names with the call (two origins may share some)
    let mut best: Option<(usize, usize)> = None;
    for (i, a) in auths.iter().enumerate() {
        let used = match &a.scheme {
            Scheme::Bearer { header } => sends(Place::Header, header) && scheme_word(x, header) == "bearer",
            Scheme::Basic { header } => sends(Place::Header, header) && scheme_word(x, header) == "basic",
            Scheme::ApiKeyHeader { name } => {
                let word = scheme_word(x, name);
                let plain = !authorization(name) || (word != "bearer" && word != "basic");
                let csrf = auths.iter().any(|b| matches!(&b.scheme, Scheme::Csrf { header, .. } if header == name));
                sends(Place::Header, name) && plain && !csrf
            }
            Scheme::ApiKeyQuery { name } => sends(Place::Query, name),
            Scheme::Csrf { header, .. } => sends(Place::Header, header),
            Scheme::Cookie { names } => {
                let n = sent_cookies.iter().filter(|c| names.iter().any(|m| m.as_str() == **c)).count();
                if n > 0 && best.is_none_or(|(b, _)| n > b) {
                    best = Some((n, i));
                }
                false
            }
        };
        if used {
            out.push(i);
        }
    }
    if let Some((_, i)) = best {
        out.push(i);
        out.sort_unstable();
    }
    out
}

/// The call whose response handed a secret out, when [`Auth::source`] says so (`set by POST /api/login ($.access_token)`):
/// `nexo api call --login` runs it first and reads the secret from `json_path`. A cookie or a header source gives none.
#[derive(Debug, Clone, PartialEq)]
pub struct Login {
    pub method: String,
    /// The path as the source shows it; it may hold `<opaque>` for a segment that looked like an id.
    pub path: String,
    /// `$.access_token`.
    pub json_path: String,
}

/// Reads the login out of a source text; `None` when the source is not a response body field.
pub fn login_of(source: &str) -> Option<Login> {
    let rest = source.strip_prefix("set by ")?;
    let (call, tail) = rest.rsplit_once(" (")?;
    let json_path = tail.strip_suffix(')')?;
    if !json_path.starts_with('$') {
        return None;
    }
    let (method, path) = call.split_once(' ')?;
    if method.is_empty() || !path.starts_with('/') {
        return None;
    }
    Some(Login { method: method.to_string(), path: path.to_string(), json_path: json_path.to_string() })
}

#[cfg(test)]
mod login_tests {
    use super::*;

    #[test]
    fn a_body_source_gives_a_login_and_the_others_do_not() {
        let l = login_of("set by POST /api/login ($.access_token)").unwrap();
        assert_eq!(
            (l.method.as_str(), l.path.as_str(), l.json_path.as_str()),
            ("POST", "/api/login", "$.access_token")
        );
        assert_eq!(login_of("set by GET /x (header x-token)"), None);
        assert_eq!(login_of("cookie set by GET /"), None);
        assert_eq!(login_of("nonsense"), None);
    }
}
