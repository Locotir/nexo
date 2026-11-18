//! Credentials are never kept: the values of secret-looking headers and URL parameters are replaced as
//! the requests are recorded, so nothing the model reads (or the buffers hold) has them.

use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// What a secret value is shown as.
pub const REDACTED: &str = "REDACTED";

const NAMES: [&str; 8] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "x-auth-token",
    "x-csrf-token",
    "password",
];
const PARTS: [&str; 5] = ["token", "secret", "session", "api-key", "auth"];

/// True for a header or parameter name whose value is a credential: one of the usual names, or any name with
/// `token`, `secret`, `session`, `api-key` (also `api_key`, `apikey`) or `auth` in it, in any case.
pub(crate) fn is_secret(name: &str) -> bool {
    let name = name.trim().to_lowercase().replace('_', "-");
    NAMES.contains(&name.as_str()) || PARTS.iter().any(|p| name.contains(p)) || name.contains("apikey")
}

/// A header object (`{"Name": "value"}`) as `(name, value)` pairs, secret values replaced.
pub(crate) fn header_list(h: &Value) -> Vec<(String, String)> {
    h.as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| {
                    let value = if is_secret(k) {
                        REDACTED.to_string()
                    } else {
                        v.as_str().map_or_else(|| v.to_string(), str::to_string)
                    };
                    (k.clone(), value)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `a=1&token=x` with the values of secret names replaced (by `hide`, given the percent-decoded value); parts
/// that are not `name=value` stay.
fn params_with(text: &str, secret: &dyn Fn(&str) -> bool, hide: &dyn Fn(&str) -> String) -> String {
    text.split('&')
        .map(|part| match part.split_once('=') {
            Some((k, v)) if secret(&percent_decode(k)) => format!("{k}={}", hide(&percent_decode(v))),
            _ => part.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match bytes[i] {
            b'%' if i + 2 < bytes.len() && hex(bytes[i + 1]).is_some() && hex(bytes[i + 2]).is_some() => {
                out.push((hex(bytes[i + 1]).unwrap_or(0) * 16 + hex(bytes[i + 2]).unwrap_or(0)) as u8);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The URL with a password in its user info, and the values of secret query or fragment parameters
/// (`?token=...`, `#access_token=...`), replaced.
pub(crate) fn url(url: &str) -> String {
    url_with(url, &is_secret, &|_| REDACTED.to_string())
}

/// As [`url`], but each hidden value becomes its [`marker`], so the same value can be recognised elsewhere.
pub(crate) fn url_marked(u: &str) -> String {
    url_with(u, &secret_key, &mark)
}

fn url_with(url: &str, secret: &dyn Fn(&str) -> bool, hide: &dyn Fn(&str) -> String) -> String {
    let (rest, fragment) = match url.split_once('#') {
        Some((r, f)) => (r, Some(f)),
        None => (url, None),
    };
    let (base, query) = match rest.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (rest, None),
    };
    let mut out = user_info(base, hide);
    if let Some(q) = query {
        out.push('?');
        out.push_str(&params_with(q, secret, hide));
    }
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(&if f.contains('=') { params_with(f, secret, hide) } else { f.to_string() });
    }
    out
}

/// `https://user:pw@host/` becomes `https://user:REDACTED@host/`.
fn user_info(base: &str, hide: &dyn Fn(&str) -> String) -> String {
    let Some((scheme, rest)) = base.split_once("://") else { return base.to_string() };
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, _)| (a, &rest[a.len()..]));
    match authority.rsplit_once('@') {
        Some((info, host)) if info.contains(':') => {
            let (user, pw) = info.split_once(':').unwrap_or((info, ""));
            format!("{scheme}://{user}:{}@{host}{path}", hide(pw))
        }
        _ => base.to_string(),
    }
}

// ---- capture mode: linkable markers instead of REDACTED -------------------------------------------------------

/// The salt of this process: random, made on first use, never written anywhere. A marker is only comparable
/// inside one process, so it cannot be used to guess a secret from a table of known values.
fn salt() -> &'static [u8; 32] {
    static SALT: OnceLock<[u8; 32]> = OnceLock::new();
    SALT.get_or_init(|| {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let mut out = [0u8; 32];
        for (i, chunk) in out.chunks_mut(8).enumerate() {
            // every `RandomState` has its own random keys from the operating system
            let mut h = RandomState::new().build_hasher();
            h.write_u128(nanos);
            h.write_u32(std::process::id());
            h.write_usize(i);
            chunk.copy_from_slice(&h.finish().to_le_bytes());
        }
        out
    })
}

/// A stand-in for a secret value: `<secret:` + 16 hex characters of sha256(salt || value) + `>`. The same value
/// gives the same marker for the whole process (so a token a response hands out can be matched with the request
/// that sends it later), a different value another one, and the value itself cannot be read back from it.
pub fn marker(value: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt());
    h.update(value.as_bytes());
    let digest = h.finalize();
    let mut out = String::from("<secret:");
    for b in &digest[..8] {
        out.push_str(&format!("{b:02x}"));
    }
    out.push('>');
    out
}

/// [`marker`], but an empty value stays empty (there is nothing to hide, and nothing to link).
fn mark(value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else {
        marker(value)
    }
}

/// Name parts that mark a secret in capture mode. Copied from `nexo-api` (`exchange.rs`, `SECRET_PARTS`,
/// `SECRET_EXACT`, `secret_name`); keep the two in step. There is no dependency between the crates.
const SECRET_PARTS: [&str; 17] = [
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
    "x-amz-security",
];
const SECRET_EXACT: [&str; 8] = ["auth", "sid", "sig", "otp", "pwd", "pass", "refresh", "bearer"];

/// Whether a header, cookie, form or JSON field name says its value is a secret, in capture mode (same rule as
/// `secret_name` of `nexo-api`): the names above, `auth` as a prefix but not `author`/`authority`, or as a suffix
/// (`oauth`). Unlike [`is_secret`], `author` is not a secret. A name ending in `-sid` or `.sid` is one too.
pub(crate) fn secret_key(name: &str) -> bool {
    let raw = name.trim().to_lowercase();
    let n: String = raw.chars().filter(char::is_ascii_alphanumeric).collect();
    if n.is_empty() {
        return false;
    }
    SECRET_PARTS.iter().any(|p| n.contains(&p.replace('-', "")))
        || SECRET_EXACT.contains(&n.as_str())
        || (n.starts_with("auth") && !n.starts_with("author"))
        || n.ends_with("auth")
        || raw.ends_with("-sid")
        || raw.ends_with("_sid")
        || raw.ends_with(".sid")
}

/// Whether a string looks like a token, a hash or an encoded blob rather than words, a date or a link. Same rule
/// as `opaque` in `nexo-api` (`exchange.rs`): 24 characters or more, no whitespace, no `:`, not starting with `/`,
/// and either 32+ hex digits or at least three of lower case, upper case, digits and symbols.
pub(crate) fn opaque(s: &str) -> bool {
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

/// The value as a marker when it looks opaque (a token under an ordinary name), else as it is.
fn mark_if_opaque(value: &str) -> String {
    if opaque(value) {
        marker(value)
    } else {
        value.to_string()
    }
}

/// A header object as `(name, value)` pairs with secret values turned into markers (see [`header_value_marked`]).
pub(crate) fn header_list_marked(h: &Value) -> Vec<(String, String)> {
    h.as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| {
                    let raw = v.as_str().map_or_else(|| v.to_string(), str::to_string);
                    (k.clone(), header_value_marked(k, &raw))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One header value for capture mode. `Cookie` and `Set-Cookie` keep every cookie name and turn every cookie value
/// into a marker (`sid=<secret:..>`; the `Set-Cookie` attributes stay). `Authorization` keeps its scheme and marks
/// the token alone (`Bearer <secret:..>`), so the token matches the one in a login response. Any other secret
/// header becomes the marker of its value.
pub(crate) fn header_value_marked(name: &str, value: &str) -> String {
    let lower = name.trim().to_lowercase();
    if lower == "cookie" {
        return value.split(';').map(cookie_pair).collect::<Vec<_>>().join("; ");
    }
    if lower == "set-cookie" {
        return value.split('\n').map(set_cookie_line).collect::<Vec<_>>().join("\n");
    }
    if !secret_key(name) {
        // an opaque value under an ordinary name (`X-Foo: <token>`, `X-Foo: Bearer <token>`) is hidden too
        if let Some((scheme, rest)) = value.trim().split_once(' ') {
            if scheme.chars().all(|c| c.is_ascii_alphabetic()) && opaque(rest.trim()) {
                return format!("{scheme} {}", marker(rest.trim()));
            }
        }
        return mark_if_opaque(value);
    }
    if lower.contains("authorization") {
        if let Some((scheme, token)) = value.trim().split_once(' ') {
            if !token.trim().is_empty() && scheme.chars().all(|c| c.is_ascii_alphabetic()) {
                return format!("{scheme} {}", mark(token.trim()));
            }
        }
    }
    mark(value.trim())
}

/// `name=value` with the value marked; a part without `=` stays.
fn cookie_pair(part: &str) -> String {
    let part = part.trim();
    match part.split_once('=') {
        Some((k, v)) => format!("{k}={}", mark(v)),
        None => part.to_string(),
    }
}

fn set_cookie_line(line: &str) -> String {
    let mut parts = line.split(';');
    let mut out = vec![cookie_pair(parts.next().unwrap_or(""))];
    out.extend(parts.map(|a| a.trim().to_string()));
    out.join("; ")
}

/// A request or response body for capture mode: values of secret keys become markers. JSON (nested, any depth)
/// and `a=b&c=d` forms are read as such, multipart by its part names; any other text, or JSON that does not parse,
/// is searched for `"key": "value"` pairs. `mime` is the content type, when known.
pub(crate) fn body_marked(text: &str, mime: &str) -> String {
    let m = mime.to_lowercase();
    let t = text.trim_start();
    if m.contains("multipart") {
        return multipart_marked(text);
    }
    if m.contains("json") || t.starts_with('{') || t.starts_with('[') {
        return match serde_json::from_str::<Value>(text) {
            Ok(mut v) => {
                json_marked(&mut v);
                v.to_string()
            }
            Err(_) => text_marked(text),
        };
    }
    let form_shaped = text.contains('=') && !text.contains(['\n', '<', '{', ' ']);
    if m.contains("urlencoded") || form_shaped {
        return form_marked(text);
    }
    text_marked(text)
}

/// `a=b&c=d`: the value of a secret name, and any opaque value, becomes a marker.
fn form_marked(text: &str) -> String {
    text.split('&')
        .map(|part| match part.split_once('=') {
            Some((k, v)) => {
                let value = percent_decode(v);
                if secret_key(&percent_decode(k)) {
                    format!("{k}={}", mark(&value))
                } else if opaque(&value) {
                    format!("{k}={}", marker(&value))
                } else {
                    part.to_string()
                }
            }
            None => part.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn json_marked(v: &mut Value) {
    match v {
        Value::String(s) => {
            if opaque(s) {
                let m = marker(s);
                *s = m;
            }
        }
        Value::Object(o) => {
            for (k, val) in o.iter_mut() {
                if secret_key(k) && !val.is_null() && !val.is_boolean() {
                    let raw = val.as_str().map_or_else(|| val.to_string(), str::to_string);
                    *val = Value::String(mark(&raw));
                } else {
                    json_marked(val);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(json_marked),
        _ => {}
    }
}

/// Any text: every `"key": "value"` whose key is secret gets its value marked.
fn text_marked(text: &str) -> String {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#""((?:[^"\\]|\\.)*)"(\s*:\s*)"((?:[^"\\]|\\.)*)""#).ok());
    let Some(re) = re else { return "<body withheld: no pattern>".to_string() };
    re.replace_all(text, |c: &regex::Captures| {
        let (key, colon, value) = (&c[1], &c[2], &c[3]);
        if secret_key(key) || opaque(value) {
            format!("\"{key}\"{colon}\"{}\"", mark(value))
        } else {
            c[0].to_string()
        }
    })
    .into_owned()
}

/// A `multipart/form-data` body: the value of a part whose `name` is secret is marked, the rest stays.
fn multipart_marked(text: &str) -> String {
    let mut out = Vec::new();
    let mut secret_part = false;
    let mut in_value = false;
    for line in text.split('\n') {
        let bare = line.trim_end_matches('\r');
        if bare.starts_with("--") {
            secret_part = false;
            in_value = false;
        } else if !in_value {
            if bare.is_empty() {
                in_value = true;
            } else if bare.to_lowercase().starts_with("content-disposition") {
                let name = bare.split("name=\"").nth(1).and_then(|r| r.split('"').next()).unwrap_or("");
                secret_part = secret_key(name);
            }
        } else if secret_part || opaque(bare) {
            out.push(format!("{}{}", mark(bare), if line.ends_with('\r') { "\r" } else { "" }));
            continue;
        }
        out.push(line.to_string());
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_names_are_found_in_any_case_and_form() {
        for name in [
            "Authorization",
            "Proxy-Authorization",
            "COOKIE",
            "Set-Cookie",
            "X-Api-Key",
            "x-auth-token",
            "X-CSRF-Token",
            "X-Amz-Security-Token",
            "client_secret",
            "sessionid",
            "api_key",
            "apikey",
            "X-Custom-Auth",
            "access_token",
        ] {
            assert!(is_secret(name), "{name}");
        }
        for name in ["Accept", "Content-Type", "q", "page", "User-Agent", "Host"] {
            assert!(!is_secret(name), "{name}");
        }
    }

    #[test]
    fn headers_keep_names_and_lose_secret_values() {
        let h = serde_json::json!({"Accept": "*/*", "Authorization": "Bearer abc", "Cookie": "sid=1"});
        let list = header_list(&h);
        assert!(list.contains(&("Accept".into(), "*/*".into())));
        assert!(list.contains(&("Authorization".into(), REDACTED.into())));
        assert!(list.contains(&("Cookie".into(), REDACTED.into())));
        assert!(!format!("{list:?}").contains("abc"));
    }

    #[test]
    fn each_kind_of_credential_is_redacted() {
        let h = serde_json::json!({
            "Authorization": "Bearer s1", "Cookie": "sid=s2", "Set-Cookie": "sid=s3; HttpOnly", "x-api-key": "s4",
            "Accept-Language": "en"
        });
        let list = header_list(&h);
        for name in ["Authorization", "Cookie", "Set-Cookie", "x-api-key"] {
            assert!(list.contains(&(name.to_string(), REDACTED.to_string())), "{name}: {list:?}");
        }
        assert!(list.contains(&("Accept-Language".into(), "en".into())));
        for secret in ["s1", "s2", "s3", "s4"] {
            assert!(!format!("{list:?}").contains(secret), "{secret}");
        }
        assert_eq!(url("https://a.b/x?token=s5"), "https://a.b/x?token=REDACTED");
        assert_eq!(url("https://a.b/#access_token=s6"), "https://a.b/#access_token=REDACTED");
        assert_eq!(url("https://user:s7@a.b/"), "https://user:REDACTED@a.b/");
        assert_eq!(header_list(&serde_json::json!(null)), Vec::new());
    }

    #[test]
    fn urls_lose_secret_parameters_and_passwords() {
        assert_eq!(url("https://a.b/x?q=1&token=abc&page=2"), "https://a.b/x?q=1&token=REDACTED&page=2");
        assert_eq!(url("https://a.b/x?API_KEY=k1&Sess%69on=s"), "https://a.b/x?API_KEY=REDACTED&Sess%69on=REDACTED");
        assert_eq!(url("https://a.b/cb#access_token=zzz&state=ok"), "https://a.b/cb#access_token=REDACTED&state=ok");
        assert_eq!(url("https://me:hunter2@a.b:8080/p?q=1"), "https://me:REDACTED@a.b:8080/p?q=1");
        assert_eq!(url("https://me@a.b/p"), "https://me@a.b/p");
        assert_eq!(url("https://a.b/page#top"), "https://a.b/page#top");
        assert_eq!(url("https://a.b/x?flag&q=1"), "https://a.b/x?flag&q=1");
        assert_eq!(url("data:text/plain,hi"), "data:text/plain,hi");
    }

    #[test]
    fn marker_is_stable_distinct_and_hides_the_value() {
        let a = marker("hunter2-secret-value");
        assert_eq!(a, marker("hunter2-secret-value"));
        assert_ne!(a, marker("another-value"));
        assert!(a.starts_with("<secret:") && a.ends_with('>') && a.len() == "<secret:".len() + 16 + 1, "{a}");
        assert!(a["<secret:".len()..a.len() - 1].chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!a.contains("hunter2"));
        assert_eq!(mark(""), "");
    }

    #[test]
    fn json_bodies_lose_secret_fields_at_any_depth() {
        let raw = r#"{"user":"ann","password":"pw-1","data":{"access_token":"tok-2","list":[{"jwt":"jj-3","ok":1}],"csrf":"cs-4","sid":"si-5","author":"x"}}"#;
        let out = body_marked(raw, "application/json");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["user"], "ann");
        assert_eq!(v["password"], marker("pw-1"));
        assert_eq!(v["data"]["access_token"], marker("tok-2"));
        assert_eq!(v["data"]["list"][0]["jwt"], marker("jj-3"));
        assert_eq!(v["data"]["list"][0]["ok"], 1);
        assert_eq!(v["data"]["csrf"], marker("cs-4"));
        assert_eq!(v["data"]["sid"], marker("si-5"));
        for s in ["pw-1", "tok-2", "jj-3", "cs-4", "si-5"] {
            assert!(!out.contains(s), "{s}");
        }
        // a body that is not valid JSON (cut) still loses its secret pairs
        let cut = body_marked(r#"{"token": "abc-9", "n": 1, "pass"#, "application/json");
        assert!(!cut.contains("abc-9") && cut.contains(&marker("abc-9")), "{cut}");
    }

    #[test]
    fn form_and_multipart_bodies_lose_secret_fields() {
        let out = body_marked("user=ann&password=p%40ss&xsrf_token=x1&q=1", "application/x-www-form-urlencoded");
        assert_eq!(out, format!("user=ann&password={}&xsrf_token={}&q=1", marker("p@ss"), marker("x1")));
        assert_eq!(body_marked("a=1&sid=zz", ""), format!("a=1&sid={}", marker("zz")));
        let mp = "--b\r\nContent-Disposition: form-data; name=\"passwd\"\r\n\r\nmp-secret\r\n--b\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nhello\r\n--b--";
        let out = body_marked(mp, "multipart/form-data; boundary=b");
        assert!(!out.contains("mp-secret") && out.contains("hello") && out.contains(&marker("mp-secret")), "{out}");
    }

    #[test]
    fn cookies_and_authorization_become_markers_that_link() {
        let c = header_value_marked("Cookie", "sid=abc; theme=dark");
        assert_eq!(c, format!("sid={}; theme={}", marker("abc"), marker("dark")));
        let sc = header_value_marked("Set-Cookie", "sid=abc; Path=/; HttpOnly\nb=2; Secure");
        assert_eq!(sc, format!("sid={}; Path=/; HttpOnly\nb={}; Secure", marker("abc"), marker("2")));
        // the token of a login response matches the one a later request sends
        assert_eq!(header_value_marked("Authorization", "Bearer tok-2"), format!("Bearer {}", marker("tok-2")));
        assert_eq!(header_value_marked("X-Api-Key", "k-1"), marker("k-1"));
        assert_eq!(header_value_marked("Accept", "*/*"), "*/*");
        let all = format!("{c}{sc}");
        assert!(!all.contains("=abc") && !all.contains("dark"));
        let list = header_list_marked(&serde_json::json!({"Authorization": "Bearer t9", "Accept": "*/*"}));
        assert!(list.contains(&("Accept".into(), "*/*".into())));
        assert!(!format!("{list:?}").contains("t9"));
    }

    #[test]
    fn urls_get_markers_in_capture_mode_only() {
        let u = url_marked("https://me:pw1@a.b/x?q=1&token=t1#access_token=t2");
        assert_eq!(
            u,
            format!("https://me:{}@a.b/x?q=1&token={}#access_token={}", marker("pw1"), marker("t1"), marker("t2"))
        );
        assert_eq!(url("https://a.b/x?token=t1"), "https://a.b/x?token=REDACTED");
    }

    #[test]
    fn capture_names_know_author_from_auth() {
        for n in ["author", "authority", "authors", "Authors-Name"] {
            assert!(!secret_key(n), "{n}");
        }
        for n in
            ["auth", "oauth", "authToken", "Authorization", "x-auth", "sessionid", "connect.sid", "X_SID", "csrfToken"]
        {
            assert!(secret_key(n), "{n}");
        }
        let out = body_marked(r#"{"author":"ann","authority":"a.b","auth":"abc"}"#, "application/json");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!((v["author"].as_str(), v["authority"].as_str()), (Some("ann"), Some("a.b")));
        assert_eq!(v["auth"], marker("abc"));
        // the old rule of the model-facing tools is untouched
        assert!(is_secret("author"));
    }

    #[test]
    fn opaque_values_become_markers_under_any_name() {
        let tok = "eyJhbGciOiJIUzI1NiJ9.abc_DEF-1234567890";
        assert!(opaque(tok) && opaque(&"a1".repeat(16)));
        assert!(
            !opaque("short") && !opaque("two words that are long enough") && !opaque("/some/long/path/here/ok/yes/no")
        );
        let login =
            body_marked(&format!(r#"{{"data":{{"t":"{tok}","list":["{tok}"],"name":"ann"}}}}"#), "application/json");
        let v: Value = serde_json::from_str(&login).unwrap();
        assert_eq!(v["data"]["t"], marker(tok));
        assert_eq!(v["data"]["list"][0], marker(tok));
        assert_eq!(v["data"]["name"], "ann");
        assert_eq!(header_value_marked("Authorization", &format!("Bearer {tok}")), format!("Bearer {}", marker(tok)));
        assert_eq!(header_value_marked("X-Foo", tok), marker(tok));
        assert_eq!(header_value_marked("X-Foo", &format!("Bearer {tok}")), format!("Bearer {}", marker(tok)));
        assert_eq!(body_marked(&format!("a=1&b={tok}"), ""), format!("a=1&b={}", marker(tok)));
        assert!(!login.contains(tok));
    }
}
