//! Credentials are never kept: the values of secret-looking headers and URL parameters are replaced as
//! the requests are recorded, so nothing the model reads (or the buffers hold) has them.

use serde_json::Value;

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

/// `a=1&token=x` with the values of secret names replaced; parts that are not `name=value` stay.
fn params(text: &str) -> String {
    text.split('&')
        .map(|part| match part.split_once('=') {
            Some((k, _)) if is_secret(&percent_decode(k)) => format!("{k}={REDACTED}"),
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
    let (rest, fragment) = match url.split_once('#') {
        Some((r, f)) => (r, Some(f)),
        None => (url, None),
    };
    let (base, query) = match rest.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (rest, None),
    };
    let mut out = user_info(base);
    if let Some(q) = query {
        out.push('?');
        out.push_str(&params(q));
    }
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(&if f.contains('=') { params(f) } else { f.to_string() });
    }
    out
}

/// `https://user:pw@host/` becomes `https://user:REDACTED@host/`.
fn user_info(base: &str) -> String {
    let Some((scheme, rest)) = base.split_once("://") else { return base.to_string() };
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, _)| (a, &rest[a.len()..]));
    match authority.rsplit_once('@') {
        Some((info, host)) if info.contains(':') => {
            let user = info.split_once(':').map_or(info, |(u, _)| u);
            format!("{scheme}://{user}:{REDACTED}@{host}{path}")
        }
        _ => base.to_string(),
    }
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
}
