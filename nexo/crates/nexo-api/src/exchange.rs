//! One request and its response, as read from a capture (a HAR file, or nexo's own browser). Secret values do not
//! survive the reading: a header, cookie, query parameter or body field that carries one is replaced by [`SECRET`]
//! there and then, and only a short fingerprint ([`Fp`]) is kept, so that a token a request sends can still be traced
//! to the earlier response that handed it out. Nothing nexo api prints or writes can hold a secret.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// What a secret value is shown as, everywhere.
pub const SECRET: &str = "<secret>";

/// A short fingerprint of a secret value (the first 8 bytes of its SHA-256): enough to tell that the token one request
/// sends is the one an earlier response gave, without keeping the token.
pub type Fp = [u8; 8];

/// The fingerprint of a value.
pub fn fingerprint(value: &str) -> Fp {
    let digest = Sha256::digest(value.as_bytes());
    let mut fp = [0u8; 8];
    fp.copy_from_slice(&digest[..8]);
    fp
}

/// Whether a string looks like a token, a hash or an encoded blob rather than words, a date or a link: 24 characters
/// or more without a space, a colon or a leading `/`, made of 32 or more hex digits or of at least three of lower case,
/// upper case, digits and symbols. Such a string is never shown as an example.
pub fn opaque(s: &str) -> bool {
    let n = s.chars().count();
    if n < 24 || s.contains(char::is_whitespace) || s.contains(':') || s.starts_with('/') {
        return false;
    }
    if n >= 32 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    let classes = [
        s.chars().any(|c| c.is_ascii_lowercase()),
        s.chars().any(|c| c.is_ascii_uppercase()),
        s.chars().any(|c| c.is_ascii_digit()),
        s.chars().any(|c| !c.is_ascii_alphanumeric()),
    ];
    classes.iter().filter(|&&b| b).count() >= 3
}

/// Where a secret was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Place {
    Header,
    Cookie,
    Query,
    Body,
}

/// A secret seen in a capture: where it was, under which name (a header, cookie or parameter name, or the JSON path of
/// a body field such as `$.data.access_token`), and its fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Secret {
    pub place: Place,
    pub name: String,
    pub fp: Fp,
}

/// A request or response body.
#[derive(Debug, Clone, PartialEq)]
pub struct Body {
    /// The media type, lower case, without parameters (`application/json`).
    pub mime: String,
    /// The body as JSON with its secrets replaced, when it was JSON (an anti-XSSI prefix such as `)]}'` removed) or a
    /// form (`application/x-www-form-urlencoded`, read as an object of strings); `None` for anything else.
    pub json: Option<Value>,
    /// Its size in bytes as captured.
    pub size: usize,
}

/// One request and its response.
#[derive(Debug, Clone, PartialEq)]
pub struct Exchange {
    /// Its place in the capture (0, 1, ...): an earlier request has a smaller number.
    pub seq: usize,
    /// Upper case.
    pub method: String,
    /// The URL with secret query values replaced.
    pub url: url::Url,
    /// Lower-case names, HTTP/2 pseudo-headers (`:path`) left out, secret values replaced (`Bearer <secret>` keeps
    /// its scheme word).
    pub req_headers: Vec<(String, String)>,
    pub req_body: Option<Body>,
    /// 0 when no response arrived.
    pub status: u16,
    /// Lower-case names, secret values replaced (`set-cookie: sid=<secret>; Path=/`).
    pub resp_headers: Vec<(String, String)>,
    pub resp_body: Option<Body>,
    /// What the browser called the request, lower case (`xhr`, `fetch`, `document`, `script`, `image`, `ping`,
    /// `websocket`, ...); empty when the capture does not say.
    pub kind: String,
    /// The secrets the request sent: headers, cookies, query parameters, body fields.
    pub sent: Vec<Secret>,
    /// The secrets the response handed out: `set-cookie`, body fields with secret names.
    pub got: Vec<Secret>,
}

impl Exchange {
    /// The first request header with this lower-case name.
    pub fn req_header(&self, name: &str) -> Option<&str> {
        self.req_headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// The first response header with this lower-case name.
    pub fn resp_header(&self, name: &str) -> Option<&str> {
        self.resp_headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// `https://host` or `https://host:port`.
    pub fn origin(&self) -> String {
        self.url.origin().ascii_serialization()
    }
}

/// An exchange as a capture gives it, secrets and all; [`Raw::into_exchange`] takes them out.
#[derive(Debug, Clone, Default)]
pub struct Raw {
    pub method: String,
    pub url: String,
    pub req_headers: Vec<(String, String)>,
    /// (media type as captured, text)
    pub req_body: Option<(String, String)>,
    pub status: u16,
    pub resp_headers: Vec<(String, String)>,
    /// (media type as captured, text)
    pub resp_body: Option<(String, String)>,
    pub kind: String,
}

/// Names (normalized) that contain one of these carry a secret.
const SECRET_PARTS: [&str; 16] = [
    "token",
    "secret",
    "password",
    "passwd",
    "apikey",
    "authorization",
    "session",
    "sessid",
    "cookie",
    "jwt",
    "signature",
    "csrf",
    "xsrf",
    "credential",
    "privatekey",
    "accesskey",
];

/// Names (normalized) that are a secret only when they are exactly one of these.
const SECRET_EXACT: [&str; 8] = ["auth", "sid", "sig", "otp", "pwd", "pass", "refresh", "bearer"];

/// Texts longer than this many bytes are not parsed as JSON.
const MAX_PARSE: usize = 4 * 1024 * 1024;

/// A value shorter than this is not hunted for elsewhere in the same exchange (too likely to be a common word).
const MIN_ECHO: usize = 8;

/// The name with only ASCII letters and digits left, lower case.
fn normalize(name: &str) -> String {
    name.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

/// Whether a header, cookie, form or JSON field name says its value is a secret (`x-api-key`, `access_token`,
/// `Authorization`, `csrfToken`, `sid`). `author` is not one; `auth` and `oauth` are.
pub fn secret_name(name: &str) -> bool {
    let n = normalize(name);
    if n.is_empty() {
        return false;
    }
    SECRET_PARTS.iter().any(|p| n.contains(p))
        || SECRET_EXACT.contains(&n.as_str())
        || (n.starts_with("auth") && !n.starts_with("author"))
        || n.ends_with("auth")
}

/// Whether a query parameter carries a secret: a [`secret_name`], or a bare `key` (`?key=AIza...`).
pub fn secret_param(name: &str) -> bool {
    secret_name(name) || normalize(name) == "key"
}

/// A URL taken from a response (a `Location` header, a next-page link) made safe to print: the password and the
/// fragment are cut, and the value of every secret query parameter becomes [`SECRET`], by the same rules as
/// [`Raw::into_exchange`]. A relative URL stays relative. Nothing is recorded.
pub fn clean_url(text: &str) -> String {
    let t = text.trim();
    let (mut u, relative) = match url::Url::parse(t) {
        Ok(u) => (u, false),
        Err(_) => match url::Url::parse("http://relative.invalid/").and_then(|b| b.join(t)) {
            Ok(u) => (u, true),
            Err(_) => return "<unreadable url>".to_string(),
        },
    };
    let _ = u.set_password(None);
    u.set_fragment(None);
    let pairs: Vec<(String, String)> = u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    if pairs.iter().any(|(k, v)| secret_param(k) && !v.is_empty()) {
        let clean: Vec<(String, String)> = pairs
            .into_iter()
            .map(|(k, v)| if secret_param(&k) && !v.is_empty() { (k, SECRET.to_string()) } else { (k, v) })
            .collect();
        u.set_query(None);
        u.query_pairs_mut().extend_pairs(clean.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    if !relative {
        return u.to_string();
    }
    match u.query() {
        Some(q) => format!("{}?{}", u.path(), q),
        None => u.path().to_string(),
    }
}

/// Records a secret and remembers its raw value so that the same string can be hunted for elsewhere.
fn mark(list: &mut Vec<Secret>, raw: &mut std::collections::HashSet<String>, place: Place, name: &str, value: &str) {
    list.push(Secret { place, name: name.to_string(), fp: fingerprint(value) });
    raw.insert(value.to_string());
}

type Echo = std::collections::HashSet<String>;

/// `Bearer abc` becomes `Bearer <secret>`; a value with no scheme becomes `<secret>`.
fn clean_authorization(name: &str, value: &str, list: &mut Vec<Secret>, raw: &mut Echo) -> String {
    let v = value.trim();
    if v.is_empty() {
        return value.to_string();
    }
    match v.split_once(char::is_whitespace) {
        Some((scheme, rest)) if !rest.trim().is_empty() => {
            mark(list, raw, Place::Header, name, rest.trim());
            format!("{} {}", scheme, SECRET)
        }
        _ => {
            mark(list, raw, Place::Header, name, v);
            SECRET.to_string()
        }
    }
}

/// `a=1; b=2` becomes `a=<secret>; b=<secret>`.
fn clean_cookie(value: &str, list: &mut Vec<Secret>, raw: &mut Echo) -> String {
    let mut parts = Vec::new();
    for part in value.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('=') {
            Some((n, v)) => {
                let n = n.trim();
                mark(list, raw, Place::Cookie, n, v.trim());
                parts.push(format!("{}={}", n, SECRET));
            }
            None => {
                mark(list, raw, Place::Cookie, "", part);
                parts.push(SECRET.to_string());
            }
        }
    }
    parts.join("; ")
}

/// `sid=abc; Path=/` becomes `sid=<secret>; Path=/` (one line per cookie).
fn clean_set_cookie(value: &str, list: &mut Vec<Secret>, raw: &mut Echo) -> String {
    let mut lines = Vec::new();
    for line in value.split('\n') {
        let (first, attrs) = match line.split_once(';') {
            Some((f, a)) => (f, Some(a)),
            None => (line, None),
        };
        let cleaned = match first.split_once('=') {
            Some((n, v)) => {
                let n = n.trim();
                mark(list, raw, Place::Cookie, n, v.trim());
                format!("{}={}", n, SECRET)
            }
            None if first.trim().is_empty() => first.to_string(),
            None => {
                mark(list, raw, Place::Cookie, "", first.trim());
                SECRET.to_string()
            }
        };
        match attrs {
            Some(a) => lines.push(format!("{};{}", cleaned, a)),
            None => lines.push(cleaned),
        }
    }
    lines.join("\n")
}

/// Lower-case names, pseudo-headers dropped, secret values replaced.
fn clean_headers(headers: Vec<(String, String)>, list: &mut Vec<Secret>, raw: &mut Echo) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(headers.len());
    for (name, value) in headers {
        let name = name.trim().to_ascii_lowercase();
        if name.starts_with(':') {
            continue;
        }
        let value = if name == "authorization" || name == "proxy-authorization" {
            clean_authorization(&name, &value, list, raw)
        } else if name == "cookie" {
            clean_cookie(&value, list, raw)
        } else if name == "set-cookie" {
            clean_set_cookie(&value, list, raw)
        } else if secret_name(&name) && !value.is_empty() {
            mark(list, raw, Place::Header, &name, &value);
            SECRET.to_string()
        } else {
            value
        };
        out.push((name, value));
    }
    out
}

/// The value of a header, whatever the case of its name.
fn find_header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone())
}

/// A media type: lower case, parameters cut, trimmed.
fn media_type(s: &str) -> String {
    s.split(';').next().unwrap_or("").trim().to_ascii_lowercase()
}

/// Takes off an anti-XSSI prefix: a `)]}'` line, `while(1);` or `for(;;);`.
fn strip_xssi(text: &str) -> &str {
    let t = text.trim_start();
    if t.starts_with(")]}'") {
        return match t.find('\n') {
            Some(i) => &t[i + 1..],
            None => "",
        };
    }
    for prefix in ["while(1);", "for(;;);"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            return rest.trim_start();
        }
    }
    t
}

/// Replaces the secret fields of a JSON value (an object field with a secret name and a non-empty string) and records
/// them with their JSON path.
fn redact_json(v: &mut Value, path: &str, list: &mut Vec<Secret>, raw: &mut Echo) {
    match v {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                let here = format!("{}.{}", path, key);
                if secret_name(key) {
                    if let Value::String(s) = child {
                        if !s.is_empty() {
                            mark(list, raw, Place::Body, &here, s.as_str());
                            *s = SECRET.to_string();
                            continue;
                        }
                    }
                }
                redact_json(child, &here, list, raw);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter_mut().enumerate() {
                redact_json(child, &format!("{}[{}]", path, i), list, raw);
            }
        }
        _ => {}
    }
}

/// Replaces every string that equals a secret value seen in this exchange, wherever it sits.
fn scrub_echo(v: &mut Value, raw: &Echo) {
    match v {
        Value::String(s) => {
            if s.len() >= MIN_ECHO && raw.contains(s.as_str()) {
                *s = SECRET.to_string();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|x| scrub_echo(x, raw)),
        Value::Object(map) => map.iter_mut().for_each(|(_, x)| scrub_echo(x, raw)),
        _ => {}
    }
}

/// Builds a [`Body`] from captured text.
fn make_body(mime: &str, text: &str, header_type: Option<&str>, list: &mut Vec<Secret>, raw: &mut Echo) -> Body {
    let mut mime = media_type(mime);
    if mime.is_empty() {
        mime = header_type.map(media_type).unwrap_or_default();
    }
    let mut json = None;
    if text.len() <= MAX_PARSE {
        if mime == "application/x-www-form-urlencoded" {
            let mut map = serde_json::Map::new();
            for (k, v) in url::form_urlencoded::parse(text.as_bytes()) {
                map.insert(k.into_owned(), Value::String(v.into_owned()));
            }
            if !map.is_empty() {
                json = Some(Value::Object(map));
            }
        } else {
            let t = strip_xssi(text);
            let t = t.trim();
            if mime.contains("json") || t.starts_with('{') || t.starts_with('[') {
                json = serde_json::from_str::<Value>(t).ok();
            }
        }
    }
    if let Some(v) = json.as_mut() {
        redact_json(v, "$", list, raw);
    }
    Body { mime, json, size: text.len() }
}

impl Raw {
    /// Turns a captured exchange into an [`Exchange`] with every secret replaced by [`SECRET`] and recorded as a
    /// fingerprint: `Authorization` and `Cookie` values, secret-named headers, query parameters and body fields,
    /// `Set-Cookie` values in the response, and any other string equal to one of those values. `None` when the URL
    /// does not parse.
    pub fn into_exchange(self, seq: usize) -> Option<Exchange> {
        let mut url = url::Url::parse(self.url.trim()).ok()?;
        let mut sent = Vec::new();
        let mut got = Vec::new();
        let mut echo = Echo::new();

        // A password in the URL and a fragment (OAuth implicit grants put tokens there) never survive.
        if let Some(p) = url.password() {
            let p = p.to_string();
            mark(&mut sent, &mut echo, Place::Header, "url-password", &p);
            let _ = url.set_password(None);
        }
        url.set_fragment(None);

        let pairs: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        if pairs.iter().any(|(k, v)| secret_param(k) && !v.is_empty()) {
            let mut clean = Vec::with_capacity(pairs.len());
            for (k, v) in pairs {
                if secret_param(&k) && !v.is_empty() {
                    mark(&mut sent, &mut echo, Place::Query, &k, &v);
                    clean.push((k, SECRET.to_string()));
                } else {
                    clean.push((k, v));
                }
            }
            url.set_query(None);
            url.query_pairs_mut().extend_pairs(clean.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        }

        let req_type = find_header(&self.req_headers, "content-type");
        let resp_type = find_header(&self.resp_headers, "content-type");
        let req_headers = clean_headers(self.req_headers, &mut sent, &mut echo);
        let mut req_body =
            self.req_body.map(|(mime, text)| make_body(&mime, &text, req_type.as_deref(), &mut sent, &mut echo));
        let resp_headers = clean_headers(self.resp_headers, &mut got, &mut echo);
        let mut resp_body =
            self.resp_body.map(|(mime, text)| make_body(&mime, &text, resp_type.as_deref(), &mut got, &mut echo));

        for body in [req_body.as_mut(), resp_body.as_mut()].into_iter().flatten() {
            if let Some(v) = body.json.as_mut() {
                scrub_echo(v, &echo);
            }
        }

        let method = self.method.trim().to_ascii_uppercase();
        Some(Exchange {
            seq,
            method: if method.is_empty() { "GET".to_string() } else { method },
            url,
            req_headers,
            req_body,
            status: self.status,
            resp_headers,
            resp_body,
            kind: self.kind.trim().to_ascii_lowercase(),
            sent,
            got,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(url: &str) -> Raw {
        Raw { method: "get".into(), url: url.into(), status: 200, ..Raw::default() }
    }

    fn h(n: &str, v: &str) -> (String, String) {
        (n.to_string(), v.to_string())
    }

    #[test]
    fn secret_names_are_recognised() {
        for n in [
            "Authorization",
            "x-api-key",
            "access_token",
            "csrfToken",
            "X-XSRF-TOKEN",
            "sessionid",
            "Cookie",
            "client_secret",
            "password",
            "jwt",
            "signature",
            "sid",
            "OTP",
            "auth",
            "oauth",
            "authToken",
            "refresh",
            "PrivateKey",
            "accessKey",
        ] {
            assert!(secret_name(n), "{n} should be secret");
        }
    }

    #[test]
    fn plain_names_are_not_secret() {
        for n in ["author", "authors", "name", "content-type", "page", "id", "key", "user-agent", ""] {
            assert!(!secret_name(n), "{n} should not be secret");
        }
    }

    #[test]
    fn bare_key_is_a_secret_param_but_not_a_secret_name() {
        assert!(secret_param("key"));
        assert!(secret_param("KEY"));
        assert!(secret_param("access_token"));
        assert!(!secret_param("keyword"));
        assert!(!secret_name("key"));
    }

    #[test]
    fn unparsable_url_gives_none() {
        assert!(raw("not a url").into_exchange(0).is_none());
        assert!(raw("").into_exchange(0).is_none());
    }

    #[test]
    fn method_is_upper_case_and_headers_lower_case_without_pseudo_headers() {
        let mut r = raw("https://a.test/x");
        r.req_headers = vec![h(":path", "/x"), h("Accept", "text/html"), h(":authority", "a.test")];
        let x = r.into_exchange(3).unwrap();
        assert_eq!(x.seq, 3);
        assert_eq!(x.method, "GET");
        assert_eq!(x.req_headers, vec![h("accept", "text/html")]);
    }

    #[test]
    fn bearer_scheme_is_kept_and_token_replaced() {
        let mut r = raw("https://a.test/x");
        r.req_headers = vec![h("Authorization", "Bearer abc123tokenvalue")];
        let x = r.into_exchange(0).unwrap();
        assert_eq!(x.req_header("authorization"), Some("Bearer <secret>"));
        assert_eq!(
            x.sent,
            vec![Secret { place: Place::Header, name: "authorization".into(), fp: fingerprint("abc123tokenvalue") }]
        );
    }

    #[test]
    fn other_schemes_are_kept_and_a_bare_value_is_replaced_whole() {
        let mut r = raw("https://a.test/x");
        r.req_headers = vec![h("authorization", "Basic dXNlcjpwYXNz"), h("proxy-authorization", "rawtokenvalue")];
        let x = r.into_exchange(0).unwrap();
        assert_eq!(x.req_header("authorization"), Some("Basic <secret>"));
        assert_eq!(x.req_header("proxy-authorization"), Some("<secret>"));
        assert_eq!(x.sent.len(), 2);
    }

    #[test]
    fn cookie_pairs_are_replaced_one_by_one() {
        let mut r = raw("https://a.test/x");
        r.req_headers = vec![h("Cookie", "sid=abc; theme=dark")];
        let x = r.into_exchange(0).unwrap();
        assert_eq!(x.req_header("cookie"), Some("sid=<secret>; theme=<secret>"));
        assert_eq!(x.sent.len(), 2);
        assert_eq!(x.sent[0], Secret { place: Place::Cookie, name: "sid".into(), fp: fingerprint("abc") });
        assert_eq!(x.sent[1], Secret { place: Place::Cookie, name: "theme".into(), fp: fingerprint("dark") });
    }

    #[test]
    fn secret_named_request_headers_are_replaced() {
        let mut r = raw("https://a.test/x");
        r.req_headers = vec![h("X-Api-Key", "k-12345678"), h("x-csrf-token", "cs"), h("accept", "*/*")];
        let x = r.into_exchange(0).unwrap();
        assert_eq!(x.req_header("x-api-key"), Some("<secret>"));
        assert_eq!(x.req_header("x-csrf-token"), Some("<secret>"));
        assert_eq!(x.req_header("accept"), Some("*/*"));
        assert!(x.sent.iter().all(|s| s.place == Place::Header));
        assert_eq!(x.sent.len(), 2);
    }

    #[test]
    fn set_cookie_keeps_its_attributes_and_goes_to_got() {
        let mut r = raw("https://a.test/login");
        r.resp_headers = vec![h("Set-Cookie", "sid=abc; Path=/; HttpOnly")];
        let x = r.into_exchange(0).unwrap();
        assert_eq!(x.resp_header("set-cookie"), Some("sid=<secret>; Path=/; HttpOnly"));
        assert_eq!(x.got, vec![Secret { place: Place::Cookie, name: "sid".into(), fp: fingerprint("abc") }]);
        assert!(x.sent.is_empty());
    }

    #[test]
    fn joined_set_cookie_lines_are_each_cleaned() {
        let mut r = raw("https://a.test/login");
        r.resp_headers = vec![h("set-cookie", "a=1111; Path=/\nb=2222; Secure")];
        let x = r.into_exchange(0).unwrap();
        assert_eq!(x.resp_header("set-cookie"), Some("a=<secret>; Path=/\nb=<secret>; Secure"));
        assert_eq!(x.got.len(), 2);
    }

    #[test]
    fn secret_query_values_are_replaced_and_others_kept() {
        let x =
            raw("https://a.test/x?api_key=AIzaSECRETVALUE&q=cats&key=k1k1k1k1#frag-token").into_exchange(0).unwrap();
        let pairs: Vec<(String, String)> = x.url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        assert_eq!(pairs, vec![h("api_key", "<secret>"), h("q", "cats"), h("key", "<secret>")]);
        assert_eq!(x.url.fragment(), None);
        assert_eq!(x.sent.len(), 2);
        assert!(x.sent.iter().all(|s| s.place == Place::Query));
    }

    #[test]
    fn query_without_secrets_is_left_byte_for_byte() {
        let x = raw("https://a.test/x?q=a%20b&page=2").into_exchange(0).unwrap();
        assert_eq!(x.url.as_str(), "https://a.test/x?q=a%20b&page=2");
        assert!(x.sent.is_empty());
    }

    #[test]
    fn password_in_the_url_is_dropped() {
        let x = raw("https://bob:hunter2hunter2@a.test/x").into_exchange(0).unwrap();
        assert!(!x.url.as_str().contains("hunter2"));
        assert_eq!(x.sent.len(), 1);
    }

    #[test]
    fn json_body_secret_fields_get_their_path() {
        let mut r = raw("https://a.test/login");
        r.resp_body = Some((
            "application/json; charset=utf-8".into(),
            r#"{"data":{"access_token":"tok-abcdef","name":"Ann"},"items":[{"refresh":"r-123456"}]}"#.into(),
        ));
        let x = r.into_exchange(0).unwrap();
        let b = x.resp_body.unwrap();
        assert_eq!(b.mime, "application/json");
        let j = b.json.unwrap();
        assert_eq!(j["data"]["access_token"], "<secret>");
        assert_eq!(j["data"]["name"], "Ann");
        assert_eq!(j["items"][0]["refresh"], "<secret>");
        let names: Vec<&str> = x.got.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["$.data.access_token", "$.items[0].refresh"]);
        assert!(x.got.iter().all(|s| s.place == Place::Body));
        assert!(x.sent.is_empty());
    }

    #[test]
    fn request_body_secrets_go_to_sent() {
        let mut r = raw("https://a.test/login");
        r.method = "POST".into();
        r.req_body = Some(("application/json".into(), r#"{"user":"ann","password":"pw-secret-1"}"#.into()));
        let x = r.into_exchange(0).unwrap();
        assert_eq!(x.sent.len(), 1);
        assert_eq!(x.sent[0].name, "$.password");
        assert_eq!(x.req_body.unwrap().json.unwrap()["user"], "ann");
    }

    #[test]
    fn empty_secret_field_is_left_alone() {
        let mut r = raw("https://a.test/x");
        r.resp_body = Some(("application/json".into(), r#"{"token":""}"#.into()));
        let x = r.into_exchange(0).unwrap();
        assert_eq!(x.resp_body.unwrap().json.unwrap()["token"], "");
        assert!(x.got.is_empty());
    }

    #[test]
    fn json_is_found_by_sniffing_when_the_mime_does_not_say() {
        let mut r = raw("https://a.test/x");
        r.resp_body = Some(("text/plain".into(), "  [1,2,3]".into()));
        let b = r.into_exchange(0).unwrap().resp_body.unwrap();
        assert_eq!(b.mime, "text/plain");
        assert!(b.json.is_some());
        assert_eq!(b.size, 9);
    }

    #[test]
    fn mime_falls_back_to_the_content_type_header() {
        let mut r = raw("https://a.test/x");
        r.resp_headers = vec![h("Content-Type", "Application/JSON; charset=utf-8")];
        r.resp_body = Some((String::new(), r#"{"a":1}"#.into()));
        let b = r.into_exchange(0).unwrap().resp_body.unwrap();
        assert_eq!(b.mime, "application/json");
        assert!(b.json.is_some());
    }

    #[test]
    fn anti_xssi_prefixes_are_removed() {
        for (text, ok) in
            [(")]}'\n{\"a\":1}", true), ("while(1);{\"a\":1}", true), ("for(;;);[1]", true), (")]}'", false)]
        {
            let mut r = raw("https://a.test/x");
            r.resp_body = Some(("application/json".into(), text.into()));
            let b = r.into_exchange(0).unwrap().resp_body.unwrap();
            assert_eq!(b.json.is_some(), ok, "{text}");
        }
    }

    #[test]
    fn html_and_binary_bodies_have_no_json() {
        let mut r = raw("https://a.test/x");
        r.resp_body = Some(("text/html".into(), "<html>token=abc</html>".into()));
        let b = r.into_exchange(0).unwrap().resp_body.unwrap();
        assert!(b.json.is_none());
        assert_eq!(b.size, 22);
    }

    #[test]
    fn invalid_json_gives_none_without_failing() {
        let mut r = raw("https://a.test/x");
        r.resp_body = Some(("application/json".into(), "{oops".into()));
        assert!(r.into_exchange(0).unwrap().resp_body.unwrap().json.is_none());
    }

    #[test]
    fn texts_over_four_megabytes_are_not_parsed() {
        let mut r = raw("https://a.test/x");
        let big = format!("[\"{}\"]", "a".repeat(MAX_PARSE));
        r.resp_body = Some(("application/json".into(), big));
        let b = r.into_exchange(0).unwrap().resp_body.unwrap();
        assert!(b.json.is_none());
        assert!(b.size > MAX_PARSE);
    }

    #[test]
    fn form_body_becomes_an_object_of_strings() {
        let mut r = raw("https://a.test/login");
        r.method = "POST".into();
        r.req_body = Some(("application/x-www-form-urlencoded".into(), "user=ann&password=p%40ssw0rd1&n=3".into()));
        let x = r.into_exchange(0).unwrap();
        let j = x.req_body.unwrap().json.unwrap();
        assert_eq!(j["user"], "ann");
        assert_eq!(j["n"], "3");
        assert_eq!(j["password"], "<secret>");
        assert_eq!(x.sent[0].fp, fingerprint("p@ssw0rd1"));
    }

    #[test]
    fn a_secret_echoed_under_a_plain_name_is_replaced_too() {
        let mut r = raw("https://a.test/x");
        r.req_headers = vec![h("authorization", "Bearer echoed-token-1234")];
        r.resp_body = Some(("application/json".into(), r#"{"you_sent":"echoed-token-1234","n":"short"}"#.into()));
        let x = r.into_exchange(0).unwrap();
        assert_eq!(x.resp_body.unwrap().json.unwrap()["you_sent"], "<secret>");
    }

    #[test]
    fn debug_output_of_a_whole_exchange_holds_no_raw_secret() {
        let secrets = [
            "BearerSecretAAA111",
            "cookieSecretBBB222",
            "setCookieSecretCCC333",
            "apiKeyHeaderSecretDDD444",
            "queryKeySecretEEE555",
            "reqBodyPasswordFFF666",
            "respBodyTokenGGG777",
            "formSecretHHH888",
            "echoedElsewhereIII999",
            "urlPasswordJJJ000",
        ];
        let mut r =
            raw(&format!("https://u:{}@a.test/x?api_key={}&q=1#access_token={}", secrets[9], secrets[4], secrets[0]));
        r.method = "post".into();
        r.req_headers = vec![
            h("Authorization", &format!("Bearer {}", secrets[0])),
            h("Cookie", &format!("sid={}", secrets[1])),
            h("x-api-key", secrets[3]),
        ];
        r.req_body = Some((
            "application/json".into(),
            format!(r#"{{"password":"{}","nested":[{{"client_secret":"{}"}}]}}"#, secrets[5], secrets[7]),
        ));
        r.resp_headers = vec![h("set-cookie", &format!("s={}; Path=/", secrets[2]))];
        r.resp_body = Some((
            "application/json".into(),
            format!(r#"{{"access_token":"{}","debug":"{}"}}"#, secrets[6], secrets[1]),
        ));
        let x = r.into_exchange(0).unwrap();
        let dump = format!("{:?}", x);
        for s in secrets {
            assert!(!dump.contains(s), "{s} leaked in {dump}");
        }
        assert!(dump.contains("<secret>"));
        assert!(!x.sent.is_empty() && !x.got.is_empty());
    }
    #[test]
    fn clean_url_hides_secret_values_the_password_and_the_fragment() {
        let a = clean_url("https://u:pw1234xyz@a.test/cb?code=1&access_token=tok98765&key=k5555#id_token=frag777");
        for s in ["pw1234xyz", "tok98765", "k5555", "frag777"] {
            assert!(!a.contains(s), "{s} leaked in {a}");
        }
        assert!(a.contains("code=1") && a.contains("access_token="));
        let r = clean_url("/login?next=%2Fhome&token=abc12345");
        assert!(r.starts_with("/login?next="), "{r}");
        assert!(!r.contains("abc12345"), "{r}");
        assert_eq!(clean_url("/plain/path"), "/plain/path");
    }
}
